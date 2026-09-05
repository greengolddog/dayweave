use std::{str::FromStr, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use dayweave_api::{
    AppState,
    auth::{RuntimeAuthenticator, Scope, hash_token},
    config::AuthMode,
    credential_auth::{
        ACCESS_TOKEN_TTL, CredentialKind, CredentialMutation, CredentialRepository,
        CredentialRepositoryError, DEVICE_CLIENT_CONTRACT_VERSION, DEVICE_SESSION_ABSOLUTE_TTL,
        DEVICE_SESSION_REFRESH_IDLE_TTL, DeviceClientKind, DeviceEnrollmentSpec, DeviceSession,
        MAX_ACTIVE_DEVICE_SESSIONS, MAX_PENDING_DEVICE_ENROLLMENTS, McpClientSpec,
        OpaqueCredential,
    },
    http::router,
    persistence::{DatabaseScope, MIGRATOR, PostgresCredentialRepository},
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keeps the concurrent and terminal exact-replay boundaries together.
async fn device_enrollment_creation_recovers_only_an_exact_pending_tuple() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-create-replay-owner", "auth-create-replay").await;
    let other_scope =
        seed_scope(pool, "auth-create-replay-other", "auth-create-replay-other").await;
    let repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let other_repository = PostgresCredentialRepository::new(pool.clone(), other_scope);
    let now = Utc::now();
    let enrollment_id = Uuid::new_v4();
    let enrollment_raw = token(CredentialKind::Enrollment, 90);
    let enrollment = OpaqueCredential::parse(CredentialKind::Enrollment, &enrollment_raw).unwrap();
    let spec = DeviceEnrollmentSpec {
        id: enrollment_id,
        client_instance_id: Uuid::new_v4(),
        client_kind: DeviceClientKind::Macos,
        device_label: "Exact replay Mac".to_owned(),
        scopes: vec![Scope::ScheduleRead, Scope::SuggestionsWrite],
        client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
        client_version: "test-create-replay-1".to_owned(),
        client_capabilities: vec!["exact-request-replay".to_owned()],
        created_at: now,
    };

    let (first, second) = tokio::join!(
        repository.create_or_replay_device_enrollment(spec.clone(), &enrollment),
        repository.create_or_replay_device_enrollment(spec.clone(), &enrollment),
    );
    let first = first.expect("first concurrent create succeeds");
    let second = second.expect("second concurrent create succeeds");
    assert_ne!(first.replayed, second.replayed);
    assert_postgres_instant_eq(first.expires_at, now + ChronoDuration::minutes(10));
    assert_postgres_instant_eq(second.expires_at, first.expires_at);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM device_enrollments WHERE id = $1",)
            .bind(enrollment_id)
            .fetch_one(pool)
            .await
            .expect("one enrollment row"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_operations WHERE entity_type = 'device_enrollment' AND entity_id = $1 AND operation_type = 'auth.device_enrollment.created'",
        )
        .bind(enrollment_id)
        .fetch_one(pool)
        .await
        .expect("one enrollment audit row"),
        1
    );

    let changed_semantic_tuples = [
        DeviceEnrollmentSpec {
            client_instance_id: Uuid::new_v4(),
            ..spec.clone()
        },
        DeviceEnrollmentSpec {
            client_kind: DeviceClientKind::Android,
            ..spec.clone()
        },
        DeviceEnrollmentSpec {
            device_label: "Changed label".to_owned(),
            ..spec.clone()
        },
        DeviceEnrollmentSpec {
            scopes: vec![Scope::ScheduleRead],
            ..spec.clone()
        },
        DeviceEnrollmentSpec {
            client_version: "test-create-replay-2".to_owned(),
            ..spec.clone()
        },
        DeviceEnrollmentSpec {
            client_capabilities: vec!["different-capability".to_owned()],
            ..spec.clone()
        },
    ];
    for changed in changed_semantic_tuples {
        assert_eq!(
            repository
                .create_or_replay_device_enrollment(changed, &enrollment)
                .await,
            Err(CredentialRepositoryError::Conflict),
            "every changed semantic field must make the creation non-replayable"
        );
    }
    let different_enrollment_raw = token(CredentialKind::Enrollment, 91);
    let different_enrollment =
        OpaqueCredential::parse(CredentialKind::Enrollment, &different_enrollment_raw).unwrap();
    assert_eq!(
        repository
            .create_or_replay_device_enrollment(spec.clone(), &different_enrollment)
            .await,
        Err(CredentialRepositoryError::Conflict)
    );
    let mut changed_id = spec.clone();
    changed_id.id = Uuid::new_v4();
    assert_eq!(
        repository
            .create_or_replay_device_enrollment(changed_id, &enrollment)
            .await,
        Err(CredentialRepositoryError::Conflict)
    );
    assert_eq!(
        other_repository
            .create_or_replay_device_enrollment(spec.clone(), &enrollment)
            .await,
        Err(CredentialRepositoryError::Conflict),
        "a global token or identifier collision cannot cross a workspace boundary"
    );

    let access_raw = token(CredentialKind::DeviceAccess, 92);
    let refresh_raw = token(CredentialKind::DeviceRefresh, 93);
    let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
    let refresh = OpaqueCredential::parse(CredentialKind::DeviceRefresh, &refresh_raw).unwrap();
    repository
        .consume_device_enrollment(
            &enrollment,
            Uuid::new_v4(),
            &access,
            &refresh,
            now + ChronoDuration::seconds(1),
        )
        .await
        .expect("consume pending enrollment");
    let mut consumed_retry = spec;
    consumed_retry.created_at = now + ChronoDuration::seconds(2);
    assert_eq!(
        repository
            .create_or_replay_device_enrollment(consumed_retry, &enrollment)
            .await,
        Err(CredentialRepositoryError::Conflict),
        "a consumed one-time enrollment is never recreated or replayed"
    );

    let expired_raw = token(CredentialKind::Enrollment, 94);
    let expired = OpaqueCredential::parse(CredentialKind::Enrollment, &expired_raw).unwrap();
    let expired_created_at = now - ChronoDuration::minutes(10) - ChronoDuration::seconds(1);
    let expired_spec = DeviceEnrollmentSpec {
        id: Uuid::new_v4(),
        client_instance_id: Uuid::new_v4(),
        client_kind: DeviceClientKind::Android,
        device_label: "Expired replay Pixel".to_owned(),
        scopes: vec![Scope::ScheduleRead],
        client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
        client_version: "test-create-replay-1".to_owned(),
        client_capabilities: vec!["exact-request-replay".to_owned()],
        created_at: expired_created_at,
    };
    repository
        .create_or_replay_device_enrollment(expired_spec.clone(), &expired)
        .await
        .expect("historical fixture created");
    let mut expired_retry = expired_spec;
    expired_retry.created_at = now;
    assert_eq!(
        repository
            .create_or_replay_device_enrollment(expired_retry, &expired)
            .await,
        Err(CredentialRepositoryError::Conflict),
        "expiry is exclusive and an expired issuance cannot be recovered"
    );

    let revoked_raw = token(CredentialKind::Enrollment, 95);
    let revoked = OpaqueCredential::parse(CredentialKind::Enrollment, &revoked_raw).unwrap();
    let revoked_spec = DeviceEnrollmentSpec {
        id: Uuid::new_v4(),
        client_instance_id: Uuid::new_v4(),
        client_kind: DeviceClientKind::Macos,
        device_label: "Revoked replay Mac".to_owned(),
        scopes: vec![Scope::ScheduleRead],
        client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
        client_version: "test-create-replay-1".to_owned(),
        client_capabilities: vec!["exact-request-replay".to_owned()],
        created_at: now,
    };
    repository
        .create_or_replay_device_enrollment(revoked_spec.clone(), &revoked)
        .await
        .expect("revocation fixture created");
    assert!(
        repository
            .revoke_device_enrollment(revoked_spec.id, now + ChronoDuration::seconds(1))
            .await
            .expect("pending enrollment revoked")
    );
    let mut revoked_retry = revoked_spec;
    revoked_retry.created_at = now + ChronoDuration::seconds(2);
    assert_eq!(
        repository
            .create_or_replay_device_enrollment(revoked_retry, &revoked)
            .await,
        Err(CredentialRepositoryError::Conflict),
        "a revoked enrollment can never be recreated or replayed"
    );

    test_database.destroy().await;
}

#[tokio::test]
async fn historical_v1_device_session_authenticates_and_refreshes_without_scope_widening() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-device-v1-owner", "auth-device-v1").await;
    let repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let now = Utc::now();

    let rejected_enrollment_raw = token(CredentialKind::Enrollment, 81);
    let rejected_enrollment =
        OpaqueCredential::parse(CredentialKind::Enrollment, &rejected_enrollment_raw).unwrap();
    assert_eq!(
        repository
            .create_device_enrollment(
                DeviceEnrollmentSpec {
                    id: Uuid::new_v4(),
                    client_instance_id: Uuid::new_v4(),
                    client_kind: DeviceClientKind::Android,
                    device_label: "New v1 device".to_owned(),
                    scopes: vec![Scope::ScheduleRead],
                    client_contract_version: 1,
                    client_version: "legacy-client".to_owned(),
                    client_capabilities: Vec::new(),
                    created_at: now,
                },
                &rejected_enrollment,
            )
            .await,
        Err(CredentialRepositoryError::InvalidInput),
        "new v1 devices must re-enroll with the current contract"
    );

    let enrollment_raw = token(CredentialKind::Enrollment, 82);
    let access_raw = token(CredentialKind::DeviceAccess, 83);
    let refresh_raw = token(CredentialKind::DeviceRefresh, 84);
    let enrollment = OpaqueCredential::parse(CredentialKind::Enrollment, &enrollment_raw).unwrap();
    let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
    let refresh = OpaqueCredential::parse(CredentialKind::DeviceRefresh, &refresh_raw).unwrap();
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: Uuid::new_v4(),
                client_instance_id: Uuid::new_v4(),
                client_kind: DeviceClientKind::Macos,
                device_label: "Historical v1 device".to_owned(),
                scopes: vec![Scope::ScheduleRead, Scope::ScheduleSimulate],
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: "migrated-client".to_owned(),
                client_capabilities: Vec::new(),
                created_at: now,
            },
            &enrollment,
        )
        .await
        .expect("seed current enrollment");
    let issued = repository
        .consume_device_enrollment(&enrollment, Uuid::new_v4(), &access, &refresh, now)
        .await
        .expect("seed current session");
    sqlx::query("UPDATE sessions SET client_contract_version = 1 WHERE id = $1")
        .bind(issued.id)
        .execute(pool)
        .await
        .expect("represent a pre-v2 stored session");

    let historical = repository
        .authenticate_device_access(&access, now + ChronoDuration::minutes(1))
        .await
        .expect("historical v1 access remains compatible");
    assert_eq!(historical.client_contract_version, 1);
    assert!(!historical.scopes.contains(&Scope::SchedulePublish));
    let widened = sqlx::query(
        "UPDATE sessions SET scopes = ARRAY['schedule_read', 'schedule_publish']::text[] \
         WHERE id = $1",
    )
    .bind(historical.id)
    .execute(pool)
    .await;
    assert!(widened.is_err(), "the database rejects scope widening");

    let next_access_raw = token(CredentialKind::DeviceAccess, 85);
    let next_refresh_raw = token(CredentialKind::DeviceRefresh, 86);
    let next_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &next_access_raw).unwrap();
    let next_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &next_refresh_raw).unwrap();
    let refreshed = repository
        .refresh_device_session(
            &refresh,
            &next_access,
            &next_refresh,
            now + ChronoDuration::minutes(2),
        )
        .await
        .expect("historical v1 refresh remains compatible");
    assert_eq!(refreshed.client_contract_version, 1);
    assert!(!refreshed.scopes.contains(&Scope::SchedulePublish));
    repository
        .authenticate_device_access(&next_access, now + ChronoDuration::minutes(3))
        .await
        .expect("refreshed v1 access authenticates");

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Covers the complete issuance/rotation/revocation lifecycle.
async fn device_credentials_are_one_time_rotated_scoped_expiring_and_hash_only() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-device-owner", "auth-device").await;
    let other_scope = seed_scope(pool, "auth-device-other", "auth-device-other").await;
    let repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let other_repository = PostgresCredentialRepository::new(pool.clone(), other_scope);
    let now = Utc::now();
    let enrollment_id = Uuid::new_v4();
    let client_instance_id = Uuid::new_v4();
    let enrollment_raw = token(CredentialKind::Enrollment, 1);
    let enrollment = OpaqueCredential::parse(CredentialKind::Enrollment, &enrollment_raw).unwrap();
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: enrollment_id,
                client_instance_id,
                client_kind: DeviceClientKind::Macos,
                device_label: "Personal Mac".to_owned(),
                scopes: vec![Scope::ScheduleRead, Scope::SuggestionsWrite],
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: "test-1".to_owned(),
                client_capabilities: vec!["exact-replay".to_owned()],
                created_at: now,
            },
            &enrollment,
        )
        .await
        .expect("enrollment created");

    let first_access_raw = token(CredentialKind::DeviceAccess, 2);
    let first_refresh_raw = token(CredentialKind::DeviceRefresh, 3);
    let second_access_raw = token(CredentialKind::DeviceAccess, 4);
    let second_refresh_raw = token(CredentialKind::DeviceRefresh, 5);
    let first_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &first_access_raw).unwrap();
    let first_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &first_refresh_raw).unwrap();
    let second_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &second_access_raw).unwrap();
    let second_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &second_refresh_raw).unwrap();
    let shared_access_raw = token(CredentialKind::DeviceAccess, 41);
    let shared_refresh_raw = token(CredentialKind::DeviceRefresh, 41);
    let shared_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &shared_access_raw).unwrap();
    let shared_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &shared_refresh_raw).unwrap();
    assert_eq!(
        repository
            .consume_device_enrollment(
                &enrollment,
                Uuid::new_v4(),
                &shared_access,
                &shared_refresh,
                now,
            )
            .await,
        Err(CredentialRepositoryError::InvalidInput),
        "changing only the public prefix cannot reuse access material as refresh material"
    );
    let enrollment_material_as_access_raw = token(CredentialKind::DeviceAccess, 1);
    let enrollment_material_as_access = OpaqueCredential::parse(
        CredentialKind::DeviceAccess,
        &enrollment_material_as_access_raw,
    )
    .unwrap();
    assert_eq!(
        repository
            .consume_device_enrollment(
                &enrollment,
                Uuid::new_v4(),
                &enrollment_material_as_access,
                &first_refresh,
                now,
            )
            .await,
        Err(CredentialRepositoryError::InvalidInput),
        "one-time enrollment material cannot be reused for an issued credential"
    );
    let first_session_id = Uuid::new_v4();
    let second_session_id = Uuid::new_v4();
    let (first_consume, second_consume) = tokio::join!(
        repository.consume_device_enrollment(
            &enrollment,
            first_session_id,
            &first_access,
            &first_refresh,
            now
        ),
        repository.consume_device_enrollment(
            &enrollment,
            second_session_id,
            &second_access,
            &second_refresh,
            now
        )
    );
    assert!(first_consume.is_ok() ^ second_consume.is_ok());
    let first_won = first_consume.is_ok();
    let session = first_consume
        .as_ref()
        .or(second_consume.as_ref())
        .expect("one enrollment consumer")
        .clone();
    let loser = if first_won {
        second_consume.unwrap_err()
    } else {
        first_consume.unwrap_err()
    };
    assert_eq!(loser, CredentialRepositoryError::InvalidCredential);
    let exact_consume_replay = if first_won {
        repository
            .consume_device_enrollment(
                &enrollment,
                first_session_id,
                &first_access,
                &first_refresh,
                now + ChronoDuration::seconds(1),
            )
            .await
    } else {
        repository
            .consume_device_enrollment(
                &enrollment,
                second_session_id,
                &second_access,
                &second_refresh,
                now + ChronoDuration::seconds(1),
            )
            .await
    }
    .expect("the exact enrollment issue can be recovered after response loss");
    assert!(exact_consume_replay.replayed);
    assert_eq!(exact_consume_replay.id, session.id);
    let listed_sessions = repository
        .list_device_sessions(now + ChronoDuration::seconds(1))
        .await
        .expect("active sessions listed");
    assert_eq!(listed_sessions.len(), 1);
    assert_eq!(listed_sessions[0].id, session.id);
    assert_eq!(session.workspace_id, scope.workspace_id);
    assert_eq!(session.user_id, scope.user_id);
    assert_eq!(session.client_instance_id, client_instance_id);
    assert_eq!(session.credential_issued_at, now);
    assert_eq!(session.access_expires_at, now + ChronoDuration::minutes(15));
    assert_eq!(
        session.refresh_idle_expires_at,
        now + ChronoDuration::days(30)
    );
    assert_eq!(session.absolute_expires_at, now + ChronoDuration::days(180));

    let (active_access, active_refresh) = if first_won {
        (&first_access, &first_refresh)
    } else {
        (&second_access, &second_refresh)
    };
    repository
        .authenticate_device_access(active_access, now + ChronoDuration::minutes(1))
        .await
        .expect("active access credential");
    assert_eq!(
        other_repository
            .authenticate_device_access(active_access, now + ChronoDuration::minutes(1))
            .await,
        Err(CredentialRepositoryError::InvalidCredential)
    );
    assert_eq!(
        repository
            .authenticate_device_access(active_access, now + ChronoDuration::minutes(15))
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "access expiry is exclusive"
    );

    let refresh_at = now + ChronoDuration::minutes(16);
    let active_access_marker = if first_won { 2 } else { 4 };
    let active_refresh_marker = if first_won { 3 } else { 5 };
    let current_refresh_as_next_access_raw =
        token(CredentialKind::DeviceAccess, active_refresh_marker);
    let current_refresh_as_next_access = OpaqueCredential::parse(
        CredentialKind::DeviceAccess,
        &current_refresh_as_next_access_raw,
    )
    .unwrap();
    assert_eq!(
        repository
            .refresh_device_session(
                active_refresh,
                &current_refresh_as_next_access,
                &shared_refresh,
                refresh_at,
            )
            .await,
        Err(CredentialRepositoryError::InvalidInput),
        "current refresh material cannot be relabeled as the next access credential"
    );
    let current_access_as_next_refresh_raw =
        token(CredentialKind::DeviceRefresh, active_access_marker);
    let current_access_as_next_refresh = OpaqueCredential::parse(
        CredentialKind::DeviceRefresh,
        &current_access_as_next_refresh_raw,
    )
    .unwrap();
    assert_eq!(
        repository
            .refresh_device_session(
                active_refresh,
                &shared_access,
                &current_access_as_next_refresh,
                refresh_at,
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "stored current access material cannot be relabeled as the next refresh credential"
    );
    let rejected_refresh_raw = token(CredentialKind::DeviceRefresh, 40);
    let rejected_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &rejected_refresh_raw).unwrap();
    assert_eq!(
        repository
            .refresh_device_session(active_refresh, active_access, &rejected_refresh, refresh_at)
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "refresh cannot silently reuse the current access credential"
    );
    let rotated_access_one_raw = token(CredentialKind::DeviceAccess, 6);
    let rotated_refresh_one_raw = token(CredentialKind::DeviceRefresh, 7);
    let rotated_access_two_raw = token(CredentialKind::DeviceAccess, 8);
    let rotated_refresh_two_raw = token(CredentialKind::DeviceRefresh, 9);
    let rotated_access_one =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &rotated_access_one_raw).unwrap();
    let rotated_refresh_one =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &rotated_refresh_one_raw).unwrap();
    let rotated_access_two =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &rotated_access_two_raw).unwrap();
    let rotated_refresh_two =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &rotated_refresh_two_raw).unwrap();
    let (first_rotation, second_rotation) = tokio::join!(
        repository.refresh_device_session(
            active_refresh,
            &rotated_access_one,
            &rotated_refresh_one,
            refresh_at
        ),
        repository.refresh_device_session(
            active_refresh,
            &rotated_access_two,
            &rotated_refresh_two,
            refresh_at
        )
    );
    assert!(first_rotation.is_ok() ^ second_rotation.is_ok());
    let first_rotation_won = first_rotation.is_ok();
    let rotated = first_rotation
        .as_ref()
        .or(second_rotation.as_ref())
        .expect("one refresh rotation")
        .clone();
    let rotation_loser = if first_rotation_won {
        second_rotation.unwrap_err()
    } else {
        first_rotation.unwrap_err()
    };
    assert_eq!(rotation_loser, CredentialRepositoryError::InvalidCredential);
    assert_eq!(rotated.revision, 2);
    assert_postgres_instant_eq(rotated.credential_issued_at, refresh_at);
    assert_postgres_instant_eq(
        rotated.access_expires_at,
        refresh_at + ChronoDuration::minutes(15),
    );
    assert_postgres_instant_eq(
        rotated.refresh_idle_expires_at,
        refresh_at + ChronoDuration::days(30),
    );
    let rotated_access = if first_rotation_won {
        &rotated_access_one
    } else {
        &rotated_access_two
    };
    assert_eq!(
        repository
            .authenticate_device_access(rotated_access, refresh_at - ChronoDuration::seconds(1))
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "a rotated access token is not valid before its credential issuance time"
    );
    repository
        .authenticate_device_access(rotated_access, refresh_at)
        .await
        .expect("rotated access token works");
    let overlong_access = sqlx::query(
        "UPDATE sessions SET expires_at = credential_issued_at + interval '901 seconds' \
         WHERE id = $1",
    )
    .bind(session.id)
    .execute(pool)
    .await
    .expect_err("schema rejects access lifetimes above fifteen minutes");
    assert_postgres_code(&overlong_access, "23514");
    let overlong_refresh_idle = sqlx::query(
        "UPDATE sessions SET refresh_idle_expires_at = \
         credential_issued_at + interval '2592001 seconds' WHERE id = $1",
    )
    .bind(session.id)
    .execute(pool)
    .await
    .expect_err("schema rejects refresh-idle lifetimes above thirty days");
    assert_postgres_code(&overlong_refresh_idle, "23514");
    sqlx::query("UPDATE sessions SET device_label = E'bad\\nlabel' WHERE id = $1")
        .bind(session.id)
        .execute(pool)
        .await
        .expect("tamper device label fixture");
    assert_eq!(
        repository
            .authenticate_device_access(rotated_access, refresh_at)
            .await,
        Err(CredentialRepositoryError::Internal),
        "malformed durable identity metadata fails closed"
    );
    sqlx::query("UPDATE sessions SET device_label = 'Personal Mac' WHERE id = $1")
        .bind(session.id)
        .execute(pool)
        .await
        .expect("restore device label fixture");
    let exact_refresh_replay = if first_rotation_won {
        repository
            .refresh_device_session(
                active_refresh,
                &rotated_access_one,
                &rotated_refresh_one,
                refresh_at + ChronoDuration::seconds(1),
            )
            .await
    } else {
        repository
            .refresh_device_session(
                active_refresh,
                &rotated_access_two,
                &rotated_refresh_two,
                refresh_at + ChronoDuration::seconds(1),
            )
            .await
    }
    .expect("the exact refresh pair can be recovered after response loss");
    assert!(exact_refresh_replay.replayed);
    assert_eq!(exact_refresh_replay.id, session.id);
    let refresh_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_operations WHERE workspace_id = $1 \
         AND operation_type = 'auth.device_session.refreshed'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .expect("refresh audit count");
    assert_eq!(
        refresh_audits, 1,
        "exact replay does not duplicate audit rows"
    );

    let (access_hash, refresh_hash): (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT token_hash, refresh_token_hash FROM sessions \
         WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(session.id)
    .fetch_one(pool)
    .await
    .expect("stored session hashes");
    let enrollment_hash: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM device_enrollments WHERE id = $1")
            .bind(enrollment_id)
            .fetch_one(pool)
            .await
            .expect("stored enrollment hash");
    assert_eq!(access_hash.len(), 32);
    assert_eq!(refresh_hash.len(), 32);
    assert_eq!(enrollment_hash.len(), 32);
    for raw in [
        enrollment_raw.as_bytes(),
        first_access_raw.as_bytes(),
        first_refresh_raw.as_bytes(),
        second_access_raw.as_bytes(),
        second_refresh_raw.as_bytes(),
        rotated_access_one_raw.as_bytes(),
        rotated_refresh_one_raw.as_bytes(),
        rotated_access_two_raw.as_bytes(),
        rotated_refresh_two_raw.as_bytes(),
    ] {
        assert_ne!(access_hash.as_slice(), raw);
        assert_ne!(refresh_hash.as_slice(), raw);
        assert_ne!(enrollment_hash.as_slice(), raw);
    }

    assert!(
        repository
            .revoke_device_session(session.id, refresh_at + ChronoDuration::minutes(1))
            .await
            .expect("session revoked")
    );
    assert!(
        !repository
            .revoke_device_session(session.id, refresh_at + ChronoDuration::minutes(2))
            .await
            .expect("revocation is idempotent")
    );
    assert_eq!(
        repository
            .authenticate_device_access(rotated_access, refresh_at + ChronoDuration::minutes(2))
            .await,
        Err(CredentialRepositoryError::InvalidCredential)
    );
    assert!(
        !repository
            .revoke_device_enrollment(enrollment_id, refresh_at)
            .await
            .expect("a consumed enrollment cannot be revoked")
    );

    let expired_enrollment_raw = token(CredentialKind::Enrollment, 10);
    let expired_enrollment =
        OpaqueCredential::parse(CredentialKind::Enrollment, &expired_enrollment_raw).unwrap();
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: Uuid::new_v4(),
                client_instance_id: Uuid::new_v4(),
                client_kind: DeviceClientKind::Android,
                device_label: "Personal Pixel".to_owned(),
                scopes: vec![Scope::ScheduleRead],
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: "test-1".to_owned(),
                client_capabilities: Vec::new(),
                created_at: now,
            },
            &expired_enrollment,
        )
        .await
        .expect("expiring enrollment created");
    assert_eq!(
        repository
            .consume_device_enrollment(
                &expired_enrollment,
                Uuid::new_v4(),
                &first_access,
                &first_refresh,
                now + ChronoDuration::minutes(10)
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "enrollment expiry is exclusive"
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Each replay lifecycle fence needs an independent session.
async fn exact_replay_ignores_only_access_expiry_and_keeps_lifecycle_fences() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-replay-boundaries", "auth-replay-boundaries").await;
    let repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let now = Utc::now();

    let enrollment_fixture =
        issue_device_session(&repository, now, 100, "Enrollment replay boundaries").await;
    let enrollment = OpaqueCredential::parse(
        CredentialKind::Enrollment,
        &enrollment_fixture.enrollment_raw,
    )
    .unwrap();
    let initial_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &enrollment_fixture.access_raw)
            .unwrap();
    let initial_refresh = OpaqueCredential::parse(
        CredentialKind::DeviceRefresh,
        &enrollment_fixture.refresh_raw,
    )
    .unwrap();
    let recovered_enrollment = repository
        .consume_device_enrollment(
            &enrollment,
            enrollment_fixture.session.id,
            &initial_access,
            &initial_refresh,
            enrollment_fixture.session.access_expires_at + ChronoDuration::seconds(1),
        )
        .await
        .expect("exact enrollment replay survives access expiry");
    assert!(recovered_enrollment.replayed);
    assert_eq!(recovered_enrollment.id, enrollment_fixture.session.id);
    assert_eq!(
        repository
            .consume_device_enrollment(
                &enrollment,
                Uuid::new_v4(),
                &initial_access,
                &initial_refresh,
                enrollment_fixture.session.access_expires_at + ChronoDuration::seconds(1),
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "enrollment replay remains bound to the exact session ID"
    );
    assert_eq!(
        repository
            .consume_device_enrollment(
                &enrollment,
                enrollment_fixture.session.id,
                &initial_access,
                &initial_refresh,
                enrollment_fixture.session.refresh_idle_expires_at,
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "enrollment replay stops at the exclusive refresh-idle boundary"
    );
    assert_eq!(
        repository
            .consume_device_enrollment(
                &enrollment,
                enrollment_fixture.session.id,
                &initial_access,
                &initial_refresh,
                enrollment_fixture.session.absolute_expires_at,
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "enrollment replay stops at the exclusive absolute boundary"
    );

    let revoked_enrollment_fixture =
        issue_device_session(&repository, now, 110, "Revoked enrollment replay").await;
    assert!(
        repository
            .revoke_device_session(
                revoked_enrollment_fixture.session.id,
                now + ChronoDuration::minutes(1),
            )
            .await
            .expect("session revoked")
    );
    let revoked_enrollment = OpaqueCredential::parse(
        CredentialKind::Enrollment,
        &revoked_enrollment_fixture.enrollment_raw,
    )
    .unwrap();
    let revoked_access = OpaqueCredential::parse(
        CredentialKind::DeviceAccess,
        &revoked_enrollment_fixture.access_raw,
    )
    .unwrap();
    let revoked_refresh = OpaqueCredential::parse(
        CredentialKind::DeviceRefresh,
        &revoked_enrollment_fixture.refresh_raw,
    )
    .unwrap();
    assert_eq!(
        repository
            .consume_device_enrollment(
                &revoked_enrollment,
                revoked_enrollment_fixture.session.id,
                &revoked_access,
                &revoked_refresh,
                now + ChronoDuration::minutes(2),
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "revocation closes enrollment replay"
    );

    let refresh_fixture =
        issue_device_session(&repository, now, 120, "Refresh replay boundaries").await;
    let current_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &refresh_fixture.refresh_raw)
            .unwrap();
    let next_access_raw = token(CredentialKind::DeviceAccess, 123);
    let next_refresh_raw = token(CredentialKind::DeviceRefresh, 124);
    let next_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &next_access_raw).unwrap();
    let next_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &next_refresh_raw).unwrap();
    let refresh_at = now + ChronoDuration::minutes(1);
    let rotated = repository
        .refresh_device_session(&current_refresh, &next_access, &next_refresh, refresh_at)
        .await
        .expect("session rotated");
    assert!(!rotated.replayed);
    let recovered_refresh = repository
        .refresh_device_session(
            &current_refresh,
            &next_access,
            &next_refresh,
            rotated.access_expires_at + ChronoDuration::seconds(1),
        )
        .await
        .expect("exact refresh replay survives access expiry");
    assert!(recovered_refresh.replayed);
    assert_eq!(recovered_refresh.id, refresh_fixture.session.id);
    assert_eq!(
        repository
            .refresh_device_session(
                &current_refresh,
                &next_access,
                &next_refresh,
                rotated.refresh_idle_expires_at,
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "refresh replay stops at the exclusive refresh-idle boundary"
    );
    assert_eq!(
        repository
            .refresh_device_session(
                &current_refresh,
                &next_access,
                &next_refresh,
                rotated.absolute_expires_at,
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "refresh replay stops at the exclusive absolute boundary"
    );

    let generation_fixture =
        issue_device_session(&repository, now, 130, "Refresh generation boundaries").await;
    let generation_zero_refresh = OpaqueCredential::parse(
        CredentialKind::DeviceRefresh,
        &generation_fixture.refresh_raw,
    )
    .unwrap();
    let generation_one_access_raw = token(CredentialKind::DeviceAccess, 133);
    let generation_one_refresh_raw = token(CredentialKind::DeviceRefresh, 134);
    let generation_one_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &generation_one_access_raw).unwrap();
    let generation_one_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &generation_one_refresh_raw)
            .unwrap();
    repository
        .refresh_device_session(
            &generation_zero_refresh,
            &generation_one_access,
            &generation_one_refresh,
            now + ChronoDuration::minutes(1),
        )
        .await
        .expect("first generation rotated");
    let generation_two_access_raw = token(CredentialKind::DeviceAccess, 135);
    let generation_two_refresh_raw = token(CredentialKind::DeviceRefresh, 136);
    let generation_two_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &generation_two_access_raw).unwrap();
    let generation_two_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &generation_two_refresh_raw)
            .unwrap();
    repository
        .refresh_device_session(
            &generation_one_refresh,
            &generation_two_access,
            &generation_two_refresh,
            now + ChronoDuration::minutes(2),
        )
        .await
        .expect("second generation rotated");
    assert_eq!(
        repository
            .refresh_device_session(
                &generation_zero_refresh,
                &generation_one_access,
                &generation_one_refresh,
                now + ChronoDuration::minutes(3),
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "a replay window closes when the next generation advances"
    );
    let current_generation_replay = repository
        .refresh_device_session(
            &generation_one_refresh,
            &generation_two_access,
            &generation_two_refresh,
            now + ChronoDuration::minutes(3),
        )
        .await
        .expect("current generation remains exactly replayable");
    assert!(current_generation_replay.replayed);
    assert!(
        repository
            .revoke_device_session(
                generation_fixture.session.id,
                now + ChronoDuration::minutes(4),
            )
            .await
            .expect("generation session revoked")
    );
    assert_eq!(
        repository
            .refresh_device_session(
                &generation_one_refresh,
                &generation_two_access,
                &generation_two_refresh,
                now + ChronoDuration::minutes(5),
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "revocation closes refresh replay"
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Audits pre-cutover enrollment and session hydration corruption.
async fn device_hydration_rejects_ttl_corruption_without_the_schema_constraint() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-device-hydration", "auth-device-hydration").await;
    let repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let now = Utc::now();
    let enrollment_raw = token(CredentialKind::Enrollment, 51);
    let access_raw = token(CredentialKind::DeviceAccess, 52);
    let refresh_raw = token(CredentialKind::DeviceRefresh, 53);
    let enrollment = OpaqueCredential::parse(CredentialKind::Enrollment, &enrollment_raw).unwrap();
    let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
    let refresh = OpaqueCredential::parse(CredentialKind::DeviceRefresh, &refresh_raw).unwrap();
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: Uuid::new_v4(),
                client_instance_id: Uuid::new_v4(),
                client_kind: DeviceClientKind::Macos,
                device_label: "Synthetic hydration audit".to_owned(),
                scopes: vec![Scope::ScheduleRead],
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: "test-1".to_owned(),
                client_capabilities: Vec::new(),
                created_at: now,
            },
            &enrollment,
        )
        .await
        .expect("enrollment created");
    sqlx::query(
        "ALTER TABLE device_enrollments DROP CONSTRAINT \
         device_enrollments_runtime_scopes_check",
    )
    .execute(pool)
    .await
    .expect("drop enrollment audience constraint inside isolated audit schema");
    sqlx::query("UPDATE device_enrollments SET scopes = ARRAY['suggestions_submit']")
        .execute(pool)
        .await
        .expect("tamper pre-cutover enrollment fixture");
    assert_eq!(
        repository
            .consume_device_enrollment(&enrollment, Uuid::new_v4(), &access, &refresh, now)
            .await,
        Err(CredentialRepositoryError::Internal),
        "stored enrollment metadata with an invalid audience fails closed"
    );
    sqlx::query("UPDATE device_enrollments SET scopes = ARRAY['schedule_read']")
        .execute(pool)
        .await
        .expect("restore enrollment fixture");
    let session = repository
        .consume_device_enrollment(&enrollment, Uuid::new_v4(), &access, &refresh, now)
        .await
        .expect("session issued");

    sqlx::query("ALTER TABLE sessions DROP CONSTRAINT sessions_v1_runtime_shape_check")
        .execute(pool)
        .await
        .expect("drop shape constraint inside isolated audit schema");
    sqlx::query(
        "UPDATE sessions SET expires_at = credential_issued_at + interval '16 minutes' \
         WHERE id = $1",
    )
    .bind(session.id)
    .execute(pool)
    .await
    .expect("tamper access expiry fixture");
    assert_eq!(
        repository
            .authenticate_device_access(&access, now + ChronoDuration::minutes(1))
            .await,
        Err(CredentialRepositoryError::Internal),
        "hydration rejects overlong access expiry even without the database constraint"
    );

    sqlx::query(
        "UPDATE sessions SET expires_at = credential_issued_at + interval '15 minutes', \
         refresh_idle_expires_at = credential_issued_at + interval '31 days' WHERE id = $1",
    )
    .bind(session.id)
    .execute(pool)
    .await
    .expect("tamper refresh-idle expiry fixture");
    assert_eq!(
        repository
            .authenticate_device_access(&access, now + ChronoDuration::minutes(2))
            .await,
        Err(CredentialRepositoryError::Internal),
        "hydration rejects overlong refresh-idle expiry"
    );

    sqlx::query(
        "UPDATE sessions SET refresh_idle_expires_at = credential_issued_at + interval '30 days', \
         absolute_expires_at = created_at + interval '181 days' WHERE id = $1",
    )
    .bind(session.id)
    .execute(pool)
    .await
    .expect("tamper absolute expiry fixture");
    assert_eq!(
        repository
            .authenticate_device_access(&access, now + ChronoDuration::minutes(3))
            .await,
        Err(CredentialRepositoryError::Internal),
        "hydration rejects overlong absolute expiry"
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Covers scope, tenancy, expiry, and revocation together.
async fn mcp_credentials_enforce_scopes_ttl_revocation_and_hash_only_storage() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-mcp-owner", "auth-mcp").await;
    let other_scope = seed_scope(pool, "auth-mcp-other", "auth-mcp-other").await;
    let repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let other_repository = PostgresCredentialRepository::new(pool.clone(), other_scope);
    let now = Utc::now();
    let client_id = Uuid::new_v4();
    let credential_raw = token(CredentialKind::McpClient, 31);
    let credential = OpaqueCredential::parse(CredentialKind::McpClient, &credential_raw).unwrap();
    let client = repository
        .register_mcp_client(
            McpClientSpec {
                id: client_id,
                client_identifier: "personal-chat-client".to_owned(),
                display_name: "Personal chat suggestions".to_owned(),
                scopes: vec![Scope::ScheduleRead, Scope::SuggestionsSubmit],
                allowed_origins: vec!["https://assistant.example.test".to_owned()],
                client_contract_version: 1,
                client_version: "test-1".to_owned(),
                client_capabilities: vec!["origin-binding".to_owned()],
                created_at: now,
                requested_expires_at: None,
            },
            &credential,
        )
        .await
        .expect("MCP client registered");
    let listed_clients = repository
        .list_mcp_clients(now)
        .await
        .expect("active MCP clients listed");
    assert_eq!(listed_clients.len(), 1);
    assert_eq!(listed_clients[0].id, client_id);
    assert_postgres_instant_eq(client.expires_at, now + ChronoDuration::days(90));
    assert_eq!(
        client.scopes,
        vec![Scope::ScheduleRead, Scope::SuggestionsSubmit]
    );
    assert_eq!(client.workspace_id, scope.workspace_id);
    assert_eq!(client.user_id, scope.user_id);

    let authenticated = repository
        .authenticate_mcp_client(&credential, now + ChronoDuration::minutes(1))
        .await
        .expect("active MCP credential");
    assert_eq!(authenticated.id, client_id);
    assert_eq!(
        authenticated
            .last_seen_at
            .map(|value| value.timestamp_micros()),
        Some((now + ChronoDuration::minutes(1)).timestamp_micros())
    );
    assert_eq!(
        other_repository
            .authenticate_mcp_client(&credential, now + ChronoDuration::minutes(1))
            .await,
        Err(CredentialRepositoryError::InvalidCredential)
    );

    sqlx::query(
        "UPDATE mcp_clients SET allowed_origins = ARRAY['http://public.example.test'] \
         WHERE id = $1",
    )
    .bind(client_id)
    .execute(pool)
    .await
    .expect("tamper MCP origin fixture");
    assert_eq!(
        repository
            .authenticate_mcp_client(&credential, now + ChronoDuration::minutes(1))
            .await,
        Err(CredentialRepositoryError::Internal),
        "malformed durable origins fail closed"
    );
    sqlx::query(
        "UPDATE mcp_clients SET allowed_origins = \
         ARRAY['https://assistant.example.test', 'https://assistant.example.test'] \
         WHERE id = $1",
    )
    .bind(client_id)
    .execute(pool)
    .await
    .expect("duplicate MCP origin fixture");
    assert_eq!(
        repository
            .authenticate_mcp_client(&credential, now + ChronoDuration::minutes(1))
            .await,
        Err(CredentialRepositoryError::Internal),
        "duplicate durable origins fail closed"
    );
    sqlx::query(
        "UPDATE mcp_clients SET allowed_origins = ARRAY['https://assistant.example.test'], \
         display_name = E'bad\\nlabel' WHERE id = $1",
    )
    .bind(client_id)
    .execute(pool)
    .await
    .expect("tamper MCP label fixture");
    assert_eq!(
        repository
            .authenticate_mcp_client(&credential, now + ChronoDuration::minutes(1))
            .await,
        Err(CredentialRepositoryError::Internal),
        "malformed durable labels fail closed"
    );
    sqlx::query("UPDATE mcp_clients SET display_name = 'Personal chat suggestions' WHERE id = $1")
        .bind(client_id)
        .execute(pool)
        .await
        .expect("restore MCP label fixture");

    let stored_hash: Vec<u8> =
        sqlx::query_scalar("SELECT credential_hash FROM mcp_clients WHERE id = $1")
            .bind(client_id)
            .fetch_one(pool)
            .await
            .expect("stored MCP hash");
    assert_eq!(stored_hash.len(), 32);
    assert_ne!(stored_hash.as_slice(), credential_raw.as_bytes());

    assert!(
        repository
            .revoke_mcp_client(client_id, now + ChronoDuration::minutes(2))
            .await
            .expect("MCP credential revoked")
    );
    assert!(
        !repository
            .revoke_mcp_client(client_id, now + ChronoDuration::minutes(3))
            .await
            .expect("MCP revocation is idempotent")
    );
    assert_eq!(
        repository
            .authenticate_mcp_client(&credential, now + ChronoDuration::minutes(3))
            .await,
        Err(CredentialRepositoryError::InvalidCredential)
    );

    let expired_raw = token(CredentialKind::McpClient, 32);
    let expired = OpaqueCredential::parse(CredentialKind::McpClient, &expired_raw).unwrap();
    let expired_client = repository
        .register_mcp_client(
            McpClientSpec {
                id: Uuid::new_v4(),
                client_identifier: "short-lived-client".to_owned(),
                display_name: "Short lived".to_owned(),
                scopes: vec![Scope::SuggestionsSubmit],
                allowed_origins: Vec::new(),
                client_contract_version: 1,
                client_version: "test-1".to_owned(),
                client_capabilities: Vec::new(),
                created_at: now,
                requested_expires_at: Some(now + ChronoDuration::minutes(1)),
            },
            &expired,
        )
        .await
        .expect("short-lived MCP credential");
    assert_eq!(
        repository
            .authenticate_mcp_client(&expired, expired_client.expires_at)
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "MCP expiry is exclusive"
    );

    let too_long_raw = token(CredentialKind::McpClient, 33);
    let too_long = OpaqueCredential::parse(CredentialKind::McpClient, &too_long_raw).unwrap();
    assert_eq!(
        repository
            .register_mcp_client(
                McpClientSpec {
                    id: Uuid::new_v4(),
                    client_identifier: "overlong-client".to_owned(),
                    display_name: "Overlong".to_owned(),
                    scopes: vec![Scope::ScheduleRead],
                    allowed_origins: Vec::new(),
                    client_contract_version: 1,
                    client_version: "test-1".to_owned(),
                    client_capabilities: Vec::new(),
                    created_at: now,
                    requested_expires_at: Some(
                        now + ChronoDuration::days(365) + ChronoDuration::seconds(1)
                    ),
                },
                &too_long,
            )
            .await,
        Err(CredentialRepositoryError::InvalidInput)
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Exercises the complete public/protected auth HTTP lifecycle.
async fn auth_http_runtime_issues_replays_lists_and_revokes_without_echoing_device_secrets() {
    const STATIC_TOKEN: &str = "synthetic-static-bootstrap-token-for-tests";
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-http-owner", "auth-http").await;
    let repository: Arc<dyn CredentialRepository> =
        Arc::new(PostgresCredentialRepository::new(pool.clone(), scope));
    let clock = Arc::new(SystemClock);
    let authenticator = Arc::new(RuntimeAuthenticator::new(
        Some(Arc::new(vec![hash_token(STATIC_TOKEN)])),
        repository.clone(),
        clock.clone(),
    ));
    let proposal_repository: Arc<dyn ProposalRepository> =
        Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposal_repository,
        clock,
        Duration::from_hours(24),
    ));
    let app =
        router(
            AppState::new(proposals, authenticator.clone(), Readiness::default())
                .with_credential_auth(repository, authenticator, AuthMode::Hybrid),
        );

    let enrollment_id = Uuid::new_v4();
    let client_instance_id = Uuid::new_v4();
    let proposed_enrollment_token = token(CredentialKind::Enrollment, 80);
    let enrollment_body = json!({
        "id": enrollment_id,
        "enrollment_token": proposed_enrollment_token,
        "client_instance_id": client_instance_id,
        "client_kind": "macos",
        "device_label": "Synthetic HTTP Mac",
        "client_contract_version": DEVICE_CLIENT_CONTRACT_VERSION,
        "client_version": "test-1",
        "client_capabilities": ["exact-replay"]
    });
    let enrollment_response = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/device-enrollments",
            STATIC_TOKEN,
            Some(enrollment_body.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(enrollment_response.status(), StatusCode::CREATED);
    assert_eq!(
        enrollment_response.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let enrollment = response_json(enrollment_response).await;
    let enrollment_token = enrollment["enrollment_token"].as_str().unwrap().to_owned();
    assert_eq!(enrollment["id"], enrollment_id.to_string());
    assert_eq!(enrollment["replayed"], false);
    assert_eq!(
        enrollment["client_contract_version"],
        DEVICE_CLIENT_CONTRACT_VERSION
    );
    assert_eq!(enrollment_token, proposed_enrollment_token);
    let enrollment_expires_at = enrollment["expires_at"].clone();

    let replayed_creation = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/device-enrollments",
            STATIC_TOKEN,
            Some(enrollment_body.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(replayed_creation.status(), StatusCode::OK);
    let replayed_creation = response_json(replayed_creation).await;
    assert_eq!(replayed_creation["id"], enrollment_id.to_string());
    assert_eq!(replayed_creation["enrollment_token"], enrollment_token);
    assert_eq!(replayed_creation["expires_at"], enrollment_expires_at);
    assert_eq!(
        replayed_creation["client_contract_version"],
        DEVICE_CLIENT_CONTRACT_VERSION
    );
    assert_eq!(replayed_creation["replayed"], true);

    let mut conflicting_enrollment_body = enrollment_body;
    conflicting_enrollment_body["device_label"] = json!("Different synthetic Mac");
    let conflicting_creation = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/device-enrollments",
            STATIC_TOKEN,
            Some(conflicting_enrollment_body),
        ))
        .await
        .unwrap();
    assert_eq!(conflicting_creation.status(), StatusCode::CONFLICT);
    let conflicting_creation_text = String::from_utf8(
        conflicting_creation
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(!conflicting_creation_text.contains(&enrollment_token));

    let stored_enrollment_hash: Vec<u8> = sqlx::query_scalar(
        "SELECT token_hash FROM device_enrollments WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(enrollment_id)
    .fetch_one(pool)
    .await
    .expect("proposed enrollment stored by digest");
    assert_eq!(stored_enrollment_hash.len(), 32);
    assert_ne!(stored_enrollment_hash, enrollment_token.as_bytes());

    let session_id = Uuid::new_v4();
    let access = token(CredentialKind::DeviceAccess, 81);
    let refresh = token(CredentialKind::DeviceRefresh, 82);
    let consume_body = json!({
        "session_id": session_id,
        "access_token": access,
        "refresh_token": refresh
    });
    let consumed_response = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/device-enrollments/consume",
            &enrollment_token,
            Some(consume_body.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(consumed_response.status(), StatusCode::CREATED);
    let consumed_bytes = consumed_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let consumed_text = String::from_utf8(consumed_bytes.to_vec()).unwrap();
    assert!(!consumed_text.contains(&access));
    assert!(!consumed_text.contains(&refresh));
    assert_eq!(
        serde_json::from_str::<Value>(&consumed_text).unwrap()["replayed"],
        false
    );

    let replayed_enrollment = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/device-enrollments/consume",
            &enrollment_token,
            Some(consume_body),
        ))
        .await
        .unwrap();
    assert_eq!(replayed_enrollment.status(), StatusCode::OK);
    assert_eq!(response_json(replayed_enrollment).await["replayed"], true);

    let next_access = token(CredentialKind::DeviceAccess, 83);
    let next_refresh = token(CredentialKind::DeviceRefresh, 84);
    let refresh_body = json!({
        "next_access_token": next_access,
        "next_refresh_token": next_refresh
    });
    let refreshed = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/sessions/refresh",
            &refresh,
            Some(refresh_body.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(refreshed.status(), StatusCode::OK);
    assert_eq!(response_json(refreshed).await["replayed"], false);
    let replayed_refresh = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/sessions/refresh",
            &refresh,
            Some(refresh_body),
        ))
        .await
        .unwrap();
    assert_eq!(replayed_refresh.status(), StatusCode::OK);
    assert_eq!(response_json(replayed_refresh).await["replayed"], true);

    let listed = app
        .clone()
        .oneshot(auth_request("GET", "/v1/auth/sessions", &next_access, None))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(
        response_json(listed).await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let mcp_issued = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/mcp-clients",
            STATIC_TOKEN,
            Some(json!({
                "client_identifier": "synthetic-http-mcp",
                "display_name": "Synthetic HTTP MCP",
                "scopes": ["schedule_read"],
                "allowed_origins": ["https://chatgpt.com"],
                "client_contract_version": 1,
                "client_version": "test-1"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(mcp_issued.status(), StatusCode::CREATED);
    let mcp_issued = response_json(mcp_issued).await;
    let mcp_credential = mcp_issued["credential"].as_str().unwrap().to_owned();
    assert!(mcp_credential.starts_with(CredentialKind::McpClient.prefix()));
    let mcp_id = mcp_issued["client"]["id"].as_str().unwrap().to_owned();
    let listed_mcp = app
        .clone()
        .oneshot(auth_request(
            "GET",
            "/v1/auth/mcp-clients",
            STATIC_TOKEN,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(listed_mcp.status(), StatusCode::OK);
    let listed_mcp = listed_mcp.into_body().collect().await.unwrap().to_bytes();
    let listed_mcp = String::from_utf8(listed_mcp.to_vec()).unwrap();
    assert!(!listed_mcp.contains(&mcp_credential));
    assert_eq!(
        serde_json::from_str::<Value>(&listed_mcp).unwrap()["clients"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let revoked_mcp = app
        .clone()
        .oneshot(auth_request(
            "DELETE",
            &format!("/v1/auth/mcp-clients/{mcp_id}"),
            STATIC_TOKEN,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked_mcp.status(), StatusCode::NO_CONTENT);

    let revoked_session = app
        .clone()
        .oneshot(auth_request(
            "DELETE",
            &format!("/v1/auth/sessions/{session_id}"),
            &next_access,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked_session.status(), StatusCode::NO_CONTENT);
    let rejected = app
        .oneshot(auth_request("GET", "/v1/items", &next_access, None))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Covers the complete two-device inventory and remote-revocation contract.
async fn two_device_inventory_and_remote_revocation_are_scoped_ordered_and_secret_free() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-inventory-owner", "auth-inventory").await;
    let postgres_repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let base = DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("current timestamp")
        - ChronoDuration::minutes(5);
    let shared_label = "Owner device";
    let mac = issue_device_session_with_metadata(
        &postgres_repository,
        base,
        160,
        DeviceClientKind::Macos,
        shared_label,
        vec![
            Scope::ItemsRead,
            Scope::AuthSessionsRead,
            Scope::AuthSessionsWrite,
        ],
        "macos-inventory-1",
        vec!["session-inventory".to_owned()],
    )
    .await;
    let android = issue_device_session_with_metadata(
        &postgres_repository,
        base + ChronoDuration::seconds(1),
        170,
        DeviceClientKind::Android,
        shared_label,
        vec![Scope::ItemsRead, Scope::AuthSessionsRead],
        "android-inventory-1",
        vec!["session-inventory".to_owned()],
    )
    .await;

    let repository: Arc<dyn CredentialRepository> = Arc::new(postgres_repository.clone());
    let clock = Arc::new(SystemClock);
    let authenticator = Arc::new(RuntimeAuthenticator::new(
        None,
        repository.clone(),
        clock.clone(),
    ));
    let proposal_repository: Arc<dyn ProposalRepository> =
        Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposal_repository,
        clock,
        Duration::from_hours(24),
    ));
    let app =
        router(
            AppState::new(proposals, authenticator.clone(), Readiness::default())
                .with_credential_auth(repository, authenticator, AuthMode::CredentialOnly),
        );

    let listed_response = app
        .clone()
        .oneshot(auth_request(
            "GET",
            "/v1/auth/sessions",
            &android.access_raw,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(listed_response.status(), StatusCode::OK);
    assert_eq!(
        listed_response.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let listed_bytes = listed_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let listed_text = String::from_utf8(listed_bytes.to_vec()).unwrap();
    for secret in [
        &mac.enrollment_raw,
        &mac.access_raw,
        &mac.refresh_raw,
        &android.enrollment_raw,
        &android.access_raw,
        &android.refresh_raw,
    ] {
        assert!(
            !listed_text.contains(secret),
            "session inventory must never echo credential plaintext"
        );
    }
    let listed: Value = serde_json::from_str(&listed_text).unwrap();
    assert_eq!(listed.as_object().expect("inventory envelope").len(), 1);
    let sessions = listed["sessions"].as_array().expect("session array");
    assert_eq!(sessions.len(), 2);

    let android_last_seen: DateTime<Utc> = sqlx::query_scalar(
        "SELECT last_seen_at FROM sessions WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(android.session.id)
    .fetch_one(pool)
    .await
    .expect("authenticated Android last-seen timestamp");
    let mut expected_android = android.session.clone();
    expected_android.last_seen_at = android_last_seen;
    assert_device_session_json(&sessions[0], &expected_android);
    assert_device_session_json(&sessions[1], &mac.session);
    assert_eq!(
        sessions[0]["id"],
        android.session.id.to_string(),
        "the caller's locally stored session UUID is the current-device basis"
    );
    assert_eq!(sessions[0]["device_label"], sessions[1]["device_label"]);

    let reader_cannot_revoke = app
        .clone()
        .oneshot(auth_request(
            "DELETE",
            &format!("/v1/auth/sessions/{}", mac.session.id),
            &android.access_raw,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(reader_cannot_revoke.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response_json(reader_cannot_revoke).await["error"]["code"],
        "forbidden"
    );

    let revoked = app
        .clone()
        .oneshot(auth_request(
            "DELETE",
            &format!("/v1/auth/sessions/{}", android.session.id),
            &mac.access_raw,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        revoked.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    assert!(
        revoked
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    let rejected_access = app
        .clone()
        .oneshot(auth_request("GET", "/v1/items", &android.access_raw, None))
        .await
        .unwrap();
    assert_eq!(rejected_access.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response_json(rejected_access).await["error"]["code"],
        "unauthorized"
    );

    let next_access = token(CredentialKind::DeviceAccess, 180);
    let next_refresh = token(CredentialKind::DeviceRefresh, 181);
    let rejected_refresh = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/sessions/refresh",
            &android.refresh_raw,
            Some(json!({
                "next_access_token": next_access,
                "next_refresh_token": next_refresh,
            })),
        ))
        .await
        .unwrap();
    assert_eq!(rejected_refresh.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response_json(rejected_refresh).await["error"]["code"],
        "unauthorized"
    );

    let relisted = app
        .oneshot(auth_request(
            "GET",
            "/v1/auth/sessions",
            &mac.access_raw,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(relisted.status(), StatusCode::OK);
    let relisted = response_json(relisted).await;
    let remaining = relisted["sessions"].as_array().expect("remaining sessions");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["id"], mac.session.id.to_string());
    assert!(
        remaining
            .iter()
            .all(|session| session["id"] != android.session.id.to_string()),
        "the remotely revoked session must be absent from active inventory"
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Exercises the complete transactional cap and HTTP failure contract.
async fn active_device_session_capacity_serializes_consumption_and_keeps_recovery_paths() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-session-cap-owner", "auth-session-cap").await;
    let postgres_repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let base = DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("current timestamp")
        - ChronoDuration::minutes(1);

    let mut active = Vec::new();
    for index in 0..(MAX_ACTIVE_DEVICE_SESSIONS - 1) {
        let marker = u8::try_from(index * 3 + 1).expect("fixture marker range");
        let scopes = if index == 0 {
            vec![Scope::ScheduleRead, Scope::AuthSessionsRead]
        } else {
            vec![Scope::ScheduleRead]
        };
        active.push(
            issue_device_session_with_metadata(
                &postgres_repository,
                base,
                marker,
                DeviceClientKind::Macos,
                &format!("Capacity device {index}"),
                scopes,
                "capacity-test-1",
                vec!["session-capacity".to_owned()],
            )
            .await,
        );
    }

    let first = pending_device_enrollment_fixture(
        base + ChronoDuration::seconds(1),
        100,
        Uuid::new_v4(),
        DeviceClientKind::Android,
        "Concurrent contender A",
        vec![Scope::ScheduleRead],
    );
    let second = pending_device_enrollment_fixture(
        base + ChronoDuration::seconds(1),
        110,
        Uuid::new_v4(),
        DeviceClientKind::Android,
        "Concurrent contender B",
        vec![Scope::ScheduleRead],
    );
    persist_pending_device_enrollment(&postgres_repository, &first).await;
    persist_pending_device_enrollment(&postgres_repository, &second).await;

    let issuance_time = base + ChronoDuration::seconds(2);
    let (first_result, second_result) = tokio::join!(
        consume_pending_device_enrollment(&postgres_repository, &first, issuance_time),
        consume_pending_device_enrollment(&postgres_repository, &second, issuance_time),
    );
    let (winner, loser, issued) = match (first_result, second_result) {
        (Ok(issued), Err(CredentialRepositoryError::Conflict)) => (&first, &second, issued),
        (Err(CredentialRepositoryError::Conflict), Ok(issued)) => (&second, &first, issued),
        unexpected => panic!("exactly one concurrent consumption must win: {unexpected:?}"),
    };
    assert!(!issued.replayed);
    assert_eq!(issued.id, winner.session_id);

    let at_capacity = postgres_repository
        .list_device_sessions(issuance_time)
        .await
        .expect("bounded active inventory");
    assert_eq!(at_capacity.len(), MAX_ACTIVE_DEVICE_SESSIONS);
    assert!(
        at_capacity
            .iter()
            .any(|session| session.id == winner.session_id)
    );
    assert!(
        at_capacity
            .iter()
            .all(|session| session.id != loser.session_id)
    );

    let replay = consume_pending_device_enrollment(
        &postgres_repository,
        winner,
        issuance_time + ChronoDuration::seconds(1),
    )
    .await
    .expect("exact committed consume remains recoverable at capacity");
    assert!(replay.replayed);
    assert_eq!(replay.id, winner.session_id);

    let repository: Arc<dyn CredentialRepository> = Arc::new(postgres_repository.clone());
    let clock = Arc::new(SystemClock);
    let authenticator = Arc::new(RuntimeAuthenticator::new(
        None,
        repository.clone(),
        clock.clone(),
    ));
    let proposal_repository: Arc<dyn ProposalRepository> =
        Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposal_repository,
        clock,
        Duration::from_hours(24),
    ));
    let app =
        router(
            AppState::new(proposals, authenticator.clone(), Readiness::default())
                .with_credential_auth(repository, authenticator, AuthMode::CredentialOnly),
        );

    let rejected = app
        .clone()
        .oneshot(auth_request(
            "POST",
            "/v1/auth/device-enrollments/consume",
            &loser.enrollment_raw,
            Some(json!({
                "session_id": loser.session_id,
                "access_token": loser.access_raw,
                "refresh_token": loser.refresh_raw,
            })),
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::CONFLICT);
    assert_eq!(response_json(rejected).await["error"]["code"], "conflict");

    let replacement = pending_device_enrollment_fixture(
        issuance_time + ChronoDuration::seconds(2),
        120,
        active[1].session.client_instance_id,
        DeviceClientKind::Macos,
        "Replacement installation",
        vec![Scope::ScheduleRead],
    );
    persist_pending_device_enrollment(&postgres_repository, &replacement).await;
    let replacement_result = consume_pending_device_enrollment(
        &postgres_repository,
        &replacement,
        issuance_time + ChronoDuration::seconds(3),
    )
    .await
    .expect("same-installation replacement remains available at capacity");
    assert!(!replacement_result.replayed);
    let after_replacement = postgres_repository
        .list_device_sessions(issuance_time + ChronoDuration::seconds(3))
        .await
        .expect("bounded inventory after replacement");
    assert_eq!(after_replacement.len(), MAX_ACTIVE_DEVICE_SESSIONS);
    assert!(
        after_replacement
            .iter()
            .all(|session| session.id != active[1].session.id)
    );
    assert!(
        after_replacement
            .iter()
            .any(|session| session.id == replacement.session_id)
    );
    let replacement_replay = consume_pending_device_enrollment(
        &postgres_repository,
        &replacement,
        issuance_time + ChronoDuration::seconds(4),
    )
    .await
    .expect("replacement response-loss replay remains available at capacity");
    assert!(replacement_replay.replayed);

    let after_access_expiry = issuance_time + ACCESS_TOKEN_TTL + ChronoDuration::seconds(5);
    assert_eq!(
        postgres_repository
            .list_device_sessions(after_access_expiry)
            .await
            .expect("refreshable sessions outlive their access credentials")
            .len(),
        MAX_ACTIVE_DEVICE_SESSIONS
    );
    let after_access_expiry_contender = pending_device_enrollment_fixture(
        after_access_expiry,
        130,
        Uuid::new_v4(),
        DeviceClientKind::Android,
        "Contender after access expiry",
        vec![Scope::ScheduleRead],
    );
    persist_pending_device_enrollment(&postgres_repository, &after_access_expiry_contender).await;
    assert_eq!(
        consume_pending_device_enrollment(
            &postgres_repository,
            &after_access_expiry_contender,
            after_access_expiry,
        )
        .await,
        Err(CredentialRepositoryError::Conflict),
        "access-token expiry must not free a refreshable authority slot"
    );

    // Simulate a historical invariant violation without defining a migration
    // that would silently discard authority. Listing must error, never truncate.
    let overflow_access_hash = hash_token("historical-overflow-access");
    let overflow_refresh_hash = hash_token("historical-overflow-refresh");
    sqlx::query(
        "INSERT INTO sessions (id, workspace_id, user_id, token_hash, client_kind, \
         device_label, metadata, created_at, last_seen_at, expires_at, auth_version, \
         client_instance_id, refresh_token_hash, scopes, refresh_idle_expires_at, \
         absolute_expires_at, credential_issued_at, revision, client_contract_version, \
         client_version, client_capabilities) \
         SELECT $1, workspace_id, user_id, $2, client_kind, device_label, metadata, created_at, \
         last_seen_at, expires_at, auth_version, $3, $4, scopes, refresh_idle_expires_at, \
         absolute_expires_at, credential_issued_at, revision, client_contract_version, \
         client_version, client_capabilities FROM sessions WHERE id = $5",
    )
    .bind(Uuid::new_v4())
    .bind(overflow_access_hash.as_slice())
    .bind(Uuid::new_v4())
    .bind(overflow_refresh_hash.as_slice())
    .bind(active[0].session.id)
    .execute(pool)
    .await
    .expect("historical overflow fixture inserted");
    assert_eq!(
        postgres_repository
            .list_device_sessions(issuance_time + ChronoDuration::seconds(4))
            .await,
        Err(CredentialRepositoryError::Internal),
        "a legacy overflow must fail closed instead of hiding sessions"
    );
    let overflow_response = app
        .oneshot(auth_request(
            "GET",
            "/v1/auth/sessions",
            &active[0].access_raw,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        overflow_response.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keeps the concurrent, replay, revocation, and expiry boundaries together.
async fn pending_device_enrollment_capacity_is_live_bounded_and_concurrent_safe() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "auth-pending-cap-owner", "auth-pending-cap").await;
    let repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let base = DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("current timestamp");

    let mut pending = Vec::new();
    for index in 0..(MAX_PENDING_DEVICE_ENROLLMENTS - 1) {
        let fixture = pending_device_enrollment_fixture(
            base,
            u8::try_from(index * 3 + 1).expect("fixture marker range"),
            Uuid::new_v4(),
            DeviceClientKind::Android,
            &format!("Pending device {index}"),
            vec![Scope::ScheduleRead],
        );
        persist_pending_device_enrollment(&repository, &fixture).await;
        pending.push(fixture);
    }

    let first = pending_device_enrollment_fixture(
        base,
        100,
        Uuid::new_v4(),
        DeviceClientKind::Macos,
        "Pending contender A",
        vec![Scope::ScheduleRead],
    );
    let second = pending_device_enrollment_fixture(
        base,
        110,
        Uuid::new_v4(),
        DeviceClientKind::Macos,
        "Pending contender B",
        vec![Scope::ScheduleRead],
    );
    let first_token =
        OpaqueCredential::parse(CredentialKind::Enrollment, &first.enrollment_raw).unwrap();
    let second_token =
        OpaqueCredential::parse(CredentialKind::Enrollment, &second.enrollment_raw).unwrap();
    let (first_result, second_result) = tokio::join!(
        repository.create_or_replay_device_enrollment(first.spec.clone(), &first_token),
        repository.create_or_replay_device_enrollment(second.spec.clone(), &second_token),
    );
    let (winner, loser) = match (first_result, second_result) {
        (Ok(created), Err(CredentialRepositoryError::Conflict)) => {
            assert!(!created.replayed);
            (&first, &second)
        }
        (Err(CredentialRepositoryError::Conflict), Ok(created)) => {
            assert!(!created.replayed);
            (&second, &first)
        }
        unexpected => panic!("exactly one concurrent pending creation must win: {unexpected:?}"),
    };
    let live_pending = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM device_enrollments WHERE workspace_id = $1 AND user_id = $2 \
         AND consumed_at IS NULL AND revoked_at IS NULL AND expires_at > $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(base)
    .fetch_one(pool)
    .await
    .expect("pending enrollment count");
    assert_eq!(
        usize::try_from(live_pending).expect("nonnegative pending count"),
        MAX_PENDING_DEVICE_ENROLLMENTS
    );

    let winner_token =
        OpaqueCredential::parse(CredentialKind::Enrollment, &winner.enrollment_raw).unwrap();
    let mut replay_spec = winner.spec.clone();
    replay_spec.created_at = base + ChronoDuration::seconds(1);
    let replay = repository
        .create_or_replay_device_enrollment(replay_spec, &winner_token)
        .await
        .expect("exact pending request remains replayable at capacity");
    assert!(replay.replayed);
    assert_postgres_instant_eq(replay.expires_at, base + ChronoDuration::minutes(10));

    let loser_token =
        OpaqueCredential::parse(CredentialKind::Enrollment, &loser.enrollment_raw).unwrap();
    assert_eq!(
        repository
            .create_device_enrollment(loser.spec.clone(), &loser_token)
            .await,
        Err(CredentialRepositoryError::Conflict),
        "the non-replay repository entry point enforces the same pending cap"
    );

    assert!(
        repository
            .revoke_device_enrollment(pending[0].spec.id, base + ChronoDuration::seconds(1))
            .await
            .expect("one pending authority revoked")
    );
    let admitted = repository
        .create_or_replay_device_enrollment(loser.spec.clone(), &loser_token)
        .await
        .expect("revocation frees one pending slot");
    assert!(!admitted.replayed);

    let after_expiry = pending_device_enrollment_fixture(
        base + ChronoDuration::minutes(11),
        120,
        Uuid::new_v4(),
        DeviceClientKind::Android,
        "Pending after expiry",
        vec![Scope::ScheduleRead],
    );
    persist_pending_device_enrollment(&repository, &after_expiry).await;

    test_database.destroy().await;
}

struct IssuedDeviceSession {
    session: DeviceSession,
    enrollment_raw: String,
    access_raw: String,
    refresh_raw: String,
}

struct PendingDeviceEnrollment {
    spec: DeviceEnrollmentSpec,
    session_id: Uuid,
    enrollment_raw: String,
    access_raw: String,
    refresh_raw: String,
}

fn pending_device_enrollment_fixture(
    now: DateTime<Utc>,
    marker: u8,
    client_instance_id: Uuid,
    client_kind: DeviceClientKind,
    device_label: &str,
    scopes: Vec<Scope>,
) -> PendingDeviceEnrollment {
    PendingDeviceEnrollment {
        spec: DeviceEnrollmentSpec {
            id: Uuid::new_v4(),
            client_instance_id,
            client_kind,
            device_label: device_label.to_owned(),
            scopes,
            client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
            client_version: "capacity-test-1".to_owned(),
            client_capabilities: vec!["session-capacity".to_owned()],
            created_at: now,
        },
        session_id: Uuid::new_v4(),
        enrollment_raw: token(CredentialKind::Enrollment, marker),
        access_raw: token(
            CredentialKind::DeviceAccess,
            marker.checked_add(1).expect("fixture marker range"),
        ),
        refresh_raw: token(
            CredentialKind::DeviceRefresh,
            marker.checked_add(2).expect("fixture marker range"),
        ),
    }
}

async fn persist_pending_device_enrollment(
    repository: &PostgresCredentialRepository,
    pending: &PendingDeviceEnrollment,
) {
    let enrollment =
        OpaqueCredential::parse(CredentialKind::Enrollment, &pending.enrollment_raw).unwrap();
    repository
        .create_device_enrollment(pending.spec.clone(), &enrollment)
        .await
        .expect("pending enrollment created");
}

async fn consume_pending_device_enrollment(
    repository: &PostgresCredentialRepository,
    pending: &PendingDeviceEnrollment,
    now: DateTime<Utc>,
) -> Result<CredentialMutation<DeviceSession>, CredentialRepositoryError> {
    let enrollment =
        OpaqueCredential::parse(CredentialKind::Enrollment, &pending.enrollment_raw).unwrap();
    let access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &pending.access_raw).unwrap();
    let refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &pending.refresh_raw).unwrap();
    repository
        .consume_device_enrollment(&enrollment, pending.session_id, &access, &refresh, now)
        .await
}

async fn issue_device_session(
    repository: &PostgresCredentialRepository,
    now: DateTime<Utc>,
    marker: u8,
    device_label: &str,
) -> IssuedDeviceSession {
    issue_device_session_with_metadata(
        repository,
        now,
        marker,
        DeviceClientKind::Macos,
        device_label,
        vec![Scope::ScheduleRead],
        "replay-boundary-test-1",
        vec!["exact-replay".to_owned()],
    )
    .await
}

#[allow(clippy::too_many_arguments)] // Test helper keeps every client-visible identity field explicit.
async fn issue_device_session_with_metadata(
    repository: &PostgresCredentialRepository,
    now: DateTime<Utc>,
    marker: u8,
    client_kind: DeviceClientKind,
    device_label: &str,
    scopes: Vec<Scope>,
    client_version: &str,
    client_capabilities: Vec<String>,
) -> IssuedDeviceSession {
    let enrollment_raw = token(CredentialKind::Enrollment, marker);
    let access_raw = token(
        CredentialKind::DeviceAccess,
        marker.checked_add(1).expect("fixture marker range"),
    );
    let refresh_raw = token(
        CredentialKind::DeviceRefresh,
        marker.checked_add(2).expect("fixture marker range"),
    );
    let enrollment = OpaqueCredential::parse(CredentialKind::Enrollment, &enrollment_raw).unwrap();
    let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
    let refresh = OpaqueCredential::parse(CredentialKind::DeviceRefresh, &refresh_raw).unwrap();
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: Uuid::new_v4(),
                client_instance_id: Uuid::new_v4(),
                client_kind,
                device_label: device_label.to_owned(),
                scopes,
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: client_version.to_owned(),
                client_capabilities,
                created_at: now,
            },
            &enrollment,
        )
        .await
        .expect("replay fixture enrollment created");
    let session = repository
        .consume_device_enrollment(&enrollment, Uuid::new_v4(), &access, &refresh, now)
        .await
        .expect("replay fixture session issued");
    assert!(!session.replayed);
    assert_postgres_instant_eq(session.access_expires_at, now + ACCESS_TOKEN_TTL);
    assert_postgres_instant_eq(
        session.refresh_idle_expires_at,
        now + DEVICE_SESSION_REFRESH_IDLE_TTL,
    );
    assert_postgres_instant_eq(
        session.absolute_expires_at,
        now + DEVICE_SESSION_ABSOLUTE_TTL,
    );
    IssuedDeviceSession {
        session: session.value,
        enrollment_raw,
        access_raw,
        refresh_raw,
    }
}

fn auth_request(method: &str, uri: &str, bearer: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

async fn response_json(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn assert_device_session_json(actual: &Value, expected: &DeviceSession) {
    assert_eq!(
        actual,
        &json!({
            "id": expected.id,
            "client_instance_id": expected.client_instance_id,
            "client_kind": expected.client_kind,
            "device_label": expected.device_label,
            "scopes": expected.scopes,
            "client_contract_version": expected.client_contract_version,
            "client_version": expected.client_version,
            "client_capabilities": expected.client_capabilities,
            "created_at": expected.created_at,
            "last_seen_at": expected.last_seen_at,
            "credential_issued_at": expected.credential_issued_at,
            "access_expires_at": expected.access_expires_at,
            "refresh_idle_expires_at": expected.refresh_idle_expires_at,
            "absolute_expires_at": expected.absolute_expires_at,
            "revision": expected.revision,
        }),
        "session inventory must contain the exact secret-free metadata contract"
    );
}

fn token(kind: CredentialKind, marker: u8) -> String {
    format!("{}{}", kind.prefix(), URL_SAFE_NO_PAD.encode([marker; 32]))
}

fn assert_postgres_code(error: &sqlx::Error, expected: &str) {
    assert!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .is_some_and(|code| code == expected),
        "unexpected PostgreSQL error classification"
    );
}

fn assert_postgres_instant_eq(actual: DateTime<Utc>, expected: DateTime<Utc>) {
    assert_eq!(
        actual.timestamp_micros(),
        expected.timestamp_micros(),
        "PostgreSQL timestamps round to microsecond precision"
    );
}

struct TestDatabase {
    admin: PgPool,
    pool: PgPool,
    schema: String,
}

impl TestDatabase {
    async fn from_environment() -> Option<Self> {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; credential PostgreSQL test skipped");
            return None;
        };
        let options = PgConnectOptions::from_str(&database_url)
            .expect("valid DAYWEAVE_TEST_DATABASE_URL")
            .disable_statement_logging();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("connect test PostgreSQL");
        let schema = format!("dayweave_auth_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .expect("create isolated credential test schema");
        let connection_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .after_connect(move |connection, _| {
                let statement = format!("SET search_path TO {connection_schema}");
                Box::pin(async move {
                    connection.execute(AssertSqlSafe(statement)).await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .expect("connect isolated credential test pool");
        Some(Self {
            admin,
            pool,
            schema,
        })
    }

    async fn destroy(self) {
        self.pool.close().await;
        self.admin
            .execute(AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .await
            .expect("drop isolated credential test schema");
        self.admin.close().await;
    }
}

async fn seed_scope(pool: &PgPool, subject: &str, slug: &str) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
         VALUES ($1, $2, 'Credential test owner', 'Europe/Madrid')",
    )
    .bind(scope.user_id)
    .bind(subject)
    .execute(pool)
    .await
    .expect("test user");
    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
         VALUES ($1, $2, $3, 'Credential test workspace', 'Europe/Madrid')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(slug)
    .execute(pool)
    .await
    .expect("test workspace");
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("test membership");
    scope
}
