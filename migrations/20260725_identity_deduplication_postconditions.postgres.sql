-- Canonical avionics identity postconditions and the narrowly-scoped legacy
-- consolidation guard. Existing unreviewed collisions remain migratable;
-- approved catalog identities and ready listing associations are strict.

BEGIN;

SET LOCAL search_path = public, pg_catalog, pg_temp;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(migration_name)) > 0)
);

LOCK TABLE ONLY public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1 FROM ONLY public.schema_migration_contracts
    WHERE migration_name = '20260725_identity_deduplication_postconditions'
      AND (
        contract_version IS DISTINCT FROM 6
        OR contract_fingerprint IS DISTINCT FROM
          'cd001240b48a1480fd8bbee39b9ddedbba01d00fad45cbac315cec7a243cf133'
      )
  ) THEN
    RAISE EXCEPTION
      'installed identity deduplication postconditions migration has a different contract';
  END IF;
END
$migration_guard$;

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
    AND BTRIM(normalized_manufacturer_identifier) <> '';

CREATE TABLE IF NOT EXISTS avionics_manufacturer_canonical_keys (
  avionics_manufacturer_id BIGINT PRIMARY KEY
    REFERENCES avionics_manufacturers(id) ON DELETE CASCADE,
  canonical_manufacturer_key TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (avionics_manufacturer_id, canonical_manufacturer_key),
  CHECK (canonical_manufacturer_key ~ '^[a-z0-9]+$')
);

CREATE INDEX IF NOT EXISTS idx_avionics_manufacturer_canonical_keys_lookup
  ON avionics_manufacturer_canonical_keys (canonical_manufacturer_key);

CREATE OR REPLACE VIEW avionics_manufacturer_normalization_contract AS
WITH separated AS (
  SELECT
    manufacturer.id AS avionics_manufacturer_id,
    manufacturer.name,
    manufacturer.normalized_name,
    ' ' || LOWER(REPLACE(REPLACE(REPLACE(REPLACE(
      BTRIM(manufacturer.name), '-', ' '), '/', ' '), '.', ' '), '_', '')) || ' '
      AS raw_name_tokens
  FROM avionics_manufacturers manufacturer
),
suffix_stripped AS (
  SELECT
    separated.*,
    REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
      separated.raw_name_tokens,
      ' co ', ' '), ' company ', ' '), ' corp ', ' '),
      ' corporation ', ' '), ' inc ', ' '), ' incorporated ', ' '),
      ' llc ', ' '), ' ltd ', ' '), ' limited ', ' ') AS raw_core_tokens
  FROM separated
)
SELECT
  avionics_manufacturer_id,
  CASE
    WHEN REPLACE(raw_core_tokens, ' ', '')
      IN ('cessnaaircraft', 'textronaviation') THEN 'cessna'
    WHEN REPLACE(raw_core_tokens, ' ', '')
      IN ('cirrusaircraft', 'cirrusdesign') THEN 'cirrus'
    WHEN REPLACE(raw_core_tokens, ' ', '')
      IN ('theairplanefactory', 'slingaircraft', 'slingairplane') THEN 'sling'
    ELSE REPLACE(raw_core_tokens, ' ', '')
  END AS deterministic_name_key,
  LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
    BTRIM(normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    AS stored_name_key,
  (
    BTRIM(name) <> ''
    AND name ~ '^[A-Za-z0-9 ./_-]+$'
    AND BTRIM(normalized_name) <> ''
    AND normalized_name ~ '^[A-Za-z0-9 ./_-]+$'
  ) AS uses_supported_ascii
FROM suffix_stripped;

INSERT INTO avionics_manufacturer_canonical_keys (
  avionics_manufacturer_id, canonical_manufacturer_key
)
SELECT
  manufacturer.id,
  LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
    BTRIM(manufacturer.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
FROM avionics_manufacturers manufacturer
ON CONFLICT (avionics_manufacturer_id) DO NOTHING;

CREATE OR REPLACE FUNCTION insert_avionics_manufacturer_canonical_key()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  INSERT INTO avionics_manufacturer_canonical_keys (
    avionics_manufacturer_id, canonical_manufacturer_key
  ) VALUES (
    NEW.id,
    LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
      BTRIM(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  );
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_canonical_key_insert
  ON avionics_manufacturers;
CREATE TRIGGER avionics_manufacturer_canonical_key_insert
AFTER INSERT ON avionics_manufacturers
FOR EACH ROW EXECUTE FUNCTION insert_avionics_manufacturer_canonical_key();

CREATE OR REPLACE FUNCTION preserve_avionics_manufacturer_canonical_key()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'avionics manufacturer canonical key is immutable';
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_canonical_key_immutable
  ON avionics_manufacturer_canonical_keys;
CREATE TRIGGER avionics_manufacturer_canonical_key_immutable
BEFORE UPDATE OF canonical_manufacturer_key ON avionics_manufacturer_canonical_keys
FOR EACH ROW EXECUTE FUNCTION preserve_avionics_manufacturer_canonical_key();

CREATE OR REPLACE FUNCTION prevent_avionics_manufacturer_canonical_key_delete()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF pg_trigger_depth() <= 1 AND EXISTS (
    SELECT 1 FROM avionics_manufacturers manufacturer
    WHERE manufacturer.id = OLD.avionics_manufacturer_id
  ) THEN
    RAISE EXCEPTION 'avionics manufacturer canonical key cannot be deleted directly';
  END IF;
  RETURN OLD;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_canonical_key_delete
  ON avionics_manufacturer_canonical_keys;
CREATE TRIGGER avionics_manufacturer_canonical_key_delete
BEFORE DELETE ON avionics_manufacturer_canonical_keys
FOR EACH ROW EXECUTE FUNCTION prevent_avionics_manufacturer_canonical_key_delete();

CREATE OR REPLACE FUNCTION preserve_avionics_manufacturer_normalized_key()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM avionics_manufacturer_canonical_keys manufacturer_key
    WHERE manufacturer_key.avionics_manufacturer_id = OLD.id
      AND manufacturer_key.canonical_manufacturer_key = LOWER(REPLACE(REPLACE(
        REPLACE(REPLACE(REPLACE(BTRIM(NEW.normalized_name), ' ', ''), '-', ''),
        '/', ''), '.', ''), '_', ''))
  ) THEN
    RAISE EXCEPTION 'manufacturer normalization cannot change its canonical key';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_normalized_name_preserve_key
  ON avionics_manufacturers;
CREATE TRIGGER avionics_manufacturer_normalized_name_preserve_key
BEFORE UPDATE OF normalized_name ON avionics_manufacturers
FOR EACH ROW EXECUTE FUNCTION preserve_avionics_manufacturer_normalized_key();

CREATE TABLE IF NOT EXISTS avionics_manufacturer_identities (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  canonical_name TEXT NOT NULL CHECK (BTRIM(canonical_name) <> ''),
  normalized_identity_key TEXT NOT NULL UNIQUE
    CHECK (normalized_identity_key ~ '^[a-z0-9]+$'),
  identity_evidence_kind TEXT NOT NULL
    CHECK (identity_evidence_kind = 'authoritative_reference'),
  identity_source_url TEXT NOT NULL CHECK (BTRIM(identity_source_url) <> ''),
  identity_source_title TEXT NOT NULL CHECK (BTRIM(identity_source_title) <> ''),
  identity_evidence_text TEXT NOT NULL CHECK (BTRIM(identity_evidence_text) <> ''),
  identity_confidence TEXT NOT NULL CHECK (identity_confidence = 'very_high'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (LOWER(identity_source_url) LIKE 'https://%')
);

CREATE TABLE IF NOT EXISTS avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id BIGINT PRIMARY KEY
    REFERENCES avionics_manufacturers(id) ON DELETE RESTRICT,
  avionics_manufacturer_identity_id BIGINT NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  membership_basis TEXT NOT NULL CHECK (membership_basis IN (
    'deterministic_exact', 'authoritative_primary', 'authoritative_alias'
  )),
  normalized_name_key TEXT NOT NULL
    CHECK (normalized_name_key ~ '^[a-z0-9]+$'),
  evidence_source_url TEXT NOT NULL CHECK (BTRIM(evidence_source_url) <> ''),
  evidence_source_title TEXT NOT NULL CHECK (BTRIM(evidence_source_title) <> ''),
  evidence_text TEXT NOT NULL CHECK (BTRIM(evidence_text) <> ''),
  evidence_confidence TEXT NOT NULL CHECK (evidence_confidence = 'very_high'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_avionics_manufacturer_identity_memberships_group
  ON avionics_manufacturer_identity_memberships (
    avionics_manufacturer_identity_id, avionics_manufacturer_id
  );

CREATE OR REPLACE FUNCTION preserve_avionics_manufacturer_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'approved avionics manufacturer identities are immutable';
  RETURN NULL;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_immutable
  ON avionics_manufacturer_identities;
CREATE TRIGGER avionics_manufacturer_identity_immutable
BEFORE UPDATE OR DELETE ON avionics_manufacturer_identities
FOR EACH ROW EXECUTE FUNCTION preserve_avionics_manufacturer_identity();

CREATE OR REPLACE FUNCTION validate_avionics_manufacturer_membership()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM avionics_manufacturer_canonical_keys manufacturer_key
    JOIN avionics_manufacturer_normalization_contract normalization
      ON normalization.avionics_manufacturer_id
        = manufacturer_key.avionics_manufacturer_id
    JOIN avionics_manufacturer_identities identity
      ON identity.id = NEW.avionics_manufacturer_identity_id
    WHERE manufacturer_key.avionics_manufacturer_id = NEW.avionics_manufacturer_id
      AND manufacturer_key.canonical_manufacturer_key = NEW.normalized_name_key
      AND normalization.uses_supported_ascii
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
  ) THEN
    RAISE EXCEPTION 'manufacturer membership lacks exact normalization or authoritative alias evidence';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_membership_validate_insert
  ON avionics_manufacturer_identity_memberships;
CREATE TRIGGER avionics_manufacturer_membership_validate_insert
BEFORE INSERT ON avionics_manufacturer_identity_memberships
FOR EACH ROW EXECUTE FUNCTION validate_avionics_manufacturer_membership();

CREATE OR REPLACE FUNCTION preserve_avionics_manufacturer_membership()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'avionics manufacturer identity memberships are immutable';
  RETURN NULL;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_membership_immutable
  ON avionics_manufacturer_identity_memberships;
CREATE TRIGGER avionics_manufacturer_membership_immutable
BEFORE UPDATE OR DELETE ON avionics_manufacturer_identity_memberships
FOR EACH ROW EXECUTE FUNCTION preserve_avionics_manufacturer_membership();

CREATE OR REPLACE FUNCTION preserve_identified_avionics_manufacturer_name()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM avionics_manufacturer_identity_memberships membership
    WHERE membership.avionics_manufacturer_id = OLD.id
  )
  AND (
    NEW.name IS DISTINCT FROM OLD.name
    OR NEW.normalized_name IS DISTINCT FROM OLD.normalized_name
  ) THEN
    RAISE EXCEPTION 'evidence-backed avionics manufacturer name is immutable';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_name_immutable
  ON avionics_manufacturers;
CREATE TRIGGER avionics_manufacturer_identity_name_immutable
BEFORE UPDATE OF name, normalized_name ON avionics_manufacturers
FOR EACH ROW EXECUTE FUNCTION preserve_identified_avionics_manufacturer_name();

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

DO $normalization_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM avionics_manufacturer_identity_memberships membership
    JOIN avionics_manufacturer_canonical_keys manufacturer_key
      ON manufacturer_key.avionics_manufacturer_id
        = membership.avionics_manufacturer_id
    LEFT JOIN avionics_manufacturer_normalization_contract normalization
      ON normalization.avionics_manufacturer_id
        = membership.avionics_manufacturer_id
    WHERE normalization.avionics_manufacturer_id IS NULL
       OR NOT normalization.uses_supported_ascii
       OR normalization.deterministic_name_key
         <> normalization.stored_name_key
       OR normalization.stored_name_key
         <> manufacturer_key.canonical_manufacturer_key
       OR membership.normalized_name_key
         <> manufacturer_key.canonical_manufacturer_key
  ) OR EXISTS (
    SELECT 1
    FROM avionics_models model
    WHERE model.catalog_status = 'approved'
      AND (
        model.name !~ '^[A-Za-z0-9 ./_-]+$'
        OR model.normalized_name !~ '^[A-Za-z0-9 ./_-]+$'
        OR LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
          BTRIM(model.name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
          <> LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
            BTRIM(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
        OR model.manufacturer_identifier IS NULL
        OR model.manufacturer_identifier !~ '^[A-Za-z0-9 ./_-]+$'
        OR model.normalized_manufacturer_identifier IS NULL
        OR model.normalized_manufacturer_identifier
          !~ '^[A-Za-z0-9 ./_-]+$'
        OR LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
          BTRIM(model.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
          <> LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
            BTRIM(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      )
  ) THEN
    RAISE EXCEPTION 'identity migration found persisted normalization keys detached from raw avionics evidence';
  END IF;
END;
$normalization_guard$;

DO $shape_guard$
BEGIN
  IF to_regclass('public.avionics_approved_product_identities') IS NOT NULL
    AND (
      NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'avionics_approved_product_identities'
          AND column_name = 'avionics_manufacturer_identity_id'
      )
      OR EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'avionics_approved_product_identities'
          AND column_name IN (
            'avionics_manufacturer_id', 'canonical_manufacturer_key'
          )
      )
    ) THEN
    RAISE EXCEPTION 'unexpected avionics approved-product identity registry shape';
  END IF;
END;
$shape_guard$;

CREATE TABLE IF NOT EXISTS avionics_approved_product_identities (
  avionics_model_id BIGINT PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_manufacturer_identity_id BIGINT NOT NULL
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
  CHECK (canonical_product_key ~ '^[a-z0-9]+$'),
  CHECK (canonical_identifier_key ~ '^[a-z0-9]+$')
);

CREATE OR REPLACE VIEW avionics_approved_product_graph_identities AS
SELECT avionics_model_id, avionics_manufacturer_identity_id,
       canonical_product_key, manufacturer_identifier_kind,
       canonical_identifier_key
FROM avionics_approved_product_identities;

CREATE TABLE IF NOT EXISTS avionics_manufacturer_alias_candidates (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  avionics_manufacturer_id BIGINT NOT NULL
    REFERENCES avionics_manufacturers(id) ON DELETE RESTRICT,
  candidate_manufacturer_identity_id BIGINT NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  candidate_basis TEXT NOT NULL CHECK (candidate_basis IN (
    'exact_product_name', 'exact_stable_identifier',
    'semantic_similarity', 'grounded_alias'
  )),
  matched_avionics_model_id BIGINT
    REFERENCES avionics_models(id) ON DELETE SET NULL,
  reason TEXT NOT NULL CHECK (BTRIM(reason) <> ''),
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
  reviewed_by_user_id BIGINT REFERENCES users(id) ON DELETE RESTRICT,
  reviewed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (evidence_source_url IS NULL
      AND evidence_source_title IS NULL
      AND evidence_text IS NULL)
    OR (evidence_source_url IS NOT NULL
      AND LOWER(evidence_source_url) LIKE 'https://%'
      AND evidence_source_title IS NOT NULL
      AND BTRIM(evidence_source_title) <> ''
      AND evidence_text IS NOT NULL
      AND BTRIM(evidence_text) <> '')
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
      AND BTRIM(decision_reason) <> ''
      AND reviewed_by_user_id IS NOT NULL
      AND reviewed_at IS NOT NULL)
    OR (review_status = 'approved'
      AND decision_reason IS NOT NULL
      AND BTRIM(decision_reason) <> ''
      AND decision_evidence_source_url IS NOT NULL
      AND LOWER(decision_evidence_source_url) LIKE 'https://%'
      AND decision_evidence_source_title IS NOT NULL
      AND BTRIM(decision_evidence_source_title) <> ''
      AND decision_evidence_text IS NOT NULL
      AND BTRIM(decision_evidence_text) <> ''
      AND reviewed_by_user_id IS NOT NULL
      AND reviewed_at IS NOT NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_avionics_manufacturer_alias_candidates_pending
  ON avionics_manufacturer_alias_candidates (
    avionics_manufacturer_id, candidate_manufacturer_identity_id
  )
  WHERE review_status = 'pending';

CREATE OR REPLACE FUNCTION require_pending_avionics_manufacturer_alias_candidate()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NEW.review_status <> 'pending' THEN
    RAISE EXCEPTION 'manufacturer alias candidates must be inserted pending';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_alias_candidate_pending_insert
  ON avionics_manufacturer_alias_candidates;
CREATE TRIGGER avionics_manufacturer_alias_candidate_pending_insert
BEFORE INSERT ON avionics_manufacturer_alias_candidates
FOR EACH ROW EXECUTE FUNCTION require_pending_avionics_manufacturer_alias_candidate();

CREATE OR REPLACE FUNCTION preserve_avionics_manufacturer_alias_candidate()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'manufacturer alias candidate history is immutable';
  END IF;
  IF NOT (
      NEW.id = OLD.id
      AND NEW.avionics_manufacturer_id = OLD.avionics_manufacturer_id
      AND NEW.candidate_manufacturer_identity_id
        = OLD.candidate_manufacturer_identity_id
      AND NEW.candidate_basis = OLD.candidate_basis
      AND NEW.matched_avionics_model_id
        IS DISTINCT FROM OLD.matched_avionics_model_id
      AND NEW.reason = OLD.reason
      AND NEW.evidence_source_url IS NOT DISTINCT FROM OLD.evidence_source_url
      AND NEW.evidence_source_title IS NOT DISTINCT FROM OLD.evidence_source_title
      AND NEW.evidence_text IS NOT DISTINCT FROM OLD.evidence_text
      AND NEW.confidence = OLD.confidence
      AND NEW.review_status = OLD.review_status
      AND NEW.decision_reason IS NOT DISTINCT FROM OLD.decision_reason
      AND NEW.decision_evidence_source_url
        IS NOT DISTINCT FROM OLD.decision_evidence_source_url
      AND NEW.decision_evidence_source_title
        IS NOT DISTINCT FROM OLD.decision_evidence_source_title
      AND NEW.decision_evidence_text
        IS NOT DISTINCT FROM OLD.decision_evidence_text
      AND NEW.reviewed_by_user_id IS NOT DISTINCT FROM OLD.reviewed_by_user_id
      AND NEW.reviewed_at IS NOT DISTINCT FROM OLD.reviewed_at
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
      OR NEW.matched_avionics_model_id
        IS DISTINCT FROM OLD.matched_avionics_model_id
      OR NEW.reason <> OLD.reason
      OR NEW.evidence_source_url IS DISTINCT FROM OLD.evidence_source_url
      OR NEW.evidence_source_title IS DISTINCT FROM OLD.evidence_source_title
      OR NEW.evidence_text IS DISTINCT FROM OLD.evidence_text
      OR NEW.confidence <> OLD.confidence
      OR NEW.review_status NOT IN ('approved', 'rejected')
    ) THEN
    RAISE EXCEPTION 'manufacturer alias candidates are immutable after staging except for one review decision';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_alias_candidate_immutable
  ON avionics_manufacturer_alias_candidates;
CREATE TRIGGER avionics_manufacturer_alias_candidate_immutable
BEFORE UPDATE OR DELETE ON avionics_manufacturer_alias_candidates
FOR EACH ROW EXECUTE FUNCTION preserve_avionics_manufacturer_alias_candidate();

CREATE TABLE IF NOT EXISTS avionics_manufacturer_identity_merges (
  merged_identity_id BIGINT PRIMARY KEY
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  survivor_identity_id BIGINT NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  alias_candidate_id BIGINT NOT NULL UNIQUE
    REFERENCES avionics_manufacturer_alias_candidates(id) ON DELETE RESTRICT,
  evidence_source_url TEXT NOT NULL
    CHECK (LOWER(evidence_source_url) LIKE 'https://%'),
  evidence_source_title TEXT NOT NULL CHECK (BTRIM(evidence_source_title) <> ''),
  evidence_text TEXT NOT NULL CHECK (BTRIM(evidence_text) <> ''),
  decided_by_user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (merged_identity_id <> survivor_identity_id)
);

CREATE OR REPLACE VIEW avionics_manufacturer_effective_identities AS
WITH RECURSIVE resolved(
  identity_id, effective_identity_id, depth, path
) AS (
  SELECT identity.id, identity.id, 0, ARRAY[identity.id]::BIGINT[]
  FROM avionics_manufacturer_identities identity
  UNION ALL
  SELECT resolved.identity_id, merge.survivor_identity_id,
         resolved.depth + 1,
         resolved.path || merge.survivor_identity_id
  FROM resolved
  JOIN avionics_manufacturer_identity_merges merge
    ON merge.merged_identity_id = resolved.effective_identity_id
  WHERE resolved.depth < 32
    AND NOT merge.survivor_identity_id = ANY(resolved.path)
)
SELECT resolved.identity_id,
       resolved.effective_identity_id AS avionics_manufacturer_identity_id
FROM resolved
WHERE NOT EXISTS (
  SELECT 1 FROM avionics_manufacturer_identity_merges merge
  WHERE merge.merged_identity_id = resolved.effective_identity_id
);

CREATE OR REPLACE VIEW avionics_manufacturer_effective_memberships AS
SELECT membership.avionics_manufacturer_id,
       membership.avionics_manufacturer_identity_id AS original_identity_id,
       effective.avionics_manufacturer_identity_id
FROM avionics_manufacturer_identity_memberships membership
JOIN avionics_manufacturer_effective_identities effective
  ON effective.identity_id = membership.avionics_manufacturer_identity_id;

CREATE OR REPLACE VIEW avionics_legacy_manufacturer_alias_signals AS
WITH products AS (
  SELECT model.id AS avionics_model_id,
         model.avionics_manufacturer_id,
         manufacturer.name AS manufacturer,
         model.name AS model,
         LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
           BTRIM(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
           AS canonical_product_key,
         model.manufacturer_identifier_kind,
         CASE
           WHEN model.normalized_manufacturer_identifier IS NULL THEN NULL
           ELSE LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
             BTRIM(model.normalized_manufacturer_identifier), ' ', ''), '-', ''),
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
      AND LENGTH(left_product.canonical_identifier_key) > 0
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
     AND LENGTH(left_product.canonical_identifier_key) > 0
     AND left_product.canonical_identifier_key
       = right_product.canonical_identifier_key
   )
   OR (
     LENGTH(left_product.canonical_product_key) > 0
     AND left_product.canonical_product_key
       = right_product.canonical_product_key
   )
 );

CREATE OR REPLACE FUNCTION validate_avionics_manufacturer_alias_membership()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NEW.membership_basis = 'authoritative_alias' AND NOT EXISTS (
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
  ) THEN
    RAISE EXCEPTION 'semantic manufacturer membership requires an approved authoritative alias decision';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_alias_membership_requires_decision
  ON avionics_manufacturer_identity_memberships;
CREATE TRIGGER avionics_manufacturer_alias_membership_requires_decision
BEFORE INSERT ON avionics_manufacturer_identity_memberships
FOR EACH ROW EXECUTE FUNCTION validate_avionics_manufacturer_alias_membership();

CREATE OR REPLACE FUNCTION validate_avionics_manufacturer_identity_merge()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  LOCK TABLE avionics_manufacturer_identity_merges
    IN SHARE ROW EXCLUSIVE MODE;
  IF EXISTS (
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
    ) THEN
    RAISE EXCEPTION 'manufacturer identity merge requires two roots, an approved alias decision, and no product collision';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_merge_validate
  ON avionics_manufacturer_identity_merges;
CREATE TRIGGER avionics_manufacturer_identity_merge_validate
BEFORE INSERT ON avionics_manufacturer_identity_merges
FOR EACH ROW EXECUTE FUNCTION validate_avionics_manufacturer_identity_merge();

CREATE OR REPLACE FUNCTION apply_avionics_manufacturer_identity_merge()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  UPDATE avionics_approved_product_identities
  SET avionics_manufacturer_identity_id = NEW.survivor_identity_id,
      updated_at = CURRENT_TIMESTAMP
  WHERE avionics_manufacturer_identity_id = NEW.merged_identity_id;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_merge_apply
  ON avionics_manufacturer_identity_merges;
CREATE TRIGGER avionics_manufacturer_identity_merge_apply
AFTER INSERT ON avionics_manufacturer_identity_merges
FOR EACH ROW EXECUTE FUNCTION apply_avionics_manufacturer_identity_merge();

CREATE OR REPLACE FUNCTION preserve_avionics_manufacturer_identity_merge()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'manufacturer identity merge history is immutable';
  RETURN NULL;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_manufacturer_identity_merge_immutable
  ON avionics_manufacturer_identity_merges;
CREATE TRIGGER avionics_manufacturer_identity_merge_immutable
BEFORE UPDATE OR DELETE ON avionics_manufacturer_identity_merges
FOR EACH ROW EXECUTE FUNCTION preserve_avionics_manufacturer_identity_merge();

CREATE TABLE IF NOT EXISTS avionics_catalog_consolidation_guard (
  duplicate_model_id BIGINT PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id BIGINT NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  purpose TEXT NOT NULL DEFAULT 'legacy_identity_consolidation'
    CHECK (purpose = 'legacy_identity_consolidation'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (duplicate_model_id <> survivor_model_id)
);

ALTER TABLE avionics_catalog_consolidation_guard
  DROP CONSTRAINT IF EXISTS
    avionics_catalog_consolidation_guard_duplicate_model_id_fkey;
ALTER TABLE avionics_catalog_consolidation_guard
  ADD CONSTRAINT avionics_catalog_consolidation_guard_duplicate_model_id_fkey
  FOREIGN KEY (duplicate_model_id)
  REFERENCES avionics_models(id) ON DELETE CASCADE;

CREATE OR REPLACE FUNCTION validate_avionics_catalog_consolidation_guard()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
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
  ) THEN
    RAISE EXCEPTION 'consolidation guard requires an exact product pair with an approved-or-unreviewed survivor';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_catalog_consolidation_guard_validate_insert
  ON avionics_catalog_consolidation_guard;
CREATE TRIGGER avionics_catalog_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_consolidation_guard
FOR EACH ROW EXECUTE FUNCTION validate_avionics_catalog_consolidation_guard();

CREATE OR REPLACE FUNCTION preserve_avionics_catalog_consolidation_guard()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'consolidation authorization pairs are immutable';
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_catalog_consolidation_guard_immutable
  ON avionics_catalog_consolidation_guard;
CREATE TRIGGER avionics_catalog_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_consolidation_guard
FOR EACH ROW EXECUTE FUNCTION preserve_avionics_catalog_consolidation_guard();

CREATE OR REPLACE VIEW avionics_catalog_authorized_consolidations AS
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
      BTRIM(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''));

CREATE OR REPLACE FUNCTION preserve_guarded_avionics_consolidation_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF EXISTS (
    SELECT 1 FROM avionics_catalog_consolidation_guard guard
    WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
  ) THEN
    RAISE EXCEPTION 'guarded avionics consolidation identities are immutable';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_models_consolidation_identity_immutable
  ON avionics_models;
CREATE TRIGGER avionics_models_consolidation_identity_immutable
BEFORE UPDATE OF catalog_status, avionics_manufacturer_id, name,
  normalized_name, manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
ON avionics_models
FOR EACH ROW EXECUTE FUNCTION preserve_guarded_avionics_consolidation_identity();

CREATE OR REPLACE FUNCTION validate_avionics_approved_product_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM avionics_models model
    JOIN avionics_manufacturer_effective_memberships manufacturer_identity
      ON manufacturer_identity.avionics_manufacturer_id
        = model.avionics_manufacturer_id
    WHERE model.id = NEW.avionics_model_id
      AND model.catalog_status = 'approved'
      AND manufacturer_identity.avionics_manufacturer_identity_id
        = NEW.avionics_manufacturer_identity_id
      AND LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
        BTRIM(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
        = NEW.canonical_product_key
      AND model.manufacturer_identifier_kind = NEW.manufacturer_identifier_kind
      AND LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
        BTRIM(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
        = NEW.canonical_identifier_key
  ) THEN
    RAISE EXCEPTION 'approved avionics identity must match its catalog product';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_approved_identity_validate
  ON avionics_approved_product_identities;
CREATE TRIGGER avionics_approved_identity_validate
BEFORE INSERT OR UPDATE ON avionics_approved_product_identities
FOR EACH ROW EXECUTE FUNCTION validate_avionics_approved_product_identity();

CREATE OR REPLACE FUNCTION preserve_avionics_approved_product_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF EXISTS (
    SELECT 1 FROM avionics_models model
    WHERE model.id = OLD.avionics_model_id AND model.catalog_status = 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations authorized_pair
    JOIN avionics_models survivor
      ON survivor.id = authorized_pair.survivor_model_id
    WHERE authorized_pair.duplicate_model_id = OLD.avionics_model_id
      AND survivor.catalog_status = 'approved'
  ) THEN
    RAISE EXCEPTION 'approved avionics product must retain its canonical identity';
  END IF;
  RETURN OLD;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_approved_identity_preserve_delete
  ON avionics_approved_product_identities;
CREATE TRIGGER avionics_approved_identity_preserve_delete
BEFORE DELETE ON avionics_approved_product_identities
FOR EACH ROW EXECUTE FUNCTION preserve_avionics_approved_product_identity();

CREATE OR REPLACE FUNCTION validate_avionics_model_canonical_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NEW.catalog_status = 'approved' AND (
    NOT EXISTS (
      SELECT 1
      FROM avionics_manufacturer_effective_memberships manufacturer_identity
      WHERE manufacturer_identity.avionics_manufacturer_id
        = NEW.avionics_manufacturer_id
    )
    OR LENGTH(LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
      BTRIM(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))) = 0
    OR LENGTH(LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
      BTRIM(NEW.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))) = 0
    OR NEW.name !~ '^[A-Za-z0-9 ./_-]+$'
    OR NEW.normalized_name !~ '^[A-Za-z0-9 ./_-]+$'
    OR LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
      BTRIM(NEW.name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      <> LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
        BTRIM(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    OR NEW.manufacturer_identifier IS NULL
    OR NEW.manufacturer_identifier !~ '^[A-Za-z0-9 ./_-]+$'
    OR NEW.normalized_manufacturer_identifier
      !~ '^[A-Za-z0-9 ./_-]+$'
    OR LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
      BTRIM(NEW.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      <> LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
        BTRIM(NEW.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  ) THEN
    RAISE EXCEPTION 'approved avionics product requires deterministic canonical identity keys';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_models_canonical_identity_validate_update
  ON avionics_models;
CREATE TRIGGER avionics_models_canonical_identity_validate_update
BEFORE UPDATE OF catalog_status, avionics_manufacturer_id,
  normalized_name, normalized_manufacturer_identifier
ON avionics_models
FOR EACH ROW EXECUTE FUNCTION validate_avionics_model_canonical_identity();

CREATE OR REPLACE FUNCTION sync_avionics_model_canonical_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NEW.catalog_status = 'approved' THEN
    INSERT INTO avionics_approved_product_identities (
      avionics_model_id, avionics_manufacturer_identity_id,
      canonical_product_key,
      manufacturer_identifier_kind, canonical_identifier_key
    )
    SELECT
      NEW.id,
      manufacturer_identity.avionics_manufacturer_identity_id,
      LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
        BTRIM(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', '')),
      NEW.manufacturer_identifier_kind,
      LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
        BTRIM(NEW.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    FROM avionics_manufacturer_effective_memberships manufacturer_identity
    WHERE manufacturer_identity.avionics_manufacturer_id
      = NEW.avionics_manufacturer_id
    ON CONFLICT (avionics_model_id) DO UPDATE SET
      avionics_manufacturer_identity_id
        = EXCLUDED.avionics_manufacturer_identity_id,
      canonical_product_key = EXCLUDED.canonical_product_key,
      manufacturer_identifier_kind = EXCLUDED.manufacturer_identifier_kind,
      canonical_identifier_key = EXCLUDED.canonical_identifier_key,
      updated_at = CURRENT_TIMESTAMP;
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_models_canonical_identity_sync_update
  ON avionics_models;
CREATE TRIGGER avionics_models_canonical_identity_sync_update
AFTER UPDATE OF catalog_status, avionics_manufacturer_id,
  normalized_name, normalized_manufacturer_identifier
ON avionics_models
FOR EACH ROW EXECUTE FUNCTION sync_avionics_model_canonical_identity();

CREATE OR REPLACE FUNCTION preserve_approved_avionics_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF OLD.catalog_status = 'approved' AND (
    NEW.catalog_status IS DISTINCT FROM OLD.catalog_status
    OR NEW.avionics_manufacturer_id IS DISTINCT FROM OLD.avionics_manufacturer_id
    OR NEW.name IS DISTINCT FROM OLD.name
    OR NEW.normalized_name IS DISTINCT FROM OLD.normalized_name
    OR NEW.manufacturer_identifier_kind IS DISTINCT FROM OLD.manufacturer_identifier_kind
    OR NEW.manufacturer_identifier IS DISTINCT FROM OLD.manufacturer_identifier
    OR NEW.normalized_manufacturer_identifier
      IS DISTINCT FROM OLD.normalized_manufacturer_identifier
    OR NEW.identity_source_url IS DISTINCT FROM OLD.identity_source_url
    OR NEW.identity_source_title IS DISTINCT FROM OLD.identity_source_title
    OR NEW.identity_evidence_text IS DISTINCT FROM OLD.identity_evidence_text
    OR NEW.identity_evidence_kind IS DISTINCT FROM OLD.identity_evidence_kind
    OR NEW.identity_confidence IS DISTINCT FROM OLD.identity_confidence
    OR NEW.catalog_reviewed_at IS DISTINCT FROM OLD.catalog_reviewed_at
  ) THEN
    RAISE EXCEPTION 'approved avionics product cannot be demoted or rewrite identity evidence';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_models_approved_identity_immutable
  ON avionics_models;
CREATE TRIGGER avionics_models_approved_identity_immutable
BEFORE UPDATE ON avionics_models
FOR EACH ROW EXECUTE FUNCTION preserve_approved_avionics_identity();

CREATE OR REPLACE FUNCTION guard_approved_avionics_model_delete()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF OLD.catalog_status = 'approved'
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations authorized_pair
    JOIN avionics_models survivor
      ON survivor.id = authorized_pair.survivor_model_id
    WHERE authorized_pair.duplicate_model_id = OLD.id
      AND survivor.catalog_status = 'approved'
  ) THEN
    RAISE EXCEPTION 'approved avionics product deletion requires exact consolidation authorization';
  END IF;
  RETURN OLD;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_models_approved_delete_guard
  ON avionics_models;
CREATE TRIGGER avionics_models_approved_delete_guard
BEFORE DELETE ON avionics_models
FOR EACH ROW EXECUTE FUNCTION guard_approved_avionics_model_delete();

CREATE OR REPLACE FUNCTION require_avionics_model_type_for_approval()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF TG_OP = 'INSERT' AND NEW.catalog_status = 'approved' THEN
    RAISE EXCEPTION 'avionics approval must be staged from an unreviewed product';
  END IF;
  IF TG_OP = 'UPDATE' AND NEW.catalog_status = 'approved' AND NOT EXISTS (
    SELECT 1
    FROM avionics_model_types membership
    WHERE membership.avionics_model_id = NEW.id
  ) THEN
    RAISE EXCEPTION 'approved avionics model requires at least one type';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_models_approved_types_insert
  ON avionics_models;
CREATE TRIGGER avionics_models_approved_types_insert
BEFORE INSERT ON avionics_models
FOR EACH ROW EXECUTE FUNCTION require_avionics_model_type_for_approval();

DROP TRIGGER IF EXISTS avionics_models_approved_types_update
  ON avionics_models;
CREATE TRIGGER avionics_models_approved_types_update
BEFORE UPDATE OF catalog_status ON avionics_models
FOR EACH ROW EXECUTE FUNCTION require_avionics_model_type_for_approval();

CREATE OR REPLACE FUNCTION preserve_approved_avionics_model_type()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
DECLARE
  locked_catalog_status TEXT;
BEGIN
  SELECT model.catalog_status
  INTO locked_catalog_status
  FROM avionics_models model
  WHERE model.id = OLD.avionics_model_id
  FOR UPDATE;

  IF locked_catalog_status = 'approved' AND NOT EXISTS (
    SELECT 1
    FROM avionics_model_types other
    WHERE other.avionics_model_id = OLD.avionics_model_id
      AND other.avionics_type_id <> OLD.avionics_type_id
  ) THEN
    RAISE EXCEPTION 'approved avionics model cannot lose its last type';
  END IF;
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_model_types_preserve_approved_delete
  ON avionics_model_types;
CREATE TRIGGER avionics_model_types_preserve_approved_delete
BEFORE DELETE ON avionics_model_types
FOR EACH ROW EXECUTE FUNCTION preserve_approved_avionics_model_type();

DROP TRIGGER IF EXISTS avionics_model_types_preserve_approved_update
  ON avionics_model_types;
CREATE TRIGGER avionics_model_types_preserve_approved_update
BEFORE UPDATE OF avionics_model_id ON avionics_model_types
FOR EACH ROW
WHEN (NEW.avionics_model_id IS DISTINCT FROM OLD.avionics_model_id)
EXECUTE FUNCTION preserve_approved_avionics_model_type();

CREATE OR REPLACE FUNCTION prevent_referenced_avionics_catalog_downgrade()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NEW.catalog_status <> 'approved' AND (
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
  ) THEN
    RAISE EXCEPTION 'referenced avionics catalog entry cannot be unapproved';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_models_referenced_status_update
  ON avionics_models;
CREATE TRIGGER avionics_models_referenced_status_update
BEFORE UPDATE OF catalog_status ON avionics_models
FOR EACH ROW EXECUTE FUNCTION prevent_referenced_avionics_catalog_downgrade();

-- Seed only approved products. Duplicate unreviewed candidates deliberately do
-- not occupy uniqueness slots and remain available to the consolidation pass.
INSERT INTO avionics_approved_product_identities (
  avionics_model_id, avionics_manufacturer_identity_id,
  canonical_product_key,
  manufacturer_identifier_kind, canonical_identifier_key
)
SELECT
  model.id,
  manufacturer_identity.avionics_manufacturer_identity_id,
  LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
    BTRIM(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', '')),
  model.manufacturer_identifier_kind,
  LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
    BTRIM(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
FROM avionics_models model
JOIN avionics_manufacturer_effective_memberships manufacturer_identity
  ON manufacturer_identity.avionics_manufacturer_id
    = model.avionics_manufacturer_id
WHERE model.catalog_status = 'approved'
ON CONFLICT (avionics_model_id) DO UPDATE SET
  avionics_manufacturer_identity_id
    = EXCLUDED.avionics_manufacturer_identity_id,
  canonical_product_key = EXCLUDED.canonical_product_key,
  manufacturer_identifier_kind = EXCLUDED.manufacturer_identifier_kind,
  canonical_identifier_key = EXCLUDED.canonical_identifier_key,
  updated_at = CURRENT_TIMESTAMP;

DO $avionics_approved_registry_completeness_guard$
BEGIN
  IF EXISTS (
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
            = LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
              BTRIM(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
          AND identity.manufacturer_identifier_kind
            = model.manufacturer_identifier_kind
          AND identity.canonical_identifier_key
            = LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
              BTRIM(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      )
  ) OR EXISTS (
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
         <> LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
           BTRIM(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
       OR identity.manufacturer_identifier_kind
         <> model.manufacturer_identifier_kind
       OR identity.canonical_identifier_key
         <> LOWER(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
           BTRIM(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  ) THEN
    RAISE EXCEPTION 'identity migration did not produce one matching canonical registry row per approved avionics model';
  END IF;
END;
$avionics_approved_registry_completeness_guard$;

CREATE OR REPLACE FUNCTION require_approved_avionics_suite_models()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF NEW.suite_model_id IS DISTINCT FROM OLD.suite_model_id
       AND NOT EXISTS (
         SELECT 1 FROM avionics_models model
         WHERE model.id = NEW.suite_model_id AND model.catalog_status = 'approved'
       )
       AND NOT EXISTS (
         SELECT 1 FROM avionics_catalog_authorized_consolidations guard
         JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
         JOIN avionics_models legacy ON legacy.id = guard.duplicate_model_id
         WHERE guard.duplicate_model_id = OLD.suite_model_id
           AND guard.survivor_model_id = NEW.suite_model_id
           AND survivor.catalog_status = 'unreviewed'
           AND legacy.catalog_status = 'unreviewed'
       ) THEN
      RAISE EXCEPTION 'avionics suite membership requires an approved suite catalog entry';
    END IF;
    IF NEW.component_model_id IS DISTINCT FROM OLD.component_model_id
       AND NOT EXISTS (
         SELECT 1 FROM avionics_models model
         WHERE model.id = NEW.component_model_id AND model.catalog_status = 'approved'
       )
       AND NOT EXISTS (
         SELECT 1 FROM avionics_catalog_authorized_consolidations guard
         JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
         JOIN avionics_models legacy ON legacy.id = guard.duplicate_model_id
         WHERE guard.duplicate_model_id = OLD.component_model_id
           AND guard.survivor_model_id = NEW.component_model_id
           AND survivor.catalog_status = 'unreviewed'
           AND legacy.catalog_status = 'unreviewed'
       ) THEN
      RAISE EXCEPTION 'avionics suite membership requires an approved component catalog entry';
    END IF;
  ELSIF NOT EXISTS (
    SELECT 1 FROM avionics_models model
    WHERE model.id = NEW.suite_model_id AND model.catalog_status = 'approved'
  ) OR NOT EXISTS (
    SELECT 1 FROM avionics_models model
    WHERE model.id = NEW.component_model_id AND model.catalog_status = 'approved'
  ) THEN
    RAISE EXCEPTION 'avionics suite membership requires approved catalog entries';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_suite_components_approved_insert
  ON avionics_suite_components;
CREATE TRIGGER avionics_suite_components_approved_insert
BEFORE INSERT ON avionics_suite_components
FOR EACH ROW EXECUTE FUNCTION require_approved_avionics_suite_models();
DROP TRIGGER IF EXISTS avionics_suite_components_approved_update
  ON avionics_suite_components;
CREATE TRIGGER avionics_suite_components_approved_update
BEFORE UPDATE ON avionics_suite_components
FOR EACH ROW EXECUTE FUNCTION require_approved_avionics_suite_models();

CREATE OR REPLACE FUNCTION require_approved_default_avionics_model()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM avionics_models model
    WHERE model.id = NEW.avionics_model_id AND model.catalog_status = 'approved'
  ) AND NOT (
    TG_OP = 'UPDATE'
    AND NEW.avionics_model_id IS DISTINCT FROM OLD.avionics_model_id
    AND EXISTS (
      SELECT 1 FROM avionics_catalog_authorized_consolidations guard
      JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
      JOIN avionics_models legacy ON legacy.id = guard.duplicate_model_id
      WHERE guard.duplicate_model_id = OLD.avionics_model_id
        AND guard.survivor_model_id = NEW.avionics_model_id
        AND survivor.catalog_status = 'unreviewed'
        AND legacy.catalog_status = 'unreviewed'
    )
  ) THEN
    RAISE EXCEPTION 'default avionics association requires an approved catalog entry';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS aircraft_model_variant_default_avionics_approved_insert
  ON aircraft_model_variant_default_avionics;
CREATE TRIGGER aircraft_model_variant_default_avionics_approved_insert
BEFORE INSERT ON aircraft_model_variant_default_avionics
FOR EACH ROW EXECUTE FUNCTION require_approved_default_avionics_model();
DROP TRIGGER IF EXISTS aircraft_model_variant_default_avionics_approved_update
  ON aircraft_model_variant_default_avionics;
CREATE TRIGGER aircraft_model_variant_default_avionics_approved_update
BEFORE UPDATE OF avionics_model_id ON aircraft_model_variant_default_avionics
FOR EACH ROW EXECUTE FUNCTION require_approved_default_avionics_model();

CREATE OR REPLACE FUNCTION require_approved_listing_avionics_models()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF NEW.avionics_model_id IS DISTINCT FROM OLD.avionics_model_id
       AND NOT EXISTS (
         SELECT 1 FROM avionics_models model
         WHERE model.id = NEW.avionics_model_id AND model.catalog_status = 'approved'
       )
       AND NOT EXISTS (
         SELECT 1
         FROM avionics_catalog_authorized_consolidations guard
         JOIN aircraft_sale_listings listing
           ON listing.id = NEW.aircraft_sale_listing_id
         WHERE guard.duplicate_model_id = OLD.avionics_model_id
           AND guard.survivor_model_id = NEW.avionics_model_id
           AND listing.ingestion_state <> 'ready'
           AND NOT listing.is_verified
       ) THEN
      RAISE EXCEPTION 'listing avionics association requires an approved catalog entry';
    END IF;
    IF NEW.replaces_avionics_model_id IS DISTINCT FROM OLD.replaces_avionics_model_id
       AND NEW.replaces_avionics_model_id IS NOT NULL
       AND NOT EXISTS (
         SELECT 1 FROM avionics_models model
         WHERE model.id = NEW.replaces_avionics_model_id
           AND model.catalog_status = 'approved'
       )
       AND NOT EXISTS (
         SELECT 1
         FROM avionics_catalog_authorized_consolidations guard
         JOIN aircraft_sale_listings listing
           ON listing.id = NEW.aircraft_sale_listing_id
         WHERE guard.duplicate_model_id = OLD.replaces_avionics_model_id
           AND guard.survivor_model_id = NEW.replaces_avionics_model_id
           AND listing.ingestion_state <> 'ready'
           AND NOT listing.is_verified
       ) THEN
      RAISE EXCEPTION 'listing avionics replacement requires an approved catalog entry';
    END IF;
  ELSIF NOT EXISTS (
    SELECT 1 FROM avionics_models model
    WHERE model.id = NEW.avionics_model_id AND model.catalog_status = 'approved'
  ) OR (
    NEW.replaces_avionics_model_id IS NOT NULL
    AND NOT EXISTS (
      SELECT 1 FROM avionics_models model
      WHERE model.id = NEW.replaces_avionics_model_id
        AND model.catalog_status = 'approved'
    )
  ) THEN
    RAISE EXCEPTION 'listing avionics association requires approved catalog entries';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_approved_insert
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_approved_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION require_approved_listing_avionics_models();
DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_approved_update
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_approved_update
BEFORE UPDATE OF avionics_model_id, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION require_approved_listing_avionics_models();

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_aircraft_sale_listing_avionics_unique_displacement
  ON aircraft_sale_listing_avionics (
    aircraft_sale_listing_id, replaces_avionics_model_id
  )
  WHERE configuration_action IN ('replaces', 'removes');

CREATE OR REPLACE VIEW avionics_semantic_duplicate_listing_links AS
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

CREATE OR REPLACE VIEW avionics_semantic_invalid_replacement_links AS
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
    AND link.replaces_avionics_model_id
      IS DISTINCT FROM link.avionics_model_id);

CREATE OR REPLACE VIEW avionics_semantic_duplicate_displacement_targets AS
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

CREATE OR REPLACE VIEW avionics_semantic_installed_displacement_conflicts AS
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

CREATE OR REPLACE VIEW avionics_semantic_invalid_listing_action_graphs AS
SELECT listing_id, 'duplicate_installed_subject'::TEXT AS issue
FROM avionics_semantic_duplicate_listing_links
UNION
SELECT listing_id, 'invalid_action_subject_target'::TEXT AS issue
FROM avionics_semantic_invalid_replacement_links
UNION
SELECT listing_id, 'duplicate_displacement_target'::TEXT AS issue
FROM avionics_semantic_duplicate_displacement_targets
UNION
SELECT listing_id, 'installed_subject_is_displaced'::TEXT AS issue
FROM avionics_semantic_installed_displacement_conflicts;

CREATE OR REPLACE FUNCTION preserve_ready_listing_avionics()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
DECLARE
  target_listing_id BIGINT;
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF EXISTS (
      SELECT 1 FROM aircraft_sale_listings listing
      WHERE listing.id IN (
          OLD.aircraft_sale_listing_id, NEW.aircraft_sale_listing_id
        )
        AND (listing.ingestion_state = 'ready' OR listing.is_verified)
    ) THEN
      RAISE EXCEPTION 'ready or verified listing avionics are immutable';
    END IF;
    RETURN NEW;
  ELSIF TG_OP = 'INSERT' THEN
    target_listing_id := NEW.aircraft_sale_listing_id;
  ELSE
    target_listing_id := OLD.aircraft_sale_listing_id;
  END IF;
  IF EXISTS (
    SELECT 1 FROM aircraft_sale_listings listing
    WHERE listing.id = target_listing_id
      AND (listing.ingestion_state = 'ready' OR listing.is_verified)
  ) THEN
    RAISE EXCEPTION 'ready or verified listing avionics are immutable';
  END IF;
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_mutable_insert
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_mutable_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION preserve_ready_listing_avionics();
DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_mutable_update
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_mutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION preserve_ready_listing_avionics();
DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_mutable_delete
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_mutable_delete
BEFORE DELETE ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION preserve_ready_listing_avionics();

CREATE OR REPLACE FUNCTION require_distinct_listing_avionics_replacement()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF (NEW.configuration_action = 'installed'
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
      AND NEW.replaces_avionics_model_id
        IS DISTINCT FROM NEW.avionics_model_id)
  THEN
    RAISE EXCEPTION 'listing avionics action has invalid subject/target semantics';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_distinct_replacement_insert
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_distinct_replacement_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION require_distinct_listing_avionics_replacement();
DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_distinct_replacement_update
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_distinct_replacement_update
BEFORE UPDATE OF avionics_model_id, configuration_action, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION require_distinct_listing_avionics_replacement();

CREATE OR REPLACE FUNCTION require_unique_listing_avionics_identity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
DECLARE
  excluded_link_id BIGINT;
BEGIN
  IF TG_OP = 'UPDATE' THEN
    excluded_link_id := OLD.id;
  END IF;
  IF NEW.configuration_action IN ('installed', 'replaces') AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND (excluded_link_id IS NULL OR existing.id <> excluded_link_id)
     AND existing.configuration_action IN ('installed', 'replaces')
    JOIN avionics_approved_product_graph_identities existing_identity
      ON existing_identity.avionics_model_id = existing.avionics_model_id
     AND existing_identity.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_identity.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.avionics_model_id
  ) THEN
    RAISE EXCEPTION 'listing cannot install one canonical avionics product more than once';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_semantic_unique_insert
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_semantic_unique_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION require_unique_listing_avionics_identity();
DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_semantic_unique_update
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_semantic_unique_update
BEFORE UPDATE OF aircraft_sale_listing_id, avionics_model_id, configuration_action
ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION require_unique_listing_avionics_identity();

CREATE OR REPLACE FUNCTION require_valid_listing_avionics_action_graph()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
DECLARE
  excluded_link_id BIGINT;
BEGIN
  IF TG_OP = 'UPDATE' THEN
    excluded_link_id := OLD.id;
    PERFORM listing.id
    FROM aircraft_sale_listings listing
    WHERE listing.id IN (
      OLD.aircraft_sale_listing_id, NEW.aircraft_sale_listing_id
    )
    ORDER BY listing.id
    FOR UPDATE;
  ELSE
    PERFORM listing.id
    FROM aircraft_sale_listings listing
    WHERE listing.id = NEW.aircraft_sale_listing_id
    FOR UPDATE;
  END IF;

  IF (
    NEW.replaces_avionics_model_id IS NOT NULL
    AND EXISTS (
      SELECT 1
      FROM avionics_approved_product_graph_identities candidate
      JOIN aircraft_sale_listing_avionics existing
        ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
       AND (excluded_link_id IS NULL OR existing.id <> excluded_link_id)
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
       AND (excluded_link_id IS NULL OR existing.id <> excluded_link_id)
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
       AND (excluded_link_id IS NULL OR existing.id <> excluded_link_id)
       AND existing.configuration_action IN ('installed', 'replaces')
      JOIN avionics_approved_product_graph_identities existing_subject
        ON existing_subject.avionics_model_id = existing.avionics_model_id
       AND existing_subject.avionics_manufacturer_identity_id
         = candidate.avionics_manufacturer_identity_id
       AND existing_subject.canonical_product_key = candidate.canonical_product_key
      WHERE candidate.avionics_model_id = NEW.replaces_avionics_model_id
    )
  ) THEN
    RAISE EXCEPTION 'listing avionics action graph has duplicate or contradictory installed/displaced identities';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_action_graph_insert
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_action_graph_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION require_valid_listing_avionics_action_graph();
DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_action_graph_update
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER aircraft_sale_listing_avionics_action_graph_update
BEFORE UPDATE OF aircraft_sale_listing_id, avionics_model_id,
  configuration_action, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION require_valid_listing_avionics_action_graph();

CREATE OR REPLACE FUNCTION require_ready_listing_avionics_integrity()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF TG_OP = 'INSERT' AND NEW.ingestion_state = 'ready' THEN
    RAISE EXCEPTION 'listing cannot be inserted ready before avionics are validated';
  END IF;
  IF TG_OP = 'UPDATE' THEN
    IF NEW.ingestion_state = 'ready'
       AND OLD.ingestion_state IS DISTINCT FROM 'ready'
       AND NOT EXISTS (
         SELECT 1
         FROM pg_locks held_lock
         WHERE held_lock.locktype = 'relation'
           AND held_lock.pid = pg_backend_pid()
           AND held_lock.relation = 'aircraft_sale_listing_avionics'::regclass
           AND held_lock.mode IN (
             'ShareRowExclusiveLock', 'ExclusiveLock', 'AccessExclusiveLock'
           )
           AND held_lock.granted
       ) THEN
      RAISE EXCEPTION
        'publishing a listing requires a prior SHARE ROW EXCLUSIVE avionics table lock';
    END IF;
  END IF;
  IF NEW.ingestion_state = 'ready' AND (
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
          OR link.source_confidence IS DISTINCT FROM 'high'
          OR link.source NOT IN ('listing', 'listing_review')
        )
    )
  ) THEN
    RAISE EXCEPTION 'ready listing requires unique approved canonical avionics';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS aircraft_sale_listings_ready_semantic_avionics_insert
  ON aircraft_sale_listings;
CREATE TRIGGER aircraft_sale_listings_ready_semantic_avionics_insert
BEFORE INSERT ON aircraft_sale_listings
FOR EACH ROW EXECUTE FUNCTION require_ready_listing_avionics_integrity();
DROP TRIGGER IF EXISTS aircraft_sale_listings_ready_semantic_avionics
  ON aircraft_sale_listings;
CREATE TRIGGER aircraft_sale_listings_ready_semantic_avionics
BEFORE UPDATE OF ingestion_state ON aircraft_sale_listings
FOR EACH ROW EXECUTE FUNCTION require_ready_listing_avionics_integrity();

CREATE OR REPLACE FUNCTION require_verified_listing_ready_state()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NEW.is_verified AND NEW.ingestion_state <> 'ready' THEN
    RAISE EXCEPTION 'verified listing must be in the ready ingestion state';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS listing_verified_requires_ready_insert
  ON aircraft_sale_listings;
CREATE TRIGGER listing_verified_requires_ready_insert
BEFORE INSERT ON aircraft_sale_listings
FOR EACH ROW EXECUTE FUNCTION require_verified_listing_ready_state();

DROP TRIGGER IF EXISTS listing_verified_requires_ready_update
  ON aircraft_sale_listings;
CREATE TRIGGER listing_verified_requires_ready_update
BEFORE UPDATE OF is_verified, ingestion_state ON aircraft_sale_listings
FOR EACH ROW EXECUTE FUNCTION require_verified_listing_ready_state();

-- Preserve reference-version immutability except for the same exact,
-- guard-authorized legacy model remap used by the consolidation transaction.
CREATE OR REPLACE FUNCTION prevent_aircraft_reference_fact_mutation()
RETURNS TRIGGER AS $$
DECLARE
  parent_id BIGINT;
  parent_state TEXT;
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF TG_TABLE_NAME = 'aircraft_reference_avionics'
       AND NEW.id = OLD.id
       AND NEW.aircraft_reference_configuration_version_id
         = OLD.aircraft_reference_configuration_version_id
       AND NEW.avionics_model_id IS DISTINCT FROM OLD.avionics_model_id
       AND NEW.quantity = OLD.quantity
       AND NEW.equipment_role = OLD.equipment_role
       AND NEW.evidence_claim_id = OLD.evidence_claim_id
       AND NEW.created_at = OLD.created_at
       AND EXISTS (
         SELECT 1
         FROM avionics_catalog_authorized_consolidations guard
         WHERE guard.duplicate_model_id = OLD.avionics_model_id
           AND guard.survivor_model_id = NEW.avionics_model_id
       ) THEN
      RETURN NEW;
    END IF;
    RAISE EXCEPTION 'reference profile facts are immutable; publish a replacement version';
  END IF;
  parent_id := OLD.aircraft_reference_configuration_version_id;
  SELECT publication_state INTO parent_state
  FROM aircraft_reference_configuration_versions WHERE id = parent_id;
  IF parent_state IS DISTINCT FROM 'building' THEN
    RAISE EXCEPTION 'published reference profile facts are immutable';
  END IF;
  RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION validate_identity_aircraft_reference_avionics_insert()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM aircraft_reference_configuration_versions version
    WHERE version.id = NEW.aircraft_reference_configuration_version_id
      AND version.publication_state = 'building'
  ) OR NOT EXISTS (
    SELECT 1
    FROM avionics_models model
    WHERE model.id = NEW.avionics_model_id
      AND model.catalog_status = 'approved'
  ) THEN
    RAISE EXCEPTION 'reference avionics requires a building version and approved product';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS aircraft_reference_avionics_building_insert
  ON aircraft_reference_avionics;
CREATE TRIGGER aircraft_reference_avionics_building_insert
BEFORE INSERT ON aircraft_reference_avionics
FOR EACH ROW
EXECUTE FUNCTION validate_identity_aircraft_reference_avionics_insert();

DROP TRIGGER IF EXISTS aircraft_reference_avionics_immutable
  ON aircraft_reference_avionics;
CREATE TRIGGER aircraft_reference_avionics_immutable
BEFORE UPDATE OR DELETE ON aircraft_reference_avionics
FOR EACH ROW EXECUTE FUNCTION prevent_aircraft_reference_fact_mutation();

-- A verified flag on a non-publishable row was a legacy state bypass. Preserve
-- the row for review, but remove it from every published/verified path.
UPDATE aircraft_sale_listings
SET ingestion_state = 'quarantined',
    is_verified = FALSE,
    ingestion_error =
      'Identity postcondition migration: verified listing was not in the ready ingestion state.',
    ingestion_completed_at = NULL,
    updated_at = CURRENT_TIMESTAMP
WHERE is_verified
  AND ingestion_state <> 'ready';

-- Existing published rows that violate the new invariant are retained for
-- review but are no longer public/verified.
UPDATE aircraft_sale_listings
SET ingestion_state = 'quarantined',
    is_verified = FALSE,
    ingestion_error = CASE
      WHEN EXISTS (
        SELECT 1
        FROM aircraft_sale_listing_avionics link
        LEFT JOIN avionics_models installed ON installed.id = link.avionics_model_id
        LEFT JOIN avionics_models replaced ON replaced.id = link.replaces_avionics_model_id
        WHERE link.aircraft_sale_listing_id = aircraft_sale_listings.id
          AND (
            installed.catalog_status IS DISTINCT FROM 'approved'
            OR (link.replaces_avionics_model_id IS NOT NULL
              AND replaced.catalog_status IS DISTINCT FROM 'approved')
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
          installed.catalog_status IS DISTINCT FROM 'approved'
          OR (link.replaces_avionics_model_id IS NOT NULL
            AND replaced.catalog_status IS DISTINCT FROM 'approved')
          OR link.quantity <= 0
          OR link.source_confidence IS DISTINCT FROM 'high'
          OR link.source NOT IN ('listing', 'listing_review')
        )
    )
    OR EXISTS (
      SELECT 1
      FROM avionics_semantic_invalid_listing_action_graphs invalid_graph
      WHERE invalid_graph.listing_id = aircraft_sale_listings.id
    )
  );

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260725_identity_deduplication_postconditions',
  6,
  'cd001240b48a1480fd8bbee39b9ddedbba01d00fad45cbac315cec7a243cf133',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
