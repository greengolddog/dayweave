use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
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
    items::{
        DeadlineKind, DeadlineStrength, DurationKind, DurationSource, ItemKind, ItemService,
        ItemServiceError, ItemStatus, NewItem, SplitPolicy,
    },
    proposals::Clock,
};

use super::{
    CalendarProjectionBatch, CalendarProjectionWindow, CursorValue, DiscoveredCollection,
    GoogleCalendarPolicy, GoogleCollectionKind, GoogleEventDisposition, GoogleOutboundAccepted,
    GoogleOutboundApproval, GoogleOutboundPreview, GoogleSyncCollection, GoogleSyncRefreshAccepted,
    GoogleSyncRepository, GoogleSyncRepositoryError, GoogleSyncRole, GoogleSyncStatus,
    GoogleTaskProviderMetadata, OutboundApprovalSpec, OutboundEnqueueSpec, OutboundOperation,
    OutboundPreviewSpec, OutboundRequest, OutboundResult, OutboundWork, PreparedOutbound,
    PreparedSchedulePublicationChange, RejectedRemoteItem, RemoteCalendarSeriesChange,
    RemoteItemChange, ScheduleGooglePublicationAccepted, ScheduleGooglePublicationApproval,
    ScheduleGooglePublicationOperation, ScheduleGooglePublicationPreview,
    ScheduleGooglePublicationStatus, SchedulePublicationApprovalSpec,
    SchedulePublicationDispatchPermit, SchedulePublicationEnqueueSpec,
    SchedulePublicationObservationSource, SchedulePublicationPreviewSpec,
    SchedulePublicationResult, SchedulePublicationSource, SchedulePublicationWork, SyncClaim,
    SyncCounts, SyncFailureKind,
};

#[cfg(test)]
use super::{ScheduleBlockMapping, SchedulePublicationBlock};

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
const SCHEDULE_APPROVAL_TOKEN_PREFIX: &str = "dw_gsa1_";
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

async fn replay_or_accept_refresh<Lookup, LookupFuture, Gate, GateFuture, Accept, AcceptFuture>(
    lookup: Lookup,
    active_account_gate: Gate,
    accept: Accept,
) -> Result<GoogleSyncRefreshAccepted, GoogleSyncServiceError>
where
    Lookup: FnOnce() -> LookupFuture,
    LookupFuture:
        Future<Output = Result<Option<GoogleSyncRefreshAccepted>, GoogleSyncServiceError>>,
    Gate: FnOnce() -> GateFuture,
    GateFuture: Future<Output = Result<(), GoogleSyncServiceError>>,
    Accept: FnOnce() -> AcceptFuture,
    AcceptFuture: Future<Output = Result<GoogleSyncRefreshAccepted, GoogleSyncServiceError>>,
{
    if let Some(accepted) = lookup().await? {
        return Ok(accepted);
    }
    active_account_gate().await?;
    accept().await
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
    schedule_outbound_enabled: bool,
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
    pub(crate) const fn scope(&self) -> OAuthScope {
        self.scope
    }

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
        schedule_outbound_enabled: bool,
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
            schedule_outbound_enabled,
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
                loop {
                    match service.drain_one().await {
                        Ok(true) => tokio::task::yield_now().await,
                        Ok(false) => break,
                        Err(error) => {
                            tracing::warn!(
                                error_code = error.code(),
                                "Google sync worker iteration failed"
                            );
                            break;
                        }
                    }
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
        replay_or_accept_refresh(
            || async {
                Ok(self
                    .repository
                    .refresh_request(account_id, request_id)
                    .await?)
            },
            || async {
                self.oauth.account_for_sync(account_id).await?;
                Ok(())
            },
            || async {
                Ok(self
                    .repository
                    .request_refresh(account_id, request_id, self.clock.now())
                    .await?)
            },
        )
        .await
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
        let mut random = Zeroizing::new([0_u8; APPROVAL_TOKEN_RANDOM_BYTES]);
        getrandom::fill(&mut *random).map_err(|_| GoogleSyncServiceError::Randomness)?;
        let mut capability =
            Zeroizing::new(String::with_capacity(APPROVAL_TOKEN_PREFIX.len() + 43));
        capability.push_str(APPROVAL_TOKEN_PREFIX);
        let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(random.as_slice()));
        capability.push_str(encoded.as_str());
        let capability_hash = Sha256::digest(capability.as_bytes()).into();
        let expires_at = self
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
            .await?;
        Ok(GoogleOutboundApproval {
            preview_id,
            approval_capability: std::mem::take(&mut *capability),
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

    pub(crate) async fn preview_schedule_publication(
        &self,
        account_id: Uuid,
        collection_id: Uuid,
        expected_schedule_revision_id: Uuid,
    ) -> Result<ScheduleGooglePublicationPreview, GoogleSyncServiceError> {
        self.require_schedule_outbound_enabled()?;
        if account_id.is_nil() || collection_id.is_nil() || expected_schedule_revision_id.is_nil() {
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
        let account = self.oauth.account_for_sync(account_id).await?;
        if !account.granted_scopes.contains(GOOGLE_CALENDAR_SCOPE) {
            return Err(GoogleSyncServiceError::MissingWriteScope);
        }
        let source = self
            .repository
            .load_schedule_publication_source(
                account_id,
                collection_id,
                expected_schedule_revision_id,
            )
            .await?;
        let changes = build_schedule_publication_changes(
            &source,
            &self.cipher,
            self.scope,
            self.clock.now(),
        )?;
        let expires_at = self.clock.now()
            + Duration::from_std(self.approval_ttl)
                .map_err(|_| GoogleSyncServiceError::Internal)?;
        self.repository
            .create_schedule_publication_preview(
                SchedulePublicationPreviewSpec {
                    id: Uuid::new_v4(),
                    source,
                    changes,
                    expires_at,
                },
                self.clock.now(),
            )
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn approve_schedule_publication(
        &self,
        account_id: Uuid,
        preview_id: Uuid,
        expected_preview_hash: &str,
    ) -> Result<ScheduleGooglePublicationApproval, GoogleSyncServiceError> {
        self.require_schedule_outbound_enabled()?;
        if account_id.is_nil() || preview_id.is_nil() {
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
        let expected_preview_hash = decode_hash(expected_preview_hash)?;
        let mut random = Zeroizing::new([0_u8; APPROVAL_TOKEN_RANDOM_BYTES]);
        getrandom::fill(&mut *random).map_err(|_| GoogleSyncServiceError::Randomness)?;
        let mut capability = Zeroizing::new(String::with_capacity(
            SCHEDULE_APPROVAL_TOKEN_PREFIX.len() + 43,
        ));
        capability.push_str(SCHEDULE_APPROVAL_TOKEN_PREFIX);
        let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(random.as_slice()));
        capability.push_str(encoded.as_str());
        let capability_hash = Sha256::digest(capability.as_bytes()).into();
        let expires_at = self
            .repository
            .approve_schedule_publication(
                SchedulePublicationApprovalSpec {
                    account_id,
                    preview_id,
                    expected_preview_hash,
                    capability_hash,
                },
                self.clock.now(),
            )
            .await?;
        Ok(ScheduleGooglePublicationApproval {
            preview_id,
            approval_capability: std::mem::take(&mut *capability),
            expires_at,
        })
    }

    pub(crate) async fn enqueue_schedule_publication(
        &self,
        account_id: Uuid,
        preview_id: Uuid,
        collection_id: Uuid,
        expected_schedule_revision_id: Uuid,
        mut approval_capability: String,
    ) -> Result<ScheduleGooglePublicationAccepted, GoogleSyncServiceError> {
        if account_id.is_nil()
            || preview_id.is_nil()
            || collection_id.is_nil()
            || expected_schedule_revision_id.is_nil()
        {
            approval_capability.zeroize();
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
        let capability_hash = match schedule_approval_capability_hash(&approval_capability) {
            Ok(hash) => hash,
            Err(error) => {
                approval_capability.zeroize();
                return Err(error);
            }
        };
        approval_capability.zeroize();
        let spec = SchedulePublicationEnqueueSpec {
            account_id,
            preview_id,
            collection_id,
            expected_schedule_revision_id,
            capability_hash,
        };
        if let Some(accepted) = self
            .repository
            .schedule_publication_acceptance(&spec)
            .await?
        {
            return Ok(accepted);
        }
        self.require_schedule_outbound_enabled()?;
        self.repository
            .enqueue_schedule_publication(spec, self.clock.now())
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn schedule_publication_status(
        &self,
        account_id: Uuid,
        publication_id: Uuid,
    ) -> Result<ScheduleGooglePublicationStatus, GoogleSyncServiceError> {
        if account_id.is_nil() || publication_id.is_nil() {
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
        self.repository
            .schedule_publication_status(account_id, publication_id)
            .await
            .map_err(Into::into)
    }

    fn require_outbound_enabled(&self) -> Result<(), GoogleSyncServiceError> {
        if self.outbound_enabled {
            Ok(())
        } else {
            Err(GoogleSyncServiceError::ExternalPublicationDisabled)
        }
    }

    fn require_schedule_outbound_enabled(&self) -> Result<(), GoogleSyncServiceError> {
        if self.outbound_enabled && self.schedule_outbound_enabled {
            Ok(())
        } else {
            Err(GoogleSyncServiceError::SchedulePublicationDisabled)
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

    async fn drain_one(&self) -> Result<bool, GoogleSyncServiceError> {
        let now = self.clock.now();
        let claim = self
            .repository
            .claim_due(now, now + Duration::minutes(RUN_LEASE_MINUTES))
            .await?;
        let Some(claim) = claim else {
            return Ok(false);
        };
        match self.sync_claim(&claim, now).await {
            Ok(counts) => {
                self.repository
                    .complete_claim(
                        &claim,
                        &counts,
                        self.schedule_outbound_enabled,
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
        Ok(true)
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
        if self.schedule_outbound_enabled {
            self.process_schedule_outbound(claim).await?;
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

    #[allow(clippy::too_many_lines)] // One bounded page loop keeps projection and cursor accounting atomic.
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
            let page_remote_ids = page
                .items
                .iter()
                .map(|event| event.id.clone())
                .collect::<Vec<_>>();
            let known_schedule_remote_ids = self
                .repository
                .known_schedule_publication_remote_ids(
                    collection.account_id,
                    collection.id,
                    &page_remote_ids,
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
                match classify_schedule_calendar_event(
                    &event,
                    &known_schedule_remote_ids,
                    collection.account_id,
                    collection.id,
                    &self.cipher,
                    self.scope,
                ) {
                    ScheduleCalendarEventDisposition::Generated => {
                        // Generated schedule events are projections of the
                        // already-canonical schedule. Re-importing them as
                        // source events would create a self-conflict and
                        // consume the same capacity twice.
                        continue;
                    }
                    ScheduleCalendarEventDisposition::Rejected(reason) => {
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
                        continue;
                    }
                    ScheduleCalendarEventDisposition::External => {}
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
            let page_remote_ids = page
                .items
                .iter()
                .map(|event| event.id.clone())
                .collect::<Vec<_>>();
            let known_schedule_remote_ids = self
                .repository
                .known_schedule_publication_remote_ids(
                    collection.account_id,
                    collection.id,
                    &page_remote_ids,
                )
                .await?;
            normalize_calendar_projection_events(
                collection,
                window,
                &page_timezone,
                page.items,
                &self.cipher,
                self.scope,
                &known_schedule_remote_ids,
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

    #[allow(clippy::too_many_lines)] // Keeps durable schedule-delivery outcomes beside transitions.
    async fn process_schedule_outbound(
        &self,
        claim: &SyncClaim,
    ) -> Result<(), GoogleSyncServiceError> {
        self.require_schedule_outbound_enabled()?;
        for _ in 0..MAX_OUTBOUND_PER_RUN {
            self.heartbeat(claim).await?;
            let Some(work) = self
                .repository
                .claim_schedule_publication(claim, self.clock.now())
                .await?
            else {
                return Ok(());
            };
            match self.deliver_schedule_publication(&work).await {
                Ok(result) => match self
                    .repository
                    .complete_schedule_publication(&work, result, self.clock.now())
                    .await
                {
                    Ok(()) | Err(GoogleSyncRepositoryError::ClaimLost) => {}
                    Err(error) => return Err(error.into()),
                },
                Err(GoogleSyncServiceError::Google(GoogleError::PreconditionFailed)) => {
                    self.repository
                        .fail_schedule_publication(
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
                        .fail_schedule_publication(
                            &work,
                            "conflict",
                            "conditional_write_required",
                            self.clock.now(),
                            self.clock.now(),
                        )
                        .await?;
                }
                Err(error) if schedule_ambiguous_response_code(&error).is_some() => {
                    // Both failures can follow a successful provider mutation:
                    // a response may carry an unusable identity, or exceed the
                    // bounded response reader. Keep the possible-send row
                    // claimable for exact read reconciliation and keep it in
                    // the active fence that blocks successor publication.
                    let code = schedule_ambiguous_response_code(&error)
                        .ok_or(GoogleSyncServiceError::Internal)?;
                    let now = self.clock.now();
                    self.repository
                        .fail_schedule_publication(
                            &work,
                            "backoff",
                            code,
                            now + exponential_backoff(work.attempts),
                            now,
                        )
                        .await?;
                }
                Err(GoogleSyncServiceError::Repository(GoogleSyncRepositoryError::ClaimLost)) => {}
                Err(GoogleSyncServiceError::Google(GoogleError::Api { status })) => {
                    let code = if status == 404 {
                        "provider_not_found"
                    } else {
                        "provider_rejected"
                    };
                    self.repository
                        .fail_schedule_publication(
                            &work,
                            "conflict",
                            code,
                            self.clock.now(),
                            self.clock.now(),
                        )
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
                        .fail_schedule_publication(
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

    #[allow(clippy::too_many_lines)] // Keeps guarded generated-event variants co-located.
    async fn deliver_schedule_publication(
        &self,
        work: &SchedulePublicationWork,
    ) -> Result<SchedulePublicationResult, GoogleSyncServiceError> {
        self.require_schedule_outbound_enabled()?;
        match work.operation {
            ScheduleGooglePublicationOperation::Create => {
                if work.remote_resource_id.is_some()
                    || work.expected_etag.is_some()
                    || work.mapping_id.is_some()
                {
                    return Err(GoogleSyncServiceError::OutboundPayloadCorrupt);
                }
                let event: GoogleEvent = serde_json::from_value(work.payload.clone())
                    .map_err(|_| GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                if !schedule_calendar_event_owned_by(
                    &event,
                    work.slot_id,
                    work.incarnation,
                    work.account_id,
                    work.collection_id,
                    &self.cipher,
                    self.scope,
                ) {
                    return Err(GoogleSyncServiceError::OutboundPayloadCorrupt);
                }
                self.repository
                    .renew_schedule_publication(work, self.clock.now())
                    .await?;
                let (result, nonce, observation_source) = match self
                    .provider
                    .get_event(work.account_id, &work.collection_remote_id, &event.id)
                    .await
                {
                    Ok(found) => {
                        if !schedule_calendar_event_owned_by(
                            &found,
                            work.slot_id,
                            work.incarnation,
                            work.account_id,
                            work.collection_id,
                            &self.cipher,
                            self.scope,
                        ) || !schedule_calendar_event_matches_intent(&found, &event)
                        {
                            return Err(GoogleError::PreconditionFailed.into());
                        }
                        if found.etag.is_none() {
                            return Err(GoogleError::InvalidResponse.into());
                        }
                        let permit = self.authorize_schedule_dispatch(work, false).await?;
                        (
                            found,
                            permit.nonce,
                            SchedulePublicationObservationSource::ReconciliationRead,
                        )
                    }
                    Err(GoogleError::Api { status: 404 }) => {
                        if work.provider_post_may_have_started {
                            self.require_current_schedule_calendar_write_access(work)
                                .await?;
                            if self
                                .repository
                                .reconcile_schedule_publication_no_effect(
                                    work,
                                    "create_not_observed",
                                    self.clock.now(),
                                )
                                .await?
                            {
                                return Err(GoogleSyncRepositoryError::ClaimLost.into());
                            }
                        }
                        let prepared = self
                            .prepare_write(self.provider.prepare_insert_event(
                                work.account_id,
                                &work.collection_remote_id,
                                &event,
                            ))
                            .await?;
                        let permit = self.authorize_schedule_dispatch(work, true).await?;
                        match self.send_schedule_prepared(work, prepared, &permit).await {
                            Ok(ProviderWriteResponse::Event(result)) => (
                                *result,
                                permit.nonce,
                                SchedulePublicationObservationSource::ProviderResponse,
                            ),
                            Ok(_) => return Err(GoogleError::InvalidResponse.into()),
                            Err(GoogleSyncServiceError::Google(GoogleError::Api {
                                status: 409,
                            })) => {
                                // Google reserves 409 for a duplicate explicit
                                // event ID. Re-read that deterministic ID and
                                // adopt only the exact authenticated intent;
                                // never manufacture a replacement identity.
                                let found = match self
                                    .provider
                                    .get_event(
                                        work.account_id,
                                        &work.collection_remote_id,
                                        &event.id,
                                    )
                                    .await
                                {
                                    Ok(found) => found,
                                    Err(GoogleError::Api { status: 404 }) => {
                                        return Err(GoogleError::InvalidResponse.into());
                                    }
                                    Err(error) => return Err(error.into()),
                                };
                                return schedule_create_conflict_result(
                                    &found,
                                    &event,
                                    work.slot_id,
                                    work.incarnation,
                                    work.account_id,
                                    work.collection_id,
                                    &self.cipher,
                                    self.scope,
                                    permit.nonce,
                                );
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error) => return Err(error.into()),
                };
                validate_schedule_write_response(
                    &result,
                    &event,
                    &event.id,
                    work.slot_id,
                    work.incarnation,
                    work.account_id,
                    work.collection_id,
                    &self.cipher,
                    self.scope,
                )?;
                schedule_publication_event_result(&result, nonce, observation_source)
            }
            ScheduleGooglePublicationOperation::Update => {
                let remote_id = work
                    .remote_resource_id
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or(GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                let etag = work
                    .expected_etag
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or(GoogleSyncServiceError::ProviderProtocol)?;
                if work.mapping_id.is_none() {
                    return Err(GoogleSyncServiceError::OutboundPayloadCorrupt);
                }
                let mut event: GoogleEvent = serde_json::from_value(work.payload.clone())
                    .map_err(|_| GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                if event.id != remote_id
                    || !schedule_calendar_event_owned_by(
                        &event,
                        work.slot_id,
                        work.incarnation,
                        work.account_id,
                        work.collection_id,
                        &self.cipher,
                        self.scope,
                    )
                {
                    return Err(GoogleSyncServiceError::OutboundPayloadCorrupt);
                }
                event.etag = Some(etag.to_owned());
                if work.provider_post_may_have_started {
                    self.repository
                        .renew_schedule_publication(work, self.clock.now())
                        .await?;
                    let found = match self
                        .provider
                        .get_event(work.account_id, &work.collection_remote_id, remote_id)
                        .await
                    {
                        Ok(found) => found,
                        Err(GoogleError::Api { status: 404 }) => {
                            self.require_current_schedule_calendar_write_access(work)
                                .await?;
                            // A conditional update cannot be retried without
                            // the exact current ETag. Keep the possible-send
                            // fence active until a later positive read or
                            // explicit operator reconciliation.
                            return Err(GoogleError::InvalidResponse.into());
                        }
                        Err(error) => return Err(error.into()),
                    };
                    match schedule_update_recovery_action(
                        &found,
                        &event,
                        etag,
                        work.slot_id,
                        work.incarnation,
                        work.account_id,
                        work.collection_id,
                        &self.cipher,
                        self.scope,
                    )? {
                        ScheduleUpdateRecoveryAction::Adopt => {
                            let permit = self.authorize_schedule_dispatch(work, false).await?;
                            return schedule_publication_event_result(
                                &found,
                                permit.nonce,
                                SchedulePublicationObservationSource::ReconciliationRead,
                            );
                        }
                        ScheduleUpdateRecoveryAction::Retry => {
                            if self
                                .repository
                                .reconcile_schedule_publication_no_effect(
                                    work,
                                    "update_not_observed",
                                    self.clock.now(),
                                )
                                .await?
                            {
                                return Err(GoogleSyncRepositoryError::ClaimLost.into());
                            }
                        }
                    }
                    // Dispatch authorization is durable before network I/O.
                    // A crash between those steps leaves the provider object
                    // untouched. The still-current old ETag proves that a
                    // conditional retry cannot overwrite a concurrent edit.
                }
                let (response, permit) = self
                    .execute_guarded_schedule_write(
                        work,
                        self.provider.prepare_update_event(
                            work.account_id,
                            &work.collection_remote_id,
                            &event,
                        ),
                    )
                    .await?;
                let ProviderWriteResponse::Event(result) = response else {
                    return Err(GoogleError::InvalidResponse.into());
                };
                validate_schedule_write_response(
                    &result,
                    &event,
                    remote_id,
                    work.slot_id,
                    work.incarnation,
                    work.account_id,
                    work.collection_id,
                    &self.cipher,
                    self.scope,
                )?;
                schedule_publication_event_result(
                    &result,
                    permit.nonce,
                    SchedulePublicationObservationSource::ProviderResponse,
                )
            }
            ScheduleGooglePublicationOperation::Delete => {
                let remote_id = work
                    .remote_resource_id
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or(GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                let etag = work
                    .expected_etag
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or(GoogleSyncServiceError::ProviderProtocol)?;
                if work.mapping_id.is_none() || work.source_block_id.is_some() {
                    return Err(GoogleSyncServiceError::OutboundPayloadCorrupt);
                }
                if work.provider_post_may_have_started {
                    self.repository
                        .renew_schedule_publication(work, self.clock.now())
                        .await?;
                    match self
                        .provider
                        .get_event(work.account_id, &work.collection_remote_id, remote_id)
                        .await
                    {
                        Err(GoogleError::Api { status: 404 }) => {
                            self.require_current_schedule_calendar_write_access(work)
                                .await?;
                            let permit = self.authorize_schedule_dispatch(work, false).await?;
                            return Ok(schedule_publication_absent_result(
                                remote_id,
                                permit.nonce,
                                SchedulePublicationObservationSource::ReconciliationRead,
                            ));
                        }
                        Ok(found) => match schedule_delete_recovery_action(
                            &found,
                            remote_id,
                            etag,
                            work.slot_id,
                            work.incarnation,
                            work.account_id,
                            work.collection_id,
                            &self.cipher,
                            self.scope,
                        )? {
                            ScheduleDeleteRecoveryAction::Absent => {
                                let permit = self.authorize_schedule_dispatch(work, false).await?;
                                return Ok(schedule_publication_absent_result(
                                    remote_id,
                                    permit.nonce,
                                    SchedulePublicationObservationSource::ReconciliationRead,
                                ));
                            }
                            ScheduleDeleteRecoveryAction::Retry => {
                                if self
                                    .repository
                                    .reconcile_schedule_publication_no_effect(
                                        work,
                                        "delete_not_observed",
                                        self.clock.now(),
                                    )
                                    .await?
                                {
                                    return Err(GoogleSyncRepositoryError::ClaimLost.into());
                                }
                            }
                        },
                        Err(error) => return Err(error.into()),
                    }
                }
                let prepared = self
                    .prepare_write(self.provider.prepare_delete_event(
                        work.account_id,
                        &work.collection_remote_id,
                        remote_id,
                        etag,
                    ))
                    .await?;
                let permit = self.authorize_schedule_dispatch(work, true).await?;
                let response = match self.send_schedule_prepared(work, prepared, &permit).await {
                    Ok(response) => response,
                    Err(GoogleSyncServiceError::Google(GoogleError::Api { status: 404 })) => {
                        self.require_current_schedule_calendar_write_access(work)
                            .await?;
                        return Ok(schedule_publication_absent_result(
                            remote_id,
                            permit.nonce,
                            SchedulePublicationObservationSource::ProviderResponse,
                        ));
                    }
                    Err(error) if schedule_delete_already_absent(&error) => {
                        return Ok(schedule_publication_absent_result(
                            remote_id,
                            permit.nonce,
                            SchedulePublicationObservationSource::ProviderResponse,
                        ));
                    }
                    Err(error) => return Err(error),
                };
                if !matches!(response, ProviderWriteResponse::Empty) {
                    return Err(GoogleError::InvalidResponse.into());
                }
                Ok(schedule_publication_absent_result(
                    remote_id,
                    permit.nonce,
                    SchedulePublicationObservationSource::ProviderResponse,
                ))
            }
            ScheduleGooglePublicationOperation::Noop => {
                Err(GoogleSyncServiceError::OutboundPayloadCorrupt)
            }
        }
    }

    async fn require_current_schedule_calendar_write_access(
        &self,
        work: &SchedulePublicationWork,
    ) -> Result<(), GoogleSyncServiceError> {
        let claim = SyncClaim {
            account_id: work.account_id,
            claim_id: work.run_claim_id,
            claim_generation: work.run_claim_generation,
        };
        let calendars = match self.discover_calendars(work.account_id, Some(&claim)).await {
            Err(GoogleSyncServiceError::Google(GoogleError::Api { status: 404 })) => {
                return Err(GoogleError::InvalidResponse.into());
            }
            result => result?,
        };
        if schedule_calendar_write_access_is_current(&calendars, &work.collection_remote_id) {
            Ok(())
        } else {
            // A provider 404 is intentionally ambiguous. Missing or downgraded
            // calendar access must be reconciled by the next discovery pass;
            // it is not proof that the exact generated event was deleted.
            Err(GoogleError::InvalidResponse.into())
        }
    }

    async fn authorize_schedule_dispatch(
        &self,
        work: &SchedulePublicationWork,
        provider_write: bool,
    ) -> Result<SchedulePublicationDispatchPermit, GoogleSyncServiceError> {
        let now = self.clock.now();
        let permit = self
            .repository
            .authorize_schedule_publication_dispatch(work, provider_write, now)
            .await?;
        if permit.intent_hash != work.intent_hash || permit.expires_at <= now {
            return Err(GoogleSyncRepositoryError::ClaimLost.into());
        }
        Ok(permit)
    }

    async fn execute_guarded_schedule_write<F>(
        &self,
        work: &SchedulePublicationWork,
        preparation: F,
    ) -> Result<(ProviderWriteResponse, SchedulePublicationDispatchPermit), GoogleSyncServiceError>
    where
        F: Future<Output = Result<Box<dyn PreparedGoogleSyncWrite>, GoogleError>>,
    {
        sequence_guarded_write(
            self.prepare_write(preparation),
            || self.authorize_schedule_dispatch(work, true),
            |prepared, permit| async move {
                let response = self.send_schedule_prepared(work, prepared, &permit).await?;
                Ok((response, permit))
            },
        )
        .await
    }

    async fn send_schedule_prepared(
        &self,
        work: &SchedulePublicationWork,
        prepared: Box<dyn PreparedGoogleSyncWrite>,
        permit: &SchedulePublicationDispatchPermit,
    ) -> Result<ProviderWriteResponse, GoogleSyncServiceError> {
        match prepared.send(SystemTime::from(permit.expires_at)).await {
            Err(GoogleError::DispatchInitiationExpired) => {
                let now = self.clock.now();
                self.repository
                    .cancel_schedule_publication_before_send(
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
    known_schedule_remote_ids: &BTreeSet<String>,
    changes: &mut Vec<RemoteItemChange>,
    rejected: &mut Vec<RejectedRemoteItem>,
    normalized_bytes: &mut usize,
) -> Result<(), GoogleSyncServiceError> {
    for event in events {
        let remote_id = event.id.clone();
        match classify_schedule_calendar_event(
            &event,
            known_schedule_remote_ids,
            collection.account_id,
            collection.id,
            cipher,
            scope,
        ) {
            ScheduleCalendarEventDisposition::Generated => continue,
            ScheduleCalendarEventDisposition::Rejected(reason) => {
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
                continue;
            }
            ScheduleCalendarEventDisposition::External => {}
        }
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
            &BTreeSet::new(),
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
            google_task_metadata: None,
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
    let provider_duration = ends_at - starts_at;
    let duration_seconds = if provider_duration.subsec_nanos() == 0 {
        Some(
            provider_duration
                .num_seconds()
                .try_into()
                .map_err(|_| NormalizationError::Rejected("event_duration_invalid"))?,
        )
    } else {
        // Google accepts RFC 3339 fractional seconds while the canonical duration estimate uses
        // whole seconds. Preserve the exact event bounds and omit a lossy estimate.
        None
    };
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
            google_task_metadata: None,
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
        duration_kind: Some(if duration_seconds.is_some() {
            DurationKind::Exact
        } else {
            DurationKind::Unknown
        }),
        duration_seconds,
        duration_min_seconds: duration_seconds,
        duration_max_seconds: duration_seconds,
        duration_source: duration_seconds.map(|_| DurationSource::Imported),
        // For events these legacy fields are the exact interval boundary,
        // never a task deadline.
        deadline_kind: Some(DeadlineKind::None),
        deadline_date: None,
        deadline_at: Some(ends_at),
        deadline_strength: None,
        deadline_soft_weight: None,
        earliest_start_at: Some(starts_at),
        recurrence: None,
        flexible_constraints: constraints,
        has_own_effort: Some(false),
        split_policy: SplitPolicy::Indivisible,
        importance: 0,
        urgency: 0,
        parent_id: None,
        sibling_order: 0,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
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
        google_task_metadata: None,
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

#[allow(clippy::too_many_lines)] // Validates and projects one complete provider task envelope.
fn normalize_task(
    collection: &GoogleSyncCollection,
    task: GoogleTask,
) -> Result<RemoteItemChange, NormalizationError> {
    validate_remote_id(&task.id).map_err(|_| NormalizationError::Rejected("invalid_remote_id"))?;
    let remote_hash = payload_hash(&task)?;
    let remote_projection_hash = task_projection_hash(remote_hash, collection)?;
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
            google_task_metadata: None,
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
    // Google Tasks documents `due` as carrying only a calendar date even
    // though the wire representation is an RFC 3339 timestamp. Preserve that
    // intent instead of turning provider midnight into an exact latest finish
    // at the start of the due day.
    let due_date = task
        .due
        .as_deref()
        .map(parse_timestamp)
        .transpose()?
        .map(|due| due.date_naive());
    let provider_completed_at = parse_optional_timestamp(task.completed.as_deref())?;
    let completed = task.status.as_deref() == Some("completed") || provider_completed_at.is_some();
    // Provider identity and projection evidence live only in the sync mapping layer. Keeping the
    // canonical scheduling object empty lets an imported Inbox task move into ordinary planning
    // without carrying provider-only metadata through the public item contract.
    let remote_id = task.id.clone();
    let google_task_metadata = GoogleTaskProviderMetadata {
        hidden: task.hidden,
        position: bounded_optional(task.position.as_deref(), 1000)?,
        completed,
        completed_at: provider_completed_at,
        title_truncated,
        notes_truncated,
        legacy_marker_stripped,
    };
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
        duration_kind: Some(DurationKind::Unknown),
        duration_seconds: None,
        duration_min_seconds: None,
        duration_max_seconds: None,
        duration_source: None,
        deadline_kind: Some(if due_date.is_some() {
            DeadlineKind::Date
        } else {
            DeadlineKind::None
        }),
        deadline_date: due_date,
        deadline_at: None,
        deadline_strength: due_date.map(|_| DeadlineStrength::Hard),
        deadline_soft_weight: None,
        earliest_start_at: None,
        recurrence: None,
        flexible_constraints: json!({}),
        has_own_effort: Some(false),
        split_policy: SplitPolicy::Indivisible,
        importance: 0,
        urgency: 0,
        parent_id: None,
        sibling_order: 0,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
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
        google_task_metadata: Some(google_task_metadata),
        item: Some(item),
    })
}

fn validate_normalized_item(item: &NewItem) -> Result<(), NormalizationError> {
    crate::items::Item::new(item.clone(), Utc::now())
        .map(|_| ())
        .map_err(|_| NormalizationError::Rejected("canonical_item_invalid"))
}

#[allow(clippy::too_many_arguments)] // Every provider-visible schedule field is explicit.
fn prepare_schedule_calendar_event(
    collection: &GoogleSyncCollection,
    cipher: &SecretCipher,
    scope: OAuthScope,
    slot_id: Uuid,
    incarnation: u32,
    title: &str,
    is_sensitive: bool,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    timezone_name: &str,
) -> Result<GoogleEvent, GoogleSyncServiceError> {
    if slot_id.is_nil()
        || incarnation == 0
        || ends_at <= starts_at
        || timezone_name.trim().is_empty()
        || timezone_name.len() > 100
        || timezone_name.parse::<Tz>().is_err()
    {
        return Err(GoogleSyncServiceError::InvalidRequest);
    }
    let summary = if is_sensitive {
        "Busy".to_owned()
    } else {
        bounded_title(title, "Scheduled task").0
    };
    let event_id =
        deterministic_schedule_calendar_event_id(cipher, scope, collection, slot_id, incarnation)?;
    let mut private = BTreeMap::new();
    private.insert(
        "dayweaveScheduleOwnershipProof".to_owned(),
        schedule_calendar_ownership_proof(
            cipher,
            scope,
            collection.account_id,
            collection.id,
            slot_id,
            incarnation,
            &event_id,
        )?,
    );
    let additional_properties = BTreeMap::from([(
        "reminders".to_owned(),
        json!({
            "useDefault": false,
            "overrides": [],
        }),
    )]);
    Ok(GoogleEvent {
        id: event_id,
        etag: None,
        status: Some("confirmed".to_owned()),
        summary: Some(summary),
        description: None,
        location: None,
        start: Some(EventDateTime {
            date: None,
            date_time: Some(starts_at.to_rfc3339_opts(SecondsFormat::Micros, true)),
            time_zone: Some(timezone_name.to_owned()),
        }),
        end: Some(EventDateTime {
            date: None,
            date_time: Some(ends_at.to_rfc3339_opts(SecondsFormat::Micros, true)),
            time_zone: Some(timezone_name.to_owned()),
        }),
        recurring_event_id: None,
        original_start_time: None,
        recurrence: Vec::new(),
        transparency: Some("opaque".to_owned()),
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
        additional_properties,
    })
}

#[allow(clippy::too_many_lines)] // Keeps every reviewed schedule-diff invariant in one auditable projection.
fn build_schedule_publication_changes(
    source: &SchedulePublicationSource,
    cipher: &SecretCipher,
    scope: OAuthScope,
    now: DateTime<Utc>,
) -> Result<Vec<PreparedSchedulePublicationChange>, GoogleSyncServiceError> {
    if source.workspace_id != scope.workspace_id
        || source.user_id != scope.user_id
        || source.account_id != source.collection.account_id
        || source.schedule_revision_id.is_nil()
        || source.schedule_revision_number == 0
        || source.horizon_end <= source.horizon_start
        || source.collection.kind != GoogleCollectionKind::Calendar
        || !source.collection.selected
        || source.collection.provider_deleted
        || source.collection.sync_role != GoogleSyncRole::Writable
        || source.blocks.len() > MAX_CALENDAR_PROJECTION_ITEMS
        || source.mappings.len() > MAX_CALENDAR_PROJECTION_ITEMS
    {
        return Err(GoogleSyncServiceError::InvalidRequest);
    }

    let mut mappings = BTreeMap::new();
    for mapping in &source.mappings {
        if mapping.mapping_id.is_nil()
            || mapping.slot_id.is_nil()
            || mapping.item_id.is_nil()
            || mapping.source_block_id.is_nil()
            || mapping.incarnation == 0
            || mapping.remote_resource_id.trim().is_empty()
            || mapping.remote_etag.trim().is_empty()
            || mapping.last_ends_at <= mapping.last_starts_at
            || schedule_publication_slot_id(
                scope.workspace_id,
                mapping.item_id,
                mapping.occurrence_id,
                mapping.session_index,
            ) != mapping.slot_id
        {
            return Err(GoogleSyncServiceError::Internal);
        }
        let expected_remote_id = deterministic_schedule_calendar_event_id(
            cipher,
            scope,
            &source.collection,
            mapping.slot_id,
            mapping.incarnation,
        )?;
        if mapping.remote_resource_id != expected_remote_id {
            return Err(GoogleSyncServiceError::ProviderIdentityUnresolved);
        }
        if mappings.insert(mapping.slot_id, mapping.clone()).is_some() {
            return Err(GoogleSyncServiceError::Internal);
        }
    }

    let mut blocks = source.blocks.iter().collect::<Vec<_>>();
    blocks.sort_by_key(|block| (block.item_id, block.occurrence_id, block.session_index));
    let mut seen_sources = HashSet::new();
    let mut seen_slots = HashSet::new();
    for block in &blocks {
        if block.source_block_id.is_nil()
            || block.item_id.is_nil()
            || block.incarnation == 0
            || block.ends_at <= block.starts_at
            || block.starts_at < source.horizon_start
            || block.ends_at > source.horizon_end
            || !seen_sources.insert(block.source_block_id)
        {
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
        let slot_id = schedule_publication_slot_id(
            scope.workspace_id,
            block.item_id,
            block.occurrence_id,
            block.session_index,
        );
        if !seen_slots.insert(slot_id) {
            return Err(GoogleSyncServiceError::InvalidRequest);
        }
    }
    let future_stale_count = mappings
        .values()
        .filter(|mapping| !seen_slots.contains(&mapping.slot_id) && mapping.last_ends_at > now)
        .count();
    if blocks
        .len()
        .checked_add(future_stale_count)
        .is_none_or(|change_count| change_count > MAX_CALENDAR_PROJECTION_ITEMS)
    {
        return Err(GoogleSyncServiceError::ProviderLimitExceeded);
    }

    let mut changes = Vec::with_capacity(blocks.len().saturating_add(mappings.len()));
    for block in blocks {
        let slot_id = schedule_publication_slot_id(
            scope.workspace_id,
            block.item_id,
            block.occurrence_id,
            block.session_index,
        );
        let mapping = mappings.remove(&slot_id);
        if mapping.as_ref().is_some_and(|mapping| {
            mapping.item_id != block.item_id
                || mapping.occurrence_id != block.occurrence_id
                || mapping.session_index != block.session_index
                || mapping.incarnation != block.incarnation
        }) {
            return Err(GoogleSyncServiceError::Internal);
        }
        let event = prepare_schedule_calendar_event(
            &source.collection,
            cipher,
            scope,
            slot_id,
            block.incarnation,
            &block.title,
            block.is_sensitive,
            block.starts_at,
            block.ends_at,
            &source.timezone_name,
        )?;
        let summary = event
            .summary
            .clone()
            .ok_or(GoogleSyncServiceError::Internal)?;
        let desired_payload_hash = schedule_desired_payload_hash(
            slot_id,
            block.incarnation,
            &summary,
            block.starts_at,
            block.ends_at,
            &source.timezone_name,
        )?;
        let operation = match mapping.as_ref() {
            None => ScheduleGooglePublicationOperation::Create,
            Some(mapping)
                if mapping.last_ends_at <= now
                    && mapping.source_block_id == block.source_block_id
                    && mapping.last_starts_at == block.starts_at
                    && mapping.last_ends_at == block.ends_at =>
            {
                // Once a generated block has elapsed, the provider event is
                // immutable Calendar history. Canonical edits may still
                // change its current title/privacy/timezone-derived payload,
                // but an exact historical source instance is never rewritten.
                ScheduleGooglePublicationOperation::Noop
            }
            Some(mapping) if mapping.desired_payload_hash == desired_payload_hash => {
                ScheduleGooglePublicationOperation::Noop
            }
            Some(_) => ScheduleGooglePublicationOperation::Update,
        };
        let remote_resource_id = mapping
            .as_ref()
            .map(|mapping| mapping.remote_resource_id.clone());
        let expected_etag = mapping.as_ref().map(|mapping| mapping.remote_etag.clone());
        let ordinal = u32::try_from(changes.len())
            .map_err(|_| GoogleSyncServiceError::ProviderLimitExceeded)?;
        let mut change = PreparedSchedulePublicationChange {
            ordinal,
            slot_id,
            source_block_id: Some(block.source_block_id),
            item_id: block.item_id,
            occurrence_id: block.occurrence_id,
            session_index: block.session_index,
            incarnation: block.incarnation,
            operation,
            mapping_id: mapping.as_ref().map(|mapping| mapping.mapping_id),
            remote_resource_id,
            expected_etag,
            desired_payload_hash,
            payload: serde_json::to_value(event).map_err(|_| GoogleSyncServiceError::Internal)?,
            review_summary: json!({
                "summary": summary,
                "starts_at": block.starts_at,
                "ends_at": block.ends_at,
            }),
            starts_at: block.starts_at,
            ends_at: block.ends_at,
            intent_hash: [0; 32],
        };
        change.intent_hash = schedule_publication_intent_hash(source, &change)
            .map_err(|_| GoogleSyncServiceError::Internal)?;
        changes.push(change);
    }

    let mut stale_mappings = mappings.into_values().collect::<Vec<_>>();
    stale_mappings.sort_by_key(|mapping| {
        (
            mapping.item_id,
            mapping.occurrence_id,
            mapping.session_index,
        )
    });
    for mapping in stale_mappings {
        // Historical schedule events remain useful Calendar history and are
        // never deleted automatically. Only future capacity is reconciled.
        if mapping.last_ends_at <= now {
            continue;
        }
        let ordinal = u32::try_from(changes.len())
            .map_err(|_| GoogleSyncServiceError::ProviderLimitExceeded)?;
        let mut change = PreparedSchedulePublicationChange {
            ordinal,
            slot_id: mapping.slot_id,
            source_block_id: None,
            item_id: mapping.item_id,
            occurrence_id: mapping.occurrence_id,
            session_index: mapping.session_index,
            incarnation: mapping.incarnation,
            operation: ScheduleGooglePublicationOperation::Delete,
            mapping_id: Some(mapping.mapping_id),
            remote_resource_id: Some(mapping.remote_resource_id),
            expected_etag: Some(mapping.remote_etag),
            desired_payload_hash: mapping.desired_payload_hash,
            payload: json!({}),
            review_summary: json!({
                "summary": "Previously published DayWeave block",
                "starts_at": mapping.last_starts_at,
                "ends_at": mapping.last_ends_at,
            }),
            starts_at: mapping.last_starts_at,
            ends_at: mapping.last_ends_at,
            intent_hash: [0; 32],
        };
        change.intent_hash = schedule_publication_intent_hash(source, &change)
            .map_err(|_| GoogleSyncServiceError::Internal)?;
        changes.push(change);
    }
    Ok(changes)
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
    let due = match (
        item.deadline_kind,
        item.deadline_date,
        item.deadline_strength,
    ) {
        (DeadlineKind::None, None, None) => None,
        (DeadlineKind::Date, Some(date), Some(DeadlineStrength::Hard)) => Some(
            DateTime::<Utc>::from_naive_utc_and_offset(
                date.and_hms_opt(0, 0, 0)
                    .ok_or(GoogleSyncServiceError::InvalidOutboundItem)?,
                Utc,
            )
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        ),
        // Google Tasks cannot represent a time-of-day or DayWeave's soft
        // deadline weight. Reject rather than silently weaken or shift intent.
        _ => return Err(GoogleSyncServiceError::InvalidOutboundItem),
    };
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
        due,
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

fn schedule_publication_event_result(
    event: &GoogleEvent,
    dispatch_nonce: Uuid,
    observation_source: SchedulePublicationObservationSource,
) -> Result<SchedulePublicationResult, GoogleSyncServiceError> {
    validate_remote_id(&event.id).map_err(|_| GoogleError::InvalidResponse)?;
    let remote_etag = bounded_optional(event.etag.as_deref(), 1000)
        .map_err(|_| GoogleError::InvalidResponse)?
        .ok_or(GoogleError::InvalidResponse)?;
    Ok(SchedulePublicationResult {
        remote_resource_id: event.id.clone(),
        remote_etag: Some(remote_etag),
        remote_updated_at: event
            .updated
            .as_deref()
            .map(parse_timestamp)
            .transpose()
            .map_err(|_| GoogleError::InvalidResponse)?,
        payload_hash: payload_hash(event).map_err(|_| GoogleError::InvalidResponse)?,
        dispatch_nonce,
        observation_source,
    })
}

#[allow(clippy::too_many_arguments)] // Every provider identity fence is explicit.
fn schedule_create_conflict_result(
    found: &GoogleEvent,
    intended: &GoogleEvent,
    slot_id: Uuid,
    incarnation: u32,
    account_id: Uuid,
    collection_id: Uuid,
    cipher: &SecretCipher,
    scope: OAuthScope,
    dispatch_nonce: Uuid,
) -> Result<SchedulePublicationResult, GoogleSyncServiceError> {
    if found.id != intended.id
        || !schedule_calendar_event_owned_by(
            found,
            slot_id,
            incarnation,
            account_id,
            collection_id,
            cipher,
            scope,
        )
        || !schedule_calendar_event_matches_intent(found, intended)
    {
        return Err(GoogleError::PreconditionFailed.into());
    }
    schedule_publication_event_result(
        found,
        dispatch_nonce,
        SchedulePublicationObservationSource::ReconciliationRead,
    )
}

fn schedule_publication_absent_result(
    remote_resource_id: &str,
    dispatch_nonce: Uuid,
    observation_source: SchedulePublicationObservationSource,
) -> SchedulePublicationResult {
    SchedulePublicationResult {
        remote_resource_id: remote_resource_id.to_owned(),
        remote_etag: None,
        remote_updated_at: None,
        payload_hash: Sha256::digest(b"deleted").into(),
        dispatch_nonce,
        observation_source,
    }
}

fn schedule_delete_already_absent(error: &GoogleSyncServiceError) -> bool {
    matches!(
        error,
        GoogleSyncServiceError::Google(
            GoogleError::SyncTokenExpired | GoogleError::Api { status: 410 }
        )
    )
}

fn schedule_calendar_write_access_is_current(
    calendars: &[DiscoveredCollection],
    remote_collection_id: &str,
) -> bool {
    calendars.iter().any(|calendar| {
        calendar.kind == GoogleCollectionKind::Calendar
            && calendar.remote_id == remote_collection_id
            && !calendar.provider_deleted
            && matches!(
                calendar.provider_access_role.as_deref(),
                Some("owner" | "writer")
            )
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

fn schedule_calendar_event_matches_intent(found: &GoogleEvent, intended: &GoogleEvent) -> bool {
    let mut found = found.clone();
    let mut intended = intended.clone();
    normalize_schedule_calendar_semantics(&mut found)
        && normalize_schedule_calendar_semantics(&mut intended)
        && found == intended
}

fn normalize_schedule_calendar_semantics(event: &mut GoogleEvent) -> bool {
    normalize_calendar_provider_metadata(event);
    for bound in [&mut event.start, &mut event.end] {
        let Some(bound) = bound.as_mut() else {
            return false;
        };
        if bound.date.is_some() {
            return false;
        }
        let Some(date_time) = bound.date_time.as_deref() else {
            return false;
        };
        let Ok(parsed) = DateTime::parse_from_rfc3339(date_time) else {
            return false;
        };
        bound.date_time = Some(
            parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Micros, true),
        );
    }

    let Some(reminders) = event.additional_properties.get_mut("reminders") else {
        return false;
    };
    let Some(reminders_object) = reminders.as_object() else {
        return false;
    };
    if reminders_object.get("useDefault") != Some(&Value::Bool(false))
        || reminders_object
            .get("overrides")
            .is_some_and(|overrides| overrides.as_array().is_none_or(|values| !values.is_empty()))
        || reminders_object
            .keys()
            .any(|key| !matches!(key.as_str(), "useDefault" | "overrides"))
    {
        return false;
    }
    *reminders = json!({ "useDefault": false, "overrides": [] });

    // Google commonly materializes documented defaults in response bodies.
    // Remove only exact defaults that cannot alter a private attendee-free
    // block; non-default or unknown fields remain comparison-significant.
    for (field, default_value) in [
        ("endTimeUnspecified", Value::Bool(false)),
        ("attendeesOmitted", Value::Bool(false)),
        ("anyoneCanAddSelf", Value::Bool(false)),
        ("guestsCanInviteOthers", Value::Bool(true)),
        ("guestsCanModify", Value::Bool(false)),
        ("guestsCanSeeOtherGuests", Value::Bool(true)),
        ("privateCopy", Value::Bool(false)),
        ("locked", Value::Bool(false)),
    ] {
        if event.additional_properties.get(field) == Some(&default_value) {
            event.additional_properties.remove(field);
        }
    }
    true
}

#[allow(clippy::too_many_arguments)] // Every response identity and ownership fence is explicit.
fn validate_schedule_write_response(
    result: &GoogleEvent,
    intended: &GoogleEvent,
    expected_remote_id: &str,
    slot_id: Uuid,
    incarnation: u32,
    account_id: Uuid,
    collection_id: Uuid,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<(), GoogleSyncServiceError> {
    if result.id != expected_remote_id {
        return Err(GoogleSyncServiceError::ProviderIdentityUnresolved);
    }
    if !schedule_calendar_event_owned_by(
        result,
        slot_id,
        incarnation,
        account_id,
        collection_id,
        cipher,
        scope,
    ) || !schedule_calendar_event_matches_intent(result, intended)
    {
        return Err(GoogleError::InvalidResponse.into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScheduleUpdateRecoveryAction {
    Adopt,
    Retry,
}

#[allow(clippy::too_many_arguments)] // Every ownership and conditional-write fence is explicit.
fn schedule_update_recovery_action(
    found: &GoogleEvent,
    intended: &GoogleEvent,
    expected_etag: &str,
    slot_id: Uuid,
    incarnation: u32,
    account_id: Uuid,
    collection_id: Uuid,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<ScheduleUpdateRecoveryAction, GoogleSyncServiceError> {
    if !schedule_calendar_event_owned_by(
        found,
        slot_id,
        incarnation,
        account_id,
        collection_id,
        cipher,
        scope,
    ) {
        return Err(GoogleError::PreconditionFailed.into());
    }
    let found_etag = found
        .etag
        .as_deref()
        .filter(|etag| valid_opaque(etag, 1000))
        .ok_or(GoogleError::InvalidResponse)?;
    if schedule_calendar_event_matches_intent(found, intended) {
        return Ok(ScheduleUpdateRecoveryAction::Adopt);
    }
    if found_etag == expected_etag {
        Ok(ScheduleUpdateRecoveryAction::Retry)
    } else {
        Err(GoogleError::PreconditionFailed.into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScheduleDeleteRecoveryAction {
    Absent,
    Retry,
}

#[allow(clippy::too_many_arguments)] // Every deletion identity and ownership fence is explicit.
fn schedule_delete_recovery_action(
    found: &GoogleEvent,
    expected_remote_id: &str,
    expected_etag: &str,
    slot_id: Uuid,
    incarnation: u32,
    account_id: Uuid,
    collection_id: Uuid,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<ScheduleDeleteRecoveryAction, GoogleSyncServiceError> {
    if found.id != expected_remote_id {
        return Err(GoogleSyncServiceError::ProviderIdentityUnresolved);
    }
    // Google events.get deliberately returns cancelled tombstones, and a
    // deleted non-recurring event may contain only its ID. For our exact
    // deterministic mapping this is authoritative absence even when the
    // ownership marker and old ETag have already been stripped.
    if found.status.as_deref() == Some("cancelled") {
        return Ok(ScheduleDeleteRecoveryAction::Absent);
    }
    if !schedule_calendar_event_owned_by(
        found,
        slot_id,
        incarnation,
        account_id,
        collection_id,
        cipher,
        scope,
    ) {
        return Err(GoogleError::PreconditionFailed.into());
    }
    let found_etag = found
        .etag
        .as_deref()
        .filter(|etag| valid_opaque(etag, 1000))
        .ok_or(GoogleError::InvalidResponse)?;
    if found_etag == expected_etag {
        Ok(ScheduleDeleteRecoveryAction::Retry)
    } else {
        Err(GoogleError::PreconditionFailed.into())
    }
}

fn deterministic_schedule_calendar_event_id(
    cipher: &SecretCipher,
    scope: OAuthScope,
    collection: &GoogleSyncCollection,
    slot_id: Uuid,
    incarnation: u32,
) -> Result<String, GoogleSyncServiceError> {
    let context = serde_json::to_vec(&(
        scope.workspace_id,
        scope.user_id,
        collection.account_id,
        collection.id,
        &collection.remote_collection_id,
        slot_id,
        incarnation,
    ))
    .map_err(|_| GoogleSyncServiceError::Internal)?;
    let (version, digest) =
        cipher.identity_digest(b"dayweave.google.schedule-event-id.v1", &context)?;
    Ok(format!("s{version:x}{}", encode_hex(&digest)))
}

fn schedule_calendar_ownership_proof(
    cipher: &SecretCipher,
    scope: OAuthScope,
    account_id: Uuid,
    collection_id: Uuid,
    slot_id: Uuid,
    incarnation: u32,
    event_id: &str,
) -> Result<String, GoogleSyncServiceError> {
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&(1_u8, slot_id, incarnation))
            .map_err(|_| GoogleSyncServiceError::Internal)?,
    );
    let marker = cipher.seal(
        &plaintext,
        &schedule_calendar_marker_aad(scope, account_id, collection_id, event_id),
    )?;
    Ok(format!(
        "dwsm1.v{}.{}",
        marker.key_version,
        URL_SAFE_NO_PAD.encode(&marker.ciphertext)
    ))
}

fn schedule_calendar_marker_for_target(
    event: &GoogleEvent,
    account_id: Uuid,
    collection_id: Uuid,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> Result<Option<(Uuid, u32)>, NormalizationError> {
    let Some(properties) = event.extended_properties.as_ref() else {
        return Ok(None);
    };
    let Some(proof) = properties.private.get("dayweaveScheduleOwnershipProof") else {
        return Ok(None);
    };
    let remainder = proof
        .strip_prefix("dwsm1.v")
        .ok_or(NormalizationError::Rejected(
            "dayweave_schedule_marker_invalid",
        ))?;
    let (version, encoded) = remainder
        .split_once('.')
        .ok_or(NormalizationError::Rejected(
            "dayweave_schedule_marker_invalid",
        ))?;
    let version = version
        .parse::<u32>()
        .map_err(|_| NormalizationError::Rejected("dayweave_schedule_marker_invalid"))?;
    let envelope = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| NormalizationError::Rejected("dayweave_schedule_marker_invalid"))?;
    let mut plaintext = cipher
        .open(
            version,
            &envelope,
            &schedule_calendar_marker_aad(scope, account_id, collection_id, &event.id),
        )
        .map_err(|_| NormalizationError::Rejected("dayweave_schedule_marker_invalid"))?;
    let parsed = serde_json::from_slice::<(u8, Uuid, u32)>(&plaintext)
        .ok()
        .filter(|(schema, slot_id, incarnation)| {
            *schema == 1 && !slot_id.is_nil() && *incarnation > 0
        })
        .map(|(_, slot_id, incarnation)| (slot_id, incarnation))
        .ok_or(NormalizationError::Rejected(
            "dayweave_schedule_marker_invalid",
        ));
    plaintext.zeroize();
    parsed.map(Some)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScheduleCalendarEventDisposition {
    Generated,
    Rejected(&'static str),
    External,
}

fn classify_schedule_calendar_event(
    event: &GoogleEvent,
    known_schedule_remote_ids: &BTreeSet<String>,
    account_id: Uuid,
    collection_id: Uuid,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> ScheduleCalendarEventDisposition {
    match schedule_calendar_marker_for_target(event, account_id, collection_id, cipher, scope) {
        Ok(Some(_)) => ScheduleCalendarEventDisposition::Generated,
        Err(NormalizationError::Rejected(reason)) => {
            ScheduleCalendarEventDisposition::Rejected(reason)
        }
        Ok(None) if known_schedule_remote_ids.contains(&event.id) => {
            if event.status.as_deref() == Some("cancelled") {
                ScheduleCalendarEventDisposition::Generated
            } else {
                ScheduleCalendarEventDisposition::Rejected("dayweave_schedule_marker_missing")
            }
        }
        Ok(None) => ScheduleCalendarEventDisposition::External,
    }
}

fn schedule_calendar_event_owned_by(
    event: &GoogleEvent,
    slot_id: Uuid,
    incarnation: u32,
    account_id: Uuid,
    collection_id: Uuid,
    cipher: &SecretCipher,
    scope: OAuthScope,
) -> bool {
    matches!(
        schedule_calendar_marker_for_target(
            event,
            account_id,
            collection_id,
            cipher,
            scope,
        ),
        Ok(Some((parsed_slot, parsed_incarnation)))
            if parsed_slot == slot_id && parsed_incarnation == incarnation
    )
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

fn schedule_calendar_marker_aad(
    scope: OAuthScope,
    account_id: Uuid,
    collection_id: Uuid,
    event_id: &str,
) -> Vec<u8> {
    serde_json::to_vec(&(
        "dayweave.google.schedule-calendar-marker.v1",
        scope.workspace_id,
        scope.user_id,
        account_id,
        collection_id,
        event_id,
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

/// Stable logical identity for one generated work session. The scheduler's
/// source block UUID includes its start time and therefore cannot identify a
/// moved Google event. This value intentionally excludes placement bounds.
#[must_use]
pub(crate) fn schedule_publication_slot_id(
    workspace_id: Uuid,
    item_id: Uuid,
    occurrence_id: Option<Uuid>,
    session_index: u16,
) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"dayweave.google.schedule-slot.v1\0");
    digest.update(workspace_id.as_bytes());
    digest.update(item_id.as_bytes());
    match occurrence_id {
        Some(occurrence_id) => {
            digest.update([1]);
            digest.update(occurrence_id.as_bytes());
        }
        None => digest.update([0; 17]),
    }
    digest.update(session_index.to_be_bytes());
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // Mark this deterministic UUID as RFC 4122 variant / version 5. The
    // digest construction above is the canonical namespace operation.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

pub(crate) fn schedule_desired_payload_hash(
    slot_id: Uuid,
    incarnation: u32,
    summary: &str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    timezone_name: &str,
) -> Result<[u8; 32], GoogleSyncServiceError> {
    let bytes = serde_json::to_vec(&(
        "dayweave.google.schedule-desired-payload.v1",
        slot_id,
        incarnation,
        summary,
        starts_at,
        ends_at,
        timezone_name,
        "confirmed",
        "opaque",
        "private",
        "default",
        "no_attendees",
        "no_notifications",
    ))
    .map_err(|_| GoogleSyncServiceError::Internal)?;
    Ok(Sha256::digest(bytes).into())
}

#[allow(clippy::too_many_lines)] // The complete external-effect binding is intentionally explicit.
pub(crate) fn schedule_publication_intent_hash(
    source: &SchedulePublicationSource,
    change: &PreparedSchedulePublicationChange,
) -> Result<[u8; 32], serde_json::Error> {
    let bytes = serde_json::to_vec(&(
        "dayweave.google.schedule-publication-intent.v2",
        (source.workspace_id, source.user_id, source.account_id),
        (
            source.collection.id,
            source.collection.revision,
            &source.collection.remote_collection_id,
            GOOGLE_CALENDAR_SCOPE,
        ),
        (
            source.schedule_revision_id,
            source.schedule_revision_number,
            source.schedule_publication_hash,
            &source.timezone_name,
            source.horizon_start,
            source.horizon_end,
        ),
        (
            change.ordinal,
            change.slot_id,
            change.source_block_id,
            change.item_id,
            change.occurrence_id,
            change.session_index,
            change.incarnation,
            change.operation,
        ),
        (
            change.mapping_id,
            change.remote_resource_id.as_deref(),
            change.expected_etag.as_deref(),
            change.desired_payload_hash,
            change.starts_at,
            change.ends_at,
        ),
        (&change.payload, &change.review_summary),
    ))?;
    Ok(Sha256::digest(bytes).into())
}

pub(crate) fn schedule_publication_desired_set_hash(
    source: &SchedulePublicationSource,
    changes: &[PreparedSchedulePublicationChange],
) -> Result<[u8; 32], serde_json::Error> {
    let desired = changes
        .iter()
        .filter(|change| change.source_block_id.is_some())
        .map(|change| {
            (
                change.ordinal,
                change.slot_id,
                change.source_block_id,
                change.item_id,
                change.occurrence_id,
                change.session_index,
                change.incarnation,
                change.desired_payload_hash,
                change.starts_at,
                change.ends_at,
            )
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&(
        "dayweave.google.schedule-desired-set.v1",
        source.workspace_id,
        source.user_id,
        source.schedule_revision_id,
        source.schedule_revision_number,
        source.schedule_publication_hash,
        &source.timezone_name,
        source.horizon_start,
        source.horizon_end,
        desired,
    ))?;
    Ok(Sha256::digest(bytes).into())
}

pub(crate) fn schedule_publication_preview_hash(
    preview_id: Uuid,
    source: &SchedulePublicationSource,
    changes: &[PreparedSchedulePublicationChange],
    expires_at: DateTime<Utc>,
) -> Result<[u8; 32], serde_json::Error> {
    let desired_set_hash = schedule_publication_desired_set_hash(source, changes)?;
    let intent_hashes = changes
        .iter()
        .map(|change| {
            (
                change.ordinal,
                change.intent_hash,
                change.operation,
                change.slot_id,
                change.source_block_id,
                change.remote_resource_id.as_deref(),
                change.expected_etag.as_deref(),
                &change.review_summary,
                change.starts_at,
                change.ends_at,
            )
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&(
        "dayweave.google.schedule-publication-preview.v2",
        preview_id,
        source.workspace_id,
        source.user_id,
        source.account_id,
        source.collection.id,
        source.collection.revision,
        &source.collection.remote_collection_id,
        source.schedule_revision_id,
        source.schedule_revision_number,
        source.schedule_publication_hash,
        desired_set_hash,
        intent_hashes,
        expires_at,
    ))?;
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
    capability_hash_with_prefix(value, APPROVAL_TOKEN_PREFIX)
}

fn schedule_approval_capability_hash(value: &str) -> Result<[u8; 32], GoogleSyncServiceError> {
    capability_hash_with_prefix(value, SCHEDULE_APPROVAL_TOKEN_PREFIX)
}

fn capability_hash_with_prefix(
    value: &str,
    prefix: &str,
) -> Result<[u8; 32], GoogleSyncServiceError> {
    let payload = value
        .strip_prefix(prefix)
        .ok_or(GoogleSyncServiceError::InvalidApprovalCapability)?;
    let decoded = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| GoogleSyncServiceError::InvalidApprovalCapability)?,
    );
    let canonical = Zeroizing::new(URL_SAFE_NO_PAD.encode(decoded.as_slice()));
    if decoded.len() != APPROVAL_TOKEN_RANDOM_BYTES
        || canonical.as_str() != payload
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

fn task_projection_hash(
    remote_hash: [u8; 32],
    collection: &GoogleSyncCollection,
) -> Result<[u8; 32], NormalizationError> {
    payload_hash(&(
        // Tasks previously shared the generic v2 domain with calendar-series
        // imports. v3 forces unchanged due values through date normalization
        // without churning unrelated Calendar mappings.
        "dayweave.google.task_canonical_projection.v3",
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

fn schedule_ambiguous_response_code(error: &GoogleSyncServiceError) -> Option<&'static str> {
    match error {
        GoogleSyncServiceError::ProviderIdentityUnresolved => Some("provider_identity_unresolved"),
        GoogleSyncServiceError::Google(GoogleError::ResponseTooLarge) => {
            Some("provider_response_too_large")
        }
        _ => None,
    }
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
    #[error("Google schedule-block publication is disabled by deployment policy")]
    SchedulePublicationDisabled,
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
            Self::Repository(GoogleSyncRepositoryError::ItemExecutionActive) => {
                short_backoff("item_execution_active")
            }
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

fn short_backoff(code: &'static str) -> FailureDisposition {
    FailureDisposition {
        kind: SyncFailureKind::Backoff,
        code,
        delay: Duration::seconds(30),
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
    use std::{
        collections::HashMap,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        },
    };

    const REFRESH_ACCOUNT_ACTIVE: u8 = 0;
    const REFRESH_ACCOUNT_PAUSED: u8 = 1;
    const REFRESH_ACCOUNT_DISCONNECTED: u8 = 2;

    #[test]
    fn ambiguous_schedule_write_responses_stay_reconcilable() {
        assert_eq!(
            schedule_ambiguous_response_code(&GoogleSyncServiceError::ProviderIdentityUnresolved),
            Some("provider_identity_unresolved")
        );
        assert_eq!(
            schedule_ambiguous_response_code(&GoogleSyncServiceError::Google(
                GoogleError::ResponseTooLarge,
            )),
            Some("provider_response_too_large")
        );
        assert_eq!(
            schedule_ambiguous_response_code(&GoogleSyncServiceError::Google(
                GoogleError::PreconditionFailed,
            )),
            None
        );
    }

    struct RefreshReplayLifecycle {
        account_state: AtomicU8,
        accepted: Mutex<HashMap<Uuid, GoogleSyncRefreshAccepted>>,
        next_generation: AtomicU64,
        gate_calls: AtomicUsize,
        accept_calls: AtomicUsize,
    }

    impl RefreshReplayLifecycle {
        fn new() -> Self {
            Self {
                account_state: AtomicU8::new(REFRESH_ACCOUNT_ACTIVE),
                accepted: Mutex::new(HashMap::new()),
                next_generation: AtomicU64::new(0),
                gate_calls: AtomicUsize::new(0),
                accept_calls: AtomicUsize::new(0),
            }
        }

        async fn request(
            &self,
            account_id: Uuid,
            request_id: Uuid,
        ) -> Result<GoogleSyncRefreshAccepted, GoogleSyncServiceError> {
            replay_or_accept_refresh(
                || async {
                    Ok(self
                        .accepted
                        .lock()
                        .expect("accepted refresh lock")
                        .get(&request_id)
                        .cloned())
                },
                || async {
                    self.gate_calls.fetch_add(1, Ordering::SeqCst);
                    if self.account_state.load(Ordering::SeqCst) == REFRESH_ACCOUNT_ACTIVE {
                        Ok(())
                    } else {
                        Err(GoogleSyncServiceError::OAuth(
                            GoogleOAuthServiceError::Repository(
                                crate::google_oauth::GoogleOAuthRepositoryError::AccountStateConflict,
                            ),
                        ))
                    }
                },
                || async {
                    self.accept_calls.fetch_add(1, Ordering::SeqCst);
                    let accepted = GoogleSyncRefreshAccepted {
                        account_id,
                        request_id,
                        refresh_generation: self.next_generation.fetch_add(1, Ordering::SeqCst) + 1,
                        requested_at: "2026-09-02T08:00:00Z"
                            .parse()
                            .expect("fixed refresh time"),
                    };
                    self.accepted
                        .lock()
                        .expect("accepted refresh lock")
                        .insert(request_id, accepted.clone());
                    Ok(accepted)
                },
            )
            .await
        }
    }

    #[tokio::test]
    async fn accepted_refresh_replays_before_paused_and_disconnected_account_gates() {
        let lifecycle = RefreshReplayLifecycle::new();
        let account_id = Uuid::from_u128(0xacc0_0001);
        let accepted_request_id = Uuid::from_u128(0xfeed_0001);

        // Model an accepted request whose HTTP response never reached the
        // client. The durable acceptance is the only recovery proof.
        let original = lifecycle
            .request(account_id, accepted_request_id)
            .await
            .expect("initial refresh accepted");
        assert_eq!(original.refresh_generation, 1);
        assert_eq!(lifecycle.gate_calls.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.accept_calls.load(Ordering::SeqCst), 1);

        lifecycle
            .account_state
            .store(REFRESH_ACCOUNT_PAUSED, Ordering::SeqCst);
        let paused_replay = lifecycle
            .request(account_id, accepted_request_id)
            .await
            .expect("exact accepted request replays while paused");
        assert_eq!(paused_replay, original);
        assert_eq!(lifecycle.gate_calls.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.accept_calls.load(Ordering::SeqCst), 1);
        let paused_new = lifecycle
            .request(account_id, Uuid::from_u128(0xfeed_0002))
            .await;
        assert!(matches!(
            paused_new,
            Err(GoogleSyncServiceError::OAuth(
                GoogleOAuthServiceError::Repository(
                    crate::google_oauth::GoogleOAuthRepositoryError::AccountStateConflict
                )
            ))
        ));
        assert_eq!(lifecycle.accept_calls.load(Ordering::SeqCst), 1);

        lifecycle
            .account_state
            .store(REFRESH_ACCOUNT_DISCONNECTED, Ordering::SeqCst);
        let disconnected_replay = lifecycle
            .request(account_id, accepted_request_id)
            .await
            .expect("exact accepted request replays while disconnected");
        assert_eq!(disconnected_replay, original);
        assert_eq!(lifecycle.gate_calls.load(Ordering::SeqCst), 2);
        assert_eq!(lifecycle.accept_calls.load(Ordering::SeqCst), 1);
        let disconnected_new = lifecycle
            .request(account_id, Uuid::from_u128(0xfeed_0003))
            .await;
        assert!(matches!(
            disconnected_new,
            Err(GoogleSyncServiceError::OAuth(
                GoogleOAuthServiceError::Repository(
                    crate::google_oauth::GoogleOAuthRepositoryError::AccountStateConflict
                )
            ))
        ));
        assert_eq!(lifecycle.accept_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn active_execution_close_conflict_is_a_short_backoff() {
        let failure =
            GoogleSyncServiceError::Repository(GoogleSyncRepositoryError::ItemExecutionActive)
                .failure();
        assert_eq!(failure.kind, SyncFailureKind::Backoff);
        assert_eq!(failure.code, "item_execution_active");
        assert_eq!(failure.delay, Duration::seconds(30));
    }

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

    fn schedule_source(
        blocks: Vec<SchedulePublicationBlock>,
        mappings: Vec<ScheduleBlockMapping>,
    ) -> SchedulePublicationSource {
        let scope = test_oauth_scope();
        SchedulePublicationSource {
            workspace_id: scope.workspace_id,
            user_id: scope.user_id,
            account_id: Uuid::from_u128(11),
            collection: collection(GoogleSyncRole::Writable, true),
            schedule_revision_id: Uuid::from_u128(0x700),
            schedule_revision_number: 7,
            schedule_publication_hash: [0x77; 32],
            timezone_name: "UTC".to_owned(),
            horizon_start: parse_timestamp("2026-09-03T00:00:00Z").expect("start"),
            horizon_end: parse_timestamp("2026-09-05T00:00:00Z").expect("end"),
            blocks,
            mappings,
        }
    }

    #[test]
    fn schedule_slot_identity_is_stable_across_moves_and_separates_sessions() {
        let workspace = Uuid::from_u128(1);
        let item = Uuid::from_u128(2);
        let occurrence = Uuid::from_u128(3);
        let first = schedule_publication_slot_id(workspace, item, Some(occurrence), 0);
        assert_eq!(
            first,
            schedule_publication_slot_id(workspace, item, Some(occurrence), 0)
        );
        assert_ne!(
            first,
            schedule_publication_slot_id(workspace, item, Some(occurrence), 1)
        );
        assert_ne!(
            first,
            schedule_publication_slot_id(workspace, item, None, 0)
        );
        assert_ne!(
            first,
            schedule_publication_slot_id(Uuid::from_u128(4), item, Some(occurrence), 0)
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One fixture verifies privacy, redaction, and target binding together.
    fn schedule_event_is_private_redacted_and_target_bound() {
        let collection = collection(GoogleSyncRole::Writable, true);
        let scope = test_oauth_scope();
        let cipher = test_marker_cipher();
        let slot_id = schedule_publication_slot_id(
            scope.workspace_id,
            Uuid::from_u128(0x901),
            Some(Uuid::from_u128(0x902)),
            2,
        );
        let starts_at = parse_timestamp("2026-09-03T09:00:00Z").expect("start");
        let ends_at = parse_timestamp("2026-09-03T10:00:00Z").expect("end");
        let first = prepare_schedule_calendar_event(
            &collection,
            &cipher,
            scope,
            slot_id,
            1,
            "Never expose this sensitive title",
            true,
            starts_at,
            ends_at,
            "UTC",
        )
        .expect("event");
        let second = prepare_schedule_calendar_event(
            &collection,
            &cipher,
            scope,
            slot_id,
            1,
            "Never expose this sensitive title",
            true,
            starts_at,
            ends_at,
            "UTC",
        )
        .expect("second event");
        assert_eq!(first.id, second.id);
        assert_eq!(first.summary.as_deref(), Some("Busy"));
        assert!(first.description.is_none());
        assert!(first.location.is_none());
        assert!(first.attendees.is_empty());
        assert!(first.attachments.is_empty());
        assert!(first.conference_data.is_none());
        assert_eq!(
            first.additional_properties.get("reminders"),
            Some(&json!({ "useDefault": false, "overrides": [] }))
        );
        assert_eq!(first.visibility.as_deref(), Some("private"));
        assert_eq!(first.transparency.as_deref(), Some("opaque"));
        let encoded = serde_json::to_string(&first).expect("serialize");
        assert!(!encoded.contains("Never expose"));
        assert!(!encoded.contains(&slot_id.to_string()));
        assert!(encoded.contains("\"useDefault\":false"));
        assert_eq!(
            schedule_calendar_marker_for_target(
                &first,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            ),
            Ok(Some((slot_id, 1)))
        );
        assert!(matches!(
            schedule_calendar_marker_for_target(
                &first,
                collection.account_id,
                Uuid::from_u128(0x999),
                &cipher,
                scope,
            ),
            Err(NormalizationError::Rejected(
                "dayweave_schedule_marker_invalid"
            ))
        ));

        let mut replayed = first.clone();
        replayed.id.push('0');
        assert!(matches!(
            schedule_calendar_marker_for_target(
                &replayed,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            ),
            Err(NormalizationError::Rejected(
                "dayweave_schedule_marker_invalid"
            ))
        ));

        let paris = prepare_schedule_calendar_event(
            &collection,
            &cipher,
            scope,
            slot_id,
            1,
            "Canonical response",
            false,
            starts_at,
            ends_at,
            "Europe/Paris",
        )
        .expect("Paris event");
        let mut provider_response = paris.clone();
        provider_response.start.as_mut().expect("start").date_time =
            Some("2026-09-03T11:00:00+02:00".to_owned());
        provider_response.end.as_mut().expect("end").date_time =
            Some("2026-09-03T12:00:00.000+02:00".to_owned());
        provider_response
            .additional_properties
            .insert("reminders".to_owned(), json!({ "useDefault": false }));
        for (field, value) in [
            ("endTimeUnspecified", json!(false)),
            ("attendeesOmitted", json!(false)),
            ("guestsCanInviteOthers", json!(true)),
            ("guestsCanModify", json!(false)),
            ("guestsCanSeeOtherGuests", json!(true)),
        ] {
            provider_response
                .additional_properties
                .insert(field.to_owned(), value);
        }
        provider_response.additional_properties.insert(
            "creator".to_owned(),
            json!({ "email": "calendar-owner@example.test", "self": true }),
        );
        assert!(schedule_calendar_event_matches_intent(
            &provider_response,
            &paris
        ));
        provider_response.visibility = Some("public".to_owned());
        assert!(!schedule_calendar_event_matches_intent(
            &provider_response,
            &paris
        ));
    }

    #[test]
    fn schedule_create_duplicate_id_adopts_only_the_exact_owned_intent() {
        let calendar = collection(GoogleSyncRole::Writable, true);
        let scope = test_oauth_scope();
        let cipher = test_marker_cipher();
        let slot_id =
            schedule_publication_slot_id(scope.workspace_id, Uuid::from_u128(0x908), None, 0);
        let mut intended = prepare_schedule_calendar_event(
            &calendar,
            &cipher,
            scope,
            slot_id,
            1,
            "Deterministic create",
            false,
            parse_timestamp("2026-09-03T09:00:00Z").expect("start"),
            parse_timestamp("2026-09-03T10:00:00Z").expect("end"),
            "UTC",
        )
        .expect("intent");
        let nonce = Uuid::from_u128(0x909);
        intended.etag = Some("duplicate-etag".to_owned());

        let adopted = schedule_create_conflict_result(
            &intended,
            &intended,
            slot_id,
            1,
            calendar.account_id,
            calendar.id,
            &cipher,
            scope,
            nonce,
        )
        .expect("exact existing deterministic ID is adopted");
        assert_eq!(adopted.remote_resource_id.as_str(), intended.id.as_str());
        assert_eq!(adopted.remote_etag.as_deref(), Some("duplicate-etag"));
        assert_eq!(adopted.dispatch_nonce, nonce);
        assert_eq!(
            adopted.observation_source,
            SchedulePublicationObservationSource::ReconciliationRead
        );

        let mut missing_etag = intended.clone();
        missing_etag.etag = None;
        assert!(matches!(
            schedule_create_conflict_result(
                &missing_etag,
                &intended,
                slot_id,
                1,
                calendar.account_id,
                calendar.id,
                &cipher,
                scope,
                nonce,
            ),
            Err(GoogleSyncServiceError::Google(GoogleError::InvalidResponse))
        ));

        let mut wrong_intent = intended.clone();
        wrong_intent.summary = Some("A different event at that ID".to_owned());
        assert!(matches!(
            schedule_create_conflict_result(
                &wrong_intent,
                &intended,
                slot_id,
                1,
                calendar.account_id,
                calendar.id,
                &cipher,
                scope,
                nonce,
            ),
            Err(GoogleSyncServiceError::Google(
                GoogleError::PreconditionFailed
            ))
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Exercises every generated-event authentication rejection branch.
    fn schedule_projection_ignores_authenticated_generated_events() {
        let collection = collection(GoogleSyncRole::Writable, true);
        let scope = test_oauth_scope();
        let cipher = test_marker_cipher();
        let starts_at = parse_timestamp("2026-09-03T09:00:00Z").expect("start");
        let ends_at = parse_timestamp("2026-09-03T10:00:00Z").expect("end");
        let slot_id =
            schedule_publication_slot_id(scope.workspace_id, Uuid::from_u128(0x910), None, 0);
        let event = prepare_schedule_calendar_event(
            &collection,
            &cipher,
            scope,
            slot_id,
            1,
            "Generated block",
            false,
            starts_at,
            ends_at,
            "UTC",
        )
        .expect("event");
        let mut changes = Vec::new();
        let mut rejected = Vec::new();
        let mut bytes = 0;
        normalize_calendar_projection_events(
            &collection,
            CalendarProjectionWindow {
                start: starts_at - Duration::hours(1),
                end: ends_at + Duration::hours(1),
            },
            "UTC",
            vec![event.clone()],
            &cipher,
            scope,
            &BTreeSet::new(),
            &mut changes,
            &mut rejected,
            &mut bytes,
        )
        .expect("projection");
        assert!(changes.is_empty());
        assert!(rejected.is_empty());
        assert_eq!(bytes, 0);

        let known_ids = BTreeSet::from([event.id.clone()]);
        let mut marker_stripped = event.clone();
        marker_stripped.extended_properties = None;
        let cancelled: GoogleEvent = serde_json::from_value(json!({
            "id": event.id,
            "status": "cancelled"
        }))
        .expect("ID-only cancelled event");
        let mut malformed_marker = event;
        malformed_marker
            .extended_properties
            .as_mut()
            .expect("marker")
            .private
            .get_mut("dayweaveScheduleOwnershipProof")
            .expect("proof")
            .push('x');
        assert_eq!(
            classify_schedule_calendar_event(
                &marker_stripped,
                &known_ids,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            ),
            ScheduleCalendarEventDisposition::Rejected("dayweave_schedule_marker_missing")
        );
        assert_eq!(
            classify_schedule_calendar_event(
                &cancelled,
                &known_ids,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            ),
            ScheduleCalendarEventDisposition::Generated
        );
        assert_eq!(
            classify_schedule_calendar_event(
                &malformed_marker,
                &known_ids,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            ),
            ScheduleCalendarEventDisposition::Rejected("dayweave_schedule_marker_invalid")
        );

        let mut changes = Vec::new();
        let mut rejected = Vec::new();
        let mut bytes = 0;
        normalize_calendar_projection_events(
            &collection,
            CalendarProjectionWindow {
                start: starts_at - Duration::hours(1),
                end: ends_at + Duration::hours(1),
            },
            "UTC",
            vec![marker_stripped, cancelled],
            &cipher,
            scope,
            &known_ids,
            &mut changes,
            &mut rejected,
            &mut bytes,
        )
        .expect("known generated projections");
        assert!(changes.is_empty());
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].reason, "dayweave_schedule_marker_missing");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One table verifies all pre-acceptance update recovery outcomes.
    fn schedule_update_recovery_retries_crash_or_transport_before_acceptance() {
        let collection = collection(GoogleSyncRole::Writable, true);
        let scope = test_oauth_scope();
        let cipher = test_marker_cipher();
        let slot_id =
            schedule_publication_slot_id(scope.workspace_id, Uuid::from_u128(0x920), None, 0);
        let starts_at = parse_timestamp("2026-09-03T09:00:00Z").expect("start");
        let ends_at = parse_timestamp("2026-09-03T10:00:00Z").expect("end");
        let mut intended = prepare_schedule_calendar_event(
            &collection,
            &cipher,
            scope,
            slot_id,
            1,
            "New placement",
            false,
            starts_at,
            ends_at,
            "UTC",
        )
        .expect("intended event");
        intended.etag = Some("etag-before-update".to_owned());

        let mut untouched = prepare_schedule_calendar_event(
            &collection,
            &cipher,
            scope,
            slot_id,
            1,
            "Old placement",
            false,
            starts_at - Duration::hours(1),
            ends_at - Duration::hours(1),
            "UTC",
        )
        .expect("old event");
        untouched.etag = Some("etag-before-update".to_owned());
        assert_eq!(
            schedule_update_recovery_action(
                &untouched,
                &intended,
                "etag-before-update",
                slot_id,
                1,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            )
            .expect("unchanged old ETag is safe to retry"),
            ScheduleUpdateRecoveryAction::Retry
        );

        let mut accepted = intended.clone();
        accepted.etag = Some("etag-after-update".to_owned());
        assert_eq!(
            schedule_update_recovery_action(
                &accepted,
                &intended,
                "etag-before-update",
                slot_id,
                1,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            )
            .expect("accepted intent is adopted"),
            ScheduleUpdateRecoveryAction::Adopt
        );

        let mut missing_etag = accepted.clone();
        missing_etag.etag = None;
        assert!(matches!(
            schedule_update_recovery_action(
                &missing_etag,
                &intended,
                "etag-before-update",
                slot_id,
                1,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            ),
            Err(GoogleSyncServiceError::Google(GoogleError::InvalidResponse))
        ));

        untouched.etag = Some("etag-from-concurrent-edit".to_owned());
        assert!(matches!(
            schedule_update_recovery_action(
                &untouched,
                &intended,
                "etag-before-update",
                slot_id,
                1,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            ),
            Err(GoogleSyncServiceError::Google(
                GoogleError::PreconditionFailed
            ))
        ));

        let mut malformed_success = accepted.clone();
        malformed_success.etag = None;
        assert!(matches!(
            schedule_publication_event_result(
                &malformed_success,
                Uuid::from_u128(0x921),
                SchedulePublicationObservationSource::ProviderResponse,
            ),
            Err(GoogleSyncServiceError::Google(GoogleError::InvalidResponse))
        ));
        malformed_success.etag = Some("etag-after-update".to_owned());
        malformed_success.updated = Some("not-a-timestamp".to_owned());
        assert!(matches!(
            schedule_publication_event_result(
                &malformed_success,
                Uuid::from_u128(0x922),
                SchedulePublicationObservationSource::ProviderResponse,
            ),
            Err(GoogleSyncServiceError::Google(GoogleError::InvalidResponse))
        ));

        let mut semantically_wrong_success = accepted.clone();
        semantically_wrong_success.summary = Some("Provider returned another state".to_owned());
        assert!(matches!(
            validate_schedule_write_response(
                &semantically_wrong_success,
                &intended,
                &intended.id,
                slot_id,
                1,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            ),
            Err(GoogleSyncServiceError::Google(GoogleError::InvalidResponse))
        ));
    }

    #[test]
    fn schedule_delete_recovery_accepts_google_cancelled_tombstones() {
        assert!(schedule_delete_already_absent(
            &GoogleError::Api { status: 410 }.into()
        ));
        assert!(!schedule_delete_already_absent(
            &GoogleError::Api { status: 404 }.into()
        ));
        assert!(schedule_delete_already_absent(
            &GoogleError::SyncTokenExpired.into()
        ));
        let collection = collection(GoogleSyncRole::Writable, true);
        let scope = test_oauth_scope();
        let cipher = test_marker_cipher();
        let slot_id =
            schedule_publication_slot_id(scope.workspace_id, Uuid::from_u128(0x930), None, 0);
        let active = prepare_schedule_calendar_event(
            &collection,
            &cipher,
            scope,
            slot_id,
            1,
            "Published block",
            false,
            parse_timestamp("2026-09-03T09:00:00Z").expect("start"),
            parse_timestamp("2026-09-03T10:00:00Z").expect("end"),
            "UTC",
        )
        .expect("active event");
        let remote_id = active.id.clone();
        let tombstone: GoogleEvent = serde_json::from_value(json!({
            "id": remote_id,
            "status": "cancelled"
        }))
        .expect("ID-only Google tombstone");
        assert_eq!(
            schedule_delete_recovery_action(
                &tombstone,
                &active.id,
                "etag-before-delete",
                slot_id,
                1,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            )
            .expect("cancelled tombstone proves absence"),
            ScheduleDeleteRecoveryAction::Absent
        );

        let mut retained_cancelled = active.clone();
        retained_cancelled.status = Some("cancelled".to_owned());
        retained_cancelled.etag = Some("etag-after-delete".to_owned());
        assert_eq!(
            schedule_delete_recovery_action(
                &retained_cancelled,
                &active.id,
                "etag-before-delete",
                slot_id,
                1,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            )
            .expect("retained cancelled event proves absence"),
            ScheduleDeleteRecoveryAction::Absent
        );

        let mut unchanged = active;
        unchanged.etag = Some("etag-before-delete".to_owned());
        assert_eq!(
            schedule_delete_recovery_action(
                &unchanged,
                &unchanged.id,
                "etag-before-delete",
                slot_id,
                1,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            )
            .expect("unchanged owned event is safe to delete again"),
            ScheduleDeleteRecoveryAction::Retry
        );

        let mut missing_etag = unchanged;
        missing_etag.etag = None;
        assert!(matches!(
            schedule_delete_recovery_action(
                &missing_etag,
                &missing_etag.id,
                "etag-before-delete",
                slot_id,
                1,
                collection.account_id,
                collection.id,
                &cipher,
                scope,
            ),
            Err(GoogleSyncServiceError::Google(GoogleError::InvalidResponse))
        ));
    }

    #[test]
    fn schedule_delete_404_requires_current_writable_calendar_proof() {
        let mut target = DiscoveredCollection {
            kind: GoogleCollectionKind::Calendar,
            remote_id: "target@example.test".to_owned(),
            display_name: "Publication calendar".to_owned(),
            provider_access_role: Some("owner".to_owned()),
            provider_primary: false,
            provider_selected: true,
            provider_hidden: false,
            provider_deleted: false,
        };
        assert!(schedule_calendar_write_access_is_current(
            std::slice::from_ref(&target),
            "target@example.test"
        ));

        target.provider_access_role = Some("reader".to_owned());
        assert!(!schedule_calendar_write_access_is_current(
            std::slice::from_ref(&target),
            "target@example.test"
        ));
        target.provider_access_role = Some("writer".to_owned());
        target.provider_deleted = true;
        assert!(!schedule_calendar_write_access_is_current(
            std::slice::from_ref(&target),
            "target@example.test"
        ));
        target.provider_deleted = false;
        assert!(!schedule_calendar_write_access_is_current(
            std::slice::from_ref(&target),
            "another@example.test"
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One diff fixture makes create/no-op/update/delete identity relationships explicit.
    fn schedule_diff_emits_create_noop_update_and_future_delete() {
        let scope = test_oauth_scope();
        let cipher = test_marker_cipher();
        let calendar = collection(GoogleSyncRole::Writable, true);
        let starts_at = parse_timestamp("2026-09-03T09:00:00Z").expect("start");
        let ends_at = parse_timestamp("2026-09-03T10:00:00Z").expect("end");
        let block = |item: u128, source: u128, title: &str| SchedulePublicationBlock {
            source_block_id: Uuid::from_u128(source),
            item_id: Uuid::from_u128(item),
            occurrence_id: None,
            session_index: 0,
            incarnation: 1,
            title: title.to_owned(),
            starts_at,
            ends_at,
            is_sensitive: false,
        };
        let noop_block = block(0xa2, 0xb2, "Unchanged");
        let noop_slot =
            schedule_publication_slot_id(scope.workspace_id, noop_block.item_id, None, 0);
        let noop_hash =
            schedule_desired_payload_hash(noop_slot, 1, "Unchanged", starts_at, ends_at, "UTC")
                .expect("desired hash");
        let update_block = block(0xa3, 0xb3, "Changed");
        let update_slot =
            schedule_publication_slot_id(scope.workspace_id, update_block.item_id, None, 0);
        let future_delete_slot =
            schedule_publication_slot_id(scope.workspace_id, Uuid::from_u128(0xa4), None, 0);
        let past_delete_slot =
            schedule_publication_slot_id(scope.workspace_id, Uuid::from_u128(0xa5), None, 0);
        let remote_id = |slot_id| {
            deterministic_schedule_calendar_event_id(&cipher, scope, &calendar, slot_id, 1)
                .expect("provider id")
        };
        let mapping = |mapping_id: u128,
                       slot_id: Uuid,
                       item_id: u128,
                       source_id: u128,
                       desired_payload_hash: [u8; 32],
                       map_start: DateTime<Utc>,
                       map_end: DateTime<Utc>| ScheduleBlockMapping {
            mapping_id: Uuid::from_u128(mapping_id),
            slot_id,
            item_id: Uuid::from_u128(item_id),
            occurrence_id: None,
            session_index: 0,
            incarnation: 1,
            source_block_id: Uuid::from_u128(source_id),
            remote_resource_id: remote_id(slot_id),
            remote_etag: format!("etag-{mapping_id:x}"),
            desired_payload_hash,
            last_starts_at: map_start,
            last_ends_at: map_end,
        };
        let source = schedule_source(
            vec![block(0xa1, 0xb1, "Create"), noop_block, update_block],
            vec![
                mapping(0xc2, noop_slot, 0xa2, 0xd2, noop_hash, starts_at, ends_at),
                mapping(0xc3, update_slot, 0xa3, 0xd3, [0; 32], starts_at, ends_at),
                mapping(
                    0xc4,
                    future_delete_slot,
                    0xa4,
                    0xd4,
                    [4; 32],
                    starts_at,
                    ends_at,
                ),
                mapping(
                    0xc5,
                    past_delete_slot,
                    0xa5,
                    0xd5,
                    [5; 32],
                    starts_at - Duration::days(2),
                    ends_at - Duration::days(2),
                ),
            ],
        );
        let changes = build_schedule_publication_changes(
            &source,
            &cipher,
            scope,
            starts_at - Duration::hours(1),
        )
        .expect("diff");
        assert_eq!(
            changes
                .iter()
                .map(|change| change.operation)
                .collect::<Vec<_>>(),
            vec![
                ScheduleGooglePublicationOperation::Create,
                ScheduleGooglePublicationOperation::Noop,
                ScheduleGooglePublicationOperation::Update,
                ScheduleGooglePublicationOperation::Delete,
            ]
        );
        assert_eq!(changes[3].source_block_id, None);
        assert_eq!(
            changes[3].review_summary["summary"],
            "Previously published DayWeave block"
        );
        assert!(changes.iter().all(|change| change.intent_hash != [0; 32]));

        let expires_at = starts_at + Duration::minutes(10);
        let original_preview_hash = schedule_publication_preview_hash(
            Uuid::from_u128(0x6000),
            &source,
            &changes,
            expires_at,
        )
        .expect("preview hash");
        let mut tampered_review = changes.clone();
        let original_intent = tampered_review[0].intent_hash;
        tampered_review[0].review_summary["summary"] = json!("Different reviewed text");
        let recalculated_intent =
            schedule_publication_intent_hash(&source, &tampered_review[0]).expect("intent hash");
        assert_ne!(original_intent, recalculated_intent);
        assert_ne!(
            original_preview_hash,
            schedule_publication_preview_hash(
                Uuid::from_u128(0x6000),
                &source,
                &tampered_review,
                expires_at,
            )
            .expect("tampered preview hash")
        );

        let mut forged_stale_mapping = source.clone();
        forged_stale_mapping
            .mappings
            .iter_mut()
            .find(|mapping| mapping.slot_id == future_delete_slot)
            .expect("future stale mapping")
            .remote_resource_id = "arbitrary-user-event".to_owned();
        assert!(matches!(
            build_schedule_publication_changes(
                &forged_stale_mapping,
                &cipher,
                scope,
                starts_at - Duration::hours(1),
            ),
            Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
        ));
    }

    #[test]
    fn schedule_diff_never_rewrites_an_exact_elapsed_history_instance() {
        let scope = test_oauth_scope();
        let cipher = test_marker_cipher();
        let calendar = collection(GoogleSyncRole::Writable, true);
        let starts_at = parse_timestamp("2026-09-03T09:00:00Z").expect("start");
        let ends_at = parse_timestamp("2026-09-03T10:00:00Z").expect("end");
        let item_id = Uuid::from_u128(0xe1);
        let source_block_id = Uuid::from_u128(0xe2);
        let slot_id = schedule_publication_slot_id(scope.workspace_id, item_id, None, 0);
        let block = SchedulePublicationBlock {
            source_block_id,
            item_id,
            occurrence_id: None,
            session_index: 0,
            incarnation: 1,
            title: "Edited after completion".to_owned(),
            starts_at,
            ends_at,
            is_sensitive: true,
        };
        let mapping = ScheduleBlockMapping {
            mapping_id: Uuid::from_u128(0xe3),
            slot_id,
            item_id,
            occurrence_id: None,
            session_index: 0,
            incarnation: 1,
            source_block_id,
            remote_resource_id: deterministic_schedule_calendar_event_id(
                &cipher, scope, &calendar, slot_id, 1,
            )
            .expect("remote ID"),
            remote_etag: "elapsed-etag".to_owned(),
            // Deliberately differs from the edited sensitive title and the
            // publication timezone below. Neither may rewrite elapsed history.
            desired_payload_hash: [0; 32],
            last_starts_at: starts_at,
            last_ends_at: ends_at,
        };
        let mut source = schedule_source(vec![block], vec![mapping]);
        source.timezone_name = "Europe/Paris".to_owned();

        let changes = build_schedule_publication_changes(
            &source,
            &cipher,
            scope,
            ends_at + Duration::seconds(1),
        )
        .expect("elapsed history diff");

        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].operation,
            ScheduleGooglePublicationOperation::Noop
        );
        assert_eq!(changes[0].mapping_id, Some(Uuid::from_u128(0xe3)));
    }

    #[test]
    fn schedule_diff_caps_combined_blocks_and_future_deletes() {
        let scope = test_oauth_scope();
        let cipher = test_marker_cipher();
        let calendar = collection(GoogleSyncRole::Writable, true);
        let starts_at = parse_timestamp("2026-09-03T09:00:00Z").expect("start");
        let ends_at = parse_timestamp("2026-09-03T10:00:00Z").expect("end");
        let blocks = (0..MAX_CALENDAR_PROJECTION_ITEMS)
            .map(|index| SchedulePublicationBlock {
                source_block_id: Uuid::from_u128(0x20_0000 + index as u128),
                item_id: Uuid::from_u128(0x10_0000 + index as u128),
                occurrence_id: None,
                session_index: 0,
                incarnation: 1,
                title: "Bounded block".to_owned(),
                starts_at,
                ends_at,
                is_sensitive: false,
            })
            .collect::<Vec<_>>();
        let stale_item_id = Uuid::from_u128(0x30_0000);
        let stale_slot = schedule_publication_slot_id(scope.workspace_id, stale_item_id, None, 0);
        let stale_mapping = ScheduleBlockMapping {
            mapping_id: Uuid::from_u128(0x40_0000),
            slot_id: stale_slot,
            item_id: stale_item_id,
            occurrence_id: None,
            session_index: 0,
            incarnation: 1,
            source_block_id: Uuid::from_u128(0x50_0000),
            remote_resource_id: deterministic_schedule_calendar_event_id(
                &cipher, scope, &calendar, stale_slot, 1,
            )
            .expect("remote ID"),
            remote_etag: "etag-stale".to_owned(),
            desired_payload_hash: [3; 32],
            last_starts_at: starts_at,
            last_ends_at: ends_at,
        };
        assert!(matches!(
            build_schedule_publication_changes(
                &schedule_source(blocks, vec![stale_mapping]),
                &cipher,
                scope,
                starts_at - Duration::hours(1),
            ),
            Err(GoogleSyncServiceError::ProviderLimitExceeded)
        ));
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
                duration_kind: None,
                duration_seconds: Some(duration_seconds),
                duration_min_seconds: None,
                duration_max_seconds: None,
                duration_source: None,
                deadline_kind: None,
                deadline_date: None,
                deadline_at: Some(ends_at),
                deadline_strength: None,
                deadline_soft_weight: None,
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
                has_own_effort: None,
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
                blocked_reason_kind: None,
                blocked_by_item_id: None,
                blocked_reason: None,
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
        assert_eq!(
            item.duration_seconds, None,
            "fractional provider intervals retain exact bounds without a lossy whole-second estimate"
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
        assert_eq!(
            change.google_task_metadata,
            Some(GoogleTaskProviderMetadata {
                hidden: true,
                position: Some("0001".to_owned()),
                completed: true,
                completed_at: Some(
                    "2026-08-29T12:00:00Z"
                        .parse()
                        .expect("completion timestamp")
                ),
                title_truncated: false,
                notes_truncated: false,
                legacy_marker_stripped: false,
            })
        );
        let item = change.item.expect("upsert");
        assert!(!item.is_sensitive);
        assert_eq!(item.status, ItemStatus::Completed);
        assert_eq!(change.remote_parent_id.as_deref(), Some("parent-1"));
        assert_eq!(item.flexible_constraints, json!({}));
        assert_eq!(item.deadline_kind, Some(DeadlineKind::Date));
        assert_eq!(
            item.deadline_date,
            Some(NaiveDate::from_ymd_opt(2026, 8, 30).expect("valid due date"))
        );
        assert!(item.deadline_at.is_none());
        assert_eq!(item.deadline_strength, Some(DeadlineStrength::Hard));
    }

    #[test]
    fn google_task_due_round_trips_as_a_hard_date_without_losing_intent() {
        let mut tasks_collection = collection(GoogleSyncRole::Writable, true);
        tasks_collection.kind = GoogleCollectionKind::TaskList;
        let provider_task = GoogleTask {
            id: "task-date".to_owned(),
            etag: Some("etag-date".to_owned()),
            title: "Date only".to_owned(),
            notes: None,
            status: Some("needsAction".to_owned()),
            due: Some("2026-08-30T00:00:00.000Z".to_owned()),
            completed: None,
            updated: Some("2026-08-29T12:00:00Z".to_owned()),
            parent: None,
            position: None,
            links: None,
            deleted: false,
            hidden: false,
        };
        let legacy_v2_hash = projection_hash(
            payload_hash(&provider_task).expect("provider payload hash"),
            &tasks_collection,
        )
        .expect("legacy projection hash");
        let change = normalize_task(&tasks_collection, provider_task).expect("date-only task");
        assert_ne!(
            change.remote_projection_hash, legacy_v2_hash,
            "the task-only projection version must re-normalize unchanged v2 due values"
        );
        let normalized = change.item.expect("upsert");
        let item = crate::items::Item::new(normalized, Utc::now()).expect("canonical task");
        let prepared = prepare_task_outbound(item.clone(), OutboundOperation::Upsert)
            .expect("hard date is representable");
        assert_eq!(prepared.payload["due"], "2026-08-30T00:00:00.000Z");

        let mut soft = item.clone();
        soft.deadline_strength = Some(DeadlineStrength::Soft);
        soft.deadline_soft_weight = Some(1);
        assert!(matches!(
            prepare_task_outbound(soft, OutboundOperation::Upsert),
            Err(GoogleSyncServiceError::InvalidOutboundItem)
        ));

        let mut exact = item;
        exact.deadline_kind = DeadlineKind::DateTime;
        exact.deadline_date = None;
        exact.deadline_at = Some(
            "2026-08-30T12:00:00Z"
                .parse()
                .expect("valid exact timestamp"),
        );
        assert!(matches!(
            prepare_task_outbound(exact, OutboundOperation::Upsert),
            Err(GoogleSyncServiceError::InvalidOutboundItem)
        ));
    }

    #[test]
    fn outbound_calendar_requires_owned_firm_block_and_uses_stable_id() {
        let now = truncate_to_microseconds(Utc::now()).expect("microsecond test clock");
        let input = NewItem {
            id: Uuid::from_u128(44),
            is_sensitive: false,
            kind: ItemKind::Event,
            status: ItemStatus::Scheduled,
            title: "Focus".to_owned(),
            notes: None,
            timezone_name: "UTC".to_owned(),
            duration_kind: None,
            duration_seconds: Some(3600),
            duration_min_seconds: None,
            duration_max_seconds: None,
            duration_source: None,
            deadline_kind: None,
            deadline_date: None,
            deadline_at: Some(now + Duration::hours(1)),
            deadline_strength: None,
            deadline_soft_weight: None,
            earliest_start_at: Some(now),
            recurrence: None,
            flexible_constraints: json!({"dayweave_firm_block": {
                "owned": true,
                "starts_at": now,
                "ends_at": now + Duration::hours(1),
            }}),
            has_own_effort: None,
            split_policy: SplitPolicy::Indivisible,
            importance: 0,
            urgency: 0,
            parent_id: None,
            sibling_order: 0,
            blocked_reason_kind: None,
            blocked_by_item_id: None,
            blocked_reason: None,
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
        let schedule_token = format!(
            "{SCHEDULE_APPROVAL_TOKEN_PREFIX}{}",
            URL_SAFE_NO_PAD.encode([8_u8; 32])
        );
        assert!(schedule_approval_capability_hash(&schedule_token).is_ok());
        assert!(schedule_approval_capability_hash(&token).is_err());
        assert!(approval_capability_hash(&schedule_token).is_err());

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
                duration_kind: None,
                duration_seconds: None,
                duration_min_seconds: None,
                duration_max_seconds: None,
                duration_source: None,
                deadline_kind: None,
                deadline_date: None,
                deadline_at: None,
                deadline_strength: None,
                deadline_soft_weight: None,
                earliest_start_at: None,
                recurrence: None,
                flexible_constraints: json!({}),
                has_own_effort: None,
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
                blocked_reason_kind: None,
                blocked_by_item_id: None,
                blocked_reason: None,
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
        assert!(
            change
                .google_task_metadata
                .as_ref()
                .is_some_and(|metadata| metadata.legacy_marker_stripped)
        );
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
        assert!(
            change
                .google_task_metadata
                .as_ref()
                .is_some_and(|metadata| metadata.legacy_marker_stripped)
        );
        let item = change.item.expect("normalized task");

        assert_eq!(
            item.notes.as_deref(),
            Some("ordinary first line ordinary second line ordinary third line")
        );
        assert_eq!(item.flexible_constraints, json!({}));
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
    fn task_provider_metadata_records_truncation_and_bounds_order_tokens() {
        let mut tasks_collection = collection(GoogleSyncRole::ReadOnly, true);
        tasks_collection.kind = GoogleCollectionKind::TaskList;
        let task = GoogleTask {
            id: "task_id_with_large_display_fields".to_owned(),
            etag: Some("etag-large".to_owned()),
            title: "t".repeat(501),
            notes: Some("n".repeat(100_001)),
            status: Some("needsAction".to_owned()),
            due: None,
            completed: None,
            updated: None,
            parent: None,
            position: Some("p".repeat(1000)),
            links: None,
            deleted: false,
            hidden: false,
        };
        let change = normalize_task(&tasks_collection, task.clone()).expect("bounded task");
        let metadata = change.google_task_metadata.expect("provider metadata");
        assert!(metadata.title_truncated);
        assert!(metadata.notes_truncated);
        assert_eq!(
            metadata.position.as_deref(),
            Some("p".repeat(1000).as_str())
        );
        assert_eq!(change.item.expect("task").flexible_constraints, json!({}));

        let mut invalid = task;
        invalid.position = Some("p".repeat(1001));
        assert!(matches!(
            normalize_task(&tasks_collection, invalid),
            Err(NormalizationError::Rejected("provider_metadata_invalid"))
        ));
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
