-- Keep catalog-product reuse and exact listing-association corroboration as
-- separate positive conclusions. A product attested from one listing must
-- never silently validate another listing occurrence.

BEGIN;

CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version BIGINT NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name =
      '20260805_listing_avionics_association_corroborations'
      AND NOT (
        contract_version = 1
        AND contract_fingerprint =
          '2c4661b8bf76e1a28d5ab5c636ed100f5d73f845c44b9515e5f46c5827e66fc9'
      )
  ) THEN
    RAISE EXCEPTION
      'installed listing-avionics corroboration migration has a different contract';
  END IF;
END
$migration_guard$;

CREATE TABLE IF NOT EXISTS
  aircraft_sale_listing_avionics_corroborations (
    listing_link_id BIGINT NOT NULL
      REFERENCES aircraft_sale_listing_avionics(id) ON DELETE CASCADE,
    association_role TEXT NOT NULL
      CHECK (association_role IN ('installed', 'replacement')),
    avionics_model_id BIGINT NOT NULL
      REFERENCES avionics_product_reuse_attestations(avionics_model_id)
      ON DELETE CASCADE,
    observation_sha256 TEXT NOT NULL
      CHECK (observation_sha256 ~ '^[0-9a-f]{64}$'),
    product_fingerprint TEXT NOT NULL
      CHECK (product_fingerprint ~ '^[0-9a-f]{64}$'),
    policy_version TEXT NOT NULL
      CHECK (policy_version = 'listing_avionics_association_v1'),
    corroborated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (listing_link_id, association_role)
  );

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_corroborations_model
ON aircraft_sale_listing_avionics_corroborations (avionics_model_id);

CREATE OR REPLACE FUNCTION validate_listing_avionics_corroboration()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NOT EXISTS (
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
  ) THEN
    RAISE EXCEPTION
      'listing avionics corroboration requires the exact current link role and product attestation';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS listing_avionics_corroborations_validate_insert
  ON aircraft_sale_listing_avionics_corroborations;
CREATE TRIGGER listing_avionics_corroborations_validate_insert
BEFORE INSERT ON aircraft_sale_listing_avionics_corroborations
FOR EACH ROW EXECUTE FUNCTION validate_listing_avionics_corroboration();

CREATE OR REPLACE FUNCTION preserve_listing_avionics_corroboration()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  RAISE EXCEPTION 'listing avionics corroborations are replaced, never updated';
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS listing_avionics_corroborations_immutable_update
  ON aircraft_sale_listing_avionics_corroborations;
CREATE TRIGGER listing_avionics_corroborations_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_corroborations
FOR EACH ROW EXECUTE FUNCTION preserve_listing_avionics_corroboration();

CREATE OR REPLACE FUNCTION invalidate_listing_avionics_corroboration()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_corroborations
  WHERE listing_link_id = NEW.id;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS listing_avionics_corroborations_invalidate_link_update
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER listing_avionics_corroborations_invalidate_link_update
AFTER UPDATE OF
  aircraft_sale_listing_id,
  avionics_model_id,
  quantity,
  source_notes,
  configuration_action,
  replaces_avionics_model_id
ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION invalidate_listing_avionics_corroboration();

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260805_listing_avionics_association_corroborations',
  1,
  '2c4661b8bf76e1a28d5ab5c636ed100f5d73f845c44b9515e5f46c5827e66fc9',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = EXCLUDED.contract_version,
  contract_fingerprint = EXCLUDED.contract_fingerprint
WHERE schema_migration_contracts.contract_version = EXCLUDED.contract_version
  AND schema_migration_contracts.contract_fingerprint =
      EXCLUDED.contract_fingerprint;

COMMIT;
