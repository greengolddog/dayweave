-- Canonical sensitivity is an explicit, required item contract. Historical
-- rows predate the flag and are conservatively classified as non-sensitive;
-- all current writes carry the field explicitly through item JSON snapshots.

ALTER TABLE items
    ADD COLUMN is_sensitive boolean NOT NULL DEFAULT false;

UPDATE item_changes
SET payload = jsonb_set(payload, '{is_sensitive}', 'false'::jsonb, true)
WHERE change_kind = 'upsert'
  AND NOT (payload ? 'is_sensitive');

ALTER TABLE item_changes
    ADD CONSTRAINT item_changes_upsert_sensitivity_check CHECK (
        change_kind <> 'upsert'
        OR (
            payload ? 'is_sensitive'
            AND jsonb_typeof(payload -> 'is_sensitive') = 'boolean'
        )
    );

UPDATE idempotency_keys
SET response_json = jsonb_set(response_json, '{is_sensitive}', 'false'::jsonb, true)
WHERE state = 'completed'
  AND resource_type = 'item'
  AND response_json IS NOT NULL
  AND NOT (response_json ? 'is_sensitive');

ALTER TABLE idempotency_keys
    ADD CONSTRAINT idempotency_item_response_sensitivity_check CHECK (
        state <> 'completed'
        OR resource_type IS DISTINCT FROM 'item'
        OR response_json IS NULL
        OR (
            response_json ? 'is_sensitive'
            AND jsonb_typeof(response_json -> 'is_sensitive') = 'boolean'
        )
    );
