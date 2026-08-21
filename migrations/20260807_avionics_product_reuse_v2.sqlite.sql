-- Invalidate every attestation produced before the target-aware OEM verifier.
--
-- Catalog products and listing observations remain historical facts. Reuse
-- eligibility, exact listing corroborations, and their collision scopes are
-- disposable positive conclusions and must be earned again under v2.

PRAGMA foreign_keys = OFF;
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

DROP TABLE IF EXISTS temp.avionics_product_reuse_v2_migration_guard;
CREATE TEMP TABLE avionics_product_reuse_v2_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_product_reuse_v2_migration_guard (accepted)
SELECT CASE
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260803_avionics_product_reuse_attestations'
      AND contract_version = 2
      AND contract_fingerprint =
        '8ad6e935e1222a03e2da4848a9e3c6f4b7f50ee027a6e50ede3b692d034cae55'
  )
  AND EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name =
          '20260805_listing_avionics_association_corroborations'
      AND contract_version = 1
      AND contract_fingerprint =
        '2c4661b8bf76e1a28d5ab5c636ed100f5d73f845c44b9515e5f46c5827e66fc9'
  )
  AND EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260806_listing_avionics_collision_closure'
      AND contract_version = 1
      AND contract_fingerprint =
        '363fd039068667cca351c0009c0621e55942186a5d63804cf0e7da8212fa26b3'
  )
  AND (
    NOT EXISTS (
      SELECT 1
      FROM schema_migration_contracts
      WHERE migration_name = '20260807_avionics_product_reuse_v2'
    )
    OR EXISTS (
      SELECT 1
      FROM schema_migration_contracts
      WHERE migration_name = '20260807_avionics_product_reuse_v2'
        AND contract_version = 1
        AND contract_fingerprint =
          'efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc'
    )
  ) THEN 1
  ELSE 0
END;
DROP TABLE avionics_product_reuse_v2_migration_guard;

-- Remove only conclusions whose parent was produced under obsolete
-- semantics. These explicit deletes make the cleanup independent of whether
-- foreign-key enforcement was enabled by the migration caller.
DELETE FROM aircraft_sale_listing_avionics_corroboration_scopes
WHERE EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_avionics_corroborations corroboration
  JOIN avionics_product_reuse_attestations attestation
    ON attestation.avionics_model_id = corroboration.avionics_model_id
  WHERE corroboration.listing_link_id =
        aircraft_sale_listing_avionics_corroboration_scopes.listing_link_id
    AND corroboration.association_role =
        aircraft_sale_listing_avionics_corroboration_scopes.association_role
    AND attestation.policy_version <> 'avionics_reuse_v2'
);

DELETE FROM aircraft_sale_listing_avionics_corroborations
WHERE EXISTS (
  SELECT 1
  FROM avionics_product_reuse_attestations attestation
  WHERE attestation.avionics_model_id =
        aircraft_sale_listing_avionics_corroborations.avionics_model_id
    AND attestation.policy_version <> 'avionics_reuse_v2'
);

-- SQLite reparses triggers on other tables while a referenced table is
-- renamed. Remove every cross-table reference before the rebuild and restore
-- the canonical definitions below.
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_insert;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_delete;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_update;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_capability_update;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_identity_update;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_origin_revocation;
DROP TRIGGER IF EXISTS listing_avionics_corroborations_validate_insert;

DROP TABLE IF EXISTS avionics_product_reuse_attestations_v2;
CREATE TABLE avionics_product_reuse_attestations_v2 (
  avionics_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_authoritative_source_origin_id INTEGER NOT NULL
    REFERENCES avionics_authoritative_source_origins(id) ON DELETE RESTRICT,
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'avionics_reuse_v2'),
  product_fingerprint TEXT NOT NULL,
  attested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(product_fingerprint) = 64),
  CHECK (product_fingerprint = lower(product_fingerprint)),
  CHECK (product_fingerprint NOT GLOB '*[^0-9a-f]*')
);

-- This predicate deliberately copies no v1 attestation. It makes an exact
-- reapplication idempotent without turning migration into an attestation
-- issuance path.
INSERT INTO avionics_product_reuse_attestations_v2 (
  avionics_model_id,
  avionics_authoritative_source_origin_id,
  policy_version,
  product_fingerprint,
  attested_at
)
SELECT
  avionics_model_id,
  avionics_authoritative_source_origin_id,
  policy_version,
  product_fingerprint,
  attested_at
FROM avionics_product_reuse_attestations
WHERE policy_version = 'avionics_reuse_v2';

DROP TABLE avionics_product_reuse_attestations;
ALTER TABLE avionics_product_reuse_attestations_v2
RENAME TO avionics_product_reuse_attestations;

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

CREATE TRIGGER avionics_product_reuse_invalidate_type_insert
AFTER INSERT ON avionics_model_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = NEW.avionics_model_id;
END;

CREATE TRIGGER avionics_product_reuse_invalidate_type_delete
AFTER DELETE ON avionics_model_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = OLD.avionics_model_id;
END;

CREATE TRIGGER avionics_product_reuse_invalidate_type_update
AFTER UPDATE OF avionics_model_id, avionics_type_id ON avionics_model_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id IN (
    OLD.avionics_model_id, NEW.avionics_model_id
  );
END;

CREATE TRIGGER avionics_product_reuse_invalidate_capability_update
AFTER UPDATE OF name, normalized_name ON avionics_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id IN (
    SELECT membership.avionics_model_id
    FROM avionics_model_types membership
    WHERE membership.avionics_type_id = NEW.id
  );
END;

CREATE TRIGGER avionics_product_reuse_invalidate_identity_update
AFTER UPDATE ON avionics_approved_product_identities
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = NEW.avionics_model_id;
END;

CREATE TRIGGER avionics_product_reuse_invalidate_origin_revocation
AFTER INSERT ON avionics_authoritative_source_origin_revocations
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_authoritative_source_origin_id =
        NEW.avionics_authoritative_source_origin_id;
END;

CREATE TRIGGER listing_avionics_corroborations_validate_insert
BEFORE INSERT ON aircraft_sale_listing_avionics_corroborations
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_avionics link
  JOIN avionics_product_reuse_attestations attestation
    ON attestation.avionics_model_id = NEW.avionics_model_id
   AND attestation.product_fingerprint = NEW.product_fingerprint
  WHERE link.id = NEW.listing_link_id
    AND (
      (
        NEW.association_role = 'installed'
        AND link.avionics_model_id = NEW.avionics_model_id
      )
      OR
      (
        NEW.association_role = 'replacement'
        AND link.configuration_action IN ('replaces', 'removes')
        AND link.replaces_avionics_model_id = NEW.avionics_model_id
      )
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'listing avionics corroboration requires the exact current link role and product attestation'
  );
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260807_avionics_product_reuse_v2',
  1,
  'efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
PRAGMA foreign_keys = ON;
PRAGMA foreign_key_check;
