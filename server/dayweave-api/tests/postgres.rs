use std::{collections::BTreeSet, str::FromStr, time::Duration};

use chrono::{Duration as ChronoDuration, Utc};
use dayweave_api::{
    google_oauth::{
        AuthorizationCompletion, AuthorizationResolution, CallbackClaim, DisconnectMutation,
        EncryptedCredentials, GoogleAccountStatus, GoogleOAuthRepository,
        GoogleOAuthRepositoryError, NewOAuthSession, OAuthIdempotency, SealedSecret,
    },
    persistence::{
        DatabaseScope, IdempotencyDecision, IdempotencyError, MIGRATOR, NewOutboxMessage,
        PostgresGoogleOAuthRepository, PostgresIdempotencyRepository, PostgresOutboxRepository,
        PostgresProposalRepository,
    },
    proposals::{
        NewProposal, Proposal, ProposalKind, ProposalRepository, ProposalSource, RepositoryError,
    },
    readiness::Readiness,
};
use serde_json::json;
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

#[test]
fn embedded_migrations_cover_the_durable_domain_without_compile_time_database_access() {
    let versions: Vec<_> = MIGRATOR.iter().map(|migration| migration.version).collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6]);

    let schema = [
        include_str!("../migrations/0001_identity_and_items.sql"),
        include_str!("../migrations/0002_schedule_sync_and_audit.sql"),
        include_str!("../migrations/0003_proposals_mcp_idempotency_outbox.sql"),
        include_str!("../migrations/0004_item_delta_sync.sql"),
        include_str!("../migrations/0005_execution_sessions.sql"),
        include_str!("../migrations/0006_google_oauth.sql"),
    ]
    .join("\n");
    for table in [
        "users",
        "workspaces",
        "provider_accounts",
        "items",
        "item_hierarchy",
        "item_dependencies",
        "schedule_revisions",
        "schedule_blocks",
        "provider_sync_mappings",
        "provider_sync_cursors",
        "sessions",
        "audit_operations",
        "outbox_messages",
        "proposals",
        "mcp_clients",
        "idempotency_keys",
        "item_changes",
        "execution_sessions",
        "execution_state",
        "google_oauth_sessions",
        "google_oauth_cleanup_tokens",
        "google_oauth_scope_state",
        "google_oauth_guardian_resolutions",
        "google_oauth_legacy_credential_quarantine",
    ] {
        assert!(schema.contains(&format!("CREATE TABLE {table}")), "{table}");
    }
    assert!(schema.contains("timestamptz"));
    assert!(!schema.contains("timestamp without time zone"));
    assert!(schema.contains("trashed_at"));
    assert!(schema.contains("tombstoned_at"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn postgres_migrations_repository_idempotency_and_outbox_are_transactional() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL integration test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    MIGRATOR.run(pool).await.expect("migrations are repeatable");

    let scope = seed_scope(pool).await;
    let repository = PostgresProposalRepository::new(pool.clone(), scope);
    let now = Utc::now();
    let original = Proposal::new(
        NewProposal {
            submitted_by: "integration-test".to_owned(),
            source: ProposalSource::ExternalMcp,
            source_reference: Some("conversation-safe-label".to_owned()),
            kind: ProposalKind::SchedulePlan,
            title: "Review a proposed schedule".to_owned(),
            explanation: Some("Stored only in the Suggestions Inbox".to_owned()),
            payload: json!({"proposal_only": true}),
            expires_at: now + ChronoDuration::days(7),
        },
        now,
    )
    .unwrap();
    repository.insert(original.clone()).await.unwrap();
    assert_eq!(
        repository.get(original.id).await.unwrap().title,
        original.title
    );

    let other_scope = seed_other_scope(pool).await;
    let other_repository = PostgresProposalRepository::new(pool.clone(), other_scope);
    assert_eq!(
        other_repository.get(original.id).await.unwrap_err(),
        RepositoryError::NotFound(original.id)
    );

    let mut changed = original.clone();
    changed.revision = 2;
    changed.title = "Updated review title".to_owned();
    changed.updated_at = now + ChronoDuration::minutes(1);
    assert_eq!(
        repository.replace(changed.clone(), 9).await.unwrap_err(),
        RepositoryError::RevisionConflict {
            expected: 9,
            actual: 1,
        }
    );
    repository.replace(changed.clone(), 1).await.unwrap();
    repository.delete(changed.id, 2).await.unwrap();
    assert_eq!(
        repository.get(changed.id).await.unwrap_err(),
        RepositoryError::NotFound(changed.id)
    );
    let retained: bool = sqlx::query_scalar(
        "SELECT trashed_at IS NOT NULL FROM proposals WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(changed.id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(retained);
    let mutation_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
         AND aggregate_type = 'proposal' AND aggregate_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(changed.id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(mutation_events, 3);
    let audit_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_operations WHERE workspace_id = $1 \
         AND entity_type = 'proposal' AND entity_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(changed.id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(audit_events, 3);

    let idempotency = PostgresIdempotencyRepository::new(pool.clone(), scope);
    let fingerprint = [7_u8; 32];
    let expires_at = Utc::now() + ChronoDuration::hours(1);
    let different_response = json!({"different": true});
    assert_eq!(
        idempotency
            .reserve(
                "mcp.submit_proposal",
                "stable-client-key",
                &fingerprint,
                expires_at
            )
            .await
            .unwrap(),
        IdempotencyDecision::Acquired
    );
    assert_eq!(
        idempotency
            .reserve(
                "mcp.submit_proposal",
                "stable-client-key",
                &fingerprint,
                expires_at
            )
            .await
            .unwrap(),
        IdempotencyDecision::InProgress
    );
    assert_eq!(
        idempotency
            .reserve(
                "mcp.submit_proposal",
                "stable-client-key",
                &[8_u8; 32],
                expires_at,
            )
            .await
            .unwrap(),
        IdempotencyDecision::Conflict
    );
    let replay = json!({"proposal_id": changed.id, "review_required": true});
    idempotency
        .complete(
            "mcp.submit_proposal",
            "stable-client-key",
            &fingerprint,
            Some("proposal"),
            Some(changed.id),
            Some(&replay),
        )
        .await
        .unwrap();
    idempotency
        .complete(
            "mcp.submit_proposal",
            "stable-client-key",
            &fingerprint,
            Some("proposal"),
            Some(changed.id),
            Some(&replay),
        )
        .await
        .unwrap();
    assert_eq!(
        idempotency
            .complete(
                "mcp.submit_proposal",
                "stable-client-key",
                &fingerprint,
                Some("proposal"),
                Some(changed.id),
                Some(&different_response),
            )
            .await
            .unwrap_err(),
        IdempotencyError::Conflict
    );
    assert_eq!(
        idempotency
            .reserve(
                "mcp.submit_proposal",
                "stable-client-key",
                &fingerprint,
                expires_at
            )
            .await
            .unwrap(),
        IdempotencyDecision::Replay {
            resource_id: Some(changed.id),
            response: Some(replay),
        }
    );
    let raw_key_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM idempotency_keys WHERE key_hash = $1")
            .bind(b"stable-client-key".as_slice())
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(raw_key_count, 0);

    let outbox = PostgresOutboxRepository::new(pool.clone(), scope);
    let event_id = outbox
        .enqueue(NewOutboxMessage {
            aggregate_type: "sync_cursor".to_owned(),
            aggregate_id: Uuid::new_v4(),
            aggregate_revision: Some(1),
            event_type: "sync.cursor_advanced".to_owned(),
            deduplication_key: Some("sync-cursor-fixture-1".to_owned()),
            payload: json!({"cursor_advanced": true}),
            headers: json!({}),
            available_at: Utc::now(),
        })
        .await
        .unwrap();
    let claimed = outbox
        .claim_batch("integration-worker", 10, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(claimed.iter().any(|message| message.id == event_id));
    outbox
        .mark_published(event_id, "integration-worker")
        .await
        .unwrap();
    let published: bool =
        sqlx::query_scalar("SELECT published_at IS NOT NULL FROM outbox_messages WHERE id = $1")
            .bind(event_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(published);

    test_database.destroy().await;
}

fn google_idempotency(
    namespace: &'static str,
    key_marker: u8,
    fingerprint_marker: u8,
    now: chrono::DateTime<Utc>,
) -> OAuthIdempotency {
    OAuthIdempotency {
        namespace,
        key_hash: [key_marker; 32],
        request_fingerprint: [fingerprint_marker; 32],
        expires_at: now + ChronoDuration::days(1),
    }
}

fn google_session(
    id: Uuid,
    state_marker: u8,
    ciphertext_marker: u8,
    now: chrono::DateTime<Utc>,
) -> NewOAuthSession {
    NewOAuthSession {
        id,
        owner_subject_hash: [4_u8; 32],
        state_hash: [state_marker; 32],
        encrypted_verifier: SealedSecret {
            key_version: 1,
            ciphertext: vec![ciphertext_marker; 48],
        },
        encrypted_authorization_url: SealedSecret {
            key_version: 1,
            ciphertext: vec![ciphertext_marker.wrapping_add(1); 96],
        },
        requested_scopes: [
            "https://www.googleapis.com/auth/calendar".to_owned(),
            "https://www.googleapis.com/auth/tasks".to_owned(),
        ]
        .into_iter()
        .collect(),
        expected_account_id: None,
        expected_account_revision: None,
        make_default: false,
        created_at: now,
        expires_at: now + ChronoDuration::minutes(10),
    }
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)]
async fn postgres_google_oauth_is_fenced_recoverable_scoped_and_idempotent() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool).await;
    let other_scope = seed_other_scope(pool).await;
    let repository = PostgresGoogleOAuthRepository::new(pool.clone(), scope);
    let other_repository = PostgresGoogleOAuthRepository::new(pool.clone(), other_scope);
    let now = Utc::now();
    let exchange_stale_before = now - ChronoDuration::minutes(2);
    let first_session = google_session(Uuid::new_v4(), 3, 9, now);
    let first_start_key = google_idempotency("google_oauth_start", 1, 11, now);
    let first_started = repository
        .create_session(
            first_session.clone(),
            first_start_key.clone(),
            exchange_stale_before,
        )
        .await
        .expect("first session created");
    assert!(!first_started.replayed);
    let replayed_start = repository
        .create_session(
            first_session.clone(),
            first_start_key.clone(),
            exchange_stale_before,
        )
        .await
        .expect("start replayed");
    assert!(replayed_start.replayed);
    assert_eq!(replayed_start.id, first_started.id);
    assert_eq!(
        replayed_start.encrypted_authorization_url.ciphertext,
        first_started.encrypted_authorization_url.ciphertext
    );
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 4, 10, now),
                google_idempotency("google_oauth_start", 1, 12, now),
                exchange_stale_before,
            )
            .await,
        Err(GoogleOAuthRepositoryError::IdempotencyConflict)
    ));

    // A newer pending flow atomically supersedes the older one. Once exchange
    // starts, a third flow is rejected so only one refresh credential can be issued.
    let session_id = Uuid::new_v4();
    let state_hash = [5_u8; 32];
    repository
        .create_session(
            google_session(session_id, 5, 11, now),
            google_idempotency("google_oauth_start", 2, 13, now),
            exchange_stale_before,
        )
        .await
        .expect("newest pending session created");
    assert!(matches!(
        repository
            .claim_callback(first_session.state_hash, now, exchange_stale_before)
            .await,
        Err(GoogleOAuthRepositoryError::InvalidCallbackState)
    ));

    let (first, second) = tokio::join!(
        repository.claim_callback(state_hash, now, exchange_stale_before),
        repository.claim_callback(state_hash, now, exchange_stale_before)
    );
    assert!(first.is_ok() ^ second.is_ok());
    let CallbackClaim::Exchange(claimed) = first.or(second).expect("one callback claim") else {
        panic!("pending callback must claim the exchange lease");
    };
    assert!(matches!(
        repository
            .claim_callback(state_hash, now, exchange_stale_before)
            .await,
        Err(GoogleOAuthRepositoryError::InvalidCallbackState)
    ));
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 6, 12, now),
                google_idempotency("google_oauth_start", 3, 14, now),
                exchange_stale_before,
            )
            .await,
        Err(GoogleOAuthRepositoryError::AuthorizationInProgress)
    ));

    repository
        .hold_cleanup_token(
            session_id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![77; 64],
            },
            now,
        )
        .await
        .expect("new refresh token durably held before staging");

    let account_id = Uuid::new_v4();
    repository
        .stage_authorization(AuthorizationCompletion {
            session_id,
            owner_subject_hash: claimed.owner_subject_hash,
            expected_account_revision: None,
            account_id,
            make_default: false,
            external_account_id: "google-user-postgres".to_owned(),
            display_label: "owner@example.test".to_owned(),
            credentials: EncryptedCredentials {
                sealed: SealedSecret {
                    key_version: 1,
                    ciphertext: vec![8; 64],
                },
            },
            granted_scopes: claimed.requested_scopes,
            token_expires_at: now + ChronoDuration::hours(1),
            now,
        })
        .await
        .expect("authorization durably staged");
    assert!(matches!(
        repository
            .claim_callback(state_hash, now, exchange_stale_before)
            .await,
        Ok(CallbackClaim::Staged {
            session_id: staged_id
        }) if staged_id == session_id
    ));
    assert!(matches!(
        repository
            .resolve_authorization(session_id)
            .await
            .expect("exact staged resolution"),
        AuthorizationResolution::Staged
    ));
    let (first_consumer, concurrent_consumer) = tokio::join!(
        repository.complete_staged_authorization(session_id),
        repository.complete_staged_authorization(session_id)
    );
    let account = first_consumer.expect("first staged consumer");
    assert_eq!(
        concurrent_consumer.expect("concurrent consumer gets installed account"),
        account
    );
    assert!(matches!(
        repository
            .resolve_authorization(session_id)
            .await
            .expect("exact consumed resolution"),
        AuthorizationResolution::Consumed(ref installed) if *installed == account
    ));
    assert_eq!(
        repository
            .cleanup_status()
            .await
            .expect("held cleanup removed with installation")
            .held,
        0
    );
    assert_eq!(account.status, GoogleAccountStatus::Active);
    assert_eq!(account.external_account_id, "google-user-postgres");
    let staged_secrets_scrubbed: bool = sqlx::query_scalar(
        "SELECT encrypted_pkce_verifier IS NULL AND verifier_key_version IS NULL \
         AND staged_encrypted_credentials IS NULL AND staged_credential_key_version IS NULL \
         FROM google_oauth_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await
    .expect("PKCE scrub check");
    assert!(staged_secrets_scrubbed);
    assert!(matches!(
        repository
            .claim_callback(state_hash, now, exchange_stale_before)
            .await,
        Err(GoogleOAuthRepositoryError::InvalidCallbackState)
    ));
    assert!(
        other_repository
            .account()
            .await
            .expect("other scope")
            .is_none()
    );

    let reconnect_id = Uuid::new_v4();
    let reconnect_state = [16_u8; 32];
    let mut reconnect_session = google_session(reconnect_id, 16, 17, now);
    reconnect_session.expected_account_id = Some(account.id);
    reconnect_session.expected_account_revision = Some(account.revision);
    repository
        .create_session(
            reconnect_session,
            google_idempotency("google_oauth_start", 16, 16, now),
            exchange_stale_before,
        )
        .await
        .expect("reauthorization session created");
    let CallbackClaim::Exchange(reconnect_claim) = repository
        .claim_callback(reconnect_state, now, exchange_stale_before)
        .await
        .expect("reauthorization claimed")
    else {
        panic!("reauthorization must claim exchange");
    };
    repository
        .stage_authorization(AuthorizationCompletion {
            session_id: reconnect_id,
            owner_subject_hash: reconnect_claim.owner_subject_hash,
            expected_account_revision: Some(account.revision),
            account_id: account.id,
            make_default: false,
            external_account_id: account.external_account_id.clone(),
            display_label: "updated@example.test".to_owned(),
            credentials: EncryptedCredentials {
                sealed: SealedSecret {
                    key_version: 1,
                    ciphertext: vec![18; 64],
                },
            },
            granted_scopes: reconnect_claim.requested_scopes.clone(),
            token_expires_at: now + ChronoDuration::hours(2),
            now,
        })
        .await
        .expect("reauthorization staged");
    let account = repository
        .complete_staged_authorization(reconnect_id)
        .await
        .expect("reauthorization installed");
    assert_eq!(account.revision, 2);
    assert_eq!(account.display_label, "updated@example.test");

    let second_account_id = Uuid::new_v4();
    let second_session_id = Uuid::new_v4();
    let second_state = [60_u8; 32];
    repository
        .create_session(
            google_session(second_session_id, 60, 61, now),
            google_idempotency("google_oauth_start", 60, 60, now),
            exchange_stale_before,
        )
        .await
        .expect("second identity session created");
    let CallbackClaim::Exchange(second_claim) = repository
        .claim_callback(second_state, now, exchange_stale_before)
        .await
        .expect("second identity exchange claimed")
    else {
        panic!("second identity must claim exchange");
    };
    repository
        .stage_authorization(AuthorizationCompletion {
            session_id: second_session_id,
            owner_subject_hash: second_claim.owner_subject_hash,
            expected_account_revision: None,
            account_id: second_account_id,
            make_default: false,
            external_account_id: "google-user-postgres-two".to_owned(),
            display_label: "second@example.test".to_owned(),
            credentials: EncryptedCredentials {
                sealed: SealedSecret {
                    key_version: 1,
                    ciphertext: vec![61; 64],
                },
            },
            granted_scopes: second_claim.requested_scopes,
            token_expires_at: now + ChronoDuration::hours(1),
            now,
        })
        .await
        .expect("second identity staged");
    let second_account = repository
        .complete_staged_authorization(second_session_id)
        .await
        .expect("second identity installed");
    assert_eq!(second_account.id, second_account_id);
    assert!(!second_account.is_default);
    let all_accounts = repository.accounts().await.expect("multi-account list");
    assert_eq!(all_accounts.len(), 2);
    assert_eq!(
        all_accounts
            .iter()
            .filter(|snapshot| snapshot.account.is_default)
            .count(),
        1
    );
    assert_eq!(all_accounts[0].account.id, account.id);

    let pause_key = google_idempotency("google_oauth_pause", 21, 31, now);
    let paused = repository
        .set_paused(
            account.id,
            account.revision,
            true,
            now,
            exchange_stale_before,
            pause_key.clone(),
        )
        .await
        .expect("paused");
    assert!(!paused.replayed);
    let paused_replay = repository
        .set_paused(
            account.id,
            account.revision,
            true,
            now,
            exchange_stale_before,
            pause_key.clone(),
        )
        .await
        .expect("pause replayed");
    assert!(paused_replay.replayed);
    assert_eq!(paused_replay.account, paused.account);
    assert!(matches!(
        repository
            .set_paused(
                account.id,
                account.revision,
                true,
                now,
                exchange_stale_before,
                google_idempotency("google_oauth_pause", 21, 99, now),
            )
            .await,
        Err(GoogleOAuthRepositoryError::IdempotencyConflict)
    ));
    let resume_key = google_idempotency("google_oauth_resume", 22, 32, now);
    let resumed = repository
        .set_paused(
            paused.account.id,
            paused.account.revision,
            false,
            now,
            exchange_stale_before,
            resume_key.clone(),
        )
        .await
        .expect("resumed");
    let resumed_replay = repository
        .set_paused(
            paused.account.id,
            paused.account.revision,
            false,
            now,
            exchange_stale_before,
            resume_key,
        )
        .await
        .expect("resume replayed");
    assert!(resumed_replay.replayed);
    assert_eq!(resumed_replay.account, resumed.account);
    let pause_replay_after_newer_mutation = repository
        .set_paused(
            account.id,
            account.revision,
            true,
            now,
            exchange_stale_before,
            pause_key,
        )
        .await
        .expect("historical response replayed exactly");
    assert_eq!(pause_replay_after_newer_mutation.account, paused.account);

    let first_claim_id = Uuid::new_v4();
    let disconnect_key = google_idempotency("google_oauth_disconnect", 23, 33, now);
    let disconnect = repository
        .claim_disconnect(
            resumed.account.id,
            resumed.account.revision,
            first_claim_id,
            now,
            now - ChronoDuration::minutes(2),
            exchange_stale_before,
            disconnect_key.clone(),
        )
        .await
        .expect("disconnect claimed");
    let DisconnectMutation::Execute(disconnect) = disconnect else {
        panic!("first disconnect must execute");
    };
    assert_eq!(disconnect.credentials.sealed.ciphertext, vec![18; 64]);
    assert_eq!(disconnect.protected_accounts.len(), 2);
    assert!(
        repository
            .cleanup_status()
            .await
            .expect("disconnect fence status")
            .revocation_fenced
    );
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 71, 72, now),
                google_idempotency("google_oauth_start", 71, 71, now),
                exchange_stale_before,
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    assert!(matches!(
        repository
            .claim_volatile_revocation(
                Uuid::new_v4(),
                Uuid::new_v4(),
                now,
                now - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    assert!(matches!(
        repository
            .complete_disconnect(
                resumed.account.id,
                first_claim_id,
                disconnect.credential_generation + 1,
                now,
                disconnect_key.clone(),
            )
            .await,
        Err(GoogleOAuthRepositoryError::CleanupClaimLost)
    ));
    assert_eq!(
        repository
            .fail_disconnect(
                resumed.account.id,
                first_claim_id,
                disconnect.credential_generation + 1,
                now,
            )
            .await,
        Err(GoogleOAuthRepositoryError::CleanupClaimLost)
    );
    repository
        .fail_disconnect(
            resumed.account.id,
            first_claim_id,
            disconnect.credential_generation,
            now,
        )
        .await
        .expect("revocation failure retained");
    let retained = repository
        .account()
        .await
        .expect("account lookup")
        .expect("credentials retained");
    assert_eq!(
        retained.account.status,
        GoogleAccountStatus::RevocationFailed
    );
    assert_eq!(retained.credentials.sealed.ciphertext, vec![18; 64]);

    let retry_claim_id = Uuid::new_v4();
    let retried = repository
        .claim_disconnect(
            resumed.account.id,
            resumed.account.revision,
            retry_claim_id,
            now,
            now - ChronoDuration::minutes(2),
            exchange_stale_before,
            disconnect_key.clone(),
        )
        .await
        .expect("disconnect retried");
    let DisconnectMutation::Execute(retried) = retried else {
        panic!("disconnect retry must execute");
    };
    assert_eq!(retried.protected_accounts.len(), 2);
    let revoked = repository
        .complete_disconnect(
            retained.account.id,
            retry_claim_id,
            retried.credential_generation,
            now,
            disconnect_key.clone(),
        )
        .await
        .expect("disconnect completed");
    assert_eq!(revoked.account.status, GoogleAccountStatus::Revoked);
    assert!(!revoked.replayed);
    let disconnect_replay = repository
        .claim_disconnect(
            retained.account.id,
            resumed.account.revision,
            Uuid::new_v4(),
            now,
            now - ChronoDuration::minutes(2),
            exchange_stale_before,
            disconnect_key,
        )
        .await
        .expect("completed disconnect replayed");
    assert!(matches!(
        disconnect_replay,
        DisconnectMutation::Replay(ref account) if *account == revoked.account
    ));
    let promoted = repository
        .account()
        .await
        .expect("account lookup")
        .expect("remaining identity promoted deterministically");
    assert_eq!(promoted.account.id, second_account.id);
    assert!(promoted.account.is_default);
    assert_eq!(promoted.account.revision, second_account.revision + 1);
    let credentials_scrubbed: bool = sqlx::query_scalar(
        "SELECT encrypted_credentials IS NULL AND credential_key_version IS NULL \
         FROM provider_accounts WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(account_id)
    .fetch_one(pool)
    .await
    .expect("credential scrub check");
    assert!(credentials_scrubbed);

    let expired_id = Uuid::new_v4();
    let mut expired_session = google_session(expired_id, 40, 41, now);
    expired_session.expires_at = now + ChronoDuration::minutes(1);
    repository
        .create_session(
            expired_session.clone(),
            google_idempotency("google_oauth_start", 40, 40, now),
            exchange_stale_before,
        )
        .await
        .expect("expiring session created");
    assert!(matches!(
        repository
            .claim_callback(
                expired_session.state_hash,
                now + ChronoDuration::minutes(2),
                exchange_stale_before,
            )
            .await,
        Err(GoogleOAuthRepositoryError::InvalidCallbackState)
    ));
    let expired_secrets_scrubbed: bool = sqlx::query_scalar(
        "SELECT encrypted_pkce_verifier IS NULL AND verifier_key_version IS NULL \
         AND encrypted_authorization_url IS NULL AND authorization_url_key_version IS NULL \
         FROM google_oauth_sessions WHERE id = $1",
    )
    .bind(expired_id)
    .fetch_one(pool)
    .await
    .expect("expired session scrub check");
    assert!(expired_secrets_scrubbed);

    // A process crash after claiming exchange cannot wedge the scope forever.
    let stale_session = google_session(Uuid::new_v4(), 50, 51, now);
    repository
        .create_session(
            stale_session.clone(),
            google_idempotency("google_oauth_start", 50, 50, now),
            exchange_stale_before,
        )
        .await
        .expect("stale exchange fixture created");
    repository
        .claim_callback(stale_session.state_hash, now, exchange_stale_before)
        .await
        .expect("stale exchange fixture claimed");
    repository
        .hold_cleanup_token(
            stale_session.id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![88; 64],
            },
            now,
        )
        .await
        .expect("post-exchange refresh survives process crash");
    repository
        .identify_cleanup_token(stale_session.id, "google-orphan-postgres", now)
        .await
        .expect("verified orphan identity is retained with cleanup custody");
    let recovery_time = now + ChronoDuration::minutes(3);
    repository
        .recover_startup(recovery_time, recovery_time - ChronoDuration::minutes(2))
        .await
        .expect("startup explicitly recovers stale exchange custody");
    repository
        .create_session(
            google_session(Uuid::new_v4(), 51, 52, recovery_time),
            google_idempotency("google_oauth_start", 51, 51, recovery_time),
            recovery_time - ChronoDuration::minutes(2),
        )
        .await
        .expect("stale exchange lease recovered");
    let stale_status: String =
        sqlx::query_scalar("SELECT status FROM google_oauth_sessions WHERE id = $1")
            .bind(stale_session.id)
            .fetch_one(pool)
            .await
            .expect("stale session status");
    assert_eq!(stale_status, "failed");
    let cleanup_status = repository
        .cleanup_status()
        .await
        .expect("stale held cleanup becomes retryable");
    assert_eq!(cleanup_status.held, 0);
    assert_eq!(cleanup_status.pending, 1);
    let first_cleanup_claim_id = Uuid::new_v4();
    let first_cleanup = repository
        .claim_cleanup(
            first_cleanup_claim_id,
            recovery_time,
            recovery_time - ChronoDuration::minutes(2),
            recovery_time - ChronoDuration::minutes(2),
            None,
        )
        .await
        .expect("cleanup claim")
        .expect("cleanup credential retained");
    assert_eq!(first_cleanup.session_id, stale_session.id);
    assert_eq!(first_cleanup.attempt, 1);
    assert_eq!(
        first_cleanup.encrypted_refresh_token.ciphertext,
        vec![88; 64]
    );
    repository
        .fail_cleanup(
            stale_session.id,
            first_cleanup_claim_id,
            first_cleanup.credential_generation,
            recovery_time,
            recovery_time + ChronoDuration::seconds(1),
        )
        .await
        .expect("failed revocation remains pending");
    let failed_cleanup_status = repository
        .cleanup_status()
        .await
        .expect("failed cleanup status");
    assert_eq!(failed_cleanup_status.pending, 1);
    assert_eq!(failed_cleanup_status.last_failure_at, Some(recovery_time));
    let second_cleanup_claim_id = Uuid::new_v4();
    let second_cleanup = repository
        .claim_cleanup(
            second_cleanup_claim_id,
            recovery_time + ChronoDuration::seconds(1),
            recovery_time - ChronoDuration::minutes(2),
            recovery_time - ChronoDuration::minutes(2),
            None,
        )
        .await
        .expect("cleanup retry claim")
        .expect("failed cleanup remains retryable");
    assert_eq!(second_cleanup.attempt, 2);
    repository
        .complete_cleanup(
            stale_session.id,
            second_cleanup_claim_id,
            second_cleanup.credential_generation,
            recovery_time + ChronoDuration::seconds(1),
        )
        .await
        .expect("successful or invalid-token revocation clears cleanup");
    let cleanup_status = repository.cleanup_status().await.expect("cleanup cleared");
    assert_eq!(
        cleanup_status.held + cleanup_status.pending + cleanup_status.retrying,
        0
    );

    test_database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)]
async fn postgres_cleanup_fence_serializes_claim_backoff_and_all_install_phases() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool).await;
    let repository = PostgresGoogleOAuthRepository::new(pool.clone(), scope);
    let now = Utc::now();

    for (id, external, is_default, marker) in [
        (Uuid::new_v4(), "google-one", true, 31_u8),
        (Uuid::new_v4(), "google-two", false, 32_u8),
    ] {
        sqlx::query(
            "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
             external_account_id, display_label, encrypted_credentials, credential_key_version, \
             status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', $4, $4, $5, 1, \
             'active', true, $6)",
        )
        .bind(id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(external)
        .bind(vec![marker; 64])
        .bind(is_default)
        .execute(pool)
        .await
        .expect("seed protected account");
    }

    let race_account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', 'google-race', \
         'google-race', $4, 1, 'active', true, false)",
    )
    .bind(race_account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![33_u8; 64])
    .execute(pool)
    .await
    .expect("seed race account");
    let race_session = google_session(Uuid::new_v4(), 88, 89, now);
    let race_claim_id = Uuid::new_v4();
    let race_disconnect_key = google_idempotency("google_oauth_disconnect", 88, 88, now);
    let disconnect_repository = repository.clone();
    let connect_repository = repository.clone();
    let (disconnect_race, connect_race) = tokio::join!(
        disconnect_repository.claim_disconnect(
            race_account_id,
            1,
            race_claim_id,
            now,
            now - ChronoDuration::minutes(2),
            now - ChronoDuration::minutes(2),
            race_disconnect_key.clone(),
        ),
        connect_repository.create_session(
            race_session.clone(),
            google_idempotency("google_oauth_start", 88, 89, now),
            now - ChronoDuration::minutes(2),
        ),
    );
    let disconnect_claim = match (disconnect_race, connect_race) {
        (
            Ok(DisconnectMutation::Execute(claim)),
            Err(GoogleOAuthRepositoryError::RevocationInProgress),
        ) => claim,
        (Err(GoogleOAuthRepositoryError::AuthorizationInProgress), Ok(_)) => {
            repository
                .claim_callback(
                    race_session.state_hash,
                    now,
                    now - ChronoDuration::minutes(2),
                )
                .await
                .expect("winning connect session is claimable");
            repository
                .fail_authorization(race_session.id, now)
                .await
                .expect("clear winning connect session");
            let DisconnectMutation::Execute(claim) = repository
                .claim_disconnect(
                    race_account_id,
                    1,
                    race_claim_id,
                    now,
                    now - ChronoDuration::minutes(2),
                    now - ChronoDuration::minutes(2),
                    race_disconnect_key.clone(),
                )
                .await
                .expect("disconnect proceeds after connect is closed")
            else {
                panic!("disconnect executes");
            };
            claim
        }
        outcome => panic!("connect/disconnect must serialize exclusively: {outcome:?}"),
    };
    assert_eq!(disconnect_claim.protected_accounts.len(), 3);
    repository
        .complete_disconnect(
            race_account_id,
            disconnect_claim.claim_id,
            disconnect_claim.credential_generation,
            now,
            race_disconnect_key,
        )
        .await
        .expect("race account disconnect completes under its exact fence");

    let session = google_session(Uuid::new_v4(), 91, 92, now);
    repository
        .create_session(
            session.clone(),
            google_idempotency("google_oauth_start", 91, 91, now),
            now - ChronoDuration::minutes(2),
        )
        .await
        .expect("create cleanup source session");
    let CallbackClaim::Exchange(claimed) = repository
        .claim_callback(session.state_hash, now, now - ChronoDuration::minutes(2))
        .await
        .expect("claim callback")
    else {
        panic!("new session exchanges");
    };
    repository
        .hold_cleanup_token(
            claimed.id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![93; 64],
            },
            now,
        )
        .await
        .expect("hold cleanup");
    repository
        .abandon_authorization(claimed.id, now)
        .await
        .expect("promote cleanup");
    let first = repository
        .claim_cleanup(
            Uuid::new_v4(),
            now,
            now - ChronoDuration::minutes(2),
            now - ChronoDuration::minutes(2),
            Some(claimed.id),
        )
        .await
        .expect("claim cleanup")
        .expect("pending cleanup");
    assert_eq!(first.protected_accounts.len(), 2);
    assert!(matches!(
        repository
            .claim_volatile_revocation(
                claimed.id,
                Uuid::new_v4(),
                now,
                now - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));

    let create_repository = repository.clone();
    let complete_repository = repository.clone();
    let stage_repository = repository.clone();
    let blocked_session = google_session(Uuid::new_v4(), 94, 95, now);
    let dummy_completion = AuthorizationCompletion {
        session_id: claimed.id,
        owner_subject_hash: [4; 32],
        expected_account_revision: None,
        account_id: Uuid::new_v4(),
        make_default: false,
        external_account_id: "unused".to_owned(),
        display_label: "unused".to_owned(),
        credentials: EncryptedCredentials {
            sealed: SealedSecret {
                key_version: 1,
                ciphertext: vec![96; 64],
            },
        },
        granted_scopes: BTreeSet::default(),
        token_expires_at: now + ChronoDuration::hours(1),
        now,
    };
    let (create_result, complete_result, stage_result) = tokio::join!(
        create_repository.create_session(
            blocked_session,
            google_idempotency("google_oauth_start", 94, 94, now),
            now - ChronoDuration::minutes(2),
        ),
        complete_repository.complete_staged_authorization(claimed.id),
        stage_repository.stage_authorization(dummy_completion),
    );
    assert!(matches!(
        create_result,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    assert_eq!(
        complete_result,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    );
    assert_eq!(
        stage_result,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    );

    let retry_at = now + ChronoDuration::seconds(5);
    repository
        .fail_cleanup(
            first.session_id,
            first.claim_id,
            first.credential_generation,
            now,
            retry_at,
        )
        .await
        .expect("ambiguous revoke retains fence");
    assert!(
        repository
            .claim_cleanup(
                Uuid::new_v4(),
                now + ChronoDuration::seconds(4),
                now - ChronoDuration::minutes(2),
                now - ChronoDuration::minutes(2),
                None,
            )
            .await
            .expect("backoff query")
            .is_none()
    );
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 97, 98, now),
                google_idempotency("google_oauth_start", 97, 97, now),
                now - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    let second = repository
        .claim_cleanup(
            Uuid::new_v4(),
            retry_at,
            now - ChronoDuration::minutes(2),
            now - ChronoDuration::minutes(2),
            None,
        )
        .await
        .expect("due retry")
        .expect("same fenced cleanup is retryable");
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(second.attempt, 2);
    repository
        .complete_cleanup(
            second.session_id,
            second.claim_id,
            second.credential_generation,
            retry_at,
        )
        .await
        .expect("definitive provider result clears fence");
    let guardian_session = google_session(Uuid::new_v4(), 99, 100, retry_at);
    repository
        .create_session(
            guardian_session.clone(),
            google_idempotency("google_oauth_start", 99, 99, retry_at),
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("installs resume after fence is atomically cleared");
    repository
        .claim_callback(
            guardian_session.state_hash,
            retry_at,
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("guardian source exchanges");
    let first_guardian = repository
        .claim_volatile_revocation(
            guardian_session.id,
            Uuid::new_v4(),
            retry_at,
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("guardian acquires durable scope fence");
    assert_eq!(first_guardian.protected_accounts.len(), 2);
    assert!(matches!(
        repository
            .claim_volatile_revocation(
                guardian_session.id,
                Uuid::new_v4(),
                retry_at,
                retry_at - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    let guardian_recovery_at = retry_at + ChronoDuration::minutes(3);
    let replacement_guardian = repository
        .claim_volatile_revocation(
            guardian_session.id,
            Uuid::new_v4(),
            guardian_recovery_at,
            guardian_recovery_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("a crashed guardian lease is stolen only after its bounded timeout");
    assert_eq!(
        replacement_guardian.credential_generation,
        first_guardian.credential_generation
    );
    assert_eq!(
        repository
            .release_volatile_revocation(
                guardian_session.id,
                first_guardian.claim_id,
                first_guardian.credential_generation,
            )
            .await,
        Err(GoogleOAuthRepositoryError::CleanupClaimLost)
    );
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 101, 102, retry_at),
                google_idempotency("google_oauth_start", 101, 101, retry_at),
                retry_at - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    repository
        .hold_cleanup_token(
            guardian_session.id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![103; 64],
            },
            guardian_recovery_at,
        )
        .await
        .expect("owning guardian may transfer token to durable hold");
    repository
        .release_volatile_revocation(
            guardian_session.id,
            replacement_guardian.claim_id,
            replacement_guardian.credential_generation,
        )
        .await
        .expect("exact guardian claim releases fence without deleting hold");
    repository
        .release_volatile_revocation(
            guardian_session.id,
            replacement_guardian.claim_id,
            replacement_guardian.credential_generation,
        )
        .await
        .expect("lost release acknowledgement replays exactly");
    repository
        .abandon_authorization(guardian_session.id, guardian_recovery_at)
        .await
        .expect("durable hold becomes pending");
    let durable = repository
        .claim_cleanup(
            Uuid::new_v4(),
            guardian_recovery_at,
            guardian_recovery_at - ChronoDuration::minutes(2),
            guardian_recovery_at - ChronoDuration::minutes(2),
            Some(guardian_session.id),
        )
        .await
        .expect("durable cleanup claim")
        .expect("released guardian retained durable token");
    repository
        .complete_cleanup(
            durable.session_id,
            durable.claim_id,
            durable.credential_generation,
            guardian_recovery_at,
        )
        .await
        .expect("durable recovery cleanup completes");

    let definitive_session = google_session(Uuid::new_v4(), 104, 105, retry_at);
    repository
        .create_session(
            definitive_session.clone(),
            google_idempotency("google_oauth_start", 104, 104, retry_at),
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("definitive guardian session");
    repository
        .claim_callback(
            definitive_session.state_hash,
            retry_at,
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("definitive guardian exchange");
    repository
        .hold_cleanup_token(
            definitive_session.id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![106; 64],
            },
            retry_at,
        )
        .await
        .expect("simulate lost hold acknowledgement");
    let definitive = repository
        .claim_volatile_revocation(
            definitive_session.id,
            Uuid::new_v4(),
            retry_at,
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("definitive revocation fence");
    repository
        .complete_volatile_revocation(
            definitive_session.id,
            definitive.claim_id,
            definitive.credential_generation,
        )
        .await
        .expect("definitive provider result atomically clears ambiguous hold and fence");
    repository
        .complete_volatile_revocation(
            definitive_session.id,
            definitive.claim_id,
            definitive.credential_generation,
        )
        .await
        .expect("lost definitive acknowledgement replays exactly");
    let definitive_hold_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM google_oauth_cleanup_tokens WHERE workspace_id = $1 \
         AND user_id = $2 AND session_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(definitive_session.id)
    .fetch_one(pool)
    .await
    .expect("definitive hold count");
    assert_eq!(definitive_hold_count, 0);
    assert!(
        !repository
            .cleanup_status()
            .await
            .expect("guardian fence cleared")
            .revocation_fenced
    );

    let ambiguous_at = guardian_recovery_at + ChronoDuration::minutes(5);
    let ambiguous_session = google_session(Uuid::new_v4(), 107, 108, ambiguous_at);
    repository
        .create_session(
            ambiguous_session.clone(),
            google_idempotency("google_oauth_start", 107, 107, ambiguous_at),
            ambiguous_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("ambiguous guardian session");
    repository
        .claim_callback(
            ambiguous_session.state_hash,
            ambiguous_at,
            ambiguous_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("ambiguous exchange claimed");
    repository
        .claim_volatile_revocation(
            ambiguous_session.id,
            Uuid::new_v4(),
            ambiguous_at,
            ambiguous_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("guardian fence is durable before in-memory custody");
    let restart_at = ambiguous_at + ChronoDuration::minutes(3);
    repository
        .recover_startup(restart_at, restart_at - ChronoDuration::minutes(2))
        .await
        .expect("startup converts an identity-unknown guardian into recovery");
    let ambiguous_status = repository.cleanup_status().await.expect("recovery status");
    assert!(ambiguous_status.operator_recovery_required);
    assert!(ambiguous_status.revocation_fenced);
    assert_eq!(ambiguous_status.uncertain_authorizations, 1);
    let readiness = Readiness::with_database(pool.clone(), scope.workspace_id, scope.user_id);
    readiness.set_ready(true);
    assert!(!readiness.check().await);
    let recovery = repository
        .acknowledge_operator_recovery(restart_at)
        .await
        .expect("externally revoked grants resolve ambiguous guardian custody");
    assert_eq!(recovery.accounts_marked_reauthorization_required, 2);
    assert_eq!(recovery.legacy_accounts_finalized, 0);
    assert!(readiness.check().await);

    test_database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)]
async fn google_oauth_migration_quarantines_until_verified_operator_recovery() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in [
        include_str!("../migrations/0001_identity_and_items.sql"),
        include_str!("../migrations/0002_schedule_sync_and_audit.sql"),
        include_str!("../migrations/0003_proposals_mcp_idempotency_outbox.sql"),
        include_str!("../migrations/0004_item_delta_sync.sql"),
        include_str!("../migrations/0005_execution_sessions.sql"),
    ] {
        pool.execute(migration).await.expect("legacy migration");
    }
    let scope = seed_scope(pool).await;
    let calendar_legacy_id = Uuid::new_v4();
    let tasks_legacy_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled) VALUES ($1, $2, $3, 'google_calendar', $4, 'Legacy Google', \
         $5, 1, 'active', true)",
    )
    .bind(calendar_legacy_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind("google-user-upgrade")
    .bind(vec![1_u8; 64])
    .execute(pool)
    .await
    .expect("legacy Calendar account");
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled) VALUES ($1, $2, $3, 'google_tasks', $4, 'Legacy Google Tasks', \
         $5, 1, 'reauthorization_required', false)",
    )
    .bind(tasks_legacy_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind("google-user-upgrade")
    .bind(vec![2_u8; 64])
    .execute(pool)
    .await
    .expect("legacy Tasks account");

    pool.execute(include_str!("../migrations/0006_google_oauth.sql"))
        .await
        .expect("Google OAuth migration");
    let quarantined_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_accounts WHERE id = ANY($1) \
         AND provider IN ('google_calendar', 'google_tasks') \
         AND status = 'operator_recovery_required' AND NOT sync_enabled AND NOT is_default \
         AND encrypted_credentials IS NOT NULL AND credential_key_version IS NOT NULL \
         AND disconnected_at IS NULL",
    )
    .bind(vec![calendar_legacy_id, tasks_legacy_id])
    .fetch_one(pool)
    .await
    .expect("legacy rows remain non-terminal until verified recovery");
    assert_eq!(quarantined_count, 2);
    let preserved_legacy: Vec<(Uuid, String, Vec<u8>, i32)> = sqlx::query_as(
        "SELECT source_account_id, legacy_provider, encrypted_credentials, \
         credential_key_version FROM google_oauth_legacy_credential_quarantine \
         WHERE workspace_id = $1 AND user_id = $2 ORDER BY legacy_provider",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_all(pool)
    .await
    .expect("legacy opaque envelopes preserved in quarantine");
    assert_eq!(preserved_legacy.len(), 2);
    assert!(preserved_legacy.contains(&(
        calendar_legacy_id,
        "google_calendar".to_owned(),
        vec![1_u8; 64],
        1,
    )));
    assert!(preserved_legacy.contains(&(
        tasks_legacy_id,
        "google_tasks".to_owned(),
        vec![2_u8; 64],
        1,
    )));

    let repository = PostgresGoogleOAuthRepository::new(pool.clone(), scope);
    let readiness = Readiness::with_database(pool.clone(), scope.workspace_id, scope.user_id);
    readiness.set_ready(true);
    assert!(
        !readiness.check().await,
        "legacy credential custody must make readiness fail"
    );
    let cleanup = repository
        .cleanup_status()
        .await
        .expect("legacy recovery is visible");
    assert_eq!(cleanup.legacy_recovery_required, 2);
    assert!(cleanup.operator_recovery_required);
    assert!(cleanup.revocation_fenced);
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 110, 111, Utc::now()),
                google_idempotency("google_oauth_start", 110, 110, Utc::now()),
                Utc::now() - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));

    let recovery = repository
        .acknowledge_operator_recovery(Utc::now())
        .await
        .expect("operator-confirmed external grant revocation finalizes quarantine");
    assert_eq!(recovery.accounts_marked_reauthorization_required, 0);
    assert_eq!(recovery.legacy_accounts_finalized, 2);
    let finalized_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_accounts WHERE id = ANY($1) AND provider = 'google' \
         AND status = 'revoked' AND NOT sync_enabled AND encrypted_credentials IS NULL \
         AND credential_key_version IS NULL AND cardinality(granted_scopes) = 0 \
         AND token_expires_at IS NULL AND disconnected_at IS NOT NULL",
    )
    .bind(vec![calendar_legacy_id, tasks_legacy_id])
    .fetch_one(pool)
    .await
    .expect("confirmed legacy credentials are scrubbed");
    assert_eq!(finalized_count, 2);
    let confirmed_quarantine: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM google_oauth_legacy_credential_quarantine \
         WHERE workspace_id = $1 AND user_id = $2 AND recovery_confirmed_at IS NOT NULL \
         AND encrypted_credentials IS NULL AND credential_key_version IS NULL",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(pool)
    .await
    .expect("quarantine confirmation is durable");
    assert_eq!(confirmed_quarantine, 2);
    let cleanup = repository
        .cleanup_status()
        .await
        .expect("legacy recovery clears");
    assert_eq!(cleanup.legacy_recovery_required, 0);
    assert!(!cleanup.operator_recovery_required);
    assert!(!cleanup.revocation_fenced);
    assert!(
        readiness.check().await,
        "readiness recovers only after durable operator acknowledgement"
    );

    let reconnected_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', $4, \
         'Reconnected Google', $5, 1, 'active', true, true)",
    )
    .bind(reconnected_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind("google-user-upgrade")
    .bind(vec![3_u8; 64])
    .execute(pool)
    .await
    .expect("same external identity can reconnect after revocation");

    let second_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', $4, \
         'Second Google', $5, 1, 'active', true, false)",
    )
    .bind(second_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind("google-user-upgrade-two")
    .bind(vec![4_u8; 64])
    .execute(pool)
    .await
    .expect("two active Google identities coexist after upgrade");
    let active_accounts: (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE is_default) FROM provider_accounts \
         WHERE workspace_id = $1 AND user_id = $2 AND provider = 'google' \
         AND status <> 'revoked'",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(pool)
    .await
    .expect("multi-account migration state");
    assert_eq!(active_accounts, (2, 1));
    let duplicate_default = sqlx::query(
        "UPDATE provider_accounts SET is_default = true WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(second_id)
    .execute(pool)
    .await;
    assert!(duplicate_default.is_err());

    test_database.destroy().await;
}

struct TestDatabase {
    admin: PgPool,
    pool: PgPool,
    schema: String,
}

impl TestDatabase {
    async fn create(database_url: &str) -> Self {
        let options = PgConnectOptions::from_str(database_url)
            .expect("valid DAYWEAVE_TEST_DATABASE_URL")
            .disable_statement_logging();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("connect test PostgreSQL");
        let schema = format!("dayweave_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .expect("create isolated test schema");
        let connection_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _| {
                let statement = format!("SET search_path TO {connection_schema}");
                Box::pin(async move {
                    connection.execute(AssertSqlSafe(statement)).await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .expect("connect isolated test pool");
        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn destroy(self) {
        self.pool.close().await;
        self.admin
            .execute(AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .await
            .expect("drop isolated test schema");
        self.admin.close().await;
    }
}

async fn seed_scope(pool: &PgPool) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    insert_scope(pool, scope, "owner-one", "personal-one").await;
    scope
}

async fn seed_other_scope(pool: &PgPool) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    insert_scope(pool, scope, "owner-two", "personal-two").await;
    scope
}

async fn insert_scope(pool: &PgPool, scope: DatabaseScope, subject: &str, slug: &str) {
    sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
         VALUES ($1, $2, 'Test owner', 'Europe/Madrid')",
    )
    .bind(scope.user_id)
    .bind(subject)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
         VALUES ($1, $2, $3, 'Test workspace', 'Europe/Madrid')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(slug)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .unwrap();
}
