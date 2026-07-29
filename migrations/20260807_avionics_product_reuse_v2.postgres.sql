-- Invalidate every attestation produced before the target-aware OEM verifier.
--
-- Catalog products and listing observations remain historical facts. Reuse
-- eligibility, exact listing corroborations, and their collision scopes are
-- disposable positive conclusions and must be earned again under v2.

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
  IF NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260803_avionics_product_reuse_attestations'
      AND contract_version = 2
      AND contract_fingerprint =
        '8ad6e935e1222a03e2da4848a9e3c6f4b7f50ee027a6e50ede3b692d034cae55'
  ) OR NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name =
          '20260805_listing_avionics_association_corroborations'
      AND contract_version = 1
      AND contract_fingerprint =
        '2c4661b8bf76e1a28d5ab5c636ed100f5d73f845c44b9515e5f46c5827e66fc9'
  ) OR NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260806_listing_avionics_collision_closure'
      AND contract_version = 1
      AND contract_fingerprint =
        '363fd039068667cca351c0009c0621e55942186a5d63804cf0e7da8212fa26b3'
  ) OR EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260807_avionics_product_reuse_v2'
      AND (
        contract_version <> 1
        OR contract_fingerprint <>
          'efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc'
      )
  ) THEN
    RAISE EXCEPTION
      'installed avionics product reuse-v2 migration has a different contract or is missing a required predecessor';
  END IF;
END
$migration_guard$;

LOCK TABLE
  avionics_product_reuse_attestations,
  aircraft_sale_listing_avionics_corroborations,
  aircraft_sale_listing_avionics_corroboration_scopes
IN SHARE ROW EXCLUSIVE MODE;

-- The cascading foreign keys remove each dependent corroboration and scope.
-- No obsolete attestation is copied or rewritten into the new policy.
DELETE FROM avionics_product_reuse_attestations
WHERE policy_version <> 'avionics_reuse_v2';

DO $drop_policy_constraints$
DECLARE
  constraint_name TEXT;
BEGIN
  FOR constraint_name IN
    SELECT actual.conname
    FROM pg_constraint actual
    WHERE actual.conrelid =
          to_regclass('avionics_product_reuse_attestations')
      AND actual.contype = 'c'
      AND position(
        'policy_version' IN lower(pg_get_constraintdef(actual.oid))
      ) > 0
  LOOP
    EXECUTE format(
      'ALTER TABLE avionics_product_reuse_attestations DROP CONSTRAINT %I',
      constraint_name
    );
  END LOOP;
END
$drop_policy_constraints$;
ALTER TABLE avionics_product_reuse_attestations
  ADD CONSTRAINT avionics_product_reuse_attestations_policy_version_check
  CHECK (policy_version = 'avionics_reuse_v2');

DROP INDEX IF EXISTS idx_avionics_product_reuse_origin;
CREATE INDEX idx_avionics_product_reuse_origin
  ON avionics_product_reuse_attestations (
    avionics_authoritative_source_origin_id
  );

CREATE OR REPLACE FUNCTION validate_avionics_product_reuse_attestation()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
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
  ) THEN
    RAISE EXCEPTION
      'avionics reuse attestation requires an approved product bound to one active exact manufacturer origin';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_product_reuse_attestations_validate_insert
  ON avionics_product_reuse_attestations;
CREATE TRIGGER avionics_product_reuse_attestations_validate_insert
BEFORE INSERT ON avionics_product_reuse_attestations
FOR EACH ROW EXECUTE FUNCTION validate_avionics_product_reuse_attestation();

CREATE OR REPLACE FUNCTION preserve_avionics_product_reuse_attestation()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'avionics reuse attestations are replaced, never updated';
  RETURN NULL;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_product_reuse_attestations_immutable_update
  ON avionics_product_reuse_attestations;
CREATE TRIGGER avionics_product_reuse_attestations_immutable_update
BEFORE UPDATE ON avionics_product_reuse_attestations
FOR EACH ROW EXECUTE FUNCTION preserve_avionics_product_reuse_attestation();

CREATE OR REPLACE FUNCTION invalidate_avionics_product_reuse_for_type()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF TG_OP = 'INSERT' THEN
    DELETE FROM avionics_product_reuse_attestations
    WHERE avionics_model_id = NEW.avionics_model_id;
    RETURN NEW;
  ELSIF TG_OP = 'DELETE' THEN
    DELETE FROM avionics_product_reuse_attestations
    WHERE avionics_model_id = OLD.avionics_model_id;
    RETURN OLD;
  ELSE
    DELETE FROM avionics_product_reuse_attestations
    WHERE avionics_model_id IN (
      OLD.avionics_model_id, NEW.avionics_model_id
    );
    RETURN NEW;
  END IF;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_insert
  ON avionics_model_types;
CREATE TRIGGER avionics_product_reuse_invalidate_type_insert
AFTER INSERT ON avionics_model_types
FOR EACH ROW EXECUTE FUNCTION invalidate_avionics_product_reuse_for_type();

DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_delete
  ON avionics_model_types;
CREATE TRIGGER avionics_product_reuse_invalidate_type_delete
AFTER DELETE ON avionics_model_types
FOR EACH ROW EXECUTE FUNCTION invalidate_avionics_product_reuse_for_type();

DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_update
  ON avionics_model_types;
CREATE TRIGGER avionics_product_reuse_invalidate_type_update
AFTER UPDATE OF avionics_model_id, avionics_type_id ON avionics_model_types
FOR EACH ROW EXECUTE FUNCTION invalidate_avionics_product_reuse_for_type();

CREATE OR REPLACE FUNCTION invalidate_avionics_product_reuse_for_capability()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM avionics_product_reuse_attestations attestation
  USING avionics_model_types membership
  WHERE membership.avionics_type_id = NEW.id
    AND attestation.avionics_model_id = membership.avionics_model_id;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_capability_update
  ON avionics_types;
CREATE TRIGGER avionics_product_reuse_invalidate_capability_update
AFTER UPDATE OF name, normalized_name ON avionics_types
FOR EACH ROW
EXECUTE FUNCTION invalidate_avionics_product_reuse_for_capability();

CREATE OR REPLACE FUNCTION invalidate_avionics_product_reuse_for_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = NEW.avionics_model_id;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_identity_update
  ON avionics_approved_product_identities;
CREATE TRIGGER avionics_product_reuse_invalidate_identity_update
AFTER UPDATE ON avionics_approved_product_identities
FOR EACH ROW EXECUTE FUNCTION invalidate_avionics_product_reuse_for_identity();

CREATE OR REPLACE FUNCTION invalidate_avionics_product_reuse_for_revocation()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_authoritative_source_origin_id =
        NEW.avionics_authoritative_source_origin_id;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_origin_revocation
  ON avionics_authoritative_source_origin_revocations;
CREATE TRIGGER avionics_product_reuse_invalidate_origin_revocation
AFTER INSERT ON avionics_authoritative_source_origin_revocations
FOR EACH ROW EXECUTE FUNCTION invalidate_avionics_product_reuse_for_revocation();

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260807_avionics_product_reuse_v2',
  1,
  'efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = EXCLUDED.contract_version,
  contract_fingerprint = EXCLUDED.contract_fingerprint,
  installed_at = EXCLUDED.installed_at
WHERE schema_migration_contracts.contract_version = EXCLUDED.contract_version
  AND schema_migration_contracts.contract_fingerprint =
      EXCLUDED.contract_fingerprint;

COMMIT;
