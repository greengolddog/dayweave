-- Persist the exact deployment-keyed principal used by the external deletion
-- authority. These fields are content-free and survive the local purge. Rows
-- created by the earlier route-less foundation remain nullable for migration
-- safety, but cannot be advanced by the repository without an exact binding.

ALTER TABLE account_deletion_lifecycles
    ADD COLUMN external_principal_key_version integer,
    ADD COLUMN external_principal_pseudonym bytea,
    ADD CONSTRAINT account_deletion_external_principal_binding_check CHECK (
        (external_principal_key_version IS NULL
            AND external_principal_pseudonym IS NULL)
        OR
        (external_principal_key_version > 0
            AND external_principal_pseudonym IS NOT NULL
            AND octet_length(external_principal_pseudonym) = 32
            AND external_principal_pseudonym
                <> decode(repeat('00', 32), 'hex'))
    );

CREATE FUNCTION guard_account_deletion_external_principal_binding() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.external_principal_key_version IS NULL
           OR NEW.external_principal_pseudonym IS NULL
           OR NEW.external_principal_key_version <= 0
           OR octet_length(NEW.external_principal_pseudonym) <> 32
           OR NEW.external_principal_pseudonym
                = decode(repeat('00', 32), 'hex')
        THEN
            RAISE EXCEPTION USING
                ERRCODE = 'DWREQ',
                MESSAGE = 'account deletion external principal binding is required';
        END IF;
        RETURN NEW;
    END IF;
    IF OLD.external_principal_key_version
            IS DISTINCT FROM NEW.external_principal_key_version
       OR OLD.external_principal_pseudonym
            IS DISTINCT FROM NEW.external_principal_pseudonym
    THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWCON',
            MESSAGE = 'account deletion external principal binding is immutable';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER account_deletion_external_principal_binding_guard
    BEFORE INSERT OR UPDATE ON account_deletion_lifecycles
    FOR EACH ROW EXECUTE FUNCTION guard_account_deletion_external_principal_binding();

DO $pin_account_deletion_external_principal_guard$
DECLARE
    trusted_schema name := current_schema();
BEGIN
    EXECUTE format(
        'ALTER FUNCTION %I.guard_account_deletion_external_principal_binding() '
        'SET search_path TO %I, pg_catalog, pg_temp',
        trusted_schema,
        trusted_schema
    );
END
$pin_account_deletion_external_principal_guard$;

REVOKE ALL ON FUNCTION guard_account_deletion_external_principal_binding() FROM PUBLIC;
