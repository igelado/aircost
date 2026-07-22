-- Apply to a backup first. This migration is intentionally not run by the app.
-- Existing listings and valuation metadata are quarantined/unreviewed rather
-- than silently promoted into the new valuation-grade contract.

BEGIN;

ALTER TABLE aircraft_sale_listings
  ALTER COLUMN engine_hours DROP NOT NULL,
  ALTER COLUMN propeller_hours DROP NOT NULL,
  ADD COLUMN ingestion_state TEXT NOT NULL DEFAULT 'incomplete',
  ADD COLUMN ingestion_error TEXT,
  ADD COLUMN ingestion_completed_at TEXT,
  ADD COLUMN engine_time_basis TEXT NOT NULL DEFAULT 'unknown',
  ADD COLUMN engine_time_evidence TEXT,
  ADD COLUMN engine_time_confidence TEXT,
  ADD COLUMN propeller_time_basis TEXT NOT NULL DEFAULT 'unknown',
  ADD COLUMN propeller_time_evidence TEXT,
  ADD COLUMN propeller_time_confidence TEXT,
  ADD COLUMN installed_engine_model_id BIGINT REFERENCES engine_models(id),
  ADD COLUMN installed_engine_source_url TEXT,
  ADD COLUMN installed_engine_evidence_text TEXT,
  ADD COLUMN installed_engine_confidence TEXT,
  ADD COLUMN installed_propeller_model_id BIGINT REFERENCES propeller_models(id),
  ADD COLUMN installed_propeller_source_url TEXT,
  ADD COLUMN installed_propeller_evidence_text TEXT,
  ADD COLUMN installed_propeller_confidence TEXT;

UPDATE aircraft_sale_listings
SET ingestion_state = 'quarantined',
    ingestion_error = 'Migrated from the pre-readiness schema; review evidence before marking ready.',
    ingestion_completed_at = NULL,
    engine_time_basis = 'unknown',
    propeller_time_basis = 'unknown';

ALTER TABLE aircraft_sale_listings
  ADD CONSTRAINT aircraft_sale_listings_ingestion_state_check
    CHECK (ingestion_state IN ('incomplete', 'ready', 'quarantined')),
  ADD CONSTRAINT aircraft_sale_listings_engine_basis_check
    CHECK (engine_time_basis IN ('SNEW', 'SMOH', 'SFOH', 'SPOH', 'unknown')),
  ADD CONSTRAINT aircraft_sale_listings_propeller_basis_check
    CHECK (propeller_time_basis IN ('SNEW', 'SMOH', 'SFOH', 'SPOH', 'unknown')),
  ADD CONSTRAINT aircraft_sale_listings_engine_confidence_check
    CHECK (engine_time_confidence IS NULL OR engine_time_confidence IN ('high', 'medium', 'low')),
  ADD CONSTRAINT aircraft_sale_listings_propeller_confidence_check
    CHECK (propeller_time_confidence IS NULL OR propeller_time_confidence IN ('high', 'medium', 'low')),
  ADD CONSTRAINT aircraft_sale_listings_installed_engine_confidence_check
    CHECK (installed_engine_confidence IS NULL OR installed_engine_confidence IN ('high', 'medium', 'low')),
  ADD CONSTRAINT aircraft_sale_listings_installed_propeller_confidence_check
    CHECK (installed_propeller_confidence IS NULL OR installed_propeller_confidence IN ('high', 'medium', 'low')),
  ADD CONSTRAINT aircraft_sale_listings_price_check CHECK (
    ingestion_state = 'quarantined'
    OR asking_price_usd BETWEEN 1000 AND 250000000
  ),
  ADD CONSTRAINT aircraft_sale_listings_airframe_hours_check
    CHECK (airframe_hours >= 0 AND airframe_hours <= 100000),
  ADD CONSTRAINT aircraft_sale_listings_engine_hours_check
    CHECK (engine_hours IS NULL OR (engine_hours >= 0 AND engine_hours <= 100000)),
  ADD CONSTRAINT aircraft_sale_listings_propeller_hours_check
    CHECK (propeller_hours IS NULL OR (propeller_hours >= 0 AND propeller_hours <= 100000)),
  ADD CONSTRAINT aircraft_sale_listings_engine_missing_basis_check
    CHECK (engine_hours IS NOT NULL OR engine_time_basis = 'unknown'),
  ADD CONSTRAINT aircraft_sale_listings_propeller_missing_basis_check
    CHECK (propeller_hours IS NOT NULL OR propeller_time_basis = 'unknown'),
  ADD CONSTRAINT aircraft_sale_listings_installed_engine_evidence_check CHECK (
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
  ADD CONSTRAINT aircraft_sale_listings_installed_propeller_evidence_check CHECK (
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
  ADD CONSTRAINT aircraft_sale_listings_ready_check
    CHECK (ingestion_state <> 'ready' OR (ingestion_error IS NULL AND ingestion_completed_at IS NOT NULL)),
  ADD CONSTRAINT aircraft_sale_listings_quarantined_check
    CHECK (ingestion_state <> 'quarantined' OR ingestion_error IS NOT NULL);

CREATE INDEX idx_aircraft_sale_listings_ingestion
  ON aircraft_sale_listings (ingestion_state, status, added_at);

ALTER TABLE aircraft_model_variant_price_points
  ADD COLUMN evidence_kind TEXT NOT NULL DEFAULT 'unreviewed',
  ADD COLUMN is_valuation_eligible BOOLEAN NOT NULL DEFAULT FALSE,
  ADD CONSTRAINT aircraft_model_variant_price_points_evidence_kind_check
    CHECK (evidence_kind IN (
      'direct_model_year', 'direct_other_year', 'interpolated', 'inferred', 'unreviewed'
    )),
  ADD CONSTRAINT aircraft_model_variant_price_points_eligibility_check CHECK (
    NOT is_valuation_eligible
    OR (
      source_confidence = 'high'
      AND evidence_kind = 'direct_model_year'
      AND purchase_price_reference_year = model_year
    )
  );

ALTER TABLE engine_models
  ADD COLUMN evidence_kind TEXT NOT NULL DEFAULT 'unreviewed',
  ADD COLUMN is_valuation_eligible BOOLEAN NOT NULL DEFAULT FALSE,
  ADD CONSTRAINT engine_models_evidence_kind_check
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  ADD CONSTRAINT engine_models_eligibility_check CHECK (
    NOT is_valuation_eligible
    OR (
      evidence_kind = 'authoritative_reference'
      AND source_confidence = 'high'
      AND source_url IS NOT NULL
      AND tbo_hours > 0
      AND overhaul_cost_usd >= 0
      AND value_reference_year BETWEEN 1900 AND 2200
    )
  );

ALTER TABLE propeller_models
  ADD COLUMN evidence_kind TEXT NOT NULL DEFAULT 'unreviewed',
  ADD COLUMN is_valuation_eligible BOOLEAN NOT NULL DEFAULT FALSE,
  ADD CONSTRAINT propeller_models_evidence_kind_check
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  ADD CONSTRAINT propeller_models_eligibility_check CHECK (
    NOT is_valuation_eligible
    OR (
      evidence_kind = 'authoritative_reference'
      AND source_confidence = 'high'
      AND source_url IS NOT NULL
      AND tbo_hours > 0
      AND overhaul_cost_usd >= 0
      AND value_reference_year BETWEEN 1900 AND 2200
    )
  );

ALTER TABLE aircraft_model_spec_versions
  ADD COLUMN configuration_scope TEXT NOT NULL DEFAULT 'unreviewed',
  ADD COLUMN source_confidence TEXT,
  ADD COLUMN evidence_kind TEXT NOT NULL DEFAULT 'unreviewed',
  ADD COLUMN is_valuation_eligible BOOLEAN NOT NULL DEFAULT FALSE,
  ADD CONSTRAINT aircraft_model_spec_versions_configuration_scope_check
    CHECK (configuration_scope IN ('factory_default', 'listing_specific', 'unreviewed')),
  ADD CONSTRAINT aircraft_model_spec_versions_source_confidence_check
    CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low')),
  ADD CONSTRAINT aircraft_model_spec_versions_evidence_kind_check
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  ADD CONSTRAINT aircraft_model_spec_versions_eligibility_check CHECK (
    NOT is_valuation_eligible
    OR (
      configuration_scope = 'factory_default'
      AND source_confidence = 'high'
      AND evidence_kind = 'authoritative_reference'
      AND source_url IS NOT NULL
      AND BTRIM(source_url) <> ''
    )
  );

ALTER TABLE avionics_models
  ADD COLUMN value_basis TEXT NOT NULL DEFAULT 'unreviewed',
  ADD COLUMN replacement_cost_usd DOUBLE PRECISION,
  ADD COLUMN valuation_scope TEXT NOT NULL DEFAULT 'unit',
  ADD CONSTRAINT avionics_models_value_basis_check
    CHECK (value_basis IN ('installed_contribution', 'replacement_cost', 'unreviewed')),
  ADD CONSTRAINT avionics_models_valuation_scope_check
    CHECK (valuation_scope IN ('unit', 'integrated_suite')),
  ADD CONSTRAINT avionics_models_installed_value_check CHECK (
    value_basis <> 'installed_contribution'
    OR (
      estimated_unit_value_usd >= 0
      AND replacement_cost_usd >= estimated_unit_value_usd
      AND value_reference_year BETWEEN 1900 AND 2200
      AND value_source IS NOT NULL
      AND BTRIM(value_source) <> ''
    )
  );

CREATE TABLE avionics_suite_components (
  suite_model_id BIGINT NOT NULL REFERENCES avionics_models(id) ON DELETE CASCADE,
  component_model_id BIGINT NOT NULL REFERENCES avionics_models(id) ON DELETE CASCADE,
  quantity BIGINT NOT NULL DEFAULT 1 CHECK (quantity > 0),
  PRIMARY KEY (suite_model_id, component_model_id),
  CHECK (suite_model_id <> component_model_id)
);

ALTER TABLE aircraft_sale_listing_avionics
  ADD COLUMN configuration_action TEXT NOT NULL DEFAULT 'installed',
  ADD COLUMN replaces_avionics_model_id BIGINT REFERENCES avionics_models(id),
  ADD COLUMN source_confidence TEXT,
  ADD CONSTRAINT aircraft_sale_listing_avionics_configuration_action_check
    CHECK (configuration_action IN ('installed', 'replaces', 'removes')),
  ADD CONSTRAINT aircraft_sale_listing_avionics_source_confidence_check
    CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low')),
  ADD CONSTRAINT aircraft_sale_listing_avionics_action_target_check CHECK (
    (configuration_action = 'installed' AND replaces_avionics_model_id IS NULL)
    OR
    (configuration_action IN ('replaces', 'removes') AND replaces_avionics_model_id IS NOT NULL)
  );

CREATE TABLE aircraft_sale_listing_facts (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  aircraft_sale_listing_id BIGINT NOT NULL
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
CREATE INDEX idx_aircraft_sale_listing_facts_listing
  ON aircraft_sale_listing_facts (aircraft_sale_listing_id, fact_kind);

COMMIT;
