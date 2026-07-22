-- Apply to a backup first and invoke sqlite3 with -bail. This is a one-time
-- migration and is intentionally not run by the application.
--
-- Existing avionics catalog rows and their associations are preserved. Every
-- legacy catalog row remains `unreviewed`; no row is promoted or deleted by
-- this migration. New listing/default associations require an explicitly
-- approved, source-backed catalog identity.

PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

ALTER TABLE avionics_models
  ADD COLUMN catalog_status TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (catalog_status IN ('unreviewed', 'approved', 'rejected'));
ALTER TABLE avionics_models
  ADD COLUMN manufacturer_identifier_kind TEXT
    CHECK (
      manufacturer_identifier_kind IS NULL
      OR manufacturer_identifier_kind IN (
        'manufacturer_part_number', 'manufacturer_model_number', 'sku'
      )
    );
ALTER TABLE avionics_models ADD COLUMN manufacturer_identifier TEXT;
ALTER TABLE avionics_models ADD COLUMN normalized_manufacturer_identifier TEXT;
ALTER TABLE avionics_models ADD COLUMN identity_source_url TEXT;
ALTER TABLE avionics_models ADD COLUMN identity_source_title TEXT;
ALTER TABLE avionics_models ADD COLUMN identity_evidence_text TEXT;
ALTER TABLE avionics_models
  ADD COLUMN identity_evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (identity_evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed'));
ALTER TABLE avionics_models
  ADD COLUMN identity_confidence TEXT
    CHECK (
      identity_confidence IS NULL
      OR identity_confidence IN ('very_high', 'high', 'medium', 'low')
    );
ALTER TABLE avionics_models ADD COLUMN catalog_reviewed_at TEXT;

CREATE UNIQUE INDEX idx_avionics_models_manufacturer_identifier
  ON avionics_models (
    avionics_manufacturer_id,
    normalized_manufacturer_identifier
  )
  WHERE normalized_manufacturer_identifier IS NOT NULL
    AND length(trim(normalized_manufacturer_identifier)) > 0;

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

COMMIT;
PRAGMA foreign_key_check;
