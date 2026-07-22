-- Clean aircraft identity and reference-configuration catalog.
--
-- This migration is additive so the new catalog can be curated in shadow mode.
-- Existing aircraft manufacturers/models/variants, price points, specifications,
-- and default-avionics rows are deliberately not copied or promoted. They are
-- migration inputs only and can be removed after an explicit, reviewed cutover.
-- Apply to a backup first and invoke sqlite3 with -bail.

PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

CREATE TABLE curation_evidence_sources (
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

CREATE INDEX idx_curation_evidence_sources_domain
  ON curation_evidence_sources (source_domain, source_tier);

CREATE TABLE curation_evidence_claims (
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

CREATE INDEX idx_curation_evidence_claims_source
  ON curation_evidence_claims (evidence_source_id, claim_kind);

CREATE TRIGGER curation_evidence_sources_immutable_update
BEFORE UPDATE ON curation_evidence_sources
BEGIN SELECT RAISE(ABORT, 'curation evidence sources are immutable'); END;
CREATE TRIGGER curation_evidence_sources_immutable_delete
BEFORE DELETE ON curation_evidence_sources
BEGIN SELECT RAISE(ABORT, 'curation evidence sources are immutable'); END;
CREATE TRIGGER curation_evidence_claims_validate_once
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
CREATE TRIGGER curation_evidence_claims_immutable_delete
BEFORE DELETE ON curation_evidence_claims
BEGIN SELECT RAISE(ABORT, 'curation evidence claims are immutable'); END;

CREATE TABLE aircraft_curation_interaction_runs (
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

CREATE INDEX idx_aircraft_curation_runs_status
  ON aircraft_curation_interaction_runs (purpose, run_status, started_at);

CREATE TABLE aircraft_identity_observations (
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

CREATE INDEX idx_aircraft_identity_observations_listing
  ON aircraft_identity_observations (aircraft_sale_listing_id);

CREATE TABLE aircraft_identity_resolution_cases (
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

CREATE INDEX idx_aircraft_identity_cases_observation
  ON aircraft_identity_resolution_cases (observation_id, resolution_scope);

CREATE TABLE aircraft_identity_resolution_candidates (
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

CREATE TABLE aircraft_identity_decisions (
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

CREATE INDEX idx_aircraft_identity_decisions_case
  ON aircraft_identity_decisions (resolution_case_id, decision_status);

CREATE TABLE aircraft_identity_decision_claims (
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

CREATE TABLE aircraft_reference_profile_proposals (
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
-- approved-by-construction catalog. The legacy engine_models/propeller_models
-- tables are intentionally not copied: their existing rows remain outside the
-- trusted reference-profile boundary until individually evidenced and approved.
CREATE TABLE aircraft_engine_catalog_models (
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

CREATE TABLE aircraft_propeller_catalog_models (
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

CREATE TABLE aircraft_markets (
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

CREATE TABLE aircraft_makes (
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

CREATE TABLE aircraft_model_families (
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

CREATE TABLE aircraft_designations (
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

CREATE TABLE aircraft_make_aliases (
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

CREATE TABLE aircraft_family_aliases (
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

CREATE TABLE aircraft_designation_aliases (
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

CREATE TABLE aircraft_designation_identifiers (
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
CREATE UNIQUE INDEX idx_aircraft_make_aliases_scope
  ON aircraft_make_aliases (
    aircraft_make_id, normalized_alias, coalesce(aircraft_market_id, 0)
  );
CREATE UNIQUE INDEX idx_aircraft_family_aliases_scope
  ON aircraft_family_aliases (
    aircraft_model_family_id, normalized_alias, coalesce(aircraft_market_id, 0)
  );
CREATE UNIQUE INDEX idx_aircraft_designation_aliases_scope
  ON aircraft_designation_aliases (
    aircraft_designation_id, normalized_alias, coalesce(aircraft_market_id, 0)
  );
CREATE UNIQUE INDEX idx_aircraft_designation_identifiers_scope
  ON aircraft_designation_identifiers (
    aircraft_designation_id, authority, identifier_kind,
    normalized_identifier_value, coalesce(aircraft_market_id, 0)
  );

CREATE TABLE aircraft_generations (
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

CREATE TABLE aircraft_generation_designations (
  aircraft_generation_id INTEGER NOT NULL
    REFERENCES aircraft_generations(id) ON DELETE CASCADE,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE CASCADE,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (aircraft_generation_id, aircraft_designation_id)
);

CREATE TABLE aircraft_factory_packages (
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

CREATE TABLE aircraft_package_applicability (
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

CREATE UNIQUE INDEX idx_aircraft_package_applicability_scope
  ON aircraft_package_applicability (
    aircraft_factory_package_id, aircraft_designation_id,
    coalesce(aircraft_generation_id, 0),
    coalesce(valid_from_model_year, 0), coalesce(valid_to_model_year, 0)
  );

CREATE TABLE aircraft_reference_configurations (
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

CREATE UNIQUE INDEX idx_aircraft_reference_config_base_no_generation
  ON aircraft_reference_configurations (aircraft_designation_id)
  WHERE configuration_kind = 'base' AND aircraft_generation_id IS NULL;
CREATE UNIQUE INDEX idx_aircraft_reference_config_base_generation
  ON aircraft_reference_configurations (aircraft_designation_id, aircraft_generation_id)
  WHERE configuration_kind = 'base' AND aircraft_generation_id IS NOT NULL;
CREATE UNIQUE INDEX idx_aircraft_reference_config_tier_no_generation
  ON aircraft_reference_configurations (aircraft_designation_id, tier_package_id)
  WHERE configuration_kind = 'tier' AND aircraft_generation_id IS NULL;
CREATE UNIQUE INDEX idx_aircraft_reference_config_tier_generation
  ON aircraft_reference_configurations (
    aircraft_designation_id, aircraft_generation_id, tier_package_id
  )
  WHERE configuration_kind = 'tier' AND aircraft_generation_id IS NOT NULL;

CREATE TABLE aircraft_serial_number_schemes (
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

CREATE TABLE aircraft_reference_configuration_versions (
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

CREATE INDEX idx_aircraft_reference_versions_lookup
  ON aircraft_reference_configuration_versions (
    aircraft_reference_configuration_id, model_year, publication_state, revision
  );

CREATE TABLE aircraft_reference_applicability_scopes (
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

CREATE INDEX idx_aircraft_reference_scope_market
  ON aircraft_reference_applicability_scopes (
    aircraft_market_id, aircraft_serial_number_scheme_id,
    serial_from_sort_key, serial_to_sort_key
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

CREATE TABLE aircraft_reference_avionics (
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

CREATE TABLE aircraft_reference_engines (
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

CREATE TABLE aircraft_reference_propellers (
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

CREATE TABLE aircraft_feature_definitions (
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

CREATE TABLE aircraft_reference_features (
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

-- Clean component-catalog rows are accepted only when the exact authoritative
-- identifier claim is validated, primary-source, and linked to the matching
-- approved decision. Once accepted they are immutable; corrections create a
-- separately approved catalog identity instead of rewriting history.
CREATE TRIGGER aircraft_engine_catalog_models_require_approval
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

CREATE TRIGGER aircraft_propeller_catalog_models_require_approval
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

CREATE TRIGGER aircraft_engine_catalog_models_immutable_update
BEFORE UPDATE ON aircraft_engine_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved engine catalog models are immutable'); END;
CREATE TRIGGER aircraft_engine_catalog_models_immutable_delete
BEFORE DELETE ON aircraft_engine_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved engine catalog models are immutable'); END;
CREATE TRIGGER aircraft_propeller_catalog_models_immutable_update
BEFORE UPDATE ON aircraft_propeller_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved propeller catalog models are immutable'); END;
CREATE TRIGGER aircraft_propeller_catalog_models_immutable_delete
BEFORE DELETE ON aircraft_propeller_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved propeller catalog models are immutable'); END;

-- Every canonical aircraft identity/configuration row must be backed by one
-- approved decision with at least one validated primary-source identity claim.
CREATE TRIGGER aircraft_makes_require_approval
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

CREATE TRIGGER aircraft_families_require_approval
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

CREATE TRIGGER aircraft_designations_require_approval
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

CREATE TRIGGER aircraft_aliases_require_approval_make
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

CREATE TRIGGER aircraft_aliases_require_approval_family
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

CREATE TRIGGER aircraft_aliases_require_approval_designation
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

CREATE TRIGGER aircraft_identifiers_require_approval
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

CREATE TRIGGER aircraft_generations_require_approval
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

CREATE TRIGGER aircraft_generation_designations_require_approval
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

CREATE TRIGGER aircraft_packages_require_approval
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

CREATE TRIGGER aircraft_package_applicability_require_approval
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

CREATE TRIGGER aircraft_reference_configurations_require_approval
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

CREATE TRIGGER aircraft_feature_definitions_require_approval
BEFORE INSERT ON aircraft_feature_definitions
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions
  WHERE id = NEW.approval_decision_id
    AND decision_status = 'approved' AND decision_action = 'approve_new'
    AND entity_kind = 'feature_definition'
)
BEGIN SELECT RAISE(ABORT, 'feature definition requires an approved decision'); END;

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

-- Profile children may only be assembled while the parent is building.
CREATE TRIGGER aircraft_reference_scope_building_insert
BEFORE INSERT ON aircraft_reference_applicability_scopes
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
BEGIN SELECT RAISE(ABORT, 'reference profile children require a building version'); END;
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
CREATE TRIGGER aircraft_reference_avionics_building_insert
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
CREATE TRIGGER aircraft_reference_engines_building_insert
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
CREATE TRIGGER aircraft_reference_propellers_building_insert
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
CREATE TRIGGER aircraft_reference_features_building_insert
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
CREATE TRIGGER aircraft_reference_scope_immutable_update
BEFORE UPDATE ON aircraft_reference_applicability_scopes
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_price_immutable_update
BEFORE UPDATE ON aircraft_reference_prices
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_avionics_immutable_update
BEFORE UPDATE ON aircraft_reference_avionics
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_engines_immutable_update
BEFORE UPDATE ON aircraft_reference_engines
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_propellers_immutable_update
BEFORE UPDATE ON aircraft_reference_propellers
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_features_immutable_update
BEFORE UPDATE ON aircraft_reference_features
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;

CREATE TRIGGER aircraft_reference_scope_immutable_delete
BEFORE DELETE ON aircraft_reference_applicability_scopes
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_price_immutable_delete
BEFORE DELETE ON aircraft_reference_prices
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_avionics_immutable_delete
BEFORE DELETE ON aircraft_reference_avionics
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_engines_immutable_delete
BEFORE DELETE ON aircraft_reference_engines
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_propellers_immutable_delete
BEFORE DELETE ON aircraft_reference_propellers
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_features_immutable_delete
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

CREATE TRIGGER aircraft_reference_versions_immutable
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

-- Privacy-minimized, target-scoped FAA releasable-registry snapshots. Only
-- current MASTER rows requested by listings and the ACFTREF/ENGINE rows
-- reachable from those matches are retained. Registrant, address, other-name,
-- Mode-S, and all unrelated registry rows are intentionally absent.
CREATE TABLE faa_registry_snapshots (
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

CREATE INDEX idx_faa_registry_snapshots_current
  ON faa_registry_snapshots (snapshot_date DESC, id DESC);

CREATE TRIGGER faa_registry_snapshots_require_exact_evidence
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

CREATE TABLE faa_registry_aircraft (
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

CREATE INDEX idx_faa_registry_aircraft_code
  ON faa_registry_aircraft (snapshot_id, aircraft_code);
CREATE INDEX idx_faa_registry_engine_code
  ON faa_registry_aircraft (snapshot_id, engine_code);

CREATE TABLE faa_registry_aircraft_references (
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

CREATE TABLE faa_registry_engine_references (
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

-- Every target gets an explicit matched/absent row. No coverage row means the
-- N-number was not scanned in this target-scoped snapshot.
CREATE TABLE faa_registry_coverage (
  snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  n_number TEXT NOT NULL,
  lookup_status TEXT NOT NULL CHECK (lookup_status IN ('matched', 'absent')),
  PRIMARY KEY (snapshot_id, n_number),
  CHECK (substr(n_number, 1, 1) = 'N' AND length(n_number) BETWEEN 2 AND 6)
);

CREATE INDEX idx_faa_registry_coverage_lookup
  ON faa_registry_coverage (n_number, snapshot_id);

CREATE TRIGGER faa_registry_aircraft_references_reachable
BEFORE INSERT ON faa_registry_aircraft_references
WHEN NOT EXISTS (
  SELECT 1 FROM faa_registry_aircraft aircraft
  WHERE aircraft.snapshot_id = NEW.snapshot_id
    AND aircraft.aircraft_code = NEW.aircraft_code
)
BEGIN SELECT RAISE(ABORT, 'FAA aircraft reference must be reachable from a target match'); END;

CREATE TRIGGER faa_registry_engine_references_reachable
BEFORE INSERT ON faa_registry_engine_references
WHEN NOT EXISTS (
  SELECT 1 FROM faa_registry_aircraft aircraft
  WHERE aircraft.snapshot_id = NEW.snapshot_id
    AND aircraft.engine_code = NEW.engine_code
)
BEGIN SELECT RAISE(ABORT, 'FAA engine reference must be reachable from a target match'); END;

CREATE TRIGGER faa_registry_coverage_consistent
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

CREATE TRIGGER faa_registry_snapshots_immutable_update
BEFORE UPDATE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;
CREATE TRIGGER faa_registry_snapshots_immutable_delete
BEFORE DELETE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;
CREATE TRIGGER faa_registry_aircraft_immutable_update
BEFORE UPDATE ON faa_registry_aircraft
BEGIN SELECT RAISE(ABORT, 'FAA registry aircraft are immutable'); END;
CREATE TRIGGER faa_registry_aircraft_immutable_delete
BEFORE DELETE ON faa_registry_aircraft
BEGIN SELECT RAISE(ABORT, 'FAA registry aircraft are immutable'); END;
CREATE TRIGGER faa_registry_aircraft_references_immutable_update
BEFORE UPDATE ON faa_registry_aircraft_references
BEGIN SELECT RAISE(ABORT, 'FAA aircraft references are immutable'); END;
CREATE TRIGGER faa_registry_aircraft_references_immutable_delete
BEFORE DELETE ON faa_registry_aircraft_references
BEGIN SELECT RAISE(ABORT, 'FAA aircraft references are immutable'); END;
CREATE TRIGGER faa_registry_engine_references_immutable_update
BEFORE UPDATE ON faa_registry_engine_references
BEGIN SELECT RAISE(ABORT, 'FAA engine references are immutable'); END;
CREATE TRIGGER faa_registry_engine_references_immutable_delete
BEFORE DELETE ON faa_registry_engine_references
BEGIN SELECT RAISE(ABORT, 'FAA engine references are immutable'); END;
CREATE TRIGGER faa_registry_coverage_immutable_update
BEFORE UPDATE ON faa_registry_coverage
BEGIN SELECT RAISE(ABORT, 'FAA registry coverage is immutable'); END;
CREATE TRIGGER faa_registry_coverage_immutable_delete
BEFORE DELETE ON faa_registry_coverage
BEGIN SELECT RAISE(ABORT, 'FAA registry coverage is immutable'); END;

COMMIT;
