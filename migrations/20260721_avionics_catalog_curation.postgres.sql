-- Apply to a backup first. This migration is intentionally not run by the
-- application.
--
-- Existing avionics catalog rows and their associations are preserved. Every
-- legacy catalog row remains `unreviewed`; no row is promoted or deleted by
-- this migration. New listing/default associations require an explicitly
-- approved, source-backed catalog identity.

BEGIN;

ALTER TABLE avionics_models
  ADD COLUMN catalog_status TEXT NOT NULL DEFAULT 'unreviewed',
  ADD COLUMN manufacturer_identifier_kind TEXT,
  ADD COLUMN manufacturer_identifier TEXT,
  ADD COLUMN normalized_manufacturer_identifier TEXT,
  ADD COLUMN identity_source_url TEXT,
  ADD COLUMN identity_source_title TEXT,
  ADD COLUMN identity_evidence_text TEXT,
  ADD COLUMN identity_evidence_kind TEXT NOT NULL DEFAULT 'unreviewed',
  ADD COLUMN identity_confidence TEXT,
  ADD COLUMN catalog_reviewed_at TEXT;

ALTER TABLE avionics_models
  ADD CONSTRAINT avionics_models_catalog_status_check
    CHECK (catalog_status IN ('unreviewed', 'approved', 'rejected')),
  ADD CONSTRAINT avionics_models_manufacturer_identifier_kind_check
    CHECK (
      manufacturer_identifier_kind IS NULL
      OR manufacturer_identifier_kind IN (
        'manufacturer_part_number', 'manufacturer_model_number', 'sku'
      )
    ),
  ADD CONSTRAINT avionics_models_identity_evidence_kind_check
    CHECK (identity_evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  ADD CONSTRAINT avionics_models_identity_confidence_check
    CHECK (
      identity_confidence IS NULL
      OR identity_confidence IN ('very_high', 'high', 'medium', 'low')
    ),
  ADD CONSTRAINT avionics_models_manufacturer_identifier_pair_check
    CHECK (
      (
        manufacturer_identifier_kind IS NULL
        AND manufacturer_identifier IS NULL
        AND normalized_manufacturer_identifier IS NULL
      )
      OR (
        manufacturer_identifier_kind IS NOT NULL
        AND manufacturer_identifier IS NOT NULL
        AND BTRIM(manufacturer_identifier) <> ''
        AND normalized_manufacturer_identifier IS NOT NULL
        AND BTRIM(normalized_manufacturer_identifier) <> ''
      )
    ),
  ADD CONSTRAINT avionics_models_catalog_review_check
    CHECK (
      catalog_status = 'unreviewed'
      OR (catalog_reviewed_at IS NOT NULL AND BTRIM(catalog_reviewed_at) <> '')
    ),
  ADD CONSTRAINT avionics_models_approved_identity_check
    CHECK (
      catalog_status <> 'approved'
      OR (
        BTRIM(name) <> ''
        AND BTRIM(normalized_name) <> ''
        AND LOWER(BTRIM(normalized_name)) NOT IN (
          'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
          'avionics', 'avionics suite', 'integrated avionics',
          'integrated avionics suite', 'glass panel', 'flight instruments',
          'standard flight instruments', 'standard vfr avionics',
          'standard ifr avionics', 'radio', 'radios', 'nav com',
          'navigation system', 'gps', 'autopilot', 'transponder', 'ads b',
          'weather radar', 'audio panel', 'display', 'equipment'
        )
        AND POSITION(' series ' IN (' ' || LOWER(BTRIM(normalized_name)) || ' ')) = 0
        AND POSITION(' family ' IN (' ' || LOWER(BTRIM(normalized_name)) || ' ')) = 0
        AND manufacturer_identifier_kind IS NOT NULL
        AND manufacturer_identifier IS NOT NULL
        AND BTRIM(manufacturer_identifier) <> ''
        AND normalized_manufacturer_identifier IS NOT NULL
        AND BTRIM(normalized_manufacturer_identifier) <> ''
        AND identity_source_url IS NOT NULL
        AND BTRIM(identity_source_url) <> ''
        AND identity_source_title IS NOT NULL
        AND BTRIM(identity_source_title) <> ''
        AND identity_evidence_text IS NOT NULL
        AND BTRIM(identity_evidence_text) <> ''
        AND identity_evidence_kind = 'authoritative_reference'
        AND identity_confidence = 'very_high'
        AND catalog_reviewed_at IS NOT NULL
        AND BTRIM(catalog_reviewed_at) <> ''
        AND LOWER(identity_source_url) NOT LIKE '%/listing/%'
        AND LOWER(identity_source_url) NOT LIKE '%/listings/%'
        AND LOWER(identity_source_url) NOT LIKE '%/aircraft-for-sale/%'
        AND LOWER(identity_source_url) NOT LIKE '%/classifieds/%'
      )
    );

CREATE UNIQUE INDEX idx_avionics_models_manufacturer_identifier
  ON avionics_models (
    avionics_manufacturer_id,
    normalized_manufacturer_identifier
  )
  WHERE normalized_manufacturer_identifier IS NOT NULL
    AND BTRIM(normalized_manufacturer_identifier) <> '';

CREATE OR REPLACE FUNCTION require_approved_avionics_suite_models()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM avionics_models suite_model
    WHERE suite_model.id = NEW.suite_model_id
      AND suite_model.catalog_status = 'approved'
  ) THEN
    RAISE EXCEPTION 'avionics suite membership requires an approved suite catalog entry';
  END IF;
  IF NOT EXISTS (
    SELECT 1
    FROM avionics_models component_model
    WHERE component_model.id = NEW.component_model_id
      AND component_model.catalog_status = 'approved'
  ) THEN
    RAISE EXCEPTION 'avionics suite membership requires an approved component catalog entry';
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
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM avionics_models model
    WHERE model.id = NEW.avionics_model_id
      AND model.catalog_status = 'approved'
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
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM avionics_models model
    WHERE model.id = NEW.avionics_model_id
      AND model.catalog_status = 'approved'
  ) THEN
    RAISE EXCEPTION 'listing avionics association requires an approved catalog entry';
  END IF;
  IF NEW.replaces_avionics_model_id IS NOT NULL AND NOT EXISTS (
    SELECT 1
    FROM avionics_models replaced_model
    WHERE replaced_model.id = NEW.replaces_avionics_model_id
      AND replaced_model.catalog_status = 'approved'
  ) THEN
    RAISE EXCEPTION 'listing avionics replacement requires an approved catalog entry';
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

CREATE OR REPLACE FUNCTION prevent_referenced_avionics_catalog_downgrade()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
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

COMMIT;
