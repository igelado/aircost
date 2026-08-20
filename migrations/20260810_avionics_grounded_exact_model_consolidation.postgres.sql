-- Allow a current, complete grounded collision review to consolidate only
-- exact stored-model duplicates. The guard remains transient and contains no
-- Gemini prompt, response, or URL-context dossier.

BEGIN;

CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

LOCK TABLE
  avionics_models,
  avionics_manufacturer_identity_memberships,
  avionics_manufacturer_identity_merges,
  avionics_catalog_consolidation_guard
IN ACCESS EXCLUSIVE MODE;

DO $migration_guard$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260808_avionics_descriptive_consolidation'
      AND contract_version = 1
      AND contract_fingerprint =
        '3aacf958efa7fb5e24c5897cf0369d40cb506b2a22444d629ea0a76462ce1a70'
  ) OR EXISTS (
    SELECT 1 FROM avionics_catalog_consolidation_guard
  ) OR EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name =
          '20260810_avionics_grounded_exact_model_consolidation'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          '36f9ff06bf42fc769508ecfe578f4b4a11f2e0072b81efebed1dee8958654f2a'
      )
  ) THEN
    RAISE EXCEPTION
      'installed grounded exact-model consolidation migration has a different contract, is missing a required predecessor, or found a leaked transient guard';
  END IF;
END
$migration_guard$;

ALTER TABLE avionics_catalog_consolidation_guard
  DROP CONSTRAINT IF EXISTS avionics_catalog_consolidation_guard_purpose_check;
ALTER TABLE avionics_catalog_consolidation_guard
  ADD CONSTRAINT avionics_catalog_consolidation_guard_purpose_check
  CHECK (purpose = 'legacy_identity_consolidation');

CREATE TABLE IF NOT EXISTS avionics_catalog_grounded_consolidation_authorizations (
  authorization_sha256 TEXT PRIMARY KEY CHECK (authorization_sha256 ~ '^[0-9a-f]{64}$'),
  survivor_model_id BIGINT NOT NULL REFERENCES avionics_models(id) ON DELETE RESTRICT,
  effective_manufacturer_identity_id BIGINT NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  normalized_model_key TEXT NOT NULL CHECK (BTRIM(normalized_model_key) <> ''),
  expected_member_count BIGINT NOT NULL CHECK (expected_member_count >= 2),
  reviewed_catalog_fingerprint TEXT NOT NULL CHECK (reviewed_catalog_fingerprint ~ '^[0-9a-f]{64}$'),
  manufacturer_collision_snapshot_sha256 TEXT NOT NULL
    CHECK (manufacturer_collision_snapshot_sha256 ~ '^[0-9a-f]{64}$'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS avionics_catalog_grounded_consolidation_guard (
  duplicate_model_id BIGINT PRIMARY KEY REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id BIGINT NOT NULL REFERENCES avionics_models(id) ON DELETE RESTRICT,
  authorization_sha256 TEXT NOT NULL
    REFERENCES avionics_catalog_grounded_consolidation_authorizations(authorization_sha256)
    ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (duplicate_model_id <> survivor_model_id)
);
CREATE TABLE IF NOT EXISTS avionics_catalog_grounded_consolidation_claim (
  authorization_sha256 TEXT PRIMARY KEY
    REFERENCES avionics_catalog_grounded_consolidation_authorizations(authorization_sha256)
    ON DELETE RESTRICT,
  survivor_model_id BIGINT NOT NULL REFERENCES avionics_models(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE OR REPLACE FUNCTION validate_grounded_avionics_consolidation_authorization()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM avionics_models survivor
    JOIN avionics_manufacturer_effective_memberships survivor_identity
      ON survivor_identity.avionics_manufacturer_id = survivor.avionics_manufacturer_id
    WHERE survivor.id = NEW.survivor_model_id
      AND survivor.catalog_status = 'unreviewed'
      AND survivor_identity.avionics_manufacturer_identity_id = NEW.effective_manufacturer_identity_id
      AND survivor.normalized_name = NEW.normalized_model_key
      AND (SELECT COUNT(*) FROM avionics_models member
           JOIN avionics_manufacturer_effective_memberships member_identity
             ON member_identity.avionics_manufacturer_id = member.avionics_manufacturer_id
           WHERE member_identity.avionics_manufacturer_identity_id = NEW.effective_manufacturer_identity_id
             AND member.normalized_name = NEW.normalized_model_key) = NEW.expected_member_count
  ) THEN
    RAISE EXCEPTION 'grounded consolidation authorization requires the complete current exact-model group';
  END IF;
  RETURN NEW;
END;
$function$;
DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_authorization_validate_
ON avionics_catalog_grounded_consolidation_authorizations;
CREATE TRIGGER avionics_catalog_grounded_consolidation_authorization_validate_
BEFORE INSERT ON avionics_catalog_grounded_consolidation_authorizations
FOR EACH ROW EXECUTE FUNCTION validate_grounded_avionics_consolidation_authorization();

CREATE OR REPLACE FUNCTION preserve_grounded_avionics_consolidation_transient()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'grounded avionics consolidation rows are immutable';
  RETURN NULL;
END;
$function$;
DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_authorization_immutable
  ON avionics_catalog_grounded_consolidation_authorizations;
CREATE TRIGGER avionics_catalog_grounded_consolidation_authorization_immutable
BEFORE UPDATE ON avionics_catalog_grounded_consolidation_authorizations
FOR EACH ROW EXECUTE FUNCTION preserve_grounded_avionics_consolidation_transient();

CREATE OR REPLACE FUNCTION validate_grounded_avionics_consolidation_guard()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF EXISTS (SELECT 1 FROM avionics_catalog_grounded_consolidation_claim claim
             WHERE claim.authorization_sha256 = NEW.authorization_sha256)
    OR NOT EXISTS (
      SELECT 1 FROM avionics_catalog_grounded_consolidation_authorizations authorization_row
      JOIN avionics_models duplicate ON duplicate.id = NEW.duplicate_model_id
      JOIN avionics_models survivor ON survivor.id = NEW.survivor_model_id
      JOIN avionics_manufacturer_effective_memberships duplicate_identity
        ON duplicate_identity.avionics_manufacturer_id = duplicate.avionics_manufacturer_id
      JOIN avionics_manufacturer_effective_memberships survivor_identity
        ON survivor_identity.avionics_manufacturer_id = survivor.avionics_manufacturer_id
      WHERE authorization_row.authorization_sha256 = NEW.authorization_sha256
        AND authorization_row.survivor_model_id = NEW.survivor_model_id
        AND duplicate.catalog_status = 'unreviewed' AND survivor.catalog_status = 'unreviewed'
        AND duplicate_identity.avionics_manufacturer_identity_id = authorization_row.effective_manufacturer_identity_id
        AND survivor_identity.avionics_manufacturer_identity_id = authorization_row.effective_manufacturer_identity_id
        AND duplicate.normalized_name = authorization_row.normalized_model_key
        AND survivor.normalized_name = authorization_row.normalized_model_key
        AND (duplicate.manufacturer_identifier_kind IS NULL
          OR survivor.manufacturer_identifier_kind IS NULL
          OR (duplicate.manufacturer_identifier_kind = survivor.manufacturer_identifier_kind
            AND LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(BTRIM(duplicate.normalized_manufacturer_identifier),' ',''),'-',''),'/',''),'.',''),'_',''))
              = LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(BTRIM(survivor.normalized_manufacturer_identifier),' ',''),'-',''),'/',''),'.',''),'_',''))))
        AND (SELECT COUNT(*) FROM avionics_catalog_grounded_consolidation_guard existing
             WHERE existing.authorization_sha256 = NEW.authorization_sha256)
            < authorization_row.expected_member_count - 1
    ) THEN
    RAISE EXCEPTION 'grounded consolidation guard requires an inactive complete-group authorization and an exact current pair';
  END IF;
  RETURN NEW;
END;
$function$;
DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_guard_validate_insert
  ON avionics_catalog_grounded_consolidation_guard;
CREATE TRIGGER avionics_catalog_grounded_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_grounded_consolidation_guard
FOR EACH ROW EXECUTE FUNCTION validate_grounded_avionics_consolidation_guard();
DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_guard_immutable
  ON avionics_catalog_grounded_consolidation_guard;
CREATE TRIGGER avionics_catalog_grounded_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_grounded_consolidation_guard
FOR EACH ROW EXECUTE FUNCTION preserve_grounded_avionics_consolidation_transient();

CREATE OR REPLACE VIEW avionics_catalog_valid_grounded_consolidation_pairs AS
SELECT guard.authorization_sha256, guard.duplicate_model_id, guard.survivor_model_id
FROM avionics_catalog_grounded_consolidation_guard guard
JOIN avionics_catalog_grounded_consolidation_authorizations authorization_row
  ON authorization_row.authorization_sha256 = guard.authorization_sha256
WHERE (SELECT COUNT(*) FROM avionics_catalog_grounded_consolidation_guard sibling
       WHERE sibling.authorization_sha256 = authorization_row.authorization_sha256
         AND sibling.survivor_model_id = authorization_row.survivor_model_id)
      = authorization_row.expected_member_count - 1
  AND (SELECT COUNT(*) FROM avionics_models member
       JOIN avionics_manufacturer_effective_memberships member_identity
         ON member_identity.avionics_manufacturer_id = member.avionics_manufacturer_id
       WHERE member_identity.avionics_manufacturer_identity_id = authorization_row.effective_manufacturer_identity_id
         AND member.normalized_name = authorization_row.normalized_model_key)
      = authorization_row.expected_member_count
  AND NOT EXISTS (
    SELECT 1 FROM avionics_models member
    JOIN avionics_manufacturer_effective_memberships member_identity
      ON member_identity.avionics_manufacturer_id = member.avionics_manufacturer_id
    WHERE member_identity.avionics_manufacturer_identity_id = authorization_row.effective_manufacturer_identity_id
      AND member.normalized_name = authorization_row.normalized_model_key
      AND (member.catalog_status <> 'unreviewed'
        OR (member.id <> authorization_row.survivor_model_id
          AND NOT EXISTS (SELECT 1 FROM avionics_catalog_grounded_consolidation_guard required_guard
                          WHERE required_guard.authorization_sha256 = authorization_row.authorization_sha256
                            AND required_guard.duplicate_model_id = member.id
                            AND required_guard.survivor_model_id = authorization_row.survivor_model_id)))
  )
  AND NOT EXISTS (
    SELECT 1 FROM avionics_models left_model
    JOIN avionics_manufacturer_effective_memberships left_identity
      ON left_identity.avionics_manufacturer_id = left_model.avionics_manufacturer_id
    JOIN avionics_models right_model ON right_model.id > left_model.id
    JOIN avionics_manufacturer_effective_memberships right_identity
      ON right_identity.avionics_manufacturer_id = right_model.avionics_manufacturer_id
     AND right_identity.avionics_manufacturer_identity_id = left_identity.avionics_manufacturer_identity_id
    WHERE left_identity.avionics_manufacturer_identity_id = authorization_row.effective_manufacturer_identity_id
      AND left_model.normalized_name = authorization_row.normalized_model_key
      AND right_model.normalized_name = authorization_row.normalized_model_key
      AND left_model.manufacturer_identifier_kind IS NOT NULL
      AND right_model.manufacturer_identifier_kind IS NOT NULL
      AND (left_model.manufacturer_identifier_kind <> right_model.manufacturer_identifier_kind
        OR LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(BTRIM(left_model.normalized_manufacturer_identifier),' ',''),'-',''),'/',''),'.',''),'_',''))
          <> LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(BTRIM(right_model.normalized_manufacturer_identifier),' ',''),'-',''),'/',''),'.',''),'_','')))
  );

CREATE OR REPLACE FUNCTION validate_grounded_avionics_consolidation_claim()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM avionics_catalog_grounded_consolidation_authorizations authorization_row
    WHERE authorization_row.authorization_sha256 = NEW.authorization_sha256
      AND authorization_row.survivor_model_id = NEW.survivor_model_id
      AND (SELECT COUNT(*) FROM avionics_catalog_valid_grounded_consolidation_pairs valid
           WHERE valid.authorization_sha256 = NEW.authorization_sha256
             AND valid.survivor_model_id = NEW.survivor_model_id)
          = authorization_row.expected_member_count - 1
  ) THEN
    RAISE EXCEPTION 'grounded consolidation claim requires every member of the complete current exact-model group';
  END IF;
  RETURN NEW;
END;
$function$;
DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_claim_validate_insert
  ON avionics_catalog_grounded_consolidation_claim;
CREATE TRIGGER avionics_catalog_grounded_consolidation_claim_validate_insert
BEFORE INSERT ON avionics_catalog_grounded_consolidation_claim
FOR EACH ROW EXECUTE FUNCTION validate_grounded_avionics_consolidation_claim();
DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_claim_immutable
  ON avionics_catalog_grounded_consolidation_claim;
CREATE TRIGGER avionics_catalog_grounded_consolidation_claim_immutable
BEFORE UPDATE ON avionics_catalog_grounded_consolidation_claim
FOR EACH ROW EXECUTE FUNCTION preserve_grounded_avionics_consolidation_transient();

CREATE OR REPLACE FUNCTION validate_avionics_catalog_consolidation_guard()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NOT (
    NEW.purpose = 'legacy_identity_consolidation'
    AND EXISTS (
        SELECT 1
        FROM avionics_models duplicate
        JOIN avionics_models survivor ON survivor.id = NEW.survivor_model_id
        WHERE duplicate.id = NEW.duplicate_model_id
          AND duplicate.catalog_status IN ('unreviewed', 'approved')
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
                    AND BTRIM(candidate.decision_evidence_source_url) <> ''
                    AND candidate.decision_evidence_source_title IS NOT NULL
                    AND BTRIM(candidate.decision_evidence_source_title) <> ''
                    AND candidate.decision_evidence_text IS NOT NULL
                    AND BTRIM(candidate.decision_evidence_text) <> ''
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
          AND BTRIM(duplicate.manufacturer_identifier) <> ''
          AND duplicate.normalized_manufacturer_identifier IS NOT NULL
          AND BTRIM(duplicate.normalized_manufacturer_identifier) <> ''
          AND survivor.manufacturer_identifier IS NOT NULL
          AND BTRIM(survivor.manufacturer_identifier) <> ''
          AND survivor.normalized_manufacturer_identifier IS NOT NULL
          AND BTRIM(survivor.normalized_manufacturer_identifier) <> ''
          AND LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
            BTRIM(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
            = LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
              BTRIM(duplicate.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
          AND LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
            BTRIM(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
            = LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
              BTRIM(survivor.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
          AND LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
            BTRIM(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
            = LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
              BTRIM(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    )
  ) THEN
    RAISE EXCEPTION 'consolidation guard pair does not satisfy its declared identity authority';
  END IF;
  RETURN NEW;
END;
$function$;

CREATE OR REPLACE VIEW avionics_catalog_authorized_consolidations AS
SELECT guard.duplicate_model_id, guard.survivor_model_id
FROM avionics_catalog_consolidation_guard guard
JOIN avionics_models duplicate ON duplicate.id = guard.duplicate_model_id
JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
WHERE guard.purpose = 'legacy_identity_consolidation'
  AND duplicate.catalog_status IN ('unreviewed', 'approved')
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
            AND BTRIM(candidate.decision_evidence_source_url) <> ''
            AND candidate.decision_evidence_source_title IS NOT NULL
            AND BTRIM(candidate.decision_evidence_source_title) <> ''
            AND candidate.decision_evidence_text IS NOT NULL
            AND BTRIM(candidate.decision_evidence_text) <> ''
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
  AND BTRIM(duplicate.manufacturer_identifier) <> ''
  AND duplicate.normalized_manufacturer_identifier IS NOT NULL
  AND BTRIM(duplicate.normalized_manufacturer_identifier) <> ''
  AND survivor.manufacturer_identifier IS NOT NULL
  AND BTRIM(survivor.manufacturer_identifier) <> ''
  AND survivor.normalized_manufacturer_identifier IS NOT NULL
  AND BTRIM(survivor.normalized_manufacturer_identifier) <> ''
  AND LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
    BTRIM(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    = LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
      BTRIM(duplicate.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  AND LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
    BTRIM(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    = LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
      BTRIM(survivor.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  AND LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
    BTRIM(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    = LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
      BTRIM(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
UNION ALL
SELECT grounded_guard.duplicate_model_id, grounded_guard.survivor_model_id
FROM avionics_catalog_grounded_consolidation_guard grounded_guard
JOIN avionics_catalog_grounded_consolidation_claim claim
  ON claim.authorization_sha256 = grounded_guard.authorization_sha256
 AND claim.survivor_model_id = grounded_guard.survivor_model_id
UNION ALL
SELECT human_guard.duplicate_model_id, human_guard.survivor_model_id
FROM avionics_catalog_human_consolidation_guard human_guard
JOIN avionics_catalog_human_consolidation_claim claim
 ON claim.authorization_sha256 = human_guard.authorization_sha256
 AND claim.survivor_model_id = human_guard.survivor_model_id;

CREATE OR REPLACE FUNCTION preserve_guarded_avionics_consolidation_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF EXISTS (
    SELECT 1 FROM avionics_catalog_consolidation_guard guard
    WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
    UNION ALL
    SELECT 1 FROM avionics_catalog_grounded_consolidation_guard grounded_guard
    WHERE grounded_guard.duplicate_model_id = OLD.id
       OR grounded_guard.survivor_model_id = OLD.id
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
  '20260810_avionics_grounded_exact_model_consolidation',
  1,
  '36f9ff06bf42fc769508ecfe578f4b4a11f2e0072b81efebed1dee8958654f2a',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
