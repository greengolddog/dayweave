use std::sync::Arc;

use dayweave_google::{
    GoogleClient, GoogleError, StaticAccessToken,
    calendar::{
        CalendarWrite, EventAttendee, EventDateTime, EventInstanceListOptions, EventListOptions,
        EventWriteApproval, ExtendedProperties, FreeBusyRequest, FreeBusyRequestItem, GoogleEvent,
        SendUpdates,
    },
    tasks::{GoogleTask, TaskInsertOptions},
};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

fn client(server: &MockServer) -> GoogleClient {
    GoogleClient::with_base_url(
        Arc::new(StaticAccessToken::new("test-access-token")),
        &format!("{}/", server.uri()),
    )
    .expect("wiremock URL is a valid API base")
}

fn event(attendees: Vec<EventAttendee>) -> GoogleEvent {
    GoogleEvent {
        id: "event-1".to_owned(),
        etag: Some("\"etag-1\"".to_owned()),
        status: Some("confirmed".to_owned()),
        summary: Some("Planning".to_owned()),
        description: None,
        location: None,
        start: Some(EventDateTime {
            date: None,
            date_time: Some("2026-08-30T09:00:00+02:00".to_owned()),
            time_zone: Some("Europe/Madrid".to_owned()),
        }),
        end: Some(EventDateTime {
            date: None,
            date_time: Some("2026-08-30T10:00:00+02:00".to_owned()),
            time_zone: Some("Europe/Madrid".to_owned()),
        }),
        recurring_event_id: None,
        original_start_time: None,
        recurrence: Vec::new(),
        transparency: None,
        visibility: Some("private".to_owned()),
        event_type: Some("default".to_owned()),
        attendees,
        conference_data: None,
        attachments: Vec::new(),
        updated: Some("2026-08-29T20:00:00Z".to_owned()),
        sequence: Some(1),
        extended_properties: Some(ExtendedProperties::default()),
    }
}

#[tokio::test]
async fn lists_events_with_encoded_calendar_id_and_sync_contract() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/calendars/team%2Fprimary/events"))
        .and(header("authorization", "Bearer test-access-token"))
        .and(query_param("showDeleted", "true"))
        .and(query_param("singleEvents", "false"))
        .and(query_param("maxResults", "2500"))
        .and(query_param("timeMin", "2026-08-29T00:00:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {
                    "id": "all-day",
                    "status": "confirmed",
                    "summary": "Birthday",
                    "eventType": "birthday",
                    "start": {"date": "2026-08-30"},
                    "end": {"date": "2026-08-31"},
                    "attendees": [{"email": "me@example.test", "self": true}]
                },
                {
                    "id": "deleted-instance",
                    "status": "cancelled",
                    "recurringEventId": "series-1",
                    "originalStartTime": {"dateTime": "2026-08-31T09:00:00Z"}
                }
            ],
            "nextSyncToken": "sync-2",
            "timeZone": "Europe/Madrid"
        })))
        .mount(&server)
        .await;

    let page = client(&server)
        .list_events(
            "team/primary",
            &EventListOptions {
                time_min: Some("2026-08-29T00:00:00Z".to_owned()),
                ..EventListOptions::default()
            },
        )
        .await
        .expect("event page parses");

    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].event_type.as_deref(), Some("birthday"));
    assert!(page.items[0].attendees[0].self_);
    assert!(page.items[1].start.is_none());
    assert_eq!(page.next_sync_token.as_deref(), Some("sync-2"));
}

#[tokio::test]
async fn rejects_invalid_incremental_sync_before_network_access() {
    let server = MockServer::start().await;
    let error = client(&server)
        .list_events(
            "primary",
            &EventListOptions {
                sync_token: Some("sync-1".to_owned()),
                time_max: Some("2026-09-01T00:00:00Z".to_owned()),
                ..EventListOptions::default()
            },
        )
        .await
        .expect_err("mixed sync token and time bounds must fail");

    assert!(matches!(error, GoogleError::InvalidSyncRequest(_)));
    assert!(
        server
            .received_requests()
            .await
            .expect("request journal")
            .is_empty()
    );
}

#[tokio::test]
async fn maps_expired_sync_token_to_bounded_resync_signal() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/calendars/primary/events"))
        .and(query_param("syncToken", "expired"))
        .respond_with(ResponseTemplate::new(410))
        .mount(&server)
        .await;

    let error = client(&server)
        .list_events(
            "primary",
            &EventListOptions {
                sync_token: Some("expired".to_owned()),
                ..EventListOptions::default()
            },
        )
        .await
        .expect_err("410 requires a bounded full resync");

    assert!(matches!(error, GoogleError::SyncTokenExpired));
}

#[tokio::test]
async fn calendar_list_tombstones_allow_omitted_display_metadata() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList"))
        .and(query_param("showDeleted", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "removed-calendar", "deleted": true}]
        })))
        .mount(&server)
        .await;

    let page = client(&server)
        .list_calendars(None, None)
        .await
        .expect("minimal calendar tombstone parses");
    assert!(page.items[0].deleted);
    assert!(page.items[0].summary.is_empty());
    assert!(page.items[0].access_role.is_empty());
}

#[tokio::test]
async fn authorized_transport_never_follows_provider_redirects() {
    let source = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList"))
        .and(header("authorization", "Bearer test-access-token"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/capture", target.uri())),
        )
        .mount(&source)
        .await;

    let error = client(&source)
        .list_calendars(None, None)
        .await
        .expect_err("redirect is surfaced instead of followed");
    assert!(matches!(error, GoogleError::Api { status: 302 }));
    assert!(
        target
            .received_requests()
            .await
            .expect("target request journal")
            .is_empty()
    );
}

#[tokio::test]
async fn attendee_write_requires_explicit_approval_without_sending_request() {
    let server = MockServer::start().await;
    let external = event(vec![EventAttendee {
        email: "guest@example.test".to_owned(),
        display_name: None,
        response_status: Some("needsAction".to_owned()),
        self_: false,
        organizer: false,
        optional: false,
    }]);

    let error = client(&server)
        .insert_event(
            "primary",
            &external,
            &EventWriteApproval::PrivateAppOwned,
            SendUpdates::All,
        )
        .await
        .expect_err("external mutation needs explicit confirmation");

    assert!(matches!(error, GoogleError::ApprovalRequired));
    assert!(
        server
            .received_requests()
            .await
            .expect("request journal")
            .is_empty()
    );
}

#[tokio::test]
async fn explicit_approval_writes_attendees_and_notification_policy() {
    let server = MockServer::start().await;
    let external = event(vec![EventAttendee {
        email: "guest@example.test".to_owned(),
        display_name: Some("Guest".to_owned()),
        response_status: Some("needsAction".to_owned()),
        self_: false,
        organizer: false,
        optional: false,
    }]);
    Mock::given(method("POST"))
        .and(path("/calendar/v3/calendars/primary/events"))
        .and(query_param("sendUpdates", "all"))
        .and(body_json(&external))
        .respond_with(ResponseTemplate::new(200).set_body_json(&external))
        .mount(&server)
        .await;

    let written = client(&server)
        .insert_event(
            "primary",
            &external,
            &EventWriteApproval::Explicit {
                audit_id: "approval-123".to_owned(),
            },
            SendUpdates::All,
        )
        .await
        .expect("approved event is written");

    assert_eq!(written, external);
}

#[tokio::test]
async fn conditional_event_update_maps_stale_etag() {
    let server = MockServer::start().await;
    let updated = event(Vec::new());
    Mock::given(method("PUT"))
        .and(path("/calendar/v3/calendars/primary/events/event-1"))
        .and(header("if-match", "\"etag-1\""))
        .respond_with(ResponseTemplate::new(412))
        .mount(&server)
        .await;

    let error = client(&server)
        .update_event(
            "primary",
            &updated,
            &EventWriteApproval::PrivateAppOwned,
            SendUpdates::None,
        )
        .await
        .expect_err("stale event must not overwrite provider state");

    assert!(matches!(error, GoogleError::PreconditionFailed));
}

#[tokio::test]
async fn conditional_task_update_encodes_ids_and_maps_stale_etag() {
    let server = MockServer::start().await;
    let task = GoogleTask {
        id: "task/one".to_owned(),
        etag: Some("\"task-etag\"".to_owned()),
        title: "Safe update".to_owned(),
        notes: Some("[DayWeave item:00000000-0000-0000-0000-000000000001]".to_owned()),
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
    Mock::given(method("PUT"))
        .and(path("/tasks/v1/lists/list%2Fone/tasks/task%2Fone"))
        .and(header("if-match", "\"task-etag\""))
        .respond_with(ResponseTemplate::new(412))
        .mount(&server)
        .await;

    let error = client(&server)
        .update_task("list/one", &task)
        .await
        .expect_err("stale task must not overwrite provider state");
    assert!(matches!(error, GoogleError::PreconditionFailed));
}

#[tokio::test]
async fn task_listing_preserves_completed_hidden_and_deleted_state() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/tasks/v1/lists/inbox%2Fpersonal/tasks"))
        .and(query_param("showCompleted", "true"))
        .and(query_param("showDeleted", "true"))
        .and(query_param("showHidden", "true"))
        .and(query_param("maxResults", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": "task-1",
                "status": "completed",
                "deleted": true,
                "hidden": true
            }]
        })))
        .mount(&server)
        .await;

    let page = client(&server)
        .list_tasks("inbox/personal", None, None)
        .await
        .expect("task tombstones parse");

    assert!(page.items[0].deleted);
    assert!(page.items[0].hidden);
    assert!(page.items[0].title.is_empty());
}

#[tokio::test]
async fn exposes_numeric_retry_after_without_response_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/tasks/v1/users/@me/lists"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "17"))
        .mount(&server)
        .await;

    let error = client(&server)
        .list_task_lists(None)
        .await
        .expect_err("rate limit must be surfaced");

    assert!(matches!(
        error,
        GoogleError::RateLimited {
            retry_after_seconds: Some(17)
        }
    ));
}

#[tokio::test]
async fn creates_dedicated_calendar_and_queries_free_busy_without_event_content() {
    let server = MockServer::start().await;
    let calendar = CalendarWrite {
        summary: "DayWeave".to_owned(),
        description: Some("Firm private schedule blocks".to_owned()),
        location: None,
        time_zone: Some("Europe/Madrid".to_owned()),
    };
    Mock::given(method("POST"))
        .and(path("/calendar/v3/calendars"))
        .and(body_json(&calendar))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "dayweave@example.test",
            "etag": "calendar-etag",
            "summary": "DayWeave",
            "description": "Firm private schedule blocks",
            "timeZone": "Europe/Madrid"
        })))
        .mount(&server)
        .await;
    let created = client(&server)
        .create_calendar(&calendar)
        .await
        .expect("dedicated calendar is created");
    assert_eq!(created.id.as_deref(), Some("dayweave@example.test"));

    let free_busy = FreeBusyRequest {
        time_min: "2026-08-30T00:00:00+02:00".to_owned(),
        time_max: "2026-08-31T00:00:00+02:00".to_owned(),
        time_zone: Some("Europe/Madrid".to_owned()),
        items: vec![FreeBusyRequestItem {
            id: "dayweave@example.test".to_owned(),
        }],
    };
    Mock::given(method("POST"))
        .and(path("/calendar/v3/freeBusy"))
        .and(body_json(&free_busy))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "timeMin": "2026-08-29T22:00:00Z",
            "timeMax": "2026-08-30T22:00:00Z",
            "calendars": {
                "dayweave@example.test": {
                    "busy": [{
                        "start": "2026-08-30T09:00:00+02:00",
                        "end": "2026-08-30T10:00:00+02:00"
                    }]
                }
            }
        })))
        .mount(&server)
        .await;
    let availability = client(&server)
        .query_free_busy(&free_busy)
        .await
        .expect("free/busy response parses");
    assert_eq!(
        availability.calendars["dayweave@example.test"].busy.len(),
        1
    );
}

#[tokio::test]
async fn rejects_invalid_free_busy_range_without_network_access() {
    let server = MockServer::start().await;
    let error = client(&server)
        .query_free_busy(&FreeBusyRequest {
            time_min: "2026-08-31T00:00:00Z".to_owned(),
            time_max: "2026-08-30T00:00:00Z".to_owned(),
            time_zone: None,
            items: vec![FreeBusyRequestItem {
                id: "primary".to_owned(),
            }],
        })
        .await
        .expect_err("reversed range is invalid");
    assert!(matches!(error, GoogleError::InvalidSyncRequest(_)));
    assert!(
        server
            .received_requests()
            .await
            .expect("request journal")
            .is_empty()
    );
}

#[tokio::test]
async fn lists_recurring_instances_with_encoded_series_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/calendar/v3/calendars/team%2Fprimary/events/series%2Fone/instances",
        ))
        .and(query_param("showDeleted", "true"))
        .and(query_param("timeMin", "2026-08-30T00:00:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": "instance-1",
                "status": "confirmed",
                "start": {"dateTime": "2026-08-30T09:00:00Z"},
                "end": {"dateTime": "2026-08-30T09:30:00Z"},
                "recurringEventId": "series/one"
            }]
        })))
        .mount(&server)
        .await;

    let page = client(&server)
        .list_event_instances(
            "team/primary",
            "series/one",
            &EventInstanceListOptions {
                time_min: Some("2026-08-30T00:00:00Z".to_owned()),
                ..EventInstanceListOptions::default()
            },
        )
        .await
        .expect("instance page parses");
    assert_eq!(
        page.items[0].recurring_event_id.as_deref(),
        Some("series/one")
    );
}

#[tokio::test]
async fn positions_google_subtask_with_encoded_task_list() {
    let server = MockServer::start().await;
    let task = GoogleTask {
        id: "child".to_owned(),
        etag: None,
        title: "Pack charger".to_owned(),
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
    Mock::given(method("POST"))
        .and(path("/tasks/v1/lists/travel%2Flist/tasks"))
        .and(query_param("parent", "parent-1"))
        .and(query_param("previous", "sibling-1"))
        .and(body_json(json!({
            "title": "Pack charger",
            "status": "needsAction"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(&task))
        .mount(&server)
        .await;
    let created = client(&server)
        .insert_task_at(
            "travel/list",
            &task,
            &TaskInsertOptions {
                parent: Some("parent-1".to_owned()),
                previous: Some("sibling-1".to_owned()),
            },
        )
        .await
        .expect("positioned task is created");
    assert_eq!(created.title, "Pack charger");
}
