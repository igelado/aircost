-- Repair canonical aircraft catalog retrieval keys that were historically
-- written with the manufacturer-aware `normalize_name` helper. Retrieval keys
-- are mechanical only: lowercase ASCII letters/digits, every other character
-- is a separator, and separator runs are collapsed and trimmed.
--
-- This migration never merges, deletes, or re-identifies a catalog row. It
-- preserves every row ID, approval decision, relationship, and assignment.
-- Empty derived keys, scoped key collisions, and cross-make alias collisions
-- abort the transaction before an immutable row is touched.

BEGIN;

SET LOCAL search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL,
  contract_fingerprint TEXT NOT NULL,
  installed_at TEXT NOT NULL
);

LOCK TABLE public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260729_aircraft_catalog_retrieval_keys'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          'b40b266fc450810cf89acc78c9405f4cd7d816ea38d389114e93a20cfea6901d'
      )
  ) THEN
    RAISE EXCEPTION
      'installed aircraft catalog retrieval keys migration has a different contract';
  END IF;
END
$migration_guard$;

CREATE OR REPLACE FUNCTION aircraft_retrieval_key(value TEXT)
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
STRICT
PARALLEL SAFE
AS $function$
  SELECT trim(lower(regexp_replace(value, '[^A-Za-z0-9]+', ' ', 'g')));
$function$;

CREATE TEMP TABLE aircraft_catalog_retrieval_key_repairs (
  entity_kind TEXT NOT NULL,
  entity_id BIGINT NOT NULL,
  scope_id BIGINT NOT NULL,
  normalized_name TEXT NOT NULL,
  PRIMARY KEY (entity_kind, entity_id)
) ON COMMIT DROP;

INSERT INTO aircraft_catalog_retrieval_key_repairs (
  entity_kind, entity_id, scope_id, normalized_name
)
SELECT 'make', id, 0, aircraft_retrieval_key(name)
FROM aircraft_makes
UNION ALL
SELECT 'family', id, aircraft_make_id, aircraft_retrieval_key(name)
FROM aircraft_model_families
UNION ALL
SELECT
  'generation', id, aircraft_model_family_id, aircraft_retrieval_key(name)
FROM aircraft_generations
UNION ALL
SELECT
  'package', id, aircraft_model_family_id, aircraft_retrieval_key(name)
FROM aircraft_factory_packages;

DO $block$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM aircraft_catalog_retrieval_key_repairs repair
    WHERE repair.normalized_name = ''
  ) THEN
    RAISE EXCEPTION 'aircraft catalog retrieval-key repair produced an empty key';
  END IF;

  IF EXISTS (
    SELECT 1
    FROM aircraft_catalog_retrieval_key_repairs left_repair
    JOIN aircraft_catalog_retrieval_key_repairs right_repair
      ON right_repair.entity_kind = left_repair.entity_kind
     AND right_repair.scope_id = left_repair.scope_id
     AND right_repair.normalized_name = left_repair.normalized_name
     AND right_repair.entity_id > left_repair.entity_id
  ) THEN
    RAISE EXCEPTION 'aircraft catalog retrieval-key repair would create a scoped collision';
  END IF;

  IF EXISTS (
    SELECT 1
    FROM aircraft_catalog_retrieval_key_repairs repair
    JOIN aircraft_make_aliases alias
      ON alias.normalized_alias = repair.normalized_name
     AND alias.aircraft_make_id <> repair.entity_id
    WHERE repair.entity_kind = 'make'
  ) THEN
    RAISE EXCEPTION 'aircraft catalog retrieval-key repair would collide with another make alias';
  END IF;

  IF EXISTS (
    SELECT 1
    FROM (
      SELECT normalized_name FROM aircraft_makes
      UNION ALL
      SELECT normalized_name FROM aircraft_model_families
      UNION ALL
      SELECT normalized_name FROM aircraft_generations
      UNION ALL
      SELECT normalized_name FROM aircraft_factory_packages
    ) catalog_key
    WHERE catalog_key.normalized_name LIKE
      '\_\_aircost\_catalog\_key\_repair\_\_%' ESCAPE '\'
  ) THEN
    RAISE EXCEPTION 'reserved aircraft catalog retrieval-key repair prefix is already in use';
  END IF;
END;
$block$;

DROP TRIGGER IF EXISTS aircraft_make_retrieval_key_validate
  ON aircraft_makes;
DROP TRIGGER IF EXISTS aircraft_family_retrieval_key_validate
  ON aircraft_model_families;
DROP TRIGGER IF EXISTS aircraft_generation_retrieval_key_validate
  ON aircraft_generations;
DROP TRIGGER IF EXISTS aircraft_package_retrieval_key_validate
  ON aircraft_factory_packages;

-- Keep DELETE protection active while allowing this transaction's narrow
-- normalized_name updates. The original combined triggers are restored below.
DROP TRIGGER IF EXISTS assigned_aircraft_make_immutable ON aircraft_makes;
CREATE TRIGGER assigned_aircraft_make_immutable
BEFORE DELETE ON aircraft_makes
FOR EACH ROW
EXECUTE FUNCTION preserve_assigned_aircraft_entity('aircraft_make_id');
DROP TRIGGER IF EXISTS assigned_aircraft_family_immutable
  ON aircraft_model_families;
CREATE TRIGGER assigned_aircraft_family_immutable
BEFORE DELETE ON aircraft_model_families
FOR EACH ROW
EXECUTE FUNCTION preserve_assigned_aircraft_entity(
  'aircraft_model_family_id'
);
DROP TRIGGER IF EXISTS assigned_aircraft_generation_immutable
  ON aircraft_generations;
CREATE TRIGGER assigned_aircraft_generation_immutable
BEFORE DELETE ON aircraft_generations
FOR EACH ROW
EXECUTE FUNCTION preserve_assigned_aircraft_entity('aircraft_generation_id');
DROP TRIGGER IF EXISTS assigned_aircraft_package_immutable
  ON aircraft_factory_packages;
CREATE TRIGGER assigned_aircraft_package_immutable
BEFORE DELETE ON aircraft_factory_packages
FOR EACH ROW
EXECUTE FUNCTION preserve_assigned_aircraft_entity(
  'aircraft_factory_package_id'
);

DROP TRIGGER IF EXISTS compatibility_projected_make_immutable_update
  ON aircraft_makes;
DROP TRIGGER IF EXISTS compatibility_projected_family_immutable_update
  ON aircraft_model_families;
DROP TRIGGER IF EXISTS compatibility_projected_generation_immutable_update
  ON aircraft_generations;
DROP TRIGGER IF EXISTS compatibility_projected_package_immutable_update
  ON aircraft_factory_packages;

UPDATE aircraft_makes catalog
SET normalized_name =
      '__aircost_catalog_key_repair__make_' || catalog.id,
    updated_at = CURRENT_TIMESTAMP
FROM aircraft_catalog_retrieval_key_repairs repair
WHERE repair.entity_kind = 'make'
  AND repair.entity_id = catalog.id
  AND repair.normalized_name <> catalog.normalized_name;
UPDATE aircraft_model_families catalog
SET normalized_name =
      '__aircost_catalog_key_repair__family_' || catalog.id,
    updated_at = CURRENT_TIMESTAMP
FROM aircraft_catalog_retrieval_key_repairs repair
WHERE repair.entity_kind = 'family'
  AND repair.entity_id = catalog.id
  AND repair.normalized_name <> catalog.normalized_name;
UPDATE aircraft_generations catalog
SET normalized_name =
      '__aircost_catalog_key_repair__generation_' || catalog.id,
    updated_at = CURRENT_TIMESTAMP
FROM aircraft_catalog_retrieval_key_repairs repair
WHERE repair.entity_kind = 'generation'
  AND repair.entity_id = catalog.id
  AND repair.normalized_name <> catalog.normalized_name;
UPDATE aircraft_factory_packages catalog
SET normalized_name =
      '__aircost_catalog_key_repair__package_' || catalog.id,
    updated_at = CURRENT_TIMESTAMP
FROM aircraft_catalog_retrieval_key_repairs repair
WHERE repair.entity_kind = 'package'
  AND repair.entity_id = catalog.id
  AND repair.normalized_name <> catalog.normalized_name;

UPDATE aircraft_makes catalog
SET normalized_name = repair.normalized_name
FROM aircraft_catalog_retrieval_key_repairs repair
WHERE repair.entity_kind = 'make'
  AND repair.entity_id = catalog.id
  AND catalog.normalized_name =
      '__aircost_catalog_key_repair__make_' || catalog.id;
UPDATE aircraft_model_families catalog
SET normalized_name = repair.normalized_name
FROM aircraft_catalog_retrieval_key_repairs repair
WHERE repair.entity_kind = 'family'
  AND repair.entity_id = catalog.id
  AND catalog.normalized_name =
      '__aircost_catalog_key_repair__family_' || catalog.id;
UPDATE aircraft_generations catalog
SET normalized_name = repair.normalized_name
FROM aircraft_catalog_retrieval_key_repairs repair
WHERE repair.entity_kind = 'generation'
  AND repair.entity_id = catalog.id
  AND catalog.normalized_name =
      '__aircost_catalog_key_repair__generation_' || catalog.id;
UPDATE aircraft_factory_packages catalog
SET normalized_name = repair.normalized_name
FROM aircraft_catalog_retrieval_key_repairs repair
WHERE repair.entity_kind = 'package'
  AND repair.entity_id = catalog.id
  AND catalog.normalized_name =
      '__aircost_catalog_key_repair__package_' || catalog.id;

CREATE OR REPLACE FUNCTION require_aircraft_catalog_retrieval_key()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NEW.normalized_name <> aircraft_retrieval_key(NEW.name) THEN
    RAISE EXCEPTION '% requires its deterministic aircraft retrieval key',
      TG_TABLE_NAME;
  END IF;
  RETURN NEW;
END;
$function$;

CREATE TRIGGER aircraft_make_retrieval_key_validate
BEFORE INSERT OR UPDATE OF name, normalized_name ON aircraft_makes
FOR EACH ROW
EXECUTE FUNCTION require_aircraft_catalog_retrieval_key();
CREATE TRIGGER aircraft_family_retrieval_key_validate
BEFORE INSERT OR UPDATE OF name, normalized_name ON aircraft_model_families
FOR EACH ROW
EXECUTE FUNCTION require_aircraft_catalog_retrieval_key();
CREATE TRIGGER aircraft_generation_retrieval_key_validate
BEFORE INSERT OR UPDATE OF name, normalized_name ON aircraft_generations
FOR EACH ROW
EXECUTE FUNCTION require_aircraft_catalog_retrieval_key();
CREATE TRIGGER aircraft_package_retrieval_key_validate
BEFORE INSERT OR UPDATE OF name, normalized_name ON aircraft_factory_packages
FOR EACH ROW
EXECUTE FUNCTION require_aircraft_catalog_retrieval_key();

DROP TRIGGER IF EXISTS assigned_aircraft_make_immutable ON aircraft_makes;
CREATE TRIGGER assigned_aircraft_make_immutable
BEFORE UPDATE OR DELETE ON aircraft_makes
FOR EACH ROW
EXECUTE FUNCTION preserve_assigned_aircraft_entity('aircraft_make_id');
DROP TRIGGER IF EXISTS assigned_aircraft_family_immutable
  ON aircraft_model_families;
CREATE TRIGGER assigned_aircraft_family_immutable
BEFORE UPDATE OR DELETE ON aircraft_model_families
FOR EACH ROW
EXECUTE FUNCTION preserve_assigned_aircraft_entity(
  'aircraft_model_family_id'
);
DROP TRIGGER IF EXISTS assigned_aircraft_generation_immutable
  ON aircraft_generations;
CREATE TRIGGER assigned_aircraft_generation_immutable
BEFORE UPDATE OR DELETE ON aircraft_generations
FOR EACH ROW
EXECUTE FUNCTION preserve_assigned_aircraft_entity('aircraft_generation_id');
DROP TRIGGER IF EXISTS assigned_aircraft_package_immutable
  ON aircraft_factory_packages;
CREATE TRIGGER assigned_aircraft_package_immutable
BEFORE UPDATE OR DELETE ON aircraft_factory_packages
FOR EACH ROW
EXECUTE FUNCTION preserve_assigned_aircraft_entity(
  'aircraft_factory_package_id'
);

CREATE TRIGGER compatibility_projected_make_immutable_update
BEFORE UPDATE ON aircraft_makes
FOR EACH ROW
EXECUTE FUNCTION preserve_compatibility_projected_aircraft_entity(
  'aircraft_make_id'
);
CREATE TRIGGER compatibility_projected_family_immutable_update
BEFORE UPDATE ON aircraft_model_families
FOR EACH ROW
EXECUTE FUNCTION preserve_compatibility_projected_aircraft_entity(
  'aircraft_model_family_id'
);
CREATE TRIGGER compatibility_projected_generation_immutable_update
BEFORE UPDATE ON aircraft_generations
FOR EACH ROW
EXECUTE FUNCTION preserve_compatibility_projected_aircraft_entity(
  'aircraft_generation_id'
);
CREATE TRIGGER compatibility_projected_package_immutable_update
BEFORE UPDATE ON aircraft_factory_packages
FOR EACH ROW
EXECUTE FUNCTION preserve_compatibility_projected_aircraft_entity(
  'aircraft_factory_package_id'
);

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260729_aircraft_catalog_retrieval_keys',
  1,
  'b40b266fc450810cf89acc78c9405f4cd7d816ea38d389114e93a20cfea6901d',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
