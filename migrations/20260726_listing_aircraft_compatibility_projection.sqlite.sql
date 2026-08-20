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

CREATE TEMP TABLE aircraft_compatibility_projection_migration_contract_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO aircraft_compatibility_projection_migration_contract_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260726_listing_aircraft_compatibility_projection'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260726_listing_aircraft_compatibility_projection'
      AND contract_version = 2
      AND contract_fingerprint =
        '0a182d5972d62be3d906395df8d08b741bc3e23d713badf7596b360048aa45ba'
  ) THEN 1
  ELSE 0
END;
DROP TABLE aircraft_compatibility_projection_migration_contract_guard;

-- Every unresolved new listing points at one schema-owned placeholder. Literal
-- extracted labels live only in aircraft_identity_observations.
CREATE TABLE IF NOT EXISTS aircraft_sale_listing_pending_compatibility_placeholder (
  singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
  aircraft_manufacturer_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_manufacturers(id) ON DELETE RESTRICT,
  aircraft_model_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_models(id) ON DELETE RESTRICT,
  aircraft_model_variant_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_model_variants(id) ON DELETE RESTRICT
);

-- Parsed/manual fields are useful retrieval hints but are not quoted source
-- evidence. Keep them in an explicitly non-authoritative staging table rather
-- than weakening aircraft_identity_observations.exact_source_evidence.
CREATE TABLE IF NOT EXISTS aircraft_listing_identity_input_observations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL,
  source_url TEXT,
  observed_make TEXT NOT NULL,
  observed_family TEXT NOT NULL,
  observed_designation TEXT NOT NULL,
  model_year INTEGER NOT NULL CHECK (model_year BETWEEN 1900 AND 2200),
  serial_number TEXT,
  registration_number TEXT,
  input_json TEXT NOT NULL,
  observation_sha256 TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(observed_make)) > 0),
  CHECK (length(trim(observed_family)) > 0),
  CHECK (length(trim(observed_designation)) > 0),
  CHECK (length(trim(input_json)) > 0)
);
CREATE INDEX IF NOT EXISTS idx_aircraft_listing_identity_input_listing
  ON aircraft_listing_identity_input_observations (aircraft_sale_listing_id);

-- Raw input history is append-only. Deleting its parent listing preserves the
-- observation and may clear only the nullable listing reference through the
-- declared ON DELETE SET NULL action.
CREATE TRIGGER IF NOT EXISTS aircraft_listing_identity_input_append_only_update
BEFORE UPDATE ON aircraft_listing_identity_input_observations
WHEN NOT (
  OLD.aircraft_sale_listing_id IS NOT NULL
  AND NEW.aircraft_sale_listing_id IS NULL
  AND NOT EXISTS (
    SELECT 1 FROM aircraft_sale_listings listing
    WHERE listing.id = OLD.aircraft_sale_listing_id
  )
  AND NEW.id IS OLD.id
  AND NEW.source_url IS OLD.source_url
  AND NEW.observed_make IS OLD.observed_make
  AND NEW.observed_family IS OLD.observed_family
  AND NEW.observed_designation IS OLD.observed_designation
  AND NEW.model_year IS OLD.model_year
  AND NEW.serial_number IS OLD.serial_number
  AND NEW.registration_number IS OLD.registration_number
  AND NEW.input_json IS OLD.input_json
  AND NEW.observation_sha256 IS OLD.observation_sha256
  AND NEW.created_at IS OLD.created_at
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft listing identity input observations are append-only');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_listing_identity_input_append_only_delete
BEFORE DELETE ON aircraft_listing_identity_input_observations
BEGIN
  SELECT RAISE(ABORT, 'aircraft listing identity input observations are append-only');
END;

CREATE TABLE IF NOT EXISTS aircraft_compatibility_placeholder_preseed_guard (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO aircraft_compatibility_placeholder_preseed_guard (valid)
SELECT 0
WHERE NOT EXISTS (
  SELECT 1 FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
AND (
  EXISTS (
    SELECT 1 FROM aircraft_manufacturers
    WHERE id = -1 OR normalized_name = '__aircost_pending_faa_make__'
  )
  OR EXISTS (
    SELECT 1 FROM aircraft_models
    WHERE id = -1 OR normalized_name = '__aircost_pending_faa_family__'
  )
  OR EXISTS (
    SELECT 1 FROM aircraft_model_variants
    WHERE id = -1 OR normalized_name = '__aircost_pending_faa_identity__'
  )
);
DROP TABLE aircraft_compatibility_placeholder_preseed_guard;

INSERT INTO aircraft_manufacturers (id, name, normalized_name)
VALUES (-1, 'Pending FAA curation', '__aircost_pending_faa_make__')
ON CONFLICT (normalized_name) DO NOTHING;

INSERT INTO aircraft_models (
  id, aircraft_manufacturer_id, name, normalized_name
)
SELECT -1, id, 'Pending FAA curation', '__aircost_pending_faa_family__'
FROM aircraft_manufacturers
WHERE normalized_name = '__aircost_pending_faa_make__'
ON CONFLICT (aircraft_manufacturer_id, normalized_name) DO NOTHING;

INSERT INTO aircraft_model_variants (
  id, aircraft_model_id, name, normalized_name
)
SELECT -1, id, 'Pending FAA curation', '__aircost_pending_faa_identity__'
FROM aircraft_models
WHERE normalized_name = '__aircost_pending_faa_family__'
  AND aircraft_manufacturer_id = (
    SELECT id FROM aircraft_manufacturers
    WHERE normalized_name = '__aircost_pending_faa_make__'
  )
ON CONFLICT (aircraft_model_id, normalized_name) DO NOTHING;

INSERT INTO aircraft_sale_listing_pending_compatibility_placeholder (
  singleton_id, aircraft_manufacturer_id, aircraft_model_id,
  aircraft_model_variant_id
)
SELECT 1, manufacturer.id, model.id, variant.id
FROM aircraft_manufacturers manufacturer
JOIN aircraft_models model
  ON model.aircraft_manufacturer_id = manufacturer.id
JOIN aircraft_model_variants variant
  ON variant.aircraft_model_id = model.id
WHERE manufacturer.name = 'Pending FAA curation'
  AND manufacturer.normalized_name = '__aircost_pending_faa_make__'
  AND model.name = 'Pending FAA curation'
  AND model.normalized_name = '__aircost_pending_faa_family__'
  AND variant.name = 'Pending FAA curation'
  AND variant.normalized_name = '__aircost_pending_faa_identity__'
ON CONFLICT (singleton_id) DO NOTHING;

CREATE TABLE IF NOT EXISTS aircraft_compatibility_placeholder_upgrade_guard (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
DELETE FROM aircraft_compatibility_placeholder_upgrade_guard;
INSERT INTO aircraft_compatibility_placeholder_upgrade_guard (valid)
SELECT 0
WHERE NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
  JOIN aircraft_manufacturers manufacturer
    ON manufacturer.id = placeholder.aircraft_manufacturer_id
  JOIN aircraft_models model
    ON model.id = placeholder.aircraft_model_id
   AND model.aircraft_manufacturer_id = manufacturer.id
  JOIN aircraft_model_variants variant
    ON variant.id = placeholder.aircraft_model_variant_id
   AND variant.aircraft_model_id = model.id
  WHERE placeholder.singleton_id = 1
    AND manufacturer.id = -1
    AND manufacturer.name = 'Pending FAA curation'
    AND manufacturer.normalized_name = '__aircost_pending_faa_make__'
    AND model.id = -1
    AND model.name = 'Pending FAA curation'
    AND model.normalized_name = '__aircost_pending_faa_family__'
    AND variant.id = -1
    AND variant.name = 'Pending FAA curation'
    AND variant.normalized_name = '__aircost_pending_faa_identity__'
);
DROP TABLE aircraft_compatibility_placeholder_upgrade_guard;

-- The sole bridge from canonical aircraft identity to the legacy valuation
-- hierarchy. Provenance is copied from the live immutable assignment at
-- creation so the projection survives later deletion of the source listing.
CREATE TABLE IF NOT EXISTS aircraft_valuation_compatibility_projections (
  aircraft_model_variant_id INTEGER PRIMARY KEY
    REFERENCES aircraft_model_variants(id) ON DELETE RESTRICT,
  aircraft_make_id INTEGER NOT NULL,
  aircraft_model_family_id INTEGER NOT NULL,
  aircraft_designation_id INTEGER NOT NULL,
  aircraft_generation_id INTEGER,
  aircraft_factory_package_id INTEGER,
  created_from_aircraft_sale_listing_id INTEGER NOT NULL,
  created_from_identity_assignment_id INTEGER NOT NULL,
  identity_decision_id INTEGER NOT NULL
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  faa_registry_snapshot_id INTEGER NOT NULL
    REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  faa_n_number TEXT NOT NULL,
  faa_source_record_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (aircraft_model_family_id, aircraft_make_id)
    REFERENCES aircraft_model_families(id, aircraft_make_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_designation_id, aircraft_model_family_id)
    REFERENCES aircraft_designations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_generation_id, aircraft_model_family_id)
    REFERENCES aircraft_generations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_factory_package_id, aircraft_model_family_id)
    REFERENCES aircraft_factory_packages(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, faa_n_number)
    REFERENCES faa_registry_aircraft(snapshot_id, n_number) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, faa_source_record_sha256)
    REFERENCES faa_registry_aircraft(snapshot_id, source_record_sha256) ON DELETE RESTRICT,
  CHECK (aircraft_make_id > 0),
  CHECK (aircraft_model_family_id > 0),
  CHECK (aircraft_designation_id > 0),
  CHECK (aircraft_generation_id IS NULL OR aircraft_generation_id > 0),
  CHECK (aircraft_factory_package_id IS NULL OR aircraft_factory_package_id > 0),
  CHECK (created_from_aircraft_sale_listing_id > 0),
  CHECK (created_from_identity_assignment_id > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_valuation_projection_identity
  ON aircraft_valuation_compatibility_projections (
    aircraft_make_id, aircraft_model_family_id, aircraft_designation_id,
    coalesce(aircraft_generation_id, 0),
    coalesce(aircraft_factory_package_id, 0)
  );

CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_pending_compatibility_placeholder
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility placeholder is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_immutable_delete
BEFORE DELETE ON aircraft_sale_listing_pending_compatibility_placeholder
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility placeholder is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_manufacturer_immutable_update
BEFORE UPDATE ON aircraft_manufacturers
WHEN OLD.id = (
  SELECT aircraft_manufacturer_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility manufacturer is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_manufacturer_immutable_delete
BEFORE DELETE ON aircraft_manufacturers
WHEN OLD.id = (
  SELECT aircraft_manufacturer_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility manufacturer is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_model_immutable_update
BEFORE UPDATE ON aircraft_models
WHEN OLD.id = (
  SELECT aircraft_model_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility model is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_model_immutable_delete
BEFORE DELETE ON aircraft_models
WHEN OLD.id = (
  SELECT aircraft_model_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility model is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_variant_immutable_update
BEFORE UPDATE ON aircraft_model_variants
WHEN OLD.id = (
  SELECT aircraft_model_variant_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility variant is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_variant_immutable_delete
BEFORE DELETE ON aircraft_model_variants
WHEN OLD.id = (
  SELECT aircraft_model_variant_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility variant is immutable'); END;

CREATE TRIGGER IF NOT EXISTS listing_insert_requires_aircraft_projection_or_placeholder
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.aircraft_model_variant_id <> (
  SELECT aircraft_model_variant_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
AND NOT EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_variant_id =
        NEW.aircraft_model_variant_id
)
BEGIN
  SELECT RAISE(ABORT, 'new listing must use the pending aircraft placeholder or an existing canonical projection');
END;

-- A transition is deliberately short-lived. Its insertion proves that the
-- target is either the exact existing projection or a fresh, unreferenced,
-- deterministic reserved-key variant for the assignment.
CREATE TABLE IF NOT EXISTS aircraft_valuation_projection_transitions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  identity_assignment_id INTEGER NOT NULL,
  transition_kind TEXT NOT NULL CHECK (
    transition_kind IN ('initial', 'current_repair', 'successor')
  ),
  selected_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (identity_assignment_id, aircraft_sale_listing_id)
    REFERENCES aircraft_sale_listing_identity_assignments(
      id, aircraft_sale_listing_id
    ) ON DELETE CASCADE
);

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_transition_validate_insert
BEFORE INSERT ON aircraft_valuation_projection_transitions
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = NEW.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = listing.id
  JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
  JOIN aircraft_model_families family
    ON family.id = assignment.aircraft_model_family_id
   AND family.aircraft_make_id = make.id
  JOIN aircraft_designations designation
    ON designation.id = assignment.aircraft_designation_id
   AND designation.aircraft_model_family_id = family.id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = assignment.faa_registry_snapshot_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
    AND listing.ingestion_state <> 'ready'
    AND assignment.aircraft_make_id > 0
    AND assignment.aircraft_model_family_id > 0
    AND assignment.aircraft_designation_id > 0
    AND (
      assignment.aircraft_generation_id IS NULL
      OR assignment.aircraft_generation_id > 0
    )
    AND (
      assignment.aircraft_factory_package_id IS NULL
      OR assignment.aircraft_factory_package_id > 0
    )
    AND snapshot.id = (
      SELECT id FROM faa_registry_snapshots
      ORDER BY snapshot_date DESC, id DESC LIMIT 1
    )
    AND (
      (NEW.transition_kind = 'initial'
        AND assignment.supersedes_assignment_id IS NULL
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_current_identity_assignments current_assignment
          WHERE current_assignment.aircraft_sale_listing_id = listing.id
        ))
      OR (NEW.transition_kind = 'current_repair'
        AND EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_current_identity_assignments current_assignment
          WHERE current_assignment.aircraft_sale_listing_id = listing.id
            AND current_assignment.identity_assignment_id = assignment.id
        ))
      OR (NEW.transition_kind = 'successor'
        AND EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_current_identity_assignments current_assignment
          WHERE current_assignment.aircraft_sale_listing_id = listing.id
            AND current_assignment.identity_assignment_id =
                  assignment.supersedes_assignment_id
        ))
    )
    AND (
      EXISTS (
        SELECT 1
        FROM aircraft_valuation_compatibility_projections projection
        WHERE projection.aircraft_make_id = assignment.aircraft_make_id
          AND projection.aircraft_model_family_id =
                assignment.aircraft_model_family_id
          AND projection.aircraft_designation_id =
                assignment.aircraft_designation_id
          AND projection.aircraft_generation_id IS
                assignment.aircraft_generation_id
          AND projection.aircraft_factory_package_id IS
                assignment.aircraft_factory_package_id
      )
      OR (
        (
          NOT EXISTS (
            SELECT 1 FROM aircraft_manufacturers
            WHERE normalized_name =
              '__aircost_projection_make_' || make.id || '__'
          )
          OR EXISTS (
            SELECT 1
            FROM aircraft_valuation_compatibility_projections projection
            JOIN aircraft_model_variants projected_variant
              ON projected_variant.id = projection.aircraft_model_variant_id
            JOIN aircraft_models projected_model
              ON projected_model.id = projected_variant.aircraft_model_id
            JOIN aircraft_manufacturers projected_manufacturer
              ON projected_manufacturer.id =
                   projected_model.aircraft_manufacturer_id
            WHERE projection.aircraft_make_id = make.id
              AND projected_manufacturer.name = make.name
              AND projected_manufacturer.normalized_name =
                   '__aircost_projection_make_' || make.id || '__'
          )
        )
        AND (
          NOT EXISTS (
            SELECT 1 FROM aircraft_models
            WHERE normalized_name =
              '__aircost_projection_family_' || family.id || '__'
          )
          OR EXISTS (
            SELECT 1
            FROM aircraft_valuation_compatibility_projections projection
            JOIN aircraft_model_variants projected_variant
              ON projected_variant.id = projection.aircraft_model_variant_id
            JOIN aircraft_models projected_model
              ON projected_model.id = projected_variant.aircraft_model_id
            WHERE projection.aircraft_model_family_id = family.id
              AND projected_model.name = family.name
              AND projected_model.normalized_name =
                   '__aircost_projection_family_' || family.id || '__'
          )
        )
        AND NOT EXISTS (
          SELECT 1 FROM aircraft_model_variants
          WHERE normalized_name =
            '__aircost_projection_identity_'
            || designation.id || '_'
            || coalesce(assignment.aircraft_generation_id, 0) || '_'
            || coalesce(assignment.aircraft_factory_package_id, 0) || '__'
        )
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft projection command requires a current FAA assignment and either an exact projection or collision-free reserved keys');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_transition_immutable_update
BEFORE UPDATE ON aircraft_valuation_projection_transitions
BEGIN SELECT RAISE(ABORT, 'aircraft projection transitions are immutable'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_projection_validate_insert
BEFORE INSERT ON aircraft_valuation_compatibility_projections
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_valuation_projection_transitions transition
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = transition.identity_assignment_id
   AND assignment.aircraft_sale_listing_id =
         transition.aircraft_sale_listing_id
  JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
  JOIN aircraft_model_families family
    ON family.id = assignment.aircraft_model_family_id
   AND family.aircraft_make_id = make.id
  JOIN aircraft_designations designation
    ON designation.id = assignment.aircraft_designation_id
   AND designation.aircraft_model_family_id = family.id
  LEFT JOIN aircraft_generations generation
    ON generation.id = assignment.aircraft_generation_id
   AND generation.aircraft_model_family_id = family.id
  LEFT JOIN aircraft_factory_packages package
    ON package.id = assignment.aircraft_factory_package_id
   AND package.aircraft_model_family_id = family.id
  JOIN aircraft_model_variants legacy_variant
    ON legacy_variant.id = NEW.aircraft_model_variant_id
  JOIN aircraft_models legacy_model
    ON legacy_model.id = legacy_variant.aircraft_model_id
  JOIN aircraft_manufacturers legacy_manufacturer
    ON legacy_manufacturer.id = legacy_model.aircraft_manufacturer_id
  WHERE assignment.aircraft_make_id = NEW.aircraft_make_id
    AND assignment.aircraft_model_family_id = NEW.aircraft_model_family_id
    AND assignment.aircraft_designation_id = NEW.aircraft_designation_id
    AND assignment.aircraft_generation_id IS NEW.aircraft_generation_id
    AND assignment.aircraft_factory_package_id IS
          NEW.aircraft_factory_package_id
    AND assignment.aircraft_sale_listing_id =
          NEW.created_from_aircraft_sale_listing_id
    AND assignment.id = NEW.created_from_identity_assignment_id
    AND assignment.identity_decision_id = NEW.identity_decision_id
    AND assignment.identity_evidence_claim_id = NEW.identity_evidence_claim_id
    AND assignment.faa_registry_snapshot_id = NEW.faa_registry_snapshot_id
    AND assignment.faa_n_number = NEW.faa_n_number
    AND assignment.faa_source_record_sha256 = NEW.faa_source_record_sha256
    AND legacy_manufacturer.name = make.name
    AND legacy_manufacturer.normalized_name =
          '__aircost_projection_make_' || make.id || '__'
    AND legacy_model.name = family.name
    AND legacy_model.normalized_name =
          '__aircost_projection_family_' || family.id || '__'
    AND legacy_variant.name =
      designation.official_designation
      || CASE WHEN generation.id IS NULL THEN '' ELSE ' / ' || generation.name END
      || CASE WHEN package.id IS NULL THEN '' ELSE ' / ' || package.name END
    AND legacy_variant.normalized_name =
      '__aircost_projection_identity_'
      || designation.id || '_'
      || coalesce(generation.id, 0) || '_'
      || coalesce(package.id, 0) || '__'
    AND (
      assignment.aircraft_generation_id IS NULL
      OR EXISTS (
        SELECT 1 FROM aircraft_generation_designations applicability
        WHERE applicability.aircraft_generation_id =
              assignment.aircraft_generation_id
          AND applicability.aircraft_designation_id =
              assignment.aircraft_designation_id
      )
    )
    AND (
      assignment.aircraft_factory_package_id IS NULL
      OR EXISTS (
        SELECT 1 FROM aircraft_package_applicability applicability
        WHERE applicability.aircraft_factory_package_id =
              assignment.aircraft_factory_package_id
          AND applicability.aircraft_designation_id =
              assignment.aircraft_designation_id
          AND (
            applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id IS
                  assignment.aircraft_generation_id
          )
      )
    )
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_sale_listings child
      WHERE child.aircraft_model_variant_id = legacy_variant.id
    )
    AND NOT EXISTS (
      SELECT 1 FROM rental_aircraft_offerings child
      WHERE child.aircraft_model_variant_id = legacy_variant.id
    )
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_model_spec_versions child
      WHERE child.aircraft_model_variant_id = legacy_variant.id
    )
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_model_variant_price_points child
      WHERE child.aircraft_model_variant_id = legacy_variant.id
    )
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_model_variant_default_avionics child
      WHERE child.aircraft_model_variant_id = legacy_variant.id
    )
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft compatibility projection requires the active command, exact copied assignment provenance, and its fresh reserved hierarchy');
END;

-- A transition row is a command, not durable state. The command creates the
-- projection when needed, repoints the listing, selects the assignment, and
-- deletes itself inside the same INSERT statement. Any failed sub-step rolls
-- the entire statement back, so no committed bypass capability can remain.
CREATE TRIGGER IF NOT EXISTS aircraft_valuation_transition_execute
AFTER INSERT ON aircraft_valuation_projection_transitions
BEGIN
  INSERT INTO aircraft_manufacturers (name, normalized_name)
  SELECT
    make.name,
    '__aircost_projection_make_' || make.id || '__'
  FROM aircraft_sale_listing_identity_assignments assignment
  JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id =
          NEW.aircraft_sale_listing_id
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_manufacturers existing
      WHERE existing.normalized_name =
        '__aircost_projection_make_' || make.id || '__'
    );

  INSERT INTO aircraft_models (
    aircraft_manufacturer_id, name, normalized_name
  )
  SELECT
    legacy_manufacturer.id,
    family.name,
    '__aircost_projection_family_' || family.id || '__'
  FROM aircraft_sale_listing_identity_assignments assignment
  JOIN aircraft_model_families family
    ON family.id = assignment.aircraft_model_family_id
  JOIN aircraft_manufacturers legacy_manufacturer
    ON legacy_manufacturer.normalized_name =
       '__aircost_projection_make_' || assignment.aircraft_make_id || '__'
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id =
          NEW.aircraft_sale_listing_id
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_models existing
      WHERE existing.normalized_name =
        '__aircost_projection_family_' || family.id || '__'
    );

  INSERT INTO aircraft_model_variants (
    aircraft_model_id, name, normalized_name
  )
  SELECT
    legacy_model.id,
    designation.official_designation
      || CASE WHEN generation.id IS NULL THEN '' ELSE ' / ' || generation.name END
      || CASE WHEN package.id IS NULL THEN '' ELSE ' / ' || package.name END,
    '__aircost_projection_identity_'
      || designation.id || '_'
      || coalesce(generation.id, 0) || '_'
      || coalesce(package.id, 0) || '__'
  FROM aircraft_sale_listing_identity_assignments assignment
  JOIN aircraft_designations designation
    ON designation.id = assignment.aircraft_designation_id
  LEFT JOIN aircraft_generations generation
    ON generation.id = assignment.aircraft_generation_id
  LEFT JOIN aircraft_factory_packages package
    ON package.id = assignment.aircraft_factory_package_id
  JOIN aircraft_models legacy_model
    ON legacy_model.normalized_name =
       '__aircost_projection_family_'
       || assignment.aircraft_model_family_id || '__'
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id =
          NEW.aircraft_sale_listing_id
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_model_variants existing
      WHERE existing.normalized_name =
        '__aircost_projection_identity_'
        || designation.id || '_'
        || coalesce(generation.id, 0) || '_'
        || coalesce(package.id, 0) || '__'
    );

  INSERT INTO aircraft_valuation_compatibility_projections (
    aircraft_model_variant_id,
    aircraft_make_id,
    aircraft_model_family_id,
    aircraft_designation_id,
    aircraft_generation_id,
    aircraft_factory_package_id,
    created_from_aircraft_sale_listing_id,
    created_from_identity_assignment_id,
    identity_decision_id,
    identity_evidence_claim_id,
    faa_registry_snapshot_id,
    faa_n_number,
    faa_source_record_sha256
  )
  SELECT
    legacy_variant.id,
    assignment.aircraft_make_id,
    assignment.aircraft_model_family_id,
    assignment.aircraft_designation_id,
    assignment.aircraft_generation_id,
    assignment.aircraft_factory_package_id,
    assignment.aircraft_sale_listing_id,
    assignment.id,
    assignment.identity_decision_id,
    assignment.identity_evidence_claim_id,
    assignment.faa_registry_snapshot_id,
    assignment.faa_n_number,
    assignment.faa_source_record_sha256
  FROM aircraft_sale_listing_identity_assignments assignment
  JOIN aircraft_model_variants legacy_variant
    ON legacy_variant.normalized_name =
       '__aircost_projection_identity_'
       || assignment.aircraft_designation_id || '_'
       || coalesce(assignment.aircraft_generation_id, 0) || '_'
       || coalesce(assignment.aircraft_factory_package_id, 0) || '__'
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id =
          NEW.aircraft_sale_listing_id
    AND NOT EXISTS (
      SELECT 1
      FROM aircraft_valuation_compatibility_projections projection
      WHERE projection.aircraft_make_id = assignment.aircraft_make_id
        AND projection.aircraft_model_family_id =
              assignment.aircraft_model_family_id
        AND projection.aircraft_designation_id =
              assignment.aircraft_designation_id
        AND projection.aircraft_generation_id IS
              assignment.aircraft_generation_id
        AND projection.aircraft_factory_package_id IS
              assignment.aircraft_factory_package_id
    );

  UPDATE aircraft_sale_listings
  SET aircraft_model_variant_id = (
        SELECT projection.aircraft_model_variant_id
        FROM aircraft_valuation_compatibility_projections projection
        JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = NEW.identity_assignment_id
         AND assignment.aircraft_sale_listing_id =
               NEW.aircraft_sale_listing_id
         AND projection.aircraft_make_id = assignment.aircraft_make_id
         AND projection.aircraft_model_family_id =
               assignment.aircraft_model_family_id
         AND projection.aircraft_designation_id =
               assignment.aircraft_designation_id
         AND projection.aircraft_generation_id IS
               assignment.aircraft_generation_id
         AND projection.aircraft_factory_package_id IS
               assignment.aircraft_factory_package_id
      ),
      updated_at = CURRENT_TIMESTAMP
  WHERE id = NEW.aircraft_sale_listing_id;

  INSERT INTO aircraft_sale_listing_current_identity_assignments (
    aircraft_sale_listing_id, identity_assignment_id, selected_at
  )
  SELECT
    NEW.aircraft_sale_listing_id, NEW.identity_assignment_id, NEW.selected_at
  WHERE NEW.transition_kind = 'initial';

  UPDATE aircraft_sale_listing_current_identity_assignments
  SET identity_assignment_id = NEW.identity_assignment_id,
      selected_at = NEW.selected_at
  WHERE NEW.transition_kind = 'successor'
    AND aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
    AND identity_assignment_id = (
      SELECT supersedes_assignment_id
      FROM aircraft_sale_listing_identity_assignments
      WHERE id = NEW.identity_assignment_id
        AND aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
    );

  DELETE FROM aircraft_valuation_projection_transitions
  WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_projection_immutable_update
BEFORE UPDATE ON aircraft_valuation_compatibility_projections
BEGIN SELECT RAISE(ABORT, 'aircraft compatibility projections are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_valuation_projection_immutable_delete
BEFORE DELETE ON aircraft_valuation_compatibility_projections
BEGIN SELECT RAISE(ABORT, 'aircraft compatibility projections are immutable'); END;

CREATE TRIGGER IF NOT EXISTS projected_aircraft_variant_immutable_update
BEFORE UPDATE ON aircraft_model_variants
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_variant_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft variants are immutable'); END;
CREATE TRIGGER IF NOT EXISTS projected_aircraft_variant_immutable_delete
BEFORE DELETE ON aircraft_model_variants
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_variant_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft variants are immutable'); END;

CREATE TRIGGER IF NOT EXISTS projected_aircraft_model_immutable_update
BEFORE UPDATE ON aircraft_models
WHEN EXISTS (
  SELECT 1
  FROM aircraft_model_variants variant
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id = variant.id
  WHERE variant.aircraft_model_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft models are immutable'); END;
CREATE TRIGGER IF NOT EXISTS projected_aircraft_model_immutable_delete
BEFORE DELETE ON aircraft_models
WHEN EXISTS (
  SELECT 1
  FROM aircraft_model_variants variant
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id = variant.id
  WHERE variant.aircraft_model_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft models are immutable'); END;

CREATE TRIGGER IF NOT EXISTS projected_aircraft_manufacturer_immutable_update
BEFORE UPDATE ON aircraft_manufacturers
WHEN EXISTS (
  SELECT 1
  FROM aircraft_models model
  JOIN aircraft_model_variants variant ON variant.aircraft_model_id = model.id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id = variant.id
  WHERE model.aircraft_manufacturer_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft manufacturers are immutable'); END;
CREATE TRIGGER IF NOT EXISTS projected_aircraft_manufacturer_immutable_delete
BEFORE DELETE ON aircraft_manufacturers
WHEN EXISTS (
  SELECT 1
  FROM aircraft_models model
  JOIN aircraft_model_variants variant ON variant.aircraft_model_id = model.id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id = variant.id
  WHERE model.aircraft_manufacturer_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft manufacturers are immutable'); END;

CREATE TRIGGER IF NOT EXISTS compatibility_projected_make_immutable_update
BEFORE UPDATE ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_make_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft makes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_make_immutable_delete
BEFORE DELETE ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_make_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft makes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_family_immutable_update
BEFORE UPDATE ON aircraft_model_families
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_family_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft families are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_family_immutable_delete
BEFORE DELETE ON aircraft_model_families
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_family_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft families are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_designation_immutable_update
BEFORE UPDATE ON aircraft_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_designation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft designations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_designation_immutable_delete
BEFORE DELETE ON aircraft_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_designation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft designations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_generation_immutable_update
BEFORE UPDATE ON aircraft_generations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_generation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected aircraft generations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_generation_immutable_delete
BEFORE DELETE ON aircraft_generations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_generation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected aircraft generations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_package_immutable_update
BEFORE UPDATE ON aircraft_factory_packages
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_factory_package_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected aircraft packages are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_package_immutable_delete
BEFORE DELETE ON aircraft_factory_packages
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_factory_package_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected aircraft packages are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_generation_link_immutable_update
BEFORE UPDATE ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_generation_id = OLD.aircraft_generation_id
    AND projection.aircraft_designation_id = OLD.aircraft_designation_id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected generation applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_generation_link_immutable_delete
BEFORE DELETE ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_generation_id = OLD.aircraft_generation_id
    AND projection.aircraft_designation_id = OLD.aircraft_designation_id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected generation applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_package_link_immutable_update
BEFORE UPDATE ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_factory_package_id =
        OLD.aircraft_factory_package_id
    AND projection.aircraft_designation_id = OLD.aircraft_designation_id
    AND (
      OLD.aircraft_generation_id IS NULL
      OR projection.aircraft_generation_id = OLD.aircraft_generation_id
    )
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected package applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_package_link_immutable_delete
BEFORE DELETE ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_factory_package_id =
        OLD.aircraft_factory_package_id
    AND projection.aircraft_designation_id = OLD.aircraft_designation_id
    AND (
      OLD.aircraft_generation_id IS NULL
      OR projection.aircraft_generation_id = OLD.aircraft_generation_id
    )
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected package applicability is immutable'); END;

-- An assigned listing can change its compatibility FK only while an exact
-- transition is active. Routine updates retain the existing projected FK.
CREATE TRIGGER IF NOT EXISTS listing_aircraft_projection_transition_update
BEFORE UPDATE OF aircraft_model_variant_id ON aircraft_sale_listings
WHEN NEW.aircraft_model_variant_id <> OLD.aircraft_model_variant_id
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_valuation_projection_transitions transition
    JOIN aircraft_sale_listing_identity_assignments assignment
      ON assignment.id = transition.identity_assignment_id
     AND assignment.aircraft_sale_listing_id =
           transition.aircraft_sale_listing_id
    JOIN aircraft_valuation_compatibility_projections projection
      ON projection.aircraft_make_id = assignment.aircraft_make_id
     AND projection.aircraft_model_family_id =
           assignment.aircraft_model_family_id
     AND projection.aircraft_designation_id =
           assignment.aircraft_designation_id
     AND projection.aircraft_generation_id IS
           assignment.aircraft_generation_id
     AND projection.aircraft_factory_package_id IS
           assignment.aircraft_factory_package_id
    WHERE transition.aircraft_sale_listing_id = NEW.id
      AND projection.aircraft_model_variant_id =
            NEW.aircraft_model_variant_id
  )
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft compatibility FK may change only through an exact guarded transition');
END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_projection_insert
BEFORE INSERT ON aircraft_sale_listing_current_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN aircraft_valuation_projection_transitions transition
    ON transition.aircraft_sale_listing_id = listing.id
   AND transition.identity_assignment_id = NEW.identity_assignment_id
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = NEW.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id =
         listing.aircraft_model_variant_id
   AND projection.aircraft_make_id = assignment.aircraft_make_id
   AND projection.aircraft_model_family_id =
         assignment.aircraft_model_family_id
   AND projection.aircraft_designation_id =
         assignment.aircraft_designation_id
   AND projection.aircraft_generation_id IS
         assignment.aircraft_generation_id
   AND projection.aircraft_factory_package_id IS
         assignment.aircraft_factory_package_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
)
BEGIN
  SELECT RAISE(ABORT, 'current aircraft identity requires the exact guarded listing projection');
END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_projection_update
BEFORE UPDATE OF identity_assignment_id
ON aircraft_sale_listing_current_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN aircraft_valuation_projection_transitions transition
    ON transition.aircraft_sale_listing_id = listing.id
   AND transition.identity_assignment_id = NEW.identity_assignment_id
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = NEW.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id =
         listing.aircraft_model_variant_id
   AND projection.aircraft_make_id = assignment.aircraft_make_id
   AND projection.aircraft_model_family_id =
         assignment.aircraft_model_family_id
   AND projection.aircraft_designation_id =
         assignment.aircraft_designation_id
   AND projection.aircraft_generation_id IS
         assignment.aircraft_generation_id
   AND projection.aircraft_factory_package_id IS
         assignment.aircraft_factory_package_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
)
BEGIN
  SELECT RAISE(ABORT, 'current aircraft identity requires the exact guarded listing projection');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_transition_validate_delete
BEFORE DELETE ON aircraft_valuation_projection_transitions
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN aircraft_sale_listing_current_identity_assignments current_assignment
    ON current_assignment.aircraft_sale_listing_id = listing.id
   AND current_assignment.identity_assignment_id = OLD.identity_assignment_id
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = listing.id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id =
         listing.aircraft_model_variant_id
   AND projection.aircraft_make_id = assignment.aircraft_make_id
   AND projection.aircraft_model_family_id =
         assignment.aircraft_model_family_id
   AND projection.aircraft_designation_id =
         assignment.aircraft_designation_id
   AND projection.aircraft_generation_id IS
         assignment.aircraft_generation_id
   AND projection.aircraft_factory_package_id IS
         assignment.aircraft_factory_package_id
  WHERE listing.id = OLD.aircraft_sale_listing_id
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft projection transition cannot close before exact pointer and listing projection');
END;

CREATE VIEW IF NOT EXISTS aircraft_sale_listing_exact_compatibility_projections AS
SELECT
  listing.id AS listing_id,
  current_assignment.identity_assignment_id,
  listing.aircraft_model_variant_id,
  assignment.aircraft_make_id,
  assignment.aircraft_model_family_id,
  assignment.aircraft_designation_id,
  assignment.aircraft_generation_id,
  assignment.aircraft_factory_package_id
FROM aircraft_sale_listings listing
JOIN aircraft_sale_listing_current_identity_assignments current_assignment
  ON current_assignment.aircraft_sale_listing_id = listing.id
JOIN aircraft_sale_listing_identity_assignments assignment
  ON assignment.id = current_assignment.identity_assignment_id
 AND assignment.aircraft_sale_listing_id = listing.id
JOIN aircraft_valuation_compatibility_projections projection
  ON projection.aircraft_model_variant_id =
       listing.aircraft_model_variant_id
 AND projection.aircraft_make_id = assignment.aircraft_make_id
 AND projection.aircraft_model_family_id =
       assignment.aircraft_model_family_id
 AND projection.aircraft_designation_id =
       assignment.aircraft_designation_id
 AND projection.aircraft_generation_id IS assignment.aircraft_generation_id
 AND projection.aircraft_factory_package_id IS
       assignment.aircraft_factory_package_id;

UPDATE aircraft_sale_listings
SET ingestion_state = 'quarantined',
    ingestion_error =
      'aircraft compatibility projection migration: ready listing has no exact canonical projection',
    ingestion_completed_at = NULL,
    is_verified = 0,
    updated_at = CURRENT_TIMESTAMP
WHERE ingestion_state = 'ready'
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_exact_compatibility_projections exact_projection
    WHERE exact_projection.listing_id = aircraft_sale_listings.id
  );

CREATE TRIGGER IF NOT EXISTS listing_ready_requires_aircraft_projection
BEFORE UPDATE OF ingestion_state, aircraft_model_variant_id
ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_current_identity_assignments current_assignment
    JOIN aircraft_sale_listing_identity_assignments assignment
      ON assignment.id = current_assignment.identity_assignment_id
     AND assignment.aircraft_sale_listing_id =
           current_assignment.aircraft_sale_listing_id
    JOIN aircraft_valuation_compatibility_projections projection
      ON projection.aircraft_model_variant_id =
           NEW.aircraft_model_variant_id
     AND projection.aircraft_make_id = assignment.aircraft_make_id
     AND projection.aircraft_model_family_id =
           assignment.aircraft_model_family_id
     AND projection.aircraft_designation_id =
           assignment.aircraft_designation_id
     AND projection.aircraft_generation_id IS
           assignment.aircraft_generation_id
     AND projection.aircraft_factory_package_id IS
           assignment.aircraft_factory_package_id
    WHERE current_assignment.aircraft_sale_listing_id = NEW.id
  )
BEGIN
  SELECT RAISE(ABORT, 'ready listing requires its exact canonical aircraft compatibility projection');
END;

CREATE TRIGGER IF NOT EXISTS listing_ready_insert_requires_aircraft_projection
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_exact_compatibility_projections exact_projection
    WHERE exact_projection.listing_id = NEW.id
      AND exact_projection.aircraft_model_variant_id =
            NEW.aircraft_model_variant_id
  )
BEGIN
  SELECT RAISE(ABORT, 'ready listing must first persist its exact canonical aircraft compatibility projection');
END;

CREATE TRIGGER IF NOT EXISTS listing_ready_rejects_pending_aircraft_placeholder
BEFORE UPDATE OF ingestion_state, aircraft_model_variant_id
ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
  AND NEW.aircraft_model_variant_id = (
    SELECT aircraft_model_variant_id
    FROM aircraft_sale_listing_pending_compatibility_placeholder
    WHERE singleton_id = 1
  )
BEGIN
  SELECT RAISE(ABORT, 'pending aircraft compatibility placeholder cannot become ready');
END;

-- Existing valuation rows may be inserted for a projected variant, but an
-- UPDATE cannot silently move evidence into or out of a canonical projection.
CREATE TRIGGER IF NOT EXISTS projected_aircraft_spec_variant_move
BEFORE UPDATE OF aircraft_model_variant_id ON aircraft_model_spec_versions
WHEN NEW.aircraft_model_variant_id <> OLD.aircraft_model_variant_id
 AND (
   EXISTS (SELECT 1 FROM aircraft_valuation_compatibility_projections
           WHERE aircraft_model_variant_id = OLD.aircraft_model_variant_id)
   OR EXISTS (SELECT 1 FROM aircraft_valuation_compatibility_projections
              WHERE aircraft_model_variant_id = NEW.aircraft_model_variant_id)
 )
BEGIN SELECT RAISE(ABORT, 'aircraft spec evidence cannot move into or out of a projected variant'); END;

CREATE TRIGGER IF NOT EXISTS projected_aircraft_price_variant_move
BEFORE UPDATE OF aircraft_model_variant_id
ON aircraft_model_variant_price_points
WHEN NEW.aircraft_model_variant_id <> OLD.aircraft_model_variant_id
 AND (
   EXISTS (SELECT 1 FROM aircraft_valuation_compatibility_projections
           WHERE aircraft_model_variant_id = OLD.aircraft_model_variant_id)
   OR EXISTS (SELECT 1 FROM aircraft_valuation_compatibility_projections
              WHERE aircraft_model_variant_id = NEW.aircraft_model_variant_id)
 )
BEGIN SELECT RAISE(ABORT, 'aircraft price evidence cannot move into or out of a projected variant'); END;

CREATE TRIGGER IF NOT EXISTS projected_aircraft_default_avionics_variant_move
BEFORE UPDATE OF aircraft_model_variant_id
ON aircraft_model_variant_default_avionics
WHEN NEW.aircraft_model_variant_id <> OLD.aircraft_model_variant_id
 AND (
   EXISTS (SELECT 1 FROM aircraft_valuation_compatibility_projections
           WHERE aircraft_model_variant_id = OLD.aircraft_model_variant_id)
   OR EXISTS (SELECT 1 FROM aircraft_valuation_compatibility_projections
              WHERE aircraft_model_variant_id = NEW.aircraft_model_variant_id)
 )
BEGIN SELECT RAISE(ABORT, 'default avionics evidence cannot move into or out of a projected variant'); END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260726_listing_aircraft_compatibility_projection',
  2,
  '0a182d5972d62be3d906395df8d08b741bc3e23d713badf7596b360048aa45ba',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
