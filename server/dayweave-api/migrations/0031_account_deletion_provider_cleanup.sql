-- Durable, content-free provider-cleanup orchestration for the still-disabled
-- account-deletion workflow. Credential ciphertext remains only in the
-- fenced tenant tables: detached evidence stores a fixed SHA-256 commitment
-- and source coordinates, never tokens, labels, external identities, scopes,
-- payloads, or raw provider errors.

ALTER TABLE account_deletion_lifecycles
    ADD COLUMN provider_cleanup_policy_version smallint,
    ADD COLUMN provider_cleanup_target_count integer,
    ADD COLUMN provider_cleanup_manifest_hash bytea,
    ADD CONSTRAINT account_deletion_provider_cleanup_manifest_check CHECK (
        (provider_cleanup_policy_version IS NULL
            AND provider_cleanup_target_count IS NULL
            AND provider_cleanup_manifest_hash IS NULL)
        OR
        (provider_cleanup_policy_version IS NOT NULL
            AND provider_cleanup_target_count IS NOT NULL
            AND provider_cleanup_manifest_hash IS NOT NULL
            AND provider_cleanup_policy_version = 1
            AND provider_cleanup_target_count BETWEEN 0 AND 64
            AND octet_length(provider_cleanup_manifest_hash) = 32
            AND provider_cleanup_manifest_hash
                <> decode(repeat('00', 32), 'hex'))
    );

CREATE TABLE account_deletion_provider_cleanup_targets (
    deletion_id uuid NOT NULL REFERENCES account_deletion_lifecycles(id),
    provider_account_id uuid NOT NULL
        CHECK (provider_account_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    provider varchar(16) NOT NULL CHECK (provider = 'google'),
    provider_account_revision bigint NOT NULL CHECK (provider_account_revision > 0),
    credential_generation bigint NOT NULL CHECK (credential_generation >= 0),
    credential_key_version integer NOT NULL CHECK (credential_key_version > 0),
    encrypted_credentials_hash bytea NOT NULL
        CHECK (octet_length(encrypted_credentials_hash) = 32
            AND encrypted_credentials_hash <> decode(repeat('00', 32), 'hex')),
    status varchar(24) NOT NULL DEFAULT 'pending' CHECK (status IN (
        'pending', 'claimed', 'retry_wait', 'revoked', 'operator_required'
    )),
    attempt_count integer NOT NULL DEFAULT 0
        CHECK (attempt_count BETWEEN 0 AND 12),
    claim_id uuid
        CHECK (claim_id IS NULL OR claim_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    claimed_at timestamptz,
    lease_expires_at timestamptz,
    next_attempt_at timestamptz NOT NULL,
    deadline_at timestamptz NOT NULL,
    completed_at timestamptz,
    operator_required_at timestamptz,
    outcome_evidence_hash bytea,
    last_failure_code varchar(32) CHECK (last_failure_code IN (
        'provider_unavailable', 'provider_rejected', 'credential_unavailable',
        'credential_drift', 'claim_lease_expired', 'retry_exhausted',
        'deadline_exceeded'
    )),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (deletion_id, provider_account_id),
    CHECK (deadline_at = created_at + interval '24 hours'),
    CHECK (next_attempt_at BETWEEN created_at AND deadline_at),
    CHECK (updated_at >= created_at),
    CHECK (
        (status = 'claimed'
            AND claim_id IS NOT NULL
            AND claimed_at IS NOT NULL
            AND lease_expires_at IS NOT NULL)
        OR
        (status <> 'claimed'
            AND claim_id IS NULL
            AND claimed_at IS NULL
            AND lease_expires_at IS NULL)
    ),
    CHECK (lease_expires_at IS NULL OR lease_expires_at = claimed_at + interval '15 minutes'),
    CHECK ((outcome_evidence_hash IS NULL)
        OR (octet_length(outcome_evidence_hash) = 32
            AND outcome_evidence_hash <> decode(repeat('00', 32), 'hex'))),
    CHECK (
        (status = 'pending'
            AND attempt_count = 0
            AND completed_at IS NULL
            AND operator_required_at IS NULL
            AND outcome_evidence_hash IS NULL
            AND last_failure_code IS NULL)
        OR
        (status = 'claimed'
            AND attempt_count BETWEEN 1 AND 12
            AND completed_at IS NULL
            AND operator_required_at IS NULL
            AND outcome_evidence_hash IS NULL)
        OR
        (status = 'retry_wait'
            AND attempt_count BETWEEN 1 AND 11
            AND completed_at IS NULL
            AND operator_required_at IS NULL
            AND outcome_evidence_hash IS NULL
            AND last_failure_code IS NOT NULL)
        OR
        (status = 'revoked'
            AND attempt_count BETWEEN 1 AND 12
            AND completed_at IS NOT NULL
            AND operator_required_at IS NULL
            AND outcome_evidence_hash IS NOT NULL
            AND last_failure_code IS NULL)
        OR
        (status = 'operator_required'
            AND completed_at IS NULL
            AND operator_required_at IS NOT NULL
            AND outcome_evidence_hash IS NULL
            AND last_failure_code IS NOT NULL)
    )
);

CREATE UNIQUE INDEX account_deletion_provider_cleanup_targets_claim_uq
    ON account_deletion_provider_cleanup_targets (claim_id)
    WHERE claim_id IS NOT NULL;

CREATE INDEX account_deletion_provider_cleanup_targets_due_idx
    ON account_deletion_provider_cleanup_targets (
        deletion_id, next_attempt_at, provider_account_id
    ) WHERE status IN ('pending', 'retry_wait');

-- PostgreSQL independently reproduces the versioned Rust manifest encoding so
-- a direct writer cannot seal a complete-looking count with an arbitrary
-- digest. The NUL-terminated domain below is 55 bytes.
CREATE FUNCTION calculate_account_deletion_provider_cleanup_manifest(target_deletion_id uuid)
RETURNS bytea
LANGUAGE plpgsql STABLE AS $manifest$
DECLARE
    encoded bytea := int8send(55::bigint)
        || decode(
            '64617977656176652f6163636f756e742d64656c6574696f6e2d70726f76696465722d636c65616e75702d6d616e69666573742f763100',
            'hex'
        )
        || uuid_send(target_deletion_id);
    target_count bigint;
    target record;
BEGIN
    SELECT count(*) INTO target_count
      FROM account_deletion_provider_cleanup_targets
     WHERE deletion_id = target_deletion_id;
    encoded := encoded || int8send(target_count);
    FOR target IN
        SELECT provider_account_id, provider_account_revision,
               credential_generation, credential_key_version,
               encrypted_credentials_hash
          FROM account_deletion_provider_cleanup_targets
         WHERE deletion_id = target_deletion_id
         ORDER BY provider, provider_account_id, provider_account_revision,
                  credential_generation, credential_key_version,
                  encrypted_credentials_hash
    LOOP
        encoded := encoded
            || decode('01', 'hex')
            || uuid_send(target.provider_account_id)
            || int8send(target.provider_account_revision)
            || int8send(target.credential_generation)
            || int4send(target.credential_key_version)
            || target.encrypted_credentials_hash;
    END LOOP;
    RETURN sha256(encoded);
END
$manifest$;

CREATE TABLE account_deletion_provider_cleanup_attempts (
    deletion_id uuid NOT NULL,
    provider_account_id uuid NOT NULL
        CHECK (provider_account_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    attempt_number integer NOT NULL CHECK (attempt_number BETWEEN 1 AND 12),
    claim_id uuid NOT NULL UNIQUE
        CHECK (claim_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    claimed_at timestamptz NOT NULL,
    lease_expires_at timestamptz NOT NULL,
    finished_at timestamptz NOT NULL,
    outcome varchar(24) NOT NULL CHECK (outcome IN (
        'revoked', 'already_absent', 'retryable_failure', 'operator_required'
    )),
    evidence_hash bytea,
    failure_code varchar(32) CHECK (failure_code IN (
        'provider_unavailable', 'provider_rejected', 'credential_unavailable',
        'credential_drift', 'claim_lease_expired', 'retry_exhausted',
        'deadline_exceeded'
    )),
    PRIMARY KEY (deletion_id, provider_account_id, attempt_number),
    FOREIGN KEY (deletion_id, provider_account_id)
        REFERENCES account_deletion_provider_cleanup_targets(
            deletion_id, provider_account_id
        ),
    CHECK (lease_expires_at = claimed_at + interval '15 minutes'),
    CHECK (finished_at >= claimed_at),
    CHECK (evidence_hash IS NULL OR (
        octet_length(evidence_hash) = 32
        AND evidence_hash <> decode(repeat('00', 32), 'hex')
    )),
    CHECK (
        (outcome IN ('revoked', 'already_absent')
            AND evidence_hash IS NOT NULL
            AND failure_code IS NULL)
        OR
        (outcome = 'retryable_failure'
            AND evidence_hash IS NULL
            AND failure_code IS NOT NULL
            AND failure_code IN ('provider_unavailable', 'claim_lease_expired'))
        OR
        (outcome = 'operator_required'
            AND evidence_hash IS NULL
            AND failure_code IS NOT NULL
            AND failure_code IN (
                'provider_rejected', 'credential_unavailable', 'credential_drift'
            ))
    )
);

CREATE FUNCTION guard_account_deletion_provider_cleanup_attempt() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    target account_deletion_provider_cleanup_targets%ROWTYPE;
    observed_at timestamptz;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup attempts are immutable';
    END IF;
    -- Claim IDs move from targets to immutable receipts. Separate unique
    -- indexes cannot prevent reuse across those two tables during a concurrent
    -- transition. Serialize detached writes, then use this VOLATILE trigger's
    -- fresh READ COMMITTED snapshots for every cross-table check. A fixed
    -- transaction snapshot could miss an already committed receipt. Try-lock
    -- rather than wait: repositories may already hold lifecycle/target rows,
    -- so waiting here could invert the direct writer's row-lock order.
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWREQ',
            MESSAGE = 'account deletion provider cleanup requires read committed';
    END IF;
    IF NOT pg_try_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.provider-cleanup-claims.v1', 0
    )) THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup write is contended';
    END IF;
    SELECT * INTO target
      FROM account_deletion_provider_cleanup_targets
     WHERE deletion_id = NEW.deletion_id
       AND provider_account_id = NEW.provider_account_id
     FOR UPDATE;
    IF NOT FOUND
       OR target.status <> 'claimed'
       OR target.claim_id IS DISTINCT FROM NEW.claim_id
       OR target.attempt_count IS DISTINCT FROM NEW.attempt_number
       OR target.claimed_at IS DISTINCT FROM NEW.claimed_at
       OR target.lease_expires_at IS DISTINCT FROM NEW.lease_expires_at
    THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup attempt is stale';
    END IF;
    observed_at := clock_timestamp();
    -- The repository samples clock_timestamp() before writing the receipt.
    -- Permit that sampled value only within this transaction, and independently
    -- check live lease eligibility against the database clock so backdating a
    -- supplied timestamp cannot revive an expired claim.
    IF NEW.finished_at < transaction_timestamp()
       OR NEW.finished_at > observed_at
       OR (NEW.failure_code = 'claim_lease_expired' AND (
            target.lease_expires_at > observed_at
            OR NEW.finished_at < target.lease_expires_at
        ))
       OR ((NEW.outcome IN ('revoked', 'already_absent')
                OR NEW.failure_code = 'provider_unavailable')
            AND (target.lease_expires_at <= observed_at
                OR NEW.finished_at >= target.lease_expires_at))
    THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'invalid account deletion provider cleanup attempt time';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER account_deletion_provider_cleanup_attempt_guard
    BEFORE INSERT OR UPDATE OR DELETE ON account_deletion_provider_cleanup_attempts
    FOR EACH ROW EXECUTE FUNCTION guard_account_deletion_provider_cleanup_attempt();

CREATE FUNCTION require_account_deletion_provider_cleanup_attempt_consumed() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    target account_deletion_provider_cleanup_targets%ROWTYPE;
    retry_at timestamptz;
BEGIN
    SELECT * INTO target
      FROM account_deletion_provider_cleanup_targets
     WHERE deletion_id = NEW.deletion_id
       AND provider_account_id = NEW.provider_account_id;
    retry_at := NEW.finished_at
        + LEAST((1::bigint << (NEW.attempt_number - 1)), 3600) * interval '1 second';

    IF NOT FOUND OR (
        (NEW.outcome IN ('revoked', 'already_absent')
            AND target.status = 'revoked'
            AND target.attempt_count = NEW.attempt_number
            AND target.claim_id IS NULL
            AND target.completed_at = NEW.finished_at
            AND target.updated_at = NEW.finished_at
            AND target.outcome_evidence_hash = NEW.evidence_hash)
        OR
        (NEW.outcome = 'retryable_failure'
            AND NEW.failure_code = 'provider_unavailable'
            AND target.attempt_count = NEW.attempt_number
            AND target.claim_id IS NULL
            AND target.updated_at = NEW.finished_at
            AND (
                (target.status = 'retry_wait'
                    AND target.last_failure_code = 'provider_unavailable'
                    AND target.next_attempt_at = retry_at
                    AND retry_at < target.deadline_at)
                OR
                (target.status = 'operator_required'
                    AND target.operator_required_at = NEW.finished_at
                    AND (
                        (target.last_failure_code = 'retry_exhausted'
                            AND NEW.attempt_number = 12)
                        OR
                        (target.last_failure_code = 'deadline_exceeded'
                            AND retry_at >= target.deadline_at)
                    ))
            ))
        OR
        (NEW.outcome = 'retryable_failure'
            AND NEW.failure_code = 'claim_lease_expired'
            AND (
                (target.status = 'claimed'
                    AND target.attempt_count = NEW.attempt_number + 1
                    AND target.claim_id IS NOT NULL
                    AND target.claim_id <> NEW.claim_id
                    AND target.claimed_at = NEW.finished_at
                    AND target.updated_at = NEW.finished_at
                    AND target.lease_expires_at
                        = NEW.finished_at + interval '15 minutes')
                OR
                (target.status = 'operator_required'
                    AND target.attempt_count = NEW.attempt_number
                    AND target.claim_id IS NULL
                    AND target.operator_required_at = NEW.finished_at
                    AND target.updated_at = NEW.finished_at
                    AND (
                        (target.last_failure_code = 'retry_exhausted'
                            AND NEW.attempt_number = 12)
                        OR
                        (target.last_failure_code = 'deadline_exceeded'
                            AND NEW.finished_at >= target.deadline_at)
                    ))
            ))
        OR
        (NEW.outcome = 'operator_required'
            AND target.status = 'operator_required'
            AND target.attempt_count = NEW.attempt_number
            AND target.claim_id IS NULL
            AND target.operator_required_at = NEW.finished_at
            AND target.updated_at = NEW.finished_at
            AND target.last_failure_code = NEW.failure_code)
    ) IS NOT TRUE THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup attempt was not consumed';
    END IF;
    RETURN NULL;
END
$guard$;

CREATE CONSTRAINT TRIGGER account_deletion_provider_cleanup_attempt_consumed
    AFTER INSERT ON account_deletion_provider_cleanup_attempts
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION require_account_deletion_provider_cleanup_attempt_consumed();

CREATE FUNCTION guard_account_deletion_provider_cleanup_target() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    parent_status varchar(32);
    completion account_deletion_provider_cleanup_attempts%ROWTYPE;
    observed_at timestamptz;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup targets are immutable';
    END IF;
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWREQ',
            MESSAGE = 'account deletion provider cleanup requires read committed';
    END IF;
    IF NOT pg_try_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.provider-cleanup-claims.v1', 0
    )) THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup write is contended';
    END IF;
    IF TG_OP = 'INSERT' THEN
        -- Serialize additions with manifest sealing. BEFORE INSERT has not
        -- installed a target row yet, so this matches the repository's
        -- lifecycle-before-target order.
        SELECT status INTO parent_status
          FROM account_deletion_lifecycles
         WHERE id = NEW.deletion_id
         FOR UPDATE;
    ELSE
        -- UPDATE already holds its target row. Locking the lifecycle here
        -- would invert the repository's lifecycle-before-target order. This
        -- migration blocks every exit from provider_cleanup, so reading its
        -- status is sufficient until the runtime permit protocol is added.
        SELECT status INTO parent_status
          FROM account_deletion_lifecycles
         WHERE id = NEW.deletion_id;
    END IF;
    IF NOT FOUND THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup lifecycle is missing';
    END IF;
    observed_at := clock_timestamp();
    IF NEW.updated_at < transaction_timestamp()
       OR NEW.updated_at > observed_at
    THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'invalid account deletion provider cleanup operation time';
    END IF;

    IF TG_OP = 'INSERT' THEN
        IF parent_status <> 'fenced'
           OR NEW.status <> 'pending'
           OR NEW.attempt_count <> 0
           OR NEW.next_attempt_at <> NEW.created_at
           OR NEW.updated_at <> NEW.created_at
           OR NOT EXISTS (
                SELECT 1 FROM provider_accounts AS account
                 WHERE account.workspace_id = (
                        SELECT workspace_id FROM account_deletion_lifecycles
                         WHERE id = NEW.deletion_id
                    )
                   AND account.user_id = (
                        SELECT user_id FROM account_deletion_lifecycles
                         WHERE id = NEW.deletion_id
                    )
                   AND account.id = NEW.provider_account_id
                   AND account.provider = NEW.provider
                   AND account.provider = 'google'
                   AND account.status IN ('active', 'paused', 'reauthorization_required')
                   AND account.revision = NEW.provider_account_revision
                   AND account.credential_key_version = NEW.credential_key_version
                   AND sha256(account.encrypted_credentials)
                        = NEW.encrypted_credentials_hash
                   AND EXISTS (
                        SELECT 1 FROM google_oauth_scope_state AS scope_state
                         WHERE scope_state.workspace_id = account.workspace_id
                           AND scope_state.user_id = account.user_id
                           AND scope_state.credential_generation
                                = NEW.credential_generation
                           AND scope_state.revocation_kind IS NULL
                    )
            )
        THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWREQ',
                MESSAGE = 'invalid account deletion provider cleanup target';
        END IF;
        RETURN NEW;
    END IF;

    IF parent_status <> 'provider_cleanup'
       OR OLD.deletion_id IS DISTINCT FROM NEW.deletion_id
       OR OLD.provider_account_id IS DISTINCT FROM NEW.provider_account_id
       OR OLD.provider IS DISTINCT FROM NEW.provider
       OR OLD.provider_account_revision IS DISTINCT FROM NEW.provider_account_revision
       OR OLD.credential_generation IS DISTINCT FROM NEW.credential_generation
       OR OLD.credential_key_version IS DISTINCT FROM NEW.credential_key_version
       OR OLD.encrypted_credentials_hash IS DISTINCT FROM NEW.encrypted_credentials_hash
       OR OLD.deadline_at IS DISTINCT FROM NEW.deadline_at
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
       OR NEW.updated_at < OLD.updated_at
    THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup target binding is immutable';
    END IF;

    IF OLD.status IN ('pending', 'retry_wait') AND NEW.status = 'claimed' THEN
        IF NEW.attempt_count <> OLD.attempt_count + 1
           OR NEW.claim_id IS NULL
           OR NEW.claimed_at <> NEW.updated_at
           OR NEW.lease_expires_at <> NEW.updated_at + interval '15 minutes'
           OR NEW.next_attempt_at IS DISTINCT FROM OLD.next_attempt_at
           OR NEW.last_failure_code IS DISTINCT FROM OLD.last_failure_code
           OR NEW.updated_at < OLD.next_attempt_at
           OR NEW.updated_at >= OLD.deadline_at
           OR observed_at < OLD.next_attempt_at
           OR observed_at >= OLD.deadline_at
           OR EXISTS (
                SELECT 1 FROM account_deletion_provider_cleanup_attempts
                 WHERE claim_id = NEW.claim_id
            )
        THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWCON',
                MESSAGE = 'invalid account deletion provider cleanup claim';
        END IF;
        RETURN NEW;
    END IF;

    SELECT * INTO completion
      FROM account_deletion_provider_cleanup_attempts
     WHERE deletion_id = OLD.deletion_id
       AND provider_account_id = OLD.provider_account_id
       AND attempt_number = OLD.attempt_count
       AND claim_id = OLD.claim_id;

    IF OLD.status = 'claimed' AND NEW.status = 'claimed' THEN
        IF NOT FOUND
           OR completion.outcome <> 'retryable_failure'
           OR completion.failure_code <> 'claim_lease_expired'
           OR OLD.lease_expires_at > NEW.updated_at
           OR OLD.lease_expires_at > observed_at
           OR NEW.attempt_count <> OLD.attempt_count + 1
           OR NEW.claim_id IS NULL
           OR NEW.claim_id = OLD.claim_id
           OR NEW.claimed_at <> NEW.updated_at
           OR NEW.lease_expires_at <> NEW.updated_at + interval '15 minutes'
           OR NEW.next_attempt_at IS DISTINCT FROM OLD.next_attempt_at
           OR NEW.last_failure_code IS DISTINCT FROM OLD.last_failure_code
           OR NEW.updated_at >= OLD.deadline_at
           OR observed_at >= OLD.deadline_at
           OR EXISTS (
                SELECT 1 FROM account_deletion_provider_cleanup_attempts
                 WHERE claim_id = NEW.claim_id
            )
        THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWCON',
                MESSAGE = 'invalid account deletion provider cleanup claim takeover';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.status = 'claimed' AND NEW.status IN ('retry_wait', 'revoked', 'operator_required') THEN
        IF NOT FOUND
           OR NEW.attempt_count <> OLD.attempt_count
           OR NEW.claim_id IS NOT NULL
           OR NEW.claimed_at IS NOT NULL
           OR NEW.lease_expires_at IS NOT NULL
           OR completion.finished_at <> NEW.updated_at
        THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWCON',
                MESSAGE = 'invalid account deletion provider cleanup resolution';
        END IF;
        IF NEW.status = 'retry_wait' AND (
            completion.outcome <> 'retryable_failure'
            OR NEW.last_failure_code <> completion.failure_code
            OR completion.failure_code <> 'provider_unavailable'
            OR completion.finished_at >= OLD.lease_expires_at
            OR observed_at >= OLD.lease_expires_at
            OR NEW.next_attempt_at <> NEW.updated_at
                + LEAST((1::bigint << (completion.attempt_number - 1)), 3600)
                    * interval '1 second'
            OR NEW.next_attempt_at >= OLD.deadline_at
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWCON',
                MESSAGE = 'invalid account deletion provider cleanup retry';
        ELSIF NEW.status = 'revoked' AND (
            completion.outcome NOT IN ('revoked', 'already_absent')
            OR completion.finished_at >= OLD.lease_expires_at
            OR observed_at >= OLD.lease_expires_at
            OR NEW.completed_at <> NEW.updated_at
            OR NEW.outcome_evidence_hash <> completion.evidence_hash
            OR NEW.next_attempt_at IS DISTINCT FROM OLD.next_attempt_at
            OR NEW.last_failure_code IS NOT NULL
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWCON',
                MESSAGE = 'invalid account deletion provider cleanup success';
        ELSIF NEW.status = 'operator_required' AND (
            NEW.operator_required_at <> NEW.updated_at
            OR NEW.next_attempt_at IS DISTINCT FROM OLD.next_attempt_at
            OR NEW.last_failure_code IS NULL
            OR NOT (
                (completion.outcome = 'operator_required'
                    AND NEW.last_failure_code = completion.failure_code)
                OR
                (completion.outcome = 'retryable_failure'
                    AND completion.failure_code IN (
                        'provider_unavailable', 'claim_lease_expired'
                    )
                    AND (
                        (NEW.last_failure_code = 'retry_exhausted'
                            AND OLD.attempt_count = 12)
                        OR
                        (NEW.last_failure_code = 'deadline_exceeded'
                            AND (
                                (completion.failure_code = 'provider_unavailable'
                                    AND NEW.updated_at
                                        + LEAST((1::bigint << (
                                            completion.attempt_number - 1
                                        )), 3600) * interval '1 second'
                                        >= OLD.deadline_at)
                                OR
                                (completion.failure_code = 'claim_lease_expired'
                                    AND NEW.updated_at >= OLD.deadline_at)
                            ))
                    ))
            )
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWCON',
                MESSAGE = 'invalid account deletion provider cleanup intervention';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.status IN ('pending', 'retry_wait') AND NEW.status = 'operator_required' THEN
        IF NEW.attempt_count <> OLD.attempt_count
           OR NEW.claim_id IS NOT NULL
           OR NEW.operator_required_at <> NEW.updated_at
           OR NEW.next_attempt_at IS DISTINCT FROM OLD.next_attempt_at
           OR NEW.last_failure_code NOT IN (
                'credential_unavailable', 'credential_drift',
                'retry_exhausted', 'deadline_exceeded'
            )
           OR (NEW.last_failure_code = 'deadline_exceeded'
                AND (NEW.updated_at < OLD.deadline_at
                    OR observed_at < OLD.deadline_at))
           OR (NEW.last_failure_code = 'retry_exhausted'
                AND OLD.attempt_count < 12)
        THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWCON',
                MESSAGE = 'invalid account deletion provider cleanup intervention';
        END IF;
        RETURN NEW;
    END IF;

    RAISE EXCEPTION USING
        ERRCODE = 'DWCON',
        MESSAGE = 'invalid account deletion provider cleanup target transition';
END
$guard$;

CREATE TRIGGER account_deletion_provider_cleanup_target_guard
    BEFORE INSERT OR UPDATE OR DELETE ON account_deletion_provider_cleanup_targets
    FOR EACH ROW EXECUTE FUNCTION guard_account_deletion_provider_cleanup_target();

CREATE FUNCTION require_account_deletion_provider_cleanup_seal() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM account_deletion_lifecycles
         WHERE id = NEW.deletion_id
           AND status = 'provider_cleanup'
           AND provider_cleanup_policy_version = 1
           AND provider_cleanup_manifest_hash IS NOT NULL
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup target was not atomically sealed';
    END IF;
    RETURN NULL;
END
$guard$;

CREATE CONSTRAINT TRIGGER account_deletion_provider_cleanup_target_sealed
    AFTER INSERT ON account_deletion_provider_cleanup_targets
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION require_account_deletion_provider_cleanup_seal();

CREATE FUNCTION guard_account_deletion_provider_cleanup_lifecycle() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    actual_count integer;
    source_count integer;
    unresolved_count integer;
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.provider_cleanup_policy_version IS NOT NULL
           OR NEW.provider_cleanup_target_count IS NOT NULL
           OR NEW.provider_cleanup_manifest_hash IS NOT NULL
        THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWREQ',
                MESSAGE = 'account deletion provider cleanup manifest must start empty';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.status = 'fenced' AND NEW.status = 'provider_cleanup' THEN
        SELECT count(*)::integer INTO actual_count
          FROM account_deletion_provider_cleanup_targets
         WHERE deletion_id = NEW.id;
        SELECT count(*)::integer INTO source_count
          FROM provider_accounts
         WHERE workspace_id = NEW.workspace_id
           AND user_id = NEW.user_id
           AND status <> 'revoked';
        IF OLD.provider_cleanup_policy_version IS NOT NULL
           OR OLD.provider_cleanup_target_count IS NOT NULL
           OR OLD.provider_cleanup_manifest_hash IS NOT NULL
           OR NEW.provider_cleanup_policy_version IS DISTINCT FROM 1
           OR NEW.provider_cleanup_target_count IS DISTINCT FROM actual_count
           OR actual_count IS DISTINCT FROM source_count
           OR NEW.provider_cleanup_manifest_hash IS NULL
           OR NEW.provider_cleanup_manifest_hash IS DISTINCT FROM
                calculate_account_deletion_provider_cleanup_manifest(NEW.id)
           OR NEW.external_principal_key_version IS NULL
           OR NEW.external_principal_pseudonym IS NULL
           OR NOT EXISTS (
                SELECT 1 FROM account_deletion_fences AS fence
                 WHERE fence.deletion_id = NEW.id
                   AND fence.workspace_id = NEW.workspace_id
                   AND fence.user_id = NEW.user_id
                   AND fence.owner_subject_hash = NEW.owner_subject_hash
            )
           OR EXISTS (
                SELECT 1 FROM google_oauth_sessions
                 WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
                   AND status IN ('pending', 'exchanging', 'staged')
            )
           OR EXISTS (
                SELECT 1 FROM google_oauth_cleanup_tokens
                 WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
            )
           OR EXISTS (
                SELECT 1 FROM google_oauth_legacy_credential_quarantine
                 WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
                   AND recovery_confirmed_at IS NULL
            )
           OR EXISTS (
                SELECT 1 FROM google_oauth_scope_state
                 WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
                   AND revocation_kind IS NOT NULL
            )
           OR EXISTS (
                SELECT 1 FROM provider_accounts
                 WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
                   AND status <> 'revoked'
                   AND (provider <> 'google' OR status IN (
                       'disconnecting', 'revocation_failed',
                       'operator_recovery_required'
                   ))
            )
           OR EXISTS (
                SELECT 1 FROM google_sync_runs
                 WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
                   AND state = 'running'
            )
           OR EXISTS (
                SELECT 1 FROM google_sync_outbox
                 WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
                   AND state = 'delivering'
            )
           OR EXISTS (
                SELECT 1 FROM google_schedule_publication_outbox
                 WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
                   AND state = 'delivering'
            )
           OR EXISTS (
                SELECT 1 FROM google_schedule_publication_batches
                 WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
                   AND (state = 'delivering' OR delivering_count > 0)
            )
           OR EXISTS (
                SELECT 1 FROM account_deletion_provider_cleanup_targets
                 WHERE deletion_id = NEW.id
                   AND (status <> 'pending'
                        OR attempt_count <> 0
                        OR created_at <> NEW.updated_at)
            )
        THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWCON',
                MESSAGE = 'account deletion provider cleanup manifest is incomplete';
        END IF;
    ELSIF OLD.provider_cleanup_policy_version
            IS DISTINCT FROM NEW.provider_cleanup_policy_version
       OR OLD.provider_cleanup_target_count
            IS DISTINCT FROM NEW.provider_cleanup_target_count
       OR OLD.provider_cleanup_manifest_hash
            IS DISTINCT FROM NEW.provider_cleanup_manifest_hash
    THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion provider cleanup manifest is immutable';
    END IF;

    IF OLD.status = 'provider_cleanup' AND NEW.status = 'purge' THEN
        SELECT count(*)::integer,
               count(*) FILTER (WHERE status <> 'revoked')::integer
          INTO actual_count, unresolved_count
          FROM account_deletion_provider_cleanup_targets
         WHERE deletion_id = NEW.id;
        IF OLD.provider_cleanup_policy_version IS DISTINCT FROM 1
           OR OLD.provider_cleanup_target_count IS DISTINCT FROM actual_count
           OR OLD.provider_cleanup_manifest_hash IS NULL
           OR unresolved_count <> 0
        THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWCON',
                MESSAGE = 'account deletion provider cleanup is incomplete';
        END IF;
        RAISE EXCEPTION USING
            ERRCODE = 'DWREQ',
            MESSAGE = 'runtime-held external restore permit is unavailable';
    ELSIF OLD.status = 'purge' AND NEW.status = 'backup_wait' THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWREQ',
            MESSAGE = 'runtime-held external restore permit is unavailable';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER account_deletion_provider_cleanup_lifecycle_guard
    BEFORE INSERT OR UPDATE ON account_deletion_lifecycles
    FOR EACH ROW EXECUTE FUNCTION guard_account_deletion_provider_cleanup_lifecycle();

DO $pin_account_deletion_provider_cleanup_guards$
DECLARE
    trusted_schema name := current_schema();
    function_name name;
BEGIN
    FOREACH function_name IN ARRAY ARRAY[
        'guard_account_deletion_provider_cleanup_attempt',
        'require_account_deletion_provider_cleanup_attempt_consumed',
        'guard_account_deletion_provider_cleanup_target',
        'require_account_deletion_provider_cleanup_seal',
        'guard_account_deletion_provider_cleanup_lifecycle'
    ]::name[]
    LOOP
        EXECUTE format(
            'ALTER FUNCTION %I.%I() SET search_path TO %I, pg_catalog, pg_temp',
            trusted_schema,
            function_name,
            trusted_schema
        );
        EXECUTE format(
            'REVOKE ALL ON FUNCTION %I.%I() FROM PUBLIC',
            trusted_schema,
            function_name
        );
    END LOOP;
    EXECUTE format(
        'ALTER FUNCTION %I.calculate_account_deletion_provider_cleanup_manifest(uuid) '
        'SET search_path TO %I, pg_catalog, pg_temp',
        trusted_schema,
        trusted_schema
    );
    EXECUTE format(
        'REVOKE ALL ON FUNCTION %I.calculate_account_deletion_provider_cleanup_manifest(uuid) '
        'FROM PUBLIC',
        trusted_schema
    );
END
$pin_account_deletion_provider_cleanup_guards$;
