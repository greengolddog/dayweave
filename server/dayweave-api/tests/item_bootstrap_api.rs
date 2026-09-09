use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use dayweave_api::{
    AppState,
    auth::StaticTokenAuthenticator,
    http::router,
    items::{IdempotencyKey, InMemoryItemRepository, ItemService, NewItem},
    proposals::{Clock, InMemoryProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "synthetic-bootstrap-api-token";

struct TestClock(Mutex<DateTime<Utc>>);
impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

fn app(items: Arc<ItemService>) -> Router {
    let proposals = Arc::new(ProposalService::new(
        Arc::new(InMemoryProposalRepository::default()),
        Arc::new(SystemClock),
        Duration::from_hours(24),
    ));
    let ready = Readiness::default();
    ready.set_ready(true);
    router(
        AppState::new(
            proposals,
            Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN])),
            ready,
        )
        .with_items(items),
    )
}

fn input(id: Uuid) -> NewItem {
    serde_json::from_value(json!({
    "id":id,"is_sensitive":false,"kind":"task","status":"planned","title":"Synthetic snapshot item",
    "notes":null,"timezone_name":"UTC","duration_seconds":60,"deadline_at":null,"earliest_start_at":null,
    "recurrence":null,"flexible_constraints":{},"split_policy":{"type":"indivisible"},
    "importance":1,"urgency":1,"parent_id":null,"sibling_order":0
})).unwrap()
}

async fn call(app: &Router, uri: &str, expected: StatusCode) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), expected);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One issued ticket proves scope, integrity, stable pages, expiry and legacy handoff together.
async fn explicit_bootstrap_is_frozen_paged_and_hands_back_the_legacy_stream() {
    let clock = Arc::new(TestClock(Mutex::new(Utc::now())));
    let service = Arc::new(ItemService::new(
        Arc::new(InMemoryItemRepository::default()),
        clock.clone(),
    ));
    for index in 1..=301_u128 {
        service
            .create(
                input(Uuid::from_u128(index)),
                IdempotencyKey {
                    key: format!("bootstrap-{index:06}"),
                    fingerprint: [1; 32],
                },
            )
            .await
            .unwrap();
    }
    let app = app(service.clone());
    let first = call(
        &app,
        "/v1/items/delta?bootstrap=current&limit=1",
        StatusCode::OK,
    )
    .await;
    assert_eq!(first.as_object().unwrap().len(), 3);
    assert_eq!(first["changes"].as_array().unwrap().len(), 300);
    assert_eq!(first["has_more"], true);
    let ticket = first["next_cursor"].as_str().unwrap();
    let foreign = self::app(Arc::new(ItemService::new(
        Arc::new(InMemoryItemRepository::default()),
        clock.clone(),
    )));
    let foreign_response = call(
        &foreign,
        &format!("/v1/items/delta?cursor={ticket}"),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(
        foreign_response["error"]["code"],
        "item_bootstrap_cursor_invalid"
    );
    let mut damaged = URL_SAFE_NO_PAD.decode(ticket).unwrap();
    *damaged.last_mut().unwrap() ^= 1;
    let damaged = URL_SAFE_NO_PAD.encode(damaged);
    let tampered = call(
        &app,
        &format!("/v1/items/delta?cursor={damaged}"),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(tampered["error"]["code"], "item_bootstrap_cursor_invalid");
    let truncated = URL_SAFE_NO_PAD.encode(&URL_SAFE_NO_PAD.decode(ticket).unwrap()[..39]);
    let truncated_response = call(
        &app,
        &format!("/v1/items/delta?cursor={truncated}"),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(
        truncated_response["error"]["code"],
        "item_bootstrap_cursor_invalid"
    );
    let repeated = call(&app, "/v1/items/delta?bootstrap=current", StatusCode::OK).await;
    assert_eq!(
        first, repeated,
        "lost initial replies reuse the fixed ticket"
    );
    let newest = Uuid::from_u128(999);
    service
        .create(
            input(newest),
            IdempotencyKey {
                key: "bootstrap-later".into(),
                fingerprint: [2; 32],
            },
        )
        .await
        .unwrap();
    let last = call(
        &app,
        &format!("/v1/items/delta?cursor={ticket}&limit=1"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(last["changes"].as_array().unwrap().len(), 1);
    assert_eq!(last["has_more"], false);
    let terminal = last["next_cursor"].as_str().unwrap();
    let following = call(
        &app,
        &format!("/v1/items/delta?cursor={terminal}"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(following["changes"][0]["item"]["id"], newest.to_string());
    let legacy = call(&app, "/v1/items/delta?limit=1", StatusCode::OK).await;
    assert_eq!(legacy["changes"].as_array().unwrap().len(), 1);
    assert_ne!(legacy["next_cursor"], first["next_cursor"]);
    let invalid = call(
        &app,
        &format!("/v1/items/delta?bootstrap=current&cursor={ticket}"),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(invalid["error"]["code"], "item_bootstrap_cursor_invalid");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/items/stream")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::ACCEPT, "text/event-stream")
                .header("last-event-id", ticket)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    *clock.0.lock().unwrap() += chrono::Duration::minutes(11);
    let expired = call(
        &app,
        &format!("/v1/items/delta?cursor={ticket}"),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(expired["error"]["code"], "item_bootstrap_expired");
}

#[tokio::test]
async fn bootstrap_empty_and_unsupported_mode_are_explicit_without_reinterpreting_history() {
    let service = Arc::new(ItemService::new(
        Arc::new(InMemoryItemRepository::default()),
        Arc::new(SystemClock),
    ));
    let app = app(service);
    let empty = call(&app, "/v1/items/delta?bootstrap=current", StatusCode::OK).await;
    assert_eq!(empty["changes"], json!([]));
    assert_eq!(empty["has_more"], false);
    let invalid = call(
        &app,
        "/v1/items/delta?bootstrap=unknown",
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(invalid["error"]["code"], "item_bootstrap_cursor_invalid");
    let history = call(&app, "/v1/items/delta", StatusCode::OK).await;
    assert_eq!(empty, history);
}

#[tokio::test]
async fn bootstrap_capacity_reuses_current_ticket_and_expires_without_losing_legacy_history() {
    let clock = Arc::new(TestClock(Mutex::new(Utc::now())));
    let service = Arc::new(ItemService::new(
        Arc::new(InMemoryItemRepository::default()),
        clock.clone(),
    ));
    let application = app(service.clone());
    let mut last_snapshot = Value::Null;
    for index in 1..=16_u128 {
        service
            .create(
                input(Uuid::from_u128(index)),
                IdempotencyKey {
                    key: format!("capacity-{index:06}"),
                    fingerprint: [0x41; 32],
                },
            )
            .await
            .unwrap();
        last_snapshot = call(
            &application,
            "/v1/items/delta?bootstrap=current",
            StatusCode::OK,
        )
        .await;
        assert_eq!(
            last_snapshot["changes"].as_array().unwrap().len(),
            usize::try_from(index).unwrap()
        );
        assert_eq!(last_snapshot["has_more"], false);
    }
    // All sixteen live slots are occupied: an additional successful request
    // at this same head must reuse a ticket rather than allocate a seventeenth.
    let reused = call(
        &application,
        "/v1/items/delta?bootstrap=current",
        StatusCode::OK,
    )
    .await;
    assert_eq!(reused, last_snapshot);
    service
        .create(
            input(Uuid::from_u128(17)),
            IdempotencyKey {
                key: "capacity-seventeenth".into(),
                fingerprint: [0x42; 32],
            },
        )
        .await
        .unwrap();
    let exhausted = call(
        &application,
        "/v1/items/delta?bootstrap=current",
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(exhausted["error"]["code"], "item_bootstrap_capacity");
    let old_terminal = last_snapshot["next_cursor"].as_str().unwrap();
    let historical = call(
        &application,
        &format!("/v1/items/delta?cursor={old_terminal}"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(historical["changes"].as_array().unwrap().len(), 1);
    assert_eq!(
        historical["changes"][0]["item"]["id"],
        Uuid::from_u128(17).to_string()
    );
    *clock.0.lock().unwrap() += chrono::Duration::minutes(11);
    let renewed = call(
        &application,
        "/v1/items/delta?bootstrap=current",
        StatusCode::OK,
    )
    .await;
    assert_eq!(renewed["changes"].as_array().unwrap().len(), 17);
    assert_eq!(renewed["has_more"], false);
    let after_expiry = call(
        &application,
        &format!("/v1/items/delta?cursor={old_terminal}"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        after_expiry, historical,
        "expiring snapshot tickets never expires canonical history"
    );
}
