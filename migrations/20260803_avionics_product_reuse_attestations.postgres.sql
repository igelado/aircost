-- Positive-only current-policy eligibility for reusing approved avionics.
--
-- Existing approved products are intentionally not seeded. They remain
-- historical catalog and collision-review inputs, but cannot bypass grounding
-- until a current pipeline admission writes a complete fingerprint bound to
-- one active exact manufacturer origin.

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
    WHERE migration_name = '20260803_avionics_product_reuse_attestations'
      AND NOT (
        (
          contract_version IS NOT DISTINCT FROM 1
          AND contract_fingerprint IS NOT DISTINCT FROM
            'edfe54b792fa91890bd1708ad23b58f4fd9f9c717b42147f5edb948d67ccd837'
        )
        OR (
          contract_version IS NOT DISTINCT FROM 2
          AND contract_fingerprint IS NOT DISTINCT FROM
            '8ad6e935e1222a03e2da4848a9e3c6f4b7f50ee027a6e50ede3b692d034cae55'
        )
      )
  ) THEN
    RAISE EXCEPTION
      'installed avionics product reuse-attestation migration has a different contract';
  END IF;
END
$migration_guard$;

CREATE TABLE IF NOT EXISTS avionics_product_reuse_attestations (
  avionics_model_id BIGINT PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_authoritative_source_origin_id BIGINT NOT NULL
    REFERENCES avionics_authoritative_source_origins(id) ON DELETE RESTRICT,
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'avionics_reuse_v1'),
  product_fingerprint TEXT NOT NULL
    CHECK (product_fingerprint ~ '^[0-9a-f]{64}$'),
  attested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

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
  '20260803_avionics_product_reuse_attestations',
  2,
  '8ad6e935e1222a03e2da4848a9e3c6f4b7f50ee027a6e50ede3b692d034cae55',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = EXCLUDED.contract_version,
  contract_fingerprint = EXCLUDED.contract_fingerprint,
  installed_at = EXCLUDED.installed_at
WHERE schema_migration_contracts.contract_version = 1
  AND schema_migration_contracts.contract_fingerprint =
      'edfe54b792fa91890bd1708ad23b58f4fd9f9c717b42147f5edb948d67ccd837';

COMMIT;
