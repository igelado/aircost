-- Apply after 20260721_avionics_catalog_curation.sqlite.sql. Back up the
-- database first and invoke sqlite3 with -bail. This is a one-time migration
-- and is intentionally not run by the application.
--
-- Product identity is independent from capability. The legacy scalar type is
-- copied into the many-to-many table before avionics_models is rebuilt without
-- that column. No legacy catalog row is merged, promoted, or deleted.

-- SQLite requires a table rebuild because the legacy type participates in a
-- table-level UNIQUE constraint. Foreign keys must be disabled before BEGIN;
-- they are checked in full after the transaction commits.
PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;

-- These enforcement triggers live on referencing tables but resolve catalog
-- rows by name. Remove them during the no-table interval of the rebuild and
-- recreate them verbatim below.
DROP TRIGGER aircraft_model_variant_default_avionics_approved_insert;
DROP TRIGGER aircraft_model_variant_default_avionics_approved_update;
DROP TRIGGER aircraft_sale_listing_avionics_approved_insert;
DROP TRIGGER aircraft_sale_listing_avionics_approved_update;
DROP TRIGGER avionics_suite_components_approved_insert;
DROP TRIGGER avionics_suite_components_approved_update;

CREATE TABLE avionics_model_types (
  avionics_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_type_id INTEGER NOT NULL
    REFERENCES avionics_types(id) ON DELETE RESTRICT,
  PRIMARY KEY (avionics_model_id, avionics_type_id)
);

CREATE INDEX idx_avionics_model_types_type
  ON avionics_model_types (avionics_type_id, avionics_model_id);

-- NAV/COM was a legacy product-class label. In the capability ontology it is
-- the two independent capabilities NAV and COM, never a third composite type.
INSERT OR IGNORE INTO avionics_types (name, normalized_name)
VALUES ('NAV', 'nav'), ('COM', 'com');

INSERT OR ABORT INTO avionics_model_types (
  avionics_model_id,
  avionics_type_id
)
SELECT model.id, model.avionics_type_id
FROM avionics_models model
JOIN avionics_types legacy_type
  ON legacy_type.id = model.avionics_type_id
WHERE legacy_type.normalized_name <> 'nav com'
UNION ALL
SELECT model.id, atomic_type.id
FROM avionics_models model
JOIN avionics_types legacy_type
  ON legacy_type.id = model.avionics_type_id
JOIN avionics_types atomic_type
  ON atomic_type.normalized_name IN ('nav', 'com')
WHERE legacy_type.normalized_name = 'nav com';

CREATE TABLE avionics_models_rebuilt (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  avionics_manufacturer_id INTEGER NOT NULL REFERENCES avionics_manufacturers(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  catalog_status TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (catalog_status IN ('unreviewed', 'approved', 'rejected')),
  manufacturer_identifier_kind TEXT
    CHECK (
      manufacturer_identifier_kind IS NULL
      OR manufacturer_identifier_kind IN (
        'manufacturer_part_number', 'manufacturer_model_number', 'sku'
      )
    ),
  manufacturer_identifier TEXT,
  normalized_manufacturer_identifier TEXT,
  identity_source_url TEXT,
  identity_source_title TEXT,
  identity_evidence_text TEXT,
  identity_evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (identity_evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  identity_confidence TEXT
    CHECK (identity_confidence IS NULL OR identity_confidence IN ('very_high', 'high', 'medium', 'low')),
  catalog_reviewed_at TEXT,
  introduced_year INTEGER,
  discontinued_year INTEGER,
  estimated_unit_value_usd REAL,
  value_basis TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (value_basis IN ('installed_contribution', 'replacement_cost', 'unreviewed')),
  replacement_cost_usd REAL,
  value_reference_year INTEGER,
  value_source TEXT,
  valuation_scope TEXT NOT NULL DEFAULT 'unit'
    CHECK (valuation_scope IN ('unit', 'integrated_suite')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (
      manufacturer_identifier_kind IS NULL
      AND manufacturer_identifier IS NULL
      AND normalized_manufacturer_identifier IS NULL
    )
    OR (
      manufacturer_identifier_kind IS NOT NULL
      AND manufacturer_identifier IS NOT NULL
      AND length(trim(manufacturer_identifier)) > 0
      AND normalized_manufacturer_identifier IS NOT NULL
      AND length(trim(normalized_manufacturer_identifier)) > 0
    )
  ),
  CHECK (
    catalog_status = 'unreviewed'
    OR (catalog_reviewed_at IS NOT NULL AND length(trim(catalog_reviewed_at)) > 0)
  ),
  CHECK (
    catalog_status <> 'approved'
    OR (
      length(trim(name)) > 0
      AND length(trim(normalized_name)) > 0
      AND lower(trim(normalized_name)) NOT IN (
        'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
        'avionics', 'avionics suite', 'integrated avionics',
        'integrated avionics suite', 'glass panel', 'flight instruments',
        'standard flight instruments', 'standard vfr avionics',
        'standard ifr avionics', 'radio', 'radios', 'nav com',
        'navigation system', 'gps', 'autopilot', 'transponder', 'ads b',
        'weather radar', 'audio panel', 'display', 'equipment'
      )
      AND instr(' ' || lower(trim(normalized_name)) || ' ', ' series ') = 0
      AND instr(' ' || lower(trim(normalized_name)) || ' ', ' family ') = 0
      AND manufacturer_identifier_kind IS NOT NULL
      AND manufacturer_identifier IS NOT NULL
      AND length(trim(manufacturer_identifier)) > 0
      AND normalized_manufacturer_identifier IS NOT NULL
      AND length(trim(normalized_manufacturer_identifier)) > 0
      AND identity_source_url IS NOT NULL
      AND length(trim(identity_source_url)) > 0
      AND identity_source_title IS NOT NULL
      AND length(trim(identity_source_title)) > 0
      AND identity_evidence_text IS NOT NULL
      AND length(trim(identity_evidence_text)) > 0
      AND identity_evidence_kind = 'authoritative_reference'
      AND identity_confidence = 'very_high'
      AND catalog_reviewed_at IS NOT NULL
      AND length(trim(catalog_reviewed_at)) > 0
      AND lower(identity_source_url) NOT LIKE '%/listing/%'
      AND lower(identity_source_url) NOT LIKE '%/listings/%'
      AND lower(identity_source_url) NOT LIKE '%/aircraft-for-sale/%'
      AND lower(identity_source_url) NOT LIKE '%/classifieds/%'
    )
  ),
  CHECK (
    value_basis <> 'installed_contribution'
    OR (
      estimated_unit_value_usd >= 0
      AND replacement_cost_usd >= estimated_unit_value_usd
      AND value_reference_year BETWEEN 1900 AND 2200
      AND value_source IS NOT NULL
      AND length(trim(value_source)) > 0
    )
  )
);

INSERT OR ABORT INTO avionics_models_rebuilt (
  id,
  avionics_manufacturer_id,
  name,
  normalized_name,
  catalog_status,
  manufacturer_identifier_kind,
  manufacturer_identifier,
  normalized_manufacturer_identifier,
  identity_source_url,
  identity_source_title,
  identity_evidence_text,
  identity_evidence_kind,
  identity_confidence,
  catalog_reviewed_at,
  introduced_year,
  discontinued_year,
  estimated_unit_value_usd,
  value_basis,
  replacement_cost_usd,
  value_reference_year,
  value_source,
  valuation_scope,
  created_at,
  updated_at
)
SELECT
  id,
  avionics_manufacturer_id,
  name,
  normalized_name,
  catalog_status,
  manufacturer_identifier_kind,
  manufacturer_identifier,
  normalized_manufacturer_identifier,
  identity_source_url,
  identity_source_title,
  identity_evidence_text,
  identity_evidence_kind,
  identity_confidence,
  catalog_reviewed_at,
  introduced_year,
  discontinued_year,
  estimated_unit_value_usd,
  value_basis,
  replacement_cost_usd,
  value_reference_year,
  value_source,
  valuation_scope,
  created_at,
  updated_at
FROM avionics_models;

DROP TABLE avionics_models;
ALTER TABLE avionics_models_rebuilt RENAME TO avionics_models;

DELETE FROM avionics_types
WHERE normalized_name = 'nav com'
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_model_types membership
    WHERE membership.avionics_type_id = avionics_types.id
  );

CREATE UNIQUE INDEX idx_avionics_models_manufacturer_identifier
  ON avionics_models (
    avionics_manufacturer_id,
    normalized_manufacturer_identifier
  )
  WHERE normalized_manufacturer_identifier IS NOT NULL
    AND length(trim(normalized_manufacturer_identifier)) > 0;

-- Same-name legacy candidates remain available for grounded consolidation,
-- while approved identities are protected from ambiguous duplicates.
CREATE UNIQUE INDEX idx_avionics_models_approved_manufacturer_name
  ON avionics_models (avionics_manufacturer_id, normalized_name)
  WHERE catalog_status = 'approved';

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

CREATE TRIGGER avionics_suite_components_approved_update
BEFORE UPDATE ON avionics_suite_components
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

CREATE TRIGGER aircraft_model_variant_default_avionics_approved_update
BEFORE UPDATE OF avionics_model_id ON aircraft_model_variant_default_avionics
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'default avionics association requires an approved catalog entry');
END;

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
    FROM avionics_models replaced_model
    WHERE replaced_model.id = NEW.replaces_avionics_model_id
      AND replaced_model.catalog_status = 'approved'
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics association requires approved catalog entries');
END;

CREATE TRIGGER aircraft_sale_listing_avionics_approved_update
BEFORE UPDATE OF avionics_model_id, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
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
    FROM avionics_models replaced_model
    WHERE replaced_model.id = NEW.replaces_avionics_model_id
      AND replaced_model.catalog_status = 'approved'
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics association requires approved catalog entries');
END;

-- Recreate the five enforcement triggers attached to the rebuilt table.
CREATE TRIGGER avionics_installed_value_insert_check
BEFORE INSERT ON avionics_models
WHEN NEW.value_basis = 'installed_contribution' AND (
  NEW.estimated_unit_value_usd IS NULL OR NEW.estimated_unit_value_usd < 0
  OR NEW.replacement_cost_usd IS NULL
  OR NEW.replacement_cost_usd < NEW.estimated_unit_value_usd
  OR NEW.value_reference_year IS NULL
  OR NEW.value_reference_year NOT BETWEEN 1900 AND 2200
  OR NEW.value_source IS NULL
  OR length(trim(NEW.value_source)) = 0
)
BEGIN
  SELECT RAISE(ABORT, 'installed avionics contribution lacks a valid replacement basis');
END;

CREATE TRIGGER avionics_installed_value_update_check
BEFORE UPDATE ON avionics_models
WHEN NEW.value_basis = 'installed_contribution' AND (
  NEW.estimated_unit_value_usd IS NULL OR NEW.estimated_unit_value_usd < 0
  OR NEW.replacement_cost_usd IS NULL
  OR NEW.replacement_cost_usd < NEW.estimated_unit_value_usd
  OR NEW.value_reference_year IS NULL
  OR NEW.value_reference_year NOT BETWEEN 1900 AND 2200
  OR NEW.value_source IS NULL
  OR length(trim(NEW.value_source)) = 0
)
BEGIN
  SELECT RAISE(ABORT, 'installed avionics contribution lacks a valid replacement basis');
END;

CREATE TRIGGER avionics_models_catalog_lifecycle_insert
BEFORE INSERT ON avionics_models
WHEN (
  (
    NEW.manufacturer_identifier_kind IS NULL
    AND (
      NEW.manufacturer_identifier IS NOT NULL
      OR NEW.normalized_manufacturer_identifier IS NOT NULL
    )
  )
  OR (
    NEW.manufacturer_identifier_kind IS NOT NULL
    AND (
      NEW.manufacturer_identifier IS NULL
      OR length(trim(NEW.manufacturer_identifier)) = 0
      OR NEW.normalized_manufacturer_identifier IS NULL
      OR length(trim(NEW.normalized_manufacturer_identifier)) = 0
    )
  )
  OR (
    NEW.catalog_status <> 'unreviewed'
    AND (
      NEW.catalog_reviewed_at IS NULL
      OR length(trim(NEW.catalog_reviewed_at)) = 0
    )
  )
  OR (
    NEW.catalog_status = 'approved'
    AND (
      length(trim(NEW.name)) = 0
      OR length(trim(NEW.normalized_name)) = 0
      OR lower(trim(NEW.normalized_name)) IN (
        'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
        'avionics', 'avionics suite', 'integrated avionics',
        'integrated avionics suite', 'glass panel', 'flight instruments',
        'standard flight instruments', 'standard vfr avionics',
        'standard ifr avionics', 'radio', 'radios', 'nav com',
        'navigation system', 'gps', 'autopilot', 'transponder', 'ads b',
        'weather radar', 'audio panel', 'display', 'equipment'
      )
      OR instr(' ' || lower(trim(NEW.normalized_name)) || ' ', ' series ') > 0
      OR instr(' ' || lower(trim(NEW.normalized_name)) || ' ', ' family ') > 0
      OR NEW.manufacturer_identifier_kind IS NULL
      OR NEW.manufacturer_identifier IS NULL
      OR length(trim(NEW.manufacturer_identifier)) = 0
      OR NEW.normalized_manufacturer_identifier IS NULL
      OR length(trim(NEW.normalized_manufacturer_identifier)) = 0
      OR NEW.identity_source_url IS NULL
      OR length(trim(NEW.identity_source_url)) = 0
      OR NEW.identity_source_title IS NULL
      OR length(trim(NEW.identity_source_title)) = 0
      OR NEW.identity_evidence_text IS NULL
      OR length(trim(NEW.identity_evidence_text)) = 0
      OR NEW.identity_evidence_kind IS NOT 'authoritative_reference'
      OR NEW.identity_confidence IS NOT 'very_high'
      OR NEW.catalog_reviewed_at IS NULL
      OR length(trim(NEW.catalog_reviewed_at)) = 0
      OR lower(NEW.identity_source_url) LIKE '%/listing/%'
      OR lower(NEW.identity_source_url) LIKE '%/listings/%'
      OR lower(NEW.identity_source_url) LIKE '%/aircraft-for-sale/%'
      OR lower(NEW.identity_source_url) LIKE '%/classifieds/%'
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'invalid avionics catalog lifecycle or identity evidence');
END;

CREATE TRIGGER avionics_models_catalog_lifecycle_update
BEFORE UPDATE ON avionics_models
WHEN (
  (
    NEW.manufacturer_identifier_kind IS NULL
    AND (
      NEW.manufacturer_identifier IS NOT NULL
      OR NEW.normalized_manufacturer_identifier IS NOT NULL
    )
  )
  OR (
    NEW.manufacturer_identifier_kind IS NOT NULL
    AND (
      NEW.manufacturer_identifier IS NULL
      OR length(trim(NEW.manufacturer_identifier)) = 0
      OR NEW.normalized_manufacturer_identifier IS NULL
      OR length(trim(NEW.normalized_manufacturer_identifier)) = 0
    )
  )
  OR (
    NEW.catalog_status <> 'unreviewed'
    AND (
      NEW.catalog_reviewed_at IS NULL
      OR length(trim(NEW.catalog_reviewed_at)) = 0
    )
  )
  OR (
    NEW.catalog_status = 'approved'
    AND (
      length(trim(NEW.name)) = 0
      OR length(trim(NEW.normalized_name)) = 0
      OR lower(trim(NEW.normalized_name)) IN (
        'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
        'avionics', 'avionics suite', 'integrated avionics',
        'integrated avionics suite', 'glass panel', 'flight instruments',
        'standard flight instruments', 'standard vfr avionics',
        'standard ifr avionics', 'radio', 'radios', 'nav com',
        'navigation system', 'gps', 'autopilot', 'transponder', 'ads b',
        'weather radar', 'audio panel', 'display', 'equipment'
      )
      OR instr(' ' || lower(trim(NEW.normalized_name)) || ' ', ' series ') > 0
      OR instr(' ' || lower(trim(NEW.normalized_name)) || ' ', ' family ') > 0
      OR NEW.manufacturer_identifier_kind IS NULL
      OR NEW.manufacturer_identifier IS NULL
      OR length(trim(NEW.manufacturer_identifier)) = 0
      OR NEW.normalized_manufacturer_identifier IS NULL
      OR length(trim(NEW.normalized_manufacturer_identifier)) = 0
      OR NEW.identity_source_url IS NULL
      OR length(trim(NEW.identity_source_url)) = 0
      OR NEW.identity_source_title IS NULL
      OR length(trim(NEW.identity_source_title)) = 0
      OR NEW.identity_evidence_text IS NULL
      OR length(trim(NEW.identity_evidence_text)) = 0
      OR NEW.identity_evidence_kind IS NOT 'authoritative_reference'
      OR NEW.identity_confidence IS NOT 'very_high'
      OR NEW.catalog_reviewed_at IS NULL
      OR length(trim(NEW.catalog_reviewed_at)) = 0
      OR lower(NEW.identity_source_url) LIKE '%/listing/%'
      OR lower(NEW.identity_source_url) LIKE '%/listings/%'
      OR lower(NEW.identity_source_url) LIKE '%/aircraft-for-sale/%'
      OR lower(NEW.identity_source_url) LIKE '%/classifieds/%'
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'invalid avionics catalog lifecycle or identity evidence');
END;

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
)
BEGIN
  SELECT RAISE(ABORT, 'referenced avionics catalog entry cannot be unapproved');
END;

-- Approval is staged: create an unreviewed product, attach at least one
-- capability, then approve it. An approved product can never be left typeless.
CREATE TRIGGER avionics_models_approved_types_insert
BEFORE INSERT ON avionics_models
WHEN NEW.catalog_status = 'approved'
AND NOT EXISTS (
  SELECT 1
  FROM avionics_model_types membership
  WHERE membership.avionics_model_id = NEW.id
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics model requires at least one type');
END;

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

COMMIT;
PRAGMA foreign_keys = ON;
PRAGMA foreign_key_check;
