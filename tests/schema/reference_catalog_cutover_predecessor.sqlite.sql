-- Exact owned-object predecessor for the 20260819 reference catalog
-- cutover. Tests apply this to a fresh canonical dependency graph before
-- inserting legacy rows. The migration itself attests that this reconstruction
-- is byte-for-byte the accepted predecessor; do not make it permissive.
PRAGMA foreign_keys = ON;

DELETE FROM schema_migration_contracts
WHERE migration_name = '20260819_reference_catalog_cutover';

DROP VIEW aircraft_reference_serial_key_errors;

DROP TRIGGER aircraft_reference_scope_canonical_insert;
DROP TRIGGER aircraft_reference_scope_key_recompute_insert;
DROP TRIGGER aircraft_serial_schemes_preserve_ordering;
DROP TRIGGER aircraft_reference_versions_publish;
DROP TRIGGER aircraft_reference_versions_require_approval;
DROP TRIGGER aircraft_serial_schemes_require_approval;
DROP TRIGGER aircraft_valuation_projection_validate_insert;
DROP TRIGGER avionics_models_referenced_status_update;

DROP TABLE aircraft_reference_fact_set_attestations;
DROP TABLE official_dollar_normalization_facts;
DROP TABLE listing_verification_run_items;
DROP TABLE aircraft_reference_prices;

CREATE TABLE depreciation_profiles (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE,
  age_decay_rate REAL NOT NULL,
  long_run_residual_fraction REAL NOT NULL,
  new_to_used_discount_fraction REAL NOT NULL,
  new_to_used_discount_years REAL NOT NULL,
  airframe_doubling_discount REAL NOT NULL,
  max_airframe_premium REAL NOT NULL,
  max_airframe_discount REAL NOT NULL,
  replacement_floor_fraction REAL NOT NULL DEFAULT 0,
  minimum_value_fraction REAL NOT NULL,
  high_time_threshold_hours REAL,
  high_time_discount_at_double_threshold REAL NOT NULL,
  is_system_profile INTEGER NOT NULL DEFAULT 0 CHECK (is_system_profile IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE depreciation_profile_fit_metadata (
  depreciation_profile_id INTEGER PRIMARY KEY
    REFERENCES depreciation_profiles(id) ON DELETE CASCADE,
  fit_scope TEXT NOT NULL CHECK (fit_scope IN ('global', 'category', 'model')),
  fit_scope_key TEXT NOT NULL,
  fit_category TEXT NOT NULL,
  sample_count INTEGER NOT NULL,
  rmse_usd REAL NOT NULL,
  mae_fraction REAL NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (fit_scope, fit_scope_key)
);
CREATE TABLE component_depreciation_profiles (
  component_type TEXT PRIMARY KEY
    CHECK (component_type IN ('engine', 'propeller', 'avionics')),
  age_decay_rate REAL,
  long_run_residual_fraction REAL,
  baseline_life_fraction REAL,
  sample_count INTEGER NOT NULL DEFAULT 0,
  rmse_usd REAL,
  mae_fraction REAL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE aircraft_model_spec_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_id INTEGER NOT NULL REFERENCES aircraft_models(id),
  aircraft_model_variant_id INTEGER NOT NULL REFERENCES aircraft_model_variants(id),
  effective_from TEXT NOT NULL,
  effective_to TEXT,
  depreciation_profile_id INTEGER REFERENCES depreciation_profiles(id),
  average_inflation_rate REAL NOT NULL DEFAULT 0.025,
  fuel_burn_gph REAL,
  oil_quarts_per_hour REAL,
  oil_price_per_quart_usd REAL,
  engine_model_id INTEGER REFERENCES engine_models(id),
  engine_count INTEGER NOT NULL DEFAULT 1,
  engine_tbo_hours REAL,
  engine_overhaul_cost_usd REAL,
  engine_value_baseline_life_fraction REAL NOT NULL DEFAULT 0.5,
  propeller_model_id INTEGER REFERENCES propeller_models(id),
  propeller_count INTEGER NOT NULL DEFAULT 1,
  propeller_tbo_hours REAL,
  propeller_overhaul_cost_usd REAL,
  propeller_value_baseline_life_fraction REAL NOT NULL DEFAULT 0.5,
  annual_inspection_usd REAL,
  other_maintenance_per_hour REAL,
  source_url TEXT,
  configuration_scope TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (configuration_scope IN ('factory_default', 'listing_specific', 'unreviewed')),
  source_confidence TEXT
    CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low')),
  evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1)),
  created_by_user_id INTEGER REFERENCES users(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (effective_to IS NULL OR effective_to > effective_from),
  CHECK (
    is_valuation_eligible = 0
    OR (
      configuration_scope = 'factory_default'
      AND source_confidence = 'high'
      AND evidence_kind = 'authoritative_reference'
      AND source_url IS NOT NULL
      AND length(trim(source_url)) > 0
    )
  )
);
CREATE TABLE aircraft_model_variant_price_points (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_variant_id INTEGER NOT NULL REFERENCES aircraft_model_variants(id),
  model_year INTEGER NOT NULL,
  purchase_price_new_usd REAL NOT NULL,
  purchase_price_reference_year INTEGER NOT NULL,
  source_url TEXT NOT NULL,
  source_title TEXT NOT NULL,
  source_notes TEXT NOT NULL,
  source_confidence TEXT NOT NULL,
  evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN (
      'direct_model_year', 'direct_other_year', 'interpolated', 'inferred', 'unreviewed'
    )),
  is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_variant_id, model_year),
  CHECK (
    is_valuation_eligible = 0
    OR (
      source_confidence = 'high'
      AND evidence_kind = 'direct_model_year'
      AND purchase_price_reference_year = model_year
    )
  )
);
CREATE TABLE aircraft_model_variant_default_avionics (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_variant_id INTEGER NOT NULL REFERENCES aircraft_model_variants(id),
  model_year INTEGER NOT NULL,
  avionics_model_id INTEGER NOT NULL REFERENCES avionics_models(id),
  quantity INTEGER NOT NULL DEFAULT 1,
  source_url TEXT NOT NULL,
  source_title TEXT NOT NULL,
  source_notes TEXT NOT NULL,
  source_confidence TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_variant_id, model_year, avionics_model_id)
);
CREATE TABLE aircraft_model_variant_default_avionics_candidates (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  quarantined_default_avionics_id INTEGER UNIQUE,
  aircraft_model_variant_id INTEGER NOT NULL
    REFERENCES aircraft_model_variants(id) ON DELETE RESTRICT,
  model_year INTEGER NOT NULL,
  avionics_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  quantity INTEGER NOT NULL,
  source_url TEXT NOT NULL,
  source_title TEXT NOT NULL,
  source_notes TEXT NOT NULL,
  source_confidence TEXT NOT NULL,
  pending_reason TEXT NOT NULL DEFAULT 'factory_default_claim_unverified',
  quarantined_created_at TEXT,
  quarantined_updated_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_variant_id, model_year, avionics_model_id),
  CHECK (pending_reason IN (
    'catalog_product_unverified', 'factory_default_claim_unverified'
  )),
  CHECK (
    (
      quarantined_default_avionics_id IS NULL
      AND quarantined_created_at IS NULL
      AND quarantined_updated_at IS NULL
    )
    OR (
      quarantined_default_avionics_id > 0
      AND quarantined_created_at IS NOT NULL
      AND quarantined_updated_at IS NOT NULL
    )
  )
);
CREATE TABLE listing_verification_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL
    REFERENCES listing_verification_runs(id) ON DELETE CASCADE,
  listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  status TEXT NOT NULL DEFAULT 'queued'
    CHECK (status IN (
      'queued', 'running', 'verified', 'pending_review',
      'pending_reference', 'blocked', 'failed', 'cancelled'
    )),
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  lease_token TEXT,
  lease_expires_at_epoch_seconds INTEGER,
  outcome_json TEXT,
  reason_code TEXT,
  reason TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  started_at TEXT,
  completed_at TEXT,
  UNIQUE (run_id, position),
  UNIQUE (run_id, listing_id),
  CHECK (lease_token IS NULL OR length(trim(lease_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = 'running'
      AND lease_token IS NOT NULL
      AND lease_expires_at_epoch_seconds IS NOT NULL
      AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status <> 'running'
      AND lease_token IS NULL
      AND lease_expires_at_epoch_seconds IS NULL)
  ),
  CHECK (
    (status IN ('queued', 'running') AND completed_at IS NULL)
    OR
    (status IN (
      'verified', 'pending_review', 'pending_reference',
      'blocked', 'failed', 'cancelled'
    ) AND completed_at IS NOT NULL)
  ),
  CHECK (
    outcome_json IS NULL
    OR (
      length(outcome_json) BETWEEN 2 AND 65536
      AND json_valid(outcome_json)
      AND json_type(outcome_json) = 'object'
    )
  ),
  CHECK (
    status NOT IN (
      'verified', 'pending_review', 'pending_reference', 'blocked'
    )
    OR outcome_json IS NOT NULL
  ),
  CHECK (reason_code IS NULL OR length(trim(reason_code)) BETWEEN 1 AND 100),
  CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 2000)
);
CREATE TABLE aircraft_reference_prices (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  price_kind TEXT NOT NULL CHECK (price_kind IN (
    'base_msrp', 'equipped_msrp', 'tier_increment', 'other_factory_price'
  )),
  amount REAL NOT NULL CHECK (amount > 0),
  currency TEXT NOT NULL CHECK (length(currency) = 3 AND currency = upper(currency)),
  price_reference_year INTEGER NOT NULL CHECK (price_reference_year BETWEEN 1900 AND 2200),
  evidence_kind TEXT NOT NULL CHECK (evidence_kind IN (
    'direct_model_year', 'direct_other_year', 'interpolated', 'inferred'
  )),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, price_kind, currency)
);
CREATE INDEX idx_depreciation_profile_fit_metadata_category
  ON depreciation_profile_fit_metadata (fit_category);
CREATE INDEX idx_aircraft_model_spec_versions_model
  ON aircraft_model_spec_versions (
    aircraft_model_id,
    aircraft_model_variant_id,
    effective_from
  );
CREATE INDEX idx_aircraft_model_variant_price_points_lookup
  ON aircraft_model_variant_price_points (aircraft_model_variant_id, model_year);
CREATE INDEX idx_aircraft_model_variant_default_avionics_lookup
  ON aircraft_model_variant_default_avionics (aircraft_model_variant_id, model_year);
CREATE INDEX idx_aircraft_default_avionics_candidates_product
  ON aircraft_model_variant_default_avionics_candidates (
    avionics_model_id, aircraft_model_variant_id, model_year, id
  );
CREATE UNIQUE INDEX idx_listing_verification_run_items_one_active_listing
  ON listing_verification_run_items (listing_id)
  WHERE status IN ('queued', 'running');
CREATE UNIQUE INDEX idx_listing_verification_run_items_one_running_per_run
  ON listing_verification_run_items (run_id)
  WHERE status = 'running';
CREATE INDEX idx_listing_verification_run_items_claim
  ON listing_verification_run_items (run_id, status, position, id);
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
WHEN NEW.avionics_model_id IS NOT OLD.avionics_model_id
AND NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
)
AND NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_authorized_consolidations guard
  JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
  JOIN avionics_models legacy ON legacy.id = OLD.avionics_model_id
  WHERE guard.duplicate_model_id = OLD.avionics_model_id
    AND guard.survivor_model_id = NEW.avionics_model_id
    AND survivor.catalog_status = 'unreviewed'
    AND legacy.catalog_status = 'unreviewed'
)
BEGIN
  SELECT RAISE(ABORT, 'default avionics association requires an approved catalog entry');
END;
CREATE TRIGGER aircraft_default_avionics_candidate_active_conflict_insert
BEFORE INSERT ON aircraft_model_variant_default_avionics_candidates
WHEN EXISTS (
  SELECT 1
  FROM aircraft_model_variant_default_avionics active
  WHERE active.aircraft_model_variant_id = NEW.aircraft_model_variant_id
    AND active.model_year = NEW.model_year
    AND active.avionics_model_id = NEW.avionics_model_id
)
BEGIN
  SELECT RAISE(ABORT, 'default avionics claim already exists in the canonical table');
END;
CREATE TRIGGER aircraft_default_avionics_candidate_claim_immutable
BEFORE UPDATE ON aircraft_model_variant_default_avionics_candidates
BEGIN
  SELECT RAISE(ABORT, 'pending default avionics claims must be replaced, admitted, or rejected explicitly');
END;
CREATE TRIGGER aircraft_default_avionics_candidate_admission_guard
BEFORE INSERT ON aircraft_model_variant_default_avionics
WHEN EXISTS (
  SELECT 1
  FROM aircraft_model_variant_default_avionics_candidates candidate
  WHERE candidate.aircraft_model_variant_id = NEW.aircraft_model_variant_id
    AND candidate.model_year = NEW.model_year
    AND candidate.avionics_model_id = NEW.avionics_model_id
)
AND NOT EXISTS (
  SELECT 1
  FROM aircraft_model_variant_default_avionics_candidates candidate
  WHERE candidate.aircraft_model_variant_id = NEW.aircraft_model_variant_id
    AND candidate.model_year = NEW.model_year
    AND candidate.avionics_model_id = NEW.avionics_model_id
    AND candidate.quantity = NEW.quantity
    AND candidate.source_url = NEW.source_url
    AND candidate.source_title = NEW.source_title
    AND candidate.source_notes = NEW.source_notes
    AND candidate.source_confidence = NEW.source_confidence
)
BEGIN
  SELECT RAISE(ABORT, 'canonical default admission must exactly match its pending claim');
END;
CREATE TRIGGER aircraft_default_avionics_candidate_admission_move
AFTER INSERT ON aircraft_model_variant_default_avionics
BEGIN
  DELETE FROM aircraft_model_variant_default_avionics_candidates
  WHERE aircraft_model_variant_id = NEW.aircraft_model_variant_id
    AND model_year = NEW.model_year
    AND avionics_model_id = NEW.avionics_model_id
    AND quantity = NEW.quantity
    AND source_url = NEW.source_url
    AND source_title = NEW.source_title
    AND source_notes = NEW.source_notes
    AND source_confidence = NEW.source_confidence;
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
  OR EXISTS (
    SELECT 1
    FROM aircraft_reference_avionics reference_link
    WHERE reference_link.avionics_model_id = OLD.id
  )
)
BEGIN
  SELECT RAISE(ABORT, 'referenced avionics catalog entry cannot be unapproved');
END;
CREATE TRIGGER aircraft_serial_schemes_require_approval
BEFORE INSERT ON aircraft_serial_number_schemes
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'serial_scheme'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'serial scheme requires an approved evidence-backed decision'); END;
CREATE TRIGGER aircraft_reference_versions_require_approval
BEFORE INSERT ON aircraft_reference_configuration_versions
WHEN NEW.publication_state <> 'building'
OR NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'reference_profile'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
OR (
  NEW.supersedes_version_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_reference_configuration_versions previous
    WHERE previous.id = NEW.supersedes_version_id
      AND previous.aircraft_reference_configuration_id = NEW.aircraft_reference_configuration_id
      AND previous.model_year = NEW.model_year
      AND previous.publication_state = 'published'
  )
)
BEGIN SELECT RAISE(ABORT, 'reference profile requires building state, approved evidence, and a valid predecessor'); END;
CREATE TRIGGER aircraft_reference_price_building_insert
BEFORE INSERT ON aircraft_reference_prices
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR (
  NEW.evidence_kind = 'direct_model_year'
  AND NEW.price_reference_year <> (
    SELECT model_year FROM aircraft_reference_configuration_versions
    WHERE id = NEW.aircraft_reference_configuration_version_id
  )
)
BEGIN SELECT RAISE(ABORT, 'reference price requires a building version and consistent year'); END;
CREATE TRIGGER aircraft_reference_price_immutable_update
BEFORE UPDATE ON aircraft_reference_prices
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_price_immutable_delete
BEFORE DELETE ON aircraft_reference_prices
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_versions_publish
BEFORE UPDATE OF publication_state ON aircraft_reference_configuration_versions
WHEN NEW.publication_state = 'published'
BEGIN
  SELECT RAISE(ABORT, 'only a building reference profile can be published')
  WHERE OLD.publication_state <> 'building';
  SELECT RAISE(ABORT, 'published reference profile requires published_at')
  WHERE NEW.published_at IS NULL;
  SELECT RAISE(ABORT, 'published reference profile requires applicability')
  WHERE NOT EXISTS (
    SELECT 1 FROM aircraft_reference_applicability_scopes scope
    WHERE scope.aircraft_reference_configuration_version_id = NEW.id
  );
  SELECT RAISE(ABORT, 'published reference profile requires direct exact-year primary price evidence')
  WHERE NOT EXISTS (
    SELECT 1
    FROM aircraft_reference_prices price
    JOIN curation_evidence_claims claim ON claim.id = price.evidence_claim_id
    JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
    WHERE price.aircraft_reference_configuration_version_id = NEW.id
      AND price.price_kind IN ('base_msrp', 'equipped_msrp')
      AND price.evidence_kind = 'direct_model_year'
      AND price.price_reference_year = NEW.model_year
      AND claim.validation_status = 'validated'
      AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
  );
  SELECT RAISE(ABORT, 'published reference profile requires approved engine catalog models')
  WHERE EXISTS (
    SELECT 1
    FROM aircraft_reference_engines engine
    LEFT JOIN aircraft_engine_catalog_models model
      ON model.id = engine.aircraft_engine_catalog_model_id
     AND model.catalog_status = 'approved'
    WHERE engine.aircraft_reference_configuration_version_id = NEW.id
      AND model.id IS NULL
  );
  SELECT RAISE(ABORT, 'published reference profile requires approved propeller catalog models')
  WHERE EXISTS (
    SELECT 1
    FROM aircraft_reference_propellers propeller
    LEFT JOIN aircraft_propeller_catalog_models model
      ON model.id = propeller.aircraft_propeller_catalog_model_id
     AND model.catalog_status = 'approved'
    WHERE propeller.aircraft_reference_configuration_version_id = NEW.id
      AND model.id IS NULL
  );
  SELECT RAISE(ABORT, 'published reference profile facts require validated primary evidence')
  WHERE EXISTS (
    SELECT 1
    FROM (
      SELECT evidence_claim_id FROM aircraft_reference_applicability_scopes
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id FROM aircraft_reference_prices
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id FROM aircraft_reference_avionics
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id FROM aircraft_reference_engines
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id FROM aircraft_reference_propellers
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id FROM aircraft_reference_features
      WHERE aircraft_reference_configuration_version_id = NEW.id
    ) fact
    JOIN curation_evidence_claims claim ON claim.id = fact.evidence_claim_id
    JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
    WHERE claim.validation_status <> 'validated'
       OR source.source_tier NOT IN ('manufacturer_primary', 'regulator_primary')
  );
  SELECT RAISE(ABORT, 'reference profile contains overlapping applicability scopes')
  WHERE EXISTS (
    SELECT 1
    FROM aircraft_reference_applicability_scopes left_scope
    JOIN aircraft_reference_applicability_scopes right_scope
      ON right_scope.aircraft_reference_configuration_version_id = left_scope.aircraft_reference_configuration_version_id
     AND right_scope.id > left_scope.id
     AND right_scope.aircraft_market_id = left_scope.aircraft_market_id
    WHERE left_scope.aircraft_reference_configuration_version_id = NEW.id
      AND (
        left_scope.applies_to_all_serials = 1
        OR right_scope.applies_to_all_serials = 1
        OR (
          left_scope.aircraft_serial_number_scheme_id = right_scope.aircraft_serial_number_scheme_id
          AND coalesce(left_scope.serial_prefix, '') = coalesce(right_scope.serial_prefix, '')
          AND left_scope.serial_from_sort_key <= right_scope.serial_to_sort_key
          AND right_scope.serial_from_sort_key <= left_scope.serial_to_sort_key
        )
      )
  );
  SELECT RAISE(ABORT, 'published reference profile applicability overlaps an existing version')
  WHERE EXISTS (
    SELECT 1
    FROM aircraft_reference_applicability_scopes candidate
    JOIN aircraft_reference_applicability_scopes existing
      ON existing.aircraft_market_id = candidate.aircraft_market_id
    JOIN aircraft_reference_configuration_versions existing_version
      ON existing_version.id = existing.aircraft_reference_configuration_version_id
    WHERE candidate.aircraft_reference_configuration_version_id = NEW.id
      AND existing_version.id <> NEW.id
      AND existing_version.aircraft_reference_configuration_id = NEW.aircraft_reference_configuration_id
      AND existing_version.model_year = NEW.model_year
      AND existing_version.publication_state = 'published'
      AND (
        candidate.applies_to_all_serials = 1
        OR existing.applies_to_all_serials = 1
        OR (
          candidate.aircraft_serial_number_scheme_id = existing.aircraft_serial_number_scheme_id
          AND coalesce(candidate.serial_prefix, '') = coalesce(existing.serial_prefix, '')
          AND candidate.serial_from_sort_key <= existing.serial_to_sort_key
          AND existing.serial_from_sort_key <= candidate.serial_to_sort_key
        )
      )
  );
END;
CREATE TRIGGER aircraft_valuation_projection_validate_insert
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
CREATE TRIGGER projected_aircraft_spec_variant_move
BEFORE UPDATE OF aircraft_model_variant_id ON aircraft_model_spec_versions
WHEN NEW.aircraft_model_variant_id <> OLD.aircraft_model_variant_id
 AND (
   EXISTS (SELECT 1 FROM aircraft_valuation_compatibility_projections
           WHERE aircraft_model_variant_id = OLD.aircraft_model_variant_id)
   OR EXISTS (SELECT 1 FROM aircraft_valuation_compatibility_projections
              WHERE aircraft_model_variant_id = NEW.aircraft_model_variant_id)
 )
BEGIN SELECT RAISE(ABORT, 'aircraft spec evidence cannot move into or out of a projected variant'); END;
CREATE TRIGGER projected_aircraft_price_variant_move
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
CREATE TRIGGER projected_aircraft_default_avionics_variant_move
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
