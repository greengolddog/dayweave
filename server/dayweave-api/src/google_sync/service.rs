use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    sync::Arc,
    time::{Duration as StdDuration, SystemTime},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use dayweave_google::{
    AccessTokenProvider, GoogleClient, GoogleError, PreparedGoogleRequest,
    calendar::{
        CalendarListPage, EventDateTime, EventListOptions, EventListPage, EventWriteApproval,
        ExtendedProperties, GoogleEvent, SendUpdates,
    },
    tasks::{GoogleTask, TaskListPage, TaskPage},
};
use secrecy::SecretString;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    config::{
        GOOGLE_CALENDAR_READONLY_SCOPE, GOOGLE_CALENDAR_SCOPE, GOOGLE_TASKS_READONLY_SCOPE,
        GOOGLE_TASKS_SCOPE,
    },
    google_oauth::{
        CryptoError, GoogleOAuthService, GoogleOAuthServiceError, OAuthScope, SecretCipher,
        sync_cursor_aad,
    },
    items::{ItemKind, ItemService, ItemServiceError, ItemStatus, NewItem, SplitPolicy},
    proposals::Clock,
};

use super::{
    CalendarProjectionBatch, CalendarProjectionWindow, CursorValue, DiscoveredCollection,
    GoogleCalendarPolicy, GoogleCollectionKind, GoogleEventDisposition, GoogleOutboundAccepted,
    GoogleOutboundApproval, GoogleOutboundPreview, GoogleSyncCollection, GoogleSyncRefreshAccepted,
    GoogleSyncRepository, GoogleSyncRepositoryError, GoogleSyncRole, GoogleSyncStatus,
    OutboundApprovalSpec, OutboundEnqueueSpec, OutboundOperation, OutboundPreviewSpec,
    OutboundRequest, OutboundResult, OutboundWork, PreparedOutbound, RejectedRemoteItem,
    RemoteCalendarSeriesChange, RemoteItemChange, SyncClaim, SyncCounts, SyncFailureKind,
};

const MAX_PAGES: usize = 100;
const MAX_COLLECTIONS: usize = 10_000;
const MAX_ITEMS_PER_RUN: usize = 100_000;
const MAX_CALENDAR_PROJECTION_ITEMS: usize = 10_000;
const MAX_CALENDAR_PROJECTION_NORMALIZED_BYTES: usize = 32 * 1024 * 1024;
const MAX_CALENDAR_PROJECTION_WINDOW_DAYS: i64 = 150;
const CALENDAR_PROJECTION_LOOKBACK_DAYS: i64 = 30;
const CALENDAR_PROJECTION_LOOKAHEAD_DAYS: i64 = 120;
const MAX_OUTBOUND_PER_RUN: usize = 100;
const TASK_CURSOR_OVERLAP_MINUTES: i64 = 2;
const RUN_LEASE_MINUTES: i64 = 10;
const PERIODIC_SYNC_MINUTES: i64 = 15;
const WORKER_POLL_SECONDS: u64 = 30;
const APPROVAL_TOKEN_PREFIX: &str = "dw_ga1_";
const APPROVAL_TOKEN_RANDOM_BYTES: usize = 32;
const DISPATCH_PREPARATION_TIMEOUT_SECONDS: u64 = 30;

async fn sequence_guarded_write<P, A, R, E, PFut, AFut, SFut, Authorize, Send>(
    preparation: PFut,
    authorize: Authorize,
    send: Send,
) -> Result<R, E>
where
    PFut: Future<Output = Result<P, E>>,
    Authorize: FnOnce() -> AFut,
    AFut: Future<Output = Result<A, E>>,
    Send: FnOnce(P, A) -> SFut,
    SFut: Future<Output = Result<R, E>>,
{
    // Constructing the authorization future only after preparation completes
    // is intentional: OAuth refresh and request serialization cannot consume
    // any part of the 30-second database initiation capability.
    let prepared = preparation.await?;
    let authorization = authorize().await?;
    send(prepared, authorization).await
}

pub(crate) enum ProviderWriteResponse {
    Event(Box<GoogleEvent>),
    Task(Box<GoogleTask>),
    Empty,
}

#[async_trait]
pub(crate) trait PreparedGoogleSyncWrite: Send {
    async fn send(
        self: Box<Self>,
        initiation_deadline: SystemTime,
    ) -> Result<ProviderWriteResponse, GoogleError>;
}

enum PreparedResponseKind {
    Event,
    Task,
    Empty,
}

// Deliberately not Debug: the prepared request contains an OAuth bearer
// header. It is single-use and never crosses the durable repository boundary.
struct ProductionPreparedGoogleWrite {
    request: PreparedGoogleRequest,
    response_kind: PreparedResponseKind,
}

#[async_trait]
impl PreparedGoogleSyncWrite for ProductionPreparedGoogleWrite {
    async fn send(
        self: Box<Self>,
        initiation_deadline: SystemTime,
    ) -> Result<ProviderWriteResponse, GoogleError> {
        let Self {
            request,
            response_kind,
        } = *self;
        match response_kind {
            PreparedResponseKind::Event => request
                .send_json(Some(initiation_deadline))
                .await
                .map(Box::new)
                .map(ProviderWriteResponse::Event),
            PreparedResponseKind::Task => request
                .send_json(Some(initiation_deadline))
                .await
                .map(Box::new)
                .map(ProviderWriteResponse::Task),
            PreparedResponseKind::Empty => request
                .send_empty(Some(initiation_deadline))
                .await
                .map(|()| ProviderWriteResponse::Empty),
        }
    }
}

#[async_trait]
pub(crate) trait GoogleSyncProvider: Send + Sync {
    async fn list_calendars(
        &self,
        account_id: Uuid,
        page_token: Option<&str>,
    ) -> Result<CalendarListPage, GoogleError>;

    async fn list_task_lists(
        &self,
        account_id: Uuid,
        page_token: Option<&str>,
    ) -> Result<TaskListPage, GoogleError>;

    async fn list_events(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        options: &EventListOptions,
    ) -> Result<EventListPage, GoogleError>;

    async fn get_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<GoogleEvent, GoogleError>;

    async fn prepare_insert_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event: &GoogleEvent,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>;

    async fn prepare_update_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event: &GoogleEvent,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>;

    async fn prepare_delete_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event_id: &str,
        etag: &str,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>;

    async fn list_tasks(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        page_token: Option<&str>,
        updated_min: Option<&str>,
    ) -> Result<TaskPage, GoogleError>;

    async fn prepare_insert_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        _task: &GoogleTask,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>;

    async fn prepare_update_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        _task: &GoogleTask,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>;

    async fn prepare_delete_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        task_id: &str,
        etag: &str,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>;
}

#[derive(Clone)]
pub(crate) struct ProductionGoogleSyncProvider {
    oauth: Arc<GoogleOAuthService>,
}

impl std::fmt::Debug for ProductionGoogleSyncProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductionGoogleSyncProvider")
            .finish_non_exhaustive()
    }
}

impl ProductionGoogleSyncProvider {
    #[must_use]
    pub(crate) fn new(oauth: Arc<GoogleOAuthService>) -> Self {
        Self { oauth }
    }

    fn client(&self, account_id: Uuid) -> Result<GoogleClient, GoogleError> {
        GoogleClient::production(Arc::new(OAuthAccountAccessToken {
            oauth: self.oauth.clone(),
            account_id,
        }))
    }
}

#[derive(Clone)]
struct OAuthAccountAccessToken {
    oauth: Arc<GoogleOAuthService>,
    account_id: Uuid,
}

#[async_trait]
impl AccessTokenProvider for OAuthAccountAccessToken {
    async fn access_token(&self) -> Result<SecretString, GoogleError> {
        self.oauth
            .access_token_for_sync(self.account_id)
            .await
            .map_err(map_oauth_transport_error)
    }
}

fn map_oauth_transport_error(error: GoogleOAuthServiceError) -> GoogleError {
    match error {
        GoogleOAuthServiceError::Google(error) => error,
        GoogleOAuthServiceError::IntegrationTimeout => GoogleError::Temporary { status: 504 },
        _ => GoogleError::Unauthorized,
    }
}

#[async_trait]
impl GoogleSyncProvider for ProductionGoogleSyncProvider {
    async fn list_calendars(
        &self,
        account_id: Uuid,
        page_token: Option<&str>,
    ) -> Result<CalendarListPage, GoogleError> {
        self.client(account_id)?
            .list_calendars(page_token, None)
            .await
    }

    async fn list_task_lists(
        &self,
        account_id: Uuid,
        page_token: Option<&str>,
    ) -> Result<TaskListPage, GoogleError> {
        self.client(account_id)?.list_task_lists(page_token).await
    }

    async fn list_events(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        options: &EventListOptions,
    ) -> Result<EventListPage, GoogleError> {
        self.client(account_id)?
            .list_events(calendar_id, options)
            .await
    }

    async fn get_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<GoogleEvent, GoogleError> {
        self.client(account_id)?
            .get_event(calendar_id, event_id)
            .await
    }

    async fn prepare_insert_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event: &GoogleEvent,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError> {
        let request = self
            .client(account_id)?
            .prepare_insert_event(
                calendar_id,
                event,
                &EventWriteApproval::PrivateAppOwned,
                SendUpdates::None,
            )
            .await?;
        Ok(Box::new(ProductionPreparedGoogleWrite {
            request,
            response_kind: PreparedResponseKind::Event,
        }))
    }

    async fn prepare_update_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event: &GoogleEvent,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError> {
        let request = self
            .client(account_id)?
            .prepare_update_event(
                calendar_id,
                event,
                &EventWriteApproval::PrivateAppOwned,
                SendUpdates::None,
            )
            .await?;
        Ok(Box::new(ProductionPreparedGoogleWrite {
            request,
            response_kind: PreparedResponseKind::Event,
        }))
    }

    async fn prepare_delete_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event_id: &str,
        etag: &str,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError> {
        let request = self
            .client(account_id)?
            .prepare_delete_event(
                calendar_id,
                event_id,
                etag,
                &EventWriteApproval::PrivateAppOwned,
                SendUpdates::None,
            )
            .await?;
        Ok(Box::new(ProductionPreparedGoogleWrite {
            request,
            response_kind: PreparedResponseKind::Empty,
        }))
    }

    async fn list_tasks(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        page_token: Option<&str>,
        updated_min: Option<&str>,
    ) -> Result<TaskPage, GoogleError> {
        self.client(account_id)?
            .list_tasks(task_list_id, page_token, updated_min)
            .await
    }

    async fn prepare_insert_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        task: &GoogleTask,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError> {
        let request = self
            .client(account_id)?
            .prepare_insert_task(task_list_id, task)
            .await?;
        Ok(Box::new(ProductionPreparedGoogleWrite {
            request,
            response_kind: PreparedResponseKind::Task,
        }))
    }

    async fn prepare_update_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        task: &GoogleTask,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError> {
        let request = self
            .client(account_id)?
            .prepare_update_task(task_list_id, task)
            .await?;
        Ok(Box::new(ProductionPreparedGoogleWrite {
            request,
            response_kind: PreparedResponseKind::Task,
        }))
    }

    async fn prepare_delete_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        task_id: &str,
        etag: &str,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError> {
        let request = self
            .client(account_id)?
            .prepare_delete_task(task_list_id, task_id, etag)
            .await?;
        Ok(Box::new(ProductionPreparedGoogleWrite {
            request,
            response_kind: PreparedResponseKind::Empty,
        }))
    }
}

pub(crate) struct GoogleSyncService {
    repository: Arc<dyn GoogleSyncRepository>,
    provider: Arc<dyn GoogleSyncProvider>,
    oauth: Arc<GoogleOAuthService>,
    items: Arc<ItemService>,
    cipher: SecretCipher,
    scope: OAuthScope,
    clock: Arc<dyn Clock>,
    outbound_enabled: bool,
    approval_ttl: StdDuration,
}

impl std::fmt::Debug for GoogleSyncService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleSyncService")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl GoogleSyncService {
    #[must_use]
    #[allow(clippy::too_many_arguments)] // Builds the complete fail-closed integration boundary.
    pub(crate) fn new(
        repository: Arc<dyn GoogleSyncRepository>,
        provider: Arc<dyn GoogleSyncProvider>,
        oauth: Arc<GoogleOAuthService>,
        items: Arc<ItemService>,
        cipher: SecretCipher,
        scope: OAuthScope,
        clock: Arc<dyn Clock>,
        outbound_enabled: bool,
        approval_ttl: StdDuration,
    ) -> Self {
        Self {
            repository,
            provider,
            oauth,
            items,
            cipher,
            scope,
            clock,
            outbound_enabled,
            approval_ttl,
        }
    }

    pub(crate) async fn recover_startup(&self) -> Result<(), GoogleSyncServiceError> {
        self.repository.recover_startup(self.clock.now()).await?;
        Ok(())
    }

    pub(crate) fn spawn_worker(self: &Arc<Self>) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(StdDuration::from_secs(WORKER_POLL_SECONDS));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = service.drain_one().await {
                    tracing::warn!(
                        error_code = error.code(),
                        "Google sync worker iteration failed"
                    );
                }
            }
        });
    }

    pub(crate) async fn discover(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<GoogleSyncCollection>, GoogleSyncServiceError> {
        self.discover_inner(account_id, None).await
    }

    async fn discover_inner(
        &self,
        account_id: Uuid,
        claim: Option<&SyncClaim>,
    ) -> Result<Vec<GoogleSyncCollection>, GoogleSyncServiceError> {
        let account = self.oauth.account_for_sync(account_id).await?;
        if has_calendar_read(&account.granted_scopes) {
            let calendars = self.discover_calendars(account_id, claim).await?;
            self.repository
                .replace_discovered(
                    account_id,
                    claim,
                    GoogleCollectionKind::Calendar,
                    calendars,
                    self.clock.now(),
                )
                .await?;
        }
        if has_tasks_read(&account.granted_scopes) {
            let task_lists = self.discover_task_lists(account_id, claim).await?;
            self.repository
                .replace_discovered(
                    account_id,
                    claim,
                    GoogleCollectionKind::TaskList,
                    task_lists,
                    self.clock.now(),
                )
                .await?;
        }
        self.repository
            .collections(account_id)
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn collections(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<GoogleSyncCollection>, GoogleSyncServiceError> {
        self.oauth.account_for_sync(account_id).await?;
        Ok(self.repository.collections(account_id).await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn configure_collection(
        &self,
        account_id: Uuid,
        collection_id: Uuid,
        expected_revision: u64,
        selected: bool,
        visible: bool,
        role: GoogleSyncRole,
        calendar_policy: GoogleCalendarPolicy,
    ) -> Result<GoogleSyncCollection, GoogleSyncServiceError> {
        if expected_revision == 0 {
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
        let account = self.oauth.account_for_sync(account_id).await?;
        let collection = self
            .repository
            .collection(account_id, collection_id)
            .await?;
        if selected {
            let has_read_scope = match collection.kind {
                GoogleCollectionKind::Calendar => has_calendar_read(&account.granted_scopes),
                GoogleCollectionKind::TaskList => has_tasks_read(&account.granted_scopes),
            };
            if !has_read_scope {
                return Err(GoogleSyncServiceError::MissingReadScope);
            }
        }
        match (collection.kind, role) {
            (GoogleCollectionKind::Calendar, GoogleSyncRole::Writable)
                if !account.granted_scopes.contains(GOOGLE_CALENDAR_SCOPE) =>
            {
                return Err(GoogleSyncServiceError::MissingWriteScope);
            }
            (GoogleCollectionKind::TaskList, GoogleSyncRole::Writable)
                if !account.granted_scopes.contains(GOOGLE_TASKS_SCOPE) =>
            {
                return Err(GoogleSyncServiceError::MissingWriteScope);
            }
            _ => {}
        }
        Ok(self
            .repository
            .configure_collection(
                account_id,
                collection_id,
                expected_revision,
                selected,
                visible,
                role,
                calendar_policy,
                self.clock.now(),
            )
            .await?)
    }

    pub(crate) async fn request_refresh(
        &self,
        account_id: Uuid,
        request_id: Uuid,
    ) -> Result<GoogleSyncRefreshAccepted, GoogleSyncServiceError> {
        if request_id.is_nil() {
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
        self.oauth.account_for_sync(account_id).await?;
        let now = self.clock.now();
        Ok(self
            .repository
            .request_refresh(account_id, request_id, now)
            .await?)
    }

    pub(crate) async fn status(
        &self,
        account_id: Uuid,
    ) -> Result<GoogleSyncStatus, GoogleSyncServiceError> {
        self.oauth.account_for_sync(account_id).await?;
        let (run, outbox) = tokio::try_join!(
            self.repository.run_status(account_id),
            self.repository.outbox_counts(account_id)
        )?;
        Ok(GoogleSyncStatus {
            run,
            import_conflicts: outbox.import_conflicts,
            pending_outbound: outbox.pending,
            conflicted_outbound: outbox.conflicted,
            failed_outbound: outbox.failed,
            last_outbound_error_code: outbox.last_error_code,
            last_outbound_error_at: outbox.last_error_at,
            next_outbound_attempt_at: outbox.next_attempt_at,
        })
    }

    pub(crate) async fn preview_outbound(
        &self,
        account_id: Uuid,
        request: OutboundRequest,
    ) -> Result<GoogleOutboundPreview, GoogleSyncServiceError> {
        self.require_outbound_enabled()?;
        if request.expected_item_revision == 0 {
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
        let account = self.oauth.account_for_sync(account_id).await?;
        let collection = self
            .repository
            .collection(account_id, request.collection_id)
            .await?;
        let item = self.items.get_including_deleted(request.item_id).await?;
        if item.revision != request.expected_item_revision {
            return Err(GoogleSyncRepositoryError::RevisionConflict {
                expected: request.expected_item_revision,
                actual: item.revision,
            }
            .into());
        }
        let prepared = match collection.kind {
            GoogleCollectionKind::Calendar => {
                if !account.granted_scopes.contains(GOOGLE_CALENDAR_SCOPE) {
                    return Err(GoogleSyncServiceError::MissingWriteScope);
                }
                prepare_calendar_outbound(
                    item,
                    request.operation,
                    &collection,
                    &self.cipher,
                    self.scope,
                )?
            }
            GoogleCollectionKind::TaskList => {
                if !account.granted_scopes.contains(GOOGLE_TASKS_SCOPE) {
                    return Err(GoogleSyncServiceError::MissingWriteScope);
                }
                prepare_task_outbound(item, request.operation)?
            }
        };
        let required_scope = match collection.kind {
            GoogleCollectionKind::Calendar => GOOGLE_CALENDAR_SCOPE,
            GoogleCollectionKind::TaskList => GOOGLE_TASKS_SCOPE,
        };
        let id = Uuid::new_v4();
        let expires_at = self.clock.now()
            + Duration::from_std(self.approval_ttl)
                .map_err(|_| GoogleSyncServiceError::Internal)?;
        self.repository
            .create_outbound_preview(
                OutboundPreviewSpec {
                    id,
                    account_id,
                    collection_id: collection.id,
                    collection_revision: collection.revision,
                    collection_remote_id: collection.remote_collection_id.clone(),
                    collection_display_name: collection.display_name,
                    required_scope,
                    prepared,
                    expires_at,
                },
                self.clock.now(),
            )
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn approve_outbound(
        &self,
        account_id: Uuid,
        preview_id: Uuid,
        expected_preview_hash: &str,
    ) -> Result<GoogleOutboundApproval, GoogleSyncServiceError> {
        self.require_outbound_enabled()?;
        let expected_preview_hash = decode_hash(expected_preview_hash)?;
        let mut random = [0_u8; APPROVAL_TOKEN_RANDOM_BYTES];
        getrandom::fill(&mut random).map_err(|_| GoogleSyncServiceError::Randomness)?;
        let mut capability = String::with_capacity(APPROVAL_TOKEN_PREFIX.len() + 43);
        capability.push_str(APPROVAL_TOKEN_PREFIX);
        capability.push_str(&URL_SAFE_NO_PAD.encode(random));
        random.zeroize();
        let capability_hash = Sha256::digest(capability.as_bytes()).into();
        let expires_at = match self
            .repository
            .approve_outbound(
                OutboundApprovalSpec {
                    account_id,
                    preview_id,
                    expected_preview_hash,
                    capability_hash,
                },
                self.clock.now(),
            )
            .await
        {
            Ok(expires_at) => expires_at,
            Err(error) => {
                capability.zeroize();
                return Err(error.into());
            }
        };
        Ok(GoogleOutboundApproval {
            preview_id,
            approval_capability: capability,
            expires_at,
        })
    }

    pub(crate) async fn enqueue_outbound(
        &self,
        account_id: Uuid,
        request: OutboundRequest,
        mut approval_capability: String,
    ) -> Result<GoogleOutboundAccepted, GoogleSyncServiceError> {
        if let Err(error) = self.require_outbound_enabled() {
            approval_capability.zeroize();
            return Err(error);
        }
        if request.expected_item_revision == 0 {
            approval_capability.zeroize();
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
        let capability_hash = match approval_capability_hash(&approval_capability) {
            Ok(hash) => hash,
            Err(error) => {
                approval_capability.zeroize();
                return Err(error);
            }
        };
        approval_capability.zeroize();
        Ok(self
            .repository
            .enqueue_outbound(
                OutboundEnqueueSpec {
                    account_id,
                    request,
                    capability_hash,
                },
                self.clock.now(),
            )
            .await?)
    }

    fn require_outbound_enabled(&self) -> Result<(), GoogleSyncServiceError> {
        if self.outbound_enabled {
            Ok(())
        } else {
            Err(GoogleSyncServiceError::ExternalPublicationDisabled)
        }
    }

    async fn discover_calendars(
        &self,
        account_id: Uuid,
        claim: Option<&SyncClaim>,
    ) -> Result<Vec<DiscoveredCollection>, GoogleSyncServiceError> {
        let mut page_token = None;
        let mut seen_tokens = HashSet::new();
        let mut result = Vec::new();
        for _ in 0..MAX_PAGES {
            if let Some(claim) = claim {
                self.heartbeat(claim).await?;
            }
            let page = self
                .provider
                .list_calendars(account_id, page_token.as_deref())
                .await?;
            for entry in page.items {
                validate_remote_id(&entry.id)?;
                if result.len() == MAX_COLLECTIONS {
                    return Err(GoogleSyncServiceError::ProviderLimitExceeded);
                }
                let provider_access_role = if entry.deleted && entry.access_role.is_empty() {
                    None
                } else {
                    Some(bounded_ascii_label(&entry.access_role, 32)?)
                };
                result.push(DiscoveredCollection {
                    kind: GoogleCollectionKind::Calendar,
                    remote_id: entry.id,
                    display_name: bounded_title(&entry.summary, "Unnamed calendar").0,
                    provider_access_role,
                    provider_primary: entry.primary,
                    provider_selected: entry.selected,
                    provider_hidden: entry.hidden,
                    provider_deleted: entry.deleted,
                });
            }
            let Some(next) = page.next_page_token else {
                return Ok(result);
            };
            validate_page_token(&next, &mut seen_tokens)?;
            page_token = Some(next);
        }
        Err(GoogleSyncServiceError::ProviderLimitExceeded)
    }

    async fn discover_task_lists(
        &self,
        account_id: Uuid,
        claim: Option<&SyncClaim>,
    ) -> Result<Vec<DiscoveredCollection>, GoogleSyncServiceError> {
        let mut page_token = None;
        let mut seen_tokens = HashSet::new();
        let mut result = Vec::new();
        for _ in 0..MAX_PAGES {
            if let Some(claim) = claim {
                self.heartbeat(claim).await?;
            }
            let page = self
                .provider
                .list_task_lists(account_id, page_token.as_deref())
                .await?;
            for list in page.items {
                validate_remote_id(&list.id)?;
                if result.len() == MAX_COLLECTIONS {
                    return Err(GoogleSyncServiceError::ProviderLimitExceeded);
                }
                result.push(DiscoveredCollection {
                    kind: GoogleCollectionKind::TaskList,
                    remote_id: list.id,
                    display_name: bounded_title(&list.title, "Unnamed task list").0,
                    provider_access_role: None,
                    provider_primary: false,
                    provider_selected: true,
                    provider_hidden: false,
                    provider_deleted: false,
                });
            }
            let Some(next) = page.next_page_token else {
                return Ok(result);
            };
            validate_page_token(&next, &mut seen_tokens)?;
            page_token = Some(next);
        }
        Err(GoogleSyncServiceError::ProviderLimitExceeded)
    }

    async fn drain_one(&self) -> Result<(), GoogleSyncServiceError> {
        let now = self.clock.now();
        let claim = self
            .repository
            .claim_due(now, now + Duration::minutes(RUN_LEASE_MINUTES))
            .await?;
        let Some(claim) = claim else {
            return Ok(());
        };
        match self.sync_claim(&claim, now).await {
            Ok(counts) => {
                self.repository
                    .complete_claim(
                        &claim,
                        &counts,
                        self.clock.now(),
                        self.clock.now() + Duration::minutes(PERIODIC_SYNC_MINUTES),
                    )
                    .await?;
            }
            Err(error) => {
                let mut failure = error.failure();
                if failure.kind == SyncFailureKind::Backoff && failure.code == "provider_temporary"
                {
                    let attempts = self
                        .repository
                        .run_status(claim.account_id)
                        .await
                        .ok()
                        .flatten()
                        .map_or(0, |status| status.consecutive_failures);
                    failure.delay = exponential_backoff(attempts);
                }
                let failed_at = self.clock.now();
                self.repository
                    .fail_claim(
                        &claim,
                        failure.kind,
                        failure.code,
                        failed_at,
                        failed_at + failure.delay,
                    )
                    .await?;
                tracing::warn!(
                    account_id = %claim.account_id,
                    error_code = failure.code,
                    "Google sync account reconciliation failed"
                );
            }
        }
        Ok(())
    }

    async fn sync_claim(
        &self,
        claim: &SyncClaim,
        started_at: DateTime<Utc>,
    ) -> Result<SyncCounts, GoogleSyncServiceError> {
        // Invalidate every currently selected Calendar before discovery or
        // provider I/O. Any discovery, source-page, projection-page, protocol,
        // or persistence failure must leave scheduling fail-closed rather than
        // trusting an indefinitely stale previously-complete generation.
        self.repository
            .begin_calendar_projection_refresh(claim, started_at)
            .await?;
        let collections = self.discover_inner(claim.account_id, Some(claim)).await?;
        let account = self.oauth.account_for_sync(claim.account_id).await?;
        let mut counts = SyncCounts::default();
        for collection in collections
            .iter()
            .filter(|collection| collection.selected && !collection.provider_deleted)
        {
            let imported = match collection.kind {
                GoogleCollectionKind::Calendar => {
                    if !has_calendar_read(&account.granted_scopes) {
                        return Err(GoogleSyncServiceError::MissingReadScope);
                    }
                    self.sync_calendar(collection, started_at, claim).await?
                }
                GoogleCollectionKind::TaskList => {
                    if !has_tasks_read(&account.granted_scopes) {
                        return Err(GoogleSyncServiceError::MissingReadScope);
                    }
                    self.sync_tasks(collection, started_at, claim).await?
                }
            };
            counts.merge(&imported);
        }
        if self.outbound_enabled {
            self.process_outbound(claim).await?;
        }
        Ok(counts)
    }

    async fn sync_calendar(
        &self,
        collection: &GoogleSyncCollection,
        started_at: DateTime<Utc>,
        claim: &SyncClaim,
    ) -> Result<SyncCounts, GoogleSyncServiceError> {
        let projection_window = calendar_projection_window(started_at)?;
        let collection_key = format!("calendar:{}", collection.id);
        let stored = self
            .open_cursor(collection.account_id, &collection_key)
            .await?;
        let (mut sync_token, mut expected_revision) = match stored {
            Some((CursorValue::Calendar { sync_token }, revision)) => {
                (Some(sync_token), Some(revision))
            }
            Some(_) => return Err(GoogleSyncServiceError::CursorCorrupt),
            None => (None, None),
        };
        let mut restarted = false;
        loop {
            match self
                .sync_calendar_pages(collection, sync_token.as_deref(), claim)
                .await
            {
                Ok((mut counts, next_token, seen_remote_ids)) => {
                    if sync_token.is_none() {
                        let swept = self
                            .repository
                            .sweep_full_snapshot(
                                claim,
                                collection.id,
                                collection.revision,
                                &seen_remote_ids,
                                self.clock.now(),
                            )
                            .await?;
                        counts.merge(&swept);
                    }
                    // Scheduling consumes only this complete, bounded
                    // singleEvents projection. Fetch and normalize every page
                    // before crossing the atomic repository boundary.
                    let projection = self
                        .fetch_calendar_projection(collection, projection_window, claim)
                        .await?;
                    let projected = self
                        .repository
                        .replace_calendar_projection(claim, projection, self.clock.now())
                        .await?;
                    counts.merge(&projected.counts);
                    if !projected.complete {
                        return Err(GoogleSyncServiceError::ProviderProtocol);
                    }
                    // Do not advance the durable source cursor/last-import
                    // watermark when the scheduling projection failed. If this
                    // store races, retrying the source lane is idempotent and
                    // safer than reporting partial success.
                    self.store_cursor(
                        claim,
                        collection.id,
                        collection.revision,
                        &collection_key,
                        expected_revision,
                        &CursorValue::Calendar {
                            sync_token: next_token,
                        },
                        Some(started_at),
                    )
                    .await?;
                    return Ok(counts);
                }
                Err(GoogleSyncServiceError::Google(GoogleError::SyncTokenExpired))
                    if sync_token.is_some() && !restarted =>
                {
                    self.repository
                        .clear_cursor(
                            claim,
                            collection.id,
                            collection.revision,
                            &collection_key,
                            self.clock.now(),
                        )
                        .await?;
                    sync_token = None;
                    expected_revision = None;
                    restarted = true;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn sync_calendar_pages(
        &self,
        collection: &GoogleSyncCollection,
        sync_token: Option<&str>,
        claim: &SyncClaim,
    ) -> Result<(SyncCounts, String, Vec<String>), GoogleSyncServiceError> {
        let mut page_token = None;
        let mut seen_tokens = HashSet::new();
        let mut seen_remote_ids = HashSet::new();
        let mut counts = SyncCounts::default();
        let mut item_count = 0_usize;
        for _ in 0..MAX_PAGES {
            self.heartbeat(claim).await?;
            let options = EventListOptions {
                page_token: page_token.clone(),
                sync_token: sync_token.map(str::to_owned),
                single_events: false,
                // Cursorless reconciliation is intentionally unbounded. Google
                // requires a complete replacement after a 410, and a bounded
                // window cannot prove that an absent event was deleted or
                // merely moved outside that window.
                time_min: None,
                time_max: None,
                max_results: Some(2500),
            };
            let page = self
                .provider
                .list_events(
                    collection.account_id,
                    &collection.remote_collection_id,
                    &options,
                )
                .await?;
            for event in page.items {
                item_count += 1;
                if item_count.is_multiple_of(100) {
                    self.heartbeat(claim).await?;
                }
                if item_count > MAX_ITEMS_PER_RUN {
                    return Err(GoogleSyncServiceError::ProviderLimitExceeded);
                }
                let remote_id = event.id.clone();
                validate_remote_id(&remote_id)?;
                if !seen_remote_ids.insert(remote_id.clone()) {
                    return Err(GoogleSyncServiceError::ProviderProtocol);
                }
                match normalize_calendar_series_authenticated(
                    collection,
                    event,
                    &self.cipher,
                    self.scope,
                ) {
                    Ok(change) => {
                        let outcome = self
                            .repository
                            .apply_calendar_series_metadata(claim, change, self.clock.now())
                            .await?;
                        counts.add(outcome);
                    }
                    Err(NormalizationError::Rejected(reason)) => {
                        self.repository
                            .mark_rejected(
                                claim,
                                collection.id,
                                collection.revision,
                                &remote_id,
                                reason,
                                self.clock.now(),
                            )
                            .await?;
                        counts.rejected += 1;
                    }
                }
            }
            if let Some(next) = page.next_page_token {
                validate_page_token(&next, &mut seen_tokens)?;
                page_token = Some(next);
                continue;
            }
            return page
                .next_sync_token
                .filter(|value| valid_opaque(value, 16_384))
                .map(|token| (counts, token, seen_remote_ids.into_iter().collect()))
                .ok_or(GoogleSyncServiceError::ProviderProtocol);
        }
        Err(GoogleSyncServiceError::ProviderLimitExceeded)
    }

    async fn fetch_calendar_projection(
        &self,
        collection: &GoogleSyncCollection,
        window: CalendarProjectionWindow,
        claim: &SyncClaim,
    ) -> Result<CalendarProjectionBatch, GoogleSyncServiceError> {
        validate_calendar_projection_window(window)?;
        let mut page_token = None;
        let mut seen_tokens = HashSet::new();
        let mut seen_remote_ids = HashSet::new();
        let mut item_count = 0_usize;
        let mut normalized_bytes = 0_usize;
        let mut projection_timezone = None;
        let mut changes = Vec::new();
        let mut rejected = Vec::new();

        for _ in 0..MAX_PAGES {
            self.heartbeat(claim).await?;
            let options = expanded_event_list_options(window, page_token.clone());
            let page = self
                .provider
                .list_events(
                    collection.account_id,
                    &collection.remote_collection_id,
                    &options,
                )
                .await?;
            validate_projection_page(&page, &mut seen_remote_ids, &mut item_count)?;
            let page_timezone = consistent_projection_timezone(&page, &mut projection_timezone)?;
            let next_page_token = page.next_page_token.clone();
            normalize_calendar_projection_events(
                collection,
                window,
                &page_timezone,
                page.items,
                &self.cipher,
                self.scope,
                &mut changes,
                &mut rejected,
                &mut normalized_bytes,
            )?;

            if let Some(next) = next_page_token {
                validate_page_token(&next, &mut seen_tokens)?;
                page_token = Some(next);
                continue;
            }

            return Ok(CalendarProjectionBatch {
                account_id: collection.account_id,
                collection_id: collection.id,
                collection_revision: collection.revision,
                changes,
                rejected,
                window,
            });
        }
        Err(GoogleSyncServiceError::ProviderLimitExceeded)
    }

    async fn sync_tasks(
        &self,
        collection: &GoogleSyncCollection,
        started_at: DateTime<Utc>,
        claim: &SyncClaim,
    ) -> Result<SyncCounts, GoogleSyncServiceError> {
        let collection_key = format!("tasks:{}", collection.id);
        let stored = self
            .open_cursor(collection.account_id, &collection_key)
            .await?;
        let (updated_min, expected_revision) = match stored {
            Some((CursorValue::Tasks { updated_min }, revision)) => (
                Some((updated_min - Duration::minutes(TASK_CURSOR_OVERLAP_MINUTES)).to_rfc3339()),
                Some(revision),
            ),
            Some(_) => return Err(GoogleSyncServiceError::CursorCorrupt),
            None => (None, None),
        };
        let full_snapshot = updated_min.is_none();
        let remote_collection_id = collection.remote_collection_id.clone();
        let mut page_token = None;
        let mut seen_tokens = HashSet::new();
        let mut seen_remote_ids = HashSet::new();
        let mut counts = SyncCounts::default();
        let mut item_count = 0_usize;
        for _ in 0..MAX_PAGES {
            self.heartbeat(claim).await?;
            let page = self
                .provider
                .list_tasks(
                    collection.account_id,
                    &remote_collection_id,
                    page_token.as_deref(),
                    updated_min.as_deref(),
                )
                .await?;
            for task in page.items {
                item_count += 1;
                if item_count.is_multiple_of(100) {
                    self.heartbeat(claim).await?;
                }
                if item_count > MAX_ITEMS_PER_RUN {
                    return Err(GoogleSyncServiceError::ProviderLimitExceeded);
                }
                let remote_id = task.id.clone();
                seen_remote_ids.insert(remote_id.clone());
                match normalize_task(collection, task) {
                    Ok(change) => {
                        let outcome = self
                            .repository
                            .apply_remote_item(claim, change, self.clock.now())
                            .await?;
                        counts.add(outcome);
                    }
                    Err(NormalizationError::Rejected(reason)) => {
                        self.repository
                            .mark_rejected(
                                claim,
                                collection.id,
                                collection.revision,
                                &remote_id,
                                reason,
                                self.clock.now(),
                            )
                            .await?;
                        counts.rejected += 1;
                    }
                }
            }
            let Some(next) = page.next_page_token else {
                if full_snapshot {
                    let seen_remote_ids = seen_remote_ids.into_iter().collect::<Vec<_>>();
                    let swept = self
                        .repository
                        .sweep_full_snapshot(
                            claim,
                            collection.id,
                            collection.revision,
                            &seen_remote_ids,
                            self.clock.now(),
                        )
                        .await?;
                    counts.merge(&swept);
                }
                self.store_cursor(
                    claim,
                    collection.id,
                    collection.revision,
                    &collection_key,
                    expected_revision,
                    &CursorValue::Tasks {
                        updated_min: started_at,
                    },
                    Some(started_at),
                )
                .await?;
                return Ok(counts);
            };
            validate_page_token(&next, &mut seen_tokens)?;
            page_token = Some(next);
        }
        Err(GoogleSyncServiceError::ProviderLimitExceeded)
    }

    async fn open_cursor(
        &self,
        account_id: Uuid,
        collection_key: &str,
    ) -> Result<Option<(CursorValue, u64)>, GoogleSyncServiceError> {
        let Some(stored) = self.repository.cursor(account_id, collection_key).await? else {
            return Ok(None);
        };
        let mut plaintext = self.cipher.open(
            stored.key_version,
            &stored.encrypted,
            &sync_cursor_aad(
                self.scope.workspace_id,
                self.scope.user_id,
                account_id,
                collection_key,
            ),
        )?;
        let cursor =
            serde_json::from_slice(&plaintext).map_err(|_| GoogleSyncServiceError::CursorCorrupt);
        plaintext.zeroize();
        Ok(Some((cursor?, stored.revision)))
    }

    #[allow(clippy::too_many_arguments)]
    async fn store_cursor(
        &self,
        claim: &SyncClaim,
        collection_id: Uuid,
        collection_revision: u64,
        collection_key: &str,
        expected_revision: Option<u64>,
        cursor: &CursorValue,
        watermark_at: Option<DateTime<Utc>>,
    ) -> Result<(), GoogleSyncServiceError> {
        let plaintext = Zeroizing::new(
            serde_json::to_vec(cursor).map_err(|_| GoogleSyncServiceError::Internal)?,
        );
        let sealed = self.cipher.seal(
            &plaintext,
            &sync_cursor_aad(
                self.scope.workspace_id,
                self.scope.user_id,
                claim.account_id,
                collection_key,
            ),
        )?;
        self.repository
            .store_cursor(
                claim,
                collection_id,
                collection_revision,
                collection_key,
                expected_revision,
                sealed.ciphertext.clone(),
                sealed.key_version,
                watermark_at,
                self.clock.now(),
            )
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // Keeps every durable delivery outcome beside its state transition.
    async fn process_outbound(&self, claim: &SyncClaim) -> Result<(), GoogleSyncServiceError> {
        for _ in 0..MAX_OUTBOUND_PER_RUN {
            self.heartbeat(claim).await?;
            let Some(work) = self
                .repository
                .claim_outbound(claim, self.clock.now())
                .await?
            else {
                return Ok(());
            };
            match self.deliver_outbound(&work).await {
                Ok(result) => {
                    match self
                        .repository
                        .complete_outbound(&work, result, self.clock.now())
                        .await
                    {
                        Ok(()) | Err(GoogleSyncRepositoryError::ClaimLost) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(GoogleSyncServiceError::Google(GoogleError::PreconditionFailed)) => {
                    self.repository
                        .fail_outbound(
                            &work,
                            "conflict",
                            "precondition_failed",
                            self.clock.now(),
                            self.clock.now(),
                        )
                        .await?;
                }
                Err(GoogleSyncServiceError::Google(GoogleError::ConditionalWriteRequired)) => {
                    self.repository
                        .fail_outbound(
                            &work,
                            "conflict",
                            "conditional_write_required",
                            self.clock.now(),
                            self.clock.now(),
                        )
                        .await?;
                }
                Err(GoogleSyncServiceError::ProviderIdentityUnresolved) => {
                    self.repository
                        .fail_outbound(
                            &work,
                            "conflict",
                            "provider_identity_unresolved",
                            self.clock.now(),
                            self.clock.now(),
                        )
                        .await?;
                }
                Err(GoogleSyncServiceError::Repository(GoogleSyncRepositoryError::ClaimLost)) => {
                    // A concurrent account/collection/item guardian revoked or
                    // superseded this delivery. Its transaction already made
                    // the durable terminal state visible; never call Google or
                    // rewrite that operator-facing reason from this stale work.
                }
                Err(GoogleSyncServiceError::Google(GoogleError::Api { status })) => {
                    let code = if status == 404 {
                        "provider_not_found"
                    } else {
                        "provider_rejected"
                    };
                    self.repository
                        .fail_outbound(&work, "conflict", code, self.clock.now(), self.clock.now())
                        .await?;
                }
                Err(error) => {
                    let mut failure = error.failure();
                    if failure.kind == SyncFailureKind::Backoff
                        && failure.code == "provider_temporary"
                    {
                        failure.delay = exponential_backoff(work.attempts);
                    }
                    let state = if matches!(
                        failure.kind,
                        SyncFailureKind::Backoff | SyncFailureKind::ReauthorizationRequired
                    ) {
                        "backoff"
                    } else {
                        "failed"
                    };
                    self.repository
                        .fail_outbound(
                            &work,
                            state,
                            failure.code,
                            self.clock.now() + failure.delay,
                            self.clock.now(),
                        )
                        .await?;
                    if failure.kind != SyncFailureKind::Failed {
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    async fn heartbeat(&self, claim: &SyncClaim) -> Result<(), GoogleSyncServiceError> {
        let now = self.clock.now();
        self.repository
            .renew_claim(claim, now, now + Duration::minutes(RUN_LEASE_MINUTES))
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // Keeps all guarded provider delivery variants together.
    async fn deliver_outbound(
        &self,
        work: &OutboundWork,
    ) -> Result<OutboundResult, GoogleSyncServiceError> {
        self.require_outbound_enabled()?;
        match (work.entity_kind.as_str(), work.operation) {
            ("calendar_event", OutboundOperation::Upsert) => {
                let mut event: GoogleEvent = serde_json::from_value(work.payload.clone())
                    .map_err(|_| GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                let (result, dispatch_nonce) = if let Some(remote_id) = &work.remote_resource_id {
                    event.id.clone_from(remote_id);
                    event.etag.clone_from(&work.expected_etag);
                    let (response, permit) = self
                        .execute_guarded_write(
                            work,
                            self.provider.prepare_update_event(
                                work.account_id,
                                &work.collection_remote_id,
                                &event,
                            ),
                        )
                        .await?;
                    let ProviderWriteResponse::Event(result) = response else {
                        return Err(GoogleSyncServiceError::ProviderProtocol);
                    };
                    let result = *result;
                    (result, permit.nonce)
                } else {
                    self.repository
                        .renew_outbound(work, self.clock.now())
                        .await?;
                    match self
                        .provider
                        .get_event(work.account_id, &work.collection_remote_id, &event.id)
                        .await
                    {
                        Ok(found) => {
                            if !calendar_event_owned_by(
                                &found,
                                work.item_id,
                                work.account_id,
                                work.collection_id,
                                &self.cipher,
                                self.scope,
                            ) || !calendar_event_matches_intent(&found, &event)
                            {
                                return Err(GoogleError::PreconditionFailed.into());
                            }
                            if found.etag.is_none() {
                                return Err(GoogleSyncServiceError::ProviderProtocol);
                            }
                            // A previous create may have succeeded even when
                            // its response was lost. Adopt only byte-for-byte
                            // semantic intent under the authenticated proof;
                            // never overwrite a provider-side edit while
                            // trying to recover that uncertain result.
                            let permit = self.authorize_dispatch(work, false).await?;
                            (found, permit.nonce)
                        }
                        Err(GoogleError::Api { status: 404 }) => {
                            let (response, permit) = self
                                .execute_guarded_write(
                                    work,
                                    self.provider.prepare_insert_event(
                                        work.account_id,
                                        &work.collection_remote_id,
                                        &event,
                                    ),
                                )
                                .await?;
                            let ProviderWriteResponse::Event(result) = response else {
                                return Err(GoogleSyncServiceError::ProviderProtocol);
                            };
                            let result = *result;
                            (result, permit.nonce)
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                if work.remote_resource_id.is_none() && result.id != event.id {
                    return Err(GoogleSyncServiceError::ProviderProtocol);
                }
                outbound_event_result(&result, dispatch_nonce)
            }
            ("calendar_event", OutboundOperation::Delete) => {
                let remote_id = work
                    .remote_resource_id
                    .as_deref()
                    .ok_or(GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                let etag = work
                    .expected_etag
                    .as_deref()
                    .filter(|etag| !etag.trim().is_empty())
                    .ok_or(GoogleSyncServiceError::ProviderProtocol)?;
                let (response, permit) = self
                    .execute_guarded_write(
                        work,
                        self.provider.prepare_delete_event(
                            work.account_id,
                            &work.collection_remote_id,
                            remote_id,
                            etag,
                        ),
                    )
                    .await?;
                if !matches!(response, ProviderWriteResponse::Empty) {
                    return Err(GoogleSyncServiceError::ProviderProtocol);
                }
                Ok(OutboundResult {
                    remote_resource_id: remote_id.to_owned(),
                    remote_etag: None,
                    remote_updated_at: None,
                    payload_hash: Sha256::digest(b"deleted").into(),
                    dispatch_nonce: permit.nonce,
                })
            }
            ("task", OutboundOperation::Upsert) => {
                let mut task: GoogleTask = serde_json::from_value(work.payload.clone())
                    .map_err(|_| GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                if let Some(remote_id) = &work.remote_resource_id {
                    task.id.clone_from(remote_id);
                    task.etag.clone_from(&work.expected_etag);
                    let (response, permit) = self
                        .execute_guarded_write(
                            work,
                            self.provider.prepare_update_task(
                                work.account_id,
                                &work.collection_remote_id,
                                &task,
                            ),
                        )
                        .await?;
                    let ProviderWriteResponse::Task(result) = response else {
                        return Err(GoogleSyncServiceError::ProviderProtocol);
                    };
                    outbound_task_result(&result, permit.nonce)
                } else {
                    guard_new_task_insert(work.provider_post_may_have_started)?;
                    self.execute_markerless_task_create(
                        work,
                        self.provider.prepare_insert_task(
                            work.account_id,
                            &work.collection_remote_id,
                            &task,
                        ),
                    )
                    .await
                }
            }
            ("task", OutboundOperation::Delete) => {
                let remote_id = work
                    .remote_resource_id
                    .as_deref()
                    .ok_or(GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                let etag = work
                    .expected_etag
                    .as_deref()
                    .filter(|etag| !etag.trim().is_empty())
                    .ok_or(GoogleSyncServiceError::ProviderProtocol)?;
                let (response, permit) = self
                    .execute_guarded_write(
                        work,
                        self.provider.prepare_delete_task(
                            work.account_id,
                            &work.collection_remote_id,
                            remote_id,
                            etag,
                        ),
                    )
                    .await?;
                if !matches!(response, ProviderWriteResponse::Empty) {
                    return Err(GoogleSyncServiceError::ProviderProtocol);
                }
                Ok(OutboundResult {
                    remote_resource_id: remote_id.to_owned(),
                    remote_etag: None,
                    remote_updated_at: None,
                    payload_hash: Sha256::digest(b"deleted").into(),
                    dispatch_nonce: permit.nonce,
                })
            }
            _ => Err(GoogleSyncServiceError::OutboundPayloadCorrupt),
        }
    }

    async fn authorize_dispatch(
        &self,
        work: &OutboundWork,
        provider_write: bool,
    ) -> Result<super::OutboundDispatchPermit, GoogleSyncServiceError> {
        let now = self.clock.now();
        let permit = self
            .repository
            .authorize_outbound_dispatch(work, provider_write, now)
            .await?;
        if permit.intent_hash != work.intent_hash || permit.expires_at <= now {
            return Err(GoogleSyncRepositoryError::ClaimLost.into());
        }
        Ok(permit)
    }

    async fn prepare_write<F>(
        &self,
        preparation: F,
    ) -> Result<Box<dyn PreparedGoogleSyncWrite>, GoogleSyncServiceError>
    where
        F: Future<Output = Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>>,
    {
        tokio::time::timeout(
            StdDuration::from_secs(DISPATCH_PREPARATION_TIMEOUT_SECONDS),
            preparation,
        )
        .await
        .map_err(|_| GoogleSyncServiceError::DispatchPreparationTimeout)?
        .map_err(Into::into)
    }

    /// Google Tasks cannot recover a successful create whose response identity
    /// was lost. Keep preparation failures retryable without consuming the one
    /// safe POST, then classify every ambiguous outcome after final dispatch
    /// authorization as a durable identity conflict.
    async fn execute_markerless_task_create<F>(
        &self,
        work: &OutboundWork,
        preparation: F,
    ) -> Result<OutboundResult, GoogleSyncServiceError>
    where
        F: Future<Output = Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>>,
    {
        // OAuth refresh and request construction happen before the repository
        // records `provider_post_may_have_started`. Their failures prove that
        // no provider POST was initiated and must not consume the one attempt.
        let prepared = self.prepare_write(preparation).await?;
        let permit = self.authorize_dispatch(work, true).await?;
        match self.send_prepared(work, prepared, &permit).await {
            Ok(response) => markerless_task_create_result(response, permit.nonce),
            // A request transport break, a provider 5xx, or a malformed 2xx
            // body can all follow successful object creation. No marker or
            // client-selected task ID exists with which to reconcile safely.
            Err(GoogleSyncServiceError::Google(
                GoogleError::Transport(_) | GoogleError::Temporary { .. },
            )) => Err(GoogleSyncServiceError::ProviderIdentityUnresolved),
            // Explicit non-success provider responses prove that the create
            // was rejected. Dispatch expiry is separately cancelled in
            // `send_prepared` before it reaches this branch.
            Err(error) => Err(error),
        }
    }

    async fn execute_guarded_write<F>(
        &self,
        work: &OutboundWork,
        preparation: F,
    ) -> Result<(ProviderWriteResponse, super::OutboundDispatchPermit), GoogleSyncServiceError>
    where
        F: Future<Output = Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>>,
    {
        sequence_guarded_write(
            self.prepare_write(preparation),
            || self.authorize_dispatch(work, true),
            |prepared, permit| async move {
                let response = self.send_prepared(work, prepared, &permit).await?;
                Ok((response, permit))
            },
        )
        .await
    }

    async fn send_prepared(
        &self,
        work: &OutboundWork,
        prepared: Box<dyn PreparedGoogleSyncWrite>,
        permit: &super::OutboundDispatchPermit,
    ) -> Result<ProviderWriteResponse, GoogleSyncServiceError> {
        match prepared.send(SystemTime::from(permit.expires_at)).await {
            Err(GoogleError::DispatchInitiationExpired) => {
                let now = self.clock.now();
                self.repository
                    .cancel_outbound_before_send(
                        work,
                        "dispatch_initiation_expired_before_send",
                        now + Duration::seconds(30),
                        now,
                    )
                    .await?;
                Err(GoogleSyncRepositoryError::ClaimLost.into())
            }
            result => result.map_err(Into::into),
        }
    }
}

fn calendar_projection_window(
    started_at: DateTime<Utc>,
) -> Result<CalendarProjectionWindow, GoogleSyncServiceError> {
    let anchor = truncate_to_microseconds(started_at)?;
    let start = anchor
        .checked_sub_signed(Duration::days(CALENDAR_PROJECTION_LOOKBACK_DAYS))
        .ok_or(GoogleSyncServiceError::Internal)?;
    let end = anchor
        .checked_add_signed(Duration::days(CALENDAR_PROJECTION_LOOKAHEAD_DAYS))
        .ok_or(GoogleSyncServiceError::Internal)?;
    let window = CalendarProjectionWindow { start, end };
    validate_calendar_projection_window(window)?;
    Ok(window)
}

fn validate_calendar_projection_window(
    window: CalendarProjectionWindow,
) -> Result<(), GoogleSyncServiceError> {
    if window.start >= window.end
        || window.end - window.start > Duration::days(MAX_CALENDAR_PROJECTION_WINDOW_DAYS)
        || !window.start.timestamp_subsec_nanos().is_multiple_of(1_000)
        || !window.end.timestamp_subsec_nanos().is_multiple_of(1_000)
    {
        return Err(GoogleSyncServiceError::InvalidRequest);
    }
    Ok(())
}

fn expanded_event_list_options(
    window: CalendarProjectionWindow,
    page_token: Option<String>,
) -> EventListOptions {
    EventListOptions {
        page_token,
        sync_token: None,
        single_events: true,
        time_min: Some(window.start.to_rfc3339_opts(SecondsFormat::Micros, true)),
        time_max: Some(window.end.to_rfc3339_opts(SecondsFormat::Micros, true)),
        max_results: Some(2500),
    }
}

fn validate_projection_page(
    page: &EventListPage,
    seen_remote_ids: &mut HashSet<String>,
    item_count: &mut usize,
) -> Result<(), GoogleSyncServiceError> {
    let Some(timezone) = page.time_zone.as_deref() else {
        return Err(GoogleSyncServiceError::ProviderProtocol);
    };
    if !valid_opaque(timezone, 255) || timezone.parse::<Tz>().is_err() {
        return Err(GoogleSyncServiceError::ProviderProtocol);
    }
    *item_count = item_count
        .checked_add(page.items.len())
        .ok_or(GoogleSyncServiceError::ProviderLimitExceeded)?;
    if *item_count > MAX_CALENDAR_PROJECTION_ITEMS {
        return Err(GoogleSyncServiceError::ProviderLimitExceeded);
    }
    for event in &page.items {
        validate_remote_id(&event.id)?;
        if !seen_remote_ids.insert(event.id.clone()) {
            return Err(GoogleSyncServiceError::ProviderProtocol);
        }
    }
    Ok(())
}

fn consistent_projection_timezone(
    page: &EventListPage,
    expected: &mut Option<String>,
) -> Result<String, GoogleSyncServiceError> {
    let timezone = page
        .time_zone
        .as_deref()
        .ok_or(GoogleSyncServiceError::ProviderProtocol)?;
    if expected
        .as_deref()
        .is_some_and(|expected| expected != timezone)
    {
        return Err(GoogleSyncServiceError::ProviderProtocol);
    }
    if expected.is_none() {
        *expected = Some(timezone.to_owned());
    }
    Ok(timezone.to_owned())
}

#[allow(clippy::too_many_arguments)] // One page is normalized and released before fetching the next.
fn normalize_calendar_projection_events(
    collection: &GoogleSyncCollection,
    window: CalendarProjectionWindow,
    page_timezone: &str,
    events: Vec<GoogleEvent>,
    cipher: &SecretCipher,
    scope: OAuthScope,
    changes: &mut Vec<RemoteItemChange>,
    rejected: &mut Vec<RejectedRemoteItem>,
    normalized_bytes: &mut usize,
) -> Result<(), GoogleSyncServiceError> {
    for event in events {
        let remote_id = event.id.clone();
        match normalize_event_authenticated(collection, page_timezone, event, cipher, scope) {
            Ok(change) => {
                if let Some(item) = change.item.as_ref() {
                    let starts_at = item
                        .earliest_start_at
                        .ok_or(GoogleSyncServiceError::ProviderProtocol)?;
                    let ends_at = item
                        .deadline_at
                        .ok_or(GoogleSyncServiceError::ProviderProtocol)?;
                    if starts_at >= window.end || ends_at <= window.start {
                        return Err(GoogleSyncServiceError::ProviderProtocol);
                    }
                }
                *normalized_bytes = normalized_bytes
                    .checked_add(calendar_projection_change_bytes(&change)?)
                    .ok_or(GoogleSyncServiceError::ProviderLimitExceeded)?;
                if *normalized_bytes > MAX_CALENDAR_PROJECTION_NORMALIZED_BYTES {
                    return Err(GoogleSyncServiceError::ProviderLimitExceeded);
                }
                changes.push(change);
            }
            Err(NormalizationError::Rejected(reason)) => {
                *normalized_bytes = normalized_bytes
                    .checked_add(
                        remote_id
                            .len()
                            .saturating_add(reason.len())
                            .saturating_add(64),
                    )
                    .ok_or(GoogleSyncServiceError::ProviderLimitExceeded)?;
                if *normalized_bytes > MAX_CALENDAR_PROJECTION_NORMALIZED_BYTES {
                    return Err(GoogleSyncServiceError::ProviderLimitExceeded);
                }
                rejected.push(RejectedRemoteItem { remote_id, reason });
            }
        }
    }
    Ok(())
}

fn calendar_projection_change_bytes(
    change: &RemoteItemChange,
) -> Result<usize, GoogleSyncServiceError> {
    let mut bytes = 256_usize
        .checked_add(change.remote_id.len())
        .ok_or(GoogleSyncServiceError::ProviderLimitExceeded)?;
    if let Some(parent_id) = change.remote_parent_id.as_ref() {
        bytes = bytes
            .checked_add(parent_id.len())
            .ok_or(GoogleSyncServiceError::ProviderLimitExceeded)?;
    }
    if let Some(etag) = change.remote_etag.as_ref() {
        bytes = bytes
            .checked_add(etag.len())
            .ok_or(GoogleSyncServiceError::ProviderLimitExceeded)?;
    }
    if let Some(item) = change.item.as_ref() {
        bytes = bytes
            .checked_add(
                serde_json::to_vec(item)
                    .map_err(|_| GoogleSyncServiceError::Internal)?
                    .len(),
            )
            .ok_or(GoogleSyncServiceError::ProviderLimitExceeded)?;
    }
    if let Some(reviewed) = change.reviewed_provider_projection.as_ref() {
        bytes = bytes
            .checked_add(
                serde_json::to_vec(reviewed)
                    .map_err(|_| GoogleSyncServiceError::Internal)?
                    .len(),
            )
            .ok_or(GoogleSyncServiceError::ProviderLimitExceeded)?;
    }
    Ok(bytes)
}

#[cfg(test)]
fn normalize_calendar_projection_pages(
    collection: &GoogleSyncCollection,
    window: CalendarProjectionWindow,
    pages: Vec<EventListPage>,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<CalendarProjectionBatch, GoogleSyncServiceError> {
    validate_calendar_projection_window(window)?;
    if pages.is_empty() || pages.len() > MAX_PAGES {
        return Err(GoogleSyncServiceError::ProviderLimitExceeded);
    }
    let page_count = pages.len();
    let mut validated_ids = HashSet::new();
    let mut validated_tokens = HashSet::new();
    let mut validated_count = 0_usize;
    let mut projection_timezone = None;
    let mut changes = Vec::new();
    let mut rejected = Vec::new();
    let mut normalized_bytes = 0_usize;
    for (index, page) in pages.into_iter().enumerate() {
        validate_projection_page(&page, &mut validated_ids, &mut validated_count)?;
        let page_timezone = consistent_projection_timezone(&page, &mut projection_timezone)?;
        match page.next_page_token.as_deref() {
            Some(token) if index + 1 < page_count => {
                validate_page_token(token, &mut validated_tokens)?;
            }
            None if index + 1 == page_count => {}
            _ => return Err(GoogleSyncServiceError::ProviderProtocol),
        }
        normalize_calendar_projection_events(
            collection,
            window,
            &page_timezone,
            page.items,
            cipher,
            scope,
            &mut changes,
            &mut rejected,
            &mut normalized_bytes,
        )?;
    }

    Ok(CalendarProjectionBatch {
        account_id: collection.account_id,
        collection_id: collection.id,
        collection_revision: collection.revision,
        changes,
        rejected,
        window,
    })
}

fn normalize_calendar_series_authenticated(
    collection: &GoogleSyncCollection,
    event: GoogleEvent,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<RemoteCalendarSeriesChange, NormalizationError> {
    validate_remote_id(&event.id).map_err(|_| NormalizationError::Rejected("invalid_remote_id"))?;
    let remote_payload_hash = payload_hash(&event)?;
    let remote_projection_hash = projection_hash(remote_payload_hash, collection)?;
    let dayweave_item_id = calendar_dayweave_item_id(&event, collection, cipher, scope)?;
    let remote_updated_at = parse_optional_timestamp(event.updated.as_deref())?;
    let self_declined = event
        .attendees
        .iter()
        .any(|attendee| attendee.self_ && attendee.response_status.as_deref() == Some("declined"));
    let deleted = event.status.as_deref() == Some("cancelled") || self_declined;
    let reviewed_provider_projection = (dayweave_item_id.is_some() && !deleted)
        .then(|| calendar_reviewed_projection(&event))
        .transpose()
        .map_err(|_| NormalizationError::Rejected("provider_payload_invalid"))?;

    Ok(RemoteCalendarSeriesChange {
        account_id: collection.account_id,
        collection_id: collection.id,
        collection_revision: collection.revision,
        dayweave_item_id,
        remote_id: event.id,
        remote_etag: bounded_optional(event.etag.as_deref(), 1000)?,
        remote_updated_at,
        remote_payload_hash,
        remote_projection_hash,
        reviewed_provider_projection,
        deleted,
    })
}

#[allow(clippy::too_many_lines)] // The projection intentionally centralizes all event semantics.
fn normalize_event_authenticated(
    collection: &GoogleSyncCollection,
    page_timezone: &str,
    event: GoogleEvent,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<RemoteItemChange, NormalizationError> {
    validate_remote_id(&event.id).map_err(|_| NormalizationError::Rejected("invalid_remote_id"))?;
    let remote_hash = payload_hash(&event)?;
    let dayweave_item_id = calendar_dayweave_item_id(&event, collection, cipher, scope)?;
    // Provider recovery material is useful only for an authenticated
    // DayWeave-owned echo. External event content, especially private content,
    // must not gain a second durable copy in the provider mapping.
    let reviewed_provider_projection = dayweave_item_id
        .is_some()
        .then(|| calendar_reviewed_projection(&event))
        .transpose()
        .map_err(|_| NormalizationError::Rejected("provider_payload_invalid"))?;
    let remote_updated_at = parse_optional_timestamp(event.updated.as_deref())?;
    let self_response = event
        .attendees
        .iter()
        .find(|attendee| attendee.self_)
        .and_then(|attendee| attendee.response_status.as_deref());
    if event.status.as_deref() == Some("cancelled") || self_response == Some("declined") {
        return Ok(RemoteItemChange {
            account_id: collection.account_id,
            collection_id: collection.id,
            collection_revision: collection.revision,
            dayweave_item_id,
            remote_id: event.id,
            remote_parent_id: event.recurring_event_id,
            remote_etag: bounded_optional(event.etag.as_deref(), 1000)?,
            remote_updated_at,
            remote_payload_hash: remote_hash,
            remote_projection_hash: calendar_occurrence_projection_hash(None)?,
            reviewed_provider_projection,
            item: None,
        });
    }
    // `singleEvents=true` must return concrete occurrences. A live master in
    // this bounded lane would otherwise become a malformed canonical event.
    if !event.recurrence.is_empty() {
        return Err(NormalizationError::Rejected("provider_payload_invalid"));
    }
    let start = event
        .start
        .as_ref()
        .ok_or(NormalizationError::Rejected("event_bounds_missing"))?;
    let end = event
        .end
        .as_ref()
        .ok_or(NormalizationError::Rejected("event_bounds_missing"))?;
    let (starts_at, timezone_name, all_day) = parse_event_bound(start, page_timezone)?;
    let (ends_at, end_timezone_name, end_all_day) = parse_event_bound(end, &timezone_name)?;
    if ends_at <= starts_at
        || all_day != end_all_day
        || (all_day && timezone_name != end_timezone_name)
    {
        return Err(NormalizationError::Rejected("event_bounds_invalid"));
    }
    let duration = (ends_at - starts_at)
        .num_seconds()
        .try_into()
        .map_err(|_| NormalizationError::Rejected("event_duration_invalid"))?;
    let event_type = event.event_type.as_deref().unwrap_or("default");
    let disposition = event_disposition(collection, &event, event_type, all_day);
    if disposition == GoogleEventDisposition::Ignore {
        return Ok(RemoteItemChange {
            account_id: collection.account_id,
            collection_id: collection.id,
            collection_revision: collection.revision,
            dayweave_item_id,
            remote_id: event.id,
            remote_parent_id: event.recurring_event_id,
            remote_etag: bounded_optional(event.etag.as_deref(), 1000)?,
            remote_updated_at,
            remote_payload_hash: remote_hash,
            remote_projection_hash: calendar_occurrence_projection_hash(None)?,
            reviewed_provider_projection,
            item: None,
        });
    }
    let blocking = disposition == GoogleEventDisposition::Blocking
        && matches!(
            collection.sync_role,
            GoogleSyncRole::Blocking | GoogleSyncRole::Writable
        );
    let provider_private = event.visibility.as_deref().is_some_and(|visibility| {
        visibility.eq_ignore_ascii_case("private")
            || visibility.eq_ignore_ascii_case("confidential")
    });
    let protected = !collection.visible || provider_private;
    let fallback_title = if protected || blocking {
        "Busy"
    } else {
        "Google calendar event"
    };
    let title = if protected {
        "Busy".to_owned()
    } else {
        bounded_title(
            event.summary.as_deref().unwrap_or(fallback_title),
            fallback_title,
        )
        .0
    };
    let notes = if protected {
        None
    } else {
        bounded_notes(event.description.as_deref()).0
    };
    let constraints = if blocking {
        json!({
            "calendar_event": {
                "start": starts_at,
                "end": ends_at,
                "immutable": true,
                "all_day": all_day,
                "source_calendar_id": Value::Null,
            }
        })
    } else {
        json!({
            "calendar_context": {
                "start": starts_at,
                "end": ends_at,
                "all_day": all_day,
            }
        })
    };
    let item_id = Uuid::new_v4();
    let remote_id = event.id;
    let item = NewItem {
        id: item_id,
        is_sensitive: protected,
        kind: ItemKind::Event,
        status: ItemStatus::Scheduled,
        title,
        notes,
        timezone_name,
        duration_seconds: Some(duration),
        deadline_at: Some(ends_at),
        earliest_start_at: Some(starts_at),
        recurrence: None,
        flexible_constraints: constraints,
        split_policy: SplitPolicy::Indivisible,
        importance: 0,
        urgency: 0,
        parent_id: None,
        sibling_order: 0,
    };
    validate_normalized_item(&item)?;
    let remote_projection_hash = calendar_occurrence_projection_hash(Some(&item))?;
    Ok(RemoteItemChange {
        account_id: collection.account_id,
        collection_id: collection.id,
        collection_revision: collection.revision,
        dayweave_item_id,
        remote_id,
        remote_parent_id: event.recurring_event_id,
        remote_etag: bounded_optional(event.etag.as_deref(), 1000)?,
        remote_updated_at,
        remote_payload_hash: remote_hash,
        remote_projection_hash,
        reviewed_provider_projection,
        item: Some(item),
    })
}

#[cfg(test)]
fn normalize_event(
    collection: &GoogleSyncCollection,
    page_timezone: &str,
    event: GoogleEvent,
) -> Result<RemoteItemChange, NormalizationError> {
    let cipher = test_marker_cipher();
    normalize_event_authenticated(
        collection,
        page_timezone,
        event,
        &cipher,
        test_oauth_scope(),
    )
}

#[cfg(test)]
fn test_marker_cipher() -> SecretCipher {
    SecretCipher::new(
        Arc::new(BTreeMap::from([(
            1,
            crate::config::CredentialKey::from_test_bytes([0x5a; 32]),
        )])),
        1,
    )
}

#[cfg(test)]
fn test_oauth_scope() -> OAuthScope {
    OAuthScope {
        workspace_id: Uuid::from_u128(0x100),
        user_id: Uuid::from_u128(0x200),
    }
}

fn event_disposition(
    collection: &GoogleSyncCollection,
    event: &GoogleEvent,
    event_type: &str,
    all_day: bool,
) -> GoogleEventDisposition {
    // Provider semantic event types override every configurable busy/free or
    // all-day policy: birthdays and working location are always retained as
    // context, while out-of-office/focus time uses confirmed-busy policy.
    if matches!(event_type, "birthday" | "workingLocation") {
        return GoogleEventDisposition::VisibleNonblocking;
    }
    if matches!(event_type, "outOfOffice" | "focusTime") {
        return collection.calendar_policy.confirmed_busy;
    }
    if all_day {
        return collection.calendar_policy.all_day;
    }
    if event.status.as_deref() == Some("tentative") {
        return collection.calendar_policy.tentative;
    }
    if event.transparency.as_deref() == Some("transparent") {
        return collection.calendar_policy.free;
    }
    collection.calendar_policy.confirmed_busy
}

fn normalize_task(
    collection: &GoogleSyncCollection,
    task: GoogleTask,
) -> Result<RemoteItemChange, NormalizationError> {
    validate_remote_id(&task.id).map_err(|_| NormalizationError::Rejected("invalid_remote_id"))?;
    let remote_hash = payload_hash(&task)?;
    let remote_projection_hash = projection_hash(remote_hash, collection)?;
    // Google Tasks exposes notes to every client and offers no private
    // extended-property namespace. Never interpret visible text as ownership
    // proof, and never let a legacy raw item UUID flow into canonical notes.
    let dayweave_item_id = None;
    let (sanitized_notes, legacy_marker_stripped) = sanitize_task_notes(task.notes.as_deref());
    let remote_updated_at = parse_optional_timestamp(task.updated.as_deref())?;
    if task.deleted {
        return Ok(RemoteItemChange {
            account_id: collection.account_id,
            collection_id: collection.id,
            collection_revision: collection.revision,
            dayweave_item_id,
            remote_id: task.id,
            remote_parent_id: task.parent,
            remote_etag: bounded_optional(task.etag.as_deref(), 1000)?,
            remote_updated_at,
            remote_payload_hash: remote_hash,
            remote_projection_hash,
            reviewed_provider_projection: None,
            item: None,
        });
    }
    let (title, title_truncated) = if collection.visible {
        bounded_title(&task.title, "Google task")
    } else {
        ("Google task".to_owned(), false)
    };
    let (notes, notes_truncated) = if collection.visible {
        bounded_notes(sanitized_notes.as_deref())
    } else {
        (None, false)
    };
    let due = task.due.as_deref().map(parse_timestamp).transpose()?;
    let provider_completed_at = parse_optional_timestamp(task.completed.as_deref())?;
    let completed = task.status.as_deref() == Some("completed") || provider_completed_at.is_some();
    let constraints = json!({
        "google_sync": {
            "account_id": collection.account_id,
            "collection_id": collection.id,
            "remote_id": task.id,
            "remote_parent_id": task.parent,
            "position": task.position,
            "hidden": task.hidden,
            "provider_completed_at": provider_completed_at,
            "visible": collection.visible,
            "content_truncated": title_truncated || notes_truncated,
            "legacy_marker_stripped": legacy_marker_stripped,
        }
    });
    let remote_id = constraints["google_sync"]["remote_id"]
        .as_str()
        .ok_or(NormalizationError::Rejected("invalid_remote_id"))?
        .to_owned();
    let item = NewItem {
        id: Uuid::new_v4(),
        is_sensitive: !collection.visible,
        kind: ItemKind::Task,
        status: if completed {
            ItemStatus::Completed
        } else {
            ItemStatus::Inbox
        },
        title,
        notes,
        timezone_name: "UTC".to_owned(),
        duration_seconds: None,
        deadline_at: due,
        earliest_start_at: None,
        recurrence: None,
        flexible_constraints: constraints,
        split_policy: SplitPolicy::Indivisible,
        importance: 0,
        urgency: 0,
        parent_id: None,
        sibling_order: 0,
    };
    validate_normalized_item(&item)?;
    Ok(RemoteItemChange {
        account_id: collection.account_id,
        collection_id: collection.id,
        collection_revision: collection.revision,
        dayweave_item_id,
        remote_id,
        remote_parent_id: task.parent,
        remote_etag: bounded_optional(task.etag.as_deref(), 1000)?,
        remote_updated_at,
        remote_payload_hash: remote_hash,
        remote_projection_hash,
        reviewed_provider_projection: None,
        item: Some(item),
    })
}

fn validate_normalized_item(item: &NewItem) -> Result<(), NormalizationError> {
    crate::items::Item::new(item.clone(), Utc::now())
        .map(|_| ())
        .map_err(|_| NormalizationError::Rejected("canonical_item_invalid"))
}

#[allow(clippy::too_many_lines)] // Keeps the reviewed provider projection in one auditable function.
fn prepare_calendar_outbound(
    item: crate::items::Item,
    operation: OutboundOperation,
    collection: &GoogleSyncCollection,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<PreparedOutbound, GoogleSyncServiceError> {
    if item.kind != ItemKind::Event {
        return Err(GoogleSyncServiceError::InvalidOutboundItem);
    }
    if operation == OutboundOperation::Delete {
        if item.deleted_at.is_none() {
            return Err(GoogleSyncServiceError::DeleteRequiresTrash);
        }
        return Ok(PreparedOutbound {
            entity_kind: "calendar_event",
            item,
            operation,
            payload: json!({}),
        });
    }
    if item.deleted_at.is_some() {
        return Err(GoogleSyncServiceError::InvalidOutboundItem);
    }
    let firm = item
        .flexible_constraints
        .get("dayweave_firm_block")
        .and_then(Value::as_object)
        .ok_or(GoogleSyncServiceError::MissingFirmBlock)?;
    if firm.get("owned").and_then(Value::as_bool) != Some(true) {
        return Err(GoogleSyncServiceError::MissingFirmBlock);
    }
    let starts_at = firm
        .get("starts_at")
        .and_then(Value::as_str)
        .map(parse_timestamp)
        .transpose()
        .map_err(|_| GoogleSyncServiceError::MissingFirmBlock)?
        .ok_or(GoogleSyncServiceError::MissingFirmBlock)?;
    let ends_at = firm
        .get("ends_at")
        .and_then(Value::as_str)
        .map(parse_timestamp)
        .transpose()
        .map_err(|_| GoogleSyncServiceError::MissingFirmBlock)?
        .ok_or(GoogleSyncServiceError::MissingFirmBlock)?;
    if ends_at <= starts_at {
        return Err(GoogleSyncServiceError::MissingFirmBlock);
    }
    let all_day = firm
        .get("all_day")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tentative = firm
        .get("tentative")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let busy = firm.get("busy").and_then(Value::as_bool).unwrap_or(true);
    if (all_day && !collection.calendar_policy.publish_all_day)
        || (tentative && !collection.calendar_policy.publish_tentative)
        || (!busy && !collection.calendar_policy.publish_free)
    {
        return Err(GoogleSyncServiceError::OutboundPolicyDenied);
    }
    let marker_aad = calendar_marker_aad(scope, collection.account_id, collection.id);
    let marker = cipher.seal(item.id.as_bytes(), &marker_aad)?;
    let mut private = BTreeMap::new();
    private.insert(
        "dayweaveOwnershipProof".to_owned(),
        format!(
            "dwm1.v{}.{}",
            marker.key_version,
            URL_SAFE_NO_PAD.encode(&marker.ciphertext)
        ),
    );
    let (start, end) = if all_day {
        let timezone: Tz = item
            .timezone_name
            .parse()
            .map_err(|_| GoogleSyncServiceError::MissingFirmBlock)?;
        let local_start = starts_at.with_timezone(&timezone);
        let local_end = ends_at.with_timezone(&timezone);
        if local_start.time() != NaiveTime::MIN
            || local_end.time() != NaiveTime::MIN
            || local_end.date_naive() <= local_start.date_naive()
        {
            return Err(GoogleSyncServiceError::MissingFirmBlock);
        }
        (
            EventDateTime {
                date: Some(local_start.date_naive().to_string()),
                date_time: None,
                time_zone: Some(item.timezone_name.clone()),
            },
            EventDateTime {
                date: Some(local_end.date_naive().to_string()),
                date_time: None,
                time_zone: Some(item.timezone_name.clone()),
            },
        )
    } else {
        (
            EventDateTime {
                date: None,
                date_time: Some(starts_at.to_rfc3339()),
                time_zone: Some(item.timezone_name.clone()),
            },
            EventDateTime {
                date: None,
                date_time: Some(ends_at.to_rfc3339()),
                time_zone: Some(item.timezone_name.clone()),
            },
        )
    };
    let event = GoogleEvent {
        // Calendar event IDs are client-selected so a crash after provider
        // acceptance can be recovered with GET+conditional update, not a
        // duplicate insert.
        id: deterministic_calendar_event_id(cipher, scope, collection, item.id)?,
        etag: None,
        status: Some(if tentative { "tentative" } else { "confirmed" }.to_owned()),
        summary: Some(item.title.clone()),
        description: item.notes.clone(),
        location: None,
        start: Some(start),
        end: Some(end),
        recurring_event_id: None,
        original_start_time: None,
        recurrence: Vec::new(),
        transparency: Some(if busy { "opaque" } else { "transparent" }.to_owned()),
        visibility: Some("private".to_owned()),
        event_type: Some("default".to_owned()),
        attendees: Vec::new(),
        conference_data: None,
        attachments: Vec::new(),
        updated: None,
        sequence: None,
        extended_properties: Some(ExtendedProperties {
            private,
            shared: BTreeMap::new(),
        }),
        additional_properties: BTreeMap::new(),
    };
    Ok(PreparedOutbound {
        entity_kind: "calendar_event",
        item,
        operation,
        payload: serde_json::to_value(event).map_err(|_| GoogleSyncServiceError::Internal)?,
    })
}

fn prepare_task_outbound(
    item: crate::items::Item,
    operation: OutboundOperation,
) -> Result<PreparedOutbound, GoogleSyncServiceError> {
    if item.kind != ItemKind::Task {
        return Err(GoogleSyncServiceError::InvalidOutboundItem);
    }
    if operation == OutboundOperation::Delete {
        if item.deleted_at.is_none() {
            return Err(GoogleSyncServiceError::DeleteRequiresTrash);
        }
        return Ok(PreparedOutbound {
            entity_kind: "task",
            item,
            operation,
            payload: json!({}),
        });
    }
    if item.deleted_at.is_some() {
        return Err(GoogleSyncServiceError::InvalidOutboundItem);
    }
    let task = GoogleTask {
        id: String::new(),
        etag: None,
        title: item.title.clone(),
        // Never publish the legacy visible UUID marker, even if it survived in
        // an old local row or was pasted back by a user.
        notes: sanitize_task_notes(item.notes.as_deref()).0,
        status: Some(if item.status == ItemStatus::Completed {
            "completed".to_owned()
        } else {
            "needsAction".to_owned()
        }),
        due: item.deadline_at.map(|value| value.to_rfc3339()),
        completed: item.completed_at.map(|value| value.to_rfc3339()),
        updated: None,
        parent: None,
        position: None,
        links: None,
        deleted: false,
        hidden: false,
    };
    Ok(PreparedOutbound {
        entity_kind: "task",
        item,
        operation,
        payload: serde_json::to_value(task).map_err(|_| GoogleSyncServiceError::Internal)?,
    })
}

fn outbound_event_result(
    event: &GoogleEvent,
    dispatch_nonce: Uuid,
) -> Result<OutboundResult, GoogleSyncServiceError> {
    validate_remote_id(&event.id)?;
    let remote_etag = bounded_optional(event.etag.as_deref(), 1000)
        .map_err(|_| GoogleSyncServiceError::ProviderProtocol)?
        .ok_or(GoogleSyncServiceError::ProviderProtocol)?;
    Ok(OutboundResult {
        remote_resource_id: event.id.clone(),
        remote_etag: Some(remote_etag),
        remote_updated_at: event
            .updated
            .as_deref()
            .map(parse_timestamp)
            .transpose()
            .map_err(|_| GoogleSyncServiceError::ProviderProtocol)?,
        payload_hash: payload_hash(&event).map_err(|_| GoogleSyncServiceError::ProviderProtocol)?,
        dispatch_nonce,
    })
}

fn outbound_task_result(
    task: &GoogleTask,
    dispatch_nonce: Uuid,
) -> Result<OutboundResult, GoogleSyncServiceError> {
    validate_remote_id(&task.id)?;
    let remote_etag = bounded_optional(task.etag.as_deref(), 1000)
        .map_err(|_| GoogleSyncServiceError::ProviderProtocol)?
        .ok_or(GoogleSyncServiceError::ProviderProtocol)?;
    Ok(OutboundResult {
        remote_resource_id: task.id.clone(),
        remote_etag: Some(remote_etag),
        remote_updated_at: task
            .updated
            .as_deref()
            .map(parse_timestamp)
            .transpose()
            .map_err(|_| GoogleSyncServiceError::ProviderProtocol)?,
        payload_hash: payload_hash(&task).map_err(|_| GoogleSyncServiceError::ProviderProtocol)?,
        dispatch_nonce,
    })
}

fn markerless_task_create_result(
    response: ProviderWriteResponse,
    dispatch_nonce: Uuid,
) -> Result<OutboundResult, GoogleSyncServiceError> {
    let ProviderWriteResponse::Task(task) = response else {
        return Err(GoogleSyncServiceError::ProviderIdentityUnresolved);
    };
    outbound_task_result(&task, dispatch_nonce).map_err(|error| match error {
        // A 2xx response without a usable ID, ETag, timestamp, or expected
        // response variant cannot prove which provider object was created.
        GoogleSyncServiceError::ProviderProtocol => {
            GoogleSyncServiceError::ProviderIdentityUnresolved
        }
        other => other,
    })
}

fn deterministic_calendar_event_id(
    cipher: &SecretCipher,
    scope: OAuthScope,
    collection: &GoogleSyncCollection,
    item_id: Uuid,
) -> Result<String, GoogleSyncServiceError> {
    let context = serde_json::to_vec(&(
        scope.workspace_id,
        scope.user_id,
        collection.account_id,
        collection.id,
        &collection.remote_collection_id,
        item_id,
    ))
    .map_err(|_| GoogleSyncServiceError::Internal)?;
    let (version, digest) = cipher.identity_digest(b"dayweave.google.event-id.v1", &context)?;
    // Google event IDs accept lower-case hexadecimal. The keyed digest is
    // non-reversible and account/calendar scoped, unlike the former raw UUID.
    Ok(format!("d{version:x}{}", encode_hex(&digest)))
}

fn calendar_event_owned_by(
    event: &GoogleEvent,
    item_id: Uuid,
    account_id: Uuid,
    collection_id: Uuid,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> bool {
    matches!(calendar_dayweave_item_id_for_target(
        event,
        account_id,
        collection_id,
        cipher,
        scope,
    ), Ok(Some(parsed)) if parsed == item_id)
}

fn calendar_event_matches_intent(found: &GoogleEvent, intended: &GoogleEvent) -> bool {
    let mut found = found.clone();
    let mut intended = intended.clone();
    normalize_calendar_provider_metadata(&mut found);
    normalize_calendar_provider_metadata(&mut intended);
    found == intended
}

fn calendar_reviewed_projection(event: &GoogleEvent) -> Result<Value, serde_json::Error> {
    let mut event = event.clone();
    normalize_calendar_provider_metadata(&mut event);
    serde_json::to_value(event)
}

fn normalize_calendar_provider_metadata(event: &mut GoogleEvent) {
    // Google assigns these immutable/observational fields after acceptance.
    // All other fields, including currently unmodeled wire fields, remain in
    // the projection and therefore fail recovery closed if they differ.
    event.etag = None;
    event.updated = None;
    event.sequence = None;
    for field in [
        "kind",
        "htmlLink",
        "created",
        "creator",
        "organizer",
        "iCalUID",
    ] {
        event.additional_properties.remove(field);
    }
}

fn calendar_dayweave_item_id(
    event: &GoogleEvent,
    collection: &GoogleSyncCollection,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<Option<Uuid>, NormalizationError> {
    calendar_dayweave_item_id_for_target(event, collection.account_id, collection.id, cipher, scope)
}

fn calendar_dayweave_item_id_for_target(
    event: &GoogleEvent,
    account_id: Uuid,
    collection_id: Uuid,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<Option<Uuid>, NormalizationError> {
    let Some(properties) = event.extended_properties.as_ref() else {
        return Ok(None);
    };
    if properties.private.contains_key("dayweaveItemId")
        || properties.private.contains_key("dayweaveOwnership")
    {
        return Err(NormalizationError::Rejected(
            "unauthenticated_dayweave_marker",
        ));
    }
    let Some(proof) = properties.private.get("dayweaveOwnershipProof") else {
        return Ok(None);
    };
    let remainder = proof
        .strip_prefix("dwm1.v")
        .ok_or(NormalizationError::Rejected("dayweave_marker_invalid"))?;
    let (version, encoded) = remainder
        .split_once('.')
        .ok_or(NormalizationError::Rejected("dayweave_marker_invalid"))?;
    let version = version
        .parse::<u32>()
        .map_err(|_| NormalizationError::Rejected("dayweave_marker_invalid"))?;
    let envelope = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| NormalizationError::Rejected("dayweave_marker_invalid"))?;
    let plaintext = cipher
        .open(
            version,
            &envelope,
            &calendar_marker_aad(scope, account_id, collection_id),
        )
        .map_err(|_| NormalizationError::Rejected("dayweave_marker_invalid"))?;
    Uuid::from_slice(&plaintext)
        .map(Some)
        .map_err(|_| NormalizationError::Rejected("dayweave_marker_invalid"))
}

fn guard_new_task_insert(
    provider_post_may_have_started: bool,
) -> Result<(), GoogleSyncServiceError> {
    // Google Tasks does not accept a client-selected task ID. Only an explicit
    // durable marker written in the final dispatch transaction proves a POST
    // may have started; pre-send retries and attempt counters are irrelevant.
    // After such an interrupted insert, absence from a bounded list cannot
    // prove that Google did not accept the request (eventual consistency and
    // stripped tombstones are both possible). A second insert could duplicate
    // external state, so require operator reconciliation and a new revision.
    if provider_post_may_have_started {
        Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
    } else {
        Ok(())
    }
}

fn sanitize_task_notes(notes: Option<&str>) -> (Option<String>, bool) {
    let Some(notes) = notes else {
        return (None, false);
    };
    let mut normalized = String::with_capacity(notes.len());
    let mut in_inline_control_run = false;
    for character in notes.chars() {
        if character == '\n' {
            normalized.push(character);
            in_inline_control_run = false;
        } else if character.is_control() {
            if !in_inline_control_run {
                normalized.push(' ');
            }
            in_inline_control_run = true;
        } else {
            normalized.push(character);
            in_inline_control_run = false;
        }
    }
    let mut stripped = false;
    while let Some(start) = legacy_task_marker_start(&normalized) {
        let end = normalized[start..]
            .find(']')
            .map_or(normalized.len(), |offset| start + offset + 1);
        normalized.replace_range(start..end, "");
        stripped = true;
    }
    let retained = normalized
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    ((!retained.is_empty()).then_some(retained), stripped)
}

fn legacy_task_marker_start(value: &str) -> Option<usize> {
    let lower = value.to_ascii_lowercase();
    let mut searched = 0_usize;
    while let Some(relative) = lower[searched..].find("[dayweave") {
        let start = searched + relative;
        let mut suffix = start + "[dayweave".len();
        while lower
            .as_bytes()
            .get(suffix)
            .is_some_and(u8::is_ascii_whitespace)
        {
            suffix += 1;
        }
        if lower[suffix..].starts_with("item:") {
            return Some(start);
        }
        searched = start + 1;
    }
    None
}

fn calendar_marker_aad(scope: OAuthScope, account_id: Uuid, collection_id: Uuid) -> Vec<u8> {
    serde_json::to_vec(&(
        "dayweave.google.calendar-marker.v1",
        scope.workspace_id,
        scope.user_id,
        account_id,
        collection_id,
    ))
    .expect("UUID marker context serialization cannot fail")
}

fn parse_event_bound(
    value: &EventDateTime,
    fallback_timezone: &str,
) -> Result<(DateTime<Utc>, String, bool), NormalizationError> {
    if value.date.is_some() == value.date_time.is_some() {
        return Err(NormalizationError::Rejected("event_bounds_invalid"));
    }
    if let Some(date_time) = value.date_time.as_deref() {
        let parsed = parse_timestamp(date_time)?;
        let timezone = value.time_zone.as_deref().unwrap_or(fallback_timezone);
        if !valid_opaque(timezone, 255) || timezone.parse::<Tz>().is_err() {
            return Err(NormalizationError::Rejected("event_timezone_invalid"));
        }
        return Ok((parsed, timezone.to_owned(), false));
    }
    let date = value
        .date
        .as_deref()
        .ok_or(NormalizationError::Rejected("event_bounds_missing"))?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| NormalizationError::Rejected("event_date_invalid"))?;
    let timezone_name = value.time_zone.as_deref().unwrap_or(fallback_timezone);
    let timezone: Tz = timezone_name
        .parse()
        .map_err(|_| NormalizationError::Rejected("event_timezone_invalid"))?;
    let local = date
        .and_hms_opt(0, 0, 0)
        .ok_or(NormalizationError::Rejected("event_date_invalid"))?;
    let zoned = timezone
        .from_local_datetime(&local)
        .single()
        .or_else(|| timezone.from_local_datetime(&local).earliest())
        .ok_or(NormalizationError::Rejected("event_date_invalid"))?;
    Ok((zoned.with_timezone(&Utc), timezone_name.to_owned(), true))
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, NormalizationError> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| NormalizationError::Rejected("timestamp_invalid"))?;
    DateTime::from_timestamp_micros(parsed.timestamp_micros())
        .ok_or(NormalizationError::Rejected("timestamp_invalid"))
}

fn truncate_to_microseconds(value: DateTime<Utc>) -> Result<DateTime<Utc>, GoogleSyncServiceError> {
    DateTime::from_timestamp_micros(value.timestamp_micros())
        .ok_or(GoogleSyncServiceError::Internal)
}

fn parse_optional_timestamp(
    value: Option<&str>,
) -> Result<Option<DateTime<Utc>>, NormalizationError> {
    value.map(parse_timestamp).transpose()
}

fn bounded_title(value: &str, fallback: &str) -> (String, bool) {
    let sanitized = sanitize_provider_display_text(value);
    let trimmed = sanitized.trim();
    let source = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    bounded_chars(source, 500)
}

fn bounded_notes(value: Option<&str>) -> (Option<String>, bool) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return (None, false);
    };
    let sanitized = sanitize_provider_display_text(value);
    let (value, truncated) = bounded_chars(&sanitized, 100_000);
    (Some(value), truncated)
}

fn sanitize_provider_display_text(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut in_control_run = false;
    for character in value.chars() {
        if character.is_control() {
            if !in_control_run {
                sanitized.push(' ');
            }
            in_control_run = true;
        } else {
            sanitized.push(character);
            in_control_run = false;
        }
    }
    sanitized
}

fn bounded_chars(value: &str, limit: usize) -> (String, bool) {
    if value.chars().count() <= limit {
        return (value.to_owned(), false);
    }
    (value.chars().take(limit).collect(), true)
}

fn bounded_optional(
    value: Option<&str>,
    limit: usize,
) -> Result<Option<String>, NormalizationError> {
    value
        .map(|value| {
            if valid_opaque(value, limit) {
                Ok(value.to_owned())
            } else {
                Err(NormalizationError::Rejected("provider_metadata_invalid"))
            }
        })
        .transpose()
}

fn bounded_ascii_label(value: &str, limit: usize) -> Result<String, GoogleSyncServiceError> {
    if value.is_empty()
        || value.len() > limit
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(GoogleSyncServiceError::ProviderProtocol);
    }
    Ok(value.to_owned())
}

fn validate_remote_id(value: &str) -> Result<(), GoogleSyncServiceError> {
    if valid_opaque(value, 1000) {
        Ok(())
    } else {
        Err(GoogleSyncServiceError::ProviderProtocol)
    }
}

fn valid_opaque(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn validate_page_token(
    value: &str,
    seen: &mut HashSet<String>,
) -> Result<(), GoogleSyncServiceError> {
    if !valid_opaque(value, 16_384) || !seen.insert(value.to_owned()) {
        return Err(GoogleSyncServiceError::ProviderProtocol);
    }
    Ok(())
}

fn payload_hash<T: Serialize>(value: &T) -> Result<[u8; 32], NormalizationError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| NormalizationError::Rejected("provider_payload_invalid"))?;
    Ok(Sha256::digest(bytes).into())
}

#[allow(clippy::too_many_arguments)] // Every reviewed authorization dimension is explicit and hashed.
pub(crate) fn outbound_intent_hash(
    workspace_id: Uuid,
    user_id: Uuid,
    account_id: Uuid,
    collection_id: Uuid,
    collection_revision: u64,
    collection_remote_id: &str,
    collection_kind: GoogleCollectionKind,
    required_scope: &str,
    item_id: Uuid,
    item_revision: u64,
    entity_kind: &str,
    operation: OutboundOperation,
    payload: &Value,
    provider_resource_id: Option<&str>,
    provider_etag: Option<&str>,
) -> Result<[u8; 32], serde_json::Error> {
    let bytes = serde_json::to_vec(&(
        "dayweave.google.outbound-intent.v2",
        workspace_id,
        user_id,
        account_id,
        collection_id,
        collection_revision,
        collection_remote_id,
        collection_kind,
        required_scope,
        item_id,
        item_revision,
        entity_kind,
        operation,
        provider_resource_id,
        provider_etag,
        payload,
    ))?;
    Ok(Sha256::digest(bytes).into())
}

pub(crate) fn outbound_preview_hash(
    preview_id: Uuid,
    intent_hash: [u8; 32],
    expires_at: DateTime<Utc>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"dayweave.google.outbound-preview.v1\0");
    digest.update(preview_id.as_bytes());
    digest.update(intent_hash);
    digest.update(expires_at.timestamp().to_be_bytes());
    digest.update(expires_at.timestamp_subsec_nanos().to_be_bytes());
    digest.finalize().into()
}

fn approval_capability_hash(value: &str) -> Result<[u8; 32], GoogleSyncServiceError> {
    let payload = value
        .strip_prefix(APPROVAL_TOKEN_PREFIX)
        .ok_or(GoogleSyncServiceError::InvalidApprovalCapability)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| GoogleSyncServiceError::InvalidApprovalCapability)?;
    if decoded.len() != APPROVAL_TOKEN_RANDOM_BYTES
        || URL_SAFE_NO_PAD.encode(&decoded) != payload
        || value.chars().any(char::is_whitespace)
    {
        return Err(GoogleSyncServiceError::InvalidApprovalCapability);
    }
    Ok(Sha256::digest(value.as_bytes()).into())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

fn decode_hash(value: &str) -> Result<[u8; 32], GoogleSyncServiceError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GoogleSyncServiceError::InvalidApprovalCapability);
    }
    let mut result = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_nibble(pair[0])?;
        let low = decode_hex_nibble(pair[1])?;
        result[index] = (high << 4) | low;
    }
    if encode_hex(&result) != value {
        return Err(GoogleSyncServiceError::InvalidApprovalCapability);
    }
    Ok(result)
}

fn decode_hex_nibble(value: u8) -> Result<u8, GoogleSyncServiceError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(GoogleSyncServiceError::InvalidApprovalCapability),
    }
}

fn projection_hash(
    remote_hash: [u8; 32],
    collection: &GoogleSyncCollection,
) -> Result<[u8; 32], NormalizationError> {
    payload_hash(&(
        "dayweave.google.canonical-projection.v2",
        remote_hash,
        collection.visible,
        collection.sync_role,
        collection.calendar_policy,
    ))
}

fn calendar_occurrence_projection_hash(
    item: Option<&NewItem>,
) -> Result<[u8; 32], NormalizationError> {
    let Some(item) = item else {
        return payload_hash(&(
            "dayweave.google.calendar-occurrence-projection.v1",
            "absent",
        ));
    };
    // Provider payload identity/version has its own raw hash. Bind this hash
    // only to durable canonical semantics (and deliberately exclude the fresh
    // local UUID), so redacted private text changes cannot cause canonical
    // churn or reveal a content-change oracle.
    payload_hash(&(
        "dayweave.google.calendar-occurrence-projection.v1",
        "present",
        (
            item.is_sensitive,
            &item.kind,
            &item.status,
            &item.title,
            &item.notes,
            &item.timezone_name,
            item.duration_seconds,
            item.deadline_at,
            item.earliest_start_at,
        ),
        (
            &item.recurrence,
            &item.flexible_constraints,
            &item.split_policy,
            item.importance,
            item.urgency,
            item.parent_id,
            item.sibling_order,
        ),
    ))
}

fn has_calendar_read(scopes: &std::collections::BTreeSet<String>) -> bool {
    scopes.contains(GOOGLE_CALENDAR_SCOPE) || scopes.contains(GOOGLE_CALENDAR_READONLY_SCOPE)
}

fn has_tasks_read(scopes: &std::collections::BTreeSet<String>) -> bool {
    scopes.contains(GOOGLE_TASKS_SCOPE) || scopes.contains(GOOGLE_TASKS_READONLY_SCOPE)
}

fn exponential_backoff(attempts: u32) -> Duration {
    let exponent = attempts.min(7);
    Duration::seconds(30_i64.saturating_mul(1_i64 << exponent).min(3_600))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FailureDisposition {
    kind: SyncFailureKind,
    code: &'static str,
    delay: Duration,
}

#[derive(Debug, Error)]
pub(crate) enum GoogleSyncServiceError {
    #[error("Google sync request is invalid")]
    InvalidRequest,
    #[error("Google read authorization is required")]
    MissingReadScope,
    #[error("Google write authorization is required")]
    MissingWriteScope,
    #[error("Google provider response exceeded bounded sync limits")]
    ProviderLimitExceeded,
    #[error("Google provider response did not satisfy the sync protocol")]
    ProviderProtocol,
    #[error("stored Google sync cursor is corrupt")]
    CursorCorrupt,
    #[error("canonical item is not eligible for this outbound mutation")]
    InvalidOutboundItem,
    #[error("calendar publication requires a DayWeave-owned firm block")]
    MissingFirmBlock,
    #[error("provider deletion requires the canonical item to be in recoverable trash")]
    DeleteRequiresTrash,
    #[error("Google outbound publication is disabled by deployment policy")]
    ExternalPublicationDisabled,
    #[error("outbound approval capability is invalid")]
    InvalidApprovalCapability,
    #[error("collection publication policy does not permit this provider representation")]
    OutboundPolicyDenied,
    #[error("secure random generation failed")]
    Randomness,
    #[error("durable outbound payload is corrupt")]
    OutboundPayloadCorrupt,
    #[error("Google Tasks create result has no safely identifiable provider record")]
    ProviderIdentityUnresolved,
    #[error("Google provider write preparation exceeded its bounded initiation window")]
    DispatchPreparationTimeout,
    #[error("Google sync operation failed")]
    Internal,
    #[error(transparent)]
    Repository(#[from] GoogleSyncRepositoryError),
    #[error(transparent)]
    OAuth(#[from] GoogleOAuthServiceError),
    #[error(transparent)]
    Item(#[from] ItemServiceError),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Google(#[from] GoogleError),
}

impl GoogleSyncServiceError {
    fn code(&self) -> &'static str {
        self.failure().code
    }

    fn failure(&self) -> FailureDisposition {
        match self {
            Self::Google(GoogleError::Unauthorized)
            | Self::MissingReadScope
            | Self::MissingWriteScope
            | Self::Repository(
                GoogleSyncRepositoryError::ReadScopeMissing
                | GoogleSyncRepositoryError::WriteScopeMissing,
            ) => FailureDisposition {
                kind: SyncFailureKind::ReauthorizationRequired,
                code: "reauthorization_required",
                delay: Duration::hours(24),
            },
            Self::Google(GoogleError::RateLimited {
                retry_after_seconds,
            }) => FailureDisposition {
                kind: SyncFailureKind::Backoff,
                code: "rate_limited",
                delay: Duration::seconds(
                    retry_after_seconds
                        .unwrap_or(60)
                        .clamp(1, 3600)
                        .try_into()
                        .unwrap_or(3600),
                ),
            },
            Self::Google(
                GoogleError::Temporary { .. }
                | GoogleError::Transport(_)
                | GoogleError::InvalidResponse,
            ) => FailureDisposition {
                kind: SyncFailureKind::Backoff,
                code: "provider_temporary",
                delay: Duration::minutes(5),
            },
            Self::OAuth(GoogleOAuthServiceError::IntegrationTimeout) => FailureDisposition {
                kind: SyncFailureKind::Backoff,
                code: "oauth_timeout",
                delay: Duration::minutes(5),
            },
            Self::DispatchPreparationTimeout => FailureDisposition {
                kind: SyncFailureKind::Backoff,
                code: "dispatch_preparation_timeout",
                delay: Duration::seconds(30),
            },
            Self::Repository(GoogleSyncRepositoryError::CursorConflict) => FailureDisposition {
                kind: SyncFailureKind::Backoff,
                code: "cursor_conflict",
                delay: Duration::seconds(30),
            },
            Self::Repository(GoogleSyncRepositoryError::ClaimLost) => FailureDisposition {
                kind: SyncFailureKind::Backoff,
                code: "claim_lost",
                delay: Duration::seconds(30),
            },
            Self::ProviderLimitExceeded | Self::Google(GoogleError::ResponseTooLarge) => {
                FailureDisposition {
                    kind: SyncFailureKind::Failed,
                    code: "provider_limit_exceeded",
                    delay: Duration::hours(24),
                }
            }
            Self::CursorCorrupt | Self::Crypto(_) => FailureDisposition {
                kind: SyncFailureKind::Failed,
                code: "cursor_unreadable",
                delay: Duration::hours(24),
            },
            Self::OutboundPayloadCorrupt => FailureDisposition {
                kind: SyncFailureKind::Failed,
                code: "outbound_payload_corrupt",
                delay: Duration::hours(24),
            },
            Self::ProviderIdentityUnresolved => FailureDisposition {
                kind: SyncFailureKind::Failed,
                code: "provider_identity_unresolved",
                delay: Duration::hours(24),
            },
            Self::ProviderProtocol => FailureDisposition {
                kind: SyncFailureKind::Failed,
                code: "provider_protocol",
                delay: Duration::hours(24),
            },
            Self::Google(GoogleError::Api { status: 404 }) => FailureDisposition {
                kind: SyncFailureKind::Failed,
                code: "provider_not_found",
                delay: Duration::hours(24),
            },
            Self::Google(GoogleError::Api { .. }) => FailureDisposition {
                kind: SyncFailureKind::Failed,
                code: "provider_rejected",
                delay: Duration::hours(24),
            },
            _ => FailureDisposition {
                kind: SyncFailureKind::Failed,
                code: "sync_failed",
                delay: Duration::hours(24),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NormalizationError {
    Rejected(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use dayweave_google::calendar::{EventAttachment, EventAttendee};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[tokio::test]
    async fn provider_write_sequence_rechecks_guardians_after_token_preparation() {
        let guardian_revoked = Arc::new(AtomicBool::new(false));
        let provider_writes = Arc::new(AtomicUsize::new(0));
        let preparation_guardian = guardian_revoked.clone();
        let authorization_guardian = guardian_revoked.clone();
        let write_counter = provider_writes.clone();
        let result: Result<(), &'static str> = sequence_guarded_write(
            async move {
                // Models an account pause/scope loss/item or mapping mutation
                // that commits while OAuth refresh/request construction runs.
                preparation_guardian.store(true, Ordering::SeqCst);
                Ok(())
            },
            move || async move {
                if authorization_guardian.load(Ordering::SeqCst) {
                    Err("guardian_revoked")
                } else {
                    Ok(())
                }
            },
            move |(), ()| async move {
                write_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
        assert_eq!(result, Err("guardian_revoked"));
        assert_eq!(provider_writes.load(Ordering::SeqCst), 0);
    }

    fn collection(role: GoogleSyncRole, visible: bool) -> GoogleSyncCollection {
        let now = Utc::now();
        GoogleSyncCollection {
            id: Uuid::from_u128(10),
            account_id: Uuid::from_u128(11),
            kind: GoogleCollectionKind::Calendar,
            remote_collection_id: "primary".to_owned(),
            display_name: "Work".to_owned(),
            provider_access_role: Some("owner".to_owned()),
            provider_primary: true,
            provider_selected: true,
            provider_hidden: false,
            provider_deleted: false,
            selected: true,
            visible,
            sync_role: role,
            calendar_policy: GoogleCalendarPolicy::default(),
            revision: 1,
            discovered_at: now,
            configured_at: Some(now),
            last_import_at: None,
            planning_projection_state: crate::google_sync::CalendarProjectionState::Uninitialized,
            planning_generation: 0,
            planning_collection_revision: None,
            planning_window_start: None,
            planning_window_end: None,
            planning_window_refreshed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn event() -> GoogleEvent {
        GoogleEvent {
            id: "event-1".to_owned(),
            etag: Some("etag-1".to_owned()),
            status: Some("confirmed".to_owned()),
            summary: Some("Planning".to_owned()),
            description: Some("Private detail".to_owned()),
            location: Some("Room".to_owned()),
            start: Some(EventDateTime {
                date: None,
                date_time: Some("2026-08-29T10:00:00+02:00".to_owned()),
                time_zone: Some("Europe/Madrid".to_owned()),
            }),
            end: Some(EventDateTime {
                date: None,
                date_time: Some("2026-08-29T11:00:00+02:00".to_owned()),
                time_zone: Some("Europe/Madrid".to_owned()),
            }),
            recurring_event_id: None,
            original_start_time: None,
            recurrence: Vec::new(),
            transparency: Some("opaque".to_owned()),
            visibility: Some("default".to_owned()),
            event_type: Some("default".to_owned()),
            attendees: vec![EventAttendee {
                email: "owner@example.test".to_owned(),
                display_name: None,
                response_status: Some("accepted".to_owned()),
                self_: true,
                organizer: true,
                optional: false,
            }],
            conference_data: Some(json!({"entryPoints": []})),
            attachments: vec![EventAttachment {
                file_url: "https://example.test/file".to_owned(),
                title: None,
                mime_type: None,
                file_id: None,
            }],
            updated: Some("2026-08-29T08:00:00Z".to_owned()),
            sequence: Some(2),
            extended_properties: None,
            additional_properties: BTreeMap::new(),
        }
    }

    fn firm_event_item(
        id: u128,
        timezone_name: &str,
        starts_at: &str,
        ends_at: &str,
        all_day: bool,
        tentative: bool,
        busy: bool,
    ) -> crate::items::Item {
        let starts_at: DateTime<Utc> = starts_at.parse().expect("synthetic start");
        let ends_at: DateTime<Utc> = ends_at.parse().expect("synthetic end");
        let duration_seconds =
            u32::try_from((ends_at - starts_at).num_seconds()).expect("bounded synthetic duration");
        crate::items::Item::new(
            NewItem {
                id: Uuid::from_u128(id),
                is_sensitive: false,
                kind: ItemKind::Event,
                status: ItemStatus::Scheduled,
                title: "Policy fixture".to_owned(),
                notes: Some("Synthetic details".to_owned()),
                timezone_name: timezone_name.to_owned(),
                duration_seconds: Some(duration_seconds),
                deadline_at: Some(ends_at),
                earliest_start_at: Some(starts_at),
                recurrence: None,
                flexible_constraints: json!({"dayweave_firm_block": {
                    "owned": true,
                    "starts_at": starts_at,
                    "ends_at": ends_at,
                    "all_day": all_day,
                    "tentative": tentative,
                    "busy": busy,
                }}),
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
            },
            starts_at - Duration::hours(1),
        )
        .expect("valid synthetic firm event")
    }

    fn projection_window_fixture() -> CalendarProjectionWindow {
        CalendarProjectionWindow {
            start: "2026-08-01T00:00:00Z".parse().expect("window start"),
            end: "2026-12-01T00:00:00Z".parse().expect("window end"),
        }
    }

    fn event_page(items: Vec<GoogleEvent>, next_page_token: Option<&str>) -> EventListPage {
        EventListPage {
            items,
            next_page_token: next_page_token.map(str::to_owned),
            next_sync_token: None,
            time_zone: Some("Europe/Madrid".to_owned()),
        }
    }

    #[test]
    fn projection_window_and_request_are_bounded_expanded_and_microsecond_exact() {
        let started_at = "2026-08-29T12:34:56.123456789Z"
            .parse()
            .expect("start time");
        let window = calendar_projection_window(started_at).expect("projection window");
        assert_eq!(
            window.start,
            "2026-07-30T12:34:56.123456Z"
                .parse::<DateTime<Utc>>()
                .expect("lookback")
        );
        assert_eq!(
            window.end,
            "2026-12-27T12:34:56.123456Z"
                .parse::<DateTime<Utc>>()
                .expect("lookahead")
        );

        let options = expanded_event_list_options(window, Some("page-2".to_owned()));
        assert!(options.single_events);
        assert!(options.sync_token.is_none());
        assert_eq!(options.page_token.as_deref(), Some("page-2"));
        assert_eq!(
            options.time_min.as_deref(),
            Some("2026-07-30T12:34:56.123456Z")
        );
        assert_eq!(
            options.time_max.as_deref(),
            Some("2026-12-27T12:34:56.123456Z")
        );
        assert_eq!(options.max_results, Some(2500));
    }

    #[test]
    fn projection_page_validation_rejects_duplicates_cycles_invalid_timezone_and_item_10001() {
        let mut seen_ids = HashSet::new();
        let mut count = 0;
        validate_projection_page(
            &event_page(vec![event()], Some("page-a")),
            &mut seen_ids,
            &mut count,
        )
        .expect("first unique page");
        assert!(matches!(
            validate_projection_page(&event_page(vec![event()], None), &mut seen_ids, &mut count,),
            Err(GoogleSyncServiceError::ProviderProtocol)
        ));

        let mut seen_tokens = HashSet::new();
        validate_page_token("page-a", &mut seen_tokens).expect("first page token");
        validate_page_token("page-b", &mut seen_tokens).expect("second page token");
        assert!(matches!(
            validate_page_token("page-a", &mut seen_tokens),
            Err(GoogleSyncServiceError::ProviderProtocol)
        ));

        let mut invalid_timezone = event_page(Vec::new(), None);
        invalid_timezone.time_zone = Some("not/a-real-timezone".to_owned());
        let mut invalid_timezone_count = 0_usize;
        assert!(matches!(
            validate_projection_page(
                &invalid_timezone,
                &mut HashSet::new(),
                &mut invalid_timezone_count,
            ),
            Err(GoogleSyncServiceError::ProviderProtocol)
        ));
        let mut missing_timezone = event_page(Vec::new(), None);
        missing_timezone.time_zone = None;
        let mut missing_timezone_count = 0_usize;
        assert!(matches!(
            validate_projection_page(
                &missing_timezone,
                &mut HashSet::new(),
                &mut missing_timezone_count,
            ),
            Err(GoogleSyncServiceError::ProviderProtocol)
        ));

        let mut at_cap = MAX_CALENDAR_PROJECTION_ITEMS;
        assert!(matches!(
            validate_projection_page(
                &event_page(vec![event()], None),
                &mut HashSet::new(),
                &mut at_cap,
            ),
            Err(GoogleSyncServiceError::ProviderLimitExceeded)
        ));
    }

    #[test]
    fn complete_projection_pages_normalize_together_without_provider_identity_leaks() {
        let mut second = event();
        second.id = "event-2".to_owned();
        second.summary = Some("Second event".to_owned());
        let batch = normalize_calendar_projection_pages(
            &collection(GoogleSyncRole::Blocking, true),
            projection_window_fixture(),
            vec![
                event_page(vec![event()], Some("page-2")),
                event_page(vec![second], None),
            ],
            &test_marker_cipher(),
            test_oauth_scope(),
        )
        .expect("complete page set");

        assert_eq!(batch.changes.len(), 2);
        assert!(batch.rejected.is_empty());
        assert_eq!(batch.window, projection_window_fixture());
        for change in batch.changes {
            let item = change.item.expect("live occurrence");
            assert!(item.recurrence.is_none());
            assert!(item.flexible_constraints.get("calendar_event").is_some());
            let canonical = serde_json::to_string(&item).expect("canonical item");
            assert!(!canonical.contains(&change.remote_id));
            assert!(!canonical.contains("google_sync"));
        }
    }

    #[test]
    fn visible_calendar_display_text_is_safe_before_the_projection_boundary() {
        let mut multiline = event();
        multiline.summary = Some("\r\n  Planning\r\nreview\0\u{7}\u{7f}  ".to_owned());
        multiline.description = Some("Agenda\r\nfirst\tsecond\0\u{7}third".to_owned());

        let batch = normalize_calendar_projection_pages(
            &collection(GoogleSyncRole::Blocking, true),
            projection_window_fixture(),
            vec![event_page(vec![multiline], None)],
            &test_marker_cipher(),
            test_oauth_scope(),
        )
        .expect("ordinary multiline provider text remains a valid projection batch");

        assert!(batch.rejected.is_empty());
        let item = batch.changes[0].item.as_ref().expect("live occurrence");
        assert_eq!(item.title, "Planning review");
        assert_eq!(item.notes.as_deref(), Some("Agenda first second third"));
        assert!(!item.title.chars().any(char::is_control));
        assert!(
            item.notes
                .as_deref()
                .is_some_and(|notes| !notes.chars().any(char::is_control))
        );
    }

    #[test]
    fn projection_rejects_a_timezone_change_between_pages() {
        let mut second = event();
        second.id = "event-in-second-zone".to_owned();
        let mut second_page = event_page(vec![second], None);
        second_page.time_zone = Some("UTC".to_owned());

        assert!(matches!(
            normalize_calendar_projection_pages(
                &collection(GoogleSyncRole::Blocking, true),
                projection_window_fixture(),
                vec![event_page(vec![event()], Some("page-2")), second_page],
                &test_marker_cipher(),
                test_oauth_scope(),
            ),
            Err(GoogleSyncServiceError::ProviderProtocol)
        ));
    }

    #[test]
    fn projection_collects_rejection_with_valid_changes_for_atomic_fail_closed_batch() {
        let mut malformed = event();
        malformed.id = "malformed-event".to_owned();
        malformed.end = None;
        let batch = normalize_calendar_projection_pages(
            &collection(GoogleSyncRole::Blocking, true),
            projection_window_fixture(),
            vec![event_page(vec![event(), malformed], None)],
            &test_marker_cipher(),
            test_oauth_scope(),
        )
        .expect("normalization returns one complete atomic batch");

        assert_eq!(batch.changes.len(), 1);
        assert_eq!(batch.rejected.len(), 1);
        assert_eq!(batch.rejected[0].remote_id, "malformed-event");
        assert_eq!(batch.rejected[0].reason, "event_bounds_missing");
    }

    #[test]
    fn expanded_lane_rejects_live_series_master_but_series_lane_keeps_metadata() {
        let mut master = event();
        master.recurrence = vec!["RRULE:FREQ=WEEKLY".to_owned()];
        assert!(matches!(
            normalize_event(
                &collection(GoogleSyncRole::Blocking, true),
                "UTC",
                master.clone(),
            ),
            Err(NormalizationError::Rejected("provider_payload_invalid"))
        ));

        let metadata = normalize_calendar_series_authenticated(
            &collection(GoogleSyncRole::Blocking, true),
            master,
            &test_marker_cipher(),
            test_oauth_scope(),
        )
        .expect("series metadata");
        assert_eq!(metadata.remote_id, "event-1");
        assert!(!metadata.deleted);
        assert!(metadata.reviewed_provider_projection.is_none());
    }

    #[test]
    fn event_semantics_cover_exact_block_declines_and_concrete_series_instance() {
        let imported = normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", event())
            .expect("valid event")
            .item
            .expect("upsert");
        assert_eq!(imported.title, "Planning");
        assert_eq!(imported.duration_seconds, Some(3600));
        assert_eq!(imported.timezone_name, "Europe/Madrid");
        assert_eq!(
            imported.flexible_constraints,
            json!({"calendar_event": {
                "start": "2026-08-29T08:00:00Z",
                "end": "2026-08-29T09:00:00Z",
                "immutable": true,
                "all_day": false,
                "source_calendar_id": Value::Null,
            }})
        );
        assert!(imported.recurrence.is_none());

        let mut instance = event();
        instance.id = "event-instance-1".to_owned();
        instance.recurring_event_id = Some("series-1".to_owned());
        instance.original_start_time = instance.start.clone();
        assert!(
            normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", instance,)
                .expect("expanded recurrence instance")
                .item
                .expect("upsert")
                .recurrence
                .is_none()
        );

        let mut declined = event();
        declined.attendees[0].response_status = Some("declined".to_owned());
        assert!(
            normalize_event(
                &collection(GoogleSyncRole::Blocking, false),
                "UTC",
                declined,
            )
            .expect("self-declined event")
            .item
            .is_none(),
            "self-declined events are ignored and remove a prior import"
        );

        let mut guest_declined = event();
        guest_declined.attendees[0].self_ = false;
        guest_declined.attendees[0].response_status = Some("declined".to_owned());
        assert!(
            normalize_event(
                &collection(GoogleSyncRole::Blocking, false),
                "UTC",
                guest_declined,
            )
            .expect("non-self decline")
            .item
            .is_some(),
            "another attendee declining must not hide the owner's event"
        );
    }

    #[test]
    fn birthday_free_and_out_of_office_semantics_are_explicit() {
        let mut semantic_collection = collection(GoogleSyncRole::Blocking, true);
        semantic_collection.calendar_policy.free = GoogleEventDisposition::Ignore;
        let mut birthday = event();
        birthday.event_type = Some("birthday".to_owned());
        let birthday = normalize_event(&semantic_collection, "UTC", birthday)
            .expect("birthday")
            .item
            .expect("upsert");
        assert!(
            birthday
                .flexible_constraints
                .get("calendar_context")
                .is_some()
        );

        let mut working_location = event();
        working_location.id = "working-location-1".to_owned();
        working_location.event_type = Some("workingLocation".to_owned());
        let working_location = normalize_event(&semantic_collection, "UTC", working_location)
            .expect("working location")
            .item
            .expect("always retained as context");
        assert!(
            working_location
                .flexible_constraints
                .get("calendar_context")
                .is_some()
        );

        let mut free = event();
        free.transparency = Some("transparent".to_owned());
        let free = normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", free)
            .expect("free")
            .item
            .expect("upsert");
        assert!(free.flexible_constraints.get("calendar_context").is_some());

        let mut away = event();
        away.event_type = Some("outOfOffice".to_owned());
        let away = normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", away)
            .expect("out of office")
            .item
            .expect("upsert");
        assert!(away.flexible_constraints.get("calendar_event").is_some());
    }

    #[test]
    fn tentative_free_and_all_day_planning_policy_is_explicit_and_configurable() {
        let defaults = collection(GoogleSyncRole::Blocking, true);
        let mut tentative = event();
        tentative.status = Some("tentative".to_owned());
        let tentative = normalize_event(&defaults, "UTC", tentative)
            .expect("tentative event")
            .item
            .expect("default retains tentative context");
        assert!(
            tentative
                .flexible_constraints
                .get("calendar_context")
                .is_some()
        );

        let mut blocking = defaults.clone();
        blocking.calendar_policy.tentative = GoogleEventDisposition::Blocking;
        let mut provider_tentative = event();
        provider_tentative.status = Some("tentative".to_owned());
        assert!(
            normalize_event(&blocking, "UTC", provider_tentative)
                .expect("configured tentative event")
                .item
                .expect("retained")
                .flexible_constraints
                .get("calendar_event")
                .is_some()
        );

        let mut ignored = defaults;
        ignored.calendar_policy.free = GoogleEventDisposition::Ignore;
        let mut provider_free = event();
        provider_free.transparency = Some("transparent".to_owned());
        assert!(
            normalize_event(&ignored, "UTC", provider_free)
                .expect("ignored free event")
                .item
                .is_none()
        );
    }

    #[test]
    fn projection_hash_tracks_visibility_and_role_separately_from_google_payload() {
        let visible = normalize_event(&collection(GoogleSyncRole::ReadOnly, true), "UTC", event())
            .expect("visible projection");
        let hidden_collection = collection(GoogleSyncRole::Blocking, false);
        let hidden = normalize_event(&hidden_collection, "UTC", event())
            .expect("hidden blocking projection");

        assert!(!visible.item.as_ref().expect("visible item").is_sensitive);
        let hidden_item = hidden.item.as_ref().expect("hidden item");
        assert!(hidden_item.is_sensitive);
        assert_eq!(hidden_item.title, "Busy");
        assert!(hidden_item.notes.is_none());
        assert_eq!(visible.remote_payload_hash, hidden.remote_payload_hash);
        assert_ne!(
            visible.remote_projection_hash,
            hidden.remote_projection_hash
        );

        let mut changed_hidden_text = event();
        changed_hidden_text.summary = Some("CHANGED-HIDDEN-TITLE".to_owned());
        changed_hidden_text.description = Some("CHANGED-HIDDEN-NOTES".to_owned());
        changed_hidden_text.location = Some("CHANGED-HIDDEN-LOCATION".to_owned());
        let changed_hidden = normalize_event(&hidden_collection, "UTC", changed_hidden_text)
            .expect("changed hidden projection");
        assert_ne!(
            hidden.remote_payload_hash,
            changed_hidden.remote_payload_hash
        );
        assert_eq!(
            hidden.remote_projection_hash,
            changed_hidden.remote_projection_hash
        );
    }

    #[test]
    fn private_event_in_visible_collection_is_sensitive() {
        let mut private_event = event();
        private_event.summary = Some("SYNTHETIC-PRIVATE-GOOGLE-EVENT-CANARY".to_owned());
        private_event.description = Some("SYNTHETIC-PRIVATE-NOTES-CANARY".to_owned());
        private_event.location = Some("SYNTHETIC-PRIVATE-LOCATION-CANARY".to_owned());
        private_event.visibility = Some("private".to_owned());

        let first = normalize_event(
            &collection(GoogleSyncRole::ReadOnly, true),
            "UTC",
            private_event.clone(),
        )
        .expect("private event projection");
        let item = first.item.as_ref().expect("private event upsert");

        assert!(item.is_sensitive);
        assert_eq!(item.title, "Busy");
        assert!(item.notes.is_none());
        assert_eq!(
            item.flexible_constraints,
            json!({"calendar_context": {
                "start": "2026-08-29T08:00:00Z",
                "end": "2026-08-29T09:00:00Z",
                "all_day": false,
            }})
        );
        let serialized = serde_json::to_string(item).expect("canonical private item serializes");
        for canary in [
            "SYNTHETIC-PRIVATE-GOOGLE-EVENT-CANARY",
            "SYNTHETIC-PRIVATE-NOTES-CANARY",
            "SYNTHETIC-PRIVATE-LOCATION-CANARY",
            "event-1",
            "owner@example.test",
        ] {
            assert!(!serialized.contains(canary), "canonical leak: {canary}");
        }
        assert!(first.reviewed_provider_projection.is_none());

        private_event.summary = Some("CHANGED-PRIVATE-TITLE".to_owned());
        private_event.description = Some("CHANGED-PRIVATE-NOTES".to_owned());
        private_event.location = Some("CHANGED-PRIVATE-LOCATION".to_owned());
        private_event.attendees.clear();
        private_event.attachments.clear();
        private_event.conference_data = None;
        let second = normalize_event(
            &collection(GoogleSyncRole::ReadOnly, true),
            "UTC",
            private_event,
        )
        .expect("changed private event projection");

        assert_ne!(first.remote_payload_hash, second.remote_payload_hash);
        assert_eq!(first.remote_projection_hash, second.remote_projection_hash);
    }

    #[test]
    fn tombstones_need_no_bounds_and_all_day_uses_exclusive_end() {
        let mut deleted = event();
        deleted.status = Some("cancelled".to_owned());
        deleted.start = None;
        deleted.end = None;
        assert!(
            normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", deleted)
                .expect("tombstone")
                .item
                .is_none()
        );

        let mut all_day = event();
        all_day.start = Some(EventDateTime {
            date: Some("2026-03-29".to_owned()),
            date_time: None,
            time_zone: Some("Europe/Madrid".to_owned()),
        });
        all_day.end = Some(EventDateTime {
            date: Some("2026-03-30".to_owned()),
            date_time: None,
            time_zone: Some("Europe/Madrid".to_owned()),
        });
        let item = normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", all_day)
            .expect("all-day event")
            .item
            .expect("upsert");
        assert_eq!(item.duration_seconds, Some(23 * 60 * 60));
        assert_eq!(
            item.flexible_constraints["calendar_context"]["all_day"],
            true
        );

        let mut fall_back = event();
        fall_back.start = Some(EventDateTime {
            date: Some("2026-10-25".to_owned()),
            date_time: None,
            time_zone: Some("Europe/Madrid".to_owned()),
        });
        fall_back.end = Some(EventDateTime {
            date: Some("2026-10-26".to_owned()),
            date_time: None,
            time_zone: Some("Europe/Madrid".to_owned()),
        });
        assert_eq!(
            normalize_event(
                &collection(GoogleSyncRole::Blocking, true),
                "UTC",
                fall_back,
            )
            .expect("fall-back all-day event")
            .item
            .expect("upsert")
            .duration_seconds,
            Some(25 * 60 * 60)
        );
    }

    #[test]
    fn occurrence_timestamps_are_canonicalized_to_postgres_microseconds() {
        let mut precise = event();
        precise.start.as_mut().expect("start").date_time =
            Some("2026-08-29T10:00:00.123456789+02:00".to_owned());
        precise.end.as_mut().expect("end").date_time =
            Some("2026-08-29T11:00:00.987654321+02:00".to_owned());
        precise.updated = Some("2026-08-29T08:00:00.999999999Z".to_owned());

        let change = normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", precise)
            .expect("precise occurrence");
        let item = change.item.expect("upsert");
        assert_eq!(
            item.earliest_start_at,
            Some(
                "2026-08-29T08:00:00.123456Z"
                    .parse()
                    .expect("microsecond start")
            )
        );
        assert_eq!(
            item.deadline_at,
            Some(
                "2026-08-29T09:00:00.987654Z"
                    .parse()
                    .expect("microsecond end")
            )
        );
        assert_eq!(
            change.remote_updated_at,
            Some(
                "2026-08-29T08:00:00.999999Z"
                    .parse()
                    .expect("microsecond update")
            )
        );
        assert_eq!(
            item.flexible_constraints["calendar_event"]["start"],
            "2026-08-29T08:00:00.123456Z"
        );
        assert_eq!(
            item.flexible_constraints["calendar_event"]["end"],
            "2026-08-29T09:00:00.987654Z"
        );
    }

    #[test]
    fn task_tombstones_completion_hidden_parent_and_due_are_preserved() {
        let mut tasks_collection = collection(GoogleSyncRole::ReadOnly, true);
        tasks_collection.kind = GoogleCollectionKind::TaskList;
        let task = GoogleTask {
            id: "task-1".to_owned(),
            etag: Some("etag".to_owned()),
            title: "Do it".to_owned(),
            notes: Some("Details".to_owned()),
            status: Some("completed".to_owned()),
            due: Some("2026-08-30T00:00:00.000Z".to_owned()),
            completed: Some("2026-08-29T12:00:00Z".to_owned()),
            updated: Some("2026-08-29T12:00:00Z".to_owned()),
            parent: Some("parent-1".to_owned()),
            position: Some("0001".to_owned()),
            links: None,
            deleted: false,
            hidden: true,
        };
        let mut hidden_tasks_collection = tasks_collection.clone();
        hidden_tasks_collection.visible = false;
        let hidden = normalize_task(&hidden_tasks_collection, task.clone())
            .expect("hidden task")
            .item
            .expect("hidden upsert");
        assert!(hidden.is_sensitive);

        let change = normalize_task(&tasks_collection, task).expect("task");
        let item = change.item.expect("upsert");
        assert!(!item.is_sensitive);
        assert_eq!(item.status, ItemStatus::Completed);
        assert_eq!(change.remote_parent_id.as_deref(), Some("parent-1"));
        assert_eq!(item.flexible_constraints["google_sync"]["hidden"], true);
        assert_eq!(
            item.flexible_constraints["google_sync"]["provider_completed_at"],
            "2026-08-29T12:00:00Z"
        );
        assert!(item.deadline_at.is_some());
    }

    #[test]
    fn outbound_calendar_requires_owned_firm_block_and_uses_stable_id() {
        let now = Utc::now();
        let input = NewItem {
            id: Uuid::from_u128(44),
            is_sensitive: false,
            kind: ItemKind::Event,
            status: ItemStatus::Scheduled,
            title: "Focus".to_owned(),
            notes: None,
            timezone_name: "UTC".to_owned(),
            duration_seconds: Some(3600),
            deadline_at: Some(now + Duration::hours(1)),
            earliest_start_at: Some(now),
            recurrence: None,
            flexible_constraints: json!({"dayweave_firm_block": {
                "owned": true,
                "starts_at": now,
                "ends_at": now + Duration::hours(1),
            }}),
            split_policy: SplitPolicy::Indivisible,
            importance: 0,
            urgency: 0,
            parent_id: None,
            sibling_order: 0,
        };
        let item = crate::items::Item::new(input, now).expect("item");
        let collection = collection(GoogleSyncRole::Writable, true);
        let cipher = test_marker_cipher();
        let scope = test_oauth_scope();
        let prepared =
            prepare_calendar_outbound(item, OutboundOperation::Upsert, &collection, &cipher, scope)
                .expect("prepare");
        let event: GoogleEvent = serde_json::from_value(prepared.payload).expect("event");
        assert_eq!(
            event.id,
            deterministic_calendar_event_id(&cipher, scope, &collection, Uuid::from_u128(44))
                .expect("stable contextual ID")
        );
        assert!(event.attendees.is_empty());
        assert!(calendar_event_owned_by(
            &event,
            Uuid::from_u128(44),
            collection.account_id,
            collection.id,
            &cipher,
            scope,
        ));
        assert!(!calendar_event_owned_by(
            &event,
            Uuid::from_u128(45),
            collection.account_id,
            collection.id,
            &cipher,
            scope,
        ));
        let private = &event.extended_properties.as_ref().expect("proof").private;
        assert!(private.contains_key("dayweaveOwnershipProof"));
        assert!(!private.contains_key("dayweaveItemId"));
        assert!(
            !private
                .values()
                .any(|value| value.contains(&Uuid::from_u128(44).to_string()))
        );
        let source_metadata =
            normalize_calendar_series_authenticated(&collection, event.clone(), &cipher, scope)
                .expect("authenticated source metadata");
        assert_eq!(source_metadata.dayweave_item_id, Some(Uuid::from_u128(44)));
        assert!(source_metadata.reviewed_provider_projection.is_some());
        let echoed = normalize_event_authenticated(&collection, "UTC", event, &cipher, scope)
            .expect("authenticated expanded echo");
        assert_eq!(echoed.dayweave_item_id, Some(Uuid::from_u128(44)));
        assert!(echoed.reviewed_provider_projection.is_some());
        assert!(echoed.item.is_some());
    }

    #[test]
    fn calendar_provider_identity_survives_encryption_key_rotation() {
        let keys = Arc::new(BTreeMap::from([
            (1, crate::config::CredentialKey::from_test_bytes([0x31; 32])),
            (2, crate::config::CredentialKey::from_test_bytes([0x32; 32])),
        ]));
        let before_rotation = SecretCipher::new_with_identity(keys.clone(), 1, 1);
        let after_rotation = SecretCipher::new_with_identity(keys, 2, 1);
        let collection = collection(GoogleSyncRole::Writable, true);
        let scope = test_oauth_scope();
        let item = firm_event_item(
            702,
            "Europe/Madrid",
            "2026-08-29T08:00:00Z",
            "2026-08-29T09:00:00Z",
            false,
            false,
            true,
        );
        let before: GoogleEvent = serde_json::from_value(
            prepare_calendar_outbound(
                item.clone(),
                OutboundOperation::Upsert,
                &collection,
                &before_rotation,
                scope,
            )
            .expect("pre-rotation event")
            .payload,
        )
        .expect("pre-rotation provider event");
        let after: GoogleEvent = serde_json::from_value(
            prepare_calendar_outbound(
                item,
                OutboundOperation::Upsert,
                &collection,
                &after_rotation,
                scope,
            )
            .expect("post-rotation event")
            .payload,
        )
        .expect("post-rotation provider event");

        assert_eq!(before.id, after.id);
        let before_proof =
            &before.extended_properties.expect("before proof").private["dayweaveOwnershipProof"];
        let after_proof =
            &after.extended_properties.expect("after proof").private["dayweaveOwnershipProof"];
        assert!(before_proof.starts_with("dwm1.v1."));
        assert!(after_proof.starts_with("dwm1.v2."));
        assert_ne!(before_proof, after_proof);
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One table-like regression covers every publication policy dimension.
    fn outbound_calendar_policy_is_opt_in_and_preserves_dst_date_bounds() {
        let cipher = test_marker_cipher();
        let scope = test_oauth_scope();
        let defaults = collection(GoogleSyncRole::Writable, true);
        for item in [
            firm_event_item(
                601,
                "Europe/Madrid",
                "2026-03-28T23:00:00Z",
                "2026-03-29T22:00:00Z",
                true,
                false,
                true,
            ),
            firm_event_item(
                602,
                "Europe/Madrid",
                "2026-08-29T08:00:00Z",
                "2026-08-29T09:00:00Z",
                false,
                true,
                true,
            ),
            firm_event_item(
                603,
                "Europe/Madrid",
                "2026-08-29T08:00:00Z",
                "2026-08-29T09:00:00Z",
                false,
                false,
                false,
            ),
        ] {
            assert!(matches!(
                prepare_calendar_outbound(
                    item,
                    OutboundOperation::Upsert,
                    &defaults,
                    &cipher,
                    scope,
                ),
                Err(GoogleSyncServiceError::OutboundPolicyDenied)
            ));
        }

        let mut enabled = defaults;
        enabled.calendar_policy.publish_all_day = true;
        enabled.calendar_policy.publish_tentative = true;
        enabled.calendar_policy.publish_free = true;
        let spring: GoogleEvent = serde_json::from_value(
            prepare_calendar_outbound(
                firm_event_item(
                    604,
                    "Europe/Madrid",
                    "2026-03-28T23:00:00Z",
                    "2026-03-29T22:00:00Z",
                    true,
                    false,
                    true,
                ),
                OutboundOperation::Upsert,
                &enabled,
                &cipher,
                scope,
            )
            .expect("spring all-day publication")
            .payload,
        )
        .expect("spring provider event");
        assert_eq!(
            spring.start.and_then(|bound| bound.date).as_deref(),
            Some("2026-03-29")
        );
        assert_eq!(
            spring.end.and_then(|bound| bound.date).as_deref(),
            Some("2026-03-30")
        );

        let fall: GoogleEvent = serde_json::from_value(
            prepare_calendar_outbound(
                firm_event_item(
                    605,
                    "Europe/Madrid",
                    "2026-10-24T22:00:00Z",
                    "2026-10-25T23:00:00Z",
                    true,
                    false,
                    true,
                ),
                OutboundOperation::Upsert,
                &enabled,
                &cipher,
                scope,
            )
            .expect("fall all-day publication")
            .payload,
        )
        .expect("fall provider event");
        assert_eq!(
            fall.start.and_then(|bound| bound.date).as_deref(),
            Some("2026-10-25")
        );
        assert_eq!(
            fall.end.and_then(|bound| bound.date).as_deref(),
            Some("2026-10-26")
        );

        let tentative: GoogleEvent = serde_json::from_value(
            prepare_calendar_outbound(
                firm_event_item(
                    606,
                    "Europe/Madrid",
                    "2026-08-29T08:00:00Z",
                    "2026-08-29T09:00:00Z",
                    false,
                    true,
                    true,
                ),
                OutboundOperation::Upsert,
                &enabled,
                &cipher,
                scope,
            )
            .expect("tentative publication")
            .payload,
        )
        .expect("tentative provider event");
        assert_eq!(tentative.status.as_deref(), Some("tentative"));

        let free: GoogleEvent = serde_json::from_value(
            prepare_calendar_outbound(
                firm_event_item(
                    607,
                    "Europe/Madrid",
                    "2026-08-29T08:00:00Z",
                    "2026-08-29T09:00:00Z",
                    false,
                    false,
                    false,
                ),
                OutboundOperation::Upsert,
                &enabled,
                &cipher,
                scope,
            )
            .expect("free publication")
            .payload,
        )
        .expect("free provider event");
        assert_eq!(free.transparency.as_deref(), Some("transparent"));
    }

    #[test]
    fn ownership_proof_is_authenticated_and_target_bound() {
        let item_id = Uuid::from_u128(55);
        let collection = collection(GoogleSyncRole::Writable, true);
        let cipher = test_marker_cipher();
        let scope = test_oauth_scope();
        let sealed = cipher
            .seal(
                item_id.as_bytes(),
                &calendar_marker_aad(scope, collection.account_id, collection.id),
            )
            .expect("seal synthetic proof");
        let mut marked_event = event();
        marked_event.extended_properties = Some(ExtendedProperties {
            private: BTreeMap::from([(
                "dayweaveOwnershipProof".to_owned(),
                format!(
                    "dwm1.v{}.{}",
                    sealed.key_version,
                    URL_SAFE_NO_PAD.encode(&sealed.ciphertext)
                ),
            )]),
            shared: BTreeMap::new(),
        });
        assert_eq!(
            calendar_dayweave_item_id(&marked_event, &collection, &cipher, scope)
                .expect("valid event proof"),
            Some(item_id)
        );
        let mut other_collection = collection.clone();
        other_collection.id = Uuid::from_u128(999);
        assert!(matches!(
            calendar_dayweave_item_id(&marked_event, &other_collection, &cipher, scope),
            Err(NormalizationError::Rejected("dayweave_marker_invalid"))
        ));
        let other_scope = OAuthScope {
            workspace_id: scope.workspace_id,
            user_id: Uuid::from_u128(0x201),
        };
        assert!(matches!(
            calendar_dayweave_item_id(&marked_event, &collection, &cipher, other_scope),
            Err(NormalizationError::Rejected("dayweave_marker_invalid"))
        ));
        let base_id = deterministic_calendar_event_id(&cipher, scope, &collection, item_id)
            .expect("contextual ID");
        let other_target_id =
            deterministic_calendar_event_id(&cipher, scope, &other_collection, item_id)
                .expect("other target ID");
        let other_user_id =
            deterministic_calendar_event_id(&cipher, other_scope, &collection, item_id)
                .expect("other user ID");
        assert_ne!(base_id, other_target_id);
        assert_ne!(base_id, other_user_id);
        assert!(!base_id.contains(&item_id.to_string()));
        let mut legacy = event();
        legacy.extended_properties = Some(ExtendedProperties {
            private: BTreeMap::from([("dayweaveItemId".to_owned(), item_id.to_string())]),
            shared: BTreeMap::new(),
        });
        assert!(matches!(
            calendar_dayweave_item_id(&legacy, &collection, &cipher, scope),
            Err(NormalizationError::Rejected(
                "unauthenticated_dayweave_marker"
            ))
        ));
    }

    #[test]
    fn task_crash_or_lost_create_response_is_never_retried_without_provider_identity() {
        assert!(guard_new_task_insert(false).is_ok());
        assert!(matches!(
            guard_new_task_insert(true),
            Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
        ));
    }

    #[test]
    fn markerless_task_create_rejects_every_unusable_success_identity() {
        let nonce = Uuid::from_u128(0x701);
        let valid = GoogleTask {
            id: "provider-task-1".to_owned(),
            etag: Some("etag-1".to_owned()),
            title: "Synthetic task".to_owned(),
            notes: None,
            status: Some("needsAction".to_owned()),
            due: None,
            completed: None,
            updated: Some("2026-08-29T12:00:00Z".to_owned()),
            parent: None,
            position: None,
            links: None,
            deleted: false,
            hidden: false,
        };
        assert!(matches!(
            markerless_task_create_result(
                ProviderWriteResponse::Task(Box::new(valid.clone())),
                nonce,
            ),
            Ok(OutboundResult {
                remote_resource_id,
                ..
            }) if remote_resource_id == valid.id
        ));

        let mut missing_id = valid.clone();
        missing_id.id.clear();
        let mut missing_etag = valid.clone();
        missing_etag.etag = None;
        let mut invalid_updated = valid;
        invalid_updated.updated = Some("not-a-provider-timestamp".to_owned());
        for response in [
            ProviderWriteResponse::Task(Box::new(missing_id)),
            ProviderWriteResponse::Task(Box::new(missing_etag)),
            ProviderWriteResponse::Task(Box::new(invalid_updated)),
            ProviderWriteResponse::Empty,
            ProviderWriteResponse::Event(Box::new(event())),
        ] {
            assert!(matches!(
                markerless_task_create_result(response, nonce),
                Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
            ));
        }
    }

    #[test]
    fn calendar_lost_create_response_is_adopted_only_for_unchanged_reviewed_intent() {
        let item = firm_event_item(
            701,
            "Europe/Madrid",
            "2026-08-29T08:00:00Z",
            "2026-08-29T09:00:00Z",
            false,
            false,
            true,
        );
        let intended: GoogleEvent = serde_json::from_value(
            prepare_calendar_outbound(
                item,
                OutboundOperation::Upsert,
                &collection(GoogleSyncRole::Writable, true),
                &test_marker_cipher(),
                test_oauth_scope(),
            )
            .expect("prepared event")
            .payload,
        )
        .expect("intended provider event");
        let mut accepted = intended.clone();
        accepted.etag = Some("etag-after-create".to_owned());
        accepted.updated = Some("2026-08-29T09:00:00Z".to_owned());
        accepted.sequence = Some(1);
        accepted.additional_properties.insert(
            "htmlLink".to_owned(),
            json!("https://calendar.google.test/event/provider-assigned"),
        );
        assert!(calendar_event_matches_intent(&accepted, &intended));

        accepted.summary = Some("Edited in Google".to_owned());
        assert!(!calendar_event_matches_intent(&accepted, &intended));

        accepted.summary = intended.summary.clone();
        accepted
            .additional_properties
            .insert("guestsCanModify".to_owned(), json!(true));
        assert!(!calendar_event_matches_intent(&accepted, &intended));
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Mutates each authorization dimension against one stable fixture.
    fn approval_capabilities_are_strict_and_intent_hashes_bind_every_target() {
        let token = format!(
            "{APPROVAL_TOKEN_PREFIX}{}",
            URL_SAFE_NO_PAD.encode([7_u8; 32])
        );
        assert!(approval_capability_hash(&token).is_ok());
        assert!(approval_capability_hash("true").is_err());
        assert!(approval_capability_hash(&format!("{token}x")).is_err());

        let mut collection = collection(GoogleSyncRole::Writable, true);
        collection.kind = GoogleCollectionKind::TaskList;
        let scope = test_oauth_scope();
        let now = Utc::now();
        let item = crate::items::Item::new(
            NewItem {
                id: Uuid::from_u128(501),
                is_sensitive: false,
                kind: ItemKind::Task,
                status: ItemStatus::Inbox,
                title: "Bound task".to_owned(),
                notes: None,
                timezone_name: "UTC".to_owned(),
                duration_seconds: None,
                deadline_at: None,
                earliest_start_at: None,
                recurrence: None,
                flexible_constraints: json!({}),
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
            },
            now,
        )
        .expect("item");
        let prepared = prepare_task_outbound(item, OutboundOperation::Upsert).expect("task");
        let base = outbound_intent_hash(
            scope.workspace_id,
            scope.user_id,
            collection.account_id,
            collection.id,
            collection.revision,
            &collection.remote_collection_id,
            collection.kind,
            GOOGLE_TASKS_SCOPE,
            prepared.item.id,
            prepared.item.revision,
            prepared.entity_kind,
            prepared.operation,
            &prepared.payload,
            Some("task-1"),
            Some("etag-1"),
        )
        .expect("hash");
        assert_ne!(
            base,
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                Uuid::from_u128(502),
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            )
            .expect("account-swapped hash")
        );
        assert_ne!(
            base,
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision,
                "other-list",
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            )
            .expect("target-swapped hash")
        );
        for changed in [
            outbound_intent_hash(
                Uuid::from_u128(900),
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                Uuid::from_u128(901),
                collection.account_id,
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                Uuid::from_u128(902),
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_CALENDAR_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                Uuid::from_u128(903),
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &json!({"title": "mutated after approval"}),
                Some("task-1"),
                Some("etag-1"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision + 1,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-2"),
                Some("etag-1"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-2"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision + 1,
                prepared.entity_kind,
                prepared.operation,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            ),
            outbound_intent_hash(
                scope.workspace_id,
                scope.user_id,
                collection.account_id,
                collection.id,
                collection.revision,
                &collection.remote_collection_id,
                collection.kind,
                GOOGLE_TASKS_SCOPE,
                prepared.item.id,
                prepared.item.revision,
                prepared.entity_kind,
                OutboundOperation::Delete,
                &prepared.payload,
                Some("task-1"),
                Some("etag-1"),
            ),
        ] {
            assert_ne!(base, changed.expect("mutated binding hash"));
        }
    }

    #[test]
    fn task_markers_are_never_identity_and_are_not_disclosed() {
        let canary = Uuid::from_u128(77).to_string();
        let notes = format!("keep this\n[DayWeave item:{canary}]\nkeep that");
        let (sanitized, stripped) = sanitize_task_notes(Some(&notes));
        assert!(stripped);
        let sanitized = sanitized.expect("remaining notes");
        assert_eq!(sanitized, "keep this\nkeep that");
        assert!(!sanitized.contains(&canary));

        let mut tasks_collection = collection(GoogleSyncRole::ReadOnly, true);
        tasks_collection.kind = GoogleCollectionKind::TaskList;
        let change = normalize_task(
            &tasks_collection,
            GoogleTask {
                id: "remote-a".to_owned(),
                etag: Some("etag-a".to_owned()),
                title: "Task".to_owned(),
                notes: Some(notes),
                status: Some("needsAction".to_owned()),
                due: None,
                completed: None,
                updated: None,
                parent: None,
                position: None,
                links: None,
                deleted: false,
                hidden: false,
            },
        )
        .expect("normalize");
        assert!(change.dayweave_item_id.is_none());
        let mut normalized = change.item.expect("normalized task");
        assert!(
            !serde_json::to_string(&normalized)
                .expect("serialize")
                .contains(&canary)
        );

        normalized.notes = Some(format!("local details\n[dAyWeAvE ItEm:{canary}]"));
        let local = crate::items::Item::new(normalized, Utc::now()).expect("legacy local task");
        let prepared = prepare_task_outbound(local, OutboundOperation::Upsert)
            .expect("prepare sanitized task");
        assert!(!prepared.payload.to_string().contains(&canary));
        assert_eq!(prepared.payload["notes"], "local details");
    }

    #[test]
    fn task_control_obfuscated_markers_are_stripped_before_display_normalization() {
        let canary = Uuid::from_u128(78).to_string();
        let notes = format!(
            "ordinary first line\n[DayWeave\t\0\u{7}item:{canary}]\r\nordinary second\u{7}line\n[DayWeave\r\nitem:{canary}]\nordinary third line"
        );
        let (retained, stripped) = sanitize_task_notes(Some(&notes));
        assert!(stripped);
        assert_eq!(
            retained.as_deref(),
            Some("ordinary first line\nordinary second line\nordinary third line")
        );

        let mut tasks_collection = collection(GoogleSyncRole::ReadOnly, true);
        tasks_collection.kind = GoogleCollectionKind::TaskList;
        let change = normalize_task(
            &tasks_collection,
            GoogleTask {
                id: "remote-control-marker".to_owned(),
                etag: Some("etag-control-marker".to_owned()),
                title: "Task".to_owned(),
                notes: Some(notes),
                status: Some("needsAction".to_owned()),
                due: None,
                completed: None,
                updated: None,
                parent: None,
                position: None,
                links: None,
                deleted: false,
                hidden: false,
            },
        )
        .expect("control-obfuscated legacy marker is safely normalized");
        let item = change.item.expect("normalized task");

        assert_eq!(
            item.notes.as_deref(),
            Some("ordinary first line ordinary second line ordinary third line")
        );
        assert_eq!(
            item.flexible_constraints["google_sync"]["legacy_marker_stripped"],
            true
        );
        assert!(
            !item
                .notes
                .as_deref()
                .expect("retained notes")
                .contains(&canary)
        );
        assert!(
            !item
                .notes
                .as_deref()
                .expect("retained notes")
                .chars()
                .any(char::is_control)
        );
    }

    #[test]
    fn successful_provider_upserts_require_an_etag_for_future_conditional_writes() {
        let mut event = event();
        event.etag = None;
        assert!(matches!(
            outbound_event_result(&event, Uuid::from_u128(1)),
            Err(GoogleSyncServiceError::ProviderProtocol)
        ));
        let task = GoogleTask {
            id: "task-without-etag".to_owned(),
            etag: None,
            title: "Task".to_owned(),
            notes: None,
            status: Some("needsAction".to_owned()),
            due: None,
            completed: None,
            updated: None,
            parent: None,
            position: None,
            links: None,
            deleted: false,
            hidden: false,
        };
        assert!(matches!(
            outbound_task_result(&task, Uuid::from_u128(1)),
            Err(GoogleSyncServiceError::ProviderProtocol)
        ));
    }

    #[test]
    fn transient_backoff_is_bounded_and_exponential() {
        assert_eq!(exponential_backoff(0), Duration::seconds(30));
        assert_eq!(exponential_backoff(1), Duration::seconds(60));
        assert_eq!(exponential_backoff(6), Duration::minutes(32));
        assert_eq!(exponential_backoff(7), Duration::hours(1));
        assert_eq!(exponential_backoff(u32::MAX), Duration::hours(1));
    }
}
