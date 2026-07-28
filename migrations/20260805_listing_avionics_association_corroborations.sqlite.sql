-- Keep catalog-product reuse and exact listing-association corroboration as
-- separate positive conclusions. A product attested from one listing must
-- never silently validate another listing occurrence.

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

DROP TABLE IF EXISTS temp.listing_avionics_corroboration_migration_guard;
CREATE TEMP TABLE listing_avionics_corroboration_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO listing_avionics_corroboration_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name =
      '20260805_listing_avionics_association_corroborations'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name =
      '20260805_listing_avionics_association_corroborations'
      AND contract_version = 1
      AND contract_fingerprint =
        '2c4661b8bf76e1a28d5ab5c636ed100f5d73f845c44b9515e5f46c5827e66fc9'
  ) THEN 1
  ELSE 0
END;
DROP TABLE listing_avionics_corroboration_migration_guard;

CREATE TABLE IF NOT EXISTS
  aircraft_sale_listing_avionics_corroborations (
    listing_link_id INTEGER NOT NULL
      REFERENCES aircraft_sale_listing_avionics(id) ON DELETE CASCADE,
    association_role TEXT NOT NULL
      CHECK (association_role IN ('installed', 'replacement')),
    avionics_model_id INTEGER NOT NULL
      REFERENCES avionics_product_reuse_attestations(avionics_model_id)
      ON DELETE CASCADE,
    observation_sha256 TEXT NOT NULL,
    product_fingerprint TEXT NOT NULL,
    policy_version TEXT NOT NULL
      CHECK (policy_version = 'listing_avionics_association_v1'),
    corroborated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (listing_link_id, association_role),
    CHECK (length(observation_sha256) = 64),
    CHECK (observation_sha256 = lower(observation_sha256)),
    CHECK (observation_sha256 NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(product_fingerprint) = 64),
    CHECK (product_fingerprint = lower(product_fingerprint)),
    CHECK (product_fingerprint NOT GLOB '*[^0-9a-f]*')
  );

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_corroborations_model
ON aircraft_sale_listing_avionics_corroborations (avionics_model_id);

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_corroborations_validate_insert
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

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_corroborations_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_corroborations
BEGIN
  SELECT RAISE(
    ABORT,
    'listing avionics corroborations are replaced, never updated'
  );
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_corroborations_invalidate_link_update
AFTER UPDATE OF
  aircraft_sale_listing_id,
  avionics_model_id,
  quantity,
  source_notes,
  configuration_action,
  replaces_avionics_model_id
ON aircraft_sale_listing_avionics
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_corroborations
  WHERE listing_link_id = NEW.id;
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260805_listing_avionics_association_corroborations',
  1,
  '2c4661b8bf76e1a28d5ab5c636ed100f5d73f845c44b9515e5f46c5827e66fc9',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = excluded.contract_version,
  contract_fingerprint = excluded.contract_fingerprint
WHERE schema_migration_contracts.contract_version = excluded.contract_version
  AND schema_migration_contracts.contract_fingerprint =
      excluded.contract_fingerprint;

COMMIT;
