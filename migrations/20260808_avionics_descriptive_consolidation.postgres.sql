-- Permit one human-reviewed avionics consolidation authorization to bind
-- typography/descriptive equivalents whose exact stored model keys differ.
--
-- The authorization key remains the survivor key. Every member carries its
-- own exact current model key, and the valid-pairs view closes over every
-- current catalog row for every selected key under the effective manufacturer
-- identity. Runtime review remains responsible for proving that the selected
-- keys are descriptive equivalents rather than meaningful product variants.

BEGIN;

SET LOCAL search_path = public, pg_catalog, pg_temp;

CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

LOCK TABLE public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

DO $migration_guard$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name =
          '20260731_avionics_human_reviewed_consolidation'
      AND contract_version = 1
      AND contract_fingerprint =
        '93a641a0f653eacf0c8413bdb697a35c588fe34efc1419d30bf65146c8b2d55a'
  ) OR NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260807_avionics_product_reuse_v2'
      AND contract_version = 1
      AND contract_fingerprint =
        'efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc'
  ) OR EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name =
          '20260808_avionics_descriptive_consolidation'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          '3aacf958efa7fb5e24c5897cf0369d40cb506b2a22444d629ea0a76462ce1a70'
      )
  ) THEN
    RAISE EXCEPTION
      'installed descriptive avionics consolidation migration has a different contract or is missing a required predecessor';
  END IF;
END
$migration_guard$;

LOCK TABLE
  avionics_models,
  avionics_catalog_human_consolidation_authorizations,
  avionics_catalog_human_consolidation_members
IN SHARE ROW EXCLUSIVE MODE;

CREATE OR REPLACE FUNCTION validate_human_avionics_consolidation_member()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_human_consolidation_authorizations authorization_row
    JOIN avionics_models model
      ON model.id = NEW.avionics_model_id_snapshot
    JOIN avionics_manufacturers manufacturer
      ON manufacturer.id = model.avionics_manufacturer_id
    JOIN avionics_manufacturer_effective_memberships manufacturer_identity
      ON manufacturer_identity.avionics_manufacturer_id
        = model.avionics_manufacturer_id
    WHERE authorization_row.authorization_sha256 = NEW.authorization_sha256
      AND (
        (NEW.member_role = 'survivor'
          AND NEW.avionics_model_id_snapshot
            = authorization_row.survivor_model_id_snapshot)
        OR
        (NEW.member_role = 'duplicate'
          AND NEW.avionics_model_id_snapshot
            <> authorization_row.survivor_model_id_snapshot)
      )
      AND (
        SELECT COUNT(*)
        FROM avionics_catalog_human_consolidation_members existing
        WHERE existing.authorization_sha256 = NEW.authorization_sha256
      ) < authorization_row.expected_member_count
      AND NEW.effective_manufacturer_identity_id_snapshot
        = authorization_row.effective_manufacturer_identity_id_snapshot
      AND (
        NEW.member_role <> 'survivor'
        OR NEW.canonical_model_key_snapshot
          = authorization_row.canonical_model_key_snapshot
      )
      AND model.catalog_status = 'unreviewed'
      AND NEW.avionics_manufacturer_id_snapshot = model.avionics_manufacturer_id
      AND NEW.effective_manufacturer_identity_id_snapshot
        = manufacturer_identity.avionics_manufacturer_identity_id
      AND NEW.manufacturer_name_snapshot = manufacturer.name
      AND NEW.stored_manufacturer_key_snapshot = manufacturer.normalized_name
      AND NEW.model_name_snapshot = model.name
      AND NEW.stored_model_key_snapshot = model.normalized_name
      AND NEW.canonical_model_key_snapshot = model.normalized_name
      AND NEW.catalog_status_snapshot = model.catalog_status
      AND NEW.manufacturer_identifier_kind_snapshot
        IS NOT DISTINCT FROM model.manufacturer_identifier_kind
      AND NEW.manufacturer_identifier_snapshot
        IS NOT DISTINCT FROM model.manufacturer_identifier
      AND NEW.normalized_manufacturer_identifier_snapshot
        IS NOT DISTINCT FROM model.normalized_manufacturer_identifier
      AND NEW.identity_source_url_snapshot
        IS NOT DISTINCT FROM model.identity_source_url
      AND NEW.identity_source_title_snapshot
        IS NOT DISTINCT FROM model.identity_source_title
      AND NEW.identity_evidence_text_snapshot
        IS NOT DISTINCT FROM model.identity_evidence_text
      AND NEW.identity_evidence_kind_snapshot = model.identity_evidence_kind
      AND NEW.identity_confidence_snapshot
        IS NOT DISTINCT FROM model.identity_confidence
      AND NEW.catalog_reviewed_at_snapshot
        IS NOT DISTINCT FROM model.catalog_reviewed_at
  ) THEN
    RAISE EXCEPTION
      'human avionics consolidation member is not an exact current row snapshot';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS
  avionics_catalog_human_consolidation_members_validate_insert
ON avionics_catalog_human_consolidation_members;
CREATE TRIGGER
  avionics_catalog_human_consolidation_members_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_members
FOR EACH ROW EXECUTE FUNCTION validate_human_avionics_consolidation_member();

CREATE OR REPLACE VIEW avionics_catalog_valid_human_consolidation_pairs AS
SELECT
  authorization_row.authorization_sha256,
  duplicate_member.avionics_model_id_snapshot AS duplicate_model_id,
  authorization_row.survivor_model_id_snapshot AS survivor_model_id
FROM avionics_catalog_human_consolidation_authorizations authorization_row
JOIN avionics_catalog_human_consolidation_members duplicate_member
  ON duplicate_member.authorization_sha256 =
       authorization_row.authorization_sha256
 AND duplicate_member.member_role = 'duplicate'
WHERE (
    SELECT COUNT(*)
    FROM avionics_catalog_human_consolidation_members member
    WHERE member.authorization_sha256 = authorization_row.authorization_sha256
  ) = authorization_row.expected_member_count
  AND (
    SELECT COUNT(*)
    FROM avionics_catalog_human_consolidation_members member
    WHERE member.authorization_sha256 = authorization_row.authorization_sha256
      AND member.member_role = 'survivor'
      AND member.avionics_model_id_snapshot
        = authorization_row.survivor_model_id_snapshot
      AND member.canonical_model_key_snapshot
        = authorization_row.canonical_model_key_snapshot
  ) = 1
  AND (
    SELECT COUNT(*)
    FROM avionics_catalog_human_consolidation_members member
    WHERE member.authorization_sha256 = authorization_row.authorization_sha256
      AND member.member_role = 'duplicate'
  ) = authorization_row.expected_member_count - 1
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_human_consolidation_members member
    LEFT JOIN avionics_models model
      ON model.id = member.avionics_model_id_snapshot
    LEFT JOIN avionics_manufacturers manufacturer
      ON manufacturer.id = model.avionics_manufacturer_id
    LEFT JOIN avionics_manufacturer_effective_memberships manufacturer_identity
      ON manufacturer_identity.avionics_manufacturer_id
        = model.avionics_manufacturer_id
    WHERE member.authorization_sha256 = authorization_row.authorization_sha256
      AND (
        model.id IS NULL
        OR model.catalog_status <> 'unreviewed'
        OR member.avionics_manufacturer_id_snapshot
          <> model.avionics_manufacturer_id
        OR member.effective_manufacturer_identity_id_snapshot
          IS DISTINCT FROM manufacturer_identity.avionics_manufacturer_identity_id
        OR member.effective_manufacturer_identity_id_snapshot
          <> authorization_row.effective_manufacturer_identity_id_snapshot
        OR member.manufacturer_name_snapshot <> manufacturer.name
        OR member.stored_manufacturer_key_snapshot
          <> manufacturer.normalized_name
        OR member.model_name_snapshot <> model.name
        OR member.stored_model_key_snapshot <> model.normalized_name
        OR member.canonical_model_key_snapshot <> model.normalized_name
        OR member.catalog_status_snapshot <> model.catalog_status
        OR member.manufacturer_identifier_kind_snapshot
          IS DISTINCT FROM model.manufacturer_identifier_kind
        OR member.manufacturer_identifier_snapshot
          IS DISTINCT FROM model.manufacturer_identifier
        OR member.normalized_manufacturer_identifier_snapshot
          IS DISTINCT FROM model.normalized_manufacturer_identifier
        OR member.identity_source_url_snapshot
          IS DISTINCT FROM model.identity_source_url
        OR member.identity_source_title_snapshot
          IS DISTINCT FROM model.identity_source_title
        OR member.identity_evidence_text_snapshot
          IS DISTINCT FROM model.identity_evidence_text
        OR member.identity_evidence_kind_snapshot <> model.identity_evidence_kind
        OR member.identity_confidence_snapshot
          IS DISTINCT FROM model.identity_confidence
        OR member.catalog_reviewed_at_snapshot
          IS DISTINCT FROM model.catalog_reviewed_at
      )
  )
  AND (
    SELECT COUNT(*)
    FROM avionics_models current_model
    JOIN avionics_manufacturer_effective_memberships current_identity
      ON current_identity.avionics_manufacturer_id
        = current_model.avionics_manufacturer_id
    WHERE current_identity.avionics_manufacturer_identity_id
        = authorization_row.effective_manufacturer_identity_id_snapshot
      AND EXISTS (
        SELECT 1
        FROM avionics_catalog_human_consolidation_members selected_member
        WHERE selected_member.authorization_sha256
            = authorization_row.authorization_sha256
          AND selected_member.canonical_model_key_snapshot
            = current_model.normalized_name
      )
  ) = authorization_row.expected_member_count
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models current_model
    JOIN avionics_manufacturer_effective_memberships current_identity
      ON current_identity.avionics_manufacturer_id
        = current_model.avionics_manufacturer_id
    WHERE current_identity.avionics_manufacturer_identity_id
        = authorization_row.effective_manufacturer_identity_id_snapshot
      AND EXISTS (
        SELECT 1
        FROM avionics_catalog_human_consolidation_members selected_member
        WHERE selected_member.authorization_sha256
            = authorization_row.authorization_sha256
          AND selected_member.canonical_model_key_snapshot
            = current_model.normalized_name
      )
      AND NOT EXISTS (
        SELECT 1
        FROM avionics_catalog_human_consolidation_members member
        WHERE member.authorization_sha256 =
              authorization_row.authorization_sha256
          AND member.avionics_model_id_snapshot = current_model.id
      )
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_human_consolidation_members left_member
    JOIN avionics_models left_model
      ON left_model.id = left_member.avionics_model_id_snapshot
    JOIN avionics_catalog_human_consolidation_members right_member
      ON right_member.authorization_sha256 = left_member.authorization_sha256
     AND right_member.avionics_model_id_snapshot
       > left_member.avionics_model_id_snapshot
    JOIN avionics_models right_model
      ON right_model.id = right_member.avionics_model_id_snapshot
    WHERE left_member.authorization_sha256 =
          authorization_row.authorization_sha256
      AND left_model.manufacturer_identifier_kind IS NOT NULL
      AND left_model.normalized_manufacturer_identifier IS NOT NULL
      AND right_model.manufacturer_identifier_kind IS NOT NULL
      AND right_model.normalized_manufacturer_identifier IS NOT NULL
      AND (
        left_model.manufacturer_identifier_kind
          <> right_model.manufacturer_identifier_kind
        OR left_model.normalized_manufacturer_identifier
          <> right_model.normalized_manufacturer_identifier
      )
  );

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260808_avionics_descriptive_consolidation',
  1,
  '3aacf958efa7fb5e24c5897cf0369d40cb506b2a22444d629ea0a76462ce1a70',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
