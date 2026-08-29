-- Durable authentication runtime hardening.
--
-- The previous migration deliberately installed only persistence primitives.
-- This migration adds exact retry state for refresh rotation, explicit client
-- contract metadata, and the complete audience-specific scope vocabulary used
-- by the runtime cutover.

ALTER TABLE sessions
    ADD COLUMN previous_refresh_token_hash bytea,
    ADD COLUMN client_contract_version smallint NOT NULL DEFAULT 1,
    ADD COLUMN client_version varchar(100) NOT NULL DEFAULT 'unknown',
    ADD COLUMN client_capabilities text[] NOT NULL DEFAULT '{}',
    ADD CONSTRAINT sessions_previous_refresh_token_hash_check CHECK (
        previous_refresh_token_hash IS NULL OR octet_length(previous_refresh_token_hash) = 32
    ),
    ADD CONSTRAINT sessions_client_contract_version_check CHECK (client_contract_version = 1),
    ADD CONSTRAINT sessions_client_version_check CHECK (btrim(client_version) <> ''),
    ADD CONSTRAINT sessions_client_capabilities_check CHECK (
        cardinality(client_capabilities) <= 100
        AND array_position(client_capabilities, NULL) IS NULL
    );

CREATE INDEX sessions_v1_previous_refresh_lookup_idx
    ON sessions (workspace_id, user_id, previous_refresh_token_hash)
    WHERE auth_version = 1 AND previous_refresh_token_hash IS NOT NULL AND revoked_at IS NULL;

ALTER TABLE device_enrollments
    ADD COLUMN client_contract_version smallint NOT NULL DEFAULT 1,
    ADD COLUMN client_version varchar(100) NOT NULL DEFAULT 'unknown',
    ADD COLUMN client_capabilities text[] NOT NULL DEFAULT '{}',
    ADD CONSTRAINT device_enrollments_client_contract_version_check
        CHECK (client_contract_version = 1),
    ADD CONSTRAINT device_enrollments_client_version_check CHECK (btrim(client_version) <> ''),
    ADD CONSTRAINT device_enrollments_client_capabilities_check CHECK (
        cardinality(client_capabilities) <= 100
        AND array_position(client_capabilities, NULL) IS NULL
    );

ALTER TABLE mcp_clients
    ADD COLUMN client_contract_version smallint NOT NULL DEFAULT 1,
    ADD COLUMN client_version varchar(100) NOT NULL DEFAULT 'unknown',
    ADD COLUMN client_capabilities text[] NOT NULL DEFAULT '{}',
    ADD CONSTRAINT mcp_clients_client_contract_version_check CHECK (client_contract_version = 1),
    ADD CONSTRAINT mcp_clients_client_version_check CHECK (btrim(client_version) <> ''),
    ADD CONSTRAINT mcp_clients_client_capabilities_check CHECK (
        cardinality(client_capabilities) <= 100
        AND array_position(client_capabilities, NULL) IS NULL
    );

-- Constraint names for inline CHECK expressions are PostgreSQL-generated.
-- Resolve the old hard-coded scope constraints by their definition instead of
-- depending on a generated name that may differ between PostgreSQL releases.
DO $migration$
DECLARE
    target record;
BEGIN
    FOR target IN
        SELECT conrelid::regclass AS relation_name, conname
        FROM pg_constraint
        WHERE contype = 'c'
          AND conrelid IN (
              'sessions'::regclass,
              'device_enrollments'::regclass,
              'mcp_clients'::regclass
          )
          AND pg_get_constraintdef(oid) LIKE '%suggestions_submit%'
    LOOP
        EXECUTE format(
            'ALTER TABLE %s DROP CONSTRAINT %I',
            target.relation_name,
            target.conname
        );
    END LOOP;
END
$migration$;

ALTER TABLE sessions
    ADD CONSTRAINT sessions_v1_runtime_shape_check CHECK (
        auth_version <> 1 OR revoked_at IS NOT NULL OR (
            client_instance_id IS NOT NULL
            AND client_kind IN ('macos', 'android')
            AND device_label IS NOT NULL
            AND btrim(device_label) <> ''
            AND octet_length(token_hash) = 32
            AND refresh_token_hash IS NOT NULL
            AND octet_length(refresh_token_hash) = 32
            AND cardinality(scopes) > 0
            AND scopes <@ ARRAY[
                'suggestions_read', 'suggestions_write',
                'schedule_read', 'schedule_simulate',
                'items_read', 'items_write',
                'execution_read', 'execution_write',
                'google_read', 'google_write',
                'auth_sessions_read', 'auth_sessions_write',
                'auth_mcp_clients_read', 'auth_mcp_clients_write'
            ]::text[]
            AND refresh_idle_expires_at IS NOT NULL
            AND absolute_expires_at IS NOT NULL
            AND credential_issued_at IS NOT NULL
            AND credential_issued_at >= created_at
            AND credential_issued_at < absolute_expires_at
            AND last_seen_at >= credential_issued_at
            AND expires_at > credential_issued_at
            AND expires_at <= credential_issued_at + interval '900 seconds'
            AND refresh_idle_expires_at > credential_issued_at
            AND refresh_idle_expires_at <= credential_issued_at + interval '2592000 seconds'
            AND expires_at <= absolute_expires_at
            AND refresh_idle_expires_at <= absolute_expires_at
            AND absolute_expires_at <= created_at + interval '15552000 seconds'
            AND (revoked_at IS NULL OR revoked_at >= created_at)
        )
    ) NOT VALID;

ALTER TABLE device_enrollments
    ADD CONSTRAINT device_enrollments_runtime_scopes_check CHECK (
        consumed_at IS NOT NULL OR revoked_at IS NOT NULL OR (
            cardinality(scopes) > 0
            AND scopes <@ ARRAY[
                'suggestions_read', 'suggestions_write',
                'schedule_read', 'schedule_simulate',
                'items_read', 'items_write',
                'execution_read', 'execution_write',
                'google_read', 'google_write',
                'auth_sessions_read', 'auth_sessions_write',
                'auth_mcp_clients_read', 'auth_mcp_clients_write'
            ]::text[]
        )
    ) NOT VALID;

-- NOT VALID on the audience constraints permits a safe rolling migration if a
-- foundation-only deployment created version-1 rows with the earlier shared
-- scope vocabulary. New writes and updates are still checked. The
-- credential-only cutover runbook requires auditing/reissuing those rows before
-- the constraints are validated.
ALTER TABLE mcp_clients
    ADD CONSTRAINT mcp_clients_v1_runtime_shape_check CHECK (
        auth_version <> 1 OR revoked_at IS NOT NULL OR status = 'revoked' OR (
            credential_hash IS NOT NULL
            AND octet_length(credential_hash) = 32
            AND cardinality(scopes) > 0
            AND scopes <@ ARRAY[
                'schedule_read',
                'schedule_simulate',
                'suggestions_submit'
            ]::text[]
            AND cardinality(allowed_origins) <= 100
            AND array_position(allowed_origins, NULL) IS NULL
            AND expires_at IS NOT NULL
            AND expires_at > created_at
            AND expires_at <= created_at + interval '31536000 seconds'
        )
    ) NOT VALID;
