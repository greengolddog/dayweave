use std::str::FromStr;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration as ChronoDuration, Utc};
use dayweave_api::{
    auth::Scope,
    credential_auth::{
        CredentialKind, CredentialRepository, CredentialRepositoryError, DeviceClientKind,
        DeviceEnrollmentSpec, McpClientSpec, OpaqueCredential,
    },
    persistence::{DatabaseScope, MIGRATOR, PostgresCredentialRepository},
};
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

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
    let (first_consume, second_consume) = tokio::join!(
        repository.consume_device_enrollment(
            &enrollment,
            Uuid::new_v4(),
            &first_access,
            &first_refresh,
            now
        ),
        repository.consume_device_enrollment(
            &enrollment,
            Uuid::new_v4(),
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
    assert_eq!(rotated.credential_issued_at, refresh_at);
    assert_eq!(
        rotated.access_expires_at,
        refresh_at + ChronoDuration::minutes(15)
    );
    assert_eq!(
        rotated.refresh_idle_expires_at,
        refresh_at + ChronoDuration::days(30)
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
    assert_eq!(
        repository
            .refresh_device_session(
                active_refresh,
                &rotated_access_one,
                &rotated_refresh_one,
                refresh_at
            )
            .await,
        Err(CredentialRepositoryError::InvalidCredential),
        "the old refresh token cannot be replayed"
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
                created_at: now,
            },
            &enrollment,
        )
        .await
        .expect("enrollment created");
    let session = repository
        .consume_device_enrollment(&enrollment, Uuid::new_v4(), &access, &refresh, now)
        .await
        .expect("session issued");

    sqlx::query("ALTER TABLE sessions DROP CONSTRAINT sessions_v1_shape_check")
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
                created_at: now,
                requested_expires_at: None,
            },
            &credential,
        )
        .await
        .expect("MCP client registered");
    assert_eq!(client.expires_at, now + ChronoDuration::days(90));
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
        authenticated.last_seen_at,
        Some(now + ChronoDuration::minutes(1))
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
