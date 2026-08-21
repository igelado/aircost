-- Positive-only current-policy eligibility for reusing approved avionics.
--
-- Existing approved products are intentionally not seeded. They remain
-- historical catalog and collision-review inputs, but cannot bypass grounding
-- until a current pipeline admission writes a complete fingerprint bound to
-- one active exact manufacturer origin.

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

DROP TABLE IF EXISTS temp.avionics_reuse_attestation_migration_guard;
CREATE TEMP TABLE avionics_reuse_attestation_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_reuse_attestation_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260803_avionics_product_reuse_attestations'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260803_avionics_product_reuse_attestations'
      AND (
        (
          contract_version = 1
          AND contract_fingerprint =
            'edfe54b792fa91890bd1708ad23b58f4fd9f9c717b42147f5edb948d67ccd837'
        )
        OR (
          contract_version = 2
          AND contract_fingerprint =
            '8ad6e935e1222a03e2da4848a9e3c6f4b7f50ee027a6e50ede3b692d034cae55'
        )
      )
  ) THEN 1
  ELSE 0
END;
DROP TABLE avionics_reuse_attestation_migration_guard;

CREATE TABLE IF NOT EXISTS avionics_product_reuse_attestations (
  avionics_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_authoritative_source_origin_id INTEGER NOT NULL
    REFERENCES avionics_authoritative_source_origins(id) ON DELETE RESTRICT,
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'avionics_reuse_v1'),
  product_fingerprint TEXT NOT NULL,
  attested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(product_fingerprint) = 64),
  CHECK (product_fingerprint = lower(product_fingerprint)),
  CHECK (product_fingerprint NOT GLOB '*[^0-9a-f]*')
);

DROP INDEX IF EXISTS idx_avionics_product_reuse_origin;
CREATE INDEX idx_avionics_product_reuse_origin
  ON avionics_product_reuse_attestations (
    avionics_authoritative_source_origin_id
  );

DROP TRIGGER IF EXISTS
  avionics_product_reuse_attestations_validate_insert;
CREATE TRIGGER
  avionics_product_reuse_attestations_validate_insert
BEFORE INSERT ON avionics_product_reuse_attestations
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  JOIN avionics_approved_product_identities product_identity
    ON product_identity.avionics_model_id = model.id
  JOIN avionics_active_authoritative_source_origins source_origin
    ON source_origin.id =
       NEW.avionics_authoritative_source_origin_id
   AND source_origin.authority_kind = 'manufacturer_primary'
  JOIN avionics_manufacturer_effective_identities origin_identity
    ON origin_identity.identity_id =
       source_origin.avionics_manufacturer_identity_id
   AND origin_identity.avionics_manufacturer_identity_id =
       product_identity.avionics_manufacturer_identity_id
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(
    ABORT,
    'avionics reuse attestation requires an approved product bound to one active exact manufacturer origin'
  );
END;

DROP TRIGGER IF EXISTS
  avionics_product_reuse_attestations_immutable_update;
CREATE TRIGGER
  avionics_product_reuse_attestations_immutable_update
BEFORE UPDATE ON avionics_product_reuse_attestations
BEGIN
  SELECT RAISE(
    ABORT,
    'avionics reuse attestations are replaced, never updated'
  );
END;

DROP TRIGGER IF EXISTS
  avionics_product_reuse_invalidate_type_insert;
CREATE TRIGGER
  avionics_product_reuse_invalidate_type_insert
AFTER INSERT ON avionics_model_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = NEW.avionics_model_id;
END;

DROP TRIGGER IF EXISTS
  avionics_product_reuse_invalidate_type_delete;
CREATE TRIGGER
  avionics_product_reuse_invalidate_type_delete
AFTER DELETE ON avionics_model_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = OLD.avionics_model_id;
END;

DROP TRIGGER IF EXISTS
  avionics_product_reuse_invalidate_type_update;
CREATE TRIGGER
  avionics_product_reuse_invalidate_type_update
AFTER UPDATE OF avionics_model_id, avionics_type_id ON avionics_model_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id IN (
    OLD.avionics_model_id, NEW.avionics_model_id
  );
END;

DROP TRIGGER IF EXISTS
  avionics_product_reuse_invalidate_capability_update;
CREATE TRIGGER
  avionics_product_reuse_invalidate_capability_update
AFTER UPDATE OF name, normalized_name ON avionics_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id IN (
    SELECT membership.avionics_model_id
    FROM avionics_model_types membership
    WHERE membership.avionics_type_id = NEW.id
  );
END;

DROP TRIGGER IF EXISTS
  avionics_product_reuse_invalidate_identity_update;
CREATE TRIGGER
  avionics_product_reuse_invalidate_identity_update
AFTER UPDATE ON avionics_approved_product_identities
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = NEW.avionics_model_id;
END;

DROP TRIGGER IF EXISTS
  avionics_product_reuse_invalidate_origin_revocation;
CREATE TRIGGER
  avionics_product_reuse_invalidate_origin_revocation
AFTER INSERT ON avionics_authoritative_source_origin_revocations
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_authoritative_source_origin_id =
        NEW.avionics_authoritative_source_origin_id;
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260803_avionics_product_reuse_attestations',
  2,
  '8ad6e935e1222a03e2da4848a9e3c6f4b7f50ee027a6e50ede3b692d034cae55',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = excluded.contract_version,
  contract_fingerprint = excluded.contract_fingerprint,
  installed_at = excluded.installed_at
WHERE schema_migration_contracts.contract_version = 1
  AND schema_migration_contracts.contract_fingerprint =
      'edfe54b792fa91890bd1708ad23b58f4fd9f9c717b42147f5edb948d67ccd837';

COMMIT;
PRAGMA foreign_key_check;
