-- Evidence-backed human-reviewed avionics consolidation.
--
-- Automatic consolidation remains stable-identifier-only. This migration adds
-- a separate, immutable reviewer authorization whose exact current catalog
-- snapshots are revalidated before one short-lived full-set claim can activate
-- duplicate-to-survivor remaps.

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

DROP TABLE IF EXISTS temp.avionics_human_consolidation_migration_guard;
CREATE TEMP TABLE avionics_human_consolidation_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_human_consolidation_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260731_avionics_human_reviewed_consolidation'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260731_avionics_human_reviewed_consolidation'
      AND contract_version = 1
      AND contract_fingerprint =
        '93a641a0f653eacf0c8413bdb697a35c588fe34efc1419d30bf65146c8b2d55a'
  ) THEN 1
  ELSE 0
END;
DROP TABLE avionics_human_consolidation_migration_guard;

CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_authorizations (
  authorization_sha256 TEXT PRIMARY KEY,
  reviewer_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  survivor_model_id_snapshot INTEGER NOT NULL,
  effective_manufacturer_identity_id_snapshot INTEGER NOT NULL,
  canonical_model_key_snapshot TEXT NOT NULL,
  expected_member_count INTEGER NOT NULL,
  authoritative_source_url TEXT NOT NULL,
  authoritative_source_title TEXT NOT NULL,
  exact_evidence_text TEXT NOT NULL,
  provenance_listing_id_snapshot INTEGER,
  provenance_pending_review_id_snapshot INTEGER,
  provenance_review_payload_sha256 TEXT,
  provenance_review_aspect_id TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(authorization_sha256) = 64),
  CHECK (authorization_sha256 = lower(authorization_sha256)),
  CHECK (authorization_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (survivor_model_id_snapshot > 0),
  CHECK (effective_manufacturer_identity_id_snapshot > 0),
  CHECK (length(trim(canonical_model_key_snapshot)) > 0),
  CHECK (canonical_model_key_snapshot = lower(canonical_model_key_snapshot)),
  CHECK (canonical_model_key_snapshot NOT GLOB '*[^a-z0-9 ]*'),
  CHECK (expected_member_count >= 2),
  CHECK (authoritative_source_url LIKE 'https://%'),
  CHECK (lower(authoritative_source_url) NOT LIKE '%/listing/%'),
  CHECK (lower(authoritative_source_url) NOT LIKE '%/listings/%'),
  CHECK (lower(authoritative_source_url) NOT LIKE '%/aircraft-for-sale/%'),
  CHECK (lower(authoritative_source_url) NOT LIKE '%/classifieds/%'),
  CHECK (length(trim(authoritative_source_title)) > 0),
  CHECK (length(trim(exact_evidence_text)) > 0),
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
      AND length(provenance_review_payload_sha256) = 64
      AND provenance_review_payload_sha256 = lower(provenance_review_payload_sha256)
      AND provenance_review_payload_sha256 NOT GLOB '*[^0-9a-f]*'
      AND length(trim(provenance_review_aspect_id)) > 0
    )
  )
);

CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_members (
  authorization_sha256 TEXT NOT NULL
    REFERENCES avionics_catalog_human_consolidation_authorizations(
      authorization_sha256
    ) ON DELETE RESTRICT,
  avionics_model_id_snapshot INTEGER NOT NULL,
  member_role TEXT NOT NULL CHECK (member_role IN ('survivor', 'duplicate')),
  row_identity_sha256 TEXT NOT NULL,
  avionics_manufacturer_id_snapshot INTEGER NOT NULL,
  effective_manufacturer_identity_id_snapshot INTEGER NOT NULL,
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
  CHECK (length(row_identity_sha256) = 64),
  CHECK (row_identity_sha256 = lower(row_identity_sha256)),
  CHECK (row_identity_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (avionics_manufacturer_id_snapshot > 0),
  CHECK (effective_manufacturer_identity_id_snapshot > 0),
  CHECK (length(trim(manufacturer_name_snapshot)) > 0),
  CHECK (length(trim(stored_manufacturer_key_snapshot)) > 0),
  CHECK (length(trim(model_name_snapshot)) > 0),
  CHECK (length(trim(stored_model_key_snapshot)) > 0),
  CHECK (length(trim(canonical_model_key_snapshot)) > 0),
  CHECK (
    (
      manufacturer_identifier_kind_snapshot IS NULL
      AND manufacturer_identifier_snapshot IS NULL
      AND normalized_manufacturer_identifier_snapshot IS NULL
    )
    OR (
      manufacturer_identifier_kind_snapshot IS NOT NULL
      AND manufacturer_identifier_snapshot IS NOT NULL
      AND length(trim(manufacturer_identifier_snapshot)) > 0
      AND normalized_manufacturer_identifier_snapshot IS NOT NULL
      AND length(trim(normalized_manufacturer_identifier_snapshot)) > 0
    )
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_avionics_human_consolidation_one_survivor
ON avionics_catalog_human_consolidation_members (authorization_sha256)
WHERE member_role = 'survivor';

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_authorizations_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_authorizations
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation authorizations are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_authorizations_preserve
BEFORE DELETE ON avionics_catalog_human_consolidation_authorizations
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation authorization audit is permanent');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_members_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_members
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_human_consolidation_authorizations authorization
  JOIN avionics_models model ON model.id = NEW.avionics_model_id_snapshot
  JOIN avionics_manufacturers manufacturer
    ON manufacturer.id = model.avionics_manufacturer_id
  JOIN avionics_manufacturer_effective_memberships manufacturer_identity
    ON manufacturer_identity.avionics_manufacturer_id
      = model.avionics_manufacturer_id
  WHERE authorization.authorization_sha256 = NEW.authorization_sha256
    AND (
      (NEW.member_role = 'survivor'
        AND NEW.avionics_model_id_snapshot
          = authorization.survivor_model_id_snapshot)
      OR
      (NEW.member_role = 'duplicate'
        AND NEW.avionics_model_id_snapshot
          <> authorization.survivor_model_id_snapshot)
    )
    AND (
      SELECT count(*)
      FROM avionics_catalog_human_consolidation_members existing
      WHERE existing.authorization_sha256 = NEW.authorization_sha256
    ) < authorization.expected_member_count
    AND NEW.effective_manufacturer_identity_id_snapshot
      = authorization.effective_manufacturer_identity_id_snapshot
    AND NEW.canonical_model_key_snapshot
      = authorization.canonical_model_key_snapshot
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
      IS model.manufacturer_identifier_kind
    AND NEW.manufacturer_identifier_snapshot IS model.manufacturer_identifier
    AND NEW.normalized_manufacturer_identifier_snapshot
      IS model.normalized_manufacturer_identifier
    AND NEW.identity_source_url_snapshot IS model.identity_source_url
    AND NEW.identity_source_title_snapshot IS model.identity_source_title
    AND NEW.identity_evidence_text_snapshot IS model.identity_evidence_text
    AND NEW.identity_evidence_kind_snapshot = model.identity_evidence_kind
    AND NEW.identity_confidence_snapshot IS model.identity_confidence
    AND NEW.catalog_reviewed_at_snapshot IS model.catalog_reviewed_at
)
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation member is not an exact current row snapshot');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_members_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_members
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation member snapshots are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_members_preserve
BEFORE DELETE ON avionics_catalog_human_consolidation_members
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation member audit is permanent');
END;

CREATE VIEW IF NOT EXISTS
  avionics_catalog_valid_human_consolidation_pairs AS
SELECT
  authorization.authorization_sha256,
  duplicate_member.avionics_model_id_snapshot AS duplicate_model_id,
  authorization.survivor_model_id_snapshot AS survivor_model_id
FROM avionics_catalog_human_consolidation_authorizations authorization
JOIN avionics_catalog_human_consolidation_members duplicate_member
  ON duplicate_member.authorization_sha256 = authorization.authorization_sha256
 AND duplicate_member.member_role = 'duplicate'
WHERE (
    SELECT count(*)
    FROM avionics_catalog_human_consolidation_members member
    WHERE member.authorization_sha256 = authorization.authorization_sha256
  ) = authorization.expected_member_count
  AND (
    SELECT count(*)
    FROM avionics_catalog_human_consolidation_members member
    WHERE member.authorization_sha256 = authorization.authorization_sha256
      AND member.member_role = 'survivor'
      AND member.avionics_model_id_snapshot
        = authorization.survivor_model_id_snapshot
  ) = 1
  AND (
    SELECT count(*)
    FROM avionics_catalog_human_consolidation_members member
    WHERE member.authorization_sha256 = authorization.authorization_sha256
      AND member.member_role = 'duplicate'
  ) = authorization.expected_member_count - 1
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
    WHERE member.authorization_sha256 = authorization.authorization_sha256
      AND (
        model.id IS NULL
        OR model.catalog_status <> 'unreviewed'
        OR member.avionics_manufacturer_id_snapshot
          <> model.avionics_manufacturer_id
        OR member.effective_manufacturer_identity_id_snapshot
          IS NOT manufacturer_identity.avionics_manufacturer_identity_id
        OR member.effective_manufacturer_identity_id_snapshot
          <> authorization.effective_manufacturer_identity_id_snapshot
        OR member.manufacturer_name_snapshot <> manufacturer.name
        OR member.stored_manufacturer_key_snapshot <> manufacturer.normalized_name
        OR member.model_name_snapshot <> model.name
        OR member.stored_model_key_snapshot <> model.normalized_name
        OR member.canonical_model_key_snapshot <> model.normalized_name
        OR member.canonical_model_key_snapshot
          <> authorization.canonical_model_key_snapshot
        OR member.catalog_status_snapshot <> model.catalog_status
        OR member.manufacturer_identifier_kind_snapshot
          IS NOT model.manufacturer_identifier_kind
        OR member.manufacturer_identifier_snapshot
          IS NOT model.manufacturer_identifier
        OR member.normalized_manufacturer_identifier_snapshot
          IS NOT model.normalized_manufacturer_identifier
        OR member.identity_source_url_snapshot IS NOT model.identity_source_url
        OR member.identity_source_title_snapshot IS NOT model.identity_source_title
        OR member.identity_evidence_text_snapshot IS NOT model.identity_evidence_text
        OR member.identity_evidence_kind_snapshot <> model.identity_evidence_kind
        OR member.identity_confidence_snapshot IS NOT model.identity_confidence
        OR member.catalog_reviewed_at_snapshot IS NOT model.catalog_reviewed_at
      )
  )
  AND (
    SELECT count(*)
    FROM avionics_models current_model
    JOIN avionics_manufacturer_effective_memberships current_identity
      ON current_identity.avionics_manufacturer_id
        = current_model.avionics_manufacturer_id
    WHERE current_identity.avionics_manufacturer_identity_id
        = authorization.effective_manufacturer_identity_id_snapshot
      AND current_model.normalized_name
        = authorization.canonical_model_key_snapshot
  ) = authorization.expected_member_count
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models current_model
    JOIN avionics_manufacturer_effective_memberships current_identity
      ON current_identity.avionics_manufacturer_id
        = current_model.avionics_manufacturer_id
    WHERE current_identity.avionics_manufacturer_identity_id
        = authorization.effective_manufacturer_identity_id_snapshot
      AND current_model.normalized_name
        = authorization.canonical_model_key_snapshot
      AND NOT EXISTS (
        SELECT 1
        FROM avionics_catalog_human_consolidation_members member
        WHERE member.authorization_sha256 = authorization.authorization_sha256
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
    WHERE left_member.authorization_sha256 = authorization.authorization_sha256
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
  duplicate_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id INTEGER NOT NULL
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
  survivor_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_guard
WHEN EXISTS (
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
BEGIN
  SELECT RAISE(ABORT, 'human consolidation guard requires a complete current authorization');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_guard
BEGIN
  SELECT RAISE(ABORT, 'human consolidation guard pairs are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_claim_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_claim
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_human_consolidation_authorizations authorization
  WHERE authorization.authorization_sha256 = NEW.authorization_sha256
    AND authorization.survivor_model_id_snapshot = NEW.survivor_model_id
    AND (
      SELECT count(*)
      FROM avionics_catalog_human_consolidation_guard guard
      WHERE guard.authorization_sha256 = NEW.authorization_sha256
        AND guard.survivor_model_id = NEW.survivor_model_id
    ) = authorization.expected_member_count - 1
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
)
BEGIN
  SELECT RAISE(ABORT, 'human consolidation claim requires every complete current guard pair');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_claim_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_claim
BEGIN
  SELECT RAISE(ABORT, 'active human consolidation claims are immutable');
END;

DROP VIEW IF EXISTS avionics_catalog_authorized_consolidations;
CREATE VIEW avionics_catalog_authorized_consolidations AS
SELECT guard.duplicate_model_id, guard.survivor_model_id
FROM avionics_catalog_consolidation_guard guard
JOIN avionics_models duplicate ON duplicate.id = guard.duplicate_model_id
JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
WHERE duplicate.catalog_status IN ('unreviewed', 'approved')
  AND survivor.catalog_status IN ('unreviewed', 'approved')
  AND (
    survivor.catalog_status = 'approved'
    OR duplicate.catalog_status = 'unreviewed'
  )
  AND EXISTS (
      SELECT 1
      FROM avionics_manufacturer_effective_memberships duplicate_identity
      JOIN avionics_manufacturer_effective_memberships survivor_identity
        ON survivor_identity.avionics_manufacturer_id
          = survivor.avionics_manufacturer_id
      WHERE duplicate_identity.avionics_manufacturer_id
          = duplicate.avionics_manufacturer_id
        AND (
          duplicate_identity.avionics_manufacturer_identity_id
            = survivor_identity.avionics_manufacturer_identity_id
          OR EXISTS (
            SELECT 1
            FROM avionics_manufacturer_alias_candidates candidate
            JOIN avionics_manufacturer_effective_memberships source_identity
              ON source_identity.avionics_manufacturer_id
                = candidate.avionics_manufacturer_id
            JOIN avionics_manufacturer_effective_identities target_identity
              ON target_identity.identity_id
                = candidate.candidate_manufacturer_identity_id
            WHERE candidate.review_status = 'approved'
              AND candidate.decision_evidence_source_url IS NOT NULL
              AND length(trim(candidate.decision_evidence_source_url)) > 0
              AND candidate.decision_evidence_source_title IS NOT NULL
              AND length(trim(candidate.decision_evidence_source_title)) > 0
              AND candidate.decision_evidence_text IS NOT NULL
              AND length(trim(candidate.decision_evidence_text)) > 0
              AND candidate.reviewed_by_user_id IS NOT NULL
              AND candidate.reviewed_at IS NOT NULL
              AND (
                (
                  source_identity.avionics_manufacturer_identity_id
                    = duplicate_identity.avionics_manufacturer_identity_id
                  AND target_identity.avionics_manufacturer_identity_id
                    = survivor_identity.avionics_manufacturer_identity_id
                )
                OR (
                  source_identity.avionics_manufacturer_identity_id
                    = survivor_identity.avionics_manufacturer_identity_id
                  AND target_identity.avionics_manufacturer_identity_id
                    = duplicate_identity.avionics_manufacturer_identity_id
                )
              )
          )
        )
  )
  AND duplicate.manufacturer_identifier_kind IS NOT NULL
  AND duplicate.manufacturer_identifier_kind
    = survivor.manufacturer_identifier_kind
  AND duplicate.manufacturer_identifier IS NOT NULL
  AND length(trim(duplicate.manufacturer_identifier)) > 0
  AND duplicate.normalized_manufacturer_identifier IS NOT NULL
  AND length(trim(duplicate.normalized_manufacturer_identifier)) > 0
  AND survivor.manufacturer_identifier IS NOT NULL
  AND length(trim(survivor.manufacturer_identifier)) > 0
  AND survivor.normalized_manufacturer_identifier IS NOT NULL
  AND length(trim(survivor.normalized_manufacturer_identifier)) > 0
  AND lower(replace(replace(replace(replace(replace(
    trim(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    = lower(replace(replace(replace(replace(replace(
      trim(duplicate.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  AND lower(replace(replace(replace(replace(replace(
    trim(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    = lower(replace(replace(replace(replace(replace(
      trim(survivor.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  AND lower(replace(replace(replace(replace(replace(
    trim(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    = lower(replace(replace(replace(replace(replace(
      trim(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
UNION ALL
SELECT human_guard.duplicate_model_id, human_guard.survivor_model_id
FROM avionics_catalog_human_consolidation_guard human_guard
JOIN avionics_catalog_human_consolidation_claim claim
  ON claim.authorization_sha256 = human_guard.authorization_sha256
 AND claim.survivor_model_id = human_guard.survivor_model_id;

DROP TRIGGER IF EXISTS avionics_models_consolidation_identity_immutable;
CREATE TRIGGER avionics_models_consolidation_identity_immutable
BEFORE UPDATE OF catalog_status, avionics_manufacturer_id, name,
  normalized_name, manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
ON avionics_models
WHEN EXISTS (
  SELECT 1 FROM avionics_catalog_consolidation_guard guard
  WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
  UNION ALL
  SELECT 1 FROM avionics_catalog_human_consolidation_guard guard
  WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'guarded avionics consolidation identities are immutable');
END;

INSERT INTO schema_migration_contracts (
  migration_name,
  contract_version,
  contract_fingerprint,
  installed_at
) VALUES (
  '20260731_avionics_human_reviewed_consolidation',
  1,
  '93a641a0f653eacf0c8413bdb697a35c588fe34efc1419d30bf65146c8b2d55a',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
PRAGMA foreign_key_check;
