-- Allow a freshly grounded review to refresh evidence on an otherwise
-- immutable approved avionics identity. Canonical manufacturer, model, and
-- stable-identifier fields remain database-immutable.

BEGIN;

SET LOCAL search_path = public, pg_catalog, pg_temp;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version BIGINT NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

LOCK TABLE ONLY public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM ONLY public.schema_migration_contracts
    WHERE migration_name = '20260804_avionics_grounded_evidence_refresh'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          '0c44e30c662d8f51c11f7db883251c1356cfda4d53957df038988c32d3b91399'
      )
  ) THEN
    RAISE EXCEPTION
      'installed avionics grounded-evidence refresh migration has a different contract';
  END IF;
END
$migration_guard$;

CREATE OR REPLACE FUNCTION preserve_approved_avionics_identity()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF OLD.catalog_status = 'approved' AND (
    NEW.catalog_status IS DISTINCT FROM OLD.catalog_status
    OR NEW.avionics_manufacturer_id IS DISTINCT FROM OLD.avionics_manufacturer_id
    OR NEW.name IS DISTINCT FROM OLD.name
    OR NEW.normalized_name IS DISTINCT FROM OLD.normalized_name
    OR NEW.manufacturer_identifier_kind IS DISTINCT FROM OLD.manufacturer_identifier_kind
    OR NEW.manufacturer_identifier IS DISTINCT FROM OLD.manufacturer_identifier
    OR NEW.normalized_manufacturer_identifier
      IS DISTINCT FROM OLD.normalized_manufacturer_identifier
  ) THEN
    RAISE EXCEPTION
      'approved avionics product cannot be demoted or rewrite canonical identity';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_models_approved_identity_immutable
  ON avionics_models;
CREATE TRIGGER avionics_models_approved_identity_immutable
BEFORE UPDATE ON avionics_models
FOR EACH ROW EXECUTE FUNCTION preserve_approved_avionics_identity();

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260804_avionics_grounded_evidence_refresh',
  1,
  '0c44e30c662d8f51c11f7db883251c1356cfda4d53957df038988c32d3b91399',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
