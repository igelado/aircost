-- Allow a current, complete grounded collision review to consolidate only
-- exact stored-model duplicates. The guard remains transient and contains no
-- Gemini prompt, response, or URL-context dossier.

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

DROP TABLE IF EXISTS temp.avionics_grounded_exact_consolidation_migration_guard;
CREATE TEMP TABLE avionics_grounded_exact_consolidation_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_grounded_exact_consolidation_migration_guard (accepted)
SELECT CASE
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260808_avionics_descriptive_consolidation'
      AND contract_version = 1
      AND contract_fingerprint =
        '3aacf958efa7fb5e24c5897cf0369d40cb506b2a22444d629ea0a76462ce1a70'
  )
  AND NOT EXISTS (
    SELECT 1 FROM avionics_catalog_consolidation_guard
  )
  AND (
    NOT EXISTS (
      SELECT 1
      FROM schema_migration_contracts
      WHERE migration_name =
            '20260810_avionics_grounded_exact_model_consolidation'
    )
    OR EXISTS (
      SELECT 1
      FROM schema_migration_contracts
      WHERE migration_name =
            '20260810_avionics_grounded_exact_model_consolidation'
        AND contract_version = 1
        AND contract_fingerprint =
          '36f9ff06bf42fc769508ecfe578f4b4a11f2e0072b81efebed1dee8958654f2a'
    )
  ) THEN 1
  ELSE 0
END;
DROP TABLE avionics_grounded_exact_consolidation_migration_guard;

CREATE TABLE IF NOT EXISTS
  avionics_catalog_grounded_consolidation_authorizations (
  authorization_sha256 TEXT PRIMARY KEY,
  survivor_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE RESTRICT,
  effective_manufacturer_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  normalized_model_key TEXT NOT NULL,
  expected_member_count INTEGER NOT NULL CHECK (expected_member_count >= 2),
  reviewed_catalog_fingerprint TEXT NOT NULL,
  manufacturer_collision_snapshot_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(authorization_sha256) = 64 AND authorization_sha256 = lower(authorization_sha256)
         AND authorization_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(trim(normalized_model_key)) > 0),
  CHECK (length(reviewed_catalog_fingerprint) = 64
         AND reviewed_catalog_fingerprint = lower(reviewed_catalog_fingerprint)
         AND reviewed_catalog_fingerprint NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(manufacturer_collision_snapshot_sha256) = 64
         AND manufacturer_collision_snapshot_sha256 = lower(manufacturer_collision_snapshot_sha256)
         AND manufacturer_collision_snapshot_sha256 NOT GLOB '*[^0-9a-f]*')
);

DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_authorization_validate_insert;
CREATE TRIGGER avionics_catalog_grounded_consolidation_authorization_validate_insert
BEFORE INSERT ON avionics_catalog_grounded_consolidation_authorizations
WHEN NOT EXISTS (
  SELECT 1 FROM avionics_models survivor
  JOIN avionics_manufacturer_effective_memberships survivor_identity
    ON survivor_identity.avionics_manufacturer_id = survivor.avionics_manufacturer_id
  WHERE survivor.id = NEW.survivor_model_id
    AND survivor.catalog_status = 'unreviewed'
    AND survivor_identity.avionics_manufacturer_identity_id = NEW.effective_manufacturer_identity_id
    AND survivor.normalized_name = NEW.normalized_model_key
    AND (SELECT count(*) FROM avionics_models member
         JOIN avionics_manufacturer_effective_memberships member_identity
           ON member_identity.avionics_manufacturer_id = member.avionics_manufacturer_id
         WHERE member_identity.avionics_manufacturer_identity_id = NEW.effective_manufacturer_identity_id
           AND member.normalized_name = NEW.normalized_model_key) = NEW.expected_member_count
)
BEGIN
  SELECT RAISE(ABORT, 'grounded consolidation authorization requires the complete current exact-model group');
END;

DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_authorization_immutable;
CREATE TRIGGER avionics_catalog_grounded_consolidation_authorization_immutable
BEFORE UPDATE ON avionics_catalog_grounded_consolidation_authorizations
BEGIN SELECT RAISE(ABORT, 'grounded consolidation authorizations are immutable'); END;

CREATE TABLE IF NOT EXISTS avionics_catalog_grounded_consolidation_guard (
  duplicate_model_id INTEGER PRIMARY KEY REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE RESTRICT,
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
  survivor_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_guard_validate_insert;
CREATE TRIGGER avionics_catalog_grounded_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_grounded_consolidation_guard
WHEN EXISTS (SELECT 1 FROM avionics_catalog_grounded_consolidation_claim claim
             WHERE claim.authorization_sha256 = NEW.authorization_sha256)
  OR NOT EXISTS (
    SELECT 1 FROM avionics_catalog_grounded_consolidation_authorizations authorization
    JOIN avionics_models duplicate ON duplicate.id = NEW.duplicate_model_id
    JOIN avionics_models survivor ON survivor.id = NEW.survivor_model_id
    JOIN avionics_manufacturer_effective_memberships duplicate_identity
      ON duplicate_identity.avionics_manufacturer_id = duplicate.avionics_manufacturer_id
    JOIN avionics_manufacturer_effective_memberships survivor_identity
      ON survivor_identity.avionics_manufacturer_id = survivor.avionics_manufacturer_id
    WHERE authorization.authorization_sha256 = NEW.authorization_sha256
      AND authorization.survivor_model_id = NEW.survivor_model_id
      AND duplicate.catalog_status = 'unreviewed' AND survivor.catalog_status = 'unreviewed'
      AND duplicate_identity.avionics_manufacturer_identity_id = authorization.effective_manufacturer_identity_id
      AND survivor_identity.avionics_manufacturer_identity_id = authorization.effective_manufacturer_identity_id
      AND duplicate.normalized_name = authorization.normalized_model_key
      AND survivor.normalized_name = authorization.normalized_model_key
      AND (duplicate.manufacturer_identifier_kind IS NULL
        OR survivor.manufacturer_identifier_kind IS NULL
        OR (duplicate.manufacturer_identifier_kind = survivor.manufacturer_identifier_kind
          AND lower(replace(replace(replace(replace(replace(trim(duplicate.normalized_manufacturer_identifier),' ',''),'-',''),'/',''),'.',''),'_',''))
            = lower(replace(replace(replace(replace(replace(trim(survivor.normalized_manufacturer_identifier),' ',''),'-',''),'/',''),'.',''),'_',''))))
      AND (SELECT count(*) FROM avionics_catalog_grounded_consolidation_guard existing
           WHERE existing.authorization_sha256 = NEW.authorization_sha256)
          < authorization.expected_member_count - 1
  )
BEGIN
  SELECT RAISE(ABORT, 'grounded consolidation guard requires an inactive complete-group authorization and an exact current pair');
END;

DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_guard_immutable;
CREATE TRIGGER avionics_catalog_grounded_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_grounded_consolidation_guard
BEGIN SELECT RAISE(ABORT, 'grounded consolidation guard pairs are immutable'); END;

DROP VIEW IF EXISTS avionics_catalog_valid_grounded_consolidation_pairs;
CREATE VIEW avionics_catalog_valid_grounded_consolidation_pairs AS
SELECT guard.authorization_sha256, guard.duplicate_model_id, guard.survivor_model_id
FROM avionics_catalog_grounded_consolidation_guard guard
JOIN avionics_catalog_grounded_consolidation_authorizations authorization
  ON authorization.authorization_sha256 = guard.authorization_sha256
WHERE (SELECT count(*) FROM avionics_catalog_grounded_consolidation_guard sibling
       WHERE sibling.authorization_sha256 = authorization.authorization_sha256
         AND sibling.survivor_model_id = authorization.survivor_model_id)
      = authorization.expected_member_count - 1
  AND (SELECT count(*) FROM avionics_models member
       JOIN avionics_manufacturer_effective_memberships member_identity
         ON member_identity.avionics_manufacturer_id = member.avionics_manufacturer_id
       WHERE member_identity.avionics_manufacturer_identity_id = authorization.effective_manufacturer_identity_id
         AND member.normalized_name = authorization.normalized_model_key)
      = authorization.expected_member_count
  AND NOT EXISTS (
    SELECT 1 FROM avionics_models member
    JOIN avionics_manufacturer_effective_memberships member_identity
      ON member_identity.avionics_manufacturer_id = member.avionics_manufacturer_id
    WHERE member_identity.avionics_manufacturer_identity_id = authorization.effective_manufacturer_identity_id
      AND member.normalized_name = authorization.normalized_model_key
      AND (member.catalog_status <> 'unreviewed'
        OR (member.id <> authorization.survivor_model_id
          AND NOT EXISTS (SELECT 1 FROM avionics_catalog_grounded_consolidation_guard required_guard
                          WHERE required_guard.authorization_sha256 = authorization.authorization_sha256
                            AND required_guard.duplicate_model_id = member.id
                            AND required_guard.survivor_model_id = authorization.survivor_model_id)))
  )
  AND NOT EXISTS (
    SELECT 1 FROM avionics_models left_model
    JOIN avionics_manufacturer_effective_memberships left_identity
      ON left_identity.avionics_manufacturer_id = left_model.avionics_manufacturer_id
    JOIN avionics_models right_model ON right_model.id > left_model.id
    JOIN avionics_manufacturer_effective_memberships right_identity
      ON right_identity.avionics_manufacturer_id = right_model.avionics_manufacturer_id
     AND right_identity.avionics_manufacturer_identity_id = left_identity.avionics_manufacturer_identity_id
    WHERE left_identity.avionics_manufacturer_identity_id = authorization.effective_manufacturer_identity_id
      AND left_model.normalized_name = authorization.normalized_model_key
      AND right_model.normalized_name = authorization.normalized_model_key
      AND left_model.manufacturer_identifier_kind IS NOT NULL
      AND right_model.manufacturer_identifier_kind IS NOT NULL
      AND (left_model.manufacturer_identifier_kind <> right_model.manufacturer_identifier_kind
        OR lower(replace(replace(replace(replace(replace(trim(left_model.normalized_manufacturer_identifier),' ',''),'-',''),'/',''),'.',''),'_',''))
          <> lower(replace(replace(replace(replace(replace(trim(right_model.normalized_manufacturer_identifier),' ',''),'-',''),'/',''),'.',''),'_','')))
  );

DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_claim_validate_insert;
CREATE TRIGGER avionics_catalog_grounded_consolidation_claim_validate_insert
BEFORE INSERT ON avionics_catalog_grounded_consolidation_claim
WHEN NOT EXISTS (
  SELECT 1 FROM avionics_catalog_grounded_consolidation_authorizations authorization
  WHERE authorization.authorization_sha256 = NEW.authorization_sha256
    AND authorization.survivor_model_id = NEW.survivor_model_id
    AND (SELECT count(*) FROM avionics_catalog_valid_grounded_consolidation_pairs valid
         WHERE valid.authorization_sha256 = NEW.authorization_sha256
           AND valid.survivor_model_id = NEW.survivor_model_id)
        = authorization.expected_member_count - 1
)
BEGIN
  SELECT RAISE(ABORT, 'grounded consolidation claim requires every member of the complete current exact-model group');
END;

DROP TRIGGER IF EXISTS avionics_catalog_grounded_consolidation_claim_immutable;
CREATE TRIGGER avionics_catalog_grounded_consolidation_claim_immutable
BEFORE UPDATE ON avionics_catalog_grounded_consolidation_claim
BEGIN SELECT RAISE(ABORT, 'active grounded consolidation claims are immutable'); END;

DROP TABLE IF EXISTS temp.avionics_grounded_transient_state_guard;
CREATE TEMP TABLE avionics_grounded_transient_state_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_grounded_transient_state_guard (accepted)
SELECT CASE WHEN
  (SELECT count(*) FROM avionics_catalog_grounded_consolidation_authorizations)
  + (SELECT count(*) FROM avionics_catalog_grounded_consolidation_guard)
  + (SELECT count(*) FROM avionics_catalog_grounded_consolidation_claim) = 0
THEN 1 ELSE 0 END;
DROP TABLE avionics_grounded_transient_state_guard;

-- Rebuild the legacy guard to remove any pre-release widened purpose contract.
-- A valid transient guard is always empty outside its owning transaction.
DROP VIEW IF EXISTS avionics_catalog_authorized_consolidations;
DROP TRIGGER IF EXISTS avionics_catalog_consolidation_guard_validate_insert;
DROP TRIGGER IF EXISTS avionics_catalog_consolidation_guard_immutable;
DROP TABLE avionics_catalog_consolidation_guard;

CREATE TABLE avionics_catalog_consolidation_guard (
  duplicate_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  purpose TEXT NOT NULL DEFAULT 'legacy_identity_consolidation'
    CHECK (purpose = 'legacy_identity_consolidation'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (duplicate_model_id <> survivor_model_id)
);

CREATE TRIGGER avionics_catalog_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_consolidation_guard
WHEN NOT (
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
  )
)
BEGIN
  SELECT RAISE(ABORT, 'consolidation guard pair does not satisfy its declared identity authority');
END;

CREATE TRIGGER avionics_catalog_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_consolidation_guard
BEGIN
  SELECT RAISE(ABORT, 'consolidation authorization pairs are immutable');
END;

CREATE VIEW avionics_catalog_authorized_consolidations AS
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
  SELECT 1 FROM avionics_catalog_grounded_consolidation_guard grounded_guard
  WHERE grounded_guard.duplicate_model_id = OLD.id
     OR grounded_guard.survivor_model_id = OLD.id
  UNION ALL
  SELECT 1 FROM avionics_catalog_human_consolidation_guard guard
  WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'guarded avionics consolidation identities are immutable');
END;

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
