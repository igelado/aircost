-- Allow a freshly grounded review to refresh evidence on an otherwise
-- immutable approved avionics identity. Canonical manufacturer, model, and
-- stable-identifier fields remain database-immutable.

PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL,
  contract_fingerprint TEXT NOT NULL,
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(migration_name)) > 0),
  CHECK (typeof(contract_version) = 'integer' AND contract_version > 0),
  CHECK (length(contract_fingerprint) = 64),
  CHECK (contract_fingerprint = lower(contract_fingerprint)),
  CHECK (contract_fingerprint NOT GLOB '*[^0-9a-f]*')
);

DROP TABLE IF EXISTS temp.avionics_grounded_evidence_migration_guard;
CREATE TEMP TABLE avionics_grounded_evidence_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_grounded_evidence_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260804_avionics_grounded_evidence_refresh'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260804_avionics_grounded_evidence_refresh'
      AND contract_version = 1
      AND contract_fingerprint =
        '0c44e30c662d8f51c11f7db883251c1356cfda4d53957df038988c32d3b91399'
  ) THEN 1
  ELSE 0
END;
DROP TABLE avionics_grounded_evidence_migration_guard;

DROP TRIGGER IF EXISTS avionics_models_approved_identity_immutable;
CREATE TRIGGER avionics_models_approved_identity_immutable
BEFORE UPDATE ON avionics_models
WHEN OLD.catalog_status = 'approved'
AND (
  NEW.catalog_status IS NOT OLD.catalog_status
  OR NEW.avionics_manufacturer_id IS NOT OLD.avionics_manufacturer_id
  OR NEW.name IS NOT OLD.name
  OR NEW.normalized_name IS NOT OLD.normalized_name
  OR NEW.manufacturer_identifier_kind IS NOT OLD.manufacturer_identifier_kind
  OR NEW.manufacturer_identifier IS NOT OLD.manufacturer_identifier
  OR NEW.normalized_manufacturer_identifier
    IS NOT OLD.normalized_manufacturer_identifier
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics product cannot be demoted or rewrite canonical identity');
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260804_avionics_grounded_evidence_refresh',
  1,
  '0c44e30c662d8f51c11f7db883251c1356cfda4d53957df038988c32d3b91399',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = excluded.contract_version,
  contract_fingerprint = excluded.contract_fingerprint
WHERE schema_migration_contracts.contract_version = excluded.contract_version
  AND schema_migration_contracts.contract_fingerprint =
      excluded.contract_fingerprint;

COMMIT;
