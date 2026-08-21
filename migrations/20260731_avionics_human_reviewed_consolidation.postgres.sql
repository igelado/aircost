-- Evidence-backed human-reviewed avionics consolidation.
--
-- Automatic consolidation remains stable-identifier-only. This migration adds
-- a separate immutable reviewer authorization and activates all exact pairs
-- only through one short-lived, full-set database claim.

BEGIN;

SET LOCAL search_path = public, pg_catalog, pg_temp;

CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(migration_name)) > 0)
);

LOCK TABLE public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260731_avionics_human_reviewed_consolidation'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          '93a641a0f653eacf0c8413bdb697a35c588fe34efc1419d30bf65146c8b2d55a'
      )
  ) THEN
    RAISE EXCEPTION
      'installed human-reviewed avionics consolidation migration has a different contract';
  END IF;
END
$migration_guard$;

CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_authorizations (
  authorization_sha256 TEXT PRIMARY KEY,
  reviewer_user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  survivor_model_id_snapshot BIGINT NOT NULL,
  effective_manufacturer_identity_id_snapshot BIGINT NOT NULL,
  canonical_model_key_snapshot TEXT NOT NULL,
  expected_member_count BIGINT NOT NULL,
  authoritative_source_url TEXT NOT NULL,
  authoritative_source_title TEXT NOT NULL,
  exact_evidence_text TEXT NOT NULL,
  provenance_listing_id_snapshot BIGINT,
  provenance_pending_review_id_snapshot BIGINT,
  provenance_review_payload_sha256 TEXT,
  provenance_review_aspect_id TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (authorization_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (survivor_model_id_snapshot > 0),
  CHECK (effective_manufacturer_identity_id_snapshot > 0),
  CHECK (canonical_model_key_snapshot ~ '^[a-z0-9 ]+$'),
  CHECK (expected_member_count >= 2),
  CHECK (authoritative_source_url LIKE 'https://%'),
  CHECK (LOWER(authoritative_source_url) NOT LIKE '%/listing/%'),
  CHECK (LOWER(authoritative_source_url) NOT LIKE '%/listings/%'),
  CHECK (LOWER(authoritative_source_url) NOT LIKE '%/aircraft-for-sale/%'),
  CHECK (LOWER(authoritative_source_url) NOT LIKE '%/classifieds/%'),
  CHECK (BTRIM(authoritative_source_title) <> ''),
  CHECK (BTRIM(exact_evidence_text) <> ''),
  CHECK (
    (
      provenance_listing_id_snapshot IS NULL
      AND provenance_pending_review_id_snapshot IS NULL
      AND provenance_review_payload_sha256 IS NULL
      AND provenance_review_aspect_id IS NULL
    )
    OR (
      provenance_listing_id_snapshot > 0
      AND provenance_pending_review_id_snapshot > 0
      AND provenance_review_payload_sha256 ~ '^[0-9a-f]{64}$'
      AND BTRIM(provenance_review_aspect_id) <> ''
    )
  )
);

CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_members (
  authorization_sha256 TEXT NOT NULL
    REFERENCES avionics_catalog_human_consolidation_authorizations(
      authorization_sha256
    ) ON DELETE RESTRICT,
  avionics_model_id_snapshot BIGINT NOT NULL,
  member_role TEXT NOT NULL CHECK (member_role IN ('survivor', 'duplicate')),
  row_identity_sha256 TEXT NOT NULL,
  avionics_manufacturer_id_snapshot BIGINT NOT NULL,
  effective_manufacturer_identity_id_snapshot BIGINT NOT NULL,
  manufacturer_name_snapshot TEXT NOT NULL,
  stored_manufacturer_key_snapshot TEXT NOT NULL,
  model_name_snapshot TEXT NOT NULL,
  stored_model_key_snapshot TEXT NOT NULL,
  canonical_model_key_snapshot TEXT NOT NULL,
  catalog_status_snapshot TEXT NOT NULL CHECK (catalog_status_snapshot = 'unreviewed'),
  manufacturer_identifier_kind_snapshot TEXT,
  manufacturer_identifier_snapshot TEXT,
  normalized_manufacturer_identifier_snapshot TEXT,
  identity_source_url_snapshot TEXT,
  identity_source_title_snapshot TEXT,
  identity_evidence_text_snapshot TEXT,
  identity_evidence_kind_snapshot TEXT NOT NULL,
  identity_confidence_snapshot TEXT,
  catalog_reviewed_at_snapshot TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (authorization_sha256, avionics_model_id_snapshot),
  CHECK (avionics_model_id_snapshot > 0),
  CHECK (row_identity_sha256 ~ '^[0-9a-f]{64}$'),
  CHECK (avionics_manufacturer_id_snapshot > 0),
  CHECK (effective_manufacturer_identity_id_snapshot > 0),
  CHECK (BTRIM(manufacturer_name_snapshot) <> ''),
  CHECK (BTRIM(stored_manufacturer_key_snapshot) <> ''),
  CHECK (BTRIM(model_name_snapshot) <> ''),
  CHECK (BTRIM(stored_model_key_snapshot) <> ''),
  CHECK (BTRIM(canonical_model_key_snapshot) <> ''),
  CHECK (
    (
      manufacturer_identifier_kind_snapshot IS NULL
      AND manufacturer_identifier_snapshot IS NULL
      AND normalized_manufacturer_identifier_snapshot IS NULL
    )
    OR (
      manufacturer_identifier_kind_snapshot IS NOT NULL
      AND manufacturer_identifier_snapshot IS NOT NULL
      AND BTRIM(manufacturer_identifier_snapshot) <> ''
      AND normalized_manufacturer_identifier_snapshot IS NOT NULL
      AND BTRIM(normalized_manufacturer_identifier_snapshot) <> ''
    )
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_avionics_human_consolidation_one_survivor
ON avionics_catalog_human_consolidation_members (authorization_sha256)
WHERE member_role = 'survivor';

CREATE OR REPLACE FUNCTION preserve_human_avionics_consolidation_audit()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'human avionics consolidation audit is immutable and permanent';
  RETURN NULL;
END;
$function$;

DROP TRIGGER IF EXISTS
  avionics_catalog_human_consolidation_authorizations_immutable
  ON avionics_catalog_human_consolidation_authorizations;
CREATE TRIGGER avionics_catalog_human_consolidation_authorizations_immutable
BEFORE UPDATE OR DELETE
ON avionics_catalog_human_consolidation_authorizations
FOR EACH ROW EXECUTE FUNCTION preserve_human_avionics_consolidation_audit();

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
      AND NEW.canonical_model_key_snapshot
        = authorization_row.canonical_model_key_snapshot
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
    RAISE EXCEPTION 'human avionics consolidation member is not an exact current row snapshot';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_catalog_human_consolidation_members_validate_insert
  ON avionics_catalog_human_consolidation_members;
CREATE TRIGGER avionics_catalog_human_consolidation_members_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_members
FOR EACH ROW EXECUTE FUNCTION validate_human_avionics_consolidation_member();

DROP TRIGGER IF EXISTS avionics_catalog_human_consolidation_members_immutable
  ON avionics_catalog_human_consolidation_members;
CREATE TRIGGER avionics_catalog_human_consolidation_members_immutable
BEFORE UPDATE OR DELETE ON avionics_catalog_human_consolidation_members
FOR EACH ROW EXECUTE FUNCTION preserve_human_avionics_consolidation_audit();

CREATE OR REPLACE VIEW avionics_catalog_valid_human_consolidation_pairs AS
SELECT
  authorization_row.authorization_sha256,
  duplicate_member.avionics_model_id_snapshot AS duplicate_model_id,
  authorization_row.survivor_model_id_snapshot AS survivor_model_id
FROM avionics_catalog_human_consolidation_authorizations authorization_row
JOIN avionics_catalog_human_consolidation_members duplicate_member
  ON duplicate_member.authorization_sha256 = authorization_row.authorization_sha256
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
        OR member.avionics_manufacturer_id_snapshot <> model.avionics_manufacturer_id
        OR member.effective_manufacturer_identity_id_snapshot
          IS DISTINCT FROM manufacturer_identity.avionics_manufacturer_identity_id
        OR member.effective_manufacturer_identity_id_snapshot
          <> authorization_row.effective_manufacturer_identity_id_snapshot
        OR member.manufacturer_name_snapshot <> manufacturer.name
        OR member.stored_manufacturer_key_snapshot <> manufacturer.normalized_name
        OR member.model_name_snapshot <> model.name
        OR member.stored_model_key_snapshot <> model.normalized_name
        OR member.canonical_model_key_snapshot <> model.normalized_name
        OR member.canonical_model_key_snapshot
          <> authorization_row.canonical_model_key_snapshot
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
      AND current_model.normalized_name
        = authorization_row.canonical_model_key_snapshot
  ) = authorization_row.expected_member_count
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models current_model
    JOIN avionics_manufacturer_effective_memberships current_identity
      ON current_identity.avionics_manufacturer_id
        = current_model.avionics_manufacturer_id
    WHERE current_identity.avionics_manufacturer_identity_id
        = authorization_row.effective_manufacturer_identity_id_snapshot
      AND current_model.normalized_name
        = authorization_row.canonical_model_key_snapshot
      AND NOT EXISTS (
        SELECT 1
        FROM avionics_catalog_human_consolidation_members member
        WHERE member.authorization_sha256 = authorization_row.authorization_sha256
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
    WHERE left_member.authorization_sha256 = authorization_row.authorization_sha256
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

CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_guard (
  duplicate_model_id BIGINT PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id BIGINT NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  authorization_sha256 TEXT NOT NULL
    REFERENCES avionics_catalog_human_consolidation_authorizations(
      authorization_sha256
    ) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (duplicate_model_id <> survivor_model_id)
);

CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_claim (
  authorization_sha256 TEXT PRIMARY KEY
    REFERENCES avionics_catalog_human_consolidation_authorizations(
      authorization_sha256
    ) ON DELETE RESTRICT,
  survivor_model_id BIGINT NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE OR REPLACE FUNCTION validate_human_avionics_consolidation_guard()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF EXISTS (
      SELECT 1
      FROM avionics_catalog_human_consolidation_claim claim
      WHERE claim.authorization_sha256 = NEW.authorization_sha256
    )
    OR NOT EXISTS (
      SELECT 1
      FROM avionics_catalog_valid_human_consolidation_pairs valid
      WHERE valid.authorization_sha256 = NEW.authorization_sha256
        AND valid.duplicate_model_id = NEW.duplicate_model_id
        AND valid.survivor_model_id = NEW.survivor_model_id
    )
  THEN
    RAISE EXCEPTION 'human consolidation guard requires a complete current authorization';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_catalog_human_consolidation_guard_validate_insert
  ON avionics_catalog_human_consolidation_guard;
CREATE TRIGGER avionics_catalog_human_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_guard
FOR EACH ROW EXECUTE FUNCTION validate_human_avionics_consolidation_guard();

CREATE OR REPLACE FUNCTION preserve_human_avionics_consolidation_transient()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'active human avionics consolidation rows are immutable';
  RETURN NULL;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_catalog_human_consolidation_guard_immutable
  ON avionics_catalog_human_consolidation_guard;
CREATE TRIGGER avionics_catalog_human_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_guard
FOR EACH ROW EXECUTE FUNCTION preserve_human_avionics_consolidation_transient();

CREATE OR REPLACE FUNCTION validate_human_avionics_consolidation_claim()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_human_consolidation_authorizations authorization_row
    WHERE authorization_row.authorization_sha256 = NEW.authorization_sha256
      AND authorization_row.survivor_model_id_snapshot = NEW.survivor_model_id
      AND (
        SELECT COUNT(*)
        FROM avionics_catalog_human_consolidation_guard guard
        WHERE guard.authorization_sha256 = NEW.authorization_sha256
          AND guard.survivor_model_id = NEW.survivor_model_id
      ) = authorization_row.expected_member_count - 1
      AND NOT EXISTS (
        SELECT 1
        FROM avionics_catalog_valid_human_consolidation_pairs required_pair
        WHERE required_pair.authorization_sha256 = NEW.authorization_sha256
          AND NOT EXISTS (
            SELECT 1
            FROM avionics_catalog_human_consolidation_guard guard
            WHERE guard.authorization_sha256 = required_pair.authorization_sha256
              AND guard.duplicate_model_id = required_pair.duplicate_model_id
              AND guard.survivor_model_id = required_pair.survivor_model_id
          )
      )
  ) THEN
    RAISE EXCEPTION 'human consolidation claim requires every complete current guard pair';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_catalog_human_consolidation_claim_validate_insert
  ON avionics_catalog_human_consolidation_claim;
CREATE TRIGGER avionics_catalog_human_consolidation_claim_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_claim
FOR EACH ROW EXECUTE FUNCTION validate_human_avionics_consolidation_claim();

DROP TRIGGER IF EXISTS avionics_catalog_human_consolidation_claim_immutable
  ON avionics_catalog_human_consolidation_claim;
CREATE TRIGGER avionics_catalog_human_consolidation_claim_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_claim
FOR EACH ROW EXECUTE FUNCTION preserve_human_avionics_consolidation_transient();

-- Preserve the exact legacy identifier-only definition already installed by
-- the 20260725 contract, then add only the active human full-set claim. This
-- avoids duplicating or weakening the older authorization predicate.
DO $authorized_view_patch$
DECLARE
  legacy_definition TEXT;
BEGIN
  legacy_definition :=
    pg_get_viewdef('avionics_catalog_authorized_consolidations'::regclass, TRUE);
  IF STRPOS(
    legacy_definition,
    'avionics_catalog_human_consolidation_guard'
  ) = 0 THEN
    legacy_definition := REGEXP_REPLACE(legacy_definition, ';\s*$', '');
    EXECUTE
      'CREATE OR REPLACE VIEW avionics_catalog_authorized_consolidations AS '
      || legacy_definition
      || ' UNION ALL '
      || 'SELECT human_guard.duplicate_model_id, human_guard.survivor_model_id '
      || 'FROM avionics_catalog_human_consolidation_guard human_guard '
      || 'JOIN avionics_catalog_human_consolidation_claim claim '
      || 'ON claim.authorization_sha256 = human_guard.authorization_sha256 '
      || 'AND claim.survivor_model_id = human_guard.survivor_model_id';
  END IF;
END
$authorized_view_patch$;

CREATE OR REPLACE FUNCTION preserve_guarded_avionics_consolidation_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF EXISTS (
    SELECT 1 FROM avionics_catalog_consolidation_guard guard
    WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
    UNION ALL
    SELECT 1 FROM avionics_catalog_human_consolidation_guard guard
    WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
  ) THEN
    RAISE EXCEPTION 'guarded avionics consolidation identities are immutable';
  END IF;
  RETURN NEW;
END;
$function$;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260731_avionics_human_reviewed_consolidation',
  1,
  '93a641a0f653eacf0c8413bdb697a35c588fe34efc1419d30bf65146c8b2d55a',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
