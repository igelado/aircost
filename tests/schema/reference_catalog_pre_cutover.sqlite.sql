-- Exact legacy relation surface present immediately before the reference
-- catalog cutover. Historical migration tests load this only into disposable
-- databases so they exercise their own invariants instead of failing merely
-- because the current fresh schema has intentionally removed these tables.

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

CREATE INDEX idx_depreciation_profile_fit_metadata_category
  ON depreciation_profile_fit_metadata (fit_category);

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

CREATE INDEX idx_aircraft_model_spec_versions_model
  ON aircraft_model_spec_versions (
    aircraft_model_id,
    aircraft_model_variant_id,
    effective_from
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

CREATE INDEX idx_aircraft_model_variant_price_points_lookup
  ON aircraft_model_variant_price_points (aircraft_model_variant_id, model_year);

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

CREATE INDEX idx_aircraft_model_variant_default_avionics_lookup
  ON aircraft_model_variant_default_avionics (aircraft_model_variant_id, model_year);

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

CREATE INDEX idx_aircraft_default_avionics_candidates_product
  ON aircraft_model_variant_default_avionics_candidates (
    avionics_model_id, aircraft_model_variant_id, model_year, id
  );
