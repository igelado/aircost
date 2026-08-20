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

CREATE TEMP TABLE listing_aircraft_identity_migration_contract_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO listing_aircraft_identity_migration_contract_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260725_listing_aircraft_identity'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260725_listing_aircraft_identity'
      AND contract_version = 2
      AND contract_fingerprint =
        '63fb5b5213fc9eb2b7b4dcb2b0be3a9f22a80d4acae49f64e68ec1302c1437be'
  ) THEN 1
  ELSE 0
END;
DROP TABLE listing_aircraft_identity_migration_contract_guard;

-- Immutable assignment versions retain every approved correction. The small
-- current-pointer table is the only mutable state.
-- N-registered listings are evaluated in the United States market. Aliases
-- scoped to other markets are not identity evidence for this pipeline.
INSERT INTO aircraft_markets (code, name, parent_market_id)
SELECT 'US', 'United States', id
FROM aircraft_markets
WHERE code = 'GLOBAL'
ON CONFLICT (code) DO NOTHING;

CREATE TABLE IF NOT EXISTS aircraft_designation_faa_bindings (
  faa_snapshot_date TEXT NOT NULL,
  faa_archive_sha256 TEXT NOT NULL,
  faa_aircraft_code TEXT NOT NULL,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE RESTRICT,
  representative_faa_registry_snapshot_id INTEGER NOT NULL,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (representative_faa_registry_snapshot_id, faa_aircraft_code)
    REFERENCES faa_registry_aircraft_references(snapshot_id, aircraft_code)
    ON DELETE RESTRICT,
  CHECK (faa_snapshot_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  CHECK (length(faa_archive_sha256) = 64 AND faa_archive_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(trim(faa_aircraft_code)) > 0),
  PRIMARY KEY (faa_snapshot_date, faa_archive_sha256, faa_aircraft_code)
);

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_identity_assignments (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  supersedes_assignment_id INTEGER UNIQUE,
  aircraft_make_id INTEGER NOT NULL,
  aircraft_model_family_id INTEGER NOT NULL,
  aircraft_designation_id INTEGER NOT NULL,
  aircraft_generation_id INTEGER,
  aircraft_factory_package_id INTEGER,
  identity_decision_id INTEGER NOT NULL
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  faa_registry_snapshot_id INTEGER NOT NULL
    REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  faa_n_number TEXT NOT NULL,
  faa_source_record_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (id, aircraft_sale_listing_id),
  FOREIGN KEY (supersedes_assignment_id, aircraft_sale_listing_id)
    REFERENCES aircraft_sale_listing_identity_assignments(id, aircraft_sale_listing_id)
    ON DELETE CASCADE,
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
  CHECK (substr(faa_n_number, 1, 1) = 'N' AND length(faa_n_number) BETWEEN 2 AND 6),
  CHECK (
    length(faa_source_record_sha256) = 64
    AND faa_source_record_sha256 NOT GLOB '*[^0-9a-f]*'
  )
);

-- SQLite cannot alter a foreign-key action in place. Refuse to stamp the v2
-- contract over a draft v1 table whose successor chain still uses RESTRICT;
-- that database must be rebuilt or migrated with an explicit table-copy
-- procedure that preserves every immutable assignment.
CREATE TEMP TABLE listing_aircraft_identity_v2_fk_guard (
  valid INTEGER NOT NULL,
  CONSTRAINT listing_aircraft_identity_v2_requires_self_fk_on_delete_cascade
    CHECK (valid = 1)
);
INSERT INTO listing_aircraft_identity_v2_fk_guard (valid)
SELECT CASE WHEN EXISTS (
  SELECT foreign_key.id
  FROM pragma_foreign_key_list(
    'aircraft_sale_listing_identity_assignments'
  ) foreign_key
  WHERE foreign_key."table" = 'aircraft_sale_listing_identity_assignments'
  GROUP BY foreign_key.id
  HAVING count(*) = 2
    AND sum(
      foreign_key."from" = 'supersedes_assignment_id'
      AND foreign_key."to" = 'id'
    ) = 1
    AND sum(
      foreign_key."from" = 'aircraft_sale_listing_id'
      AND foreign_key."to" = 'aircraft_sale_listing_id'
    ) = 1
    AND min(upper(foreign_key.on_delete)) = 'CASCADE'
    AND max(upper(foreign_key.on_delete)) = 'CASCADE'
) THEN 1 ELSE 0 END;
DROP TABLE listing_aircraft_identity_v2_fk_guard;

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_current_identity_assignments (
  aircraft_sale_listing_id INTEGER PRIMARY KEY
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  identity_assignment_id INTEGER NOT NULL UNIQUE,
  selected_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (identity_assignment_id, aircraft_sale_listing_id)
    REFERENCES aircraft_sale_listing_identity_assignments(id, aircraft_sale_listing_id)
    ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_listing_identity_assignment_designation
  ON aircraft_sale_listing_identity_assignments (
    aircraft_designation_id, aircraft_generation_id, aircraft_factory_package_id
  );

-- SQLite has no built-in regular-expression replacement. These read-only
-- projections implement the same ASCII-alphanumeric identity key used by Rust
-- and PostgreSQL, so every punctuation character is ignored consistently.
CREATE VIEW IF NOT EXISTS aircraft_designation_identity_keys AS
WITH RECURSIVE designation_characters (
  aircraft_designation_id, source_value, character_position, identity_key
) AS (
  SELECT id, normalized_official_designation, 1, ''
  FROM aircraft_designations
  UNION ALL
  SELECT aircraft_designation_id, source_value, character_position + 1,
    identity_key || CASE
      WHEN lower(substr(source_value, character_position, 1)) GLOB '[a-z0-9]'
      THEN lower(substr(source_value, character_position, 1))
      ELSE ''
    END
  FROM designation_characters
  WHERE character_position <= length(source_value)
)
SELECT aircraft_designation_id, identity_key
FROM designation_characters
WHERE character_position > length(source_value);

CREATE VIEW IF NOT EXISTS faa_registry_aircraft_reference_identity_keys AS
WITH RECURSIVE reference_characters (
  faa_registry_snapshot_id, faa_aircraft_code, source_value,
  character_position, identity_key
) AS (
  SELECT snapshot_id, aircraft_code, coalesce(model_name, ''), 1, ''
  FROM faa_registry_aircraft_references
  UNION ALL
  SELECT faa_registry_snapshot_id, faa_aircraft_code, source_value,
    character_position + 1,
    identity_key || CASE
      WHEN lower(substr(source_value, character_position, 1)) GLOB '[a-z0-9]'
      THEN lower(substr(source_value, character_position, 1))
      ELSE ''
    END
  FROM reference_characters
  WHERE character_position <= length(source_value)
)
SELECT faa_registry_snapshot_id, faa_aircraft_code, identity_key
FROM reference_characters
WHERE character_position > length(source_value);

-- Alias keys are retrieval keys, not free-form evidence. Keep their stored
-- form deterministic, prevent overlapping scopes from resolving one FAA label
-- to two makes, and preserve approved aliases immutably.
CREATE TRIGGER IF NOT EXISTS aircraft_make_alias_identity_key_validate
BEFORE INSERT ON aircraft_make_aliases
WHEN NEW.normalized_alias = ''
  OR NEW.normalized_alias <> trim(NEW.normalized_alias)
  OR NEW.normalized_alias <> lower(NEW.normalized_alias)
  OR NEW.normalized_alias GLOB '*[^a-z0-9 ]*'
  OR instr(NEW.normalized_alias, '  ') > 0
  OR replace(NEW.normalized_alias, ' ', '') <>
     lower(replace(replace(replace(replace(replace(replace(replace(replace(
       replace(replace(trim(NEW.alias), ' ', ''), '-', ''), '.', ''), '/', ''),
       '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
BEGIN
  SELECT RAISE(ABORT, 'aircraft make alias requires its deterministic normalized retrieval key');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_make_alias_identity_collision
BEFORE INSERT ON aircraft_make_aliases
WHEN EXISTS (
  SELECT 1
  FROM aircraft_make_aliases existing_alias
  LEFT JOIN aircraft_markets existing_market
    ON existing_market.id = existing_alias.aircraft_market_id
  LEFT JOIN aircraft_markets new_market
    ON new_market.id = NEW.aircraft_market_id
  WHERE existing_alias.aircraft_make_id <> NEW.aircraft_make_id
    AND existing_alias.normalized_alias = NEW.normalized_alias
    AND (existing_alias.valid_to_model_year IS NULL
      OR NEW.valid_from_model_year IS NULL
      OR existing_alias.valid_to_model_year >= NEW.valid_from_model_year)
    AND (NEW.valid_to_model_year IS NULL
      OR existing_alias.valid_from_model_year IS NULL
      OR NEW.valid_to_model_year >= existing_alias.valid_from_model_year)
    AND (existing_alias.aircraft_market_id IS NULL
      OR NEW.aircraft_market_id IS NULL
      OR existing_alias.aircraft_market_id = NEW.aircraft_market_id
      OR existing_market.code = 'GLOBAL'
      OR new_market.code = 'GLOBAL')
)
OR EXISTS (
  SELECT 1 FROM aircraft_makes other_make
  WHERE other_make.id <> NEW.aircraft_make_id
    AND (
      other_make.normalized_name = NEW.normalized_alias
      OR lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(other_make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = replace(NEW.normalized_alias, ' ', '')
    )
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft make alias overlaps another canonical make in market/year scope');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_make_alias_identity_immutable_update
BEFORE UPDATE ON aircraft_make_aliases
BEGIN SELECT RAISE(ABORT, 'approved aircraft make aliases are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_make_alias_identity_immutable_delete
BEFORE DELETE ON aircraft_make_aliases
BEGIN SELECT RAISE(ABORT, 'approved aircraft make aliases are immutable'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_make_identity_alias_collision_insert
BEFORE INSERT ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_make_aliases alias
  WHERE lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(NEW.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
    = replace(alias.normalized_alias, ' ', '')
)
BEGIN SELECT RAISE(ABORT, 'canonical aircraft make collides with an approved alias'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_make_identity_alias_collision_update
BEFORE UPDATE OF name, normalized_name ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_make_aliases alias
  WHERE alias.aircraft_make_id <> OLD.id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(NEW.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      = replace(alias.normalized_alias, ' ', '')
)
BEGIN SELECT RAISE(ABORT, 'canonical aircraft make collides with an approved alias'); END;

-- Fail the upgrade instead of grandfathering ambiguous or mechanically
-- inconsistent aliases that could authorize the wrong FAA manufacturer.
CREATE TABLE IF NOT EXISTS aircraft_identity_alias_upgrade_guard (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
DELETE FROM aircraft_identity_alias_upgrade_guard;
INSERT INTO aircraft_identity_alias_upgrade_guard (valid)
SELECT 0
WHERE EXISTS (
  SELECT 1
  FROM aircraft_make_aliases alias
  WHERE alias.normalized_alias = ''
    OR alias.normalized_alias <> trim(alias.normalized_alias)
    OR alias.normalized_alias <> lower(alias.normalized_alias)
    OR alias.normalized_alias GLOB '*[^a-z0-9 ]*'
    OR instr(alias.normalized_alias, '  ') > 0
    OR replace(alias.normalized_alias, ' ', '') <>
       lower(replace(replace(replace(replace(replace(replace(replace(replace(
         replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/', ''),
         '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
)
OR EXISTS (
  SELECT 1
  FROM aircraft_make_aliases left_alias
  JOIN aircraft_make_aliases right_alias
    ON right_alias.id > left_alias.id
   AND right_alias.aircraft_make_id <> left_alias.aircraft_make_id
   AND right_alias.normalized_alias = left_alias.normalized_alias
  LEFT JOIN aircraft_markets left_market
    ON left_market.id = left_alias.aircraft_market_id
  LEFT JOIN aircraft_markets right_market
    ON right_market.id = right_alias.aircraft_market_id
  WHERE (left_alias.valid_to_model_year IS NULL
      OR right_alias.valid_from_model_year IS NULL
      OR left_alias.valid_to_model_year >= right_alias.valid_from_model_year)
    AND (right_alias.valid_to_model_year IS NULL
      OR left_alias.valid_from_model_year IS NULL
      OR right_alias.valid_to_model_year >= left_alias.valid_from_model_year)
    AND (left_alias.aircraft_market_id IS NULL
      OR right_alias.aircraft_market_id IS NULL
      OR left_alias.aircraft_market_id = right_alias.aircraft_market_id
      OR left_market.code = 'GLOBAL'
      OR right_market.code = 'GLOBAL')
)
OR EXISTS (
  SELECT 1
  FROM aircraft_make_aliases alias
  JOIN aircraft_makes other_make
    ON other_make.id <> alias.aircraft_make_id
  WHERE lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(other_make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
    = replace(alias.normalized_alias, ' ', '')
);
DROP TABLE aircraft_identity_alias_upgrade_guard;

CREATE TRIGGER IF NOT EXISTS aircraft_designation_faa_binding_requires_provenance
BEFORE INSERT ON aircraft_designation_faa_bindings
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_designations designation
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN aircraft_model_families family
    ON family.id = designation.aircraft_model_family_id
  JOIN aircraft_makes make
    ON make.id = family.aircraft_make_id
  JOIN aircraft_identity_decisions decision
    ON decision.id = designation.approval_decision_id
  JOIN curation_evidence_claims claim
    ON claim.id = NEW.identity_evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = NEW.representative_faa_registry_snapshot_id
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = snapshot.id
   AND reference.aircraft_code = NEW.faa_aircraft_code
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  WHERE designation.id = NEW.aircraft_designation_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'designation'
    AND claim.claim_kind = 'identity'
    AND claim.validation_status = 'validated'
    AND source.id = snapshot.evidence_source_id
    AND source.source_tier = 'regulator_primary'
    AND NEW.faa_snapshot_date = snapshot.snapshot_date
    AND NEW.faa_archive_sha256 = snapshot.archive_sha256
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      OR (
        EXISTS (
          SELECT 1 FROM faa_registry_aircraft registered_aircraft
          WHERE registered_aircraft.snapshot_id = snapshot.id
            AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
        )
        AND NOT EXISTS (
          SELECT 1
          FROM faa_registry_aircraft registered_aircraft
          WHERE registered_aircraft.snapshot_id = snapshot.id
            AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
            AND NOT EXISTS (
              SELECT 1
              FROM aircraft_make_aliases alias
              LEFT JOIN aircraft_markets market
                ON market.id = alias.aircraft_market_id
              WHERE alias.aircraft_make_id = make.id
                AND lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
                  = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
                AND (alias.aircraft_market_id IS NULL OR market.code IN ('GLOBAL', 'US'))
                AND (
                  (registered_aircraft.year_manufactured IS NULL
                    AND alias.valid_from_model_year IS NULL
                    AND alias.valid_to_model_year IS NULL)
                  OR (registered_aircraft.year_manufactured IS NOT NULL
                    AND (alias.valid_from_model_year IS NULL
                      OR alias.valid_from_model_year <= registered_aircraft.year_manufactured)
                    AND (alias.valid_to_model_year IS NULL
                      OR alias.valid_to_model_year >= registered_aircraft.year_manufactured))
                )
            )
        )
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'FAA aircraft code binding requires an exact approved designation, applicable manufacturer identity, and regulator evidence');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_designation_faa_binding_immutable_update
BEFORE UPDATE ON aircraft_designation_faa_bindings
BEGIN SELECT RAISE(ABORT, 'FAA aircraft code bindings are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_designation_faa_binding_immutable_delete
BEFORE DELETE ON aircraft_designation_faa_bindings
BEGIN SELECT RAISE(ABORT, 'FAA aircraft code bindings are immutable'); END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_requires_provenance
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_designations designation
  JOIN aircraft_identity_decisions decision
    ON decision.id = designation.approval_decision_id
  JOIN aircraft_identity_decision_claims decision_claim
    ON decision_claim.decision_id = decision.id
  JOIN curation_evidence_claims decision_evidence
    ON decision_evidence.id = decision_claim.evidence_claim_id
  JOIN curation_evidence_sources decision_source
    ON decision_source.id = decision_evidence.evidence_source_id
  JOIN curation_evidence_claims assignment_evidence
    ON assignment_evidence.id = NEW.identity_evidence_claim_id
  JOIN curation_evidence_sources assignment_source
    ON assignment_source.id = assignment_evidence.evidence_source_id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = NEW.faa_registry_snapshot_id
  WHERE designation.id = NEW.aircraft_designation_id
    AND decision.id = NEW.identity_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'designation'
    AND decision_claim.evidence_role = 'identity'
    AND decision_evidence.validation_status = 'validated'
    AND decision_source.source_tier IN ('manufacturer_primary', 'regulator_primary')
    AND assignment_evidence.claim_kind = 'identity'
    AND assignment_evidence.validation_status = 'validated'
    AND assignment_source.id = snapshot.evidence_source_id
    AND assignment_source.source_tier = 'regulator_primary'
)
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft assignment requires immutable designation-decision and current FAA evidence provenance');
END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_requires_faa_identity
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN faa_registry_aircraft aircraft
    ON aircraft.snapshot_id = NEW.faa_registry_snapshot_id
   AND aircraft.n_number = NEW.faa_n_number
   AND aircraft.source_record_sha256 = NEW.faa_source_record_sha256
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = aircraft.snapshot_id
   AND reference.aircraft_code = aircraft.aircraft_code
  JOIN faa_registry_snapshots registry_snapshot
    ON registry_snapshot.id = aircraft.snapshot_id
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN aircraft_designation_faa_bindings faa_binding
    ON faa_binding.faa_snapshot_date = registry_snapshot.snapshot_date
   AND faa_binding.faa_archive_sha256 = registry_snapshot.archive_sha256
   AND faa_binding.faa_aircraft_code = aircraft.aircraft_code
   AND faa_binding.aircraft_designation_id = designation.id
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  JOIN aircraft_makes make
    ON make.id = NEW.aircraft_make_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
    AND upper(replace(replace(trim(listing.registration_number), '-', ''), ' ', ''))
      = NEW.faa_n_number
    AND length(trim(reference.manufacturer_name)) > 0
    AND length(trim(reference.model_name)) > 0
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      OR EXISTS (
        SELECT 1
        FROM aircraft_make_aliases alias
        LEFT JOIN aircraft_markets market
          ON market.id = alias.aircraft_market_id
        WHERE alias.aircraft_make_id = make.id
          AND lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
            = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
          AND (alias.aircraft_market_id IS NULL OR market.code IN ('GLOBAL', 'US'))
          AND (alias.valid_from_model_year IS NULL
            OR alias.valid_from_model_year <= listing.model_year)
          AND (alias.valid_to_model_year IS NULL
            OR alias.valid_to_model_year >= listing.model_year)
      )
    )
    AND designation_key.identity_key = reference_key.identity_key
)
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft assignment designation does not match the exact FAA aircraft identity');
END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_requires_linear_history
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN (NEW.supersedes_assignment_id IS NULL AND EXISTS (
        SELECT 1 FROM aircraft_sale_listing_identity_assignments prior
        WHERE prior.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
      ))
  OR (NEW.supersedes_assignment_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM aircraft_sale_listing_current_identity_assignments current_assignment
        WHERE current_assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
          AND current_assignment.identity_assignment_id = NEW.supersedes_assignment_id
      ))
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft assignment must extend the current immutable history');
END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_requires_applicable_dimensions
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN (NEW.aircraft_generation_id IS NOT NULL AND NOT EXISTS (
        SELECT 1
        FROM aircraft_generation_designations link
        WHERE link.aircraft_generation_id = NEW.aircraft_generation_id
          AND link.aircraft_designation_id = NEW.aircraft_designation_id
      ))
  OR (NEW.aircraft_factory_package_id IS NOT NULL AND NOT EXISTS (
        SELECT 1
        FROM aircraft_sale_listings listing
        JOIN aircraft_package_applicability applicability
          ON applicability.aircraft_factory_package_id = NEW.aircraft_factory_package_id
         AND applicability.aircraft_designation_id = NEW.aircraft_designation_id
        WHERE listing.id = NEW.aircraft_sale_listing_id
          AND (
            (NEW.aircraft_generation_id IS NULL
              AND applicability.aircraft_generation_id IS NULL)
            OR applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id = NEW.aircraft_generation_id
          )
          AND (applicability.valid_from_model_year IS NULL
            OR applicability.valid_from_model_year <= listing.model_year)
          AND (applicability.valid_to_model_year IS NULL
            OR applicability.valid_to_model_year >= listing.model_year)
      ))
  OR (NEW.aircraft_generation_id IS NULL AND EXISTS (
        SELECT 1
        FROM aircraft_generation_designations link
        WHERE link.aircraft_designation_id = NEW.aircraft_designation_id
      ))
  OR (NEW.aircraft_factory_package_id IS NULL AND EXISTS (
        SELECT 1
        FROM aircraft_sale_listings listing
        JOIN aircraft_package_applicability applicability
          ON applicability.aircraft_designation_id = NEW.aircraft_designation_id
        JOIN aircraft_factory_packages package
          ON package.id = applicability.aircraft_factory_package_id
        WHERE listing.id = NEW.aircraft_sale_listing_id
          AND package.package_kind = 'trim_tier'
          AND (applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id = NEW.aircraft_generation_id)
          AND (applicability.valid_from_model_year IS NULL
            OR applicability.valid_from_model_year <= listing.model_year)
          AND (applicability.valid_to_model_year IS NULL
            OR applicability.valid_to_model_year >= listing.model_year)
      ))
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft assignment generation/package is not applicable to the designation and model year');
END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_identity_assignments
BEGIN SELECT RAISE(ABORT, 'listing aircraft identity assignment versions are immutable'); END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_immutable_delete
BEFORE DELETE ON aircraft_sale_listing_identity_assignments
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id = OLD.aircraft_sale_listing_id
)
BEGIN SELECT RAISE(ABORT, 'listing aircraft identity assignment versions are immutable'); END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_validate_insert
BEFORE INSERT ON aircraft_sale_listing_current_identity_assignments
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
    AND assignment.supersedes_assignment_id IS NULL
)
BEGIN SELECT RAISE(ABORT, 'initial current aircraft identity must select the listing root assignment'); END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_validate_update
BEFORE UPDATE ON aircraft_sale_listing_current_identity_assignments
WHEN NEW.aircraft_sale_listing_id <> OLD.aircraft_sale_listing_id
  OR NEW.selected_at <= OLD.selected_at
  OR NOT EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
    AND assignment.supersedes_assignment_id = OLD.identity_assignment_id
)
BEGIN SELECT RAISE(ABORT, 'current aircraft identity may advance only to its direct immutable successor'); END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_immutable_delete
BEFORE DELETE ON aircraft_sale_listing_current_identity_assignments
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id = OLD.aircraft_sale_listing_id
)
BEGIN SELECT RAISE(ABORT, 'current aircraft identity may be deleted only with its parent listing'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_make_immutable_update
BEFORE UPDATE ON aircraft_makes
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_make_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft makes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_make_immutable_delete
BEFORE DELETE ON aircraft_makes
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_make_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft makes are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_family_immutable_update
BEFORE UPDATE ON aircraft_model_families
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_model_family_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft model families are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_family_immutable_delete
BEFORE DELETE ON aircraft_model_families
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_model_family_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft model families are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_designation_immutable_update
BEFORE UPDATE ON aircraft_designations
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_designation_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft designations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_designation_immutable_delete
BEFORE DELETE ON aircraft_designations
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_designation_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft designations are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_generation_immutable_update
BEFORE UPDATE ON aircraft_generations
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_generation_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft generations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_generation_immutable_delete
BEFORE DELETE ON aircraft_generations
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_generation_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft generations are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_package_immutable_update
BEFORE UPDATE ON aircraft_factory_packages
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_factory_package_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft factory packages are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_package_immutable_delete
BEFORE DELETE ON aircraft_factory_packages
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_factory_package_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft factory packages are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_generation_designation_immutable_update
BEFORE UPDATE ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_generation_id = OLD.aircraft_generation_id
    AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
)
BEGIN SELECT RAISE(ABORT, 'assigned generation/designation applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_generation_dimension_requires_resolution
BEFORE INSERT ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_current_identity_assignments current_assignment
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = current_assignment.aircraft_sale_listing_id
  JOIN aircraft_sale_listings listing
    ON listing.id = current_assignment.aircraft_sale_listing_id
  WHERE listing.ingestion_state = 'ready'
    AND assignment.aircraft_designation_id = NEW.aircraft_designation_id
    AND assignment.aircraft_generation_id IS NULL
)
BEGIN SELECT RAISE(ABORT, 'adding a generation dimension requires resolving affected ready listing assignments first'); END;
CREATE TRIGGER IF NOT EXISTS assigned_generation_designation_immutable_delete
BEFORE DELETE ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_generation_id = OLD.aircraft_generation_id
    AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
)
BEGIN SELECT RAISE(ABORT, 'assigned generation/designation applicability is immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_package_applicability_immutable_update
BEFORE UPDATE ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_factory_package_id = OLD.aircraft_factory_package_id
    AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
    AND (OLD.aircraft_generation_id IS NULL
      OR assignment.aircraft_generation_id = OLD.aircraft_generation_id)
)
BEGIN SELECT RAISE(ABORT, 'assigned package applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_trim_tier_dimension_requires_resolution
BEFORE INSERT ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1
  FROM aircraft_factory_packages package
  CROSS JOIN aircraft_sale_listing_current_identity_assignments current_assignment
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = current_assignment.aircraft_sale_listing_id
  JOIN aircraft_sale_listings listing
    ON listing.id = current_assignment.aircraft_sale_listing_id
  WHERE package.id = NEW.aircraft_factory_package_id
    AND package.package_kind = 'trim_tier'
    AND listing.ingestion_state = 'ready'
    AND assignment.aircraft_designation_id = NEW.aircraft_designation_id
    AND assignment.aircraft_factory_package_id IS NULL
    AND (NEW.aircraft_generation_id IS NULL
      OR assignment.aircraft_generation_id = NEW.aircraft_generation_id)
    AND (NEW.valid_from_model_year IS NULL
      OR NEW.valid_from_model_year <= listing.model_year)
    AND (NEW.valid_to_model_year IS NULL
      OR NEW.valid_to_model_year >= listing.model_year)
)
BEGIN SELECT RAISE(ABORT, 'adding a trim-tier dimension requires resolving affected ready listing assignments first'); END;
CREATE TRIGGER IF NOT EXISTS assigned_package_applicability_immutable_delete
BEFORE DELETE ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_factory_package_id = OLD.aircraft_factory_package_id
    AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
    AND (OLD.aircraft_generation_id IS NULL
      OR assignment.aircraft_generation_id = OLD.aircraft_generation_id)
)
BEGIN SELECT RAISE(ABORT, 'assigned package applicability is immutable'); END;

-- Existing published rows predate this trust boundary. They cannot remain
-- grandfathered without a current evidence-backed assignment.
UPDATE aircraft_sale_listings
SET ingestion_state = 'quarantined',
    ingestion_error = 'canonical aircraft identity migration: ready listing has no current FAA-backed curated assignment',
    ingestion_completed_at = NULL,
    is_verified = 0,
    updated_at = CURRENT_TIMESTAMP
WHERE ingestion_state = 'ready'
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_current_identity_assignments current_assignment
    WHERE current_assignment.aircraft_sale_listing_id = aircraft_sale_listings.id
  );

CREATE TRIGGER IF NOT EXISTS listing_ready_requires_canonical_aircraft_insert
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
BEGIN SELECT RAISE(ABORT, 'listing cannot be inserted ready before canonical aircraft assignment'); END;

CREATE TRIGGER IF NOT EXISTS listing_ready_requires_canonical_aircraft_update
BEFORE UPDATE OF ingestion_state, aircraft_model_variant_id, model_year, registration_number, serial_number
ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready' AND NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_current_identity_assignments current_assignment
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = NEW.id
  JOIN aircraft_makes canonical_make ON canonical_make.id = assignment.aircraft_make_id
  JOIN aircraft_designations canonical_designation
    ON canonical_designation.id = assignment.aircraft_designation_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = canonical_designation.id
  JOIN faa_registry_snapshots snapshot ON snapshot.id = assignment.faa_registry_snapshot_id
  JOIN faa_registry_aircraft aircraft
    ON aircraft.snapshot_id = snapshot.id
   AND aircraft.n_number = assignment.faa_n_number
   AND aircraft.source_record_sha256 = assignment.faa_source_record_sha256
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = aircraft.snapshot_id
   AND reference.aircraft_code = aircraft.aircraft_code
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  JOIN aircraft_designation_faa_bindings faa_binding
    ON faa_binding.faa_snapshot_date = snapshot.snapshot_date
   AND faa_binding.faa_archive_sha256 = snapshot.archive_sha256
   AND faa_binding.faa_aircraft_code = aircraft.aircraft_code
   AND faa_binding.aircraft_designation_id = assignment.aircraft_designation_id
  WHERE current_assignment.aircraft_sale_listing_id = NEW.id
    AND EXISTS (
      SELECT 1
      FROM faa_registry_snapshots latest_release
      WHERE latest_release.id = (
        SELECT id FROM faa_registry_snapshots
        ORDER BY snapshot_date DESC, id DESC LIMIT 1
      )
        AND latest_release.snapshot_date = snapshot.snapshot_date
        AND latest_release.archive_sha256 = snapshot.archive_sha256
    )
    AND upper(replace(replace(trim(NEW.registration_number), '-', ''), ' ', '')) = assignment.faa_n_number
    AND (NEW.serial_number IS NULL OR trim(NEW.serial_number) = ''
      OR aircraft.manufacturer_serial_raw IS NULL
      OR upper(replace(replace(trim(NEW.serial_number), '-', ''), ' ', ''))
        = upper(replace(replace(trim(aircraft.manufacturer_serial_raw), '-', ''), ' ', '')))
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(canonical_make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      OR EXISTS (
        SELECT 1
        FROM aircraft_make_aliases alias
        LEFT JOIN aircraft_markets market
          ON market.id = alias.aircraft_market_id
        WHERE alias.aircraft_make_id = canonical_make.id
          AND lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
            = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
          AND (alias.aircraft_market_id IS NULL OR market.code IN ('GLOBAL', 'US'))
          AND (alias.valid_from_model_year IS NULL
            OR alias.valid_from_model_year <= NEW.model_year)
          AND (alias.valid_to_model_year IS NULL
            OR alias.valid_to_model_year >= NEW.model_year)
      )
    )
    AND (
      (assignment.aircraft_generation_id IS NULL AND NOT EXISTS (
        SELECT 1 FROM aircraft_generation_designations generation_link
        WHERE generation_link.aircraft_designation_id = assignment.aircraft_designation_id
      ))
      OR (assignment.aircraft_generation_id IS NOT NULL AND EXISTS (
        SELECT 1 FROM aircraft_generation_designations generation_link
        WHERE generation_link.aircraft_generation_id = assignment.aircraft_generation_id
          AND generation_link.aircraft_designation_id = assignment.aircraft_designation_id
      ))
    )
    AND (
      (assignment.aircraft_factory_package_id IS NULL AND NOT EXISTS (
        SELECT 1
        FROM aircraft_package_applicability applicability
        JOIN aircraft_factory_packages package
          ON package.id = applicability.aircraft_factory_package_id
        WHERE applicability.aircraft_designation_id = assignment.aircraft_designation_id
          AND package.package_kind = 'trim_tier'
          AND (applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id = assignment.aircraft_generation_id)
          AND (applicability.valid_from_model_year IS NULL
            OR applicability.valid_from_model_year <= NEW.model_year)
          AND (applicability.valid_to_model_year IS NULL
            OR applicability.valid_to_model_year >= NEW.model_year)
      ))
      OR (assignment.aircraft_factory_package_id IS NOT NULL AND EXISTS (
        SELECT 1
        FROM aircraft_package_applicability applicability
        WHERE applicability.aircraft_factory_package_id = assignment.aircraft_factory_package_id
          AND applicability.aircraft_designation_id = assignment.aircraft_designation_id
          AND (applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id = assignment.aircraft_generation_id)
          AND (applicability.valid_from_model_year IS NULL
            OR applicability.valid_from_model_year <= NEW.model_year)
          AND (applicability.valid_to_model_year IS NULL
            OR applicability.valid_to_model_year >= NEW.model_year)
      ))
    )
)
BEGIN SELECT RAISE(ABORT, 'ready listing requires a current canonical aircraft assignment matching current FAA identity'); END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260725_listing_aircraft_identity',
  2,
  '63fb5b5213fc9eb2b7b4dcb2b0be3a9f22a80d4acae49f64e68ec1302c1437be',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
PRAGMA foreign_key_check;
