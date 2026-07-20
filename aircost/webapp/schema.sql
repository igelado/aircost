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
  source_confidence TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (engine_manufacturer_id, normalized_name)
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
  source_confidence TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (propeller_manufacturer_id, normalized_name)
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
  created_by_user_id INTEGER REFERENCES users(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (effective_to IS NULL OR effective_to > effective_from)
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
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_variant_id, model_year)
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
  avionics_type_id INTEGER NOT NULL REFERENCES avionics_types(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  introduced_year INTEGER,
  discontinued_year INTEGER,
  estimated_unit_value_usd REAL,
  value_reference_year INTEGER,
  value_source TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (avionics_manufacturer_id, avionics_type_id, normalized_name)
);

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
  registration_number TEXT,
  serial_number TEXT,
  airframe_hours REAL NOT NULL,
  engine_hours REAL NOT NULL,
  propeller_hours REAL NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (source_url IS NOT NULL OR is_verified = 0)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_variant
  ON aircraft_sale_listings (aircraft_model_variant_id, is_verified, added_at);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_user
  ON aircraft_sale_listings (created_by_user_id);

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
  canonical_listing_id INTEGER REFERENCES aircraft_sale_listings(id)
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
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_sale_listing_id, avionics_model_id)
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
