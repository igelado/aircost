-- Apply to a backup first and invoke sqlite3 with -bail. This is a one-time
-- migration: do not reapply it once aircraft_sale_listings.ingestion_state
-- exists. This migration is intentionally not run by the app.
-- Existing listings and valuation metadata are quarantined/unreviewed rather
-- than silently promoted into the new valuation-grade contract.

PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;
BEGIN IMMEDIATE;

ALTER TABLE aircraft_sale_listings RENAME TO aircraft_sale_listings_legacy;

CREATE TABLE aircraft_sale_listings (
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
  CHECK (
    ingestion_state = 'quarantined'
    OR asking_price_usd BETWEEN 1000 AND 250000000
  ),
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

INSERT INTO aircraft_sale_listings (
  id, aircraft_model_variant_id, created_by_user_id, is_verified, source_url,
  model_year, asking_price_usd, currency, added_at, status,
  ingestion_state, ingestion_error, ingestion_completed_at,
  registration_number, serial_number, airframe_hours,
  engine_hours, engine_time_basis, propeller_hours, propeller_time_basis,
  created_at, updated_at
)
SELECT
  id, aircraft_model_variant_id, created_by_user_id, is_verified, source_url,
  model_year, asking_price_usd, currency, added_at, status,
  'quarantined',
  'Migrated from the pre-readiness schema; review evidence before marking ready.',
  NULL,
  registration_number, serial_number, airframe_hours,
  engine_hours, 'unknown', propeller_hours, 'unknown',
  created_at, updated_at
FROM aircraft_sale_listings_legacy;

DROP TABLE aircraft_sale_listings_legacy;

CREATE INDEX idx_aircraft_sale_listings_variant
  ON aircraft_sale_listings (aircraft_model_variant_id, is_verified, added_at);
CREATE INDEX idx_aircraft_sale_listings_user
  ON aircraft_sale_listings (created_by_user_id);
CREATE INDEX idx_aircraft_sale_listings_ingestion
  ON aircraft_sale_listings (ingestion_state, status, added_at);

ALTER TABLE aircraft_model_variant_price_points
  ADD COLUMN evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN (
      'direct_model_year', 'direct_other_year', 'interpolated', 'inferred', 'unreviewed'
    ));
ALTER TABLE aircraft_model_variant_price_points
  ADD COLUMN is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1));

ALTER TABLE engine_models
  ADD COLUMN evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed'));
ALTER TABLE engine_models
  ADD COLUMN is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1));

ALTER TABLE propeller_models
  ADD COLUMN evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed'));
ALTER TABLE propeller_models
  ADD COLUMN is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1));

ALTER TABLE aircraft_model_spec_versions
  ADD COLUMN configuration_scope TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (configuration_scope IN ('factory_default', 'listing_specific', 'unreviewed'));
ALTER TABLE aircraft_model_spec_versions ADD COLUMN source_confidence TEXT
  CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low'));
ALTER TABLE aircraft_model_spec_versions
  ADD COLUMN evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed'));
ALTER TABLE aircraft_model_spec_versions
  ADD COLUMN is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1));

ALTER TABLE avionics_models
  ADD COLUMN value_basis TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (value_basis IN ('installed_contribution', 'replacement_cost', 'unreviewed'));
ALTER TABLE avionics_models ADD COLUMN replacement_cost_usd REAL;
ALTER TABLE avionics_models
  ADD COLUMN valuation_scope TEXT NOT NULL DEFAULT 'unit'
    CHECK (valuation_scope IN ('unit', 'integrated_suite'));

-- A newer binary may already have created additive tables through the fresh
-- schema initializer before this explicit migration reaches the legacy tables.
-- Preserve those rows and make this one-time migration safe for that mixed
-- schema state.
CREATE TABLE IF NOT EXISTS avionics_suite_components (
  suite_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE CASCADE,
  component_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE CASCADE,
  quantity INTEGER NOT NULL DEFAULT 1 CHECK (quantity > 0),
  PRIMARY KEY (suite_model_id, component_model_id),
  CHECK (suite_model_id <> component_model_id)
);

ALTER TABLE aircraft_sale_listing_avionics
  ADD COLUMN configuration_action TEXT NOT NULL DEFAULT 'installed'
    CHECK (configuration_action IN ('installed', 'replaces', 'removes'));
ALTER TABLE aircraft_sale_listing_avionics
  ADD COLUMN replaces_avionics_model_id INTEGER REFERENCES avionics_models(id);
ALTER TABLE aircraft_sale_listing_avionics ADD COLUMN source_confidence TEXT
  CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low'));

CREATE TRIGGER price_point_eligibility_insert_check
BEFORE INSERT ON aircraft_model_variant_price_points
WHEN NEW.is_valuation_eligible = 1 AND (
  NEW.source_confidence IS NOT 'high'
  OR NEW.evidence_kind IS NOT 'direct_model_year'
  OR NEW.purchase_price_reference_year <> NEW.model_year
)
BEGIN
  SELECT RAISE(ABORT, 'valuation-eligible price point lacks direct high-confidence model-year evidence');
END;

CREATE TRIGGER price_point_eligibility_update_check
BEFORE UPDATE ON aircraft_model_variant_price_points
WHEN NEW.is_valuation_eligible = 1 AND (
  NEW.source_confidence IS NOT 'high'
  OR NEW.evidence_kind IS NOT 'direct_model_year'
  OR NEW.purchase_price_reference_year <> NEW.model_year
)
BEGIN
  SELECT RAISE(ABORT, 'valuation-eligible price point lacks direct high-confidence model-year evidence');
END;

CREATE TRIGGER aircraft_spec_eligibility_insert_check
BEFORE INSERT ON aircraft_model_spec_versions
WHEN NEW.is_valuation_eligible = 1 AND (
  NEW.configuration_scope IS NOT 'factory_default'
  OR NEW.source_confidence IS NOT 'high'
  OR NEW.evidence_kind IS NOT 'authoritative_reference'
  OR NEW.source_url IS NULL
  OR length(trim(NEW.source_url)) = 0
)
BEGIN
  SELECT RAISE(ABORT, 'valuation-eligible aircraft spec lacks authoritative factory-default evidence');
END;

CREATE TRIGGER aircraft_spec_eligibility_update_check
BEFORE UPDATE ON aircraft_model_spec_versions
WHEN NEW.is_valuation_eligible = 1 AND (
  NEW.configuration_scope IS NOT 'factory_default'
  OR NEW.source_confidence IS NOT 'high'
  OR NEW.evidence_kind IS NOT 'authoritative_reference'
  OR NEW.source_url IS NULL
  OR length(trim(NEW.source_url)) = 0
)
BEGIN
  SELECT RAISE(ABORT, 'valuation-eligible aircraft spec lacks authoritative factory-default evidence');
END;

CREATE TRIGGER engine_model_eligibility_insert_check
BEFORE INSERT ON engine_models
WHEN NEW.is_valuation_eligible = 1 AND (
  NEW.evidence_kind IS NOT 'authoritative_reference'
  OR NEW.source_confidence IS NOT 'high'
  OR NEW.source_url IS NULL
  OR NEW.tbo_hours IS NULL OR NEW.tbo_hours <= 0
  OR NEW.overhaul_cost_usd IS NULL OR NEW.overhaul_cost_usd < 0
  OR NEW.value_reference_year IS NULL
  OR NEW.value_reference_year NOT BETWEEN 1900 AND 2200
)
BEGIN
  SELECT RAISE(ABORT, 'valuation-eligible engine model lacks authoritative numeric evidence');
END;

CREATE TRIGGER engine_model_eligibility_update_check
BEFORE UPDATE ON engine_models
WHEN NEW.is_valuation_eligible = 1 AND (
  NEW.evidence_kind IS NOT 'authoritative_reference'
  OR NEW.source_confidence IS NOT 'high'
  OR NEW.source_url IS NULL
  OR NEW.tbo_hours IS NULL OR NEW.tbo_hours <= 0
  OR NEW.overhaul_cost_usd IS NULL OR NEW.overhaul_cost_usd < 0
  OR NEW.value_reference_year IS NULL
  OR NEW.value_reference_year NOT BETWEEN 1900 AND 2200
)
BEGIN
  SELECT RAISE(ABORT, 'valuation-eligible engine model lacks authoritative numeric evidence');
END;

CREATE TRIGGER propeller_model_eligibility_insert_check
BEFORE INSERT ON propeller_models
WHEN NEW.is_valuation_eligible = 1 AND (
  NEW.evidence_kind IS NOT 'authoritative_reference'
  OR NEW.source_confidence IS NOT 'high'
  OR NEW.source_url IS NULL
  OR NEW.tbo_hours IS NULL OR NEW.tbo_hours <= 0
  OR NEW.overhaul_cost_usd IS NULL OR NEW.overhaul_cost_usd < 0
  OR NEW.value_reference_year IS NULL
  OR NEW.value_reference_year NOT BETWEEN 1900 AND 2200
)
BEGIN
  SELECT RAISE(ABORT, 'valuation-eligible propeller model lacks authoritative numeric evidence');
END;

CREATE TRIGGER propeller_model_eligibility_update_check
BEFORE UPDATE ON propeller_models
WHEN NEW.is_valuation_eligible = 1 AND (
  NEW.evidence_kind IS NOT 'authoritative_reference'
  OR NEW.source_confidence IS NOT 'high'
  OR NEW.source_url IS NULL
  OR NEW.tbo_hours IS NULL OR NEW.tbo_hours <= 0
  OR NEW.overhaul_cost_usd IS NULL OR NEW.overhaul_cost_usd < 0
  OR NEW.value_reference_year IS NULL
  OR NEW.value_reference_year NOT BETWEEN 1900 AND 2200
)
BEGIN
  SELECT RAISE(ABORT, 'valuation-eligible propeller model lacks authoritative numeric evidence');
END;

CREATE TRIGGER avionics_installed_value_insert_check
BEFORE INSERT ON avionics_models
WHEN NEW.value_basis = 'installed_contribution' AND (
  NEW.estimated_unit_value_usd IS NULL OR NEW.estimated_unit_value_usd < 0
  OR NEW.replacement_cost_usd IS NULL
  OR NEW.replacement_cost_usd < NEW.estimated_unit_value_usd
  OR NEW.value_reference_year IS NULL
  OR NEW.value_reference_year NOT BETWEEN 1900 AND 2200
  OR NEW.value_source IS NULL
  OR length(trim(NEW.value_source)) = 0
)
BEGIN
  SELECT RAISE(ABORT, 'installed avionics contribution lacks a valid replacement basis');
END;

CREATE TRIGGER avionics_installed_value_update_check
BEFORE UPDATE ON avionics_models
WHEN NEW.value_basis = 'installed_contribution' AND (
  NEW.estimated_unit_value_usd IS NULL OR NEW.estimated_unit_value_usd < 0
  OR NEW.replacement_cost_usd IS NULL
  OR NEW.replacement_cost_usd < NEW.estimated_unit_value_usd
  OR NEW.value_reference_year IS NULL
  OR NEW.value_reference_year NOT BETWEEN 1900 AND 2200
  OR NEW.value_source IS NULL
  OR length(trim(NEW.value_source)) = 0
)
BEGIN
  SELECT RAISE(ABORT, 'installed avionics contribution lacks a valid replacement basis');
END;

CREATE TRIGGER aircraft_sale_listing_avionics_action_insert_check
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN NOT (
  (NEW.configuration_action = 'installed' AND NEW.replaces_avionics_model_id IS NULL)
  OR
  (NEW.configuration_action IN ('replaces', 'removes') AND NEW.replaces_avionics_model_id IS NOT NULL)
)
BEGIN
  SELECT RAISE(ABORT, 'invalid avionics action/replacement target');
END;

CREATE TRIGGER aircraft_sale_listing_avionics_action_update_check
BEFORE UPDATE OF configuration_action, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
WHEN NOT (
  (NEW.configuration_action = 'installed' AND NEW.replaces_avionics_model_id IS NULL)
  OR
  (NEW.configuration_action IN ('replaces', 'removes') AND NEW.replaces_avionics_model_id IS NOT NULL)
)
BEGIN
  SELECT RAISE(ABORT, 'invalid avionics action/replacement target');
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

COMMIT;
PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
PRAGMA foreign_key_check;
