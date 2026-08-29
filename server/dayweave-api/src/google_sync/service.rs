use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use dayweave_google::{
    AccessTokenProvider, GoogleClient, GoogleError,
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
    CursorValue, DiscoveredCollection, GoogleCollectionKind, GoogleOutboundAccepted,
    GoogleSyncCollection, GoogleSyncRefreshAccepted, GoogleSyncRepository,
    GoogleSyncRepositoryError, GoogleSyncRole, GoogleSyncStatus, OutboundOperation,
    OutboundRequest, OutboundResult, OutboundWork, PreparedOutbound, RemoteItemChange, SyncClaim,
    SyncCounts, SyncFailureKind,
};

const MAX_PAGES: usize = 100;
const MAX_COLLECTIONS: usize = 10_000;
const MAX_ITEMS_PER_RUN: usize = 100_000;
const MAX_OUTBOUND_PER_RUN: usize = 100;
const TASK_CURSOR_OVERLAP_MINUTES: i64 = 2;
const RUN_LEASE_MINUTES: i64 = 10;
const PERIODIC_SYNC_MINUTES: i64 = 15;
const WORKER_POLL_SECONDS: u64 = 30;

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

    async fn insert_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event: &GoogleEvent,
    ) -> Result<GoogleEvent, GoogleError>;

    async fn update_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event: &GoogleEvent,
    ) -> Result<GoogleEvent, GoogleError>;

    async fn delete_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event_id: &str,
        etag: Option<&str>,
    ) -> Result<(), GoogleError>;

    async fn list_tasks(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        page_token: Option<&str>,
        updated_min: Option<&str>,
    ) -> Result<TaskPage, GoogleError>;

    async fn insert_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        _task: &GoogleTask,
    ) -> Result<GoogleTask, GoogleError>;

    async fn update_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        _task: &GoogleTask,
    ) -> Result<GoogleTask, GoogleError>;

    async fn delete_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        task_id: &str,
        etag: Option<&str>,
    ) -> Result<(), GoogleError>;
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

    async fn insert_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event: &GoogleEvent,
    ) -> Result<GoogleEvent, GoogleError> {
        self.client(account_id)?
            .insert_event(
                calendar_id,
                event,
                &EventWriteApproval::PrivateAppOwned,
                SendUpdates::None,
            )
            .await
    }

    async fn update_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event: &GoogleEvent,
    ) -> Result<GoogleEvent, GoogleError> {
        self.client(account_id)?
            .update_event(
                calendar_id,
                event,
                &EventWriteApproval::PrivateAppOwned,
                SendUpdates::None,
            )
            .await
    }

    async fn delete_event(
        &self,
        account_id: Uuid,
        calendar_id: &str,
        event_id: &str,
        etag: Option<&str>,
    ) -> Result<(), GoogleError> {
        self.client(account_id)?
            .delete_event(
                calendar_id,
                event_id,
                etag,
                &EventWriteApproval::PrivateAppOwned,
                SendUpdates::None,
            )
            .await
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

    async fn insert_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        task: &GoogleTask,
    ) -> Result<GoogleTask, GoogleError> {
        self.client(account_id)?
            .insert_task(task_list_id, task)
            .await
    }

    async fn update_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        task: &GoogleTask,
    ) -> Result<GoogleTask, GoogleError> {
        self.client(account_id)?
            .update_task(task_list_id, task)
            .await
    }

    async fn delete_task(
        &self,
        account_id: Uuid,
        task_list_id: &str,
        task_id: &str,
        etag: Option<&str>,
    ) -> Result<(), GoogleError> {
        self.client(account_id)?
            .delete_task(task_list_id, task_id, etag)
            .await
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
    pub(crate) fn new(
        repository: Arc<dyn GoogleSyncRepository>,
        provider: Arc<dyn GoogleSyncProvider>,
        oauth: Arc<GoogleOAuthService>,
        items: Arc<ItemService>,
        cipher: SecretCipher,
        scope: OAuthScope,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            repository,
            provider,
            oauth,
            items,
            cipher,
            scope,
            clock,
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
                self.clock.now(),
            )
            .await?)
    }

    pub(crate) async fn request_refresh(
        &self,
        account_id: Uuid,
    ) -> Result<GoogleSyncRefreshAccepted, GoogleSyncServiceError> {
        self.oauth.account_for_sync(account_id).await?;
        let now = self.clock.now();
        self.repository.request_refresh(account_id, now).await?;
        Ok(GoogleSyncRefreshAccepted {
            account_id,
            requested_at: now,
        })
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

    pub(crate) async fn enqueue_outbound(
        &self,
        account_id: Uuid,
        request: OutboundRequest,
    ) -> Result<GoogleOutboundAccepted, GoogleSyncServiceError> {
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
                prepare_calendar_outbound(item, request.operation)?
            }
            GoogleCollectionKind::TaskList => {
                if !account.granted_scopes.contains(GOOGLE_TASKS_SCOPE) {
                    return Err(GoogleSyncServiceError::MissingWriteScope);
                }
                prepare_task_outbound(item, request.operation)?
            }
        };
        require_external_publication_approval()?;
        Ok(self
            .repository
            .enqueue_outbound(
                account_id,
                prepared,
                request.collection_id,
                self.clock.now(),
            )
            .await?)
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
        self.process_outbound(claim).await?;
        Ok(counts)
    }

    async fn sync_calendar(
        &self,
        collection: &GoogleSyncCollection,
        started_at: DateTime<Utc>,
        claim: &SyncClaim,
    ) -> Result<SyncCounts, GoogleSyncServiceError> {
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
            let page_timezone = page.time_zone.as_deref().unwrap_or("UTC");
            for event in page.items {
                item_count += 1;
                if item_count.is_multiple_of(100) {
                    self.heartbeat(claim).await?;
                }
                if item_count > MAX_ITEMS_PER_RUN {
                    return Err(GoogleSyncServiceError::ProviderLimitExceeded);
                }
                let remote_id = event.id.clone();
                seen_remote_ids.insert(remote_id.clone());
                match normalize_event(collection, page_timezone, event) {
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
            match self.deliver_outbound(&work, claim).await {
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
                Err(GoogleSyncServiceError::Google(GoogleError::Api { status: 404 }))
                    if work.operation == OutboundOperation::Delete =>
                {
                    self.repository
                        .complete_outbound(
                            &work,
                            OutboundResult {
                                remote_resource_id: work
                                    .remote_resource_id
                                    .clone()
                                    .ok_or(GoogleSyncServiceError::Internal)?,
                                remote_etag: None,
                                remote_updated_at: None,
                                payload_hash: Sha256::digest(b"deleted").into(),
                            },
                            self.clock.now(),
                        )
                        .await?;
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
        claim: &SyncClaim,
    ) -> Result<OutboundResult, GoogleSyncServiceError> {
        // The HTTP enqueue path is fenced too, but durable rows can outlive a
        // deployment (or be restored from backup). Re-check the publication
        // policy immediately before every provider mutation so queued work can
        // never bypass the audited approval boundary.
        require_external_publication_approval()?;
        let account = self.oauth.account_for_sync(work.account_id).await?;
        let has_write_scope = match work.entity_kind.as_str() {
            "calendar_event" => account.granted_scopes.contains(GOOGLE_CALENDAR_SCOPE),
            "task" => account.granted_scopes.contains(GOOGLE_TASKS_SCOPE),
            _ => return Err(GoogleSyncServiceError::OutboundPayloadCorrupt),
        };
        if !has_write_scope {
            return Err(GoogleSyncServiceError::MissingWriteScope);
        }
        match (work.entity_kind.as_str(), work.operation) {
            ("calendar_event", OutboundOperation::Upsert) => {
                let mut event: GoogleEvent = serde_json::from_value(work.payload.clone())
                    .map_err(|_| GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                let result = if let Some(remote_id) = &work.remote_resource_id {
                    event.id.clone_from(remote_id);
                    event.etag.clone_from(&work.expected_etag);
                    self.repository
                        .renew_outbound(work, self.clock.now())
                        .await?;
                    self.provider
                        .update_event(work.account_id, &work.collection_remote_id, &event)
                        .await?
                } else {
                    match self
                        .provider
                        .get_event(work.account_id, &work.collection_remote_id, &event.id)
                        .await
                    {
                        Ok(found) => {
                            if !calendar_event_owned_by(&found, work.item_id) {
                                return Err(GoogleError::PreconditionFailed.into());
                            }
                            if found.etag.is_none() {
                                return Err(GoogleSyncServiceError::ProviderProtocol);
                            }
                            event.etag = found.etag;
                            self.repository
                                .renew_outbound(work, self.clock.now())
                                .await?;
                            self.provider
                                .update_event(work.account_id, &work.collection_remote_id, &event)
                                .await?
                        }
                        Err(GoogleError::Api { status: 404 }) => {
                            self.repository
                                .renew_outbound(work, self.clock.now())
                                .await?;
                            self.provider
                                .insert_event(work.account_id, &work.collection_remote_id, &event)
                                .await?
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                outbound_event_result(&result)
            }
            ("calendar_event", OutboundOperation::Delete) => {
                let remote_id = work
                    .remote_resource_id
                    .as_deref()
                    .ok_or(GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                self.repository
                    .renew_outbound(work, self.clock.now())
                    .await?;
                self.provider
                    .delete_event(
                        work.account_id,
                        &work.collection_remote_id,
                        remote_id,
                        work.expected_etag.as_deref(),
                    )
                    .await?;
                Ok(OutboundResult {
                    remote_resource_id: remote_id.to_owned(),
                    remote_etag: None,
                    remote_updated_at: None,
                    payload_hash: Sha256::digest(b"deleted").into(),
                })
            }
            ("task", OutboundOperation::Upsert) => {
                let mut task: GoogleTask = serde_json::from_value(work.payload.clone())
                    .map_err(|_| GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                let result = if let Some(remote_id) = &work.remote_resource_id {
                    task.id.clone_from(remote_id);
                    task.etag.clone_from(&work.expected_etag);
                    self.repository
                        .renew_outbound(work, self.clock.now())
                        .await?;
                    self.provider
                        .update_task(work.account_id, &work.collection_remote_id, &task)
                        .await?
                } else if let Some(found) = self.find_task_marker(work, &task, claim).await? {
                    if found.etag.is_none() {
                        return Err(GoogleSyncServiceError::ProviderProtocol);
                    }
                    task.id = found.id;
                    task.etag = found.etag;
                    self.repository
                        .renew_outbound(work, self.clock.now())
                        .await?;
                    self.provider
                        .update_task(work.account_id, &work.collection_remote_id, &task)
                        .await?
                } else {
                    guard_new_task_insert(work.attempts)?;
                    self.repository
                        .renew_outbound(work, self.clock.now())
                        .await?;
                    self.provider
                        .insert_task(work.account_id, &work.collection_remote_id, &task)
                        .await?
                };
                outbound_task_result(&result)
            }
            ("task", OutboundOperation::Delete) => {
                let remote_id = work
                    .remote_resource_id
                    .as_deref()
                    .ok_or(GoogleSyncServiceError::OutboundPayloadCorrupt)?;
                self.repository
                    .renew_outbound(work, self.clock.now())
                    .await?;
                self.provider
                    .delete_task(
                        work.account_id,
                        &work.collection_remote_id,
                        remote_id,
                        work.expected_etag.as_deref(),
                    )
                    .await?;
                Ok(OutboundResult {
                    remote_resource_id: remote_id.to_owned(),
                    remote_etag: None,
                    remote_updated_at: None,
                    payload_hash: Sha256::digest(b"deleted").into(),
                })
            }
            _ => Err(GoogleSyncServiceError::OutboundPayloadCorrupt),
        }
    }

    async fn find_task_marker(
        &self,
        work: &OutboundWork,
        _task: &GoogleTask,
        claim: &SyncClaim,
    ) -> Result<Option<GoogleTask>, GoogleSyncServiceError> {
        let marker = task_marker(work.item_id);
        let mut page_token = None;
        let mut seen = HashSet::new();
        let mut found = None;
        for _ in 0..MAX_PAGES {
            self.heartbeat(claim).await?;
            self.repository
                .renew_outbound(work, self.clock.now())
                .await?;
            let page = self
                .provider
                .list_tasks(
                    work.account_id,
                    &work.collection_remote_id,
                    page_token.as_deref(),
                    None,
                )
                .await?;
            for candidate in page.items {
                if !candidate.deleted
                    && task_recovery_marker_matches(
                        candidate.notes.as_deref(),
                        &marker,
                        work.item_id,
                    )?
                {
                    record_task_marker_match(&mut found, candidate)?;
                }
            }
            let Some(next) = page.next_page_token else {
                return Ok(found);
            };
            validate_page_token(&next, &mut seen)?;
            page_token = Some(next);
        }
        Err(GoogleSyncServiceError::ProviderLimitExceeded)
    }
}

fn record_task_marker_match(
    found: &mut Option<GoogleTask>,
    candidate: GoogleTask,
) -> Result<(), GoogleSyncServiceError> {
    if found
        .as_ref()
        .is_some_and(|existing| existing.id != candidate.id)
    {
        return Err(GoogleSyncServiceError::ProviderIdentityUnresolved);
    }
    if found.is_none() {
        *found = Some(candidate);
    }
    Ok(())
}

fn task_recovery_marker_matches(
    notes: Option<&str>,
    marker: &str,
    item_id: Uuid,
) -> Result<bool, GoogleSyncServiceError> {
    let Some(notes) = notes else {
        return Ok(false);
    };
    if !notes.lines().any(|line| line.trim() == marker) {
        return Ok(false);
    }
    match task_dayweave_item_id(Some(notes)) {
        Ok(Some(parsed)) if parsed == item_id => Ok(true),
        Ok(_) | Err(_) => Err(GoogleSyncServiceError::ProviderIdentityUnresolved),
    }
}

#[allow(clippy::too_many_lines)] // The projection intentionally centralizes all event semantics.
fn normalize_event(
    collection: &GoogleSyncCollection,
    page_timezone: &str,
    event: GoogleEvent,
) -> Result<RemoteItemChange, NormalizationError> {
    validate_remote_id(&event.id).map_err(|_| NormalizationError::Rejected("invalid_remote_id"))?;
    let remote_hash = payload_hash(&event)?;
    let remote_projection_hash = projection_hash(remote_hash, collection)?;
    let dayweave_item_id = calendar_dayweave_item_id(&event)?;
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
            remote_projection_hash,
            item: None,
        });
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
    let (ends_at, _, end_all_day) = parse_event_bound(end, &timezone_name)?;
    if ends_at <= starts_at || all_day != end_all_day {
        return Err(NormalizationError::Rejected("event_bounds_invalid"));
    }
    let duration = (ends_at - starts_at)
        .num_seconds()
        .try_into()
        .map_err(|_| NormalizationError::Rejected("event_duration_invalid"))?;
    let event_type = event.event_type.as_deref().unwrap_or("default");
    let busy = event.transparency.as_deref() != Some("transparent")
        && !matches!(event_type, "birthday" | "workingLocation");
    let blocking = matches!(
        collection.sync_role,
        GoogleSyncRole::Blocking | GoogleSyncRole::Writable
    ) && busy;
    let fallback_title = if blocking {
        "Busy"
    } else {
        "Google calendar event"
    };
    let (title, title_truncated) = if collection.visible {
        bounded_title(
            event.summary.as_deref().unwrap_or(fallback_title),
            fallback_title,
        )
    } else {
        (fallback_title.to_owned(), false)
    };
    let (notes, notes_truncated) = if collection.visible {
        bounded_notes(event.description.as_deref())
    } else {
        (None, false)
    };
    let recurrence = (!event.recurrence.is_empty()
        || event.recurring_event_id.is_some()
        || event.original_start_time.is_some())
    .then(|| {
        json!({
            "source": "google_calendar",
            "rules": event.recurrence,
            "series_remote_id": event.recurring_event_id,
            "original_start": event.original_start_time,
        })
    });
    let constraints = json!({
        "google_sync": {
            "account_id": collection.account_id,
            "collection_id": collection.id,
            "remote_id": event.id,
            "starts_at": starts_at,
            "ends_at": ends_at,
            "all_day": all_day,
            "blocking": blocking,
            "visible": collection.visible,
            "event_type": event_type,
            "transparency": event.transparency,
            "self_response": self_response,
            "attendee_count": event.attendees.len(),
            "has_conference": event.conference_data.is_some(),
            "attachment_count": event.attachments.len(),
            "location": collection.visible.then_some(event.location).flatten(),
            "content_truncated": title_truncated || notes_truncated,
            "provider_sequence": event.sequence,
        }
    });
    let item_id = Uuid::new_v4();
    let remote_id = constraints["google_sync"]["remote_id"]
        .as_str()
        .ok_or(NormalizationError::Rejected("invalid_remote_id"))?
        .to_owned();
    let item = NewItem {
        id: item_id,
        kind: ItemKind::Event,
        status: ItemStatus::Scheduled,
        title,
        notes,
        timezone_name,
        duration_seconds: Some(duration),
        deadline_at: Some(ends_at),
        earliest_start_at: Some(starts_at),
        recurrence,
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
        remote_parent_id: event.recurring_event_id,
        remote_etag: bounded_optional(event.etag.as_deref(), 1000)?,
        remote_updated_at,
        remote_payload_hash: remote_hash,
        remote_projection_hash,
        item: Some(item),
    })
}

fn normalize_task(
    collection: &GoogleSyncCollection,
    task: GoogleTask,
) -> Result<RemoteItemChange, NormalizationError> {
    validate_remote_id(&task.id).map_err(|_| NormalizationError::Rejected("invalid_remote_id"))?;
    let remote_hash = payload_hash(&task)?;
    let remote_projection_hash = projection_hash(remote_hash, collection)?;
    let dayweave_item_id = task_dayweave_item_id(task.notes.as_deref())?;
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
            item: None,
        });
    }
    let (title, title_truncated) = if collection.visible {
        bounded_title(&task.title, "Google task")
    } else {
        ("Google task".to_owned(), false)
    };
    let (notes, notes_truncated) = if collection.visible {
        bounded_notes(task.notes.as_deref())
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
        }
    });
    let remote_id = constraints["google_sync"]["remote_id"]
        .as_str()
        .ok_or(NormalizationError::Rejected("invalid_remote_id"))?
        .to_owned();
    let item = NewItem {
        id: Uuid::new_v4(),
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
        item: Some(item),
    })
}

fn validate_normalized_item(item: &NewItem) -> Result<(), NormalizationError> {
    crate::items::Item::new(item.clone(), Utc::now())
        .map(|_| ())
        .map_err(|_| NormalizationError::Rejected("canonical_item_invalid"))
}

fn prepare_calendar_outbound(
    item: crate::items::Item,
    operation: OutboundOperation,
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
    let mut private = BTreeMap::new();
    private.insert("dayweaveItemId".to_owned(), item.id.to_string());
    private.insert("dayweaveOwnership".to_owned(), "firm_block".to_owned());
    let event = GoogleEvent {
        // Calendar event IDs are client-selected so a crash after provider
        // acceptance can be recovered with GET+conditional update, not a
        // duplicate insert.
        id: deterministic_calendar_event_id(item.id),
        etag: None,
        status: Some("confirmed".to_owned()),
        summary: Some(item.title.clone()),
        description: item.notes.clone(),
        location: None,
        start: Some(EventDateTime {
            date: None,
            date_time: Some(starts_at.to_rfc3339()),
            time_zone: Some(item.timezone_name.clone()),
        }),
        end: Some(EventDateTime {
            date: None,
            date_time: Some(ends_at.to_rfc3339()),
            time_zone: Some(item.timezone_name.clone()),
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
    let marker = task_marker(item.id);
    let notes = match item.notes.as_deref() {
        Some(notes) if !notes.is_empty() => Some(format!("{notes}\n\n{marker}")),
        _ => Some(marker),
    };
    let task = GoogleTask {
        id: String::new(),
        etag: None,
        title: item.title.clone(),
        notes,
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

fn outbound_event_result(event: &GoogleEvent) -> Result<OutboundResult, GoogleSyncServiceError> {
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
    })
}

fn outbound_task_result(task: &GoogleTask) -> Result<OutboundResult, GoogleSyncServiceError> {
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
    })
}

fn deterministic_calendar_event_id(item_id: Uuid) -> String {
    // Google accepts lower-case hexadecimal event IDs. Prefixing with a hex
    // letter keeps this distinct from externally generated identifiers.
    format!("d{}", item_id.simple())
}

fn calendar_event_owned_by(event: &GoogleEvent, item_id: Uuid) -> bool {
    event
        .extended_properties
        .as_ref()
        .is_some_and(|properties| {
            properties
                .private
                .get("dayweaveItemId")
                .is_some_and(|value| value == &item_id.to_string())
                && properties
                    .private
                    .get("dayweaveOwnership")
                    .is_some_and(|value| value == "firm_block")
        })
}

fn calendar_dayweave_item_id(event: &GoogleEvent) -> Result<Option<Uuid>, NormalizationError> {
    let Some(properties) = event.extended_properties.as_ref() else {
        return Ok(None);
    };
    let ownership = properties.private.get("dayweaveOwnership");
    let item_id = properties.private.get("dayweaveItemId");
    match (ownership.map(String::as_str), item_id) {
        (None, None) => Ok(None),
        (Some("firm_block"), Some(item_id)) => Uuid::parse_str(item_id)
            .map(Some)
            .map_err(|_| NormalizationError::Rejected("dayweave_marker_invalid")),
        _ => Err(NormalizationError::Rejected("dayweave_marker_invalid")),
    }
}

fn task_marker(item_id: Uuid) -> String {
    format!("[DayWeave item:{item_id}]")
}

fn guard_new_task_insert(attempts: u32) -> Result<(), GoogleSyncServiceError> {
    // Google Tasks does not accept a client-selected task ID. After any
    // interrupted/failed insert attempt, absence from a bounded list cannot
    // prove that Google did not accept the request (eventual consistency and
    // stripped tombstones are both possible). A second insert could duplicate
    // external state, so require operator reconciliation and a new revision.
    if attempts == 0 {
        Ok(())
    } else {
        Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
    }
}

fn require_external_publication_approval() -> Result<(), GoogleSyncServiceError> {
    // Bearer authentication plus an optimistic revision does not prove that a
    // human approved the exact provider-visible mutation. Keep publication
    // disabled until a server-minted preview token and audit record exist.
    Err(GoogleSyncServiceError::ExternalApprovalRequired)
}

fn task_dayweave_item_id(notes: Option<&str>) -> Result<Option<Uuid>, NormalizationError> {
    const PREFIX: &str = "[DayWeave item:";
    let Some(notes) = notes else {
        return Ok(None);
    };
    let mut found = None;
    for line in notes.lines().map(str::trim) {
        if !line.contains(PREFIX) {
            continue;
        }
        let value = line
            .strip_prefix(PREFIX)
            .and_then(|value| value.strip_suffix(']'))
            .ok_or(NormalizationError::Rejected("dayweave_marker_invalid"))?;
        let item_id = Uuid::parse_str(value)
            .map_err(|_| NormalizationError::Rejected("dayweave_marker_invalid"))?;
        if found.replace(item_id).is_some() {
            return Err(NormalizationError::Rejected("dayweave_marker_invalid"));
        }
    }
    Ok(found)
}

fn parse_event_bound(
    value: &EventDateTime,
    fallback_timezone: &str,
) -> Result<(DateTime<Utc>, String, bool), NormalizationError> {
    if let Some(date_time) = value.date_time.as_deref() {
        let parsed = parse_timestamp(date_time)?;
        let timezone = value
            .time_zone
            .as_deref()
            .filter(|name| name.parse::<Tz>().is_ok())
            .unwrap_or(fallback_timezone);
        let timezone = if timezone.parse::<Tz>().is_ok() {
            timezone.to_owned()
        } else {
            "UTC".to_owned()
        };
        return Ok((parsed, timezone, false));
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
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| NormalizationError::Rejected("timestamp_invalid"))
}

fn parse_optional_timestamp(
    value: Option<&str>,
) -> Result<Option<DateTime<Utc>>, NormalizationError> {
    value.map(parse_timestamp).transpose()
}

fn bounded_title(value: &str, fallback: &str) -> (String, bool) {
    let trimmed = value.trim();
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
    let (value, truncated) = bounded_chars(value, 100_000);
    (Some(value), truncated)
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

fn projection_hash(
    remote_hash: [u8; 32],
    collection: &GoogleSyncCollection,
) -> Result<[u8; 32], NormalizationError> {
    payload_hash(&(remote_hash, collection.visible, collection.sync_role))
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
    #[error("external publication requires a server-minted approval that is not yet available")]
    ExternalApprovalRequired,
    #[error("durable outbound payload is corrupt")]
    OutboundPayloadCorrupt,
    #[error("Google Tasks create result has no safely identifiable provider record")]
    ProviderIdentityUnresolved,
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
            Self::Google(GoogleError::Temporary { .. } | GoogleError::Transport(_)) => {
                FailureDisposition {
                    kind: SyncFailureKind::Backoff,
                    code: "provider_temporary",
                    delay: Duration::minutes(5),
                }
            }
            Self::OAuth(GoogleOAuthServiceError::IntegrationTimeout) => FailureDisposition {
                kind: SyncFailureKind::Backoff,
                code: "oauth_timeout",
                delay: Duration::minutes(5),
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
            Self::ProviderLimitExceeded => FailureDisposition {
                kind: SyncFailureKind::Failed,
                code: "provider_limit_exceeded",
                delay: Duration::hours(24),
            },
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
            Self::ExternalApprovalRequired => FailureDisposition {
                kind: SyncFailureKind::Failed,
                code: "external_approval_required",
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
            revision: 1,
            discovered_at: now,
            configured_at: Some(now),
            last_import_at: None,
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
            recurrence: vec!["RRULE:FREQ=WEEKLY".to_owned()],
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
        }
    }

    #[test]
    fn event_semantics_cover_visibility_busy_declined_and_series() {
        let imported = normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", event())
            .expect("valid event")
            .item
            .expect("upsert");
        assert_eq!(imported.title, "Planning");
        assert_eq!(imported.duration_seconds, Some(3600));
        assert_eq!(imported.timezone_name, "Europe/Madrid");
        assert_eq!(
            imported.flexible_constraints["google_sync"]["blocking"],
            true
        );
        assert!(imported.recurrence.is_some());

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
        let mut birthday = event();
        birthday.event_type = Some("birthday".to_owned());
        let birthday =
            normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", birthday)
                .expect("birthday")
                .item
                .expect("upsert");
        assert_eq!(
            birthday.flexible_constraints["google_sync"]["blocking"],
            false
        );

        let mut free = event();
        free.transparency = Some("transparent".to_owned());
        let free = normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", free)
            .expect("free")
            .item
            .expect("upsert");
        assert_eq!(free.flexible_constraints["google_sync"]["blocking"], false);

        let mut away = event();
        away.event_type = Some("outOfOffice".to_owned());
        let away = normalize_event(&collection(GoogleSyncRole::Blocking, true), "UTC", away)
            .expect("out of office")
            .item
            .expect("upsert");
        assert_eq!(away.flexible_constraints["google_sync"]["blocking"], true);
    }

    #[test]
    fn projection_hash_tracks_visibility_and_role_separately_from_google_payload() {
        let visible = normalize_event(&collection(GoogleSyncRole::ReadOnly, true), "UTC", event())
            .expect("visible projection");
        let hidden = normalize_event(&collection(GoogleSyncRole::Blocking, false), "UTC", event())
            .expect("hidden blocking projection");

        assert_eq!(visible.remote_payload_hash, hidden.remote_payload_hash);
        assert_ne!(
            visible.remote_projection_hash,
            hidden.remote_projection_hash
        );
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
        assert_eq!(item.flexible_constraints["google_sync"]["all_day"], true);
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
        let change = normalize_task(&tasks_collection, task).expect("task");
        let item = change.item.expect("upsert");
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
        let prepared = prepare_calendar_outbound(item, OutboundOperation::Upsert).expect("prepare");
        let event: GoogleEvent = serde_json::from_value(prepared.payload).expect("event");
        assert_eq!(
            event.id,
            deterministic_calendar_event_id(Uuid::from_u128(44))
        );
        assert!(event.attendees.is_empty());
        assert!(calendar_event_owned_by(&event, Uuid::from_u128(44)));
        assert!(!calendar_event_owned_by(&event, Uuid::from_u128(45)));
        assert_eq!(
            event
                .extended_properties
                .expect("marker")
                .private
                .get("dayweaveOwnership")
                .map(String::as_str),
            Some("firm_block")
        );
    }

    #[test]
    fn ownership_markers_are_exact_and_unambiguous() {
        let item_id = Uuid::from_u128(55);
        let mut marked_event = event();
        marked_event.extended_properties = Some(ExtendedProperties {
            private: BTreeMap::from([
                ("dayweaveItemId".to_owned(), item_id.to_string()),
                ("dayweaveOwnership".to_owned(), "firm_block".to_owned()),
            ]),
            shared: BTreeMap::new(),
        });
        assert_eq!(
            calendar_dayweave_item_id(&marked_event).expect("valid event marker"),
            Some(item_id)
        );
        marked_event
            .extended_properties
            .as_mut()
            .expect("properties")
            .private
            .remove("dayweaveOwnership");
        assert_eq!(
            calendar_dayweave_item_id(&marked_event),
            Err(NormalizationError::Rejected("dayweave_marker_invalid"))
        );

        let marker = task_marker(item_id);
        assert_eq!(
            task_dayweave_item_id(Some(&format!("notes\n\n{marker}"))).expect("valid task marker"),
            Some(item_id)
        );
        assert_eq!(
            task_dayweave_item_id(Some(&format!("{marker}\n{marker}"))),
            Err(NormalizationError::Rejected("dayweave_marker_invalid"))
        );
        assert_eq!(
            task_dayweave_item_id(Some("prefix [DayWeave item:not-a-uuid]")),
            Err(NormalizationError::Rejected("dayweave_marker_invalid"))
        );
    }

    #[test]
    fn task_create_is_attempted_at_most_once_without_a_recovered_remote_id() {
        assert!(guard_new_task_insert(0).is_ok());
        assert!(matches!(
            guard_new_task_insert(1),
            Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
        ));
        assert!(matches!(
            guard_new_task_insert(u32::MAX),
            Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
        ));
    }

    #[test]
    fn external_publication_is_fail_closed_without_a_server_minted_approval() {
        assert!(matches!(
            require_external_publication_approval(),
            Err(GoogleSyncServiceError::ExternalApprovalRequired)
        ));
    }

    #[test]
    fn task_marker_recovery_rejects_malformed_or_multiple_provider_identities() {
        let item_id = Uuid::from_u128(77);
        let marker = task_marker(item_id);
        assert!(
            task_recovery_marker_matches(Some(&marker), &marker, item_id)
                .expect("one exact marker")
        );
        assert!(matches!(
            task_recovery_marker_matches(Some(&format!("{marker}\n{marker}")), &marker, item_id,),
            Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
        ));
        assert!(matches!(
            task_recovery_marker_matches(
                Some(&format!("{marker}\n[DayWeave item:not-a-uuid]")),
                &marker,
                item_id,
            ),
            Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
        ));

        let task = |id: &str| GoogleTask {
            id: id.to_owned(),
            etag: None,
            title: "Recovered".to_owned(),
            notes: Some(marker.clone()),
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
        let mut found = None;
        record_task_marker_match(&mut found, task("remote-a")).expect("first match");
        assert!(matches!(
            record_task_marker_match(&mut found, task("remote-b")),
            Err(GoogleSyncServiceError::ProviderIdentityUnresolved)
        ));
    }

    #[test]
    fn successful_provider_upserts_require_an_etag_for_future_conditional_writes() {
        let mut event = event();
        event.etag = None;
        assert!(matches!(
            outbound_event_result(&event),
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
            outbound_task_result(&task),
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
