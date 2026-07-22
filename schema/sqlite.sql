-- Canonical SQLite schema for the Rust AirCost services.
CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  email TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL,
  auth_provider TEXT NOT NULL DEFAULT 'local',
  auth_subject TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS depreciation_profiles (
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

CREATE TABLE IF NOT EXISTS depreciation_profile_fit_metadata (
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

CREATE INDEX IF NOT EXISTS idx_depreciation_profile_fit_metadata_category
  ON depreciation_profile_fit_metadata (fit_category);

CREATE TABLE IF NOT EXISTS component_depreciation_profiles (
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

CREATE TABLE IF NOT EXISTS engine_manufacturers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS engine_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  engine_manufacturer_id INTEGER NOT NULL REFERENCES engine_manufacturers(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  tbo_hours REAL,
  overhaul_cost_usd REAL,
  value_reference_year INTEGER,
  source_url TEXT,
  source_title TEXT,
  source_confidence TEXT
    CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low')),
  evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (engine_manufacturer_id, normalized_name),
  CHECK (
    is_valuation_eligible = 0
    OR (
      evidence_kind = 'authoritative_reference'
      AND source_confidence = 'high'
      AND source_url IS NOT NULL
      AND tbo_hours > 0
      AND overhaul_cost_usd >= 0
      AND value_reference_year BETWEEN 1900 AND 2200
    )
  )
);

CREATE INDEX IF NOT EXISTS idx_engine_models_manufacturer
  ON engine_models (engine_manufacturer_id);

CREATE TABLE IF NOT EXISTS propeller_manufacturers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS propeller_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  propeller_manufacturer_id INTEGER NOT NULL REFERENCES propeller_manufacturers(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  tbo_hours REAL,
  overhaul_cost_usd REAL,
  value_reference_year INTEGER,
  source_url TEXT,
  source_title TEXT,
  source_confidence TEXT
    CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low')),
  evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (propeller_manufacturer_id, normalized_name),
  CHECK (
    is_valuation_eligible = 0
    OR (
      evidence_kind = 'authoritative_reference'
      AND source_confidence = 'high'
      AND source_url IS NOT NULL
      AND tbo_hours > 0
      AND overhaul_cost_usd >= 0
      AND value_reference_year BETWEEN 1900 AND 2200
    )
  )
);

CREATE INDEX IF NOT EXISTS idx_propeller_models_manufacturer
  ON propeller_models (propeller_manufacturer_id);

CREATE TABLE IF NOT EXISTS aircraft_manufacturers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS aircraft_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_manufacturer_id INTEGER NOT NULL REFERENCES aircraft_manufacturers(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_manufacturer_id, normalized_name)
);

CREATE TABLE IF NOT EXISTS aircraft_model_variants (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_id INTEGER NOT NULL REFERENCES aircraft_models(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_id, normalized_name)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_model_variants_model
  ON aircraft_model_variants (aircraft_model_id);

CREATE TABLE IF NOT EXISTS aircraft_model_spec_versions (
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

CREATE INDEX IF NOT EXISTS idx_aircraft_model_spec_versions_model
  ON aircraft_model_spec_versions (
    aircraft_model_id,
    aircraft_model_variant_id,
    effective_from
  );

CREATE TABLE IF NOT EXISTS aircraft_model_variant_price_points (
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

CREATE INDEX IF NOT EXISTS idx_aircraft_model_variant_price_points_lookup
  ON aircraft_model_variant_price_points (aircraft_model_variant_id, model_year);

CREATE TABLE IF NOT EXISTS avionics_manufacturers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS avionics_types (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS avionics_models (
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

CREATE UNIQUE INDEX IF NOT EXISTS idx_avionics_models_manufacturer_identifier
  ON avionics_models (
    avionics_manufacturer_id,
    normalized_manufacturer_identifier
  )
  WHERE normalized_manufacturer_identifier IS NOT NULL
    AND length(trim(normalized_manufacturer_identifier)) > 0;

-- Legacy unreviewed rows can still contain same-name candidates that require
-- evidence-based consolidation. Approved product identities cannot.
CREATE UNIQUE INDEX IF NOT EXISTS idx_avionics_models_approved_manufacturer_name
  ON avionics_models (avionics_manufacturer_id, normalized_name)
  WHERE catalog_status = 'approved';

CREATE TABLE IF NOT EXISTS avionics_model_types (
  avionics_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_type_id INTEGER NOT NULL
    REFERENCES avionics_types(id) ON DELETE RESTRICT,
  PRIMARY KEY (avionics_model_id, avionics_type_id)
);

CREATE INDEX IF NOT EXISTS idx_avionics_model_types_type
  ON avionics_model_types (avionics_type_id, avionics_model_id);

-- Approval is staged: create an unreviewed product, attach at least one
-- capability, then approve it. An approved product can never be left typeless.
CREATE TRIGGER IF NOT EXISTS avionics_models_approved_types_insert
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

CREATE TRIGGER IF NOT EXISTS avionics_models_approved_types_update
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

CREATE TRIGGER IF NOT EXISTS avionics_model_types_preserve_approved_delete
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

CREATE TRIGGER IF NOT EXISTS avionics_model_types_preserve_approved_update
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

CREATE TABLE IF NOT EXISTS avionics_suite_components (
  suite_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE CASCADE,
  component_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE CASCADE,
  quantity INTEGER NOT NULL DEFAULT 1 CHECK (quantity > 0),
  PRIMARY KEY (suite_model_id, component_model_id),
  CHECK (suite_model_id <> component_model_id)
);

CREATE TRIGGER IF NOT EXISTS avionics_suite_components_approved_insert
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

CREATE TRIGGER IF NOT EXISTS avionics_suite_components_approved_update
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

CREATE TABLE IF NOT EXISTS aircraft_model_variant_default_avionics (
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

CREATE TRIGGER IF NOT EXISTS aircraft_model_variant_default_avionics_approved_insert
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

CREATE TRIGGER IF NOT EXISTS aircraft_model_variant_default_avionics_approved_update
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

CREATE INDEX IF NOT EXISTS idx_aircraft_model_variant_default_avionics_lookup
  ON aircraft_model_variant_default_avionics (aircraft_model_variant_id, model_year);

CREATE TABLE IF NOT EXISTS aircraft_sale_listings (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_variant_id INTEGER NOT NULL REFERENCES aircraft_model_variants(id),
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  is_verified INTEGER NOT NULL DEFAULT 0 CHECK (is_verified IN (0, 1)),
  source_url TEXT,
  model_year INTEGER NOT NULL,
  asking_price_usd REAL NOT NULL,
  currency TEXT NOT NULL DEFAULT 'USD',
  added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  status TEXT NOT NULL DEFAULT 'active',
  ingestion_state TEXT NOT NULL DEFAULT 'incomplete'
    CHECK (ingestion_state IN ('incomplete', 'ready', 'quarantined')),
  ingestion_error TEXT,
  ingestion_completed_at TEXT,
  registration_number TEXT,
  serial_number TEXT,
  airframe_hours REAL NOT NULL,
  engine_hours REAL,
  engine_time_basis TEXT NOT NULL DEFAULT 'unknown'
    CHECK (engine_time_basis IN ('SNEW', 'SMOH', 'SFOH', 'SPOH', 'unknown')),
  engine_time_evidence TEXT,
  engine_time_confidence TEXT
    CHECK (engine_time_confidence IS NULL OR engine_time_confidence IN ('high', 'medium', 'low')),
  propeller_hours REAL,
  propeller_time_basis TEXT NOT NULL DEFAULT 'unknown'
    CHECK (propeller_time_basis IN ('SNEW', 'SMOH', 'SFOH', 'SPOH', 'unknown')),
  propeller_time_evidence TEXT,
  propeller_time_confidence TEXT
    CHECK (propeller_time_confidence IS NULL OR propeller_time_confidence IN ('high', 'medium', 'low')),
  installed_engine_model_id INTEGER REFERENCES engine_models(id),
  installed_engine_source_url TEXT,
  installed_engine_evidence_text TEXT,
  installed_engine_confidence TEXT
    CHECK (installed_engine_confidence IS NULL OR installed_engine_confidence IN ('high', 'medium', 'low')),
  installed_propeller_model_id INTEGER REFERENCES propeller_models(id),
  installed_propeller_source_url TEXT,
  installed_propeller_evidence_text TEXT,
  installed_propeller_confidence TEXT
    CHECK (installed_propeller_confidence IS NULL OR installed_propeller_confidence IN ('high', 'medium', 'low')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (source_url IS NOT NULL OR is_verified = 0),
  CHECK (asking_price_usd BETWEEN 1000 AND 250000000),
  CHECK (airframe_hours >= 0 AND airframe_hours <= 100000),
  CHECK (engine_hours IS NULL OR (engine_hours >= 0 AND engine_hours <= 100000)),
  CHECK (propeller_hours IS NULL OR (propeller_hours >= 0 AND propeller_hours <= 100000)),
  CHECK (engine_hours IS NOT NULL OR engine_time_basis = 'unknown'),
  CHECK (propeller_hours IS NOT NULL OR propeller_time_basis = 'unknown'),
  CHECK (
    (installed_engine_model_id IS NULL
      AND installed_engine_source_url IS NULL
      AND installed_engine_evidence_text IS NULL
      AND installed_engine_confidence IS NULL)
    OR
    (installed_engine_model_id IS NOT NULL
      AND installed_engine_source_url IS NOT NULL
      AND installed_engine_evidence_text IS NOT NULL
      AND installed_engine_confidence IS NOT NULL)
  ),
  CHECK (
    (installed_propeller_model_id IS NULL
      AND installed_propeller_source_url IS NULL
      AND installed_propeller_evidence_text IS NULL
      AND installed_propeller_confidence IS NULL)
    OR
    (installed_propeller_model_id IS NOT NULL
      AND installed_propeller_source_url IS NOT NULL
      AND installed_propeller_evidence_text IS NOT NULL
      AND installed_propeller_confidence IS NOT NULL)
  ),
  CHECK (
    ingestion_state <> 'ready'
    OR (ingestion_error IS NULL AND ingestion_completed_at IS NOT NULL)
  ),
  CHECK (ingestion_state <> 'quarantined' OR ingestion_error IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_variant
  ON aircraft_sale_listings (aircraft_model_variant_id, is_verified, added_at);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_user
  ON aircraft_sale_listings (created_by_user_id);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_ingestion
  ON aircraft_sale_listings (ingestion_state, status, added_at);

CREATE TABLE IF NOT EXISTS plugin_installs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  public_key_base64 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  revoked_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_plugin_installs_user
  ON plugin_installs (user_id, revoked_at);

CREATE TABLE IF NOT EXISTS plugin_submissions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  plugin_install_id INTEGER NOT NULL REFERENCES plugin_installs(id),
  source_url TEXT NOT NULL,
  submitted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  rendered_html TEXT NOT NULL,
  rendered_html_sha256 TEXT NOT NULL,
  signature_base64 TEXT NOT NULL,
  extracted_listing_json TEXT,
  extraction_error TEXT,
  canonical_listing_id INTEGER REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_plugin_submissions_user
  ON plugin_submissions (user_id, submitted_at);

CREATE INDEX IF NOT EXISTS idx_plugin_submissions_listing
  ON plugin_submissions (canonical_listing_id);

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  avionics_model_id INTEGER NOT NULL REFERENCES avionics_models(id),
  quantity INTEGER NOT NULL DEFAULT 1,
  source TEXT NOT NULL DEFAULT 'listing',
  source_notes TEXT,
  configuration_action TEXT NOT NULL DEFAULT 'installed'
    CHECK (configuration_action IN ('installed', 'replaces', 'removes')),
  replaces_avionics_model_id INTEGER REFERENCES avionics_models(id),
  source_confidence TEXT
    CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_sale_listing_id, avionics_model_id),
  CHECK (
    (configuration_action = 'installed' AND replaces_avionics_model_id IS NULL)
    OR
    (configuration_action IN ('replaces', 'removes') AND replaces_avionics_model_id IS NOT NULL)
  )
);

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_approved_insert
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

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_approved_update
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

CREATE TRIGGER IF NOT EXISTS avionics_models_referenced_status_update
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

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_facts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  fact_kind TEXT NOT NULL CHECK (fact_kind IN (
    'restoration', 'damage_history', 'log_completeness', 'paint_condition',
    'interior_condition', 'engine_conversion', 'airframe_conversion', 'major_modification'
  )),
  fact_value TEXT NOT NULL,
  evidence_text TEXT NOT NULL,
  source_url TEXT,
  source_confidence TEXT NOT NULL CHECK (source_confidence IN ('high', 'medium', 'low')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_sale_listing_id, fact_kind, fact_value, evidence_text)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listing_facts_listing
  ON aircraft_sale_listing_facts (aircraft_sale_listing_id, fact_kind);

CREATE TABLE IF NOT EXISTS valuation_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  capture_time TEXT NOT NULL,
  input_sha256 TEXT NOT NULL UNIQUE,
  selection_policy_json TEXT NOT NULL,
  feature_schema_version INTEGER NOT NULL,
  included_count INTEGER NOT NULL,
  excluded_count INTEGER NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS valuation_snapshot_rows (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  snapshot_id INTEGER NOT NULL REFERENCES valuation_snapshots(id) ON DELETE CASCADE,
  source_listing_id INTEGER NOT NULL,
  duplicate_group_key TEXT NOT NULL,
  inclusion_flag INTEGER NOT NULL CHECK (inclusion_flag IN (0, 1)),
  exclusion_reason TEXT,
  feature_json TEXT NOT NULL,
  target_price_usd REAL,
  row_sha256 TEXT NOT NULL,
  UNIQUE (snapshot_id, source_listing_id)
);

CREATE INDEX IF NOT EXISTS idx_valuation_snapshot_rows_snapshot_inclusion
  ON valuation_snapshot_rows (snapshot_id, inclusion_flag, source_listing_id);

CREATE TABLE IF NOT EXISTS valuation_model_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  snapshot_id INTEGER NOT NULL REFERENCES valuation_snapshots(id),
  model_kind TEXT NOT NULL CHECK (model_kind IN ('structural', 'dnn')),
  artifact_format_version INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('candidate', 'active', 'retired')),
  metrics_json TEXT NOT NULL,
  configuration_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_valuation_model_versions_one_active_kind
  ON valuation_model_versions (model_kind) WHERE state = 'active';

CREATE TABLE IF NOT EXISTS valuation_model_artifacts (
  model_version_id INTEGER NOT NULL
    REFERENCES valuation_model_versions(id) ON DELETE CASCADE,
  artifact_name TEXT NOT NULL,
  artifact_bytes BLOB NOT NULL,
  sha256 TEXT NOT NULL,
  media_type TEXT NOT NULL,
  PRIMARY KEY (model_version_id, artifact_name)
);

CREATE TABLE IF NOT EXISTS valuation_fold_predictions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  model_version_id INTEGER NOT NULL
    REFERENCES valuation_model_versions(id) ON DELETE CASCADE,
  fold_id TEXT NOT NULL,
  duplicate_group_key TEXT NOT NULL,
  source_listing_id INTEGER NOT NULL,
  actual_price_usd REAL NOT NULL,
  predicted_price_usd REAL NOT NULL,
  log_error REAL NOT NULL,
  absolute_percentage_error REAL NOT NULL,
  support_grade TEXT NOT NULL CHECK (support_grade IN ('low', 'medium', 'high'))
);

CREATE INDEX IF NOT EXISTS idx_valuation_fold_predictions_model
  ON valuation_fold_predictions (model_version_id, fold_id);

CREATE TABLE IF NOT EXISTS valuation_refresh_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  listings_changed_at TEXT NOT NULL,
  reason TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rental_clubs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  location TEXT NOT NULL,
  airport_code TEXT,
  website_url TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (normalized_name, location)
);

CREATE TABLE IF NOT EXISTS rental_club_cost_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  rental_club_id INTEGER NOT NULL REFERENCES rental_clubs(id),
  effective_from TEXT NOT NULL,
  effective_to TEXT,
  insurance_annual_usd REAL NOT NULL DEFAULT 0,
  club_monthly_usd REAL NOT NULL DEFAULT 0,
  club_annual_usd REAL NOT NULL DEFAULT 0,
  initiation_fee_usd REAL NOT NULL DEFAULT 0,
  source_url TEXT,
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (effective_to IS NULL OR effective_to > effective_from)
);

CREATE TABLE IF NOT EXISTS rental_aircraft_offerings (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  rental_club_id INTEGER NOT NULL REFERENCES rental_clubs(id),
  aircraft_model_variant_id INTEGER NOT NULL REFERENCES aircraft_model_variants(id),
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  display_name TEXT NOT NULL,
  tail_number TEXT,
  is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS rental_rate_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  rental_aircraft_offering_id INTEGER NOT NULL REFERENCES rental_aircraft_offerings(id),
  effective_from TEXT NOT NULL,
  effective_to TEXT,
  rental_rate_per_hour_usd REAL NOT NULL,
  rate_type TEXT NOT NULL DEFAULT 'wet',
  billing_meter TEXT NOT NULL DEFAULT 'hobbs',
  source_url TEXT,
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (effective_to IS NULL OR effective_to > effective_from)
);


-- Curated aircraft identity and immutable reference configurations.
CREATE TABLE IF NOT EXISTS curation_evidence_sources (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_url TEXT NOT NULL,
  resolved_url TEXT,
  source_title TEXT NOT NULL,
  publisher TEXT,
  source_domain TEXT NOT NULL,
  source_tier TEXT NOT NULL CHECK (source_tier IN (
    'manufacturer_primary', 'regulator_primary', 'recognized_secondary',
    'marketplace_observation'
  )),
  content_sha256 TEXT,
  retrieved_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (source_url, content_sha256)
);

CREATE INDEX IF NOT EXISTS idx_curation_evidence_sources_domain
  ON curation_evidence_sources (source_domain, source_tier);

CREATE TABLE IF NOT EXISTS curation_evidence_claims (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  evidence_source_id INTEGER NOT NULL
    REFERENCES curation_evidence_sources(id) ON DELETE RESTRICT,
  claim_kind TEXT NOT NULL CHECK (claim_kind IN (
    'identity', 'alias', 'applicability', 'standard_equipment', 'price',
    'specification', 'package_composition', 'other'
  )),
  subject_text TEXT NOT NULL,
  predicate_text TEXT NOT NULL,
  object_text TEXT NOT NULL,
  quoted_evidence TEXT NOT NULL,
  citation_start INTEGER,
  citation_end INTEGER,
  validation_status TEXT NOT NULL DEFAULT 'captured'
    CHECK (validation_status IN ('captured', 'validated', 'rejected')),
  validated_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(quoted_evidence)) > 0),
  CHECK (
    (citation_start IS NULL AND citation_end IS NULL)
    OR (
      citation_start IS NOT NULL AND citation_start >= 0
      AND citation_end IS NOT NULL AND citation_end > citation_start
    )
  ),
  CHECK (
    validation_status <> 'validated'
    OR validated_at IS NOT NULL
  )
);

CREATE INDEX IF NOT EXISTS idx_curation_evidence_claims_source
  ON curation_evidence_claims (evidence_source_id, claim_kind);

CREATE TRIGGER IF NOT EXISTS curation_evidence_sources_immutable_update
BEFORE UPDATE ON curation_evidence_sources
BEGIN SELECT RAISE(ABORT, 'curation evidence sources are immutable'); END;
CREATE TRIGGER IF NOT EXISTS curation_evidence_sources_immutable_delete
BEFORE DELETE ON curation_evidence_sources
BEGIN SELECT RAISE(ABORT, 'curation evidence sources are immutable'); END;
CREATE TRIGGER IF NOT EXISTS curation_evidence_claims_validate_once
BEFORE UPDATE ON curation_evidence_claims
WHEN OLD.validation_status <> 'captured'
  OR NEW.validation_status NOT IN ('validated', 'rejected')
  OR NEW.evidence_source_id <> OLD.evidence_source_id
  OR NEW.claim_kind <> OLD.claim_kind
  OR NEW.subject_text <> OLD.subject_text
  OR NEW.predicate_text <> OLD.predicate_text
  OR NEW.object_text <> OLD.object_text
  OR NEW.quoted_evidence <> OLD.quoted_evidence
  OR NEW.citation_start IS NOT OLD.citation_start
  OR NEW.citation_end IS NOT OLD.citation_end
BEGIN SELECT RAISE(ABORT, 'curation evidence claims are append-only and validate once'); END;
CREATE TRIGGER IF NOT EXISTS curation_evidence_claims_immutable_delete
BEFORE DELETE ON curation_evidence_claims
BEGIN SELECT RAISE(ABORT, 'curation evidence claims are immutable'); END;

-- Provider telemetry is separate from domain decisions and evidence. One row
-- represents one logical Gemini request, including any transport retries.
CREATE TABLE IF NOT EXISTS gemini_api_usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task TEXT NOT NULL,
  purpose TEXT NOT NULL,
  api_family TEXT NOT NULL
    CHECK (api_family IN ('generate_content', 'interactions')),
  api_version TEXT,
  model TEXT NOT NULL,
  service_tier TEXT NOT NULL DEFAULT 'standard',
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
    'pending', 'completed', 'failed', 'cancelled', 'incomplete',
    'requires_action', 'budget_exceeded'
  )),
  validation_status TEXT NOT NULL DEFAULT 'not_evaluated'
    CHECK (validation_status IN ('not_evaluated', 'accepted', 'rejected')),
  provider_request_id TEXT,
  correlation_id TEXT,
  request_fingerprint TEXT,
  aircraft_sale_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL,
  source_kind TEXT,
  source_id TEXT,
  input_tokens INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
  output_tokens INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
  thought_tokens INTEGER CHECK (thought_tokens IS NULL OR thought_tokens >= 0),
  cached_tokens INTEGER CHECK (cached_tokens IS NULL OR cached_tokens >= 0),
  tool_tokens INTEGER CHECK (tool_tokens IS NULL OR tool_tokens >= 0),
  search_query_count INTEGER
    CHECK (search_query_count IS NULL OR search_query_count >= 0),
  attempt_count INTEGER NOT NULL DEFAULT 1 CHECK (attempt_count >= 1),
  retry_count INTEGER NOT NULL DEFAULT 0
    CHECK (retry_count >= 0 AND retry_count = attempt_count - 1),
  latency_ms INTEGER CHECK (latency_ms IS NULL OR latency_ms >= 0),
  error_text TEXT,
  estimated_cost_microusd INTEGER
    CHECK (estimated_cost_microusd IS NULL OR estimated_cost_microusd >= 0),
  pricing_snapshot_json TEXT,
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (length(trim(task)) > 0),
  CHECK (length(trim(purpose)) > 0),
  CHECK (api_version IS NULL OR length(trim(api_version)) > 0),
  CHECK (length(trim(model)) > 0),
  CHECK (length(trim(service_tier)) > 0),
  CHECK (provider_request_id IS NULL OR length(trim(provider_request_id)) > 0),
  CHECK (correlation_id IS NULL OR length(trim(correlation_id)) > 0),
  CHECK (request_fingerprint IS NULL OR length(trim(request_fingerprint)) > 0),
  CHECK (
    (source_kind IS NULL AND source_id IS NULL)
    OR (
      source_kind IS NOT NULL AND length(trim(source_kind)) > 0
      AND source_id IS NOT NULL AND length(trim(source_id)) > 0
    )
  ),
  CHECK (
    (estimated_cost_microusd IS NULL AND pricing_snapshot_json IS NULL)
    OR (estimated_cost_microusd IS NOT NULL AND pricing_snapshot_json IS NOT NULL)
  ),
  CHECK (
    (status = 'pending' AND completed_at IS NULL)
    OR (status <> 'pending' AND completed_at IS NOT NULL)
  ),
  CHECK (status = 'completed' OR validation_status = 'not_evaluated'),
  CHECK (status <> 'failed' OR length(trim(error_text)) > 0),
  CHECK (status <> 'completed' OR error_text IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_correlation
  ON gemini_api_usage (correlation_id, id);
CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_task_model
  ON gemini_api_usage (task, purpose, model, service_tier, started_at);
CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_listing
  ON gemini_api_usage (aircraft_sale_listing_id, started_at);
CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_source
  ON gemini_api_usage (source_kind, source_id, started_at);

CREATE TABLE IF NOT EXISTS aircraft_curation_interaction_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  provider TEXT NOT NULL DEFAULT 'gemini',
  api_family TEXT NOT NULL CHECK (api_family IN ('interactions', 'generate_content')),
  api_version TEXT NOT NULL,
  model TEXT NOT NULL,
  purpose TEXT NOT NULL CHECK (purpose IN (
    'identity_evidence', 'identity_adjudication', 'profile_evidence',
    'profile_adjudication', 'collision_review', 'correction'
  )),
  provider_interaction_id TEXT,
  previous_interaction_id TEXT,
  prompt_version TEXT NOT NULL,
  schema_version TEXT NOT NULL,
  candidate_catalog_revision TEXT,
  store_requested INTEGER NOT NULL DEFAULT 0 CHECK (store_requested IN (0, 1)),
  request_json TEXT NOT NULL,
  response_json TEXT,
  run_status TEXT NOT NULL CHECK (run_status IN (
    'pending', 'completed', 'failed', 'cancelled', 'incomplete',
    'requires_action', 'budget_exceeded'
  )),
  input_tokens INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
  output_tokens INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
  search_query_count INTEGER NOT NULL DEFAULT 0 CHECK (search_query_count >= 0),
  latency_ms INTEGER CHECK (latency_ms IS NULL OR latency_ms >= 0),
  error_text TEXT,
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  UNIQUE (provider, provider_interaction_id),
  CHECK (run_status <> 'completed' OR completed_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_curation_runs_status
  ON aircraft_curation_interaction_runs (purpose, run_status, started_at);

CREATE TABLE IF NOT EXISTS aircraft_identity_observations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL,
  source_url TEXT,
  observed_make TEXT,
  observed_family TEXT,
  observed_designation TEXT,
  observed_generation TEXT,
  observed_package TEXT,
  model_year INTEGER CHECK (model_year IS NULL OR model_year BETWEEN 1900 AND 2200),
  serial_number TEXT,
  registration_number TEXT,
  market_code TEXT,
  exact_source_evidence TEXT NOT NULL,
  observation_sha256 TEXT NOT NULL UNIQUE,
  legacy_hint_json TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(exact_source_evidence)) > 0)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_identity_observations_listing
  ON aircraft_identity_observations (aircraft_sale_listing_id);

CREATE TABLE IF NOT EXISTS aircraft_identity_resolution_cases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  observation_id INTEGER NOT NULL
    REFERENCES aircraft_identity_observations(id) ON DELETE CASCADE,
  resolution_scope TEXT NOT NULL CHECK (resolution_scope IN (
    'make', 'family', 'designation', 'generation', 'package',
    'engine_model', 'propeller_model',
    'reference_configuration', 'reference_profile'
  )),
  job_fingerprint TEXT NOT NULL UNIQUE,
  catalog_revision TEXT NOT NULL,
  case_status TEXT NOT NULL DEFAULT 'open'
    CHECK (case_status IN ('open', 'adjudicating', 'resolved', 'blocked')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_aircraft_identity_cases_observation
  ON aircraft_identity_resolution_cases (observation_id, resolution_scope);

CREATE TABLE IF NOT EXISTS aircraft_identity_resolution_candidates (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  resolution_case_id INTEGER NOT NULL
    REFERENCES aircraft_identity_resolution_cases(id) ON DELETE CASCADE,
  candidate_kind TEXT NOT NULL CHECK (candidate_kind IN (
    'make', 'family', 'designation', 'generation', 'package',
    'engine_model', 'propeller_model',
    'reference_configuration', 'new_entity'
  )),
  candidate_entity_id INTEGER,
  rank INTEGER NOT NULL CHECK (rank >= 1),
  retrieval_method TEXT NOT NULL,
  retrieval_score REAL,
  candidate_snapshot_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (resolution_case_id, candidate_kind, rank),
  CHECK (
    (candidate_kind = 'new_entity' AND candidate_entity_id IS NULL)
    OR (candidate_kind <> 'new_entity' AND candidate_entity_id IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS aircraft_identity_decisions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  resolution_case_id INTEGER NOT NULL
    REFERENCES aircraft_identity_resolution_cases(id) ON DELETE RESTRICT,
  interaction_run_id INTEGER
    REFERENCES aircraft_curation_interaction_runs(id) ON DELETE RESTRICT,
  entity_kind TEXT NOT NULL CHECK (entity_kind IN (
    'make', 'family', 'designation', 'alias', 'identifier', 'generation',
    'generation_designation', 'package', 'package_applicability',
    'engine_model', 'propeller_model',
    'reference_configuration', 'serial_scheme', 'feature_definition',
    'reference_profile'
  )),
  decision_action TEXT NOT NULL CHECK (decision_action IN (
    'match_existing', 'approve_new', 'ambiguous', 'not_an_entity', 'reject'
  )),
  decision_status TEXT NOT NULL CHECK (decision_status IN (
    'approved', 'rejected', 'ambiguous'
  )),
  selected_entity_id INTEGER,
  decision_payload_json TEXT NOT NULL,
  deterministic_validation_json TEXT NOT NULL,
  deterministic_validation_passed INTEGER NOT NULL
    CHECK (deterministic_validation_passed IN (0, 1)),
  rationale TEXT NOT NULL,
  decided_by_user_id INTEGER REFERENCES users(id) ON DELETE RESTRICT,
  decided_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (decision_status = 'approved'
      AND decision_action IN ('match_existing', 'approve_new')
      AND deterministic_validation_passed = 1)
    OR (decision_status = 'rejected'
      AND decision_action IN ('not_an_entity', 'reject'))
    OR (decision_status = 'ambiguous' AND decision_action = 'ambiguous')
  ),
  CHECK (
    (decision_action = 'match_existing' AND selected_entity_id IS NOT NULL)
    OR (decision_action <> 'match_existing' AND selected_entity_id IS NULL)
  )
);

CREATE INDEX IF NOT EXISTS idx_aircraft_identity_decisions_case
  ON aircraft_identity_decisions (resolution_case_id, decision_status);

CREATE TABLE IF NOT EXISTS aircraft_identity_decision_claims (
  decision_id INTEGER NOT NULL
    REFERENCES aircraft_identity_decisions(id) ON DELETE CASCADE,
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  evidence_role TEXT NOT NULL CHECK (evidence_role IN (
    'identity', 'difference', 'applicability', 'standard_equipment',
    'price', 'specification'
  )),
  PRIMARY KEY (decision_id, evidence_claim_id, evidence_role)
);

CREATE TABLE IF NOT EXISTS aircraft_reference_profile_proposals (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  resolution_case_id INTEGER NOT NULL
    REFERENCES aircraft_identity_resolution_cases(id) ON DELETE CASCADE,
  interaction_run_id INTEGER
    REFERENCES aircraft_curation_interaction_runs(id) ON DELETE RESTRICT,
  proposed_identity_json TEXT NOT NULL,
  proposed_profile_json TEXT NOT NULL,
  deterministic_validation_json TEXT NOT NULL,
  validation_status TEXT NOT NULL CHECK (validation_status IN (
    'pending', 'valid', 'invalid', 'needs_review'
  )),
  catalog_revision TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Engine and propeller identities used by reference profiles live in a clean,
-- approved-by-construction catalog. Legacy engine_models/propeller_models rows
-- remain outside this trusted boundary until individually curated.
CREATE TABLE IF NOT EXISTS aircraft_engine_catalog_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manufacturer_name TEXT NOT NULL,
  normalized_manufacturer_name TEXT NOT NULL,
  model_name TEXT NOT NULL,
  normalized_model_name TEXT NOT NULL,
  identifier_authority TEXT NOT NULL,
  normalized_identifier_authority TEXT NOT NULL,
  identifier_kind TEXT NOT NULL CHECK (identifier_kind IN (
    'manufacturer_model_code', 'regulator_model_designation',
    'manufacturer_part_number'
  )),
  authoritative_identifier TEXT NOT NULL,
  normalized_authoritative_identifier TEXT NOT NULL,
  catalog_status TEXT NOT NULL DEFAULT 'approved'
    CHECK (catalog_status = 'approved'),
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(manufacturer_name)) > 0),
  CHECK (length(trim(normalized_manufacturer_name)) > 0),
  CHECK (length(trim(model_name)) > 0),
  CHECK (length(trim(normalized_model_name)) > 0),
  CHECK (length(trim(identifier_authority)) > 0),
  CHECK (length(trim(normalized_identifier_authority)) > 0),
  CHECK (length(trim(authoritative_identifier)) > 0),
  CHECK (length(trim(normalized_authoritative_identifier)) > 0),
  UNIQUE (normalized_manufacturer_name, normalized_model_name),
  UNIQUE (
    normalized_identifier_authority, identifier_kind,
    normalized_authoritative_identifier
  )
);

CREATE TABLE IF NOT EXISTS aircraft_propeller_catalog_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manufacturer_name TEXT NOT NULL,
  normalized_manufacturer_name TEXT NOT NULL,
  model_name TEXT NOT NULL,
  normalized_model_name TEXT NOT NULL,
  identifier_authority TEXT NOT NULL,
  normalized_identifier_authority TEXT NOT NULL,
  identifier_kind TEXT NOT NULL CHECK (identifier_kind IN (
    'manufacturer_model_code', 'regulator_model_designation',
    'manufacturer_part_number'
  )),
  authoritative_identifier TEXT NOT NULL,
  normalized_authoritative_identifier TEXT NOT NULL,
  catalog_status TEXT NOT NULL DEFAULT 'approved'
    CHECK (catalog_status = 'approved'),
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(manufacturer_name)) > 0),
  CHECK (length(trim(normalized_manufacturer_name)) > 0),
  CHECK (length(trim(model_name)) > 0),
  CHECK (length(trim(normalized_model_name)) > 0),
  CHECK (length(trim(identifier_authority)) > 0),
  CHECK (length(trim(normalized_identifier_authority)) > 0),
  CHECK (length(trim(authoritative_identifier)) > 0),
  CHECK (length(trim(normalized_authoritative_identifier)) > 0),
  UNIQUE (normalized_manufacturer_name, normalized_model_name),
  UNIQUE (
    normalized_identifier_authority, identifier_kind,
    normalized_authoritative_identifier
  )
);

CREATE TABLE IF NOT EXISTS aircraft_markets (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  code TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  parent_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(code)) > 0),
  CHECK (parent_market_id IS NULL OR parent_market_id <> id)
);

INSERT INTO aircraft_markets (code, name)
VALUES ('GLOBAL', 'Global')
ON CONFLICT (code) DO NOTHING;

CREATE TABLE IF NOT EXISTS aircraft_makes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(name)) > 0),
  CHECK (length(trim(normalized_name)) > 0)
);

CREATE TABLE IF NOT EXISTS aircraft_model_families (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_make_id, normalized_name),
  UNIQUE (id, aircraft_make_id),
  CHECK (length(trim(name)) > 0),
  CHECK (length(trim(normalized_name)) > 0)
);

CREATE TABLE IF NOT EXISTS aircraft_designations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id) ON DELETE RESTRICT,
  official_designation TEXT NOT NULL,
  normalized_official_designation TEXT NOT NULL,
  display_name TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_official_designation),
  UNIQUE (id, aircraft_model_family_id),
  CHECK (length(trim(official_designation)) > 0),
  CHECK (length(trim(normalized_official_designation)) > 0),
  CHECK (length(trim(display_name)) > 0)
);

CREATE TABLE IF NOT EXISTS aircraft_make_aliases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  normalized_alias TEXT NOT NULL,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  aircraft_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_make_id, normalized_alias, aircraft_market_id),
  CHECK (valid_from_model_year IS NULL OR valid_from_model_year BETWEEN 1900 AND 2200),
  CHECK (valid_to_model_year IS NULL OR valid_to_model_year BETWEEN 1900 AND 2200),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

CREATE TABLE IF NOT EXISTS aircraft_family_aliases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  normalized_alias TEXT NOT NULL,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  aircraft_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_alias, aircraft_market_id),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

CREATE TABLE IF NOT EXISTS aircraft_designation_aliases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  normalized_alias TEXT NOT NULL,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  aircraft_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_designation_id, normalized_alias, aircraft_market_id),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

CREATE TABLE IF NOT EXISTS aircraft_designation_identifiers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE CASCADE,
  authority TEXT NOT NULL,
  identifier_kind TEXT NOT NULL CHECK (identifier_kind IN (
    'manufacturer_model_code', 'type_certificate_model',
    'type_certificate_number', 'icao_type_designator', 'other_authoritative'
  )),
  identifier_value TEXT NOT NULL,
  normalized_identifier_value TEXT NOT NULL,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  aircraft_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_designation_id, authority, identifier_kind,
    normalized_identifier_value, aircraft_market_id
  ),
  CHECK (length(trim(authority)) > 0),
  CHECK (length(trim(normalized_identifier_value)) > 0),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

-- SQLite considers NULL values distinct inside a UNIQUE constraint. These
-- expression indexes keep unscoped aliases/identifiers unique as well.
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_make_aliases_scope
  ON aircraft_make_aliases (
    aircraft_make_id, normalized_alias, coalesce(aircraft_market_id, 0)
  );
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_family_aliases_scope
  ON aircraft_family_aliases (
    aircraft_model_family_id, normalized_alias, coalesce(aircraft_market_id, 0)
  );
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_designation_aliases_scope
  ON aircraft_designation_aliases (
    aircraft_designation_id, normalized_alias, coalesce(aircraft_market_id, 0)
  );
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_designation_identifiers_scope
  ON aircraft_designation_identifiers (
    aircraft_designation_id, authority, identifier_kind,
    normalized_identifier_value, coalesce(aircraft_market_id, 0)
  );

CREATE TABLE IF NOT EXISTS aircraft_generations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  ordinal INTEGER CHECK (ordinal IS NULL OR ordinal >= 0),
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_name),
  UNIQUE (id, aircraft_model_family_id)
);

CREATE TABLE IF NOT EXISTS aircraft_generation_designations (
  aircraft_generation_id INTEGER NOT NULL
    REFERENCES aircraft_generations(id) ON DELETE CASCADE,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE CASCADE,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (aircraft_generation_id, aircraft_designation_id)
);

CREATE TABLE IF NOT EXISTS aircraft_factory_packages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  package_kind TEXT NOT NULL CHECK (package_kind IN (
    'trim_tier', 'option_bundle', 'special_edition'
  )),
  exclusivity_group TEXT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_name),
  UNIQUE (id, aircraft_model_family_id),
  CHECK (package_kind <> 'trim_tier' OR length(trim(exclusivity_group)) > 0)
);

CREATE TABLE IF NOT EXISTS aircraft_package_applicability (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_factory_package_id INTEGER NOT NULL
    REFERENCES aircraft_factory_packages(id) ON DELETE CASCADE,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE CASCADE,
  aircraft_generation_id INTEGER
    REFERENCES aircraft_generations(id) ON DELETE CASCADE,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_factory_package_id, aircraft_designation_id,
    aircraft_generation_id, valid_from_model_year, valid_to_model_year
  ),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_package_applicability_scope
  ON aircraft_package_applicability (
    aircraft_factory_package_id, aircraft_designation_id,
    coalesce(aircraft_generation_id, 0),
    coalesce(valid_from_model_year, 0), coalesce(valid_to_model_year, 0)
  );

CREATE TABLE IF NOT EXISTS aircraft_reference_configurations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL,
  aircraft_designation_id INTEGER NOT NULL,
  aircraft_generation_id INTEGER,
  tier_package_id INTEGER,
  configuration_kind TEXT NOT NULL CHECK (configuration_kind IN ('base', 'tier')),
  display_name TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (aircraft_designation_id, aircraft_model_family_id)
    REFERENCES aircraft_designations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_generation_id, aircraft_model_family_id)
    REFERENCES aircraft_generations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (tier_package_id, aircraft_model_family_id)
    REFERENCES aircraft_factory_packages(id, aircraft_model_family_id) ON DELETE RESTRICT,
  CHECK (
    (configuration_kind = 'base' AND tier_package_id IS NULL)
    OR (configuration_kind = 'tier' AND tier_package_id IS NOT NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_reference_config_base_no_generation
  ON aircraft_reference_configurations (aircraft_designation_id)
  WHERE configuration_kind = 'base' AND aircraft_generation_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_reference_config_base_generation
  ON aircraft_reference_configurations (aircraft_designation_id, aircraft_generation_id)
  WHERE configuration_kind = 'base' AND aircraft_generation_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_reference_config_tier_no_generation
  ON aircraft_reference_configurations (aircraft_designation_id, tier_package_id)
  WHERE configuration_kind = 'tier' AND aircraft_generation_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_reference_config_tier_generation
  ON aircraft_reference_configurations (
    aircraft_designation_id, aircraft_generation_id, tier_package_id
  )
  WHERE configuration_kind = 'tier' AND aircraft_generation_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS aircraft_serial_number_schemes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  normalization_version TEXT NOT NULL,
  validation_pattern TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_make_id, name, normalization_version)
);

CREATE TABLE IF NOT EXISTS aircraft_reference_configuration_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configurations(id) ON DELETE RESTRICT,
  model_year INTEGER NOT NULL CHECK (model_year BETWEEN 1900 AND 2200),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  supersedes_version_id INTEGER
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE RESTRICT,
  publication_state TEXT NOT NULL DEFAULT 'building'
    CHECK (publication_state IN ('building', 'published', 'superseded')),
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  published_at TEXT,
  superseded_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_id, model_year, revision),
  UNIQUE (supersedes_version_id),
  CHECK (supersedes_version_id IS NULL OR supersedes_version_id <> id),
  CHECK (
    (publication_state = 'building' AND published_at IS NULL AND superseded_at IS NULL)
    OR (publication_state = 'published' AND published_at IS NOT NULL AND superseded_at IS NULL)
    OR (publication_state = 'superseded' AND published_at IS NOT NULL AND superseded_at IS NOT NULL)
  )
);

CREATE INDEX IF NOT EXISTS idx_aircraft_reference_versions_lookup
  ON aircraft_reference_configuration_versions (
    aircraft_reference_configuration_id, model_year, publication_state, revision
  );

CREATE TABLE IF NOT EXISTS aircraft_reference_applicability_scopes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  aircraft_market_id INTEGER NOT NULL REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  applies_to_all_serials INTEGER NOT NULL DEFAULT 1
    CHECK (applies_to_all_serials IN (0, 1)),
  aircraft_serial_number_scheme_id INTEGER
    REFERENCES aircraft_serial_number_schemes(id) ON DELETE RESTRICT,
  serial_prefix TEXT,
  serial_from_display TEXT,
  serial_to_display TEXT,
  serial_from_sort_key TEXT,
  serial_to_sort_key TEXT,
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (applies_to_all_serials = 1
      AND aircraft_serial_number_scheme_id IS NULL
      AND serial_prefix IS NULL
      AND serial_from_display IS NULL AND serial_to_display IS NULL
      AND serial_from_sort_key IS NULL AND serial_to_sort_key IS NULL)
    OR
    (applies_to_all_serials = 0
      AND aircraft_serial_number_scheme_id IS NOT NULL
      AND serial_from_display IS NOT NULL AND serial_to_display IS NOT NULL
      AND serial_from_sort_key IS NOT NULL AND serial_to_sort_key IS NOT NULL
      AND serial_from_sort_key <= serial_to_sort_key)
  ),
  UNIQUE (
    aircraft_reference_configuration_version_id, aircraft_market_id,
    aircraft_serial_number_scheme_id, serial_prefix,
    serial_from_sort_key, serial_to_sort_key
  )
);

CREATE INDEX IF NOT EXISTS idx_aircraft_reference_scope_market
  ON aircraft_reference_applicability_scopes (
    aircraft_market_id, aircraft_serial_number_scheme_id,
    serial_from_sort_key, serial_to_sort_key
  );

CREATE TABLE IF NOT EXISTS aircraft_reference_prices (
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

CREATE TABLE IF NOT EXISTS aircraft_reference_avionics (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  avionics_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE RESTRICT,
  quantity INTEGER NOT NULL CHECK (quantity > 0),
  equipment_role TEXT NOT NULL CHECK (equipment_role IN ('standard', 'included_in_tier')),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, avionics_model_id)
);

CREATE TABLE IF NOT EXISTS aircraft_reference_engines (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  aircraft_engine_catalog_model_id INTEGER NOT NULL
    REFERENCES aircraft_engine_catalog_models(id) ON DELETE RESTRICT,
  quantity INTEGER NOT NULL CHECK (quantity > 0),
  equipment_role TEXT NOT NULL CHECK (equipment_role IN ('standard', 'included_in_tier')),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_reference_configuration_version_id,
    aircraft_engine_catalog_model_id
  )
);

CREATE TABLE IF NOT EXISTS aircraft_reference_propellers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  aircraft_propeller_catalog_model_id INTEGER NOT NULL
    REFERENCES aircraft_propeller_catalog_models(id) ON DELETE RESTRICT,
  quantity INTEGER NOT NULL CHECK (quantity > 0),
  equipment_role TEXT NOT NULL CHECK (equipment_role IN ('standard', 'included_in_tier')),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_reference_configuration_version_id,
    aircraft_propeller_catalog_model_id
  )
);

CREATE TABLE IF NOT EXISTS aircraft_feature_definitions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  feature_key TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL,
  value_type TEXT NOT NULL CHECK (value_type IN ('boolean', 'number', 'text')),
  canonical_unit TEXT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK ((value_type = 'number') OR canonical_unit IS NULL)
);

CREATE TABLE IF NOT EXISTS aircraft_reference_features (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  aircraft_feature_definition_id INTEGER NOT NULL
    REFERENCES aircraft_feature_definitions(id) ON DELETE RESTRICT,
  boolean_value INTEGER CHECK (boolean_value IS NULL OR boolean_value IN (0, 1)),
  number_value REAL,
  text_value TEXT,
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_reference_configuration_version_id,
    aircraft_feature_definition_id
  ),
  CHECK (
    (boolean_value IS NOT NULL) + (number_value IS NOT NULL) + (text_value IS NOT NULL) = 1
  )
);

-- Component catalog entries require an exact validated primary-source
-- identifier claim linked to the matching approved decision.
CREATE TRIGGER IF NOT EXISTS aircraft_engine_catalog_models_require_approval
BEFORE INSERT ON aircraft_engine_catalog_models
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims decision_claim
    ON decision_claim.decision_id = decision.id
  JOIN curation_evidence_claims claim
    ON claim.id = decision_claim.evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'engine_model'
    AND decision_claim.evidence_claim_id = NEW.identity_evidence_claim_id
    AND decision_claim.evidence_role IN ('identity', 'specification')
    AND claim.claim_kind IN ('identity', 'specification')
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'engine catalog model requires an approved primary-source identifier');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_propeller_catalog_models_require_approval
BEFORE INSERT ON aircraft_propeller_catalog_models
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims decision_claim
    ON decision_claim.decision_id = decision.id
  JOIN curation_evidence_claims claim
    ON claim.id = decision_claim.evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'propeller_model'
    AND decision_claim.evidence_claim_id = NEW.identity_evidence_claim_id
    AND decision_claim.evidence_role IN ('identity', 'specification')
    AND claim.claim_kind IN ('identity', 'specification')
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'propeller catalog model requires an approved primary-source identifier');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_engine_catalog_models_immutable_update
BEFORE UPDATE ON aircraft_engine_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved engine catalog models are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_engine_catalog_models_immutable_delete
BEFORE DELETE ON aircraft_engine_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved engine catalog models are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_propeller_catalog_models_immutable_update
BEFORE UPDATE ON aircraft_propeller_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved propeller catalog models are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_propeller_catalog_models_immutable_delete
BEFORE DELETE ON aircraft_propeller_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved propeller catalog models are immutable'); END;

-- Every canonical aircraft identity/configuration row must be backed by one
-- approved decision with at least one validated primary-source identity claim.
CREATE TRIGGER IF NOT EXISTS aircraft_makes_require_approval
BEFORE INSERT ON aircraft_makes
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims decision_claim
    ON decision_claim.decision_id = decision.id
  JOIN curation_evidence_claims claim
    ON claim.id = decision_claim.evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'make'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft make requires an approved primary-source decision');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_families_require_approval
BEFORE INSERT ON aircraft_model_families
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'family'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft family requires an approved primary-source decision');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_designations_require_approval
BEFORE INSERT ON aircraft_designations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'designation'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft designation requires an approved primary-source decision');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_aliases_require_approval_make
BEFORE INSERT ON aircraft_make_aliases
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'alias'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'aircraft alias requires an approved evidence-backed decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_aliases_require_approval_family
BEFORE INSERT ON aircraft_family_aliases
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'alias'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'aircraft alias requires an approved evidence-backed decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_aliases_require_approval_designation
BEFORE INSERT ON aircraft_designation_aliases
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'alias'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'aircraft alias requires an approved evidence-backed decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_identifiers_require_approval
BEFORE INSERT ON aircraft_designation_identifiers
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'identifier'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN SELECT RAISE(ABORT, 'aircraft identifier requires an approved primary-source decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_generations_require_approval
BEFORE INSERT ON aircraft_generations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'generation'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN SELECT RAISE(ABORT, 'aircraft generation requires an approved primary-source decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_generation_designations_require_approval
BEFORE INSERT ON aircraft_generation_designations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions
  WHERE id = NEW.approval_decision_id
    AND decision_status = 'approved' AND decision_action = 'approve_new'
    AND entity_kind = 'generation_designation'
)
OR NOT EXISTS (
  SELECT 1
  FROM aircraft_generations generation
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  WHERE generation.id = NEW.aircraft_generation_id
    AND generation.aircraft_model_family_id = designation.aircraft_model_family_id
)
BEGIN SELECT RAISE(ABORT, 'generation/designation link requires approval within one family'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_packages_require_approval
BEFORE INSERT ON aircraft_factory_packages
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'package'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN SELECT RAISE(ABORT, 'aircraft package requires an approved primary-source decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_package_applicability_require_approval
BEFORE INSERT ON aircraft_package_applicability
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions
  WHERE id = NEW.approval_decision_id
    AND decision_status = 'approved' AND decision_action = 'approve_new'
    AND entity_kind = 'package_applicability'
)
OR NOT EXISTS (
  SELECT 1
  FROM aircraft_factory_packages package
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  LEFT JOIN aircraft_generations generation
    ON generation.id = NEW.aircraft_generation_id
  WHERE package.id = NEW.aircraft_factory_package_id
    AND package.aircraft_model_family_id = designation.aircraft_model_family_id
    AND (
      NEW.aircraft_generation_id IS NULL
      OR generation.aircraft_model_family_id = designation.aircraft_model_family_id
    )
)
BEGIN SELECT RAISE(ABORT, 'package applicability requires approval within one family'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_reference_configurations_require_approval
BEFORE INSERT ON aircraft_reference_configurations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'reference_configuration'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
OR (
  NEW.aircraft_generation_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1 FROM aircraft_generation_designations link
    WHERE link.aircraft_generation_id = NEW.aircraft_generation_id
      AND link.aircraft_designation_id = NEW.aircraft_designation_id
  )
)
OR (
  NEW.tier_package_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_factory_packages package
    JOIN aircraft_package_applicability applicability
      ON applicability.aircraft_factory_package_id = package.id
    WHERE package.id = NEW.tier_package_id
      AND package.package_kind = 'trim_tier'
      AND applicability.aircraft_designation_id = NEW.aircraft_designation_id
      AND (
        applicability.aircraft_generation_id IS NULL
        OR applicability.aircraft_generation_id = NEW.aircraft_generation_id
      )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'reference configuration requires approved applicable identity dimensions');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_serial_schemes_require_approval
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

CREATE TRIGGER IF NOT EXISTS aircraft_feature_definitions_require_approval
BEFORE INSERT ON aircraft_feature_definitions
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions
  WHERE id = NEW.approval_decision_id
    AND decision_status = 'approved' AND decision_action = 'approve_new'
    AND entity_kind = 'feature_definition'
)
BEGIN SELECT RAISE(ABORT, 'feature definition requires an approved decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_reference_versions_require_approval
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

-- Profile children may only be assembled while the parent is building.
CREATE TRIGGER IF NOT EXISTS aircraft_reference_scope_building_insert
BEFORE INSERT ON aircraft_reference_applicability_scopes
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
BEGIN SELECT RAISE(ABORT, 'reference profile children require a building version'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_price_building_insert
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
CREATE TRIGGER IF NOT EXISTS aircraft_reference_avionics_building_insert
BEFORE INSERT ON aircraft_reference_avionics
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR NOT EXISTS (
  SELECT 1 FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id AND model.catalog_status = 'approved'
)
BEGIN SELECT RAISE(ABORT, 'reference avionics requires a building version and approved product'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_engines_building_insert
BEFORE INSERT ON aircraft_reference_engines
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR NOT EXISTS (
  SELECT 1 FROM aircraft_engine_catalog_models model
  WHERE model.id = NEW.aircraft_engine_catalog_model_id
    AND model.catalog_status = 'approved'
)
BEGIN SELECT RAISE(ABORT, 'reference engine requires a building version and approved catalog model'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_propellers_building_insert
BEFORE INSERT ON aircraft_reference_propellers
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR NOT EXISTS (
  SELECT 1 FROM aircraft_propeller_catalog_models model
  WHERE model.id = NEW.aircraft_propeller_catalog_model_id
    AND model.catalog_status = 'approved'
)
BEGIN SELECT RAISE(ABORT, 'reference propeller requires a building version and approved catalog model'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_features_building_insert
BEFORE INSERT ON aircraft_reference_features
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR NOT EXISTS (
  SELECT 1 FROM aircraft_feature_definitions definition
  WHERE definition.id = NEW.aircraft_feature_definition_id
    AND (
      (definition.value_type = 'boolean' AND NEW.boolean_value IS NOT NULL)
      OR (definition.value_type = 'number' AND NEW.number_value IS NOT NULL)
      OR (definition.value_type = 'text' AND NEW.text_value IS NOT NULL)
    )
)
BEGIN SELECT RAISE(ABORT, 'reference feature value does not match its definition'); END;

-- No profile fact can be changed after insertion. Correct data by publishing a
-- replacement version rather than mutating a historical configuration.
CREATE TRIGGER IF NOT EXISTS aircraft_reference_scope_immutable_update
BEFORE UPDATE ON aircraft_reference_applicability_scopes
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_price_immutable_update
BEFORE UPDATE ON aircraft_reference_prices
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_avionics_immutable_update
BEFORE UPDATE ON aircraft_reference_avionics
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_engines_immutable_update
BEFORE UPDATE ON aircraft_reference_engines
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_propellers_immutable_update
BEFORE UPDATE ON aircraft_reference_propellers
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_features_immutable_update
BEFORE UPDATE ON aircraft_reference_features
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_reference_scope_immutable_delete
BEFORE DELETE ON aircraft_reference_applicability_scopes
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_price_immutable_delete
BEFORE DELETE ON aircraft_reference_prices
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_avionics_immutable_delete
BEFORE DELETE ON aircraft_reference_avionics
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_engines_immutable_delete
BEFORE DELETE ON aircraft_reference_engines
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_propellers_immutable_delete
BEFORE DELETE ON aircraft_reference_propellers
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_features_immutable_delete
BEFORE DELETE ON aircraft_reference_features
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;

-- Publication requires a complete exact-year price and at least one applicable
-- market/serial scope. It also rejects overlap with any already-published
-- version of the same logical configuration and model year.
CREATE TRIGGER IF NOT EXISTS aircraft_reference_versions_publish
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

CREATE TRIGGER IF NOT EXISTS aircraft_reference_versions_immutable
BEFORE UPDATE ON aircraft_reference_configuration_versions
WHEN OLD.publication_state IN ('published', 'superseded')
AND NOT (
  OLD.publication_state = 'published'
  AND NEW.publication_state = 'superseded'
  AND NEW.superseded_at IS NOT NULL
  AND NEW.id = OLD.id
  AND NEW.aircraft_reference_configuration_id = OLD.aircraft_reference_configuration_id
  AND NEW.model_year = OLD.model_year
  AND NEW.revision = OLD.revision
  AND NEW.approval_decision_id = OLD.approval_decision_id
  AND NEW.published_at = OLD.published_at
  AND NEW.supersedes_version_id IS OLD.supersedes_version_id
)
BEGIN SELECT RAISE(ABORT, 'published reference profile versions are immutable'); END;

-- Privacy-minimized, target-scoped FAA releasable-registry projections.
CREATE TABLE IF NOT EXISTS faa_registry_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  evidence_source_id INTEGER NOT NULL
    REFERENCES curation_evidence_sources(id) ON DELETE RESTRICT,
  snapshot_date TEXT NOT NULL,
  source_url TEXT NOT NULL,
  archive_sha256 TEXT NOT NULL,
  source_manifest_sha256 TEXT NOT NULL,
  target_set_sha256 TEXT NOT NULL,
  master_member_name TEXT NOT NULL CHECK (master_member_name = 'MASTER.txt'),
  master_member_sha256 TEXT NOT NULL,
  aircraft_member_name TEXT NOT NULL CHECK (aircraft_member_name = 'ACFTREF.txt'),
  aircraft_member_sha256 TEXT NOT NULL,
  engine_member_name TEXT NOT NULL CHECK (engine_member_name = 'ENGINE.txt'),
  engine_member_sha256 TEXT NOT NULL,
  imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (archive_sha256, target_set_sha256),
  CHECK (snapshot_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  CHECK (source_url LIKE 'https://faa.gov/%' OR source_url LIKE 'https://%.faa.gov/%'),
  CHECK (length(archive_sha256) = 64 AND archive_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(source_manifest_sha256) = 64 AND source_manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(target_set_sha256) = 64 AND target_set_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(master_member_sha256) = 64 AND master_member_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(aircraft_member_sha256) = 64 AND aircraft_member_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(engine_member_sha256) = 64 AND engine_member_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE INDEX IF NOT EXISTS idx_faa_registry_snapshots_current
  ON faa_registry_snapshots (snapshot_date DESC, id DESC);

CREATE TRIGGER IF NOT EXISTS faa_registry_snapshots_require_exact_evidence
BEFORE INSERT ON faa_registry_snapshots
WHEN NOT EXISTS (
  SELECT 1 FROM curation_evidence_sources source
  WHERE source.id = NEW.evidence_source_id
    AND source.source_domain = 'faa.gov'
    AND source.source_tier = 'regulator_primary'
    AND source.source_url = NEW.source_url
    AND source.content_sha256 = NEW.archive_sha256
)
BEGIN SELECT RAISE(ABORT, 'FAA snapshot requires exact regulator evidence provenance'); END;

CREATE TABLE IF NOT EXISTS faa_registry_aircraft (
  snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  n_number TEXT NOT NULL,
  manufacturer_serial_raw TEXT,
  manufacturer_serial_key TEXT,
  aircraft_code TEXT NOT NULL,
  engine_code TEXT,
  year_manufactured INTEGER,
  source_record_sha256 TEXT NOT NULL,
  PRIMARY KEY (snapshot_id, n_number),
  UNIQUE (snapshot_id, source_record_sha256),
  CHECK (substr(n_number, 1, 1) = 'N' AND length(n_number) BETWEEN 2 AND 6),
  CHECK (manufacturer_serial_raw IS NULL OR length(trim(manufacturer_serial_raw)) > 0),
  CHECK (manufacturer_serial_key IS NULL OR length(manufacturer_serial_key) > 0),
  CHECK (length(trim(aircraft_code)) > 0),
  CHECK (engine_code IS NULL OR length(trim(engine_code)) > 0),
  CHECK (year_manufactured IS NULL OR year_manufactured BETWEEN 1900 AND 2200),
  CHECK (length(source_record_sha256) = 64 AND source_record_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE INDEX IF NOT EXISTS idx_faa_registry_aircraft_code
  ON faa_registry_aircraft (snapshot_id, aircraft_code);
CREATE INDEX IF NOT EXISTS idx_faa_registry_engine_code
  ON faa_registry_aircraft (snapshot_id, engine_code);

CREATE TABLE IF NOT EXISTS faa_registry_aircraft_references (
  snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  aircraft_code TEXT NOT NULL,
  manufacturer_name TEXT,
  model_name TEXT,
  aircraft_type_code TEXT,
  engine_type_code TEXT,
  category_code TEXT,
  certification_indicator_code TEXT,
  engine_count INTEGER CHECK (engine_count IS NULL OR engine_count >= 0),
  seat_count INTEGER CHECK (seat_count IS NULL OR seat_count >= 0),
  weight_class_code TEXT,
  cruise_speed_mph INTEGER CHECK (cruise_speed_mph IS NULL OR cruise_speed_mph >= 0),
  type_certificate_data_sheet TEXT,
  type_certificate_holder TEXT,
  PRIMARY KEY (snapshot_id, aircraft_code),
  CHECK (length(trim(aircraft_code)) > 0)
);

CREATE TABLE IF NOT EXISTS faa_registry_engine_references (
  snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  engine_code TEXT NOT NULL,
  manufacturer_name TEXT,
  model_name TEXT,
  engine_type_code TEXT,
  horsepower INTEGER CHECK (horsepower IS NULL OR horsepower >= 0),
  thrust_pounds INTEGER CHECK (thrust_pounds IS NULL OR thrust_pounds >= 0),
  PRIMARY KEY (snapshot_id, engine_code),
  CHECK (length(trim(engine_code)) > 0)
);

CREATE TABLE IF NOT EXISTS faa_registry_coverage (
  snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  n_number TEXT NOT NULL,
  lookup_status TEXT NOT NULL CHECK (lookup_status IN ('matched', 'absent')),
  PRIMARY KEY (snapshot_id, n_number),
  CHECK (substr(n_number, 1, 1) = 'N' AND length(n_number) BETWEEN 2 AND 6)
);

CREATE INDEX IF NOT EXISTS idx_faa_registry_coverage_lookup
  ON faa_registry_coverage (n_number, snapshot_id);

CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_references_reachable
BEFORE INSERT ON faa_registry_aircraft_references
WHEN NOT EXISTS (
  SELECT 1 FROM faa_registry_aircraft aircraft
  WHERE aircraft.snapshot_id = NEW.snapshot_id
    AND aircraft.aircraft_code = NEW.aircraft_code
)
BEGIN SELECT RAISE(ABORT, 'FAA aircraft reference must be reachable from a target match'); END;

CREATE TRIGGER IF NOT EXISTS faa_registry_engine_references_reachable
BEFORE INSERT ON faa_registry_engine_references
WHEN NOT EXISTS (
  SELECT 1 FROM faa_registry_aircraft aircraft
  WHERE aircraft.snapshot_id = NEW.snapshot_id
    AND aircraft.engine_code = NEW.engine_code
)
BEGIN SELECT RAISE(ABORT, 'FAA engine reference must be reachable from a target match'); END;

CREATE TRIGGER IF NOT EXISTS faa_registry_coverage_consistent
BEFORE INSERT ON faa_registry_coverage
WHEN (NEW.lookup_status = 'matched' AND NOT EXISTS (
        SELECT 1 FROM faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id AND aircraft.n_number = NEW.n_number
      ))
  OR (NEW.lookup_status = 'absent' AND EXISTS (
        SELECT 1 FROM faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id AND aircraft.n_number = NEW.n_number
      ))
BEGIN SELECT RAISE(ABORT, 'FAA coverage must agree with its target match'); END;

CREATE TRIGGER IF NOT EXISTS faa_registry_snapshots_immutable_update
BEFORE UPDATE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_snapshots_immutable_delete
BEFORE DELETE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_immutable_update
BEFORE UPDATE ON faa_registry_aircraft
BEGIN SELECT RAISE(ABORT, 'FAA registry aircraft are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_immutable_delete
BEFORE DELETE ON faa_registry_aircraft
BEGIN SELECT RAISE(ABORT, 'FAA registry aircraft are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_references_immutable_update
BEFORE UPDATE ON faa_registry_aircraft_references
BEGIN SELECT RAISE(ABORT, 'FAA aircraft references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_references_immutable_delete
BEFORE DELETE ON faa_registry_aircraft_references
BEGIN SELECT RAISE(ABORT, 'FAA aircraft references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_engine_references_immutable_update
BEFORE UPDATE ON faa_registry_engine_references
BEGIN SELECT RAISE(ABORT, 'FAA engine references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_engine_references_immutable_delete
BEFORE DELETE ON faa_registry_engine_references
BEGIN SELECT RAISE(ABORT, 'FAA engine references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_coverage_immutable_update
BEFORE UPDATE ON faa_registry_coverage
BEGIN SELECT RAISE(ABORT, 'FAA registry coverage is immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_coverage_immutable_delete
BEFORE DELETE ON faa_registry_coverage
BEGIN SELECT RAISE(ABORT, 'FAA registry coverage is immutable'); END;
