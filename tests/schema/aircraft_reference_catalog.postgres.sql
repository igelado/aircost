-- Positive PostgreSQL publication fixture. Load schema/postgres.sql first.
INSERT INTO users (email, display_name, auth_subject)
VALUES ('schema@test', 'Schema Test', 'schema-test');

INSERT INTO curation_evidence_sources (
  source_url, source_title, source_domain, source_tier, retrieved_at
) VALUES (
  'https://manufacturer.test/manual', 'Factory manual',
  'manufacturer.test', 'manufacturer_primary', '2026-07-21'
);

INSERT INTO curation_evidence_claims (
  evidence_source_id, claim_kind, subject_text, predicate_text, object_text,
  quoted_evidence, validation_status, validated_at
) VALUES (
  1, 'identity', 'aircraft', 'identifies', 'catalog entity',
  'Authoritative factory identity and configuration evidence.',
  'validated', '2026-07-21'
);

INSERT INTO aircraft_identity_observations (
  observed_make, observed_family, observed_designation, observed_generation,
  observed_package, model_year, exact_source_evidence, observation_sha256
) VALUES (
  'Cirrus', 'SR22', 'SR22', 'G6', 'GTS', 2020,
  '2020 Cirrus SR22 G6 GTS', 'observation-1'
);

INSERT INTO aircraft_identity_resolution_cases (
  observation_id, resolution_scope, job_fingerprint, catalog_revision
) VALUES (1, 'reference_profile', 'job-1', 'catalog-1');

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
) VALUES
  (1, 'make', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'family', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'designation', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'generation', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'generation_designation', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'package', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'package_applicability', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'reference_configuration', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'reference_profile', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'engine_model', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21'),
  (1, 'propeller_model', 'approve_new', 'approved', '{}', '{}', TRUE, 'approved', '2026-07-21');

INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT id, 1, 'identity' FROM aircraft_identity_decisions;

INSERT INTO aircraft_engine_catalog_models (
  manufacturer_name, normalized_manufacturer_name,
  model_name, normalized_model_name,
  identifier_authority, normalized_identifier_authority,
  identifier_kind, authoritative_identifier,
  normalized_authoritative_identifier,
  approval_decision_id, identity_evidence_claim_id
) VALUES (
  'Continental', 'continental', 'IO-550-N', 'io-550-n',
  'Continental', 'continental', 'manufacturer_model_code',
  'IO-550-N', 'io-550-n', 10, 1
);

INSERT INTO aircraft_propeller_catalog_models (
  manufacturer_name, normalized_manufacturer_name,
  model_name, normalized_model_name,
  identifier_authority, normalized_identifier_authority,
  identifier_kind, authoritative_identifier,
  normalized_authoritative_identifier,
  approval_decision_id, identity_evidence_claim_id
) VALUES (
  'Hartzell', 'hartzell', 'PHC-J3YF-1N', 'phc-j3yf-1n',
  'Hartzell', 'hartzell', 'manufacturer_model_code',
  'PHC-J3YF-1N', 'phc-j3yf-1n', 11, 1
);

INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id)
VALUES ('Cirrus', 'cirrus', 1);

INSERT INTO aircraft_model_families (
  aircraft_make_id, name, normalized_name, approval_decision_id
) VALUES (1, 'SR22', 'sr22', 2);

INSERT INTO aircraft_designations (
  aircraft_model_family_id, official_designation,
  normalized_official_designation, display_name, approval_decision_id
) VALUES (1, 'SR22', 'sr22', 'SR22', 3);

INSERT INTO aircraft_generations (
  aircraft_model_family_id, name, normalized_name, ordinal, approval_decision_id
) VALUES (1, 'G6', 'g6', 6, 4);

INSERT INTO aircraft_generation_designations (
  aircraft_generation_id, aircraft_designation_id, approval_decision_id
) VALUES (1, 1, 5);

INSERT INTO aircraft_factory_packages (
  aircraft_model_family_id, name, normalized_name, package_kind,
  exclusivity_group, approval_decision_id
) VALUES (1, 'GTS', 'gts', 'trim_tier', 'trim', 6);

INSERT INTO aircraft_package_applicability (
  aircraft_factory_package_id, aircraft_designation_id,
  aircraft_generation_id, valid_from_model_year, approval_decision_id
) VALUES (1, 1, 1, 2017, 7);

INSERT INTO aircraft_reference_configurations (
  aircraft_model_family_id, aircraft_designation_id, aircraft_generation_id,
  tier_package_id, configuration_kind, display_name, approval_decision_id
) VALUES (1, 1, 1, 1, 'tier', 'SR22 G6 GTS', 8);

INSERT INTO aircraft_reference_configuration_versions (
  aircraft_reference_configuration_id, model_year, revision, approval_decision_id
) VALUES (1, 2020, 1, 9);

INSERT INTO aircraft_reference_applicability_scopes (
  aircraft_reference_configuration_version_id, aircraft_market_id,
  applies_to_all_serials, evidence_claim_id
) VALUES (1, 1, TRUE, 1);

INSERT INTO aircraft_reference_prices (
  aircraft_reference_configuration_version_id, price_kind, amount, currency,
  price_reference_year, evidence_kind, evidence_claim_id
) VALUES (1, 'equipped_msrp', 779900, 'USD', 2020, 'direct_model_year', 1);

INSERT INTO aircraft_reference_engines (
  aircraft_reference_configuration_version_id,
  aircraft_engine_catalog_model_id, quantity, equipment_role,
  evidence_claim_id
) VALUES (1, 1, 1, 'standard', 1);

INSERT INTO aircraft_reference_propellers (
  aircraft_reference_configuration_version_id,
  aircraft_propeller_catalog_model_id, quantity, equipment_role,
  evidence_claim_id
) VALUES (1, 1, 1, 'standard', 1);

UPDATE aircraft_reference_configuration_versions
SET publication_state = 'published', published_at = '2026-07-21'
WHERE id = 1;
