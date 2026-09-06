use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    account_deletion::{
        ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_ATTEMPTS,
        ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_TARGETS, AccountDeletionFenceConfirmation,
        AccountDeletionLifecycle, AccountDeletionMutation, AccountDeletionPreparation,
        AccountDeletionPrincipalBinding, AccountDeletionProvider,
        AccountDeletionProviderCleanupClaim, AccountDeletionProviderCleanupCompletion,
        AccountDeletionProviderCleanupFailure, AccountDeletionProviderCleanupMutation,
        AccountDeletionProviderCleanupOutcome, AccountDeletionProviderCleanupStatus,
        AccountDeletionProviderCleanupSummary, AccountDeletionProviderCleanupTargetBinding,
        AccountDeletionProviderCredentialEnvelope, AccountDeletionRepository,
        AccountDeletionRepositoryError, AccountDeletionSafetyGate, AccountDeletionSafetyGateError,
        AccountDeletionStatus, AccountDeletionTransition, DisabledAccountDeletionSafetyGate,
        account_deletion_approval_digest, account_deletion_provider_cleanup_manifest_digest,
    },
    credential_auth::{
        CredentialKind, DEVICE_CLIENT_CONTRACT_VERSION, OpaqueCredential, full_owner_device_scopes,
    },
    provider_admission::{DrainedProviderAdmission, ProviderAdmission},
};

use super::DatabaseScope;

const FRESH_AUTHORITY_WINDOW: Duration = Duration::minutes(5);
const RECOVERY_CODE_MINIMUM_AGE: Duration = Duration::days(1);
const DELETION_COOLING_OFF_PERIOD: Duration = Duration::days(1);
const PROVIDER_CLEANUP_CLAIM_LEASE: Duration = Duration::minutes(15);
const PROVIDER_CLEANUP_DEADLINE: Duration = Duration::days(1);
const PROVIDER_CLEANUP_BACKOFF_CAP_SECONDS: i64 = 3_600;

#[derive(Clone)]
pub struct PostgresAccountDeletionRepository {
    pool: PgPool,
    scope: DatabaseScope,
    safety_gate: Arc<dyn AccountDeletionSafetyGate>,
    external_principal: Option<AccountDeletionPrincipalBinding>,
    provider_admission: Option<ProviderAdmission>,
}

#[derive(Clone)]
struct ProviderCleanupTarget {
    provider_account_id: Uuid,
    provider: AccountDeletionProvider,
    provider_account_revision: u64,
    credential_generation: u64,
    credential_key_version: u32,
    encrypted_credentials_hash: [u8; 32],
}

impl ProviderCleanupTarget {
    fn binding(&self) -> AccountDeletionProviderCleanupTargetBinding {
        AccountDeletionProviderCleanupTargetBinding {
            provider: self.provider,
            provider_account_id: self.provider_account_id,
            provider_account_revision: self.provider_account_revision,
            credential_generation: self.credential_generation,
            credential_key_version: self.credential_key_version,
            encrypted_credentials_hash: self.encrypted_credentials_hash,
        }
    }
}

struct LockedProviderCleanupTarget {
    deletion_id: Uuid,
    target: ProviderCleanupTarget,
    status: AccountDeletionProviderCleanupStatus,
    attempt: u32,
    claim_id: Option<Uuid>,
    claimed_at: Option<DateTime<Utc>>,
    lease_expires_at: Option<DateTime<Utc>>,
    deadline_at: DateTime<Utc>,
}

struct ProviderCleanupSource {
    credential: AccountDeletionProviderCredentialEnvelope,
}

struct ProviderCleanupAttemptReceipt {
    deletion_id: Uuid,
    provider_account_id: Uuid,
    attempt: u32,
    claim_id: Uuid,
    claimed_at: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    outcome: &'static str,
    evidence_hash: Option<[u8; 32]>,
    failure: Option<AccountDeletionProviderCleanupFailure>,
}

struct StoredProviderCleanupAttempt {
    deletion_id: Uuid,
    provider_account_id: Uuid,
    attempt: u32,
    outcome: String,
    evidence_hash: Option<[u8; 32]>,
    failure: Option<AccountDeletionProviderCleanupFailure>,
    finished_at: DateTime<Utc>,
}

enum ProviderCleanupSourceState {
    Exact(ProviderCleanupSource),
    Unavailable,
    Drifted,
}

async fn provider_cleanup_targets(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<Vec<ProviderCleanupTarget>, AccountDeletionRepositoryError> {
    let rows = sqlx::query(
        "SELECT account.id, account.provider, account.status, account.revision, \
         account.credential_key_version, sha256(account.encrypted_credentials) \
             AS encrypted_credentials_hash, \
         scope_state.credential_generation \
         FROM provider_accounts AS account \
         JOIN google_oauth_scope_state AS scope_state \
           ON scope_state.workspace_id = account.workspace_id \
          AND scope_state.user_id = account.user_id \
         WHERE account.workspace_id = $1 AND account.user_id = $2 \
           AND account.status <> 'revoked' \
         ORDER BY account.provider, account.id \
         FOR SHARE OF account, scope_state",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    if rows.len() > ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_TARGETS {
        return Err(AccountDeletionRepositoryError::ProviderCleanupBlocked);
    }

    let mut targets = Vec::with_capacity(rows.len());
    for row in rows {
        let provider_name: String = row.try_get("provider").map_err(internal)?;
        let status: String = row.try_get("status").map_err(internal)?;
        let provider = AccountDeletionProvider::from_storage_name(&provider_name)
            .ok_or(AccountDeletionRepositoryError::ProviderCleanupBlocked)?;
        if !matches!(
            status.as_str(),
            "active" | "paused" | "reauthorization_required"
        ) {
            return Err(AccountDeletionRepositoryError::ProviderCleanupBlocked);
        }
        let key_version: Option<i32> = row.try_get("credential_key_version").map_err(internal)?;
        let key_version = key_version
            .and_then(|version| u32::try_from(version).ok())
            .filter(|version| *version > 0)
            .ok_or(AccountDeletionRepositoryError::ProviderCleanupBlocked)?;
        let hash: Option<Vec<u8>> = row
            .try_get("encrypted_credentials_hash")
            .map_err(internal)?;
        let encrypted_credentials_hash = fixed_hash(
            hash.as_deref()
                .ok_or(AccountDeletionRepositoryError::ProviderCleanupBlocked)?,
        )?;
        let provider_account_id: Uuid = row.try_get("id").map_err(internal)?;
        if provider_account_id.is_nil() {
            return Err(AccountDeletionRepositoryError::ProviderCleanupBlocked);
        }
        targets.push(ProviderCleanupTarget {
            provider_account_id,
            provider,
            provider_account_revision: revision_from_i64(
                row.try_get("revision").map_err(internal)?,
            )?,
            credential_generation: generation_from_i64(
                row.try_get("credential_generation").map_err(internal)?,
            )?,
            credential_key_version: key_version,
            encrypted_credentials_hash,
        });
    }
    Ok(targets)
}

#[allow(clippy::too_many_lines)] // Keeps one transaction's claim state machine auditable.
async fn claim_provider_cleanup_target(
    repository: &PostgresAccountDeletionRepository,
    deletion_id: Uuid,
    claim_id: Uuid,
) -> Result<Option<AccountDeletionProviderCleanupClaim>, AccountDeletionRepositoryError> {
    if deletion_id.is_nil() || claim_id.is_nil() {
        return Err(AccountDeletionRepositoryError::InvalidInput);
    }
    let principal = repository
        .external_principal
        .ok_or(AccountDeletionRepositoryError::Disabled)?;
    let mut transaction = repository.pool.begin().await.map_err(internal)?;
    let lifecycle = lock_lifecycle(&mut transaction, repository.scope, deletion_id).await?;
    if !principal.matches_local_subject_hash(&lifecycle.owner_subject_hash)
        || !lifecycle.matches_external_principal(principal)
    {
        return Err(AccountDeletionRepositoryError::InvalidAuthority);
    }
    lock_deletion_scope(
        &mut transaction,
        repository.scope,
        lifecycle.owner_subject_hash.as_slice(),
    )
    .await?;
    if lifecycle.status != AccountDeletionStatus::ProviderCleanup {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    ensure_exact_fence(&mut transaction, repository.scope, deletion_id).await?;
    let now = database_now(&mut transaction).await?;

    if let Some(target) = provider_cleanup_target_by_claim(&mut transaction, claim_id).await? {
        if target.deletion_id != deletion_id
            || target.claim_id != Some(claim_id)
            || target.status != AccountDeletionProviderCleanupStatus::Claimed
            || target
                .lease_expires_at
                .is_none_or(|expires_at| expires_at <= now)
        {
            return Err(AccountDeletionRepositoryError::Conflict);
        }
        let source =
            exact_provider_cleanup_source(&mut transaction, repository.scope, &target.target)
                .await?
                .ok_or(AccountDeletionRepositoryError::Conflict)?;
        let claim = provider_cleanup_claim(deletion_id, claim_id, &target, source, true)?;
        transaction.commit().await.map_err(internal)?;
        return Ok(Some(claim));
    }
    if provider_cleanup_attempt_by_claim(&mut transaction, claim_id)
        .await?
        .is_some()
    {
        return Err(AccountDeletionRepositoryError::Conflict);
    }

    loop {
        let Some(target) = next_provider_cleanup_target(&mut transaction, deletion_id, now).await?
        else {
            transaction.commit().await.map_err(internal)?;
            return Ok(None);
        };
        if target.deadline_at <= now {
            expire_provider_cleanup_target(
                &mut transaction,
                &target,
                now,
                AccountDeletionProviderCleanupFailure::DeadlineExceeded,
            )
            .await?;
            continue;
        }
        if target.status == AccountDeletionProviderCleanupStatus::Claimed
            && target.attempt >= ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_ATTEMPTS
        {
            expire_provider_cleanup_target(
                &mut transaction,
                &target,
                now,
                AccountDeletionProviderCleanupFailure::RetryExhausted,
            )
            .await?;
            continue;
        }
        let source =
            match provider_cleanup_source_state(&mut transaction, repository.scope, &target.target)
                .await?
            {
                ProviderCleanupSourceState::Exact(source) => source,
                ProviderCleanupSourceState::Unavailable => {
                    require_provider_cleanup_operator(
                        &mut transaction,
                        &target,
                        now,
                        AccountDeletionProviderCleanupFailure::CredentialUnavailable,
                    )
                    .await?;
                    continue;
                }
                ProviderCleanupSourceState::Drifted => {
                    require_provider_cleanup_operator(
                        &mut transaction,
                        &target,
                        now,
                        AccountDeletionProviderCleanupFailure::CredentialDrift,
                    )
                    .await?;
                    continue;
                }
            };
        let target = take_provider_cleanup_claim(&mut transaction, target, claim_id, now).await?;
        let claim = provider_cleanup_claim(deletion_id, claim_id, &target, source, false)?;
        transaction.commit().await.map_err(internal)?;
        return Ok(Some(claim));
    }
}

#[allow(clippy::too_many_lines)] // Keeps one transaction's resolution state machine auditable.
async fn resolve_provider_cleanup_target(
    repository: &PostgresAccountDeletionRepository,
    completion: AccountDeletionProviderCleanupCompletion,
) -> Result<AccountDeletionProviderCleanupMutation, AccountDeletionRepositoryError> {
    validate_provider_cleanup_completion(&completion)?;
    let principal = repository
        .external_principal
        .ok_or(AccountDeletionRepositoryError::Disabled)?;
    let mut transaction = repository.pool.begin().await.map_err(internal)?;
    let lifecycle =
        lock_lifecycle(&mut transaction, repository.scope, completion.deletion_id).await?;
    if !principal.matches_local_subject_hash(&lifecycle.owner_subject_hash)
        || !lifecycle.matches_external_principal(principal)
    {
        return Err(AccountDeletionRepositoryError::InvalidAuthority);
    }
    lock_deletion_scope(
        &mut transaction,
        repository.scope,
        lifecycle.owner_subject_hash.as_slice(),
    )
    .await?;
    if lifecycle.status != AccountDeletionStatus::ProviderCleanup {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    ensure_exact_fence(&mut transaction, repository.scope, completion.deletion_id).await?;
    let target = lock_provider_cleanup_target(
        &mut transaction,
        completion.deletion_id,
        completion.provider_account_id,
    )
    .await?;
    if let Some(receipt) =
        provider_cleanup_attempt_by_claim(&mut transaction, completion.claim_id).await?
    {
        let mutation = replay_provider_cleanup_completion(&completion, &target, &receipt)?;
        transaction.commit().await.map_err(internal)?;
        return Ok(mutation);
    }
    if target.status != AccountDeletionProviderCleanupStatus::Claimed
        || target.claim_id != Some(completion.claim_id)
        || target.attempt != completion.attempt
    {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    let claimed_at = target
        .claimed_at
        .ok_or(AccountDeletionRepositoryError::Internal)?;
    let lease_expires_at = target
        .lease_expires_at
        .ok_or(AccountDeletionRepositoryError::Internal)?;
    let now = database_now(&mut transaction).await?;
    if lease_expires_at <= now {
        return Err(AccountDeletionRepositoryError::Conflict);
    }

    let (outcome, evidence_hash, failure) = provider_cleanup_outcome_parts(completion.outcome);
    insert_provider_cleanup_attempt(
        &mut transaction,
        &ProviderCleanupAttemptReceipt {
            deletion_id: completion.deletion_id,
            provider_account_id: completion.provider_account_id,
            attempt: completion.attempt,
            claim_id: completion.claim_id,
            claimed_at,
            lease_expires_at,
            finished_at: now,
            outcome,
            evidence_hash,
            failure,
        },
    )
    .await?;

    let (status, next_attempt_at, terminal_failure) = match completion.outcome {
        AccountDeletionProviderCleanupOutcome::Revoked { .. }
        | AccountDeletionProviderCleanupOutcome::AlreadyAbsent { .. } => {
            (AccountDeletionProviderCleanupStatus::Revoked, None, None)
        }
        AccountDeletionProviderCleanupOutcome::RetryableFailure => {
            let next = provider_cleanup_retry_at(completion.attempt, now)?;
            if completion.attempt >= ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_ATTEMPTS {
                (
                    AccountDeletionProviderCleanupStatus::OperatorRequired,
                    None,
                    Some(AccountDeletionProviderCleanupFailure::RetryExhausted),
                )
            } else if next >= target.deadline_at {
                (
                    AccountDeletionProviderCleanupStatus::OperatorRequired,
                    None,
                    Some(AccountDeletionProviderCleanupFailure::DeadlineExceeded),
                )
            } else {
                (
                    AccountDeletionProviderCleanupStatus::RetryWait,
                    Some(next),
                    Some(AccountDeletionProviderCleanupFailure::ProviderUnavailable),
                )
            }
        }
        AccountDeletionProviderCleanupOutcome::OperatorRequired(failure) => (
            AccountDeletionProviderCleanupStatus::OperatorRequired,
            None,
            Some(failure),
        ),
    };
    update_resolved_provider_cleanup_target(
        &mut transaction,
        &target,
        status,
        now,
        next_attempt_at,
        evidence_hash,
        terminal_failure,
    )
    .await?;
    transaction.commit().await.map_err(internal)?;
    Ok(AccountDeletionProviderCleanupMutation {
        deletion_id: completion.deletion_id,
        provider_account_id: completion.provider_account_id,
        status,
        attempt: completion.attempt,
        next_attempt_at,
        replayed: false,
    })
}

async fn provider_cleanup_target_by_claim(
    transaction: &mut Transaction<'_, Postgres>,
    claim_id: Uuid,
) -> Result<Option<LockedProviderCleanupTarget>, AccountDeletionRepositoryError> {
    let row = sqlx::query(
        "SELECT deletion_id, provider_account_id, provider, provider_account_revision, \
         credential_generation, credential_key_version, encrypted_credentials_hash, status, \
         attempt_count, claim_id, claimed_at, lease_expires_at, next_attempt_at, deadline_at \
         FROM account_deletion_provider_cleanup_targets WHERE claim_id = $1 FOR UPDATE",
    )
    .bind(claim_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    row.as_ref()
        .map(provider_cleanup_target_from_row)
        .transpose()
}

async fn next_provider_cleanup_target(
    transaction: &mut Transaction<'_, Postgres>,
    deletion_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<LockedProviderCleanupTarget>, AccountDeletionRepositoryError> {
    let row = sqlx::query(
        "SELECT deletion_id, provider_account_id, provider, provider_account_revision, \
         credential_generation, credential_key_version, encrypted_credentials_hash, status, \
         attempt_count, claim_id, claimed_at, lease_expires_at, next_attempt_at, deadline_at \
         FROM account_deletion_provider_cleanup_targets \
         WHERE deletion_id = $1 AND ( \
             (status IN ('pending', 'retry_wait') AND next_attempt_at <= $2) \
             OR (status = 'claimed' AND lease_expires_at <= $2) \
         ) \
         ORDER BY CASE WHEN status = 'claimed' THEN lease_expires_at ELSE next_attempt_at END, \
                  provider_account_id \
         LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .bind(deletion_id)
    .bind(now)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    row.as_ref()
        .map(provider_cleanup_target_from_row)
        .transpose()
}

async fn lock_provider_cleanup_target(
    transaction: &mut Transaction<'_, Postgres>,
    deletion_id: Uuid,
    provider_account_id: Uuid,
) -> Result<LockedProviderCleanupTarget, AccountDeletionRepositoryError> {
    let row = sqlx::query(
        "SELECT deletion_id, provider_account_id, provider, provider_account_revision, \
         credential_generation, credential_key_version, encrypted_credentials_hash, status, \
         attempt_count, claim_id, claimed_at, lease_expires_at, next_attempt_at, deadline_at \
         FROM account_deletion_provider_cleanup_targets \
         WHERE deletion_id = $1 AND provider_account_id = $2 FOR UPDATE",
    )
    .bind(deletion_id)
    .bind(provider_account_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(AccountDeletionRepositoryError::Conflict)?;
    provider_cleanup_target_from_row(&row)
}

fn provider_cleanup_target_from_row(
    row: &PgRow,
) -> Result<LockedProviderCleanupTarget, AccountDeletionRepositoryError> {
    let provider_name: String = row.try_get("provider").map_err(internal)?;
    let status_name: String = row.try_get("status").map_err(internal)?;
    let key_version: i32 = row.try_get("credential_key_version").map_err(internal)?;
    Ok(LockedProviderCleanupTarget {
        deletion_id: row.try_get("deletion_id").map_err(internal)?,
        target: ProviderCleanupTarget {
            provider_account_id: row.try_get("provider_account_id").map_err(internal)?,
            provider: AccountDeletionProvider::from_storage_name(&provider_name)
                .ok_or(AccountDeletionRepositoryError::Internal)?,
            provider_account_revision: revision_from_i64(
                row.try_get("provider_account_revision").map_err(internal)?,
            )?,
            credential_generation: generation_from_i64(
                row.try_get("credential_generation").map_err(internal)?,
            )?,
            credential_key_version: u32::try_from(key_version)
                .ok()
                .filter(|version| *version > 0)
                .ok_or(AccountDeletionRepositoryError::Internal)?,
            encrypted_credentials_hash: fixed_hash(
                &row.try_get::<Vec<u8>, _>("encrypted_credentials_hash")
                    .map_err(internal)?,
            )?,
        },
        status: AccountDeletionProviderCleanupStatus::from_storage_name(&status_name)
            .ok_or(AccountDeletionRepositoryError::Internal)?,
        attempt: attempt_from_i32(row.try_get("attempt_count").map_err(internal)?)?,
        claim_id: row.try_get("claim_id").map_err(internal)?,
        claimed_at: row.try_get("claimed_at").map_err(internal)?,
        lease_expires_at: row.try_get("lease_expires_at").map_err(internal)?,
        deadline_at: row.try_get("deadline_at").map_err(internal)?,
    })
}

async fn provider_cleanup_source_state(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    target: &ProviderCleanupTarget,
) -> Result<ProviderCleanupSourceState, AccountDeletionRepositoryError> {
    let row = sqlx::query(
        "WITH source AS ( \
             SELECT account.encrypted_credentials, \
                 account.credential_key_version IS NOT NULL \
                     AND account.encrypted_credentials IS NOT NULL \
                     AND scope_state.credential_generation IS NOT NULL \
                     AND scope_state.revocation_kind IS NULL AS available, \
                 account.provider = $4 \
                     AND account.status IN ('active', 'paused', 'reauthorization_required') \
                     AND account.revision = $5 AND account.credential_key_version = $6 \
                     AND sha256(account.encrypted_credentials) = $7 \
                     AND scope_state.credential_generation = $8 \
                     AND scope_state.revocation_kind IS NULL AS exact \
             FROM provider_accounts AS account \
             LEFT JOIN google_oauth_scope_state AS scope_state \
               ON scope_state.workspace_id = account.workspace_id \
              AND scope_state.user_id = account.user_id \
             WHERE account.workspace_id = $1 AND account.user_id = $2 AND account.id = $3 \
             FOR SHARE OF account \
         ) SELECT available, COALESCE(exact, false) AS exact, \
             CASE WHEN exact THEN encrypted_credentials END AS encrypted_credentials \
         FROM source",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(target.provider_account_id)
    .bind(target.provider.as_storage_name())
    .bind(revision_to_i64(target.provider_account_revision)?)
    .bind(i32::try_from(target.credential_key_version).map_err(internal)?)
    .bind(target.encrypted_credentials_hash.as_slice())
    .bind(revision_to_i64(target.credential_generation)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(row) = row else {
        return Ok(ProviderCleanupSourceState::Unavailable);
    };
    if !row.try_get::<bool, _>("available").map_err(internal)? {
        return Ok(ProviderCleanupSourceState::Unavailable);
    }
    if !row.try_get::<bool, _>("exact").map_err(internal)? {
        return Ok(ProviderCleanupSourceState::Drifted);
    }
    // Mismatched sources never return ciphertext over the database connection.
    // The one accepted decoded copy is zeroized on every subsequent error path;
    // SQLx's internal protocol buffers are outside this envelope's ownership.
    let mut ciphertext = Zeroizing::new(
        row.try_get::<Vec<u8>, _>("encrypted_credentials")
            .map_err(internal)?,
    );
    let credential = AccountDeletionProviderCredentialEnvelope::new(
        target.credential_key_version,
        std::mem::take(&mut *ciphertext),
    );
    Ok(ProviderCleanupSourceState::Exact(ProviderCleanupSource {
        credential,
    }))
}

async fn exact_provider_cleanup_source(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    target: &ProviderCleanupTarget,
) -> Result<Option<ProviderCleanupSource>, AccountDeletionRepositoryError> {
    match provider_cleanup_source_state(transaction, scope, target).await? {
        ProviderCleanupSourceState::Exact(source) => Ok(Some(source)),
        ProviderCleanupSourceState::Unavailable | ProviderCleanupSourceState::Drifted => Ok(None),
    }
}

fn provider_cleanup_claim(
    deletion_id: Uuid,
    claim_id: Uuid,
    target: &LockedProviderCleanupTarget,
    source: ProviderCleanupSource,
    replayed: bool,
) -> Result<AccountDeletionProviderCleanupClaim, AccountDeletionRepositoryError> {
    if target.deletion_id != deletion_id
        || target.claim_id != Some(claim_id)
        || source.credential.key_version() != target.target.credential_key_version
        || source.credential.ciphertext().is_empty()
    {
        return Err(AccountDeletionRepositoryError::Internal);
    }
    let lease_expires_at = target
        .lease_expires_at
        .ok_or(AccountDeletionRepositoryError::Internal)?;
    Ok(AccountDeletionProviderCleanupClaim::new(
        deletion_id,
        target.target.binding(),
        claim_id,
        target.attempt,
        lease_expires_at,
        replayed,
        source.credential,
    ))
}

async fn take_provider_cleanup_claim(
    transaction: &mut Transaction<'_, Postgres>,
    mut target: LockedProviderCleanupTarget,
    claim_id: Uuid,
    now: DateTime<Utc>,
) -> Result<LockedProviderCleanupTarget, AccountDeletionRepositoryError> {
    if target.status == AccountDeletionProviderCleanupStatus::Claimed {
        insert_provider_cleanup_attempt(
            transaction,
            &ProviderCleanupAttemptReceipt {
                deletion_id: target.deletion_id,
                provider_account_id: target.target.provider_account_id,
                attempt: target.attempt,
                claim_id: target
                    .claim_id
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
                claimed_at: target
                    .claimed_at
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
                lease_expires_at: target
                    .lease_expires_at
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
                finished_at: now,
                outcome: "retryable_failure",
                evidence_hash: None,
                failure: Some(AccountDeletionProviderCleanupFailure::ClaimLeaseExpired),
            },
        )
        .await?;
    } else if !matches!(
        target.status,
        AccountDeletionProviderCleanupStatus::Pending
            | AccountDeletionProviderCleanupStatus::RetryWait
    ) {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    let attempt = target
        .attempt
        .checked_add(1)
        .filter(|attempt| *attempt <= ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_ATTEMPTS)
        .ok_or(AccountDeletionRepositoryError::Internal)?;
    let lease_expires_at = now
        .checked_add_signed(PROVIDER_CLEANUP_CLAIM_LEASE)
        .ok_or(AccountDeletionRepositoryError::Internal)?;
    let changed = sqlx::query(
        "UPDATE account_deletion_provider_cleanup_targets \
         SET status = 'claimed', attempt_count = $3, claim_id = $4, claimed_at = $5, \
             lease_expires_at = $6, updated_at = $5 \
         WHERE deletion_id = $1 AND provider_account_id = $2",
    )
    .bind(target.deletion_id)
    .bind(target.target.provider_account_id)
    .bind(i32::try_from(attempt).map_err(internal)?)
    .bind(claim_id)
    .bind(now)
    .bind(lease_expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(write_error)?
    .rows_affected();
    if changed != 1 {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    target.status = AccountDeletionProviderCleanupStatus::Claimed;
    target.attempt = attempt;
    target.claim_id = Some(claim_id);
    target.claimed_at = Some(now);
    target.lease_expires_at = Some(lease_expires_at);
    Ok(target)
}

async fn expire_provider_cleanup_target(
    transaction: &mut Transaction<'_, Postgres>,
    target: &LockedProviderCleanupTarget,
    now: DateTime<Utc>,
    failure: AccountDeletionProviderCleanupFailure,
) -> Result<(), AccountDeletionRepositoryError> {
    if target.status == AccountDeletionProviderCleanupStatus::Claimed {
        insert_provider_cleanup_attempt(
            transaction,
            &ProviderCleanupAttemptReceipt {
                deletion_id: target.deletion_id,
                provider_account_id: target.target.provider_account_id,
                attempt: target.attempt,
                claim_id: target
                    .claim_id
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
                claimed_at: target
                    .claimed_at
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
                lease_expires_at: target
                    .lease_expires_at
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
                finished_at: now,
                outcome: "retryable_failure",
                evidence_hash: None,
                failure: Some(AccountDeletionProviderCleanupFailure::ClaimLeaseExpired),
            },
        )
        .await?;
    }
    set_provider_cleanup_operator_required(transaction, target, now, failure).await
}

async fn require_provider_cleanup_operator(
    transaction: &mut Transaction<'_, Postgres>,
    target: &LockedProviderCleanupTarget,
    now: DateTime<Utc>,
    failure: AccountDeletionProviderCleanupFailure,
) -> Result<(), AccountDeletionRepositoryError> {
    if target.status == AccountDeletionProviderCleanupStatus::Claimed {
        insert_provider_cleanup_attempt(
            transaction,
            &ProviderCleanupAttemptReceipt {
                deletion_id: target.deletion_id,
                provider_account_id: target.target.provider_account_id,
                attempt: target.attempt,
                claim_id: target
                    .claim_id
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
                claimed_at: target
                    .claimed_at
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
                lease_expires_at: target
                    .lease_expires_at
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
                finished_at: now,
                outcome: "operator_required",
                evidence_hash: None,
                failure: Some(failure),
            },
        )
        .await?;
    }
    set_provider_cleanup_operator_required(transaction, target, now, failure).await
}

async fn set_provider_cleanup_operator_required(
    transaction: &mut Transaction<'_, Postgres>,
    target: &LockedProviderCleanupTarget,
    now: DateTime<Utc>,
    failure: AccountDeletionProviderCleanupFailure,
) -> Result<(), AccountDeletionRepositoryError> {
    let changed = sqlx::query(
        "UPDATE account_deletion_provider_cleanup_targets \
         SET status = 'operator_required', claim_id = NULL, claimed_at = NULL, \
             lease_expires_at = NULL, operator_required_at = $3, \
             last_failure_code = $4, updated_at = $3 \
         WHERE deletion_id = $1 AND provider_account_id = $2",
    )
    .bind(target.deletion_id)
    .bind(target.target.provider_account_id)
    .bind(now)
    .bind(failure.as_storage_name())
    .execute(&mut **transaction)
    .await
    .map_err(write_error)?
    .rows_affected();
    if changed == 1 {
        Ok(())
    } else {
        Err(AccountDeletionRepositoryError::Conflict)
    }
}

async fn insert_provider_cleanup_attempt(
    transaction: &mut Transaction<'_, Postgres>,
    receipt: &ProviderCleanupAttemptReceipt,
) -> Result<(), AccountDeletionRepositoryError> {
    sqlx::query(
        "INSERT INTO account_deletion_provider_cleanup_attempts (deletion_id, \
         provider_account_id, attempt_number, claim_id, claimed_at, lease_expires_at, \
         finished_at, outcome, evidence_hash, failure_code) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(receipt.deletion_id)
    .bind(receipt.provider_account_id)
    .bind(i32::try_from(receipt.attempt).map_err(internal)?)
    .bind(receipt.claim_id)
    .bind(receipt.claimed_at)
    .bind(receipt.lease_expires_at)
    .bind(receipt.finished_at)
    .bind(receipt.outcome)
    .bind(receipt.evidence_hash.map(|hash| hash.to_vec()))
    .bind(
        receipt
            .failure
            .map(AccountDeletionProviderCleanupFailure::as_storage_name),
    )
    .execute(&mut **transaction)
    .await
    .map_err(write_error)?;
    Ok(())
}

async fn update_resolved_provider_cleanup_target(
    transaction: &mut Transaction<'_, Postgres>,
    target: &LockedProviderCleanupTarget,
    status: AccountDeletionProviderCleanupStatus,
    now: DateTime<Utc>,
    next_attempt_at: Option<DateTime<Utc>>,
    evidence_hash: Option<[u8; 32]>,
    failure: Option<AccountDeletionProviderCleanupFailure>,
) -> Result<(), AccountDeletionRepositoryError> {
    let changed = sqlx::query(
        "UPDATE account_deletion_provider_cleanup_targets \
         SET status = $4, claim_id = NULL, claimed_at = NULL, lease_expires_at = NULL, \
             next_attempt_at = COALESCE($5, next_attempt_at), \
             completed_at = CASE WHEN $4 = 'revoked' THEN $6 ELSE NULL END, \
             operator_required_at = CASE WHEN $4 = 'operator_required' THEN $6 ELSE NULL END, \
             outcome_evidence_hash = $7, last_failure_code = $8, updated_at = $6 \
         WHERE deletion_id = $1 AND provider_account_id = $2 \
           AND status = 'claimed' AND claim_id = $3",
    )
    .bind(target.deletion_id)
    .bind(target.target.provider_account_id)
    .bind(target.claim_id)
    .bind(status.as_storage_name())
    .bind(next_attempt_at)
    .bind(now)
    .bind(evidence_hash.map(|hash| hash.to_vec()))
    .bind(failure.map(AccountDeletionProviderCleanupFailure::as_storage_name))
    .execute(&mut **transaction)
    .await
    .map_err(write_error)?
    .rows_affected();
    if changed == 1 {
        Ok(())
    } else {
        Err(AccountDeletionRepositoryError::Conflict)
    }
}

async fn provider_cleanup_attempt_by_claim(
    transaction: &mut Transaction<'_, Postgres>,
    claim_id: Uuid,
) -> Result<Option<StoredProviderCleanupAttempt>, AccountDeletionRepositoryError> {
    let row = sqlx::query(
        "SELECT deletion_id, provider_account_id, attempt_number, outcome, evidence_hash, \
         failure_code, finished_at FROM account_deletion_provider_cleanup_attempts \
         WHERE claim_id = $1",
    )
    .bind(claim_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let evidence: Option<Vec<u8>> = row.try_get("evidence_hash").map_err(internal)?;
    let failure: Option<String> = row.try_get("failure_code").map_err(internal)?;
    Ok(Some(StoredProviderCleanupAttempt {
        deletion_id: row.try_get("deletion_id").map_err(internal)?,
        provider_account_id: row.try_get("provider_account_id").map_err(internal)?,
        attempt: attempt_from_i32(row.try_get("attempt_number").map_err(internal)?)?,
        outcome: row.try_get("outcome").map_err(internal)?,
        evidence_hash: evidence.as_deref().map(fixed_hash).transpose()?,
        failure: match failure.as_deref() {
            Some(failure) => Some(
                AccountDeletionProviderCleanupFailure::from_storage_name(failure)
                    .ok_or(AccountDeletionRepositoryError::Internal)?,
            ),
            None => None,
        },
        finished_at: row.try_get("finished_at").map_err(internal)?,
    }))
}

fn replay_provider_cleanup_completion(
    completion: &AccountDeletionProviderCleanupCompletion,
    target: &LockedProviderCleanupTarget,
    receipt: &StoredProviderCleanupAttempt,
) -> Result<AccountDeletionProviderCleanupMutation, AccountDeletionRepositoryError> {
    let (expected_outcome, expected_evidence, expected_failure) =
        provider_cleanup_outcome_parts(completion.outcome);
    if receipt.deletion_id != completion.deletion_id
        || receipt.provider_account_id != completion.provider_account_id
        || receipt.attempt != completion.attempt
        || receipt.outcome != expected_outcome
        || receipt.evidence_hash != expected_evidence
        || receipt.failure != expected_failure
    {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    let (status, next_attempt_at) = match completion.outcome {
        AccountDeletionProviderCleanupOutcome::Revoked { .. }
        | AccountDeletionProviderCleanupOutcome::AlreadyAbsent { .. } => {
            (AccountDeletionProviderCleanupStatus::Revoked, None)
        }
        AccountDeletionProviderCleanupOutcome::OperatorRequired(_) => {
            (AccountDeletionProviderCleanupStatus::OperatorRequired, None)
        }
        AccountDeletionProviderCleanupOutcome::RetryableFailure => {
            let next = provider_cleanup_retry_at(receipt.attempt, receipt.finished_at)?;
            if receipt.attempt >= ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_ATTEMPTS
                || next >= target.deadline_at
            {
                (AccountDeletionProviderCleanupStatus::OperatorRequired, None)
            } else {
                (AccountDeletionProviderCleanupStatus::RetryWait, Some(next))
            }
        }
    };
    Ok(AccountDeletionProviderCleanupMutation {
        deletion_id: completion.deletion_id,
        provider_account_id: completion.provider_account_id,
        status,
        attempt: completion.attempt,
        next_attempt_at,
        replayed: true,
    })
}

fn validate_provider_cleanup_completion(
    completion: &AccountDeletionProviderCleanupCompletion,
) -> Result<(), AccountDeletionRepositoryError> {
    let invalid_operator_failure = match completion.outcome {
        AccountDeletionProviderCleanupOutcome::OperatorRequired(failure) => !matches!(
            failure,
            AccountDeletionProviderCleanupFailure::ProviderRejected
                | AccountDeletionProviderCleanupFailure::CredentialUnavailable
                | AccountDeletionProviderCleanupFailure::CredentialDrift
        ),
        _ => false,
    };
    let invalid_evidence = match completion.outcome {
        AccountDeletionProviderCleanupOutcome::Revoked { evidence_hash }
        | AccountDeletionProviderCleanupOutcome::AlreadyAbsent { evidence_hash } => {
            evidence_hash.iter().all(|byte| *byte == 0)
        }
        _ => false,
    };
    if completion.deletion_id.is_nil()
        || completion.provider_account_id.is_nil()
        || completion.claim_id.is_nil()
        || completion.attempt == 0
        || completion.attempt > ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_ATTEMPTS
        || invalid_operator_failure
        || invalid_evidence
    {
        Err(AccountDeletionRepositoryError::InvalidInput)
    } else {
        Ok(())
    }
}

fn provider_cleanup_outcome_parts(
    outcome: AccountDeletionProviderCleanupOutcome,
) -> (
    &'static str,
    Option<[u8; 32]>,
    Option<AccountDeletionProviderCleanupFailure>,
) {
    match outcome {
        AccountDeletionProviderCleanupOutcome::Revoked { evidence_hash } => {
            ("revoked", Some(evidence_hash), None)
        }
        AccountDeletionProviderCleanupOutcome::AlreadyAbsent { evidence_hash } => {
            ("already_absent", Some(evidence_hash), None)
        }
        AccountDeletionProviderCleanupOutcome::RetryableFailure => (
            "retryable_failure",
            None,
            Some(AccountDeletionProviderCleanupFailure::ProviderUnavailable),
        ),
        AccountDeletionProviderCleanupOutcome::OperatorRequired(failure) => {
            ("operator_required", None, Some(failure))
        }
    }
}

fn provider_cleanup_retry_at(
    attempt: u32,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, AccountDeletionRepositoryError> {
    let exponent = attempt.saturating_sub(1).min(62);
    let seconds = 1_i64
        .checked_shl(exponent)
        .unwrap_or(i64::MAX)
        .min(PROVIDER_CLEANUP_BACKOFF_CAP_SECONDS);
    now.checked_add_signed(Duration::seconds(seconds))
        .ok_or(AccountDeletionRepositoryError::Internal)
}

impl std::fmt::Debug for PostgresAccountDeletionRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresAccountDeletionRepository")
            .field("scope", &self.scope)
            .field("safety_gate", &"[REDACTED]")
            .field("external_principal", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl PostgresAccountDeletionRepository {
    /// Creates a repository whose destructive workflow is disabled. A
    /// deployment must explicitly supply both external safety integrations.
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self {
            pool,
            scope,
            safety_gate: Arc::new(DisabledAccountDeletionSafetyGate),
            external_principal: None,
            provider_admission: None,
        }
    }

    /// Supplies the external authority together with the deployment-keyed
    /// principal it must use. There is intentionally no gate-only overload:
    /// the database's local unkeyed owner digest may never be substituted.
    #[must_use]
    pub fn with_safety_gate(
        mut self,
        gate: Arc<dyn AccountDeletionSafetyGate>,
        principal: AccountDeletionPrincipalBinding,
    ) -> Self {
        self.safety_gate = gate;
        self.external_principal = Some(principal);
        self
    }

    /// Binds the exact controller used by both Google services in this runtime.
    /// Matching scope alone is insufficient: a separately constructed, idle
    /// controller cannot prove that the actual runtime has drained. Fencing
    /// additionally requires its durable backend and rechecks all overlapping
    /// registrations. This does not activate deletion or supply restore safety.
    #[must_use]
    pub fn with_provider_admission(mut self, admission: ProviderAdmission) -> Self {
        self.provider_admission = Some(admission);
        self
    }
}

#[async_trait]
#[allow(clippy::too_many_lines)]
impl AccountDeletionRepository for PostgresAccountDeletionRepository {
    async fn provider_cleanup_status(
        &self,
        deletion_id: Uuid,
    ) -> Result<Option<AccountDeletionProviderCleanupSummary>, AccountDeletionRepositoryError> {
        if deletion_id.is_nil() {
            return Err(AccountDeletionRepositoryError::InvalidInput);
        }
        let row = sqlx::query(
            "SELECT lifecycle.status, \
                 COALESCE(lifecycle.provider_cleanup_policy_version = 1 \
                     AND lifecycle.provider_cleanup_target_count = count(target.provider_account_id) \
                     AND lifecycle.provider_cleanup_manifest_hash = \
                         calculate_account_deletion_provider_cleanup_manifest(lifecycle.id), \
                     false) AS manifest_sealed, \
                 count(target.provider_account_id)::integer AS target_count, \
                 count(*) FILTER (WHERE target.status = 'pending')::integer AS pending_count, \
                 count(*) FILTER (WHERE target.status = 'claimed')::integer AS claimed_count, \
                 count(*) FILTER (WHERE target.status = 'retry_wait')::integer AS retry_wait_count, \
                 count(*) FILTER (WHERE target.status = 'revoked')::integer AS revoked_count, \
                 count(*) FILTER (WHERE target.status = 'operator_required')::integer \
                     AS operator_required_count, \
                 min(CASE WHEN target.status IN ('pending', 'retry_wait') \
                         THEN target.next_attempt_at \
                     WHEN target.status = 'claimed' THEN target.lease_expires_at END) \
                     AS next_attempt_at, \
                 COALESCE(array_agg(DISTINCT target.last_failure_code::text \
                     ORDER BY target.last_failure_code::text) FILTER ( \
                         WHERE target.status = 'operator_required'), ARRAY[]::text[]) \
                     AS operator_reasons \
             FROM account_deletion_lifecycles AS lifecycle \
             LEFT JOIN account_deletion_provider_cleanup_targets AS target \
               ON target.deletion_id = lifecycle.id \
             WHERE lifecycle.id = $1 AND lifecycle.workspace_id = $2 AND lifecycle.user_id = $3 \
             GROUP BY lifecycle.id",
        )
        .bind(deletion_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let count = |column: &str| -> Result<u32, AccountDeletionRepositoryError> {
            u32::try_from(row.try_get::<i32, _>(column).map_err(internal)?).map_err(internal)
        };
        let reasons: Vec<String> = row.try_get("operator_reasons").map_err(internal)?;
        Ok(Some(AccountDeletionProviderCleanupSummary {
            deletion_id,
            lifecycle_status: status_from_row(&row)?,
            manifest_sealed: row.try_get("manifest_sealed").map_err(internal)?,
            target_count: count("target_count")?,
            pending_count: count("pending_count")?,
            claimed_count: count("claimed_count")?,
            retry_wait_count: count("retry_wait_count")?,
            revoked_count: count("revoked_count")?,
            operator_required_count: count("operator_required_count")?,
            next_attempt_at: row.try_get("next_attempt_at").map_err(internal)?,
            operator_reasons: reasons
                .iter()
                .map(|reason| {
                    AccountDeletionProviderCleanupFailure::from_storage_name(reason)
                        .ok_or(AccountDeletionRepositoryError::Internal)
                })
                .collect::<Result<_, _>>()?,
        }))
    }

    async fn lifecycle(
        &self,
        deletion_id: Uuid,
    ) -> Result<Option<AccountDeletionLifecycle>, AccountDeletionRepositoryError> {
        if deletion_id.is_nil() {
            return Err(AccountDeletionRepositoryError::InvalidInput);
        }
        let row = sqlx::query(
            "SELECT id, workspace_id, user_id, status, revision, prepared_at, updated_at, \
             local_purge_completed_at FROM account_deletion_lifecycles \
             WHERE id = $1 AND workspace_id = $2 AND user_id = $3",
        )
        .bind(deletion_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.as_ref().map(lifecycle_from_row).transpose()
    }

    async fn prepare(
        &self,
        preparation: AccountDeletionPreparation,
        recovery_code: &OpaqueCredential<'_>,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError> {
        validate_preparation(&preparation, self.scope, recovery_code)?;
        let principal = self
            .external_principal
            .ok_or(AccountDeletionRepositoryError::Disabled)?;
        let recovery_digest = recovery_code.persistence_digest();

        // Reject stale/under-scoped/shared requests before the external
        // per-principal allowance can be consumed. The durable insert repeats
        // this entire preflight after the external call.
        let mut preflight = self.pool.begin().await.map_err(internal)?;
        let preflight_subject_hash = fetch_subject_hash(&mut preflight, self.scope).await?;
        if !principal.matches_local_subject_hash(&preflight_subject_hash) {
            return Err(AccountDeletionRepositoryError::InvalidAuthority);
        }
        lock_deletion_scope(&mut preflight, self.scope, &preflight_subject_hash).await?;
        if let Some(replay) = lookup_preparation(
            &mut *preflight,
            self.scope,
            &preparation,
            &recovery_digest,
            principal,
        )
        .await?
        {
            preflight.commit().await.map_err(internal)?;
            return Ok(replay);
        }
        ensure_personal_scope(&mut preflight, self.scope).await?;
        ensure_no_fence(&mut preflight, self.scope, &preflight_subject_hash).await?;
        ensure_no_active_lifecycle(&mut preflight, self.scope).await?;
        let preflight_at = database_now(&mut preflight).await?;
        validate_fresh_full_owner_session(
            &mut preflight,
            self.scope,
            preparation.authorizing_session_id,
            preparation.authorizing_session_revision,
            preflight_at,
        )
        .await?;
        validate_current_recovery_code(
            &mut preflight,
            self.scope,
            &preparation,
            Some(&recovery_digest),
            preflight_at,
        )
        .await?;
        preflight.commit().await.map_err(internal)?;

        let evidence = self
            .safety_gate
            .authorize_preparation(principal.pseudonym(), preparation.id)
            .await
            .map_err(safety_gate_error)?;
        if evidence
            .principal_rate_limit_hash
            .iter()
            .all(|byte| *byte == 0)
        {
            return Err(AccountDeletionRepositoryError::Internal);
        }

        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let subject_hash = fetch_subject_hash(&mut transaction, self.scope).await?;
        if !principal.matches_local_subject_hash(&subject_hash) {
            return Err(AccountDeletionRepositoryError::InvalidAuthority);
        }
        lock_deletion_scope(&mut transaction, self.scope, &subject_hash).await?;
        if let Some(replay) = lookup_preparation(
            &mut *transaction,
            self.scope,
            &preparation,
            &recovery_digest,
            principal,
        )
        .await?
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(replay);
        }
        ensure_personal_scope(&mut transaction, self.scope).await?;
        ensure_no_fence(&mut transaction, self.scope, &subject_hash).await?;
        ensure_no_active_lifecycle(&mut transaction, self.scope).await?;
        let prepared_at = database_now(&mut transaction).await?;
        let credential_issued_at = validate_fresh_full_owner_session(
            &mut transaction,
            self.scope,
            preparation.authorizing_session_id,
            preparation.authorizing_session_revision,
            prepared_at,
        )
        .await?;
        let recovery_code_created_at = validate_current_recovery_code(
            &mut transaction,
            self.scope,
            &preparation,
            Some(&recovery_digest),
            prepared_at,
        )
        .await?;

        let inserted = sqlx::query_scalar::<_, i64>(
            "INSERT INTO account_deletion_lifecycles (id, workspace_id, user_id, \
             owner_subject_hash, prepare_request_hash, explicit_approval_digest, \
             principal_rate_limit_evidence_hash, external_principal_key_version, \
             external_principal_pseudonym, \
             authorizing_session_id, authorizing_session_revision, \
             authorizing_credential_issued_at, authorizing_recovery_code_id, \
             authorizing_recovery_code_revision, authorizing_recovery_code_created_at, \
             status, revision, prepared_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
             $14, $15, 'prepared', 1, $16, $16, $16) RETURNING revision",
        )
        .bind(preparation.id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(subject_hash.as_slice())
        .bind(preparation.request_hash.as_slice())
        .bind(preparation.explicit_approval_digest.as_slice())
        .bind(evidence.principal_rate_limit_hash.as_slice())
        .bind(
            i32::try_from(principal.pseudonym().key_version())
                .map_err(|_| AccountDeletionRepositoryError::InvalidInput)?,
        )
        .bind(principal.pseudonym().digest().as_slice())
        .bind(preparation.authorizing_session_id)
        .bind(
            i64::try_from(preparation.authorizing_session_revision)
                .map_err(|_| AccountDeletionRepositoryError::InvalidInput)?,
        )
        .bind(credential_issued_at)
        .bind(preparation.authorizing_recovery_code_id)
        .bind(revision_to_i64(
            preparation.authorizing_recovery_code_revision,
        )?)
        .bind(recovery_code_created_at)
        .bind(prepared_at)
        .fetch_one(&mut *transaction)
        .await
        .map_err(write_error)?;
        transaction.commit().await.map_err(internal)?;
        Ok(AccountDeletionMutation {
            deletion_id: preparation.id,
            status: AccountDeletionStatus::Prepared,
            revision: revision_from_i64(inserted)?,
            replayed: false,
        })
    }

    async fn begin_fence(
        &self,
        confirmation: AccountDeletionFenceConfirmation,
        drained: &DrainedProviderAdmission,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError> {
        let principal = self
            .external_principal
            .ok_or(AccountDeletionRepositoryError::Disabled)?;
        let transition = confirmation.transition.clone();
        validate_transition(&transition)?;
        validate_fence_confirmation(&confirmation, &transition, self.scope)?;
        let admission = self
            .provider_admission
            .as_ref()
            .ok_or(AccountDeletionRepositoryError::Disabled)?;
        if admission.scope().workspace_id != self.scope.workspace_id
            || admission.scope().user_id != self.scope.user_id
            || !drained.matches(admission, transition.deletion_id)
        {
            return Err(AccountDeletionRepositoryError::ProviderCleanupBlocked);
        }
        if !admission.is_durable() {
            return Err(AccountDeletionRepositoryError::Disabled);
        }
        // The opaque proof stays borrowed across the entire transaction,
        // including replay and ambiguous commit results. Its controller never
        // reopens on drop, and draining occurred before any database lock.
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let lifecycle =
            lock_lifecycle(&mut transaction, self.scope, transition.deletion_id).await?;
        if !principal.matches_local_subject_hash(&lifecycle.owner_subject_hash)
            || !lifecycle.matches_external_principal(principal)
        {
            return Err(AccountDeletionRepositoryError::InvalidAuthority);
        }
        lock_deletion_scope(
            &mut transaction,
            self.scope,
            lifecycle.owner_subject_hash.as_slice(),
        )
        .await?;
        super::ensure_provider_admission_drained(
            &mut transaction,
            self.scope,
            transition.deletion_id,
        )
        .await?;
        if let Some(replay) =
            lookup_fence_confirmation(&mut transaction, &transition, &confirmation).await?
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(replay);
        }
        validate_locked_transition(&lifecycle, &transition)?;
        let current_subject_hash = fetch_subject_hash(&mut transaction, self.scope).await?;
        if current_subject_hash != lifecycle.owner_subject_hash {
            return Err(AccountDeletionRepositoryError::InvalidAuthority);
        }
        let operation_at = database_now(&mut transaction).await?;
        let ready_at = lifecycle
            .prepared_at
            .checked_add_signed(DELETION_COOLING_OFF_PERIOD)
            .ok_or(AccountDeletionRepositoryError::Internal)?;
        if operation_at < ready_at {
            return Err(AccountDeletionRepositoryError::CooldownPending);
        }
        if confirmation.confirming_session_id == lifecycle.authorizing_session_id
            && confirmation.confirming_session_revision <= lifecycle.authorizing_session_revision
        {
            return Err(AccountDeletionRepositoryError::InvalidAuthority);
        }
        let credential_issued_at = validate_fresh_full_owner_session(
            &mut transaction,
            self.scope,
            confirmation.confirming_session_id,
            confirmation.confirming_session_revision,
            operation_at,
        )
        .await?;
        validate_stored_current_recovery_code(
            &mut transaction,
            self.scope,
            lifecycle.authorizing_recovery_code_id,
            lifecycle.authorizing_recovery_code_revision,
            lifecycle.authorizing_recovery_code_created_at,
            operation_at,
        )
        .await?;
        ensure_personal_scope(&mut transaction, self.scope).await?;
        ensure_provider_cleanup_quiescent(&mut transaction, self.scope).await?;
        ensure_no_fence(
            &mut transaction,
            self.scope,
            lifecycle.owner_subject_hash.as_slice(),
        )
        .await?;
        let result_revision = transition
            .expected_revision
            .checked_add(1)
            .ok_or(AccountDeletionRepositoryError::InvalidInput)?;
        let changed = sqlx::query_scalar::<_, i64>(
            "UPDATE account_deletion_lifecycles SET status = 'fence_committing', \
             revision = revision + 1, fence_committing_at = $4, confirmed_at = $4, \
             confirming_session_id = $5, confirming_session_revision = $6, \
             confirming_credential_issued_at = $7, confirming_approval_digest = $8, \
             updated_at = $4 \
             WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
             AND status = 'prepared' AND revision = $9 RETURNING revision",
        )
        .bind(transition.deletion_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(operation_at)
        .bind(confirmation.confirming_session_id)
        .bind(revision_to_i64(confirmation.confirming_session_revision)?)
        .bind(credential_issued_at)
        .bind(confirmation.explicit_approval_digest.as_slice())
        .bind(revision_to_i64(transition.expected_revision)?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(AccountDeletionRepositoryError::Conflict)?;
        if revision_from_i64(changed)? != result_revision {
            return Err(AccountDeletionRepositoryError::Internal);
        }
        insert_fence_confirmation_receipt(
            &mut transaction,
            &transition,
            &confirmation,
            result_revision,
            operation_at,
        )
        .await?;
        sqlx::query(
            "INSERT INTO account_deletion_fences (deletion_id, workspace_id, user_id, \
             owner_subject_hash, lifecycle_revision, fenced_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(transition.deletion_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(lifecycle.owner_subject_hash.as_slice())
        .bind(revision_to_i64(result_revision)?)
        .bind(operation_at)
        .execute(&mut *transaction)
        .await
        .map_err(write_error)?;
        transaction.commit().await.map_err(internal)?;
        Ok(mutation_for_transition(&transition, result_revision, false))
    }

    async fn seal_provider_cleanup(
        &self,
        transition: AccountDeletionTransition,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError> {
        validate_transition(&transition)?;
        if transition.from != AccountDeletionStatus::Fenced
            || transition.to != AccountDeletionStatus::ProviderCleanup
            || transition.failure_code.is_some()
        {
            return Err(AccountDeletionRepositoryError::InvalidInput);
        }
        let principal = self
            .external_principal
            .ok_or(AccountDeletionRepositoryError::Disabled)?;
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let lifecycle =
            lock_lifecycle(&mut transaction, self.scope, transition.deletion_id).await?;
        if !principal.matches_local_subject_hash(&lifecycle.owner_subject_hash)
            || !lifecycle.matches_external_principal(principal)
        {
            return Err(AccountDeletionRepositoryError::InvalidAuthority);
        }
        lock_deletion_scope(
            &mut transaction,
            self.scope,
            lifecycle.owner_subject_hash.as_slice(),
        )
        .await?;
        if let Some(replay) = lookup_transition(&mut transaction, &transition).await? {
            transaction.commit().await.map_err(internal)?;
            return Ok(replay);
        }
        validate_locked_transition(&lifecycle, &transition)?;
        ensure_exact_fence(&mut transaction, self.scope, transition.deletion_id).await?;
        ensure_provider_cleanup_quiescent(&mut transaction, self.scope).await?;
        let operation_at = database_now(&mut transaction).await?;
        let deadline_at = operation_at
            .checked_add_signed(PROVIDER_CLEANUP_DEADLINE)
            .ok_or(AccountDeletionRepositoryError::Internal)?;
        let targets = provider_cleanup_targets(&mut transaction, self.scope).await?;
        let bindings = targets
            .iter()
            .map(ProviderCleanupTarget::binding)
            .collect::<Vec<_>>();
        let manifest_hash =
            account_deletion_provider_cleanup_manifest_digest(transition.deletion_id, &bindings);
        for target in &targets {
            sqlx::query(
                "INSERT INTO account_deletion_provider_cleanup_targets (deletion_id, \
                 provider_account_id, provider, provider_account_revision, \
                 credential_generation, credential_key_version, encrypted_credentials_hash, \
                 next_attempt_at, deadline_at, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $8, $8)",
            )
            .bind(transition.deletion_id)
            .bind(target.provider_account_id)
            .bind(target.provider.as_storage_name())
            .bind(revision_to_i64(target.provider_account_revision)?)
            .bind(revision_to_i64(target.credential_generation)?)
            .bind(
                i32::try_from(target.credential_key_version)
                    .map_err(|_| AccountDeletionRepositoryError::Internal)?,
            )
            .bind(target.encrypted_credentials_hash.as_slice())
            .bind(operation_at)
            .bind(deadline_at)
            .execute(&mut *transaction)
            .await
            .map_err(write_error)?;
        }
        let result_revision = transition
            .expected_revision
            .checked_add(1)
            .ok_or(AccountDeletionRepositoryError::InvalidInput)?;
        let changed = sqlx::query_scalar::<_, i64>(
            "UPDATE account_deletion_lifecycles SET status = 'provider_cleanup', \
             revision = revision + 1, provider_cleanup_at = $4, \
             provider_cleanup_policy_version = 1, provider_cleanup_target_count = $5, \
             provider_cleanup_manifest_hash = $6, updated_at = $4 \
             WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
             AND status = 'fenced' AND revision = $7 RETURNING revision",
        )
        .bind(transition.deletion_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(operation_at)
        .bind(i32::try_from(targets.len()).map_err(|_| AccountDeletionRepositoryError::Internal)?)
        .bind(manifest_hash.as_slice())
        .bind(revision_to_i64(transition.expected_revision)?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(write_error)?
        .ok_or(AccountDeletionRepositoryError::Conflict)?;
        if revision_from_i64(changed)? != result_revision {
            return Err(AccountDeletionRepositoryError::Internal);
        }
        insert_transition_receipt(&mut transaction, &transition, result_revision, operation_at)
            .await?;
        transaction.commit().await.map_err(internal)?;
        Ok(mutation_for_transition(&transition, result_revision, false))
    }

    async fn claim_provider_cleanup(
        &self,
        deletion_id: Uuid,
        claim_id: Uuid,
    ) -> Result<Option<AccountDeletionProviderCleanupClaim>, AccountDeletionRepositoryError> {
        claim_provider_cleanup_target(self, deletion_id, claim_id).await
    }

    async fn resolve_provider_cleanup(
        &self,
        completion: AccountDeletionProviderCleanupCompletion,
    ) -> Result<AccountDeletionProviderCleanupMutation, AccountDeletionRepositoryError> {
        resolve_provider_cleanup_target(self, completion).await
    }

    async fn advance(
        &self,
        transition: AccountDeletionTransition,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError> {
        validate_transition(&transition)?;
        if transition.from == AccountDeletionStatus::Fenced
            && transition.to == AccountDeletionStatus::ProviderCleanup
        {
            return Err(AccountDeletionRepositoryError::InvalidInput);
        }
        if !valid_regular_transition(&transition) {
            return Err(AccountDeletionRepositoryError::InvalidInput);
        }
        if transition.from == AccountDeletionStatus::FenceCommitting
            && transition.to == AccountDeletionStatus::Fenced
        {
            advance_with_external_tombstone(self, transition).await
        } else {
            let principal = if transition.from == AccountDeletionStatus::Prepared
                && transition.to == AccountDeletionStatus::Cancelled
            {
                None
            } else {
                Some(
                    self.external_principal
                        .ok_or(AccountDeletionRepositoryError::Disabled)?,
                )
            };
            advance_local_transition(&self.pool, self.scope, transition, principal).await
        }
    }
}

async fn advance_local_transition(
    pool: &PgPool,
    scope: DatabaseScope,
    transition: AccountDeletionTransition,
    principal: Option<AccountDeletionPrincipalBinding>,
) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError> {
    let mut transaction = pool.begin().await.map_err(internal)?;
    let lifecycle = lock_lifecycle(&mut transaction, scope, transition.deletion_id).await?;
    if principal.is_some_and(|principal| {
        !principal.matches_local_subject_hash(&lifecycle.owner_subject_hash)
            || !lifecycle.matches_external_principal(principal)
    }) {
        return Err(AccountDeletionRepositoryError::InvalidAuthority);
    }
    lock_deletion_scope(
        &mut transaction,
        scope,
        lifecycle.owner_subject_hash.as_slice(),
    )
    .await?;
    if let Some(replay) = lookup_transition(&mut transaction, &transition).await? {
        transaction.commit().await.map_err(internal)?;
        return Ok(replay);
    }
    validate_locked_transition(&lifecycle, &transition)?;
    if transition.from != AccountDeletionStatus::Prepared {
        ensure_exact_fence(&mut transaction, scope, transition.deletion_id).await?;
    }
    let operation_at = database_now(&mut transaction).await?;
    let result_revision =
        apply_transition(&mut transaction, scope, &transition, None, operation_at).await?;
    insert_transition_receipt(&mut transaction, &transition, result_revision, operation_at).await?;
    transaction.commit().await.map_err(internal)?;
    Ok(mutation_for_transition(&transition, result_revision, false))
}

/// The local `fence_committing` row and hard fence are the durable intent for
/// this cross-store action. No database lock is held while the external system
/// is awaited. If the process dies after the external commit, the exact retry
/// repeats the idempotent call and then records the same tombstone evidence.
async fn advance_with_external_tombstone(
    repository: &PostgresAccountDeletionRepository,
    transition: AccountDeletionTransition,
) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError> {
    let principal = repository
        .external_principal
        .ok_or(AccountDeletionRepositoryError::Disabled)?;
    let mut preflight = repository.pool.begin().await.map_err(internal)?;
    let lifecycle =
        lock_lifecycle(&mut preflight, repository.scope, transition.deletion_id).await?;
    lock_deletion_scope(
        &mut preflight,
        repository.scope,
        lifecycle.owner_subject_hash.as_slice(),
    )
    .await?;
    let replay = lookup_transition(&mut preflight, &transition).await?;
    if replay.is_none() {
        validate_locked_transition(&lifecycle, &transition)?;
    }
    ensure_exact_fence(&mut preflight, repository.scope, transition.deletion_id).await?;
    let current_subject_hash = fetch_subject_hash(&mut preflight, repository.scope).await?;
    if current_subject_hash != lifecycle.owner_subject_hash {
        return Err(AccountDeletionRepositoryError::InvalidAuthority);
    }
    preflight.commit().await.map_err(internal)?;

    if !principal.matches_local_subject_hash(&current_subject_hash)
        || !lifecycle.matches_external_principal(principal)
    {
        return Err(AccountDeletionRepositoryError::InvalidAuthority);
    }
    let evidence = repository
        .safety_gate
        .commit_tombstone(principal.pseudonym(), transition.deletion_id)
        .await
        .map_err(safety_gate_error)?;
    if evidence
        .external_tombstone_hash
        .iter()
        .all(|byte| *byte == 0)
    {
        return Err(AccountDeletionRepositoryError::Internal);
    }
    if let Some(replay) = replay {
        if lifecycle.external_tombstone_evidence_hash != Some(evidence.external_tombstone_hash) {
            return Err(AccountDeletionRepositoryError::Internal);
        }
        return Ok(replay);
    }

    let mut transaction = repository.pool.begin().await.map_err(internal)?;
    let lifecycle =
        lock_lifecycle(&mut transaction, repository.scope, transition.deletion_id).await?;
    lock_deletion_scope(
        &mut transaction,
        repository.scope,
        lifecycle.owner_subject_hash.as_slice(),
    )
    .await?;
    if let Some(replay) = lookup_transition(&mut transaction, &transition).await? {
        if lifecycle.external_tombstone_evidence_hash != Some(evidence.external_tombstone_hash) {
            return Err(AccountDeletionRepositoryError::Internal);
        }
        transaction.commit().await.map_err(internal)?;
        return Ok(replay);
    }
    validate_locked_transition(&lifecycle, &transition)?;
    ensure_exact_fence(&mut transaction, repository.scope, transition.deletion_id).await?;
    let operation_at = database_now(&mut transaction).await?;
    let result_revision = apply_transition(
        &mut transaction,
        repository.scope,
        &transition,
        Some(evidence.external_tombstone_hash),
        operation_at,
    )
    .await?;
    insert_transition_receipt(&mut transaction, &transition, result_revision, operation_at).await?;
    transaction.commit().await.map_err(internal)?;
    Ok(mutation_for_transition(&transition, result_revision, false))
}

fn validate_locked_transition(
    lifecycle: &LockedLifecycle,
    transition: &AccountDeletionTransition,
) -> Result<(), AccountDeletionRepositoryError> {
    if lifecycle.status != transition.from || lifecycle.revision != transition.expected_revision {
        Err(AccountDeletionRepositoryError::Conflict)
    } else {
        Ok(())
    }
}

async fn apply_transition(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    transition: &AccountDeletionTransition,
    tombstone_hash: Option<[u8; 32]>,
    operation_at: DateTime<Utc>,
) -> Result<u64, AccountDeletionRepositoryError> {
    let result_revision = transition
        .expected_revision
        .checked_add(1)
        .ok_or(AccountDeletionRepositoryError::InvalidInput)?;
    let changed = sqlx::query_scalar::<_, i64>(
        "UPDATE account_deletion_lifecycles SET status = $4, revision = revision + 1, \
         fenced_at = CASE WHEN $4 = 'fenced' THEN $5 ELSE fenced_at END, \
         provider_cleanup_at = CASE WHEN $4 = 'provider_cleanup' THEN $5 \
             ELSE provider_cleanup_at END, \
         purge_at = CASE WHEN $4 = 'purge' THEN $5 ELSE purge_at END, \
         cancelled_at = CASE WHEN $4 = 'cancelled' THEN $5 ELSE cancelled_at END, \
         external_tombstone_evidence_hash = CASE WHEN $4 = 'fenced' THEN $8 \
             ELSE external_tombstone_evidence_hash END, \
         updated_at = $5 WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
         AND status = $6 AND revision = $7 RETURNING revision",
    )
    .bind(transition.deletion_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(transition.to.as_storage_name())
    .bind(operation_at)
    .bind(transition.from.as_storage_name())
    .bind(revision_to_i64(transition.expected_revision)?)
    .bind(tombstone_hash.map(|hash| hash.to_vec()))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(AccountDeletionRepositoryError::Conflict)?;
    if revision_from_i64(changed)? != result_revision {
        return Err(AccountDeletionRepositoryError::Internal);
    }
    Ok(result_revision)
}

struct LockedLifecycle {
    status: AccountDeletionStatus,
    revision: u64,
    prepared_at: DateTime<Utc>,
    owner_subject_hash: Vec<u8>,
    external_tombstone_evidence_hash: Option<[u8; 32]>,
    external_principal_key_version: Option<i32>,
    external_principal_pseudonym: Option<Vec<u8>>,
    authorizing_session_id: Uuid,
    authorizing_session_revision: u64,
    authorizing_recovery_code_id: Uuid,
    authorizing_recovery_code_revision: u64,
    authorizing_recovery_code_created_at: DateTime<Utc>,
}

impl LockedLifecycle {
    fn matches_external_principal(&self, binding: AccountDeletionPrincipalBinding) -> bool {
        let pseudonym = binding.pseudonym();
        self.external_principal_key_version == i32::try_from(pseudonym.key_version()).ok()
            && self.external_principal_pseudonym.as_deref() == Some(pseudonym.digest().as_slice())
    }
}

async fn lookup_preparation<'e, E>(
    executor: E,
    scope: DatabaseScope,
    preparation: &AccountDeletionPreparation,
    recovery_digest: &[u8; 32],
    principal: AccountDeletionPrincipalBinding,
) -> Result<Option<AccountDeletionMutation>, AccountDeletionRepositoryError>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let row = sqlx::query(
        "SELECT prepare_request_hash = $4 \
             AND explicit_approval_digest = $5 \
             AND authorizing_session_id = $6 \
             AND authorizing_session_revision = $7 \
             AND authorizing_recovery_code_id = $8 \
             AND authorizing_recovery_code_revision = $9 \
             AND external_principal_key_version = $10 \
             AND external_principal_pseudonym = $11 AS exact, \
             EXISTS(SELECT 1 FROM account_recovery_codes AS recovery \
                 WHERE recovery.workspace_id = lifecycle.workspace_id \
                 AND recovery.user_id = lifecycle.user_id \
                 AND recovery.id = lifecycle.authorizing_recovery_code_id \
                 AND recovery.revision = lifecycle.authorizing_recovery_code_revision \
                 AND recovery.token_hash = $12 \
                 AND recovery.consumed_at IS NULL AND recovery.revoked_at IS NULL) \
                 AS recovery_exact, \
             status, revision \
         FROM account_deletion_lifecycles AS lifecycle \
         WHERE id = $1 AND workspace_id = $2 AND user_id = $3",
    )
    .bind(preparation.id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preparation.request_hash.as_slice())
    .bind(preparation.explicit_approval_digest.as_slice())
    .bind(preparation.authorizing_session_id)
    .bind(revision_to_i64(preparation.authorizing_session_revision)?)
    .bind(preparation.authorizing_recovery_code_id)
    .bind(revision_to_i64(
        preparation.authorizing_recovery_code_revision,
    )?)
    .bind(
        i32::try_from(principal.pseudonym().key_version())
            .map_err(|_| AccountDeletionRepositoryError::InvalidInput)?,
    )
    .bind(principal.pseudonym().digest().as_slice())
    .bind(recovery_digest.as_slice())
    .fetch_optional(executor)
    .await
    .map_err(internal)?;
    let Some(row) = row else {
        return Ok(None);
    };
    if !row.try_get::<bool, _>("exact").map_err(internal)? {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    if !row.try_get::<bool, _>("recovery_exact").map_err(internal)? {
        return Err(AccountDeletionRepositoryError::InvalidAuthority);
    }
    let status = status_from_row(&row)?;
    let revision = revision_from_i64(row.try_get("revision").map_err(internal)?)?;
    Ok(Some(AccountDeletionMutation {
        deletion_id: preparation.id,
        status,
        revision,
        replayed: true,
    }))
}

async fn lock_lifecycle(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    deletion_id: Uuid,
) -> Result<LockedLifecycle, AccountDeletionRepositoryError> {
    // Lifecycle keys are immutable. Exclude competing lifecycle writers while
    // allowing KEY SHARE acquired by provider-closure foreign keys: closing
    // holds the shared global barrier, which this transaction later upgrades.
    // FOR UPDATE here would invert that implicit FK/barrier lock ordering.
    let row = sqlx::query(
        "SELECT status, revision, prepared_at, owner_subject_hash, \
         external_tombstone_evidence_hash, external_principal_key_version, \
         external_principal_pseudonym, authorizing_session_id, \
         authorizing_session_revision, \
         authorizing_recovery_code_id, authorizing_recovery_code_revision, \
         authorizing_recovery_code_created_at \
         FROM account_deletion_lifecycles WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
         FOR NO KEY UPDATE",
    )
    .bind(deletion_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(AccountDeletionRepositoryError::Conflict)?;
    let subject_hash: Vec<u8> = row.try_get("owner_subject_hash").map_err(internal)?;
    if subject_hash.len() != 32 {
        return Err(AccountDeletionRepositoryError::Internal);
    }
    let tombstone_hash: Option<Vec<u8>> = row
        .try_get("external_tombstone_evidence_hash")
        .map_err(internal)?;
    let external_tombstone_evidence_hash = tombstone_hash
        .map(|hash| {
            hash.try_into()
                .map_err(|_| AccountDeletionRepositoryError::Internal)
        })
        .transpose()?;
    Ok(LockedLifecycle {
        status: status_from_row(&row)?,
        revision: revision_from_i64(row.try_get("revision").map_err(internal)?)?,
        prepared_at: row.try_get("prepared_at").map_err(internal)?,
        owner_subject_hash: subject_hash,
        external_tombstone_evidence_hash,
        external_principal_key_version: row
            .try_get("external_principal_key_version")
            .map_err(internal)?,
        external_principal_pseudonym: row
            .try_get("external_principal_pseudonym")
            .map_err(internal)?,
        authorizing_session_id: row.try_get("authorizing_session_id").map_err(internal)?,
        authorizing_session_revision: revision_from_i64(
            row.try_get("authorizing_session_revision")
                .map_err(internal)?,
        )?,
        authorizing_recovery_code_id: row
            .try_get("authorizing_recovery_code_id")
            .map_err(internal)?,
        authorizing_recovery_code_revision: revision_from_i64(
            row.try_get("authorizing_recovery_code_revision")
                .map_err(internal)?,
        )?,
        authorizing_recovery_code_created_at: row
            .try_get("authorizing_recovery_code_created_at")
            .map_err(internal)?,
    })
}

async fn lookup_transition(
    transaction: &mut Transaction<'_, Postgres>,
    transition: &AccountDeletionTransition,
) -> Result<Option<AccountDeletionMutation>, AccountDeletionRepositoryError> {
    let row = sqlx::query(
        "SELECT from_status, to_status, expected_revision, result_revision, failure_code \
         FROM account_deletion_transition_receipts \
         WHERE deletion_id = $1 AND request_hash = $2",
    )
    .bind(transition.deletion_id)
    .bind(transition.request_hash.as_slice())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let from: String = row.try_get("from_status").map_err(internal)?;
    let to: String = row.try_get("to_status").map_err(internal)?;
    let expected = revision_from_i64(row.try_get("expected_revision").map_err(internal)?)?;
    let result = revision_from_i64(row.try_get("result_revision").map_err(internal)?)?;
    let failure_code: Option<String> = row.try_get("failure_code").map_err(internal)?;
    if from != transition.from.as_storage_name()
        || to != transition.to.as_storage_name()
        || expected != transition.expected_revision
        || failure_code.as_deref() != transition.failure_code.as_deref()
    {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    Ok(Some(AccountDeletionMutation {
        deletion_id: transition.deletion_id,
        status: transition.to,
        revision: result,
        replayed: true,
    }))
}

async fn lookup_fence_confirmation(
    transaction: &mut Transaction<'_, Postgres>,
    transition: &AccountDeletionTransition,
    confirmation: &AccountDeletionFenceConfirmation,
) -> Result<Option<AccountDeletionMutation>, AccountDeletionRepositoryError> {
    let Some(replay) = lookup_transition(transaction, transition).await? else {
        return Ok(None);
    };
    let row = sqlx::query(
        "SELECT confirming_session_id, confirming_session_revision, \
         confirming_approval_digest FROM account_deletion_transition_receipts \
         WHERE deletion_id = $1 AND request_hash = $2",
    )
    .bind(transition.deletion_id)
    .bind(transition.request_hash.as_slice())
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    let session_id: Option<Uuid> = row.try_get("confirming_session_id").map_err(internal)?;
    let session_revision: Option<i64> = row
        .try_get("confirming_session_revision")
        .map_err(internal)?;
    let approval_digest: Option<Vec<u8>> = row
        .try_get("confirming_approval_digest")
        .map_err(internal)?;
    if session_id != Some(confirmation.confirming_session_id)
        || session_revision.map(revision_from_i64).transpose()?
            != Some(confirmation.confirming_session_revision)
        || approval_digest.as_deref() != Some(confirmation.explicit_approval_digest.as_slice())
    {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    Ok(Some(replay))
}

async fn insert_transition_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    transition: &AccountDeletionTransition,
    result_revision: u64,
    operation_at: DateTime<Utc>,
) -> Result<(), AccountDeletionRepositoryError> {
    sqlx::query(
        "INSERT INTO account_deletion_transition_receipts (deletion_id, request_hash, \
         from_status, to_status, expected_revision, result_revision, occurred_at, failure_code) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(transition.deletion_id)
    .bind(transition.request_hash.as_slice())
    .bind(transition.from.as_storage_name())
    .bind(transition.to.as_storage_name())
    .bind(revision_to_i64(transition.expected_revision)?)
    .bind(revision_to_i64(result_revision)?)
    .bind(operation_at)
    .bind(transition.failure_code.as_deref())
    .execute(&mut **transaction)
    .await
    .map_err(write_error)?;
    Ok(())
}

async fn insert_fence_confirmation_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    transition: &AccountDeletionTransition,
    confirmation: &AccountDeletionFenceConfirmation,
    result_revision: u64,
    operation_at: DateTime<Utc>,
) -> Result<(), AccountDeletionRepositoryError> {
    sqlx::query(
        "INSERT INTO account_deletion_transition_receipts (deletion_id, request_hash, \
         from_status, to_status, expected_revision, result_revision, occurred_at, failure_code, \
         confirming_session_id, confirming_session_revision, confirming_approval_digest) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, $8, $9, $10)",
    )
    .bind(transition.deletion_id)
    .bind(transition.request_hash.as_slice())
    .bind(transition.from.as_storage_name())
    .bind(transition.to.as_storage_name())
    .bind(revision_to_i64(transition.expected_revision)?)
    .bind(revision_to_i64(result_revision)?)
    .bind(operation_at)
    .bind(confirmation.confirming_session_id)
    .bind(revision_to_i64(confirmation.confirming_session_revision)?)
    .bind(confirmation.explicit_approval_digest.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(write_error)?;
    Ok(())
}

async fn fetch_subject_hash(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<Vec<u8>, AccountDeletionRepositoryError> {
    let digest = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT sha256(convert_to(auth_subject, 'UTF8')) FROM users WHERE id = $1",
    )
    .bind(scope.user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(AccountDeletionRepositoryError::UnsupportedScope)?;
    if digest.len() != 32 {
        return Err(AccountDeletionRepositoryError::Internal);
    }
    Ok(digest)
}

async fn database_now(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<DateTime<Utc>, AccountDeletionRepositoryError> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)
}

async fn lock_deletion_scope(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    subject_hash: &[u8],
) -> Result<(), AccountDeletionRepositoryError> {
    // The global exclusive barrier always comes first. Mutation triggers hold
    // its shared mode, so a fence cannot race any transaction even when that
    // transaction discovered several scoped identities across statements.
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.global-mutation-barrier.v1', 0))",
    )
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    // Keep the scoped locks as distinct statements: SQL does not promise an
    // evaluation order for expressions in one SELECT target list.
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.subject.v1:' || encode($1::bytea, 'hex'), 0))",
    )
    .bind(subject_hash)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.user.v1:' || $1::text, 0))",
    )
    .bind(scope.user_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.workspace.v1:' || $1::text, 0))",
    )
    .bind(scope.workspace_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn ensure_personal_scope(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), AccountDeletionRepositoryError> {
    let personal = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = $1 AND owner_user_id = $2) \
         AND EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2 \
             AND role = 'owner' AND removed_at IS NULL) \
         AND (SELECT count(*) FROM workspaces WHERE owner_user_id = $2) = 1 \
         AND (SELECT count(*) FROM workspace_members WHERE workspace_id = $1) = 1 \
         AND (SELECT count(*) FROM workspace_members WHERE user_id = $2) = 1",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if personal {
        Ok(())
    } else {
        Err(AccountDeletionRepositoryError::UnsupportedScope)
    }
}

async fn ensure_provider_cleanup_quiescent(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), AccountDeletionRepositoryError> {
    let quiescent = sqlx::query_scalar::<_, bool>(
        "SELECT NOT ( \
             EXISTS(SELECT 1 FROM google_oauth_sessions \
                 WHERE workspace_id = $1 AND user_id = $2 \
                 AND status IN ('pending', 'exchanging', 'staged')) \
          OR EXISTS(SELECT 1 FROM google_oauth_cleanup_tokens \
                 WHERE workspace_id = $1 AND user_id = $2) \
          OR EXISTS(SELECT 1 FROM google_oauth_legacy_credential_quarantine \
                 WHERE workspace_id = $1 AND user_id = $2 \
                 AND recovery_confirmed_at IS NULL) \
          OR EXISTS(SELECT 1 FROM google_oauth_scope_state \
                 WHERE workspace_id = $1 AND user_id = $2 \
                 AND revocation_kind IS NOT NULL) \
          OR EXISTS(SELECT 1 FROM provider_accounts \
                 WHERE workspace_id = $1 AND user_id = $2 AND status <> 'revoked' \
                 AND (id = '00000000-0000-0000-0000-000000000000'::uuid \
                     OR provider <> 'google' OR status IN ( \
                     'disconnecting', 'revocation_failed', 'operator_recovery_required'))) \
          OR (SELECT count(*) FROM provider_accounts \
                 WHERE workspace_id = $1 AND user_id = $2 AND status <> 'revoked') > $3 \
          OR EXISTS(SELECT 1 FROM provider_accounts AS account \
                 WHERE account.workspace_id = $1 AND account.user_id = $2 \
                 AND account.status <> 'revoked' \
                 AND NOT EXISTS(SELECT 1 FROM google_oauth_scope_state AS scope_state \
                     WHERE scope_state.workspace_id = account.workspace_id \
                     AND scope_state.user_id = account.user_id)) \
          OR EXISTS(SELECT 1 FROM google_sync_runs \
                 WHERE workspace_id = $1 AND user_id = $2 AND state = 'running') \
          OR EXISTS(SELECT 1 FROM google_sync_outbox \
                 WHERE workspace_id = $1 AND user_id = $2 AND state = 'delivering') \
          OR EXISTS(SELECT 1 FROM google_schedule_publication_outbox \
                 WHERE workspace_id = $1 AND user_id = $2 AND state = 'delivering') \
          OR EXISTS(SELECT 1 FROM google_schedule_publication_batches \
                 WHERE workspace_id = $1 AND user_id = $2 \
                 AND (state = 'delivering' OR delivering_count > 0)) \
        )",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(i64::try_from(ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_TARGETS).map_err(internal)?)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if quiescent {
        Ok(())
    } else {
        Err(AccountDeletionRepositoryError::ProviderCleanupBlocked)
    }
}

async fn ensure_no_fence(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    subject_hash: &[u8],
) -> Result<(), AccountDeletionRepositoryError> {
    let fenced = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM account_deletion_fences \
         WHERE workspace_id = $1 OR user_id = $2 OR owner_subject_hash = $3)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(subject_hash)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if fenced {
        Err(AccountDeletionRepositoryError::Conflict)
    } else {
        Ok(())
    }
}

async fn ensure_no_active_lifecycle(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), AccountDeletionRepositoryError> {
    let active = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM account_deletion_lifecycles \
         WHERE (workspace_id = $1 OR user_id = $2) AND status <> 'cancelled')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if active {
        Err(AccountDeletionRepositoryError::Conflict)
    } else {
        Ok(())
    }
}

async fn ensure_exact_fence(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    deletion_id: Uuid,
) -> Result<(), AccountDeletionRepositoryError> {
    let present = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM account_deletion_fences \
         WHERE deletion_id = $1 AND workspace_id = $2 AND user_id = $3)",
    )
    .bind(deletion_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if present {
        Ok(())
    } else {
        Err(AccountDeletionRepositoryError::Conflict)
    }
}

async fn validate_fresh_full_owner_session(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    session_id: Uuid,
    session_revision: u64,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, AccountDeletionRepositoryError> {
    let fresh_after = now
        .checked_sub_signed(FRESH_AUTHORITY_WINDOW)
        .ok_or(AccountDeletionRepositoryError::InvalidInput)?;
    let scopes = full_owner_device_scopes()
        .into_iter()
        .map(|scope| scope.as_storage_name().to_owned())
        .collect::<Vec<_>>();
    let row = sqlx::query(
        "SELECT credential_issued_at FROM sessions WHERE workspace_id = $1 AND user_id = $2 \
         AND id = $3 AND revision = $4 AND auth_version = 1 \
         AND client_contract_version = $5 AND client_kind IN ('macos', 'android') \
         AND scopes @> $6::text[] AND scopes <@ $6::text[] \
         AND cardinality(scopes) = cardinality($6::text[]) \
         AND revoked_at IS NULL AND created_at <= $7 AND credential_issued_at <= $7 \
         AND credential_issued_at >= $8 \
         AND expires_at > $7 AND refresh_idle_expires_at > $7 AND absolute_expires_at > $7 \
         FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(session_id)
    .bind(revision_to_i64(session_revision)?)
    .bind(i16::try_from(DEVICE_CLIENT_CONTRACT_VERSION).map_err(internal)?)
    .bind(scopes)
    .bind(now)
    .bind(fresh_after)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(AccountDeletionRepositoryError::InvalidAuthority)?;
    row.try_get("credential_issued_at").map_err(internal)
}

async fn validate_current_recovery_code(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    preparation: &AccountDeletionPreparation,
    recovery_digest: Option<&[u8; 32]>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, AccountDeletionRepositoryError> {
    validate_stored_recovery_code(
        transaction,
        scope,
        preparation.authorizing_recovery_code_id,
        preparation.authorizing_recovery_code_revision,
        None,
        now,
        recovery_digest,
    )
    .await
}

async fn validate_stored_current_recovery_code(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    recovery_code_id: Uuid,
    recovery_code_revision: u64,
    expected_created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), AccountDeletionRepositoryError> {
    let created_at = validate_stored_recovery_code(
        transaction,
        scope,
        recovery_code_id,
        recovery_code_revision,
        Some(expected_created_at),
        now,
        None,
    )
    .await?;
    if created_at == expected_created_at {
        Ok(())
    } else {
        Err(AccountDeletionRepositoryError::InvalidAuthority)
    }
}

#[allow(clippy::too_many_arguments)]
async fn validate_stored_recovery_code(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    recovery_code_id: Uuid,
    recovery_code_revision: u64,
    expected_created_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    recovery_digest: Option<&[u8; 32]>,
) -> Result<DateTime<Utc>, AccountDeletionRepositoryError> {
    let old_enough_at = now
        .checked_sub_signed(RECOVERY_CODE_MINIMUM_AGE)
        .ok_or(AccountDeletionRepositoryError::InvalidInput)?;
    let digest = recovery_digest.map(|digest| digest.to_vec());
    let row = sqlx::query(
        "SELECT created_at FROM account_recovery_codes \
         WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND revision = $4 \
         AND consumed_at IS NULL AND revoked_at IS NULL AND created_at <= $5 \
         AND ($6::bytea IS NULL OR token_hash = $6) \
         AND ($7::timestamptz IS NULL OR created_at = $7) FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(recovery_code_id)
    .bind(revision_to_i64(recovery_code_revision)?)
    .bind(old_enough_at)
    .bind(digest)
    .bind(expected_created_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(AccountDeletionRepositoryError::InvalidAuthority)?;
    row.try_get("created_at").map_err(internal)
}

fn validate_preparation(
    preparation: &AccountDeletionPreparation,
    scope: DatabaseScope,
    recovery_code: &OpaqueCredential<'_>,
) -> Result<(), AccountDeletionRepositoryError> {
    if preparation.id.is_nil()
        || preparation.authorizing_session_id.is_nil()
        || preparation.authorizing_session_revision == 0
        || preparation.authorizing_recovery_code_id.is_nil()
        || preparation.authorizing_recovery_code_revision == 0
        || preparation.request_hash.iter().all(|byte| *byte == 0)
        || recovery_code.kind() != CredentialKind::AccountRecovery
        || preparation.explicit_approval_digest
            != account_deletion_approval_digest(preparation.id, scope.workspace_id, scope.user_id)
    {
        return Err(AccountDeletionRepositoryError::InvalidInput);
    }
    Ok(())
}

fn validate_transition(
    transition: &AccountDeletionTransition,
) -> Result<(), AccountDeletionRepositoryError> {
    if transition.deletion_id.is_nil()
        || transition.expected_revision == 0
        || transition.request_hash.iter().all(|byte| *byte == 0)
        || transition.from == transition.to
        || transition.failure_code.as_ref().is_some_and(|code| {
            code.is_empty()
                || code.len() > 64
                || !code.bytes().enumerate().all(|(index, byte)| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || (index > 0 && byte == b'_')
                })
        })
    {
        return Err(AccountDeletionRepositoryError::InvalidInput);
    }
    Ok(())
}

fn validate_fence_confirmation(
    confirmation: &AccountDeletionFenceConfirmation,
    transition: &AccountDeletionTransition,
    scope: DatabaseScope,
) -> Result<(), AccountDeletionRepositoryError> {
    if transition.from != AccountDeletionStatus::Prepared
        || transition.to != AccountDeletionStatus::FenceCommitting
        || transition.failure_code.is_some()
        || confirmation.confirming_session_id.is_nil()
        || confirmation.confirming_session_revision == 0
        || confirmation.explicit_approval_digest
            != account_deletion_approval_digest(
                transition.deletion_id,
                scope.workspace_id,
                scope.user_id,
            )
    {
        return Err(AccountDeletionRepositoryError::InvalidInput);
    }
    Ok(())
}

fn valid_regular_transition(transition: &AccountDeletionTransition) -> bool {
    matches!(
        (transition.from, transition.to),
        (
            AccountDeletionStatus::FenceCommitting,
            AccountDeletionStatus::Fenced
        ) | (
            AccountDeletionStatus::Prepared,
            AccountDeletionStatus::Cancelled
        )
    ) && transition.failure_code.is_none()
}

fn lifecycle_from_row(
    row: &PgRow,
) -> Result<AccountDeletionLifecycle, AccountDeletionRepositoryError> {
    Ok(AccountDeletionLifecycle {
        id: row.try_get("id").map_err(internal)?,
        workspace_id: row.try_get("workspace_id").map_err(internal)?,
        user_id: row.try_get("user_id").map_err(internal)?,
        status: status_from_row(row)?,
        revision: revision_from_i64(row.try_get("revision").map_err(internal)?)?,
        prepared_at: row.try_get("prepared_at").map_err(internal)?,
        updated_at: row.try_get("updated_at").map_err(internal)?,
        local_purge_completed_at: row.try_get("local_purge_completed_at").map_err(internal)?,
    })
}

fn status_from_row(row: &PgRow) -> Result<AccountDeletionStatus, AccountDeletionRepositoryError> {
    let value: String = row.try_get("status").map_err(internal)?;
    AccountDeletionStatus::from_storage_name(&value).ok_or(AccountDeletionRepositoryError::Internal)
}

fn mutation_for_transition(
    transition: &AccountDeletionTransition,
    revision: u64,
    replayed: bool,
) -> AccountDeletionMutation {
    AccountDeletionMutation {
        deletion_id: transition.deletion_id,
        status: transition.to,
        revision,
        replayed,
    }
}

fn revision_to_i64(revision: u64) -> Result<i64, AccountDeletionRepositoryError> {
    i64::try_from(revision).map_err(|_| AccountDeletionRepositoryError::InvalidInput)
}

fn revision_from_i64(revision: i64) -> Result<u64, AccountDeletionRepositoryError> {
    revision
        .try_into()
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or(AccountDeletionRepositoryError::Internal)
}

fn generation_from_i64(generation: i64) -> Result<u64, AccountDeletionRepositoryError> {
    generation
        .try_into()
        .map_err(|_| AccountDeletionRepositoryError::Internal)
}

fn attempt_from_i32(attempt: i32) -> Result<u32, AccountDeletionRepositoryError> {
    u32::try_from(attempt)
        .ok()
        .filter(|attempt| *attempt <= ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_ATTEMPTS)
        .ok_or(AccountDeletionRepositoryError::Internal)
}

fn fixed_hash(value: &[u8]) -> Result<[u8; 32], AccountDeletionRepositoryError> {
    value
        .try_into()
        .map_err(|_| AccountDeletionRepositoryError::Internal)
}

fn safety_gate_error(error: AccountDeletionSafetyGateError) -> AccountDeletionRepositoryError {
    match error {
        AccountDeletionSafetyGateError::Disabled => AccountDeletionRepositoryError::Disabled,
        AccountDeletionSafetyGateError::Unavailable => AccountDeletionRepositoryError::Internal,
    }
}

fn write_error(error: sqlx::Error) -> AccountDeletionRepositoryError {
    let mapped = match error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .as_deref()
    {
        Some("23505" | "DWCON") => AccountDeletionRepositoryError::Conflict,
        Some("DWSCP") => AccountDeletionRepositoryError::UnsupportedScope,
        Some("DWREQ") => AccountDeletionRepositoryError::InvalidInput,
        _ => AccountDeletionRepositoryError::Internal,
    };
    drop(error);
    mapped
}

fn internal<T>(_error: T) -> AccountDeletionRepositoryError {
    AccountDeletionRepositoryError::Internal
}
