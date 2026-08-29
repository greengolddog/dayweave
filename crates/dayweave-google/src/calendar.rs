use std::collections::BTreeMap;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{GoogleClient, GoogleError};

/// A provider event page is untrusted input. This bounds peak encoded response
/// memory before `serde` expands strings, arrays, and flattened wire fields.
const MAX_EVENT_LIST_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventListOptions {
    pub page_token: Option<String>,
    pub sync_token: Option<String>,
    /// Expands recurring series into concrete occurrences when `true`.
    ///
    /// The default is `false`, which returns recurring series resources.
    pub single_events: bool,
    pub time_min: Option<String>,
    pub time_max: Option<String>,
    pub max_results: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventInstanceListOptions {
    pub page_token: Option<String>,
    pub time_min: Option<String>,
    pub time_max: Option<String>,
    pub max_results: Option<u16>,
}

impl EventInstanceListOptions {
    fn query(&self) -> Vec<(&'static str, String)> {
        let mut query = vec![
            ("showDeleted", "true".to_owned()),
            (
                "maxResults",
                self.max_results.unwrap_or(2500).min(2500).to_string(),
            ),
        ];
        if let Some(value) = &self.page_token {
            query.push(("pageToken", value.clone()));
        }
        if let Some(value) = &self.time_min {
            query.push(("timeMin", value.clone()));
        }
        if let Some(value) = &self.time_max {
            query.push(("timeMax", value.clone()));
        }
        query
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleCalendar {
    pub id: Option<String>,
    pub etag: Option<String>,
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub time_zone: Option<String>,
    pub conference_properties: Option<ConferenceProperties>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarWrite {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceProperties {
    #[serde(default)]
    pub allowed_conference_solution_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSetting {
    pub id: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarSettingsPage {
    #[serde(default)]
    pub items: Vec<CalendarSetting>,
    pub next_page_token: Option<String>,
    pub next_sync_token: Option<String>,
}

impl EventListOptions {
    fn query(&self) -> Result<Vec<(&'static str, String)>, GoogleError> {
        if self.sync_token.is_some() && (self.time_min.is_some() || self.time_max.is_some()) {
            return Err(GoogleError::InvalidSyncRequest(
                "sync_token cannot be combined with time bounds",
            ));
        }
        let mut query = vec![
            ("showDeleted", "true".to_owned()),
            ("singleEvents", self.single_events.to_string()),
            (
                "maxResults",
                self.max_results.unwrap_or(2500).min(2500).to_string(),
            ),
        ];
        if let Some(value) = &self.page_token {
            query.push(("pageToken", value.clone()));
        }
        if let Some(value) = &self.sync_token {
            query.push(("syncToken", value.clone()));
        }
        if let Some(value) = &self.time_min {
            query.push(("timeMin", value.clone()));
        }
        if let Some(value) = &self.time_max {
            query.push(("timeMax", value.clone()));
        }
        Ok(query)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarListPage {
    #[serde(default)]
    pub items: Vec<CalendarListEntry>,
    pub next_page_token: Option<String>,
    pub next_sync_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Independent fields in Google's wire schema.
pub struct CalendarListEntry {
    pub id: String,
    #[serde(default)]
    pub summary: String,
    pub description: Option<String>,
    pub time_zone: Option<String>,
    #[serde(default)]
    pub access_role: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub deleted: bool,
    pub color_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventListPage {
    #[serde(default)]
    pub items: Vec<GoogleEvent>,
    pub next_page_token: Option<String>,
    pub next_sync_token: Option<String>,
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeBusyRequest {
    pub time_min: String,
    pub time_max: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    pub items: Vec<FreeBusyRequestItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeBusyRequestItem {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeBusyResponse {
    pub time_min: String,
    pub time_max: String,
    #[serde(default)]
    pub calendars: std::collections::BTreeMap<String, FreeBusyCalendar>,
    #[serde(default)]
    pub groups: std::collections::BTreeMap<String, FreeBusyGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeBusyCalendar {
    #[serde(default)]
    pub busy: Vec<FreeBusyInterval>,
    #[serde(default)]
    pub errors: Vec<FreeBusyError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeBusyGroup {
    #[serde(default)]
    pub calendars: Vec<String>,
    #[serde(default)]
    pub errors: Vec<FreeBusyError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeBusyInterval {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeBusyError {
    pub domain: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleEvent {
    pub id: String,
    pub etag: Option<String>,
    pub status: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    /// Cancelled-event tombstones may omit both bounds during incremental sync.
    pub start: Option<EventDateTime>,
    pub end: Option<EventDateTime>,
    pub recurring_event_id: Option<String>,
    pub original_start_time: Option<EventDateTime>,
    #[serde(default)]
    pub recurrence: Vec<String>,
    pub transparency: Option<String>,
    pub visibility: Option<String>,
    pub event_type: Option<String>,
    #[serde(default)]
    pub attendees: Vec<EventAttendee>,
    pub conference_data: Option<serde_json::Value>,
    #[serde(default)]
    pub attachments: Vec<EventAttachment>,
    pub updated: Option<String>,
    pub sequence: Option<i64>,
    pub extended_properties: Option<ExtendedProperties>,
    /// Fields not yet modeled by `DayWeave` are retained so recovery never
    /// mistakes an externally changed event for the exact reviewed create.
    #[serde(default, flatten)]
    pub additional_properties: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDateTime {
    pub date: Option<String>,
    pub date_time: Option<String>,
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAttendee {
    pub email: String,
    pub display_name: Option<String>,
    pub response_status: Option<String>,
    #[serde(default, rename = "self")]
    pub self_: bool,
    #[serde(default)]
    pub organizer: bool,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAttachment {
    pub file_url: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub file_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtendedProperties {
    #[serde(default)]
    pub private: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub shared: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendUpdates {
    None,
    ExternalOnly,
    All,
}

impl SendUpdates {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ExternalOnly => "externalOnly",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventWriteApproval {
    /// Restricted to a private DayWeave-owned event with no attendees.
    PrivateAppOwned,
    /// Correlates the app's explicit confirmation with its audit record.
    Explicit { audit_id: String },
}

impl EventWriteApproval {
    fn validate(&self, event: Option<&GoogleEvent>) -> Result<(), GoogleError> {
        match self {
            Self::PrivateAppOwned if event.is_none_or(|value| value.attendees.is_empty()) => Ok(()),
            Self::Explicit { audit_id } if !audit_id.trim().is_empty() => Ok(()),
            _ => Err(GoogleError::ApprovalRequired),
        }
    }
}

impl GoogleClient {
    /// Lists calendars visible to the account.
    ///
    /// # Errors
    ///
    /// Returns transport, authorization, or provider errors.
    pub async fn list_calendars(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<CalendarListPage, GoogleError> {
        let url = self.endpoint(&["calendar", "v3", "users", "me", "calendarList"])?;
        let mut query = Vec::new();
        query.push(("showDeleted", "true"));
        if let Some(value) = page_token {
            query.push(("pageToken", value));
        }
        if let Some(value) = sync_token {
            query.push(("syncToken", value));
        }
        let request = self.request(Method::GET, url).await?.query(&query);
        self.json(request).await
    }

    /// Reads a calendar's metadata and timezone.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn get_calendar(&self, calendar_id: &str) -> Result<GoogleCalendar, GoogleError> {
        let url = self.endpoint(&["calendar", "v3", "calendars", calendar_id])?;
        let request = self.request(Method::GET, url).await?;
        self.json(request).await
    }

    /// Creates the dedicated private `DayWeave` calendar selected during
    /// onboarding. This does not invite attendees or mutate an existing event.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn create_calendar(
        &self,
        calendar: &CalendarWrite,
    ) -> Result<GoogleCalendar, GoogleError> {
        if calendar.summary.trim().is_empty() {
            return Err(GoogleError::InvalidSyncRequest(
                "calendar summary cannot be empty",
            ));
        }
        let url = self.endpoint(&["calendar", "v3", "calendars"])?;
        let request = self.request(Method::POST, url).await?;
        self.json(Self::body(request, calendar)).await
    }

    /// Lists Google Calendar account settings, including the account timezone.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn list_calendar_settings(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<CalendarSettingsPage, GoogleError> {
        let url = self.endpoint(&["calendar", "v3", "users", "me", "settings"])?;
        let mut query = Vec::new();
        if let Some(value) = page_token {
            query.push(("pageToken", value));
        }
        if let Some(value) = sync_token {
            query.push(("syncToken", value));
        }
        let request = self.request(Method::GET, url).await?.query(&query);
        self.json(request).await
    }

    /// Queries provider free/busy intervals without importing event content.
    ///
    /// # Errors
    ///
    /// Returns an invalid-request error for empty/reversed bounds or no
    /// calendars, plus typed provider errors.
    pub async fn query_free_busy(
        &self,
        request_body: &FreeBusyRequest,
    ) -> Result<FreeBusyResponse, GoogleError> {
        let time_min = time::OffsetDateTime::parse(
            &request_body.time_min,
            &time::format_description::well_known::Rfc3339,
        );
        let time_max = time::OffsetDateTime::parse(
            &request_body.time_max,
            &time::format_description::well_known::Rfc3339,
        );
        if !matches!((time_min, time_max), (Ok(start), Ok(end)) if start < end) {
            return Err(GoogleError::InvalidSyncRequest(
                "free/busy bounds must be valid increasing RFC 3339 timestamps",
            ));
        }
        if request_body.items.is_empty() {
            return Err(GoogleError::InvalidSyncRequest(
                "free/busy query requires a calendar",
            ));
        }
        let url = self.endpoint(&["calendar", "v3", "freeBusy"])?;
        let request = self.request(Method::POST, url).await?;
        self.json(Self::body(request, request_body)).await
    }

    /// Lists event resources and deletion tombstones for incremental sync.
    ///
    /// Set [`EventListOptions::single_events`] to expand recurring series into
    /// concrete occurrences. Keep that setting unchanged when continuing an
    /// incremental sync with a returned sync token.
    ///
    /// # Errors
    ///
    /// Returns [`GoogleError::InvalidSyncRequest`] when a sync token is mixed
    /// with time bounds, and typed provider errors for HTTP failures.
    pub async fn list_events(
        &self,
        calendar_id: &str,
        options: &EventListOptions,
    ) -> Result<EventListPage, GoogleError> {
        let url = self.endpoint(&["calendar", "v3", "calendars", calendar_id, "events"])?;
        let request = self
            .request(Method::GET, url)
            .await?
            .query(&options.query()?);
        self.json_limited(request, MAX_EVENT_LIST_RESPONSE_BYTES)
            .await
    }

    /// Lists concrete instances from one recurring event series. This supports
    /// occurrence editing while retaining the source `RRULE` on the series.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn list_event_instances(
        &self,
        calendar_id: &str,
        recurring_event_id: &str,
        options: &EventInstanceListOptions,
    ) -> Result<EventListPage, GoogleError> {
        let url = self.endpoint(&[
            "calendar",
            "v3",
            "calendars",
            calendar_id,
            "events",
            recurring_event_id,
            "instances",
        ])?;
        let request = self
            .request(Method::GET, url)
            .await?
            .query(&options.query());
        self.json_limited(request, MAX_EVENT_LIST_RESPONSE_BYTES)
            .await
    }

    /// Reads a single event or recurrence exception by remote ID.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn get_event(
        &self,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<GoogleEvent, GoogleError> {
        let url = self.endpoint(&[
            "calendar",
            "v3",
            "calendars",
            calendar_id,
            "events",
            event_id,
        ])?;
        let request = self.request(Method::GET, url).await?;
        self.json(request).await
    }

    /// Creates an event after enforcing the external-change approval boundary.
    /// # Errors
    ///
    /// Returns [`GoogleError::ApprovalRequired`] for attendee-bearing events
    /// without explicit approval, plus typed provider errors.
    pub async fn prepare_insert_event(
        &self,
        calendar_id: &str,
        event: &GoogleEvent,
        approval: &EventWriteApproval,
        send_updates: SendUpdates,
    ) -> Result<crate::PreparedGoogleRequest, GoogleError> {
        approval.validate(Some(event))?;
        let url = self.endpoint(&["calendar", "v3", "calendars", calendar_id, "events"])?;
        let request = self.request(Method::POST, url).await?.query(&[
            ("sendUpdates", send_updates.as_str()),
            ("conferenceDataVersion", "1"),
            ("supportsAttachments", "true"),
        ]);
        self.prepare(Self::body(request, event))
    }

    /// Creates an event after enforcing the external-change approval boundary.
    ///
    /// # Errors
    ///
    /// Returns approval, transport, authorization, or provider errors.
    pub async fn insert_event(
        &self,
        calendar_id: &str,
        event: &GoogleEvent,
        approval: &EventWriteApproval,
        send_updates: SendUpdates,
    ) -> Result<GoogleEvent, GoogleError> {
        self.prepare_insert_event(calendar_id, event, approval, send_updates)
            .await?
            .send_json(None)
            .await
    }

    /// Replaces an event conditionally using its last-seen `ETag`.
    /// # Errors
    ///
    /// Returns approval, stale-write, transport, authorization, or API errors.
    pub async fn prepare_update_event(
        &self,
        calendar_id: &str,
        event: &GoogleEvent,
        approval: &EventWriteApproval,
        send_updates: SendUpdates,
    ) -> Result<crate::PreparedGoogleRequest, GoogleError> {
        approval.validate(Some(event))?;
        let etag = event
            .etag
            .as_deref()
            .filter(|etag| !etag.trim().is_empty())
            .ok_or(GoogleError::ConditionalWriteRequired)?;
        let url = self.endpoint(&[
            "calendar",
            "v3",
            "calendars",
            calendar_id,
            "events",
            &event.id,
        ])?;
        let mut request = self.request(Method::PUT, url).await?.query(&[
            ("sendUpdates", send_updates.as_str()),
            ("conferenceDataVersion", "1"),
            ("supportsAttachments", "true"),
        ]);
        request = request.header(reqwest::header::IF_MATCH, etag);
        self.prepare(Self::body(request, event))
    }

    /// Replaces an event conditionally using its last-seen `ETag`.
    ///
    /// # Errors
    ///
    /// Returns approval, stale-write, transport, authorization, or API errors.
    pub async fn update_event(
        &self,
        calendar_id: &str,
        event: &GoogleEvent,
        approval: &EventWriteApproval,
        send_updates: SendUpdates,
    ) -> Result<GoogleEvent, GoogleError> {
        self.prepare_update_event(calendar_id, event, approval, send_updates)
            .await?
            .send_json(None)
            .await
    }

    /// Deletes an event. Callers must carry either the private-app-owned proof
    /// or an explicit confirmation audit ID.
    ///
    /// # Errors
    ///
    /// Returns approval, stale-write, transport, authorization, or API errors.
    pub async fn prepare_delete_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        etag: &str,
        approval: &EventWriteApproval,
        send_updates: SendUpdates,
    ) -> Result<crate::PreparedGoogleRequest, GoogleError> {
        approval.validate(None)?;
        if etag.trim().is_empty() {
            return Err(GoogleError::ConditionalWriteRequired);
        }
        let url = self.endpoint(&[
            "calendar",
            "v3",
            "calendars",
            calendar_id,
            "events",
            event_id,
        ])?;
        let mut request = self
            .request(Method::DELETE, url)
            .await?
            .query(&[("sendUpdates", send_updates.as_str())]);
        request = request.header(reqwest::header::IF_MATCH, etag);
        self.prepare(request)
    }

    /// Deletes an event conditionally after approval validation.
    ///
    /// # Errors
    ///
    /// Returns approval, stale-write, transport, authorization, or API errors.
    pub async fn delete_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        etag: &str,
        approval: &EventWriteApproval,
        send_updates: SendUpdates,
    ) -> Result<(), GoogleError> {
        self.prepare_delete_event(calendar_id, event_id, etag, approval, send_updates)
            .await?
            .send_empty(None)
            .await
    }
}
