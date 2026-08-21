-- Install canonical avionics identity postconditions and the narrowly-scoped
-- legacy consolidation guard. This migration intentionally permits existing
-- unreviewed collisions on non-ready listings so consolidation can run after
-- it is installed. Approved identities and ready listings are strict.

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

CREATE TEMP TABLE identity_deduplication_migration_contract_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO identity_deduplication_migration_contract_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260725_identity_deduplication_postconditions'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260725_identity_deduplication_postconditions'
      AND contract_version = 6
      AND contract_fingerprint =
        'cd001240b48a1480fd8bbee39b9ddedbba01d00fad45cbac315cec7a243cf133'
  ) THEN 1
  ELSE 0
END;
DROP TABLE identity_deduplication_migration_contract_guard;

-- Identifier namespaces are independent: a manufacturer's model number and
-- SKU may legitimately have the same normalized value.
DROP INDEX IF EXISTS idx_avionics_models_manufacturer_identifier;
CREATE UNIQUE INDEX idx_avionics_models_manufacturer_identifier
  ON avionics_models (
    avionics_manufacturer_id,
    manufacturer_identifier_kind,
    normalized_manufacturer_identifier
  )
  WHERE normalized_manufacturer_identifier IS NOT NULL
    AND length(trim(normalized_manufacturer_identifier)) > 0;

CREATE TABLE IF NOT EXISTS avionics_manufacturer_canonical_keys (
  avionics_manufacturer_id INTEGER PRIMARY KEY
    REFERENCES avionics_manufacturers(id) ON DELETE CASCADE,
  canonical_manufacturer_key TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (avionics_manufacturer_id, canonical_manufacturer_key),
  CHECK (length(canonical_manufacturer_key) > 0),
  CHECK (canonical_manufacturer_key = lower(canonical_manufacturer_key)),
  CHECK (canonical_manufacturer_key NOT GLOB '*[^a-z0-9]*')
);

CREATE INDEX IF NOT EXISTS idx_avionics_manufacturer_canonical_keys_lookup
  ON avionics_manufacturer_canonical_keys (canonical_manufacturer_key);

DROP VIEW IF EXISTS avionics_manufacturer_normalization_contract;
CREATE VIEW avionics_manufacturer_normalization_contract AS
WITH separated AS (
  SELECT
    manufacturer.id AS avionics_manufacturer_id,
    manufacturer.name,
    manufacturer.normalized_name,
    ' ' || lower(replace(replace(replace(replace(
      trim(manufacturer.name), '-', ' '), '/', ' '), '.', ' '), '_', '')) || ' '
      AS raw_name_tokens
  FROM avionics_manufacturers manufacturer
),
suffix_stripped AS (
  SELECT
    separated.*,
    replace(replace(replace(replace(replace(replace(replace(replace(replace(
      separated.raw_name_tokens,
      ' co ', ' '), ' company ', ' '), ' corp ', ' '),
      ' corporation ', ' '), ' inc ', ' '), ' incorporated ', ' '),
      ' llc ', ' '), ' ltd ', ' '), ' limited ', ' ') AS raw_core_tokens
  FROM separated
)
SELECT
  avionics_manufacturer_id,
  CASE
    WHEN replace(raw_core_tokens, ' ', '')
      IN ('cessnaaircraft', 'textronaviation') THEN 'cessna'
    WHEN replace(raw_core_tokens, ' ', '')
      IN ('cirrusaircraft', 'cirrusdesign') THEN 'cirrus'
    WHEN replace(raw_core_tokens, ' ', '')
      IN ('theairplanefactory', 'slingaircraft', 'slingairplane') THEN 'sling'
    ELSE replace(raw_core_tokens, ' ', '')
  END AS deterministic_name_key,
  lower(replace(replace(replace(replace(replace(
    trim(normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    AS stored_name_key,
  CASE
    WHEN length(trim(name)) > 0
      AND name NOT GLOB '*[^A-Za-z0-9 ./_-]*'
      AND length(trim(normalized_name)) > 0
      AND normalized_name NOT GLOB '*[^A-Za-z0-9 ./_-]*'
    THEN 1 ELSE 0
  END AS uses_supported_ascii
FROM suffix_stripped;

-- SQLite exposes no trigger-depth predicate. This transaction-local-in-effect
-- guard distinguishes an FK cascade initiated by deleting the manufacturer
-- from a direct attempt to delete and replace its canonical grouping key.
CREATE TABLE IF NOT EXISTS avionics_manufacturer_canonical_key_delete_context (
  avionics_manufacturer_id INTEGER PRIMARY KEY
);

DROP TRIGGER IF EXISTS avionics_manufacturer_canonical_key_delete_begin;
CREATE TRIGGER avionics_manufacturer_canonical_key_delete_begin
BEFORE DELETE ON avionics_manufacturers
BEGIN
  INSERT INTO avionics_manufacturer_canonical_key_delete_context (
    avionics_manufacturer_id
  ) VALUES (OLD.id)
  ON CONFLICT (avionics_manufacturer_id) DO NOTHING;
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_canonical_key_delete_end;
CREATE TRIGGER avionics_manufacturer_canonical_key_delete_end
AFTER DELETE ON avionics_manufacturers
BEGIN
  DELETE FROM avionics_manufacturer_canonical_key_delete_context
  WHERE avionics_manufacturer_id = OLD.id;
END;

INSERT INTO avionics_manufacturer_canonical_keys (
  avionics_manufacturer_id, canonical_manufacturer_key
)
SELECT
  manufacturer.id,
  lower(replace(replace(replace(replace(replace(
    trim(manufacturer.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
FROM avionics_manufacturers manufacturer
WHERE 1
ON CONFLICT (avionics_manufacturer_id) DO NOTHING;

DROP TRIGGER IF EXISTS avionics_manufacturer_canonical_key_insert;
CREATE TRIGGER avionics_manufacturer_canonical_key_insert
AFTER INSERT ON avionics_manufacturers
BEGIN
  INSERT INTO avionics_manufacturer_canonical_keys (
    avionics_manufacturer_id, canonical_manufacturer_key
  )
  VALUES (
    NEW.id,
    lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  );
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_canonical_key_immutable;
CREATE TRIGGER avionics_manufacturer_canonical_key_immutable
BEFORE UPDATE OF canonical_manufacturer_key
ON avionics_manufacturer_canonical_keys
BEGIN
  SELECT RAISE(ABORT, 'avionics manufacturer canonical key is immutable');
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_canonical_key_delete;
CREATE TRIGGER avionics_manufacturer_canonical_key_delete
BEFORE DELETE ON avionics_manufacturer_canonical_keys
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_manufacturer_canonical_key_delete_context delete_context
  WHERE delete_context.avionics_manufacturer_id = OLD.avionics_manufacturer_id
)
BEGIN
  SELECT RAISE(ABORT, 'avionics manufacturer canonical key cannot be deleted directly');
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_normalized_name_preserve_key;
CREATE TRIGGER avionics_manufacturer_normalized_name_preserve_key
BEFORE UPDATE OF normalized_name ON avionics_manufacturers
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_manufacturer_canonical_keys manufacturer_key
  WHERE manufacturer_key.avionics_manufacturer_id = OLD.id
    AND manufacturer_key.canonical_manufacturer_key = lower(replace(replace(
      replace(replace(replace(trim(NEW.normalized_name), ' ', ''), '-', ''),
      '/', ''), '.', ''), '_', ''))
)
BEGIN
  SELECT RAISE(ABORT, 'manufacturer normalization cannot change its canonical key');
END;

-- Curated manufacturer identity is evidence-backed and separate from raw
-- listing spelling. This migration deliberately seeds no identities from
-- unreviewed legacy makers.
CREATE TABLE IF NOT EXISTS avionics_manufacturer_identities (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  canonical_name TEXT NOT NULL,
  normalized_identity_key TEXT NOT NULL UNIQUE,
  identity_evidence_kind TEXT NOT NULL
    CHECK (identity_evidence_kind = 'authoritative_reference'),
  identity_source_url TEXT NOT NULL,
  identity_source_title TEXT NOT NULL,
  identity_evidence_text TEXT NOT NULL,
  identity_confidence TEXT NOT NULL CHECK (identity_confidence = 'very_high'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(canonical_name)) > 0),
  CHECK (length(normalized_identity_key) > 0),
  CHECK (normalized_identity_key = lower(normalized_identity_key)),
  CHECK (normalized_identity_key NOT GLOB '*[^a-z0-9]*'),
  CHECK (length(trim(identity_source_url)) > 0),
  CHECK (length(trim(identity_source_title)) > 0),
  CHECK (length(trim(identity_evidence_text)) > 0),
  CHECK (lower(identity_source_url) LIKE 'https://%')
);

CREATE TABLE IF NOT EXISTS avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id INTEGER PRIMARY KEY
    REFERENCES avionics_manufacturers(id) ON DELETE RESTRICT,
  avionics_manufacturer_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  membership_basis TEXT NOT NULL CHECK (membership_basis IN (
    'deterministic_exact', 'authoritative_primary', 'authoritative_alias'
  )),
  normalized_name_key TEXT NOT NULL,
  evidence_source_url TEXT NOT NULL,
  evidence_source_title TEXT NOT NULL,
  evidence_text TEXT NOT NULL,
  evidence_confidence TEXT NOT NULL CHECK (evidence_confidence = 'very_high'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(normalized_name_key) > 0),
  CHECK (normalized_name_key = lower(normalized_name_key)),
  CHECK (normalized_name_key NOT GLOB '*[^a-z0-9]*'),
  CHECK (length(trim(evidence_source_url)) > 0),
  CHECK (length(trim(evidence_source_title)) > 0),
  CHECK (length(trim(evidence_text)) > 0)
);

CREATE INDEX IF NOT EXISTS idx_avionics_manufacturer_identity_memberships_group
  ON avionics_manufacturer_identity_memberships (
    avionics_manufacturer_identity_id, avionics_manufacturer_id
  );

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_immutable_update;
CREATE TRIGGER avionics_manufacturer_identity_immutable_update
BEFORE UPDATE ON avionics_manufacturer_identities
BEGIN SELECT RAISE(ABORT, 'approved avionics manufacturer identities are immutable'); END;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_immutable_delete;
CREATE TRIGGER avionics_manufacturer_identity_immutable_delete
BEFORE DELETE ON avionics_manufacturer_identities
BEGIN SELECT RAISE(ABORT, 'approved avionics manufacturer identities are immutable'); END;

DROP TRIGGER IF EXISTS avionics_manufacturer_membership_validate_insert;
CREATE TRIGGER avionics_manufacturer_membership_validate_insert
BEFORE INSERT ON avionics_manufacturer_identity_memberships
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_manufacturer_canonical_keys manufacturer_key
  JOIN avionics_manufacturer_normalization_contract normalization
    ON normalization.avionics_manufacturer_id
      = manufacturer_key.avionics_manufacturer_id
  JOIN avionics_manufacturer_identities identity
    ON identity.id = NEW.avionics_manufacturer_identity_id
  WHERE manufacturer_key.avionics_manufacturer_id = NEW.avionics_manufacturer_id
    AND manufacturer_key.canonical_manufacturer_key = NEW.normalized_name_key
    AND normalization.uses_supported_ascii = 1
    AND normalization.deterministic_name_key
      = normalization.stored_name_key
    AND normalization.stored_name_key
      = manufacturer_key.canonical_manufacturer_key
    AND (
      NEW.membership_basis = 'authoritative_alias'
      OR (
        NEW.normalized_name_key = identity.normalized_identity_key
        AND (
          NEW.membership_basis = 'authoritative_primary'
          OR (
            NEW.membership_basis = 'deterministic_exact'
            AND NEW.evidence_source_url =
              'urn:aircost:deterministic:avionics-manufacturer-normalization:v1'
          )
        )
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'manufacturer membership lacks exact normalization or authoritative alias evidence');
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_membership_immutable_update;
CREATE TRIGGER avionics_manufacturer_membership_immutable_update
BEFORE UPDATE ON avionics_manufacturer_identity_memberships
BEGIN SELECT RAISE(ABORT, 'avionics manufacturer identity memberships are immutable'); END;

DROP TRIGGER IF EXISTS avionics_manufacturer_membership_immutable_delete;
CREATE TRIGGER avionics_manufacturer_membership_immutable_delete
BEFORE DELETE ON avionics_manufacturer_identity_memberships
BEGIN SELECT RAISE(ABORT, 'avionics manufacturer identity memberships are immutable'); END;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_name_immutable;
CREATE TRIGGER avionics_manufacturer_identity_name_immutable
BEFORE UPDATE OF name, normalized_name ON avionics_manufacturers
WHEN EXISTS (
  SELECT 1
  FROM avionics_manufacturer_identity_memberships membership
  WHERE membership.avionics_manufacturer_id = OLD.id
)
AND (
  NEW.name IS NOT OLD.name
  OR NEW.normalized_name IS NOT OLD.normalized_name
)
BEGIN
  SELECT RAISE(ABORT, 'evidence-backed avionics manufacturer name is immutable');
END;

-- Only already-approved products can establish migration-time identities;
-- their approval contract supplies the authoritative evidence. Unreviewed
-- legacy spellings never enter the curated identity list by themselves.
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key, identity_evidence_kind,
  identity_source_url, identity_source_title, identity_evidence_text,
  identity_confidence
)
SELECT manufacturer.name, manufacturer_key.canonical_manufacturer_key,
       'authoritative_reference', model.identity_source_url,
       model.identity_source_title, model.identity_evidence_text, 'very_high'
FROM avionics_models model
JOIN avionics_manufacturers manufacturer
  ON manufacturer.id = model.avionics_manufacturer_id
JOIN avionics_manufacturer_canonical_keys manufacturer_key
  ON manufacturer_key.avionics_manufacturer_id = manufacturer.id
WHERE model.catalog_status = 'approved'
  AND model.id = (
    SELECT MIN(first_model.id)
    FROM avionics_models first_model
    JOIN avionics_manufacturer_canonical_keys first_key
      ON first_key.avionics_manufacturer_id
        = first_model.avionics_manufacturer_id
    WHERE first_model.catalog_status = 'approved'
      AND first_key.canonical_manufacturer_key
        = manufacturer_key.canonical_manufacturer_key
  )
ON CONFLICT (normalized_identity_key) DO NOTHING;

INSERT INTO avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id, avionics_manufacturer_identity_id,
  membership_basis, normalized_name_key, evidence_source_url,
  evidence_source_title, evidence_text, evidence_confidence
)
SELECT manufacturer_key.avionics_manufacturer_id, identity.id,
       CASE
         WHEN manufacturer_key.avionics_manufacturer_id = (
           SELECT approved_model.avionics_manufacturer_id
           FROM avionics_models approved_model
           JOIN avionics_manufacturer_canonical_keys approved_key
             ON approved_key.avionics_manufacturer_id
               = approved_model.avionics_manufacturer_id
           WHERE approved_model.catalog_status = 'approved'
             AND approved_key.canonical_manufacturer_key
               = identity.normalized_identity_key
           ORDER BY approved_model.id
           LIMIT 1
         ) THEN 'authoritative_primary'
         ELSE 'deterministic_exact'
       END,
       manufacturer_key.canonical_manufacturer_key,
       CASE
         WHEN manufacturer_key.avionics_manufacturer_id = (
           SELECT approved_model.avionics_manufacturer_id
           FROM avionics_models approved_model
           JOIN avionics_manufacturer_canonical_keys approved_key
             ON approved_key.avionics_manufacturer_id
               = approved_model.avionics_manufacturer_id
           WHERE approved_model.catalog_status = 'approved'
             AND approved_key.canonical_manufacturer_key
               = identity.normalized_identity_key
           ORDER BY approved_model.id
           LIMIT 1
         ) THEN identity.identity_source_url
         ELSE 'urn:aircost:deterministic:avionics-manufacturer-normalization:v1'
       END,
       CASE
         WHEN manufacturer_key.avionics_manufacturer_id = (
           SELECT approved_model.avionics_manufacturer_id
           FROM avionics_models approved_model
           JOIN avionics_manufacturer_canonical_keys approved_key
             ON approved_key.avionics_manufacturer_id
               = approved_model.avionics_manufacturer_id
           WHERE approved_model.catalog_status = 'approved'
             AND approved_key.canonical_manufacturer_key
               = identity.normalized_identity_key
           ORDER BY approved_model.id
           LIMIT 1
         ) THEN identity.identity_source_title
         ELSE 'Aircost exact manufacturer normalization v1'
       END,
       CASE
         WHEN manufacturer_key.avionics_manufacturer_id = (
           SELECT approved_model.avionics_manufacturer_id
           FROM avionics_models approved_model
           JOIN avionics_manufacturer_canonical_keys approved_key
             ON approved_key.avionics_manufacturer_id
               = approved_model.avionics_manufacturer_id
           WHERE approved_model.catalog_status = 'approved'
             AND approved_key.canonical_manufacturer_key
               = identity.normalized_identity_key
           ORDER BY approved_model.id
           LIMIT 1
         ) THEN identity.identity_evidence_text
         ELSE 'The stored manufacturer spelling has the same exact deterministic normalization key as this evidence-backed identity.'
       END,
       'very_high'
FROM avionics_manufacturer_canonical_keys manufacturer_key
JOIN avionics_manufacturer_identities identity
  ON identity.normalized_identity_key
    = manufacturer_key.canonical_manufacturer_key
LEFT JOIN avionics_manufacturer_identity_memberships membership
  ON membership.avionics_manufacturer_id
    = manufacturer_key.avionics_manufacturer_id
WHERE membership.avionics_manufacturer_id IS NULL;

-- A v6 marker must never be installed over identities whose persisted
-- uniqueness keys are detached from their raw evidence.
CREATE TEMP TABLE avionics_normalization_contract_guard (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO avionics_normalization_contract_guard (valid)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM avionics_manufacturer_identity_memberships membership
    JOIN avionics_manufacturer_canonical_keys manufacturer_key
      ON manufacturer_key.avionics_manufacturer_id
        = membership.avionics_manufacturer_id
    LEFT JOIN avionics_manufacturer_normalization_contract normalization
      ON normalization.avionics_manufacturer_id
        = membership.avionics_manufacturer_id
    WHERE normalization.avionics_manufacturer_id IS NULL
       OR normalization.uses_supported_ascii <> 1
       OR normalization.deterministic_name_key
         <> normalization.stored_name_key
       OR normalization.stored_name_key
         <> manufacturer_key.canonical_manufacturer_key
       OR membership.normalized_name_key
         <> manufacturer_key.canonical_manufacturer_key
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models model
    WHERE model.catalog_status = 'approved'
      AND (
        model.name GLOB '*[^A-Za-z0-9 ./_-]*'
        OR model.normalized_name GLOB '*[^A-Za-z0-9 ./_-]*'
        OR lower(replace(replace(replace(replace(replace(
          trim(model.name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
          <> lower(replace(replace(replace(replace(replace(
            trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
        OR model.manufacturer_identifier IS NULL
        OR model.manufacturer_identifier GLOB '*[^A-Za-z0-9 ./_-]*'
        OR model.normalized_manufacturer_identifier IS NULL
        OR model.normalized_manufacturer_identifier
          GLOB '*[^A-Za-z0-9 ./_-]*'
        OR lower(replace(replace(replace(replace(replace(
          trim(model.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
          <> lower(replace(replace(replace(replace(replace(
            trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      )
  )
  THEN 1 ELSE 0
END;
DROP TABLE avionics_normalization_contract_guard;

-- Fail closed if an unshipped/incompatible identity-registry shape was
-- installed manually. The authorized live target has no such table; reruns
-- accept only this migration's clean identity-group shape.
CREATE TEMP TABLE avionics_identity_registry_shape_guard (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO avionics_identity_registry_shape_guard (valid)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'table' AND name = 'avionics_approved_product_identities'
  ) THEN 1
  WHEN EXISTS (
      SELECT 1 FROM pragma_table_info('avionics_approved_product_identities')
      WHERE name = 'avionics_manufacturer_identity_id'
    )
    AND NOT EXISTS (
      SELECT 1 FROM pragma_table_info('avionics_approved_product_identities')
      WHERE name IN ('avionics_manufacturer_id', 'canonical_manufacturer_key')
    )
  THEN 1
  ELSE 0
END;
DROP TABLE avionics_identity_registry_shape_guard;

CREATE TABLE IF NOT EXISTS avionics_approved_product_identities (
  avionics_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_manufacturer_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  canonical_product_key TEXT NOT NULL,
  manufacturer_identifier_kind TEXT NOT NULL
    CHECK (manufacturer_identifier_kind IN (
      'manufacturer_part_number', 'manufacturer_model_number', 'sku'
    )),
  canonical_identifier_key TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (avionics_manufacturer_identity_id, canonical_product_key),
  UNIQUE (
    avionics_manufacturer_identity_id, manufacturer_identifier_kind,
    canonical_identifier_key
  ),
  CHECK (length(canonical_product_key) > 0),
  CHECK (canonical_product_key = lower(canonical_product_key)),
  CHECK (canonical_product_key NOT GLOB '*[^a-z0-9]*'),
  CHECK (length(canonical_identifier_key) > 0),
  CHECK (canonical_identifier_key = lower(canonical_identifier_key)),
  CHECK (canonical_identifier_key NOT GLOB '*[^a-z0-9]*')
);

DROP VIEW IF EXISTS avionics_approved_product_graph_identities;
CREATE VIEW avionics_approved_product_graph_identities AS
SELECT avionics_model_id, avionics_manufacturer_identity_id,
       canonical_product_key, manufacturer_identifier_kind,
       canonical_identifier_key
FROM avionics_approved_product_identities;

CREATE TABLE IF NOT EXISTS avionics_manufacturer_alias_candidates (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  avionics_manufacturer_id INTEGER NOT NULL
    REFERENCES avionics_manufacturers(id) ON DELETE RESTRICT,
  candidate_manufacturer_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  candidate_basis TEXT NOT NULL CHECK (candidate_basis IN (
    'exact_product_name', 'exact_stable_identifier',
    'semantic_similarity', 'grounded_alias'
  )),
  matched_avionics_model_id INTEGER
    REFERENCES avionics_models(id) ON DELETE SET NULL,
  reason TEXT NOT NULL,
  evidence_source_url TEXT,
  evidence_source_title TEXT,
  evidence_text TEXT,
  confidence TEXT NOT NULL CHECK (confidence IN (
    'very_high', 'high', 'medium', 'low'
  )),
  review_status TEXT NOT NULL DEFAULT 'pending'
    CHECK (review_status IN ('pending', 'approved', 'rejected')),
  decision_reason TEXT,
  decision_evidence_source_url TEXT,
  decision_evidence_source_title TEXT,
  decision_evidence_text TEXT,
  reviewed_by_user_id INTEGER REFERENCES users(id) ON DELETE RESTRICT,
  reviewed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(reason)) > 0),
  CHECK (
    (evidence_source_url IS NULL
      AND evidence_source_title IS NULL
      AND evidence_text IS NULL)
    OR (evidence_source_url IS NOT NULL
      AND lower(evidence_source_url) LIKE 'https://%'
      AND evidence_source_title IS NOT NULL
      AND length(trim(evidence_source_title)) > 0
      AND evidence_text IS NOT NULL
      AND length(trim(evidence_text)) > 0)
  ),
  CHECK (
    (review_status = 'pending'
      AND decision_reason IS NULL
      AND decision_evidence_source_url IS NULL
      AND decision_evidence_source_title IS NULL
      AND decision_evidence_text IS NULL
      AND reviewed_by_user_id IS NULL
      AND reviewed_at IS NULL)
    OR (review_status = 'rejected'
      AND decision_reason IS NOT NULL
      AND length(trim(decision_reason)) > 0
      AND reviewed_by_user_id IS NOT NULL
      AND reviewed_at IS NOT NULL)
    OR (review_status = 'approved'
      AND decision_reason IS NOT NULL
      AND length(trim(decision_reason)) > 0
      AND decision_evidence_source_url IS NOT NULL
      AND lower(decision_evidence_source_url) LIKE 'https://%'
      AND decision_evidence_source_title IS NOT NULL
      AND length(trim(decision_evidence_source_title)) > 0
      AND decision_evidence_text IS NOT NULL
      AND length(trim(decision_evidence_text)) > 0
      AND reviewed_by_user_id IS NOT NULL
      AND reviewed_at IS NOT NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_avionics_manufacturer_alias_candidates_pending
  ON avionics_manufacturer_alias_candidates (
    avionics_manufacturer_id, candidate_manufacturer_identity_id
  )
  WHERE review_status = 'pending';

DROP TRIGGER IF EXISTS avionics_manufacturer_alias_candidate_pending_insert;
CREATE TRIGGER avionics_manufacturer_alias_candidate_pending_insert
BEFORE INSERT ON avionics_manufacturer_alias_candidates
WHEN NEW.review_status <> 'pending'
BEGIN
  SELECT RAISE(ABORT, 'manufacturer alias candidates must be inserted pending');
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_alias_candidate_update;
CREATE TRIGGER avionics_manufacturer_alias_candidate_update
BEFORE UPDATE ON avionics_manufacturer_alias_candidates
WHEN NOT (
    NEW.id = OLD.id
    AND NEW.avionics_manufacturer_id = OLD.avionics_manufacturer_id
    AND NEW.candidate_manufacturer_identity_id
      = OLD.candidate_manufacturer_identity_id
    AND NEW.candidate_basis = OLD.candidate_basis
    AND NEW.matched_avionics_model_id IS NOT OLD.matched_avionics_model_id
    AND NEW.reason = OLD.reason
    AND NEW.evidence_source_url IS OLD.evidence_source_url
    AND NEW.evidence_source_title IS OLD.evidence_source_title
    AND NEW.evidence_text IS OLD.evidence_text
    AND NEW.confidence = OLD.confidence
    AND NEW.review_status = OLD.review_status
    AND NEW.decision_reason IS OLD.decision_reason
    AND NEW.decision_evidence_source_url IS OLD.decision_evidence_source_url
    AND NEW.decision_evidence_source_title IS OLD.decision_evidence_source_title
    AND NEW.decision_evidence_text IS OLD.decision_evidence_text
    AND NEW.reviewed_by_user_id IS OLD.reviewed_by_user_id
    AND NEW.reviewed_at IS OLD.reviewed_at
    AND NEW.created_at = OLD.created_at
    AND EXISTS (
      SELECT 1
      FROM avionics_catalog_authorized_consolidations guard
      WHERE guard.duplicate_model_id = OLD.matched_avionics_model_id
        AND guard.survivor_model_id = NEW.matched_avionics_model_id
    )
  )
  AND (
    OLD.review_status <> 'pending'
    OR NEW.id <> OLD.id
    OR NEW.avionics_manufacturer_id <> OLD.avionics_manufacturer_id
    OR NEW.candidate_manufacturer_identity_id
      <> OLD.candidate_manufacturer_identity_id
    OR NEW.candidate_basis <> OLD.candidate_basis
    OR NEW.matched_avionics_model_id IS NOT OLD.matched_avionics_model_id
    OR NEW.reason <> OLD.reason
    OR NEW.evidence_source_url IS NOT OLD.evidence_source_url
    OR NEW.evidence_source_title IS NOT OLD.evidence_source_title
    OR NEW.evidence_text IS NOT OLD.evidence_text
    OR NEW.confidence <> OLD.confidence
    OR NEW.review_status NOT IN ('approved', 'rejected')
  )
BEGIN
  SELECT RAISE(ABORT, 'manufacturer alias candidates are immutable after staging except for one review decision');
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_alias_candidate_delete;
CREATE TRIGGER avionics_manufacturer_alias_candidate_delete
BEFORE DELETE ON avionics_manufacturer_alias_candidates
BEGIN SELECT RAISE(ABORT, 'manufacturer alias candidate history is immutable'); END;

CREATE TABLE IF NOT EXISTS avionics_manufacturer_identity_merges (
  merged_identity_id INTEGER PRIMARY KEY
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  survivor_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  alias_candidate_id INTEGER NOT NULL UNIQUE
    REFERENCES avionics_manufacturer_alias_candidates(id) ON DELETE RESTRICT,
  evidence_source_url TEXT NOT NULL,
  evidence_source_title TEXT NOT NULL,
  evidence_text TEXT NOT NULL,
  decided_by_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (merged_identity_id <> survivor_identity_id),
  CHECK (lower(evidence_source_url) LIKE 'https://%'),
  CHECK (length(trim(evidence_source_title)) > 0),
  CHECK (length(trim(evidence_text)) > 0)
);

DROP VIEW IF EXISTS avionics_manufacturer_effective_identities;
CREATE VIEW avionics_manufacturer_effective_identities AS
WITH RECURSIVE resolved(identity_id, effective_identity_id, depth, path) AS (
  SELECT identity.id, identity.id, 0, ',' || identity.id || ','
  FROM avionics_manufacturer_identities identity
  UNION ALL
  SELECT resolved.identity_id, merge.survivor_identity_id,
         resolved.depth + 1,
         resolved.path || merge.survivor_identity_id || ','
  FROM resolved
  JOIN avionics_manufacturer_identity_merges merge
    ON merge.merged_identity_id = resolved.effective_identity_id
  WHERE resolved.depth < 32
    AND instr(resolved.path, ',' || merge.survivor_identity_id || ',') = 0
)
SELECT resolved.identity_id,
       resolved.effective_identity_id AS avionics_manufacturer_identity_id
FROM resolved
WHERE NOT EXISTS (
  SELECT 1 FROM avionics_manufacturer_identity_merges merge
  WHERE merge.merged_identity_id = resolved.effective_identity_id
);

DROP VIEW IF EXISTS avionics_manufacturer_effective_memberships;
CREATE VIEW avionics_manufacturer_effective_memberships AS
SELECT membership.avionics_manufacturer_id,
       membership.avionics_manufacturer_identity_id AS original_identity_id,
       effective.avionics_manufacturer_identity_id
FROM avionics_manufacturer_identity_memberships membership
JOIN avionics_manufacturer_effective_identities effective
  ON effective.identity_id = membership.avionics_manufacturer_identity_id;

DROP VIEW IF EXISTS avionics_legacy_manufacturer_alias_signals;
CREATE VIEW avionics_legacy_manufacturer_alias_signals AS
WITH products AS (
  SELECT model.id AS avionics_model_id,
         model.avionics_manufacturer_id,
         manufacturer.name AS manufacturer,
         model.name AS model,
         lower(replace(replace(replace(replace(replace(
           trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
           AS canonical_product_key,
         model.manufacturer_identifier_kind,
         CASE
           WHEN model.normalized_manufacturer_identifier IS NULL THEN NULL
           ELSE lower(replace(replace(replace(replace(replace(
             trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''),
             '/', ''), '.', ''), '_', ''))
         END AS canonical_identifier_key
  FROM avionics_models model
  JOIN avionics_manufacturers manufacturer
    ON manufacturer.id = model.avionics_manufacturer_id
  WHERE model.catalog_status <> 'rejected'
)
SELECT
  CASE
    WHEN left_product.manufacturer_identifier_kind IS NOT NULL
      AND left_product.manufacturer_identifier_kind
        = right_product.manufacturer_identifier_kind
      AND length(left_product.canonical_identifier_key) > 0
      AND left_product.canonical_identifier_key
        = right_product.canonical_identifier_key
    THEN 'exact_stable_identifier'
    ELSE 'exact_product_name'
  END AS candidate_basis,
  left_product.avionics_manufacturer_id AS left_avionics_manufacturer_id,
  left_product.manufacturer AS left_manufacturer,
  left_product.avionics_model_id AS left_avionics_model_id,
  left_product.model AS left_model,
  right_product.avionics_manufacturer_id AS right_avionics_manufacturer_id,
  right_product.manufacturer AS right_manufacturer,
  right_product.avionics_model_id AS right_avionics_model_id,
  right_product.model AS right_model
FROM products left_product
JOIN products right_product
  ON right_product.avionics_manufacturer_id
    <> left_product.avionics_manufacturer_id
 AND (
   (
     left_product.manufacturer_identifier_kind IS NOT NULL
     AND left_product.manufacturer_identifier_kind
       = right_product.manufacturer_identifier_kind
     AND length(left_product.canonical_identifier_key) > 0
     AND left_product.canonical_identifier_key
       = right_product.canonical_identifier_key
   )
   OR (
     length(left_product.canonical_product_key) > 0
     AND left_product.canonical_product_key
       = right_product.canonical_product_key
   )
 );

DROP TRIGGER IF EXISTS avionics_manufacturer_alias_membership_requires_decision;
CREATE TRIGGER avionics_manufacturer_alias_membership_requires_decision
BEFORE INSERT ON avionics_manufacturer_identity_memberships
WHEN NEW.membership_basis = 'authoritative_alias'
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_manufacturer_alias_candidates candidate
    JOIN avionics_manufacturer_effective_identities effective
      ON effective.identity_id = candidate.candidate_manufacturer_identity_id
    WHERE candidate.avionics_manufacturer_id = NEW.avionics_manufacturer_id
      AND candidate.review_status = 'approved'
      AND effective.avionics_manufacturer_identity_id
        = NEW.avionics_manufacturer_identity_id
      AND candidate.decision_evidence_source_url = NEW.evidence_source_url
      AND candidate.decision_evidence_source_title = NEW.evidence_source_title
      AND candidate.decision_evidence_text = NEW.evidence_text
  )
BEGIN
  SELECT RAISE(ABORT, 'semantic manufacturer membership requires an approved authoritative alias decision');
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_merge_validate;
CREATE TRIGGER avionics_manufacturer_identity_merge_validate
BEFORE INSERT ON avionics_manufacturer_identity_merges
WHEN EXISTS (
    SELECT 1 FROM avionics_manufacturer_identity_merges existing
    WHERE existing.merged_identity_id = NEW.merged_identity_id
       OR existing.merged_identity_id = NEW.survivor_identity_id
  )
  OR EXISTS (
    WITH RECURSIVE incoming(identity_id, depth) AS (
      SELECT NEW.merged_identity_id, 0
      UNION ALL
      SELECT existing.merged_identity_id, incoming.depth + 1
      FROM avionics_manufacturer_identity_merges existing
      JOIN incoming
        ON existing.survivor_identity_id = incoming.identity_id
      WHERE incoming.depth < 32
    )
    SELECT 1 FROM incoming WHERE depth = 32
  )
  OR NOT EXISTS (
    SELECT 1
    FROM avionics_manufacturer_alias_candidates candidate
    JOIN avionics_manufacturer_effective_memberships membership
      ON membership.avionics_manufacturer_id = candidate.avionics_manufacturer_id
    JOIN avionics_manufacturer_effective_identities candidate_target
      ON candidate_target.identity_id = candidate.candidate_manufacturer_identity_id
    WHERE candidate.id = NEW.alias_candidate_id
      AND candidate.review_status = 'approved'
      AND membership.avionics_manufacturer_identity_id = NEW.merged_identity_id
      AND candidate_target.avionics_manufacturer_identity_id
        = NEW.survivor_identity_id
      AND candidate.decision_evidence_source_url = NEW.evidence_source_url
      AND candidate.decision_evidence_source_title = NEW.evidence_source_title
      AND candidate.decision_evidence_text = NEW.evidence_text
  )
  OR EXISTS (
    SELECT 1
    FROM avionics_approved_product_identities merged_product
    JOIN avionics_approved_product_identities survivor_product
      ON survivor_product.avionics_manufacturer_identity_id
        = NEW.survivor_identity_id
     AND (
       survivor_product.canonical_product_key
         = merged_product.canonical_product_key
       OR (
         survivor_product.manufacturer_identifier_kind
           = merged_product.manufacturer_identifier_kind
         AND survivor_product.canonical_identifier_key
           = merged_product.canonical_identifier_key
       )
     )
    WHERE merged_product.avionics_manufacturer_identity_id
      = NEW.merged_identity_id
  )
BEGIN
  SELECT RAISE(ABORT, 'manufacturer identity merge requires two roots, an approved alias decision, and no product collision');
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_merge_apply;
CREATE TRIGGER avionics_manufacturer_identity_merge_apply
AFTER INSERT ON avionics_manufacturer_identity_merges
BEGIN
  UPDATE avionics_approved_product_identities
  SET avionics_manufacturer_identity_id = NEW.survivor_identity_id,
      updated_at = CURRENT_TIMESTAMP
  WHERE avionics_manufacturer_identity_id = NEW.merged_identity_id;
END;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_merge_immutable_update;
CREATE TRIGGER avionics_manufacturer_identity_merge_immutable_update
BEFORE UPDATE ON avionics_manufacturer_identity_merges
BEGIN SELECT RAISE(ABORT, 'manufacturer identity merge history is immutable'); END;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_merge_immutable_delete;
CREATE TRIGGER avionics_manufacturer_identity_merge_immutable_delete
BEFORE DELETE ON avionics_manufacturer_identity_merges
BEGIN SELECT RAISE(ABORT, 'manufacturer identity merge history is immutable'); END;

-- The guard is transaction-scoped and must never retain rows. Rebuild it so
-- databases that installed an earlier contract gain the asymmetric delete
-- policy: deleting the duplicate consumes its guard, while the survivor
-- remains protected.
DROP VIEW IF EXISTS avionics_catalog_authorized_consolidations;
DROP TRIGGER IF EXISTS avionics_catalog_consolidation_guard_validate_insert;
DROP TRIGGER IF EXISTS avionics_catalog_consolidation_guard_immutable;
DROP TRIGGER IF EXISTS avionics_models_consolidation_identity_immutable;
DROP TABLE IF EXISTS avionics_catalog_consolidation_guard;
CREATE TABLE IF NOT EXISTS avionics_catalog_consolidation_guard (
  duplicate_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  purpose TEXT NOT NULL DEFAULT 'legacy_identity_consolidation'
    CHECK (purpose = 'legacy_identity_consolidation'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (duplicate_model_id <> survivor_model_id)
);

DROP TRIGGER IF EXISTS avionics_catalog_consolidation_guard_validate_insert;
CREATE TRIGGER avionics_catalog_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_consolidation_guard
WHEN NOT EXISTS (
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
BEGIN
  SELECT RAISE(ABORT, 'consolidation guard requires an exact product pair with an approved-or-unreviewed survivor');
END;

DROP TRIGGER IF EXISTS avionics_catalog_consolidation_guard_immutable;
CREATE TRIGGER avionics_catalog_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_consolidation_guard
BEGIN
  SELECT RAISE(ABORT, 'consolidation authorization pairs are immutable');
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
      trim(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''));

DROP TRIGGER IF EXISTS avionics_models_consolidation_identity_immutable;
CREATE TRIGGER avionics_models_consolidation_identity_immutable
BEFORE UPDATE OF catalog_status, avionics_manufacturer_id, name,
  normalized_name, manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
ON avionics_models
WHEN EXISTS (
  SELECT 1 FROM avionics_catalog_consolidation_guard guard
  WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'guarded avionics consolidation identities are immutable');
END;

DROP TRIGGER IF EXISTS avionics_approved_identity_validate_insert;
CREATE TRIGGER avionics_approved_identity_validate_insert
BEFORE INSERT ON avionics_approved_product_identities
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  JOIN avionics_manufacturer_effective_memberships manufacturer_identity
    ON manufacturer_identity.avionics_manufacturer_id
      = model.avionics_manufacturer_id
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
    AND manufacturer_identity.avionics_manufacturer_identity_id
      = NEW.avionics_manufacturer_identity_id
    AND lower(replace(replace(replace(replace(replace(
      trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = NEW.canonical_product_key
    AND model.manufacturer_identifier_kind = NEW.manufacturer_identifier_kind
    AND lower(replace(replace(replace(replace(replace(
      trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = NEW.canonical_identifier_key
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics identity must match its catalog product');
END;

DROP TRIGGER IF EXISTS avionics_approved_identity_validate_update;
CREATE TRIGGER avionics_approved_identity_validate_update
BEFORE UPDATE ON avionics_approved_product_identities
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  JOIN avionics_manufacturer_effective_memberships manufacturer_identity
    ON manufacturer_identity.avionics_manufacturer_id
      = model.avionics_manufacturer_id
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
    AND manufacturer_identity.avionics_manufacturer_identity_id
      = NEW.avionics_manufacturer_identity_id
    AND lower(replace(replace(replace(replace(replace(
      trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = NEW.canonical_product_key
    AND model.manufacturer_identifier_kind = NEW.manufacturer_identifier_kind
    AND lower(replace(replace(replace(replace(replace(
      trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = NEW.canonical_identifier_key
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics identity must match its catalog product');
END;

DROP TRIGGER IF EXISTS avionics_approved_identity_preserve_delete;
CREATE TRIGGER avionics_approved_identity_preserve_delete
BEFORE DELETE ON avionics_approved_product_identities
WHEN EXISTS (
  SELECT 1 FROM avionics_models model
  WHERE model.id = OLD.avionics_model_id AND model.catalog_status = 'approved'
)
AND NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_authorized_consolidations authorization
  JOIN avionics_models survivor
    ON survivor.id = authorization.survivor_model_id
  WHERE authorization.duplicate_model_id = OLD.avionics_model_id
    AND survivor.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics product must retain its canonical identity');
END;

DROP TRIGGER IF EXISTS avionics_models_canonical_identity_validate_update;
CREATE TRIGGER avionics_models_canonical_identity_validate_update
BEFORE UPDATE OF catalog_status, avionics_manufacturer_id,
  normalized_name, normalized_manufacturer_identifier
ON avionics_models
WHEN NEW.catalog_status = 'approved'
  AND (
    NOT EXISTS (
      SELECT 1
      FROM avionics_manufacturer_effective_memberships manufacturer_identity
      WHERE manufacturer_identity.avionics_manufacturer_id
        = NEW.avionics_manufacturer_id
    )
    OR length(lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))) = 0
    OR length(lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))) = 0
    OR NEW.name GLOB '*[^A-Za-z0-9 ./_-]*'
    OR NEW.normalized_name GLOB '*[^A-Za-z0-9 ./_-]*'
    OR lower(replace(replace(replace(replace(replace(
      trim(NEW.name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      <> lower(replace(replace(replace(replace(replace(
        trim(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    OR NEW.manufacturer_identifier IS NULL
    OR NEW.manufacturer_identifier GLOB '*[^A-Za-z0-9 ./_-]*'
    OR NEW.normalized_manufacturer_identifier
      GLOB '*[^A-Za-z0-9 ./_-]*'
    OR lower(replace(replace(replace(replace(replace(
      trim(NEW.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      <> lower(replace(replace(replace(replace(replace(
        trim(NEW.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  )
BEGIN
  SELECT RAISE(ABORT, 'approved avionics product requires deterministic canonical identity keys');
END;

DROP TRIGGER IF EXISTS avionics_models_canonical_identity_sync_update;
CREATE TRIGGER avionics_models_canonical_identity_sync_update
AFTER UPDATE OF catalog_status, avionics_manufacturer_id,
  normalized_name, normalized_manufacturer_identifier
ON avionics_models
WHEN NEW.catalog_status = 'approved'
BEGIN
  INSERT INTO avionics_approved_product_identities (
    avionics_model_id,
    avionics_manufacturer_identity_id,
    canonical_product_key,
    manufacturer_identifier_kind, canonical_identifier_key
  )
  SELECT
    NEW.id,
    manufacturer_identity.avionics_manufacturer_identity_id,
    lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', '')),
    NEW.manufacturer_identifier_kind,
    lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  FROM avionics_manufacturer_effective_memberships manufacturer_identity
  WHERE manufacturer_identity.avionics_manufacturer_id
    = NEW.avionics_manufacturer_id
  ON CONFLICT (avionics_model_id) DO UPDATE SET
    avionics_manufacturer_identity_id
      = excluded.avionics_manufacturer_identity_id,
    canonical_product_key = excluded.canonical_product_key,
    manufacturer_identifier_kind = excluded.manufacturer_identifier_kind,
    canonical_identifier_key = excluded.canonical_identifier_key,
    updated_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS avionics_models_canonical_identity_remove_update;

DROP TRIGGER IF EXISTS avionics_models_approved_identity_immutable;
CREATE TRIGGER avionics_models_approved_identity_immutable
BEFORE UPDATE ON avionics_models
WHEN OLD.catalog_status = 'approved'
AND (
  NEW.catalog_status IS NOT OLD.catalog_status
  OR NEW.avionics_manufacturer_id IS NOT OLD.avionics_manufacturer_id
  OR NEW.name IS NOT OLD.name
  OR NEW.normalized_name IS NOT OLD.normalized_name
  OR NEW.manufacturer_identifier_kind IS NOT OLD.manufacturer_identifier_kind
  OR NEW.manufacturer_identifier IS NOT OLD.manufacturer_identifier
  OR NEW.normalized_manufacturer_identifier
    IS NOT OLD.normalized_manufacturer_identifier
  OR NEW.identity_source_url IS NOT OLD.identity_source_url
  OR NEW.identity_source_title IS NOT OLD.identity_source_title
  OR NEW.identity_evidence_text IS NOT OLD.identity_evidence_text
  OR NEW.identity_evidence_kind IS NOT OLD.identity_evidence_kind
  OR NEW.identity_confidence IS NOT OLD.identity_confidence
  OR NEW.catalog_reviewed_at IS NOT OLD.catalog_reviewed_at
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics product cannot be demoted or rewrite identity evidence');
END;

DROP TRIGGER IF EXISTS avionics_models_approved_delete_guard;
CREATE TRIGGER avionics_models_approved_delete_guard
BEFORE DELETE ON avionics_models
WHEN OLD.catalog_status = 'approved'
AND NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_authorized_consolidations authorization
  JOIN avionics_models survivor
    ON survivor.id = authorization.survivor_model_id
  WHERE authorization.duplicate_model_id = OLD.id
    AND survivor.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics product deletion requires exact consolidation authorization');
END;

DROP TRIGGER IF EXISTS avionics_models_approved_types_insert;
CREATE TRIGGER avionics_models_approved_types_insert
BEFORE INSERT ON avionics_models
WHEN NEW.catalog_status = 'approved'
BEGIN
  SELECT RAISE(ABORT, 'avionics approval must be staged from an unreviewed product');
END;

DROP TRIGGER IF EXISTS avionics_models_approved_types_update;
CREATE TRIGGER avionics_models_approved_types_update
BEFORE UPDATE OF catalog_status ON avionics_models
WHEN NEW.catalog_status = 'approved'
AND NOT EXISTS (
  SELECT 1
  FROM avionics_model_types membership
  WHERE membership.avionics_model_id = NEW.id
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics model requires at least one type');
END;

DROP TRIGGER IF EXISTS avionics_model_types_preserve_approved_delete;
CREATE TRIGGER avionics_model_types_preserve_approved_delete
BEFORE DELETE ON avionics_model_types
WHEN EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = OLD.avionics_model_id
    AND model.catalog_status = 'approved'
)
AND NOT EXISTS (
  SELECT 1
  FROM avionics_model_types other
  WHERE other.avionics_model_id = OLD.avionics_model_id
    AND other.avionics_type_id <> OLD.avionics_type_id
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics model cannot lose its last type');
END;

DROP TRIGGER IF EXISTS avionics_model_types_preserve_approved_update;
CREATE TRIGGER avionics_model_types_preserve_approved_update
BEFORE UPDATE OF avionics_model_id ON avionics_model_types
WHEN NEW.avionics_model_id <> OLD.avionics_model_id
AND EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = OLD.avionics_model_id
    AND model.catalog_status = 'approved'
)
AND NOT EXISTS (
  SELECT 1
  FROM avionics_model_types other
  WHERE other.avionics_model_id = OLD.avionics_model_id
    AND other.avionics_type_id <> OLD.avionics_type_id
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics model cannot lose its last type');
END;

DROP TRIGGER IF EXISTS avionics_models_referenced_status_update;
CREATE TRIGGER avionics_models_referenced_status_update
BEFORE UPDATE OF catalog_status ON avionics_models
WHEN NEW.catalog_status <> 'approved'
AND (
  EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_avionics listing_link
    WHERE listing_link.avionics_model_id = OLD.id
       OR listing_link.replaces_avionics_model_id = OLD.id
  )
  OR EXISTS (
    SELECT 1
    FROM aircraft_model_variant_default_avionics default_link
    WHERE default_link.avionics_model_id = OLD.id
  )
  OR EXISTS (
    SELECT 1
    FROM avionics_suite_components suite_link
    WHERE suite_link.suite_model_id = OLD.id
       OR suite_link.component_model_id = OLD.id
  )
  OR EXISTS (
    SELECT 1
    FROM aircraft_reference_avionics reference_link
    WHERE reference_link.avionics_model_id = OLD.id
  )
)
BEGIN
  SELECT RAISE(ABORT, 'referenced avionics catalog entry cannot be unapproved');
END;

INSERT INTO avionics_approved_product_identities (
  avionics_model_id, avionics_manufacturer_identity_id,
  canonical_product_key,
  manufacturer_identifier_kind, canonical_identifier_key
)
SELECT
  model.id,
  manufacturer_identity.avionics_manufacturer_identity_id,
  lower(replace(replace(replace(replace(replace(
    trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', '')),
  model.manufacturer_identifier_kind,
  lower(replace(replace(replace(replace(replace(
    trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
FROM avionics_models model
JOIN avionics_manufacturer_effective_memberships manufacturer_identity
  ON manufacturer_identity.avionics_manufacturer_id
    = model.avionics_manufacturer_id
WHERE model.catalog_status = 'approved'
ON CONFLICT (avionics_model_id) DO UPDATE SET
  avionics_manufacturer_identity_id
    = excluded.avionics_manufacturer_identity_id,
  canonical_product_key = excluded.canonical_product_key,
  manufacturer_identifier_kind = excluded.manufacturer_identifier_kind,
  canonical_identifier_key = excluded.canonical_identifier_key,
  updated_at = CURRENT_TIMESTAMP;

-- Completion requires the registry to be the exact canonical projection of
-- the approved catalog, not merely a table that happens to exist.
CREATE TEMP TABLE avionics_approved_registry_completeness_guard (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO avionics_approved_registry_completeness_guard (valid)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM avionics_models model
    WHERE model.catalog_status = 'approved'
      AND NOT EXISTS (
        SELECT 1
        FROM avionics_approved_product_identities identity
        JOIN avionics_manufacturer_effective_memberships manufacturer_identity
          ON manufacturer_identity.avionics_manufacturer_id
            = model.avionics_manufacturer_id
         AND manufacturer_identity.avionics_manufacturer_identity_id
            = identity.avionics_manufacturer_identity_id
        WHERE identity.avionics_model_id = model.id
          AND identity.canonical_product_key
            = lower(replace(replace(replace(replace(replace(
              trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
          AND identity.manufacturer_identifier_kind
            = model.manufacturer_identifier_kind
          AND identity.canonical_identifier_key
            = lower(replace(replace(replace(replace(replace(
              trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      )
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_approved_product_identities identity
    JOIN avionics_models model ON model.id = identity.avionics_model_id
    WHERE model.catalog_status <> 'approved'
       OR NOT EXISTS (
         SELECT 1
         FROM avionics_manufacturer_effective_memberships manufacturer_identity
         WHERE manufacturer_identity.avionics_manufacturer_id
             = model.avionics_manufacturer_id
           AND manufacturer_identity.avionics_manufacturer_identity_id
             = identity.avionics_manufacturer_identity_id
       )
       OR identity.canonical_product_key
         <> lower(replace(replace(replace(replace(replace(
           trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
       OR identity.manufacturer_identifier_kind
         <> model.manufacturer_identifier_kind
       OR identity.canonical_identifier_key
         <> lower(replace(replace(replace(replace(replace(
           trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  )
  THEN 1 ELSE 0
END;
DROP TABLE avionics_approved_registry_completeness_guard;

DROP TRIGGER IF EXISTS avionics_suite_components_approved_insert;
CREATE TRIGGER avionics_suite_components_approved_insert
BEFORE INSERT ON avionics_suite_components
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models suite_model
  WHERE suite_model.id = NEW.suite_model_id
    AND suite_model.catalog_status = 'approved'
)
OR NOT EXISTS (
  SELECT 1
  FROM avionics_models component_model
  WHERE component_model.id = NEW.component_model_id
    AND component_model.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'avionics suite membership requires approved catalog entries');
END;

DROP TRIGGER IF EXISTS avionics_suite_components_approved_update;
CREATE TRIGGER avionics_suite_components_approved_update
BEFORE UPDATE ON avionics_suite_components
WHEN (
  NEW.suite_model_id IS NOT OLD.suite_model_id
  AND NOT EXISTS (
    SELECT 1 FROM avionics_models model
    WHERE model.id = NEW.suite_model_id AND model.catalog_status = 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = guard.duplicate_model_id
    WHERE guard.duplicate_model_id = OLD.suite_model_id
      AND guard.survivor_model_id = NEW.suite_model_id
      AND survivor.catalog_status = 'unreviewed'
      AND legacy.catalog_status = 'unreviewed'
  )
)
OR (
  NEW.component_model_id IS NOT OLD.component_model_id
  AND NOT EXISTS (
    SELECT 1 FROM avionics_models model
    WHERE model.id = NEW.component_model_id AND model.catalog_status = 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = guard.duplicate_model_id
    WHERE guard.duplicate_model_id = OLD.component_model_id
      AND guard.survivor_model_id = NEW.component_model_id
      AND survivor.catalog_status = 'unreviewed'
      AND legacy.catalog_status = 'unreviewed'
  )
)
BEGIN
  SELECT RAISE(ABORT, 'avionics suite membership requires approved catalog entries');
END;

DROP TRIGGER IF EXISTS aircraft_model_variant_default_avionics_approved_insert;
CREATE TRIGGER aircraft_model_variant_default_avionics_approved_insert
BEFORE INSERT ON aircraft_model_variant_default_avionics
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'default avionics association requires an approved catalog entry');
END;

DROP TRIGGER IF EXISTS aircraft_model_variant_default_avionics_approved_update;
CREATE TRIGGER aircraft_model_variant_default_avionics_approved_update
BEFORE UPDATE OF avionics_model_id ON aircraft_model_variant_default_avionics
WHEN NEW.avionics_model_id IS NOT OLD.avionics_model_id
AND NOT EXISTS (
  SELECT 1 FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id AND model.catalog_status = 'approved'
)
AND NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_authorized_consolidations guard
  JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
  JOIN avionics_models legacy ON legacy.id = guard.duplicate_model_id
  WHERE guard.duplicate_model_id = OLD.avionics_model_id
    AND guard.survivor_model_id = NEW.avionics_model_id
    AND survivor.catalog_status = 'unreviewed'
    AND legacy.catalog_status = 'unreviewed'
)
BEGIN
  SELECT RAISE(ABORT, 'default avionics association requires an approved catalog entry');
END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_approved_insert;
CREATE TRIGGER aircraft_sale_listing_avionics_approved_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
)
OR (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models model
    WHERE model.id = NEW.replaces_avionics_model_id
      AND model.catalog_status = 'approved'
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics association requires approved catalog entries');
END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_approved_update;
CREATE TRIGGER aircraft_sale_listing_avionics_approved_update
BEFORE UPDATE OF avionics_model_id, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
WHEN (
  NEW.avionics_model_id IS NOT OLD.avionics_model_id
  AND NOT EXISTS (
    SELECT 1 FROM avionics_models model
    WHERE model.id = NEW.avionics_model_id AND model.catalog_status = 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = guard.duplicate_model_id
    JOIN aircraft_sale_listings listing ON listing.id = NEW.aircraft_sale_listing_id
    WHERE guard.duplicate_model_id = OLD.avionics_model_id
      AND guard.survivor_model_id = NEW.avionics_model_id
      AND survivor.catalog_status = 'unreviewed'
      AND legacy.catalog_status = 'unreviewed'
      AND listing.ingestion_state <> 'ready'
      AND listing.is_verified = 0
  )
)
OR (
  NEW.replaces_avionics_model_id IS NOT OLD.replaces_avionics_model_id
  AND NEW.replaces_avionics_model_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1 FROM avionics_models model
    WHERE model.id = NEW.replaces_avionics_model_id
      AND model.catalog_status = 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = guard.duplicate_model_id
    JOIN aircraft_sale_listings listing ON listing.id = NEW.aircraft_sale_listing_id
    WHERE guard.duplicate_model_id = OLD.replaces_avionics_model_id
      AND guard.survivor_model_id = NEW.replaces_avionics_model_id
      AND survivor.catalog_status = 'unreviewed'
      AND legacy.catalog_status = 'unreviewed'
      AND listing.ingestion_state <> 'ready'
      AND listing.is_verified = 0
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics association requires approved catalog entries');
END;

DROP TRIGGER IF EXISTS aircraft_reference_avionics_building_insert;
CREATE TRIGGER aircraft_reference_avionics_building_insert
BEFORE INSERT ON aircraft_reference_avionics
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'reference avionics requires a building version and approved product');
END;

DROP TRIGGER IF EXISTS aircraft_reference_avionics_immutable_update;
CREATE TRIGGER aircraft_reference_avionics_immutable_update
BEFORE UPDATE ON aircraft_reference_avionics
WHEN NOT (
  NEW.id = OLD.id
  AND NEW.aircraft_reference_configuration_version_id
    = OLD.aircraft_reference_configuration_version_id
  AND NEW.avionics_model_id IS NOT OLD.avionics_model_id
  AND NEW.quantity = OLD.quantity
  AND NEW.equipment_role = OLD.equipment_role
  AND NEW.evidence_claim_id = OLD.evidence_claim_id
  AND NEW.created_at = OLD.created_at
  AND EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = guard.duplicate_model_id
    WHERE guard.duplicate_model_id = OLD.avionics_model_id
      AND guard.survivor_model_id = NEW.avionics_model_id
  )
)
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_reference_avionics_immutable_delete;
CREATE TRIGGER aircraft_reference_avionics_immutable_delete
BEFORE DELETE ON aircraft_reference_avionics
WHEN EXISTS (
  SELECT 1
  FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN
  SELECT RAISE(ABORT, 'published reference profile facts are immutable');
END;

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_aircraft_sale_listing_avionics_unique_displacement
  ON aircraft_sale_listing_avionics (
    aircraft_sale_listing_id, replaces_avionics_model_id
  )
  WHERE configuration_action IN ('replaces', 'removes');

DROP VIEW IF EXISTS avionics_semantic_invalid_listing_action_graphs;
DROP VIEW IF EXISTS avionics_semantic_installed_displacement_conflicts;
DROP VIEW IF EXISTS avionics_semantic_duplicate_displacement_targets;
DROP VIEW IF EXISTS avionics_semantic_invalid_replacement_links;
DROP VIEW IF EXISTS avionics_semantic_duplicate_listing_links;
CREATE VIEW avionics_semantic_duplicate_listing_links AS
SELECT
  link.aircraft_sale_listing_id AS listing_id,
  identity.avionics_manufacturer_identity_id,
  identity.canonical_product_key,
  COUNT(*) AS link_count
FROM aircraft_sale_listing_avionics link
JOIN avionics_approved_product_graph_identities identity
  ON identity.avionics_model_id = link.avionics_model_id
WHERE link.configuration_action IN ('installed', 'replaces')
GROUP BY
  link.aircraft_sale_listing_id,
  identity.avionics_manufacturer_identity_id,
  identity.canonical_product_key
HAVING COUNT(*) > 1;

CREATE VIEW avionics_semantic_invalid_replacement_links AS
SELECT link.id AS listing_link_id, link.aircraft_sale_listing_id AS listing_id
FROM aircraft_sale_listing_avionics link
LEFT JOIN avionics_approved_product_graph_identities subject
  ON subject.avionics_model_id = link.avionics_model_id
LEFT JOIN avionics_approved_product_graph_identities displaced
  ON displaced.avionics_model_id = link.replaces_avionics_model_id
WHERE (link.configuration_action = 'installed'
    AND link.replaces_avionics_model_id IS NOT NULL)
  OR (link.configuration_action = 'replaces' AND (
    link.replaces_avionics_model_id IS NULL
    OR link.replaces_avionics_model_id = link.avionics_model_id
    OR (
      subject.avionics_manufacturer_identity_id
        = displaced.avionics_manufacturer_identity_id
      AND subject.canonical_product_key = displaced.canonical_product_key
    )
  ))
  OR (link.configuration_action = 'removes'
    AND link.replaces_avionics_model_id IS NOT link.avionics_model_id);

CREATE VIEW avionics_semantic_duplicate_displacement_targets AS
SELECT
  link.aircraft_sale_listing_id AS listing_id,
  displaced.avionics_manufacturer_identity_id,
  displaced.canonical_product_key,
  COUNT(*) AS link_count
FROM aircraft_sale_listing_avionics link
JOIN avionics_approved_product_graph_identities displaced
  ON displaced.avionics_model_id = link.replaces_avionics_model_id
WHERE link.configuration_action IN ('replaces', 'removes')
GROUP BY
  link.aircraft_sale_listing_id,
  displaced.avionics_manufacturer_identity_id,
  displaced.canonical_product_key
HAVING COUNT(*) > 1;

CREATE VIEW avionics_semantic_installed_displacement_conflicts AS
SELECT DISTINCT
  installed.aircraft_sale_listing_id AS listing_id,
  subject.avionics_manufacturer_identity_id,
  subject.canonical_product_key
FROM aircraft_sale_listing_avionics installed
JOIN avionics_approved_product_graph_identities subject
  ON subject.avionics_model_id = installed.avionics_model_id
JOIN aircraft_sale_listing_avionics displacement
  ON displacement.aircraft_sale_listing_id
    = installed.aircraft_sale_listing_id
 AND displacement.configuration_action IN ('replaces', 'removes')
JOIN avionics_approved_product_graph_identities displaced
  ON displaced.avionics_model_id
    = displacement.replaces_avionics_model_id
 AND displaced.avionics_manufacturer_identity_id
    = subject.avionics_manufacturer_identity_id
 AND displaced.canonical_product_key = subject.canonical_product_key
WHERE installed.configuration_action IN ('installed', 'replaces');

CREATE VIEW avionics_semantic_invalid_listing_action_graphs AS
SELECT listing_id, 'duplicate_installed_subject' AS issue
FROM avionics_semantic_duplicate_listing_links
UNION
SELECT listing_id, 'invalid_action_subject_target' AS issue
FROM avionics_semantic_invalid_replacement_links
UNION
SELECT listing_id, 'duplicate_displacement_target' AS issue
FROM avionics_semantic_duplicate_displacement_targets
UNION
SELECT listing_id, 'installed_subject_is_displaced' AS issue
FROM avionics_semantic_installed_displacement_conflicts;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_mutable_insert;
CREATE TRIGGER aircraft_sale_listing_avionics_mutable_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id = NEW.aircraft_sale_listing_id
    AND (listing.ingestion_state = 'ready' OR listing.is_verified = 1)
)
BEGIN SELECT RAISE(ABORT, 'ready or verified listing avionics are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_mutable_update;
CREATE TRIGGER aircraft_sale_listing_avionics_mutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id IN (
      OLD.aircraft_sale_listing_id, NEW.aircraft_sale_listing_id
    )
    AND (listing.ingestion_state = 'ready' OR listing.is_verified = 1)
)
BEGIN SELECT RAISE(ABORT, 'ready or verified listing avionics are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_mutable_delete;
CREATE TRIGGER aircraft_sale_listing_avionics_mutable_delete
BEFORE DELETE ON aircraft_sale_listing_avionics
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id = OLD.aircraft_sale_listing_id
    AND (listing.ingestion_state = 'ready' OR listing.is_verified = 1)
)
BEGIN SELECT RAISE(ABORT, 'ready or verified listing avionics are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_distinct_replacement_insert;
CREATE TRIGGER aircraft_sale_listing_avionics_distinct_replacement_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN (NEW.configuration_action = 'installed'
    AND NEW.replaces_avionics_model_id IS NOT NULL)
  OR (NEW.configuration_action = 'replaces' AND (
    NEW.replaces_avionics_model_id IS NULL
    OR NEW.replaces_avionics_model_id = NEW.avionics_model_id
    OR EXISTS (
      SELECT 1
      FROM avionics_approved_product_graph_identities subject
      JOIN avionics_approved_product_graph_identities displaced
        ON displaced.avionics_model_id = NEW.replaces_avionics_model_id
       AND displaced.avionics_manufacturer_identity_id
         = subject.avionics_manufacturer_identity_id
       AND displaced.canonical_product_key = subject.canonical_product_key
      WHERE subject.avionics_model_id = NEW.avionics_model_id
    )
  ))
  OR (NEW.configuration_action = 'removes'
    AND NEW.replaces_avionics_model_id IS NOT NEW.avionics_model_id)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics action has invalid subject/target semantics');
END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_distinct_replacement_update;
CREATE TRIGGER aircraft_sale_listing_avionics_distinct_replacement_update
BEFORE UPDATE OF avionics_model_id, configuration_action, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
WHEN (NEW.configuration_action = 'installed'
    AND NEW.replaces_avionics_model_id IS NOT NULL)
  OR (NEW.configuration_action = 'replaces' AND (
    NEW.replaces_avionics_model_id IS NULL
    OR NEW.replaces_avionics_model_id = NEW.avionics_model_id
    OR EXISTS (
      SELECT 1
      FROM avionics_approved_product_graph_identities subject
      JOIN avionics_approved_product_graph_identities displaced
        ON displaced.avionics_model_id = NEW.replaces_avionics_model_id
       AND displaced.avionics_manufacturer_identity_id
         = subject.avionics_manufacturer_identity_id
       AND displaced.canonical_product_key = subject.canonical_product_key
      WHERE subject.avionics_model_id = NEW.avionics_model_id
    )
  ))
  OR (NEW.configuration_action = 'removes'
    AND NEW.replaces_avionics_model_id IS NOT NEW.avionics_model_id)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics action has invalid subject/target semantics');
END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_semantic_unique_insert;
CREATE TRIGGER aircraft_sale_listing_avionics_semantic_unique_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN NEW.configuration_action IN ('installed', 'replaces')
AND EXISTS (
  SELECT 1
  FROM avionics_approved_product_graph_identities candidate
  JOIN aircraft_sale_listing_avionics existing
    ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
   AND existing.configuration_action IN ('installed', 'replaces')
  JOIN avionics_approved_product_graph_identities existing_identity
    ON existing_identity.avionics_model_id = existing.avionics_model_id
   AND existing_identity.avionics_manufacturer_identity_id
     = candidate.avionics_manufacturer_identity_id
   AND existing_identity.canonical_product_key = candidate.canonical_product_key
  WHERE candidate.avionics_model_id = NEW.avionics_model_id
)
BEGIN
  SELECT RAISE(ABORT, 'listing cannot install one canonical avionics product more than once');
END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_semantic_unique_update;
CREATE TRIGGER aircraft_sale_listing_avionics_semantic_unique_update
BEFORE UPDATE OF aircraft_sale_listing_id, avionics_model_id, configuration_action
ON aircraft_sale_listing_avionics
WHEN NEW.configuration_action IN ('installed', 'replaces')
AND EXISTS (
  SELECT 1
  FROM avionics_approved_product_graph_identities candidate
  JOIN aircraft_sale_listing_avionics existing
    ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
   AND existing.id <> OLD.id
   AND existing.configuration_action IN ('installed', 'replaces')
  JOIN avionics_approved_product_graph_identities existing_identity
    ON existing_identity.avionics_model_id = existing.avionics_model_id
   AND existing_identity.avionics_manufacturer_identity_id
     = candidate.avionics_manufacturer_identity_id
   AND existing_identity.canonical_product_key = candidate.canonical_product_key
  WHERE candidate.avionics_model_id = NEW.avionics_model_id
)
BEGIN
  SELECT RAISE(ABORT, 'listing cannot install one canonical avionics product more than once');
END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_action_graph_insert;
CREATE TRIGGER aircraft_sale_listing_avionics_action_graph_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.configuration_action IN ('replaces', 'removes')
    JOIN avionics_approved_product_graph_identities existing_target
      ON existing_target.avionics_model_id
        = existing.replaces_avionics_model_id
     AND existing_target.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_target.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.replaces_avionics_model_id
  )
)
OR (
  NEW.configuration_action IN ('installed', 'replaces')
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.configuration_action IN ('replaces', 'removes')
    JOIN avionics_approved_product_graph_identities existing_target
      ON existing_target.avionics_model_id
        = existing.replaces_avionics_model_id
     AND existing_target.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_target.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.avionics_model_id
  )
)
OR (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.configuration_action IN ('installed', 'replaces')
    JOIN avionics_approved_product_graph_identities existing_subject
      ON existing_subject.avionics_model_id = existing.avionics_model_id
     AND existing_subject.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_subject.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.replaces_avionics_model_id
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics action graph has duplicate or contradictory installed/displaced identities');
END;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_action_graph_update;
CREATE TRIGGER aircraft_sale_listing_avionics_action_graph_update
BEFORE UPDATE OF aircraft_sale_listing_id, avionics_model_id,
  configuration_action, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
WHEN (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.id <> OLD.id
     AND existing.configuration_action IN ('replaces', 'removes')
    JOIN avionics_approved_product_graph_identities existing_target
      ON existing_target.avionics_model_id
        = existing.replaces_avionics_model_id
     AND existing_target.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_target.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.replaces_avionics_model_id
  )
)
OR (
  NEW.configuration_action IN ('installed', 'replaces')
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.id <> OLD.id
     AND existing.configuration_action IN ('replaces', 'removes')
    JOIN avionics_approved_product_graph_identities existing_target
      ON existing_target.avionics_model_id
        = existing.replaces_avionics_model_id
     AND existing_target.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_target.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.avionics_model_id
  )
)
OR (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.id <> OLD.id
     AND existing.configuration_action IN ('installed', 'replaces')
    JOIN avionics_approved_product_graph_identities existing_subject
      ON existing_subject.avionics_model_id = existing.avionics_model_id
     AND existing_subject.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_subject.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.replaces_avionics_model_id
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics action graph has duplicate or contradictory installed/displaced identities');
END;

DROP TRIGGER IF EXISTS aircraft_sale_listings_ready_semantic_avionics;
CREATE TRIGGER aircraft_sale_listings_ready_semantic_avionics
BEFORE UPDATE OF ingestion_state ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
AND (
  EXISTS (
    SELECT 1
    FROM avionics_semantic_invalid_listing_action_graphs invalid_graph
    WHERE invalid_graph.listing_id = NEW.id
  )
  OR EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_avionics link
    JOIN avionics_models model ON model.id = link.avionics_model_id
    WHERE link.aircraft_sale_listing_id = NEW.id
      AND model.catalog_status <> 'approved'
  )
  OR EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_avionics link
    JOIN avionics_models model ON model.id = link.replaces_avionics_model_id
    WHERE link.aircraft_sale_listing_id = NEW.id
      AND model.catalog_status <> 'approved'
  )
  OR EXISTS (
    SELECT 1 FROM aircraft_sale_listing_avionics link
    WHERE link.aircraft_sale_listing_id = NEW.id
      AND (
        link.quantity <= 0
        OR link.source_confidence IS NOT 'high'
        OR link.source NOT IN ('listing', 'listing_review')
      )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'ready listing requires unique approved canonical avionics');
END;

DROP TRIGGER IF EXISTS aircraft_sale_listings_ready_semantic_avionics_insert;
CREATE TRIGGER aircraft_sale_listings_ready_semantic_avionics_insert
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
BEGIN
  SELECT RAISE(ABORT, 'listing cannot be inserted ready before avionics are validated');
END;

DROP TRIGGER IF EXISTS listing_verified_requires_ready_insert;
CREATE TRIGGER listing_verified_requires_ready_insert
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.is_verified = 1 AND NEW.ingestion_state <> 'ready'
BEGIN
  SELECT RAISE(ABORT, 'verified listing must be in the ready ingestion state');
END;

DROP TRIGGER IF EXISTS listing_verified_requires_ready_update;
CREATE TRIGGER listing_verified_requires_ready_update
BEFORE UPDATE OF is_verified, ingestion_state ON aircraft_sale_listings
WHEN NEW.is_verified = 1 AND NEW.ingestion_state <> 'ready'
BEGIN
  SELECT RAISE(ABORT, 'verified listing must be in the ready ingestion state');
END;

-- A verified flag on a non-publishable row was a legacy state bypass. Preserve
-- the row for review, but remove it from every published/verified path.
UPDATE aircraft_sale_listings
SET ingestion_state = 'quarantined',
    is_verified = 0,
    ingestion_error =
      'Identity postcondition migration: verified listing was not in the ready ingestion state.',
    ingestion_completed_at = NULL,
    updated_at = CURRENT_TIMESTAMP
WHERE is_verified = 1
  AND ingestion_state <> 'ready';

-- Do not grandfather invalid published data. Rows are preserved, made private,
-- and carry a deterministic reason for repair/re-review.
UPDATE aircraft_sale_listings
SET ingestion_state = 'quarantined',
    is_verified = 0,
    ingestion_error = CASE
      WHEN EXISTS (
        SELECT 1
        FROM aircraft_sale_listing_avionics link
        LEFT JOIN avionics_models installed ON installed.id = link.avionics_model_id
        LEFT JOIN avionics_models replaced ON replaced.id = link.replaces_avionics_model_id
        WHERE link.aircraft_sale_listing_id = aircraft_sale_listings.id
          AND (
            installed.catalog_status IS NOT 'approved'
            OR (link.replaces_avionics_model_id IS NOT NULL
              AND replaced.catalog_status IS NOT 'approved')
          )
      ) THEN 'Identity postcondition migration: listing avionics include an unapproved catalog product.'
      WHEN EXISTS (
        SELECT 1
        FROM avionics_semantic_invalid_listing_action_graphs invalid_graph
        WHERE invalid_graph.listing_id = aircraft_sale_listings.id
      ) THEN 'Identity postcondition migration: listing has an invalid canonical avionics action graph.'
      ELSE 'Identity postcondition migration: listing avionics lack positive quantity and high-confidence listing/reviewer evidence.'
    END,
    ingestion_completed_at = NULL,
    updated_at = CURRENT_TIMESTAMP
WHERE ingestion_state = 'ready'
  AND (
    EXISTS (
      SELECT 1
      FROM aircraft_sale_listing_avionics link
      LEFT JOIN avionics_models installed ON installed.id = link.avionics_model_id
      LEFT JOIN avionics_models replaced ON replaced.id = link.replaces_avionics_model_id
      WHERE link.aircraft_sale_listing_id = aircraft_sale_listings.id
        AND (
          installed.catalog_status IS NOT 'approved'
          OR (link.replaces_avionics_model_id IS NOT NULL
            AND replaced.catalog_status IS NOT 'approved')
          OR link.quantity <= 0
          OR link.source_confidence IS NOT 'high'
          OR link.source NOT IN ('listing', 'listing_review')
        )
    )
    OR EXISTS (
      SELECT 1
      FROM avionics_semantic_invalid_listing_action_graphs invalid_graph
      WHERE invalid_graph.listing_id = aircraft_sale_listings.id
    )
  );

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260725_identity_deduplication_postconditions',
  6,
  'cd001240b48a1480fd8bbee39b9ddedbba01d00fad45cbac315cec7a243cf133',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
PRAGMA foreign_key_check;
