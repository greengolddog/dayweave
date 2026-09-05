use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, Response, StatusCode, header},
};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Timelike as _, Utc};
use dayweave_api::{
    AppState,
    auth::{
        AuthenticationError, Authenticator, Principal, PrincipalAudience, Scope,
        StaticTokenAuthenticator,
    },
    habits::{
        HabitAnalyticsBucket, HabitIdempotency, HabitMissedConfiguration,
        HabitMissedExplicitAction, HabitMissedPolicy, HabitMissedResolutionAction,
        HabitOccurrenceEvidence, HabitOutcomeInput, HabitOutcomeStatus, HabitRepository,
        HabitRepositoryError, HabitService, InMemoryHabitRepository, MissedReconcileWrite,
        MissedResolveWrite, OutcomeWrite, PauseCreate, PauseResume,
    },
    http::router,
    items::{IdempotencyKey as ItemIdempotencyKey, InMemoryItemRepository, ItemService, NewItem},
    proposals::{
        Clock, InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock,
    },
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::time::timeout;
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "habit-api-test-token";
const PRIVATE_NOTE: &str = "SYNTHETIC-PRIVATE-HABIT-NOTE";

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[derive(Clone)]
struct PrincipalAuthenticator(Principal);

#[async_trait]
impl Authenticator for PrincipalAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<Principal, AuthenticationError> {
        if token == TOKEN {
            Ok(self.0.clone())
        } else {
            Err(AuthenticationError::InvalidCredentials)
        }
    }
}

fn test_app_with_authenticator(
    authenticator: Arc<dyn Authenticator>,
) -> (Router, Arc<InMemoryHabitRepository>) {
    let proposals: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposals,
        Arc::new(SystemClock),
        Duration::from_hours(24),
    ));
    let readiness = Readiness::default();
    readiness.set_ready(true);
    let items = Arc::new(ItemService::new(
        Arc::new(InMemoryItemRepository::default()),
        Arc::new(SystemClock),
    ));
    let habits = Arc::new(InMemoryHabitRepository::default());
    let state = AppState::new(proposals, authenticator, readiness)
        .with_items(items)
        .with_habit_repository(habits.clone());
    (router(state), habits)
}

fn test_app() -> (Router, Arc<InMemoryHabitRepository>) {
    test_app_with_authenticator(Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN])))
}

fn test_app_with_scopes(scopes: Vec<Scope>) -> Router {
    test_app_with_authenticator(Arc::new(PrincipalAuthenticator(Principal {
        subject: "device-session:habit-api-test".to_owned(),
        scopes,
        audience: PrincipalAudience::Device,
        workspace_id: None,
        user_id: None,
        credential_id: None,
        allowed_origins: Vec::new(),
    })))
    .0
}

fn request(method: &str, uri: &str, body: Option<Value>, key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("valid request")
}

async fn body_json(response: Response<Body>) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn analytics_for_date(app: Router, habit_id: Uuid, date: NaiveDate) -> Value {
    let response = app
        .oneshot(request(
            "GET",
            &format!(
                "/v1/habits/{habit_id}/analytics?start_date={date}&end_date={date}&bucket=day"
            ),
            None,
            None,
        ))
        .await
        .expect("habit analytics response");
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

async fn replace_missed_policy(app: Router, habit_id: Uuid, policy: &str, key: &str) {
    let mut replacement = habit_body_with_missed_policy(habit_id, policy);
    replacement.as_object_mut().unwrap().remove("id");
    let response = app
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{habit_id}"),
            Some(json!({"expected_revision":1,"item":replacement})),
            Some(key),
        ))
        .await
        .expect("replace the current missed policy");
    assert_eq!(response.status(), StatusCode::OK);
}

fn habit_idempotency(
    namespace: &'static str,
    key_hash_byte: u8,
    fingerprint_byte: u8,
) -> HabitIdempotency {
    HabitIdempotency {
        namespace,
        key_hash: [key_hash_byte; 32],
        request_fingerprint: [fingerprint_byte; 32],
        operation_id: Uuid::new_v4(),
        actor_session_id: None,
    }
}

async fn round_trip_preserving_pause(
    repository: &InMemoryHabitRepository,
    habit_id: Uuid,
    pause_start: DateTime<Utc>,
    pause_end: DateTime<Utc>,
) {
    let pause_id = Uuid::new_v4();
    repository
        .create_pause(PauseCreate {
            id: pause_id,
            habit_id,
            expected_revision: 0,
            started_at: pause_start,
            preserves_streak: true,
            recorded_at: pause_start,
            idempotency: HabitIdempotency {
                namespace: "test.analytics.carry.pause",
                key_hash: [0x44; 32],
                request_fingerprint: [0x45; 32],
                operation_id: Uuid::new_v4(),
                actor_session_id: None,
            },
        })
        .await
        .expect("start preserving pause");
    repository
        .resume_pause(PauseResume {
            id: pause_id,
            habit_id,
            expected_revision: 1,
            ended_at: pause_end,
            recorded_at: pause_end,
            idempotency: HabitIdempotency {
                namespace: "test.analytics.carry.resume",
                key_hash: [0x46; 32],
                request_fingerprint: [0x47; 32],
                operation_id: Uuid::new_v4(),
                actor_session_id: None,
            },
        })
        .await
        .expect("end preserving pause");
}

fn habit_body(habit_id: Uuid) -> Value {
    json!({
        "id": habit_id,
        "is_sensitive": true,
        "kind": "habit",
        "status": "planned",
        "title": "Private reading habit",
        "notes": null,
        "timezone_name": "Europe/Paris",
        "duration_seconds": 1800,
        "deadline_at": null,
        "earliest_start_at": null,
        "recurrence": { "type": "daily", "times_per_day": 1 },
        "flexible_constraints": {
            "habit_target": { "amount": 20, "unit": "pages" },
            "preserves_streak_when_paused": true
        },
        "split_policy": { "type": "indivisible" },
        "importance": 80,
        "urgency": 60,
        "parent_id": null,
        "sibling_order": 0
    })
}

fn habit_body_with_missed_policy(habit_id: Uuid, policy: &str) -> Value {
    let mut value = habit_body(habit_id);
    value["flexible_constraints"]["habit_missed_policy"] = json!(policy);
    value
}

fn occurrence_evidence(
    evidence_id: Uuid,
    habit_id: Uuid,
    local_date: NaiveDate,
    missed_policy: &str,
) -> HabitOccurrenceEvidence {
    let planner_occurrence_id = Uuid::new_v5(&habit_id, format!("daily:{local_date}:0").as_bytes());
    let nominal_start = local_date.and_hms_opt(8, 0, 0).expect("time").and_utc();
    HabitOccurrenceEvidence {
        id: evidence_id,
        habit_id,
        planner_occurrence_id,
        source_schedule_revision_id: Uuid::new_v4(),
        source_item_revision: 1,
        policy_fingerprint: habit_policy_fingerprint(habit_id, missed_policy),
        identity: json!({
            "type": "calendar_day",
            "date": local_date,
            "bucket_ordinal": 0
        }),
        nominal_start,
        nominal_end: nominal_start + ChronoDuration::minutes(30),
        window_start: nominal_start - ChronoDuration::hours(1),
        window_end: nominal_start + ChronoDuration::hours(1),
        local_date,
        timezone_name: "Europe/Paris".to_owned(),
        expected_duration_seconds: Some(1_800),
        expected_quantity: Some(20),
        expected_unit: Some("pages".to_owned()),
    }
}

fn habit_policy_fingerprint(habit_id: Uuid, missed_policy: &str) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(
            serde_json::to_vec(&habit_policy_projection(habit_id, missed_policy)).unwrap()
        )
    )
}

fn habit_policy_fingerprint_bytes(habit_id: Uuid, missed_policy: &str) -> [u8; 32] {
    Sha256::digest(serde_json::to_vec(&habit_policy_projection(habit_id, missed_policy)).unwrap())
        .into()
}

fn habit_policy_projection(habit_id: Uuid, missed_policy: &str) -> Value {
    json!({
        "schema":"dayweave-habit-policy/1",
        "habit_id":habit_id,
        "timezone_name":"Europe/Paris",
        "recurrence":{"type":"daily","times_per_day":1},
        "constraints":{
            "habit_target":{"amount":20,"unit":"pages"},
            "habit_missed_policy":missed_policy,
            "preserves_streak_when_paused":true
        },
        "duration":{
            "kind":"exact",
            "seconds":1800,
            "minimum_seconds":1800,
            "maximum_seconds":1800,
            "source":"user"
        },
        "split":{
            "allowed":false,
            "minimum_seconds":null,
            "maximum_seconds":null
        }
    })
}

fn outcome(
    operation_id: Uuid,
    expected_revision: u64,
    status: &str,
    progress_basis_points: u16,
    now: DateTime<Utc>,
) -> Value {
    let (quantity, unit, actual_seconds, note) = if status == "unresolved" {
        (Value::Null, Value::Null, Value::Null, Value::Null)
    } else {
        (json!(10), json!("pages"), json!(900), json!(PRIVATE_NOTE))
    };
    json!({
        "operation_id": operation_id,
        "expected_revision": expected_revision,
        "outcome": {
            "status": status,
            "progress_basis_points": progress_basis_points,
            "quantity": quantity,
            "unit": unit,
            "actual_seconds": actual_seconds,
            "note": note,
            "occurred_at": now,
        }
    })
}

fn postgres_now() -> DateTime<Utc> {
    Utc::now().with_nanosecond(0).expect("whole second")
}

#[tokio::test]
async fn missed_reconcile_requires_read_and_write_without_weakening_other_habit_writes() {
    let reconcile_request = || {
        request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-scope-reconcile-001"),
        )
    };

    let write_only = test_app_with_scopes(vec![Scope::ItemsWrite]);
    let forbidden = write_only
        .clone()
        .oneshot(reconcile_request())
        .await
        .expect("write-only reconcile response");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(forbidden).await["error"]["code"], "forbidden");

    let habit_id = Uuid::new_v4();
    let evidence_id = Uuid::new_v4();
    let neighboring_write = write_only
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}/missed-resolution"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "expected_revision": 1,
                "action": "skip"
            })),
            Some("missed-scope-resolve-001"),
        ))
        .await
        .expect("neighboring write-only habit mutation response");
    assert_eq!(neighboring_write.status(), StatusCode::NOT_FOUND);

    let read_only = test_app_with_scopes(vec![Scope::ItemsRead]);
    let forbidden = read_only
        .oneshot(reconcile_request())
        .await
        .expect("read-only reconcile response");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let combined = test_app_with_scopes(vec![Scope::ItemsWrite, Scope::ItemsRead]);
    let allowed = combined
        .oneshot(reconcile_request())
        .await
        .expect("read-write reconcile response");
    assert_eq!(allowed.status(), StatusCode::OK);
    let allowed = body_json(allowed).await;
    assert_eq!(allowed["resolutions"], json!([]));
    assert_eq!(allowed["has_more"], false);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn habit_api_is_authoritative_retryable_revisioned_private_and_analytics_safe() {
    let (app, repository) = test_app();
    let habit_id = Uuid::new_v4();
    let evidence_id = Uuid::new_v4();
    let create = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(habit_body(habit_id)),
            Some("habit-create-001"),
        ))
        .await
        .expect("item response");
    assert_eq!(
        create.status(),
        StatusCode::CREATED,
        "{:?}",
        body_json(create).await
    );

    let local_date = Utc::now().date_naive();
    let planner_occurrence_id = Uuid::new_v5(&habit_id, format!("daily:{local_date}:0").as_bytes());
    let nominal_start = local_date.and_hms_opt(8, 0, 0).expect("time").and_utc();
    repository
        .insert_authoritative_occurrence(HabitOccurrenceEvidence {
            id: evidence_id,
            habit_id,
            planner_occurrence_id,
            source_schedule_revision_id: Uuid::new_v4(),
            source_item_revision: 1,
            policy_fingerprint: format!("sha256:{}", "a".repeat(64)),
            identity: json!({
                "type": "calendar_day",
                "date": local_date,
                "bucket_ordinal": 0
            }),
            nominal_start,
            nominal_end: nominal_start + ChronoDuration::minutes(30),
            window_start: nominal_start - ChronoDuration::hours(1),
            window_end: nominal_start + ChronoDuration::hours(1),
            local_date,
            timezone_name: "Europe/Paris".to_owned(),
            expected_duration_seconds: Some(1800),
            expected_quantity: Some(20),
            expected_unit: Some("pages".to_owned()),
        })
        .await
        .expect("trusted evidence seed");

    let unknown = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{}", Uuid::new_v4()),
            Some(outcome(Uuid::new_v4(), 0, "partial", 5_000, postgres_now())),
            Some("habit-unknown-001"),
        ))
        .await
        .expect("unknown evidence response");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    // The scheduler occurrence UUID is a join key, never an HTTP write identity.
    let planner_id_write = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{planner_occurrence_id}"),
            Some(outcome(Uuid::new_v4(), 0, "partial", 5_000, postgres_now())),
            Some("habit-planner-id-001"),
        ))
        .await
        .expect("planner identity response");
    assert_eq!(planner_id_write.status(), StatusCode::NOT_FOUND);

    let operation_id = Uuid::new_v4();
    let partial_body = outcome(operation_id, 0, "partial", 5_000, postgres_now());
    let partial = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(partial_body.clone()),
            Some("habit-outcome-001"),
        ))
        .await
        .expect("partial response");
    assert_eq!(partial.status(), StatusCode::OK);
    assert_eq!(partial.headers()["idempotency-replayed"], "false");
    assert_eq!(
        partial.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let partial_json = body_json(partial).await;
    assert_eq!(
        partial_json["occurrence"]["evidence"]["id"],
        json!(evidence_id)
    );
    assert_eq!(
        partial_json["occurrence"]["evidence"]["planner_occurrence_id"],
        json!(planner_occurrence_id)
    );
    assert_eq!(partial_json["occurrence"]["outcome"]["revision"], 1);

    let replay = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(partial_body.clone()),
            Some("habit-outcome-001"),
        ))
        .await
        .expect("replay response");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
    assert_eq!(body_json(replay).await["replayed"], true);

    let reused_operation = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(outcome(
                operation_id,
                1,
                "completed",
                10_000,
                postgres_now(),
            )),
            Some("habit-outcome-different-key-001"),
        ))
        .await
        .expect("operation collision response");
    assert_eq!(reused_operation.status(), StatusCode::CONFLICT);

    let cross_route_operation = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses"),
            Some(json!({
                "operation_id": operation_id,
                "pause_id": Uuid::new_v4(),
                "expected_revision": 0,
                "started_at": postgres_now() - ChronoDuration::minutes(1),
            })),
            Some("habit-cross-operation-001"),
        ))
        .await
        .expect("cross-route operation collision response");
    assert_eq!(cross_route_operation.status(), StatusCode::CONFLICT);

    let completed = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(outcome(
                Uuid::new_v4(),
                1,
                "completed",
                10_000,
                postgres_now(),
            )),
            Some("habit-outcome-002"),
        ))
        .await
        .expect("correction response");
    assert_eq!(completed.status(), StatusCode::OK);
    let completed = body_json(completed).await;
    assert_eq!(completed["occurrence"]["outcome"]["revision"], 2);
    assert!(
        completed["occurrence"]
            .as_object()
            .is_some_and(|occurrence| occurrence.contains_key("missed_resolution"))
    );
    assert!(completed["occurrence"]["missed_resolution"].is_null());

    let stale = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(outcome(Uuid::new_v4(), 1, "skipped", 5_000, postgres_now())),
            Some("habit-outcome-stale-001"),
        ))
        .await
        .expect("stale response");
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale_json = body_json(stale).await;
    assert_eq!(stale_json["error"]["details"]["actual_revision"], 2);
    assert_eq!(
        stale_json["error"]["details"]["current_occurrence"]["outcome"]["note"],
        PRIVATE_NOTE
    );

    let list = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/v1/habits/{habit_id}/occurrences?start_date={local_date}&end_date={local_date}&limit=1"
            ),
            None,
            None,
        ))
        .await
        .expect("list response");
    assert_eq!(list.status(), StatusCode::OK);
    let list_json = body_json(list).await;
    assert_eq!(list_json["occurrences"].as_array().map(Vec::len), Some(1));
    assert!(
        list_json["occurrences"][0]
            .as_object()
            .is_some_and(|occurrence| occurrence.contains_key("missed_resolution"))
    );
    assert!(list_json["occurrences"][0]["missed_resolution"].is_null());
    assert_eq!(list_json["has_more"], false);

    let analytics = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/v1/habits/{habit_id}/analytics?start_date={local_date}&end_date={local_date}&bucket=day"
            ),
            None,
            None,
        ))
        .await
        .expect("analytics response");
    assert_eq!(analytics.status(), StatusCode::OK);
    let analytics_text = body_json(analytics).await;
    assert_eq!(analytics_text["analytics"]["completed"], 1);
    assert_eq!(
        analytics_text["analytics"]["adherence_basis_points"],
        10_000
    );
    assert!(!analytics_text.to_string().contains(PRIVATE_NOTE));

    let invalid_range = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/habits/{habit_id}/occurrences?start_date=1899-12-31&end_date=1899-12-31"),
            None,
            None,
        ))
        .await
        .expect("invalid date response");
    assert_eq!(invalid_range.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body_json(invalid_range).await["error"]["code"],
        "validation_failed"
    );

    let delta = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/habits/occurrences/delta?limit=200",
            None,
            None,
        ))
        .await
        .expect("delta response");
    assert_eq!(delta.status(), StatusCode::OK);
    let delta_json = body_json(delta).await;
    assert_eq!(delta_json["changes"].as_array().map(Vec::len), Some(3));
    for change in delta_json["changes"].as_array().expect("delta changes") {
        if let Some(occurrence) = change.get("occurrence") {
            assert!(
                occurrence
                    .as_object()
                    .is_some_and(|occurrence| occurrence.contains_key("missed_resolution")),
                "every occurrence delta must carry the independent missed-resolution coordinate"
            );
        }
    }
    let cursor = delta_json["next_cursor"]
        .as_str()
        .expect("opaque cursor")
        .to_owned();
    assert!(delta_json.to_string().contains(PRIVATE_NOTE));

    let mut stream_request = request("GET", "/v1/habits/stream", None, None);
    stream_request.headers_mut().insert(
        header::ACCEPT,
        HeaderValue::from_static("text/event-stream"),
    );
    let mut stream = app
        .clone()
        .oneshot(stream_request)
        .await
        .expect("stream response");
    assert_eq!(stream.status(), StatusCode::OK);
    let frame = timeout(Duration::from_secs(1), stream.body_mut().frame())
        .await
        .expect("immediate durable invalidation")
        .expect("stream remains open")
        .expect("valid stream frame")
        .into_data()
        .expect("data frame");
    let frame = String::from_utf8(frame.to_vec()).expect("utf8 sse");
    assert!(frame.contains("event: habit-invalidation"));
    assert!(frame.contains("\"cursor\""));
    assert!(!frame.contains(PRIVATE_NOTE));
    assert!(!frame.contains(&habit_id.to_string()));
    assert!(!frame.contains(&evidence_id.to_string()));

    let resumed_stream = {
        let mut request = request("GET", "/v1/habits/stream", None, None);
        request.headers_mut().insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        request.headers_mut().insert(
            "last-event-id",
            HeaderValue::from_str(&cursor).expect("cursor header"),
        );
        app.clone().oneshot(request).await.expect("resumed stream")
    };
    assert_eq!(resumed_stream.status(), StatusCode::OK);

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/v1/items/{habit_id}?expected_revision=1"),
            None,
            Some("habit-delete-001"),
        ))
        .await
        .expect("habit delete response");
    assert_eq!(deleted.status(), StatusCode::OK);
    let historical_replay = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(partial_body),
            Some("habit-outcome-001"),
        ))
        .await
        .expect("historical replay response");
    assert_eq!(historical_replay.status(), StatusCode::OK);
    assert_eq!(historical_replay.headers()["idempotency-replayed"], "true");
    assert_eq!(
        body_json(historical_replay).await["occurrence"]["outcome"]["revision"],
        1
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn missed_reconcile_is_bounded_policy_driven_retryable_and_ask_is_cas_resolved() {
    let (app, repository) = test_app();
    let policies = [
        ("skip", Uuid::new_v4()),
        ("carry", Uuid::new_v4()),
        ("reduce_frequency", Uuid::new_v4()),
        ("ask", Uuid::new_v4()),
        ("reduce_frequency", Uuid::new_v4()),
    ];
    for (index, (policy, habit_id)) in policies.iter().enumerate() {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(habit_body_with_missed_policy(*habit_id, policy)),
                Some(&format!("missed-item-create-{index:02}")),
            ))
            .await
            .expect("create habit");
        assert_eq!(response.status(), StatusCode::CREATED);
    }
    let old_date = (postgres_now() - ChronoDuration::days(2)).date_naive();
    let future_date = (postgres_now() + ChronoDuration::days(2)).date_naive();
    let mut evidence_ids = std::collections::BTreeMap::new();
    for (policy, habit_id) in policies {
        let evidence_id = Uuid::new_v4();
        repository
            .insert_authoritative_occurrence(occurrence_evidence(
                evidence_id,
                habit_id,
                old_date,
                policy,
            ))
            .await
            .expect("seed overdue occurrence");
        evidence_ids.insert(habit_id, evidence_id);
        if policy == "reduce_frequency" && habit_id == policies[2].1 {
            repository
                .insert_authoritative_occurrence(occurrence_evidence(
                    Uuid::new_v4(),
                    habit_id,
                    future_date,
                    policy,
                ))
                .await
                .expect("seed reduction target");
        }
    }

    let caller_clock = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=5&as_of=2030-01-01T00:00:00Z",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-reconcile-caller-clock"),
        ))
        .await
        .expect("caller-clock reconciliation rejection");
    assert_eq!(caller_clock.status(), StatusCode::BAD_REQUEST);

    let reconcile_operation_id = Uuid::new_v4();
    let reconcile_body = json!({"operation_id": reconcile_operation_id});
    let first = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=5",
            Some(reconcile_body.clone()),
            Some("missed-reconcile-001"),
        ))
        .await
        .expect("reconcile response");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    assert_eq!(first.headers()["idempotency-replayed"], "false");
    let first_json = body_json(first).await;
    assert_eq!(first_json["resolutions"].as_array().map(Vec::len), Some(5));
    assert_eq!(first_json["has_more"], false);
    assert!(!first_json.to_string().contains(PRIVATE_NOTE));
    let by_habit = first_json["resolutions"]
        .as_array()
        .expect("resolution array")
        .iter()
        .map(|resolution| {
            (
                Uuid::parse_str(resolution["habit_id"].as_str().expect("habit id"))
                    .expect("habit UUID"),
                resolution.clone(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(by_habit[&policies[0].1]["action"]["type"], "skip");
    assert_eq!(by_habit[&policies[1].1]["action"]["type"], "carry");
    assert_eq!(
        by_habit[&policies[2].1]["action"]["type"],
        "reduce_frequency"
    );
    assert_eq!(
        by_habit[&policies[3].1]["action"]["type"],
        "decision_required"
    );
    assert_eq!(
        by_habit[&policies[4].1]["action"]["type"],
        "reduction_pending"
    );
    assert_eq!(by_habit[&policies[4].1]["revision"], 1);

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=5",
            Some(reconcile_body),
            Some("missed-reconcile-001"),
        ))
        .await
        .expect("reconcile replay");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
    let replay_json = body_json(replay).await;
    assert_eq!(replay_json["resolutions"], first_json["resolutions"]);

    let reconcile_conflict = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id": reconcile_operation_id})),
            Some("missed-reconcile-001"),
        ))
        .await
        .expect("conflicting reconciliation retry");
    assert_eq!(reconcile_conflict.status(), StatusCode::CONFLICT);

    let pending_habit = policies[4].1;
    let pending_target = occurrence_evidence(
        Uuid::new_v4(),
        pending_habit,
        future_date,
        "reduce_frequency",
    );
    let pending_target_id = pending_target.planner_occurrence_id;
    repository
        .insert_authoritative_occurrence(pending_target)
        .await
        .expect("publish later reduction target");
    let bind = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-reconcile-002"),
        ))
        .await
        .expect("bind pending reduction");
    assert_eq!(bind.status(), StatusCode::OK);
    let bind_json = body_json(bind).await;
    assert_eq!(bind_json["resolutions"][0]["revision"], 2);
    assert_eq!(
        bind_json["resolutions"][0]["action"]["type"],
        "reduce_frequency"
    );
    assert_eq!(
        bind_json["resolutions"][0]["action"]["suppressed_planner_occurrence_ids"][0],
        pending_target_id.to_string()
    );

    let ask_habit = policies[3].1;
    let ask_evidence = evidence_ids[&ask_habit];
    let caller_window = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{ask_habit}/occurrences/{ask_evidence}/missed-resolution"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "expected_revision": 1,
                "action": "carry",
                "carry_window_start": "2030-01-01T00:00:00Z"
            })),
            Some("missed-resolve-caller-window"),
        ))
        .await
        .expect("caller carry-window rejection");
    assert_eq!(caller_window.status(), StatusCode::BAD_REQUEST);

    let resolve_operation_id = Uuid::new_v4();
    let resolve_body = json!({
        "operation_id": resolve_operation_id,
        "expected_revision": 1,
        "action": "carry"
    });
    let resolved = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{ask_habit}/occurrences/{ask_evidence}/missed-resolution"),
            Some(resolve_body.clone()),
            Some("missed-resolve-001"),
        ))
        .await
        .expect("resolve ask response");
    assert_eq!(resolved.status(), StatusCode::OK);
    assert_eq!(resolved.headers()["idempotency-replayed"], "false");
    let resolved_json = body_json(resolved).await;
    assert_eq!(resolved_json["resolution"]["revision"], 2);
    assert_eq!(resolved_json["resolution"]["action"]["type"], "carry");
    assert!(resolved_json["resolution"]["action"]["window_start"].is_string());
    assert!(resolved_json["resolution"]["action"]["window_end"].is_string());
    assert!(!resolved_json.to_string().contains(PRIVATE_NOTE));

    let resolve_replay = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{ask_habit}/occurrences/{ask_evidence}/missed-resolution"),
            Some(resolve_body),
            Some("missed-resolve-001"),
        ))
        .await
        .expect("resolve replay");
    assert_eq!(resolve_replay.status(), StatusCode::OK);
    assert_eq!(resolve_replay.headers()["idempotency-replayed"], "true");

    let conflicting_retry = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{ask_habit}/occurrences/{ask_evidence}/missed-resolution"),
            Some(json!({
                "operation_id": resolve_operation_id,
                "expected_revision": 1,
                "action": "skip"
            })),
            Some("missed-resolve-001"),
        ))
        .await
        .expect("conflicting retry");
    assert_eq!(conflicting_retry.status(), StatusCode::CONFLICT);

    let stale = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{ask_habit}/occurrences/{ask_evidence}/missed-resolution"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "expected_revision": 1,
                "action": "skip"
            })),
            Some("missed-resolve-stale"),
        ))
        .await
        .expect("stale resolve");
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(stale).await["error"]["details"]["actual_revision"],
        2
    );

    let delta = app
        .oneshot(request(
            "GET",
            "/v1/habits/occurrences/delta?limit=200",
            None,
            None,
        ))
        .await
        .expect("habit delta");
    assert_eq!(delta.status(), StatusCode::OK);
    let delta_json = body_json(delta).await;
    assert!(delta_json.to_string().contains("missed_resolution"));
    assert!(delta_json.to_string().contains("reduction_pending"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One bounded scan proves persistence, fairness, replay, and later binding.
async fn missed_reconcile_persists_ask_prompts_and_advances_one_bounded_page_at_a_time() {
    let (app, repository) = test_app();
    let old_date = (postgres_now() - ChronoDuration::days(2)).date_naive();
    let mut expected_evidence_ids = std::collections::BTreeSet::new();
    let mut habits_by_evidence = std::collections::BTreeMap::new();
    for index in 0..2 {
        let habit_id = Uuid::new_v4();
        let created = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(habit_body_with_missed_policy(habit_id, "ask")),
                Some(&format!("missed-ask-create-{index}")),
            ))
            .await
            .expect("create ask-policy habit");
        assert_eq!(created.status(), StatusCode::CREATED);
        let evidence_id = Uuid::new_v4();
        repository
            .insert_authoritative_occurrence(occurrence_evidence(
                evidence_id,
                habit_id,
                old_date,
                "ask",
            ))
            .await
            .expect("seed overdue ask occurrence");
        expected_evidence_ids.insert(evidence_id);
        habits_by_evidence.insert(evidence_id, habit_id);
    }

    let mut observed_evidence_ids = std::collections::BTreeSet::new();
    for page in 0..2 {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/habits/missed/reconcile?limit=1",
                Some(json!({"operation_id": Uuid::new_v4()})),
                Some(&format!("missed-ask-page-{page}")),
            ))
            .await
            .expect("reconcile bounded ask page");
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let resolutions = body["resolutions"].as_array().expect("resolution page");
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0]["action"]["type"], "decision_required");
        observed_evidence_ids.insert(
            Uuid::parse_str(
                resolutions[0]["occurrence_evidence_id"]
                    .as_str()
                    .expect("evidence id"),
            )
            .expect("evidence UUID"),
        );
        assert_eq!(body["has_more"], page == 0);
    }
    assert_eq!(observed_evidence_ids, expected_evidence_ids);

    let exhausted_operation_id = Uuid::new_v4();
    let exhausted = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id": exhausted_operation_id})),
            Some("missed-ask-page-exhausted"),
        ))
        .await
        .expect("reconcile exhausted ask scan");
    assert_eq!(exhausted.status(), StatusCode::OK);
    assert_eq!(exhausted.headers()["idempotency-replayed"], "false");
    let exhausted = body_json(exhausted).await;
    assert_eq!(exhausted["resolutions"].as_array().map(Vec::len), Some(0));
    assert_eq!(exhausted["has_more"], false);

    let late_habit_id = habits_by_evidence[expected_evidence_ids
        .iter()
        .next()
        .expect("late habit evidence")];
    let late_evidence_id = Uuid::new_v4();
    repository
        .insert_authoritative_occurrence(occurrence_evidence(
            late_evidence_id,
            late_habit_id,
            old_date.pred_opt().expect("earlier overdue date"),
            "ask",
        ))
        .await
        .expect("admit evidence after a lost empty response");

    // Empty scans use bounded, expiring receipts: a lost response retries
    // exactly even when new work becomes eligible during the retry window.
    let exhausted_retry = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id": exhausted_operation_id})),
            Some("missed-ask-page-exhausted"),
        ))
        .await
        .expect("retry exhausted ask scan");
    assert_eq!(exhausted_retry.status(), StatusCode::OK);
    assert_eq!(exhausted_retry.headers()["idempotency-replayed"], "true");
    let exhausted_retry = body_json(exhausted_retry).await;
    assert_eq!(
        exhausted_retry["resolutions"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(exhausted_retry["has_more"], false);

    let fresh_after_empty = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-ask-page-after-empty"),
        ))
        .await
        .expect("scan newly eligible evidence with a fresh key");
    assert_eq!(fresh_after_empty.status(), StatusCode::OK);
    assert_eq!(fresh_after_empty.headers()["idempotency-replayed"], "false");
    let fresh_after_empty = body_json(fresh_after_empty).await;
    assert_eq!(
        fresh_after_empty["resolutions"][0]["occurrence_evidence_id"],
        late_evidence_id.to_string()
    );

    let ask_evidence_id = *expected_evidence_ids.iter().next().expect("ask evidence");
    let ask_habit_id = habits_by_evidence[&ask_evidence_id];
    let pending = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{ask_habit_id}/occurrences/{ask_evidence_id}/missed-resolution"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "expected_revision": 1,
                "action": "reduce_frequency"
            })),
            Some("missed-ask-reduce-pending"),
        ))
        .await
        .expect("resolve ask to pending reduction");
    assert_eq!(pending.status(), StatusCode::OK);
    let pending = body_json(pending).await;
    assert_eq!(pending["resolution"]["revision"], 2);
    assert_eq!(pending["resolution"]["action"]["type"], "reduction_pending");

    let pause_base = postgres_now();
    let paused_nominal_start = pause_base + ChronoDuration::minutes(2);
    let paused_local_date = paused_nominal_start
        .with_timezone(&chrono_tz::Europe::Paris)
        .date_naive();
    let mut paused_future =
        occurrence_evidence(Uuid::new_v4(), ask_habit_id, paused_local_date, "ask");
    paused_future.nominal_start = paused_nominal_start;
    paused_future.nominal_end = paused_nominal_start + ChronoDuration::minutes(1);
    paused_future.window_start = pause_base + ChronoDuration::minutes(1);
    paused_future.window_end = pause_base + ChronoDuration::minutes(4);
    let paused_planner_id = paused_future.planner_occurrence_id;
    repository
        .insert_authoritative_occurrence(paused_future)
        .await
        .expect("admit paused reduction target");

    let pause_id = Uuid::new_v4();
    let paused = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{ask_habit_id}/pauses"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "pause_id": pause_id,
                "expected_revision": 0,
                "started_at": pause_base - ChronoDuration::minutes(1),
            })),
            Some("missed-ask-target-pause"),
        ))
        .await
        .expect("pause reduction target");
    assert_eq!(paused.status(), StatusCode::OK);
    let resumed = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{ask_habit_id}/pauses/{pause_id}/resume"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "expected_revision": 1,
                "ended_at": pause_base + ChronoDuration::minutes(4),
            })),
            Some("missed-ask-target-resume"),
        ))
        .await
        .expect("bound target pause");
    assert_eq!(resumed.status(), StatusCode::OK);

    let future = occurrence_evidence(
        Uuid::new_v4(),
        ask_habit_id,
        paused_local_date.succ_opt().expect("later local date"),
        "ask",
    );
    let future_planner_id = future.planner_occurrence_id;
    repository
        .insert_authoritative_occurrence(future)
        .await
        .expect("admit later ask-policy reduction target");
    let bound = app
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-ask-reduce-bound"),
        ))
        .await
        .expect("bind ask-policy pending reduction");
    assert_eq!(bound.status(), StatusCode::OK);
    let bound = body_json(bound).await;
    assert!(bound["resolutions"].as_array().unwrap().is_empty());
    assert_eq!(bound["has_more"], false);
    assert_ne!(future_planner_id, paused_planner_id);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn missed_reconcile_cancels_obsolete_policy_and_never_admits_stale_fresh_evidence() {
    let (app, repository) = test_app();
    let habit_id = Uuid::new_v4();
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(habit_body_with_missed_policy(habit_id, "ask")),
            Some("missed-obsolete-create"),
        ))
        .await
        .expect("create ask habit");
    assert_eq!(created.status(), StatusCode::CREATED);
    let old_date = (postgres_now() - ChronoDuration::days(3)).date_naive();
    let prompted_id = Uuid::new_v4();
    let stale_fresh_id = Uuid::new_v4();
    repository
        .insert_authoritative_occurrence(occurrence_evidence(
            prompted_id,
            habit_id,
            old_date,
            "ask",
        ))
        .await
        .expect("seed prompt source");
    repository
        .insert_authoritative_occurrence(occurrence_evidence(
            stale_fresh_id,
            habit_id,
            old_date.succ_opt().expect("next date"),
            "ask",
        ))
        .await
        .expect("seed later stale source");
    let prompted = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-obsolete-prompt"),
        ))
        .await
        .expect("create decision prompt");
    let prompted = body_json(prompted).await;
    assert_eq!(
        prompted["resolutions"][0]["occurrence_evidence_id"],
        prompted_id.to_string()
    );

    let mut replacement = habit_body_with_missed_policy(habit_id, "skip");
    replacement.as_object_mut().unwrap().remove("id");
    let edited = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{habit_id}"),
            Some(json!({"expected_revision":1,"item":replacement})),
            Some("missed-obsolete-edit"),
        ))
        .await
        .expect("edit habit policy");
    assert_eq!(edited.status(), StatusCode::OK);
    let cancelled = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-obsolete-cancel"),
        ))
        .await
        .expect("cancel stale prompt");
    let cancelled = body_json(cancelled).await;
    assert_eq!(cancelled["resolutions"].as_array().map(Vec::len), Some(1));
    assert_eq!(cancelled["resolutions"][0]["action"]["type"], "cancelled");
    assert_eq!(
        cancelled["resolutions"][0]["action"]["reason"],
        "source_obsolete"
    );
    assert_eq!(
        cancelled["resolutions"][0]["action"]["resume_action"],
        "decision_required"
    );
    let stale_fresh = repository
        .list_occurrences(
            habit_id,
            old_date.succ_opt().unwrap(),
            old_date.succ_opt().unwrap(),
            None,
            10,
        )
        .await
        .expect("inspect stale fresh evidence")
        .0
        .pop()
        .expect("stale fresh evidence");
    assert_eq!(stale_fresh.evidence.id, stale_fresh_id);
    assert!(stale_fresh.missed_resolution.is_none());

    let mut restored_item = habit_body_with_missed_policy(habit_id, "ask");
    restored_item.as_object_mut().unwrap().remove("id");
    let restored_edit = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{habit_id}"),
            Some(json!({"expected_revision":2,"item":restored_item})),
            Some("missed-obsolete-restore-edit"),
        ))
        .await
        .expect("restore original policy");
    assert_eq!(restored_edit.status(), StatusCode::OK);
    let restored = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-obsolete-restore"),
        ))
        .await
        .expect("restore prompt after correction");
    let restored = body_json(restored).await;
    assert_eq!(restored["resolutions"][0]["revision"], 3);
    assert_eq!(
        restored["resolutions"][0]["action"]["type"],
        "decision_required"
    );

    let mut completed_item = habit_body_with_missed_policy(habit_id, "ask");
    completed_item.as_object_mut().unwrap().remove("id");
    completed_item["status"] = json!("completed");
    let completed = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{habit_id}"),
            Some(json!({"expected_revision":3,"item":completed_item})),
            Some("missed-inactive-complete"),
        ))
        .await
        .expect("complete prompted habit");
    assert_eq!(completed.status(), StatusCode::OK);
    let inactive = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id":Uuid::new_v4()})),
            Some("missed-inactive-cancel"),
        ))
        .await
        .expect("cancel action for inactive item");
    let inactive = body_json(inactive).await;
    assert_eq!(inactive["resolutions"][0]["revision"], 4);
    assert_eq!(inactive["resolutions"][0]["action"]["type"], "cancelled");
    assert_eq!(
        inactive["resolutions"][0]["action"]["reason"],
        "source_obsolete"
    );

    let mut active_item = habit_body_with_missed_policy(habit_id, "ask");
    active_item.as_object_mut().unwrap().remove("id");
    let activated = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{habit_id}"),
            Some(json!({"expected_revision":4,"item":active_item})),
            Some("missed-inactive-reactivate"),
        ))
        .await
        .expect("reactivate prompted habit");
    assert_eq!(activated.status(), StatusCode::OK);
    let active = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=1",
            Some(json!({"operation_id":Uuid::new_v4()})),
            Some("missed-inactive-restore"),
        ))
        .await
        .expect("restore action after item reactivation");
    let active = body_json(active).await;
    assert_eq!(active["resolutions"][0]["revision"], 5);
    assert_eq!(
        active["resolutions"][0]["action"]["type"],
        "decision_required"
    );

    let child_id = Uuid::new_v4();
    let mut child = habit_body(child_id);
    child["kind"] = json!("task");
    child["title"] = json!("Child makes habit a container");
    child["recurrence"] = Value::Null;
    child["flexible_constraints"] = json!({});
    child["parent_id"] = json!(habit_id);
    let child_created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(child),
            Some("missed-nonleaf-child-create"),
        ))
        .await
        .expect("make prompted habit non-leaf");
    assert_eq!(child_created.status(), StatusCode::CREATED);

    let nonleaf_race = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{prompted_id}/missed-resolution"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "expected_revision": 5,
                "action": "carry"
            })),
            Some("missed-nonleaf-explicit-race"),
        ))
        .await
        .expect("resolve prompt after habit became non-leaf");
    assert_eq!(nonleaf_race.status(), StatusCode::OK);
    let nonleaf_race = body_json(nonleaf_race).await;
    assert_eq!(nonleaf_race["resolution"]["revision"], 6);
    assert_eq!(nonleaf_race["resolution"]["action"]["type"], "cancelled");
    assert_eq!(
        nonleaf_race["resolution"]["action"]["reason"],
        "source_obsolete"
    );
    assert_eq!(
        nonleaf_race["resolution"]["action"]["resume_action"],
        "carry"
    );

    let nonleaf_fresh_id = Uuid::new_v4();
    repository
        .insert_authoritative_occurrence(occurrence_evidence(
            nonleaf_fresh_id,
            habit_id,
            old_date.pred_opt().expect("non-leaf fresh date"),
            "ask",
        ))
        .await
        .expect("seed overdue evidence while habit is non-leaf");
    let nonleaf_scan = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-nonleaf-no-fresh"),
        ))
        .await
        .expect("scan non-leaf habit");
    let nonleaf_scan = body_json(nonleaf_scan).await;
    assert!(nonleaf_scan["resolutions"].as_array().unwrap().is_empty());

    let child_deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/v1/items/{child_id}?expected_revision=1"),
            None,
            Some("missed-nonleaf-child-delete"),
        ))
        .await
        .expect("make habit a leaf again");
    assert_eq!(child_deleted.status(), StatusCode::OK);
    let leaf_scan = app
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-leaf-restore"),
        ))
        .await
        .expect("restore leaf scheduling actions");
    let leaf_scan = body_json(leaf_scan).await;
    let leaf_resolutions = leaf_scan["resolutions"].as_array().unwrap();
    assert!(leaf_resolutions.iter().any(|resolution| {
        resolution["occurrence_evidence_id"] == prompted_id.to_string()
            && resolution["revision"] == 7
            && resolution["action"]["type"] == "carry"
    }));
    assert!(leaf_resolutions.iter().any(|resolution| {
        resolution["occurrence_evidence_id"] == nonleaf_fresh_id.to_string()
            && resolution["revision"] == 1
            && resolution["action"]["type"] == "decision_required"
    }));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn missed_actions_cancel_restore_and_rebind_without_leaking_partial_evidence() {
    let (app, repository) = test_app();
    let old_date = (postgres_now() - ChronoDuration::days(2)).date_naive();
    let future_date = (postgres_now() + ChronoDuration::days(2)).date_naive();

    let skip_habit = Uuid::new_v4();
    let skip_evidence = Uuid::new_v4();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(habit_body_with_missed_policy(skip_habit, "skip")),
                Some("missed-correction-skip-create"),
            ))
            .await
            .expect("create skip habit")
            .status(),
        StatusCode::CREATED
    );
    repository
        .insert_authoritative_occurrence(occurrence_evidence(
            skip_evidence,
            skip_habit,
            old_date,
            "skip",
        ))
        .await
        .expect("seed skip occurrence");
    let partial = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{skip_habit}/occurrences/{skip_evidence}"),
            Some(outcome(Uuid::new_v4(), 0, "partial", 2_500, postgres_now())),
            Some("missed-correction-source-partial"),
        ))
        .await
        .expect("record partial source");
    assert_eq!(partial.status(), StatusCode::OK);
    let skipped = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-correction-source-skip"),
        ))
        .await
        .expect("reconcile partial source");
    assert_eq!(skipped.status(), StatusCode::OK);
    let skipped = body_json(skipped).await;
    assert_eq!(skipped["resolutions"][0]["action"]["type"], "skip");
    assert!(
        !skipped.to_string().contains(PRIVATE_NOTE),
        "reconcile receipts expose scheduling decisions, never private outcome evidence"
    );

    let completed = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{skip_habit}/occurrences/{skip_evidence}"),
            Some(outcome(
                Uuid::new_v4(),
                1,
                "completed",
                10_000,
                postgres_now(),
            )),
            Some("missed-correction-source-completed"),
        ))
        .await
        .expect("complete source late");
    assert_eq!(completed.status(), StatusCode::OK);
    let cancelled = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-correction-source-cancel"),
        ))
        .await
        .expect("cancel obsolete skip");
    let cancelled = body_json(cancelled).await;
    assert_eq!(cancelled["resolutions"][0]["revision"], 2);
    assert_eq!(cancelled["resolutions"][0]["action"]["type"], "cancelled");
    assert_eq!(
        cancelled["resolutions"][0]["action"]["reason"],
        "source_completed"
    );
    assert_eq!(
        cancelled["resolutions"][0]["action"]["resume_action"],
        "skip"
    );

    let unresolved = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{skip_habit}/occurrences/{skip_evidence}"),
            Some(outcome(Uuid::new_v4(), 2, "unresolved", 0, postgres_now())),
            Some("missed-correction-source-unresolved"),
        ))
        .await
        .expect("correct source back to unresolved");
    assert_eq!(unresolved.status(), StatusCode::OK);
    let restored = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-correction-source-restore"),
        ))
        .await
        .expect("restore configured skip");
    let restored = body_json(restored).await;
    assert_eq!(restored["resolutions"][0]["revision"], 3);
    assert_eq!(restored["resolutions"][0]["action"]["type"], "skip");

    let reduce_habit = Uuid::new_v4();
    let reduce_source = Uuid::new_v4();
    let reduce_target = Uuid::new_v4();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(habit_body_with_missed_policy(
                    reduce_habit,
                    "reduce_frequency",
                )),
                Some("missed-correction-reduce-create"),
            ))
            .await
            .expect("create reduce habit")
            .status(),
        StatusCode::CREATED
    );
    repository
        .insert_authoritative_occurrence(occurrence_evidence(
            reduce_source,
            reduce_habit,
            old_date,
            "reduce_frequency",
        ))
        .await
        .expect("seed reduce source");
    let target_evidence =
        occurrence_evidence(reduce_target, reduce_habit, future_date, "reduce_frequency");
    let target_planner_id = target_evidence.planner_occurrence_id;
    repository
        .insert_authoritative_occurrence(target_evidence)
        .await
        .expect("seed reduce target");
    let later_target_evidence = occurrence_evidence(
        Uuid::new_v4(),
        reduce_habit,
        future_date.succ_opt().expect("later target date"),
        "reduce_frequency",
    );
    let later_target_planner_id = later_target_evidence.planner_occurrence_id;
    repository
        .insert_authoritative_occurrence(later_target_evidence)
        .await
        .expect("seed later eligible reduce target");
    let bound = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-correction-reduce-bind"),
        ))
        .await
        .expect("bind reduction");
    let bound = body_json(bound).await;
    assert_eq!(
        bound["resolutions"][0]["action"]["type"],
        "reduce_frequency"
    );
    assert_eq!(
        bound["resolutions"][0]["action"]["suppressed_planner_occurrence_ids"][0],
        target_planner_id.to_string()
    );
    let target_partial = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{reduce_habit}/occurrences/{reduce_target}"),
            Some(outcome(Uuid::new_v4(), 0, "partial", 5_000, postgres_now())),
            Some("missed-correction-target-partial"),
        ))
        .await
        .expect("record target partial");
    assert_eq!(target_partial.status(), StatusCode::OK);
    let repended = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-correction-reduce-repend"),
        ))
        .await
        .expect("re-pend reduction with partial target");
    let repended = body_json(repended).await;
    assert_eq!(repended["resolutions"][0]["revision"], 2);
    assert_eq!(
        repended["resolutions"][0]["action"]["type"],
        "reduction_pending"
    );
    let still_pending = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-correction-reduce-exact-target"),
        ))
        .await
        .expect("retain pending reduction when its exact next target is partial");
    let still_pending = body_json(still_pending).await;
    assert!(still_pending["resolutions"].as_array().unwrap().is_empty());
    assert_eq!(still_pending["has_more"], false);
    assert_ne!(
        later_target_planner_id, target_planner_id,
        "the later eligible occurrence must not replace the ineligible immediate target"
    );

    let ask_habit = Uuid::new_v4();
    let ask_evidence = Uuid::new_v4();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(habit_body_with_missed_policy(ask_habit, "ask")),
                Some("missed-race-ask-create"),
            ))
            .await
            .expect("create ask habit")
            .status(),
        StatusCode::CREATED
    );
    repository
        .insert_authoritative_occurrence(occurrence_evidence(
            ask_evidence,
            ask_habit,
            old_date,
            "ask",
        ))
        .await
        .expect("seed ask source");
    let prompt = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/habits/missed/reconcile?limit=10",
            Some(json!({"operation_id": Uuid::new_v4()})),
            Some("missed-race-ask-prompt"),
        ))
        .await
        .expect("create ask prompt");
    assert_eq!(prompt.status(), StatusCode::OK);
    let late_completion = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{ask_habit}/occurrences/{ask_evidence}"),
            Some(outcome(
                Uuid::new_v4(),
                0,
                "completed",
                10_000,
                postgres_now(),
            )),
            Some("missed-race-ask-complete"),
        ))
        .await
        .expect("complete prompted source");
    assert_eq!(late_completion.status(), StatusCode::OK);
    let race_operation = Uuid::new_v4();
    let race_body = json!({
        "operation_id": race_operation,
        "expected_revision": 1,
        "action": "carry"
    });
    let race = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{ask_habit}/occurrences/{ask_evidence}/missed-resolution"),
            Some(race_body.clone()),
            Some("missed-race-explicit-carry"),
        ))
        .await
        .expect("resolve raced prompt");
    assert_eq!(race.status(), StatusCode::OK);
    let race = body_json(race).await;
    assert_eq!(race["resolution"]["action"]["type"], "cancelled");
    assert_eq!(race["resolution"]["action"]["resume_action"], "carry");
    let replay = app
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{ask_habit}/occurrences/{ask_evidence}/missed-resolution"),
            Some(race_body),
            Some("missed-race-explicit-carry"),
        ))
        .await
        .expect("replay raced prompt");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
}

#[tokio::test]
async fn reduction_target_does_not_become_a_second_missed_occurrence_after_expiry() {
    let (app, repository) = test_app();
    let habit_id = Uuid::new_v4();
    let create = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(habit_body_with_missed_policy(habit_id, "reduce_frequency")),
            Some("missed-target-analytics-create"),
        ))
        .await
        .expect("create reduction habit");
    assert_eq!(create.status(), StatusCode::CREATED);
    let source_date = NaiveDate::from_ymd_opt(2026, 9, 1).expect("source date");
    let target_date = source_date.succ_opt().expect("target date");
    let source = occurrence_evidence(Uuid::new_v4(), habit_id, source_date, "reduce_frequency");
    let target = occurrence_evidence(Uuid::new_v4(), habit_id, target_date, "reduce_frequency");
    let target_evidence_id = target.id;
    let target_planner_id = target.planner_occurrence_id;
    let first_clock = source.window_end + ChronoDuration::minutes(1);
    let second_clock = target.window_end + ChronoDuration::minutes(1);
    repository
        .insert_authoritative_occurrence(source)
        .await
        .expect("insert missed source");
    repository
        .insert_authoritative_occurrence(target)
        .await
        .expect("insert future target");
    let policies = std::collections::BTreeMap::from([(
        habit_id,
        HabitMissedConfiguration {
            item_revision: 1,
            policy_fingerprint: habit_policy_fingerprint_bytes(habit_id, "reduce_frequency"),
            policy: HabitMissedPolicy::ReduceFrequency,
            is_active: true,
        },
    )]);
    let first = repository
        .reconcile_missed(MissedReconcileWrite {
            policies: policies.clone(),
            limit: 10,
            recorded_at: first_clock,
            idempotency: habit_idempotency("test.missed.target-expiry.first", 0x31, 0x32),
        })
        .await
        .expect("bind future reduction target");
    assert!(matches!(
        first.value.resolutions.as_slice(),
        [resolution]
            if matches!(
                &resolution.action,
                HabitMissedResolutionAction::ReduceFrequency {
                    suppressed_planner_occurrence_ids,
                } if suppressed_planner_occurrence_ids == &[target_planner_id]
            )
    ));

    let second = repository
        .reconcile_missed(MissedReconcileWrite {
            policies,
            limit: 10,
            recorded_at: second_clock,
            idempotency: habit_idempotency("test.missed.target-expiry.second", 0x33, 0x34),
        })
        .await
        .expect("reconcile after suppressed target expires");
    assert!(second.value.resolutions.is_empty());
    assert!(!second.value.has_more);
    let target = repository
        .list_occurrences(habit_id, target_date, target_date, None, 10)
        .await
        .expect("list target")
        .0
        .into_iter()
        .find(|occurrence| occurrence.evidence.id == target_evidence_id)
        .expect("target occurrence");
    assert!(target.missed_resolution.is_none());

    let analytics = analytics_for_date(app.clone(), habit_id, target_date).await;
    assert_eq!(analytics["analytics"]["expected"], 0);
    assert_eq!(analytics["analytics"]["eligible"], 0);
    assert_eq!(analytics["analytics"]["missed"], 0);
    assert!(
        analytics["analytics"]["trends"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    replace_missed_policy(
        app.clone(),
        habit_id,
        "skip",
        "missed-target-analytics-policy-edit",
    )
    .await;
    let stale_projection_analytics = analytics_for_date(app, habit_id, target_date).await;
    assert_eq!(stale_projection_analytics["analytics"]["expected"], 1);
    assert_eq!(stale_projection_analytics["analytics"]["eligible"], 1);
    assert_eq!(stale_projection_analytics["analytics"]["missed"], 1);
}

#[tokio::test]
async fn analytics_fetches_preserving_pauses_over_a_carried_window() {
    let habit_id = Uuid::new_v4();
    let source_date = NaiveDate::from_ymd_opt(2026, 9, 1).expect("source date");
    let evidence = occurrence_evidence(Uuid::new_v4(), habit_id, source_date, "carry");
    let reconcile_at = evidence.window_end + ChronoDuration::days(1);
    let analytics_now = reconcile_at + ChronoDuration::minutes(30);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(analytics_now));
    let items = Arc::new(ItemService::new(
        Arc::new(InMemoryItemRepository::default()),
        clock.clone(),
    ));
    let item: NewItem = serde_json::from_value(habit_body_with_missed_policy(habit_id, "carry"))
        .expect("decode carry habit");
    items
        .create(
            item,
            ItemIdempotencyKey {
                key: "carry-analytics-item".to_owned(),
                fingerprint: [0x41; 32],
            },
        )
        .await
        .expect("create carry habit");
    let repository = Arc::new(InMemoryHabitRepository::default());
    repository
        .insert_authoritative_occurrence(evidence)
        .await
        .expect("insert carried evidence");
    let reconciled = repository
        .reconcile_missed(MissedReconcileWrite {
            policies: std::collections::BTreeMap::from([(
                habit_id,
                HabitMissedConfiguration {
                    item_revision: 1,
                    policy_fingerprint: habit_policy_fingerprint_bytes(habit_id, "carry"),
                    policy: HabitMissedPolicy::Carry,
                    is_active: true,
                },
            )]),
            limit: 1,
            recorded_at: reconcile_at,
            idempotency: HabitIdempotency {
                namespace: "test.analytics.carry.reconcile",
                key_hash: [0x42; 32],
                request_fingerprint: [0x43; 32],
                operation_id: Uuid::new_v4(),
                actor_session_id: None,
            },
        })
        .await
        .expect("derive carry window");
    let (carry_start, carry_end) = match &reconciled.value.resolutions[0].action {
        HabitMissedResolutionAction::Carry {
            window_start,
            window_end,
        } => (*window_start, *window_end),
        action => panic!("expected carry action, got {action:?}"),
    };
    assert!(
        carry_start
            > source_date
                .succ_opt()
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
    );

    let pause_start = carry_start + ChronoDuration::minutes(5);
    let pause_end = pause_start + ChronoDuration::minutes(5);
    round_trip_preserving_pause(repository.as_ref(), habit_id, pause_start, pause_end).await;
    assert!(analytics_now < carry_end);

    let service = HabitService::new(repository, items, clock);
    let analytics = service
        .analytics(
            habit_id,
            source_date,
            source_date,
            HabitAnalyticsBucket::Day,
        )
        .await
        .expect("analytics over carried occurrence");
    assert_eq!(analytics.totals.expected, 1);
    assert_eq!(analytics.totals.excused, 1);
    assert_eq!(analytics.totals.eligible, 0);
    assert_eq!(analytics.totals.unresolved, 0);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Covers both terminal and retroactive-pause suppressor races end to end.
async fn inactive_reduction_sources_do_not_reserve_targets_before_maintenance() {
    for (case_index, source_is_completed) in [true, false].into_iter().enumerate() {
        let repository = InMemoryHabitRepository::default();
        let habit_id = Uuid::new_v4();
        let source_date = NaiveDate::from_ymd_opt(2026, 9, 1).expect("source date");
        let prompted_date = source_date.succ_opt().expect("prompted date");
        let target_date = prompted_date.succ_opt().expect("target date");
        let later_date = target_date.succ_opt().expect("later date");
        let source = occurrence_evidence(Uuid::new_v4(), habit_id, source_date, "ask");
        let prompted = occurrence_evidence(Uuid::new_v4(), habit_id, prompted_date, "ask");
        let target = occurrence_evidence(Uuid::new_v4(), habit_id, target_date, "ask");
        let later = occurrence_evidence(Uuid::new_v4(), habit_id, later_date, "ask");
        let source_evidence_id = source.id;
        let prompted_evidence_id = prompted.id;
        let target_planner_id = target.planner_occurrence_id;
        let decision_time = prompted.window_end + ChronoDuration::minutes(1);
        let source_window_start = source.window_start;
        let source_window_end = source.window_end;
        for evidence in [source, prompted, target, later] {
            repository
                .insert_authoritative_occurrence(evidence)
                .await
                .expect("insert reduction precedence evidence");
        }
        let fingerprint = habit_policy_fingerprint_bytes(habit_id, "ask");
        let policies = std::collections::BTreeMap::from([(
            habit_id,
            HabitMissedConfiguration {
                item_revision: 1,
                policy_fingerprint: fingerprint,
                policy: HabitMissedPolicy::Ask,
                is_active: true,
            },
        )]);
        let prompted_result = repository
            .reconcile_missed(MissedReconcileWrite {
                policies: policies.clone(),
                limit: 10,
                recorded_at: decision_time,
                idempotency: HabitIdempotency {
                    namespace: "test.missed.inactive-suppressor.prompt",
                    key_hash: [u8::try_from(0x40 + case_index).unwrap(); 32],
                    request_fingerprint: [u8::try_from(0x50 + case_index).unwrap(); 32],
                    operation_id: Uuid::new_v4(),
                    actor_session_id: None,
                },
            })
            .await
            .expect("prompt both overdue sources");
        assert_eq!(prompted_result.value.resolutions.len(), 2);
        let source_reduction = repository
            .resolve_missed(MissedResolveWrite {
                habit_id,
                occurrence_id: source_evidence_id,
                expected_revision: 1,
                action: HabitMissedExplicitAction::ReduceFrequency,
                current_item_revision: 1,
                current_policy_fingerprint: fingerprint,
                current_item_is_active: true,
                recorded_at: decision_time,
                idempotency: HabitIdempotency {
                    namespace: "test.missed.inactive-suppressor.source",
                    key_hash: [u8::try_from(0x60 + case_index).unwrap(); 32],
                    request_fingerprint: [u8::try_from(0x70 + case_index).unwrap(); 32],
                    operation_id: Uuid::new_v4(),
                    actor_session_id: None,
                },
            })
            .await
            .expect("resolve the first source without skipping its expired immediate target");
        assert!(matches!(
            &source_reduction.value.action,
            HabitMissedResolutionAction::ReductionPending
        ));

        if source_is_completed {
            repository
                .put_outcome(OutcomeWrite {
                    habit_id,
                    occurrence_id: source_evidence_id,
                    expected_revision: 0,
                    outcome: HabitOutcomeInput {
                        status: HabitOutcomeStatus::Completed,
                        progress_basis_points: 10_000,
                        quantity: Some(20),
                        unit: Some("pages".to_owned()),
                        actual_seconds: Some(1_800),
                        note: None,
                        occurred_at: decision_time,
                    },
                    recorded_at: decision_time,
                    idempotency: HabitIdempotency {
                        namespace: "test.missed.inactive-suppressor.complete",
                        key_hash: [0x80; 32],
                        request_fingerprint: [u8::try_from(0x81 + case_index).unwrap(); 32],
                        operation_id: Uuid::new_v4(),
                        actor_session_id: None,
                    },
                })
                .await
                .expect("complete reduction source");
        } else {
            let pause_id = Uuid::new_v4();
            repository
                .create_pause(PauseCreate {
                    id: pause_id,
                    habit_id,
                    expected_revision: 0,
                    started_at: source_window_start,
                    preserves_streak: true,
                    recorded_at: source_window_end,
                    idempotency: HabitIdempotency {
                        namespace: "test.missed.inactive-suppressor.pause",
                        key_hash: [0x82; 32],
                        request_fingerprint: [0x83; 32],
                        operation_id: Uuid::new_v4(),
                        actor_session_id: None,
                    },
                })
                .await
                .expect("pause reduction source retroactively");
            repository
                .resume_pause(PauseResume {
                    id: pause_id,
                    habit_id,
                    expected_revision: 1,
                    ended_at: source_window_end,
                    recorded_at: source_window_end,
                    idempotency: HabitIdempotency {
                        namespace: "test.missed.inactive-suppressor.resume",
                        key_hash: [0x84; 32],
                        request_fingerprint: [0x85; 32],
                        operation_id: Uuid::new_v4(),
                        actor_session_id: None,
                    },
                })
                .await
                .expect("bound retroactive source pause");
        }

        let rebound = repository
            .resolve_missed(MissedResolveWrite {
                habit_id,
                occurrence_id: prompted_evidence_id,
                expected_revision: 1,
                action: HabitMissedExplicitAction::ReduceFrequency,
                current_item_revision: 1,
                current_policy_fingerprint: fingerprint,
                current_item_is_active: true,
                recorded_at: decision_time + ChronoDuration::seconds(1),
                idempotency: HabitIdempotency {
                    namespace: "test.missed.inactive-suppressor.rebind",
                    key_hash: [u8::try_from(0x90 + case_index).unwrap(); 32],
                    request_fingerprint: [u8::try_from(0xa0 + case_index).unwrap(); 32],
                    operation_id: Uuid::new_v4(),
                    actor_session_id: None,
                },
            })
            .await
            .expect("bind the second source to its exact target");
        assert!(matches!(
            &rebound.value.action,
            HabitMissedResolutionAction::ReduceFrequency {
                suppressed_planner_occurrence_ids,
            } if suppressed_planner_occurrence_ids == &[target_planner_id]
        ));
        let maintained = repository
            .reconcile_missed(MissedReconcileWrite {
                policies,
                limit: 10,
                recorded_at: decision_time + ChronoDuration::seconds(2),
                idempotency: HabitIdempotency {
                    namespace: "test.missed.inactive-suppressor.maintenance",
                    key_hash: [u8::try_from(0xb0 + case_index).unwrap(); 32],
                    request_fingerprint: [u8::try_from(0xc0 + case_index).unwrap(); 32],
                    operation_id: Uuid::new_v4(),
                    actor_session_id: None,
                },
            })
            .await
            .expect("cancel the inactive pending source");
        assert!(maintained.value.resolutions.iter().any(|resolution| {
            resolution.occurrence_evidence_id == source_evidence_id
                && matches!(
                    resolution.action,
                    HabitMissedResolutionAction::Cancelled { .. }
                )
        }));
        let prompted_projection = repository
            .list_occurrences(habit_id, prompted_date, prompted_date, None, 10)
            .await
            .expect("read exact target binding after maintenance")
            .0
            .into_iter()
            .find(|occurrence| occurrence.evidence.id == prompted_evidence_id)
            .expect("prompted occurrence projection");
        assert!(matches!(
            prompted_projection
                .missed_resolution
                .as_ref()
                .map(|resolution| &resolution.action),
            Some(HabitMissedResolutionAction::ReduceFrequency {
                suppressed_planner_occurrence_ids,
            }) if suppressed_planner_occurrence_ids == &[target_planner_id]
        ));
    }
}

#[tokio::test]
async fn missed_reconcile_round_robins_dense_habits_across_bounded_pages() {
    let (app, repository) = test_app();
    let dense_habit = Uuid::new_v4();
    let sparse_habit = Uuid::new_v4();
    for (index, habit_id) in [dense_habit, sparse_habit].into_iter().enumerate() {
        let created = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(habit_body_with_missed_policy(habit_id, "ask")),
                Some(&format!("missed-fair-create-{index}")),
            ))
            .await
            .expect("create fair-scan habit");
        assert_eq!(created.status(), StatusCode::CREATED);
    }
    let today = postgres_now().date_naive();
    for days_ago in [6, 5, 4] {
        repository
            .insert_authoritative_occurrence(occurrence_evidence(
                Uuid::new_v4(),
                dense_habit,
                today - ChronoDuration::days(days_ago),
                "ask",
            ))
            .await
            .expect("seed dense habit history");
    }
    repository
        .insert_authoritative_occurrence(occurrence_evidence(
            Uuid::new_v4(),
            sparse_habit,
            today - ChronoDuration::days(3),
            "ask",
        ))
        .await
        .expect("seed sparse habit history");

    let mut observed = Vec::new();
    for page in 0..2 {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/habits/missed/reconcile?limit=1",
                Some(json!({"operation_id":Uuid::new_v4()})),
                Some(&format!("missed-fair-page-{page}")),
            ))
            .await
            .expect("reconcile fair page");
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["has_more"], true);
        observed
            .push(Uuid::parse_str(body["resolutions"][0]["habit_id"].as_str().unwrap()).unwrap());
    }
    assert_eq!(observed, vec![dense_habit, sparse_habit]);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn configured_carry_recarries_but_an_ask_selected_carry_prompts_again() {
    let repository = InMemoryHabitRepository::default();
    let source_date = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
    let now = source_date.and_hms_opt(12, 0, 0).expect("time").and_utc();
    let carry_habit = Uuid::new_v4();
    let carry_source = occurrence_evidence(Uuid::new_v4(), carry_habit, source_date, "carry");
    repository
        .insert_authoritative_occurrence(carry_source)
        .await
        .expect("seed configured-carry source");
    let carry_fingerprint = habit_policy_fingerprint_bytes(carry_habit, "carry");
    let carry_policy = std::collections::BTreeMap::from([(
        carry_habit,
        HabitMissedConfiguration {
            item_revision: 1,
            policy_fingerprint: carry_fingerprint,
            policy: HabitMissedPolicy::Carry,
            is_active: true,
        },
    )]);
    let first_carry = repository
        .reconcile_missed(MissedReconcileWrite {
            policies: carry_policy.clone(),
            limit: 10,
            recorded_at: now,
            idempotency: HabitIdempotency {
                namespace: "test.missed.recarry.first",
                key_hash: [0x41; 32],
                request_fingerprint: [0x42; 32],
                operation_id: Uuid::new_v4(),
                actor_session_id: None,
            },
        })
        .await
        .expect("create configured carry");
    let first_carry_end = match first_carry.value.resolutions[0].action {
        HabitMissedResolutionAction::Carry { window_end, .. } => window_end,
        ref other => panic!("expected carry, got {other:?}"),
    };
    let recarried = repository
        .reconcile_missed(MissedReconcileWrite {
            policies: carry_policy,
            limit: 10,
            recorded_at: first_carry_end,
            idempotency: HabitIdempotency {
                namespace: "test.missed.recarry.second",
                key_hash: [0x43; 32],
                request_fingerprint: [0x44; 32],
                operation_id: Uuid::new_v4(),
                actor_session_id: None,
            },
        })
        .await
        .expect("re-carry expired configured carry");
    assert!(matches!(
        recarried.value.resolutions.as_slice(),
        [resolution]
            if resolution.revision == 2
                && matches!(
                    resolution.action,
                    HabitMissedResolutionAction::Carry { window_start, .. }
                        if window_start == first_carry_end
                )
    ));

    let ask_habit = Uuid::new_v4();
    let ask_source = occurrence_evidence(Uuid::new_v4(), ask_habit, source_date, "ask");
    let ask_source_id = ask_source.id;
    repository
        .insert_authoritative_occurrence(ask_source)
        .await
        .expect("seed ask source");
    let ask_fingerprint = habit_policy_fingerprint_bytes(ask_habit, "ask");
    let ask_policy = std::collections::BTreeMap::from([(
        ask_habit,
        HabitMissedConfiguration {
            item_revision: 1,
            policy_fingerprint: ask_fingerprint,
            policy: HabitMissedPolicy::Ask,
            is_active: true,
        },
    )]);
    repository
        .reconcile_missed(MissedReconcileWrite {
            policies: ask_policy.clone(),
            limit: 10,
            recorded_at: now,
            idempotency: HabitIdempotency {
                namespace: "test.missed.ask-carry.prompt",
                key_hash: [0x45; 32],
                request_fingerprint: [0x46; 32],
                operation_id: Uuid::new_v4(),
                actor_session_id: None,
            },
        })
        .await
        .expect("create ask prompt");
    let selected = repository
        .resolve_missed(MissedResolveWrite {
            habit_id: ask_habit,
            occurrence_id: ask_source_id,
            expected_revision: 1,
            action: HabitMissedExplicitAction::Carry,
            current_item_revision: 1,
            current_policy_fingerprint: ask_fingerprint,
            current_item_is_active: true,
            recorded_at: now,
            idempotency: HabitIdempotency {
                namespace: "test.missed.ask-carry.select",
                key_hash: [0x47; 32],
                request_fingerprint: [0x48; 32],
                operation_id: Uuid::new_v4(),
                actor_session_id: None,
            },
        })
        .await
        .expect("select carry for ask prompt");
    let selected_end = match selected.value.action {
        HabitMissedResolutionAction::Carry { window_end, .. } => window_end,
        ref other => panic!("expected selected carry, got {other:?}"),
    };
    let prompted_again = repository
        .reconcile_missed(MissedReconcileWrite {
            policies: ask_policy,
            limit: 10,
            recorded_at: selected_end,
            idempotency: HabitIdempotency {
                namespace: "test.missed.ask-carry.expired",
                key_hash: [0x49; 32],
                request_fingerprint: [0x4a; 32],
                operation_id: Uuid::new_v4(),
                actor_session_id: None,
            },
        })
        .await
        .expect("prompt again after selected carry expires");
    assert!(matches!(
        prompted_again.value.resolutions.as_slice(),
        [resolution]
            if resolution.revision == 3
                && matches!(resolution.action, HabitMissedResolutionAction::DecisionRequired)
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn habit_pause_is_single_open_revisioned_and_validated() {
    let (app, _) = test_app();
    let habit_id = Uuid::new_v4();
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(habit_body(habit_id)),
            Some("habit-create-pause-001"),
        ))
        .await
        .expect("item response");
    assert_eq!(created.status(), StatusCode::CREATED);

    let pause_id = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let started_at = postgres_now() - ChronoDuration::minutes(5);
    let body = json!({
        "operation_id": operation_id,
        "pause_id": pause_id,
        "expected_revision": 0,
        "started_at": started_at,
    });
    let started = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses"),
            Some(body.clone()),
            Some("habit-pause-001"),
        ))
        .await
        .expect("pause response");
    assert_eq!(started.status(), StatusCode::OK);
    let started_json = body_json(started).await;
    assert_eq!(started_json["pause"]["revision"], 1);
    assert_eq!(started_json["pause"]["preserves_streak"], true);

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses"),
            Some(body),
            Some("habit-pause-001"),
        ))
        .await
        .expect("pause replay");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");

    let second = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "pause_id": Uuid::new_v4(),
                "expected_revision": 0,
                "started_at": postgres_now(),
            })),
            Some("habit-pause-002"),
        ))
        .await
        .expect("second pause response");
    assert_eq!(second.status(), StatusCode::CONFLICT);

    let invalid_resume = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses/{pause_id}/resume"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "expected_revision": 1,
                "ended_at": started_at,
            })),
            Some("habit-pause-invalid-resume-001"),
        ))
        .await
        .expect("invalid resume response");
    assert_eq!(invalid_resume.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let resume_body = json!({
        "operation_id": Uuid::new_v4(),
        "expected_revision": 1,
        "ended_at": postgres_now(),
    });
    let resumed = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses/{pause_id}/resume"),
            Some(resume_body.clone()),
            Some("habit-pause-resume-001"),
        ))
        .await
        .expect("resume response");
    assert_eq!(resumed.status(), StatusCode::OK);
    assert_eq!(body_json(resumed).await["pause"]["revision"], 2);

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses/{pause_id}/resume"),
            Some(resume_body),
            Some("habit-pause-resume-001"),
        ))
        .await
        .expect("resume replay");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
}

#[test]
fn evidence_identity_contract_is_distinct_and_strict() {
    let local_date = NaiveDate::from_ymd_opt(2026, 10, 25).expect("DST date");
    let habit_id = Uuid::new_v4();
    let planner_occurrence_id = Uuid::new_v5(&habit_id, format!("daily:{local_date}:0").as_bytes());
    let value = json!({
        "id": Uuid::new_v4(),
        "habit_id": habit_id,
        "planner_occurrence_id": planner_occurrence_id,
        "source_schedule_revision_id": Uuid::new_v4(),
        "source_item_revision": 7,
        "policy_fingerprint": format!("sha256:{}", "0".repeat(64)),
        "identity": {"type":"calendar_day","date":local_date,"bucket_ordinal":0},
        "nominal_start": "2026-10-25T07:00:00Z",
        "nominal_end": "2026-10-25T07:30:00Z",
        "window_start": "2026-10-25T06:00:00Z",
        "window_end": "2026-10-25T08:00:00Z",
        "local_date": local_date,
        "timezone_name": "Europe/Paris",
        "expected_duration_seconds": 1800,
        "expected_quantity": null,
        "expected_unit": null,
    });
    let parsed: HabitOccurrenceEvidence = serde_json::from_value(value.clone()).expect("evidence");
    assert_ne!(parsed.id, parsed.planner_occurrence_id);
    parsed.validate().expect("valid recurrence evidence");
    let mut unknown = value;
    unknown["invented"] = json!(true);
    assert!(serde_json::from_value::<HabitOccurrenceEvidence>(unknown).is_err());
}

#[tokio::test]
async fn authoritative_insertion_rejects_malformed_and_legacy_recurrence_evidence() {
    let repository = InMemoryHabitRepository::default();
    let local_date = NaiveDate::from_ymd_opt(2026, 9, 4).expect("date");
    let nominal_start = local_date.and_hms_opt(8, 0, 0).expect("time").and_utc();
    let habit_id = Uuid::new_v4();
    let planner_occurrence_id = Uuid::new_v5(&habit_id, format!("daily:{local_date}:0").as_bytes());
    let valid = HabitOccurrenceEvidence {
        id: Uuid::new_v4(),
        habit_id,
        planner_occurrence_id,
        source_schedule_revision_id: Uuid::new_v4(),
        source_item_revision: 1,
        policy_fingerprint: format!("sha256:{}", "a".repeat(64)),
        identity: json!({
            "type": "calendar_day",
            "date": local_date,
            "bucket_ordinal": 0
        }),
        nominal_start,
        nominal_end: nominal_start + ChronoDuration::minutes(30),
        window_start: nominal_start - ChronoDuration::hours(1),
        window_end: nominal_start + ChronoDuration::hours(1),
        local_date,
        timezone_name: "Europe/Paris".to_owned(),
        expected_duration_seconds: Some(1_800),
        expected_quantity: Some(20),
        expected_unit: Some("pages".to_owned()),
    };
    valid.validate().expect("valid control evidence");

    let mut malformed = valid.clone();
    malformed.id = Uuid::new_v4();
    malformed.identity = json!({"type":"calendar_day","date":local_date});
    assert!(matches!(
        repository.insert_authoritative_occurrence(malformed).await,
        Err(HabitRepositoryError::Internal)
    ));

    let mut legacy_daily = valid.clone();
    legacy_daily.id = Uuid::new_v4();
    legacy_daily.identity = json!({"type":"daily","date":local_date,"ordinal":0});
    assert!(matches!(
        repository
            .insert_authoritative_occurrence(legacy_daily)
            .await,
        Err(HabitRepositoryError::Internal)
    ));

    let mut legacy_custom = valid.clone();
    legacy_custom.id = Uuid::new_v4();
    legacy_custom.identity = json!({"type":"custom"});
    assert!(matches!(
        repository
            .insert_authoritative_occurrence(legacy_custom)
            .await,
        Err(HabitRepositoryError::Internal)
    ));

    let mut noncanonical_custom_rule = valid.clone();
    noncanonical_custom_rule.id = Uuid::new_v4();
    let rule_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"habit-api-custom-rule");
    noncanonical_custom_rule.identity = json!({
        "type": "custom_rule",
        "rule_id": rule_id.to_string().to_uppercase(),
        "sequence": 0,
        "date": local_date
    });
    assert!(matches!(
        repository
            .insert_authoritative_occurrence(noncanonical_custom_rule)
            .await,
        Err(HabitRepositoryError::Internal)
    ));

    let mut non_deterministic_id = valid;
    non_deterministic_id.id = Uuid::new_v4();
    non_deterministic_id.planner_occurrence_id = Uuid::new_v4();
    assert!(matches!(
        repository
            .insert_authoritative_occurrence(non_deterministic_id)
            .await,
        Err(HabitRepositoryError::Internal)
    ));
}
