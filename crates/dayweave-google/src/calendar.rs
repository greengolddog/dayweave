use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::{GoogleClient, GoogleError};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventListOptions {
    pub page_token: Option<String>,
    pub sync_token: Option<String>,
    pub time_min: Option<String>,
    pub time_max: Option<String>,
    pub max_results: Option<u16>,
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
            ("singleEvents", "false".to_owned()),
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
    pub summary: String,
    pub description: Option<String>,
    pub time_zone: Option<String>,
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

    /// Lists complete event series and deletion tombstones for incremental sync.
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
        self.json(request).await
    }

    /// Creates an event after enforcing the external-change approval boundary.
    ///
    /// # Errors
    ///
    /// Returns [`GoogleError::ApprovalRequired`] for attendee-bearing events
    /// without explicit approval, plus typed provider errors.
    pub async fn insert_event(
        &self,
        calendar_id: &str,
        event: &GoogleEvent,
        approval: &EventWriteApproval,
        send_updates: SendUpdates,
    ) -> Result<GoogleEvent, GoogleError> {
        approval.validate(Some(event))?;
        let url = self.endpoint(&["calendar", "v3", "calendars", calendar_id, "events"])?;
        let request = self
            .request(Method::POST, url)
            .await?
            .query(&[("sendUpdates", send_updates.as_str())]);
        self.json(Self::body(request, event)).await
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
        approval.validate(Some(event))?;
        let url = self.endpoint(&[
            "calendar",
            "v3",
            "calendars",
            calendar_id,
            "events",
            &event.id,
        ])?;
        let mut request = self
            .request(Method::PUT, url)
            .await?
            .query(&[("sendUpdates", send_updates.as_str())]);
        if let Some(etag) = &event.etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        self.json(Self::body(request, event)).await
    }

    /// Deletes an event. Callers must carry either the private-app-owned proof
    /// or an explicit confirmation audit ID.
    ///
    /// # Errors
    ///
    /// Returns approval, stale-write, transport, authorization, or API errors.
    pub async fn delete_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        etag: Option<&str>,
        approval: &EventWriteApproval,
        send_updates: SendUpdates,
    ) -> Result<(), GoogleError> {
        approval.validate(None)?;
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
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        self.empty(request).await
    }
}
