use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{
    account_deletion::{
        AccountDeletionFenceConfirmation, AccountDeletionLifecycle, AccountDeletionMutation,
        AccountDeletionPreparation, AccountDeletionPrincipalBinding, AccountDeletionRepository,
        AccountDeletionRepositoryError, AccountDeletionSafetyGate, AccountDeletionSafetyGateError,
        AccountDeletionStatus, AccountDeletionTransition, DisabledAccountDeletionSafetyGate,
        account_deletion_approval_digest,
    },
    credential_auth::{
        CredentialKind, DEVICE_CLIENT_CONTRACT_VERSION, OpaqueCredential, full_owner_device_scopes,
    },
};

use super::DatabaseScope;

const FRESH_AUTHORITY_WINDOW: Duration = Duration::minutes(5);
const RECOVERY_CODE_MINIMUM_AGE: Duration = Duration::days(1);
const DELETION_COOLING_OFF_PERIOD: Duration = Duration::days(1);

#[derive(Clone)]
pub struct PostgresAccountDeletionRepository {
    pool: PgPool,
    scope: DatabaseScope,
    safety_gate: Arc<dyn AccountDeletionSafetyGate>,
    external_principal: Option<AccountDeletionPrincipalBinding>,
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
}

#[async_trait]
#[allow(clippy::too_many_lines)]
impl AccountDeletionRepository for PostgresAccountDeletionRepository {
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
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError> {
        let principal = self
            .external_principal
            .ok_or(AccountDeletionRepositoryError::Disabled)?;
        let transition = confirmation.transition.clone();
        validate_transition(&transition)?;
        validate_fence_confirmation(&confirmation, &transition, self.scope)?;
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

    async fn advance(
        &self,
        transition: AccountDeletionTransition,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError> {
        validate_transition(&transition)?;
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
    let row = sqlx::query(
        "SELECT status, revision, prepared_at, owner_subject_hash, \
         external_tombstone_evidence_hash, external_principal_key_version, \
         external_principal_pseudonym, authorizing_session_id, \
         authorizing_session_revision, \
         authorizing_recovery_code_id, authorizing_recovery_code_revision, \
         authorizing_recovery_code_created_at \
         FROM account_deletion_lifecycles WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
         FOR UPDATE",
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
            AccountDeletionStatus::Fenced,
            AccountDeletionStatus::ProviderCleanup
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
