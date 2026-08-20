-- Repair canonical aircraft catalog retrieval keys that were historically
-- written with the manufacturer-aware `normalize_name` helper. Retrieval keys
-- are mechanical only: lowercase ASCII letters/digits, every other character
-- is a separator, and separator runs are collapsed and trimmed.
--
-- This migration never merges, deletes, or re-identifies a catalog row. It
-- preserves every row ID, approval decision, relationship, and assignment.
-- Empty derived keys, scoped key collisions, and cross-make alias collisions
-- abort the transaction before an immutable row is touched.

CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL,
  contract_fingerprint TEXT NOT NULL,
  installed_at TEXT NOT NULL
);

PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

CREATE TEMP TABLE aircraft_retrieval_keys_migration_contract_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO aircraft_retrieval_keys_migration_contract_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260729_aircraft_catalog_retrieval_keys'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260729_aircraft_catalog_retrieval_keys'
      AND contract_version = 1
      AND contract_fingerprint =
        'b40b266fc450810cf89acc78c9405f4cd7d816ea38d389114e93a20cfea6901d'
  ) THEN 1
  ELSE 0
END;
DROP TABLE aircraft_retrieval_keys_migration_contract_guard;

DROP TABLE IF EXISTS temp.aircraft_catalog_retrieval_key_repairs;
CREATE TEMP TABLE aircraft_catalog_retrieval_key_repairs (
  entity_kind TEXT NOT NULL,
  entity_id INTEGER NOT NULL,
  scope_id INTEGER NOT NULL,
  normalized_name TEXT NOT NULL,
  PRIMARY KEY (entity_kind, entity_id)
);

WITH RECURSIVE
catalog_rows(entity_kind, entity_id, scope_id, display_name) AS (
  SELECT 'make', id, 0, name
  FROM aircraft_makes
  UNION ALL
  SELECT 'family', id, aircraft_make_id, name
  FROM aircraft_model_families
  UNION ALL
  SELECT 'generation', id, aircraft_model_family_id, name
  FROM aircraft_generations
  UNION ALL
  SELECT 'package', id, aircraft_model_family_id, name
  FROM aircraft_factory_packages
),
normalized(
  entity_kind, entity_id, scope_id, display_name, character_offset,
  normalized_name
) AS (
  SELECT entity_kind, entity_id, scope_id, display_name, 1, ''
  FROM catalog_rows
  UNION ALL
  SELECT
    entity_kind,
    entity_id,
    scope_id,
    display_name,
    character_offset + 1,
    CASE
      WHEN substr(display_name, character_offset, 1)
             GLOB '[A-Za-z0-9]'
        THEN normalized_name ||
             lower(substr(display_name, character_offset, 1))
      WHEN normalized_name <> ''
        AND substr(normalized_name, -1, 1) <> ' '
        THEN normalized_name || ' '
      ELSE normalized_name
    END
  FROM normalized
  WHERE character_offset <= length(display_name)
)
INSERT INTO temp.aircraft_catalog_retrieval_key_repairs (
  entity_kind, entity_id, scope_id, normalized_name
)
SELECT
  entity_kind,
  entity_id,
  scope_id,
  rtrim(normalized_name)
FROM normalized
WHERE character_offset > length(display_name);

DROP TABLE IF EXISTS temp.aircraft_catalog_retrieval_key_guard;
CREATE TEMP TABLE aircraft_catalog_retrieval_key_guard (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO temp.aircraft_catalog_retrieval_key_guard (valid)
SELECT 0
WHERE EXISTS (
  SELECT 1
  FROM temp.aircraft_catalog_retrieval_key_repairs repair
  WHERE repair.normalized_name = ''
)
OR EXISTS (
  SELECT 1
  FROM temp.aircraft_catalog_retrieval_key_repairs left_repair
  JOIN temp.aircraft_catalog_retrieval_key_repairs right_repair
    ON right_repair.entity_kind = left_repair.entity_kind
   AND right_repair.scope_id = left_repair.scope_id
   AND right_repair.normalized_name = left_repair.normalized_name
   AND right_repair.entity_id > left_repair.entity_id
)
OR EXISTS (
  SELECT 1
  FROM temp.aircraft_catalog_retrieval_key_repairs repair
  JOIN aircraft_make_aliases alias
    ON alias.normalized_alias = repair.normalized_name
   AND alias.aircraft_make_id <> repair.entity_id
  WHERE repair.entity_kind = 'make'
)
OR EXISTS (
  SELECT 1
  FROM aircraft_makes
  WHERE normalized_name GLOB '__aircost_catalog_key_repair__*'
)
OR EXISTS (
  SELECT 1
  FROM aircraft_model_families
  WHERE normalized_name GLOB '__aircost_catalog_key_repair__*'
)
OR EXISTS (
  SELECT 1
  FROM aircraft_generations
  WHERE normalized_name GLOB '__aircost_catalog_key_repair__*'
)
OR EXISTS (
  SELECT 1
  FROM aircraft_factory_packages
  WHERE normalized_name GLOB '__aircost_catalog_key_repair__*'
);
DROP TABLE temp.aircraft_catalog_retrieval_key_guard;

-- A rerun on an already repaired database has no changed rows. Dropping the
-- validator triggers also makes the two-phase unique-key swap safe if a future
-- legacy database contains keys whose old and repaired values cross.
DROP TRIGGER IF EXISTS aircraft_make_retrieval_key_validate_insert;
DROP TRIGGER IF EXISTS aircraft_make_retrieval_key_validate_update;
DROP TRIGGER IF EXISTS aircraft_family_retrieval_key_validate_insert;
DROP TRIGGER IF EXISTS aircraft_family_retrieval_key_validate_update;
DROP TRIGGER IF EXISTS aircraft_generation_retrieval_key_validate_insert;
DROP TRIGGER IF EXISTS aircraft_generation_retrieval_key_validate_update;
DROP TRIGGER IF EXISTS aircraft_package_retrieval_key_validate_insert;
DROP TRIGGER IF EXISTS aircraft_package_retrieval_key_validate_update;

-- Catalog IDs used by a listing or valuation projection are normally
-- immutable. Only their UPDATE triggers are suspended for this narrow key
-- repair; every DELETE trigger and all approval/provenance constraints remain.
DROP TRIGGER IF EXISTS assigned_aircraft_make_immutable_update;
DROP TRIGGER IF EXISTS assigned_aircraft_family_immutable_update;
DROP TRIGGER IF EXISTS assigned_aircraft_generation_immutable_update;
DROP TRIGGER IF EXISTS assigned_aircraft_package_immutable_update;
DROP TRIGGER IF EXISTS compatibility_projected_make_immutable_update;
DROP TRIGGER IF EXISTS compatibility_projected_family_immutable_update;
DROP TRIGGER IF EXISTS compatibility_projected_generation_immutable_update;
DROP TRIGGER IF EXISTS compatibility_projected_package_immutable_update;

UPDATE aircraft_makes
SET normalized_name =
      '__aircost_catalog_key_repair__make_' || id,
    updated_at = CURRENT_TIMESTAMP
WHERE EXISTS (
  SELECT 1
  FROM temp.aircraft_catalog_retrieval_key_repairs repair
  WHERE repair.entity_kind = 'make'
    AND repair.entity_id = aircraft_makes.id
    AND repair.normalized_name <> aircraft_makes.normalized_name
);
UPDATE aircraft_model_families
SET normalized_name =
      '__aircost_catalog_key_repair__family_' || id,
    updated_at = CURRENT_TIMESTAMP
WHERE EXISTS (
  SELECT 1
  FROM temp.aircraft_catalog_retrieval_key_repairs repair
  WHERE repair.entity_kind = 'family'
    AND repair.entity_id = aircraft_model_families.id
    AND repair.normalized_name <> aircraft_model_families.normalized_name
);
UPDATE aircraft_generations
SET normalized_name =
      '__aircost_catalog_key_repair__generation_' || id,
    updated_at = CURRENT_TIMESTAMP
WHERE EXISTS (
  SELECT 1
  FROM temp.aircraft_catalog_retrieval_key_repairs repair
  WHERE repair.entity_kind = 'generation'
    AND repair.entity_id = aircraft_generations.id
    AND repair.normalized_name <> aircraft_generations.normalized_name
);
UPDATE aircraft_factory_packages
SET normalized_name =
      '__aircost_catalog_key_repair__package_' || id,
    updated_at = CURRENT_TIMESTAMP
WHERE EXISTS (
  SELECT 1
  FROM temp.aircraft_catalog_retrieval_key_repairs repair
  WHERE repair.entity_kind = 'package'
    AND repair.entity_id = aircraft_factory_packages.id
    AND repair.normalized_name <> aircraft_factory_packages.normalized_name
);

UPDATE aircraft_makes
SET normalized_name = (
      SELECT repair.normalized_name
      FROM temp.aircraft_catalog_retrieval_key_repairs repair
      WHERE repair.entity_kind = 'make'
        AND repair.entity_id = aircraft_makes.id
    )
WHERE normalized_name GLOB '__aircost_catalog_key_repair__make_*';
UPDATE aircraft_model_families
SET normalized_name = (
      SELECT repair.normalized_name
      FROM temp.aircraft_catalog_retrieval_key_repairs repair
      WHERE repair.entity_kind = 'family'
        AND repair.entity_id = aircraft_model_families.id
    )
WHERE normalized_name GLOB '__aircost_catalog_key_repair__family_*';
UPDATE aircraft_generations
SET normalized_name = (
      SELECT repair.normalized_name
      FROM temp.aircraft_catalog_retrieval_key_repairs repair
      WHERE repair.entity_kind = 'generation'
        AND repair.entity_id = aircraft_generations.id
    )
WHERE normalized_name GLOB '__aircost_catalog_key_repair__generation_*';
UPDATE aircraft_factory_packages
SET normalized_name = (
      SELECT repair.normalized_name
      FROM temp.aircraft_catalog_retrieval_key_repairs repair
      WHERE repair.entity_kind = 'package'
        AND repair.entity_id = aircraft_factory_packages.id
    )
WHERE normalized_name GLOB '__aircost_catalog_key_repair__package_*';

-- Enforce the same mechanical key contract for all subsequent catalog writes.
CREATE TRIGGER aircraft_make_retrieval_key_validate_insert
BEFORE INSERT ON aircraft_makes
BEGIN
  SELECT RAISE(ABORT, 'aircraft make requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;
CREATE TRIGGER aircraft_make_retrieval_key_validate_update
BEFORE UPDATE OF name, normalized_name ON aircraft_makes
BEGIN
  SELECT RAISE(ABORT, 'aircraft make requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;

CREATE TRIGGER aircraft_family_retrieval_key_validate_insert
BEFORE INSERT ON aircraft_model_families
BEGIN
  SELECT RAISE(ABORT, 'aircraft family requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;
CREATE TRIGGER aircraft_family_retrieval_key_validate_update
BEFORE UPDATE OF name, normalized_name ON aircraft_model_families
BEGIN
  SELECT RAISE(ABORT, 'aircraft family requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;

CREATE TRIGGER aircraft_generation_retrieval_key_validate_insert
BEFORE INSERT ON aircraft_generations
BEGIN
  SELECT RAISE(ABORT, 'aircraft generation requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;
CREATE TRIGGER aircraft_generation_retrieval_key_validate_update
BEFORE UPDATE OF name, normalized_name ON aircraft_generations
BEGIN
  SELECT RAISE(ABORT, 'aircraft generation requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;

CREATE TRIGGER aircraft_package_retrieval_key_validate_insert
BEFORE INSERT ON aircraft_factory_packages
BEGIN
  SELECT RAISE(ABORT, 'aircraft package requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;
CREATE TRIGGER aircraft_package_retrieval_key_validate_update
BEFORE UPDATE OF name, normalized_name ON aircraft_factory_packages
BEGIN
  SELECT RAISE(ABORT, 'aircraft package requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;

CREATE TRIGGER assigned_aircraft_make_immutable_update
BEFORE UPDATE ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_make_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft makes are immutable'); END;
CREATE TRIGGER assigned_aircraft_family_immutable_update
BEFORE UPDATE ON aircraft_model_families
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_model_family_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft model families are immutable'); END;
CREATE TRIGGER assigned_aircraft_generation_immutable_update
BEFORE UPDATE ON aircraft_generations
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_generation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft generations are immutable'); END;
CREATE TRIGGER assigned_aircraft_package_immutable_update
BEFORE UPDATE ON aircraft_factory_packages
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_factory_package_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft factory packages are immutable'); END;

CREATE TRIGGER compatibility_projected_make_immutable_update
BEFORE UPDATE ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_make_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft makes are immutable');
END;
CREATE TRIGGER compatibility_projected_family_immutable_update
BEFORE UPDATE ON aircraft_model_families
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_family_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft families are immutable');
END;
CREATE TRIGGER compatibility_projected_generation_immutable_update
BEFORE UPDATE ON aircraft_generations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_generation_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'compatibility-projected aircraft generations are immutable');
END;
CREATE TRIGGER compatibility_projected_package_immutable_update
BEFORE UPDATE ON aircraft_factory_packages
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_factory_package_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'compatibility-projected aircraft packages are immutable');
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260729_aircraft_catalog_retrieval_keys',
  1,
  'b40b266fc450810cf89acc78c9405f4cd7d816ea38d389114e93a20cfea6901d',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

DROP TABLE temp.aircraft_catalog_retrieval_key_repairs;

COMMIT;
PRAGMA foreign_key_check;
