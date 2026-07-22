-- Apply after 20260721_avionics_catalog_curation.postgres.sql and to a backup
-- first. This is a one-time migration and is intentionally not run by the
-- application.
--
-- Product identity is independent from capability. The legacy scalar type is
-- copied into the many-to-many table before that column is removed. No legacy
-- catalog row is merged, promoted, or deleted.

BEGIN;

CREATE TABLE avionics_model_types (
  avionics_model_id BIGINT NOT NULL
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_type_id BIGINT NOT NULL
    REFERENCES avionics_types(id) ON DELETE RESTRICT,
  PRIMARY KEY (avionics_model_id, avionics_type_id)
);

CREATE INDEX idx_avionics_model_types_type
  ON avionics_model_types (avionics_type_id, avionics_model_id);

-- NAV/COM was a legacy product-class label. In the capability ontology it is
-- the two independent capabilities NAV and COM, never a third composite type.
INSERT INTO avionics_types (name, normalized_name)
VALUES ('NAV', 'nav'), ('COM', 'com')
ON CONFLICT (normalized_name) DO NOTHING;

INSERT INTO avionics_model_types (
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

-- PostgreSQL removes the legacy foreign key and composite unique constraint
-- that depend on this column together with the column itself.
ALTER TABLE avionics_models
  DROP COLUMN avionics_type_id;

DELETE FROM avionics_types
WHERE normalized_name = 'nav com'
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_model_types membership
    WHERE membership.avionics_type_id = avionics_types.id
  );

-- Same-name legacy candidates remain available for grounded consolidation,
-- while approved identities are protected from ambiguous duplicates.
CREATE UNIQUE INDEX idx_avionics_models_approved_manufacturer_name
  ON avionics_models (avionics_manufacturer_id, normalized_name)
  WHERE catalog_status = 'approved';

-- Approval is staged: create an unreviewed product, attach at least one
-- capability, then approve it. An approved product can never be left typeless.
CREATE OR REPLACE FUNCTION require_avionics_model_type_for_approval()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NEW.catalog_status = 'approved' AND NOT EXISTS (
    SELECT 1
    FROM avionics_model_types membership
    WHERE membership.avionics_model_id = NEW.id
  ) THEN
    RAISE EXCEPTION 'approved avionics model requires at least one type';
  END IF;
  RETURN NEW;
END;
$function$;

CREATE TRIGGER avionics_models_approved_types_insert
BEFORE INSERT ON avionics_models
FOR EACH ROW EXECUTE FUNCTION require_avionics_model_type_for_approval();

CREATE TRIGGER avionics_models_approved_types_update
BEFORE UPDATE OF catalog_status ON avionics_models
FOR EACH ROW EXECUTE FUNCTION require_avionics_model_type_for_approval();

CREATE OR REPLACE FUNCTION preserve_approved_avionics_model_type()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
DECLARE
  locked_catalog_status TEXT;
BEGIN
  -- Serialize membership removals with approval changes and with every other
  -- removal for this product. Without the parent-row lock, two transactions
  -- could each observe the other's membership and delete both.
  SELECT model.catalog_status
  INTO locked_catalog_status
  FROM avionics_models model
  WHERE model.id = OLD.avionics_model_id
  FOR UPDATE;

  -- A cascading parent delete no longer exposes the parent row here, so it is
  -- allowed to remove the final child membership.
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

CREATE TRIGGER avionics_model_types_preserve_approved_delete
BEFORE DELETE ON avionics_model_types
FOR EACH ROW EXECUTE FUNCTION preserve_approved_avionics_model_type();

CREATE TRIGGER avionics_model_types_preserve_approved_update
BEFORE UPDATE OF avionics_model_id ON avionics_model_types
FOR EACH ROW
WHEN (NEW.avionics_model_id IS DISTINCT FROM OLD.avionics_model_id)
EXECUTE FUNCTION preserve_approved_avionics_model_type();

COMMIT;
