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

INSERT INTO curation_evidence_claims (
  evidence_source_id, claim_kind, subject_text, predicate_text, object_text,
  quoted_evidence, validation_status, validated_at
) VALUES
  (1, 'applicability', 'aircraft', 'applies in', 'GLOBAL',
    'Authoritative factory source defines global applicability.',
    'validated', '2026-07-21'),
  (1, 'price', 'aircraft', 'equipped MSRP', '779900 USD',
    'Authoritative factory source states the equipped MSRP.',
    'validated', '2026-07-21'),
  (1, 'standard_equipment', 'aircraft', 'includes', 'factory equipment',
    'Authoritative factory source defines the complete standard equipment.',
    'validated', '2026-07-21');

INSERT INTO curation_evidence_sources (
  source_url, source_title, source_domain, source_tier, retrieved_at
) VALUES (
  'https://www.bls.gov/cpi/test-series', 'Official CPI test series',
  'bls.gov', 'regulator_primary', '2026-08-19'
);

INSERT INTO curation_evidence_claims (
  evidence_source_id, claim_kind, subject_text, predicate_text, object_text,
  quoted_evidence, validation_status, validated_at
) VALUES (
  2, 'price', 'official CPI test series', 'reports index values',
  '2019=250; 2026=300',
  'Official government series reports index values 250 and 300.',
  'validated', '2026-08-19'
);

INSERT INTO official_dollar_normalization_facts (
  source_year, target_year, index_series,
  source_index_value, target_index_value, normalization_factor,
  evidence_claim_id
) VALUES (
  2019, 2026, 'BLS CPI test series', 250, 300, 1.2, 5
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
) VALUES (1, 1, TRUE, 2);

INSERT INTO aircraft_reference_prices (
  aircraft_reference_configuration_version_id, price_kind, amount, currency,
  price_reference_year, configuration_basis, evidence_kind, evidence_claim_id
) VALUES (1, 'equipped_msrp', 779900, 'USD', 2019,
  'full_standard_configuration', 'direct_model_year', 3);

DO $duplicate_price$
BEGIN
  BEGIN
    INSERT INTO aircraft_reference_prices (
      aircraft_reference_configuration_version_id, price_kind, amount,
      currency, price_reference_year, configuration_basis, evidence_kind,
      evidence_claim_id
    ) VALUES (1, 'equipped_msrp', 789900, 'USD', 2019,
      'full_standard_configuration', 'direct_model_year', 3);
    RAISE EXCEPTION 'duplicate equipped MSRP unexpectedly accepted';
  EXCEPTION WHEN unique_violation THEN
    NULL;
  END;
END
$duplicate_price$;

INSERT INTO aircraft_reference_engines (
  aircraft_reference_configuration_version_id,
  aircraft_engine_catalog_model_id, quantity, equipment_role,
  evidence_claim_id
) VALUES (1, 1, 1, 'standard', 4);

INSERT INTO aircraft_reference_propellers (
  aircraft_reference_configuration_version_id,
  aircraft_propeller_catalog_model_id, quantity, equipment_role,
  evidence_claim_id
) VALUES (1, 1, 1, 'standard', 4);

INSERT INTO aircraft_reference_fact_set_attestations (
  aircraft_reference_configuration_version_id, fact_set_kind, evidence_claim_id
) VALUES
  (1, 'avionics', 4), (1, 'engines', 4),
  (1, 'propellers', 4), (1, 'features', 4);

UPDATE aircraft_reference_configuration_versions
SET publication_state = 'published', published_at = '2026-07-21'
WHERE id = 1;

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
) VALUES (
  1, 'reference_profile', 'approve_new', 'approved', '{}', '{}', TRUE,
  'replacement rollback test', '2026-08-19'
);
INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
) VALUES (12, 1, 'identity');

DO $test$
DECLARE
  failure_message TEXT;
BEGIN
  BEGIN
    INSERT INTO aircraft_reference_configuration_versions (
      aircraft_reference_configuration_id, model_year, revision,
      supersedes_version_id, approval_decision_id
    ) VALUES (1, 2020, 2, 1, 12);
    UPDATE aircraft_reference_configuration_versions
    SET publication_state = 'superseded', superseded_at = CURRENT_TIMESTAMP
    WHERE id = 1;
    UPDATE aircraft_reference_configuration_versions
    SET publication_state = 'published', published_at = CURRENT_TIMESTAMP
    WHERE revision = 2;
    RAISE SQLSTATE 'P0002' USING MESSAGE =
      'incomplete replacement publication unexpectedly succeeded';
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS failure_message = MESSAGE_TEXT;
    IF failure_message <> 'published reference profile requires applicability' THEN
      RAISE;
    END IF;
  END;
  IF NOT EXISTS (
    SELECT 1 FROM aircraft_reference_configuration_versions
    WHERE id = 1 AND publication_state = 'published'
  ) OR EXISTS (
    SELECT 1 FROM aircraft_reference_configuration_versions WHERE revision = 2
  ) THEN
    RAISE EXCEPTION 'failed replacement did not roll back atomically';
  END IF;
END
$test$;

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
) VALUES (
  1, 'reference_profile', 'approve_new', 'approved', '{}', '{}', TRUE,
  'GLOBAL versus US overlap guard', '2026-08-19'
);
INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT id, 1, 'identity' FROM aircraft_identity_decisions
WHERE rationale = 'GLOBAL versus US overlap guard';
INSERT INTO aircraft_reference_configuration_versions (
  aircraft_reference_configuration_id, model_year, revision,
  supersedes_version_id, approval_decision_id
)
SELECT 1, 2020, 2, 1, id FROM aircraft_identity_decisions
WHERE rationale = 'GLOBAL versus US overlap guard';
INSERT INTO aircraft_reference_applicability_scopes (
  aircraft_reference_configuration_version_id, aircraft_market_id,
  applies_to_all_serials, evidence_claim_id
)
SELECT version.id, market.id, TRUE, 2
FROM aircraft_reference_configuration_versions version
JOIN aircraft_identity_decisions decision
  ON decision.id = version.approval_decision_id
CROSS JOIN aircraft_markets market
WHERE decision.rationale = 'GLOBAL versus US overlap guard'
  AND market.code = 'US';
INSERT INTO aircraft_reference_prices (
  aircraft_reference_configuration_version_id, price_kind, amount, currency,
  price_reference_year, configuration_basis, evidence_kind, evidence_claim_id
) SELECT
  version.id, 'equipped_msrp', 789900, 'USD', 2020,
  'full_standard_configuration', 'direct_model_year', 3
FROM aircraft_reference_configuration_versions version
JOIN aircraft_identity_decisions decision
  ON decision.id = version.approval_decision_id
WHERE decision.rationale = 'GLOBAL versus US overlap guard';
INSERT INTO aircraft_reference_fact_set_attestations (
  aircraft_reference_configuration_version_id, fact_set_kind,
  evidence_claim_id
) SELECT version.id, fact_set.kind, 4
FROM aircraft_reference_configuration_versions version
JOIN aircraft_identity_decisions decision
  ON decision.id = version.approval_decision_id
CROSS JOIN (VALUES ('avionics'), ('engines'), ('propellers'), ('features'))
  AS fact_set(kind)
WHERE decision.rationale = 'GLOBAL versus US overlap guard';
DO $global_overlap$
DECLARE
  failure_message TEXT;
BEGIN
  BEGIN
    UPDATE aircraft_reference_configuration_versions
    SET publication_state = 'published', published_at = '2026-08-19'
    WHERE approval_decision_id = (
      SELECT id FROM aircraft_identity_decisions
      WHERE rationale = 'GLOBAL versus US overlap guard'
    );
    RAISE EXCEPTION 'US scope unexpectedly published over a GLOBAL scope';
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS failure_message = MESSAGE_TEXT;
    IF failure_message <>
       'reference profile applicability overlaps an existing published version'
    THEN
      RAISE;
    END IF;
  END;
END
$global_overlap$;

DO $namespace_contract$
DECLARE
  routine_names TEXT[] := ARRAY[
    'aircraft_serial_natural_sort_key',
    'validate_aircraft_serial_scheme_ordering',
    'prevent_referenced_avionics_catalog_downgrade',
    'invalidate_listing_avionics_authorization_for_capture',
    'validate_aircraft_valuation_compatibility_projection',
    'require_aircraft_catalog_approval',
    'validate_aircraft_reference_version_insert',
    'validate_faa_reference_reachability',
    'preserve_assigned_aircraft_applicability',
    'prevent_new_unresolved_aircraft_dimension',
    'validate_official_dollar_normalization_fact',
    'prevent_official_dollar_normalization_mutation',
    'validate_aircraft_reference_child_insert',
    'prevent_aircraft_reference_fact_mutation',
    'validate_aircraft_reference_version_update'
  ];
BEGIN
  IF 15 <> (
    SELECT COUNT(*)
    FROM pg_catalog.pg_proc routine
    JOIN pg_catalog.pg_namespace namespace
      ON namespace.oid = routine.pronamespace
    WHERE namespace.nspname = 'public'
      AND routine.proname = ANY (routine_names)
  ) THEN
    RAISE EXCEPTION 'cutover routines are not uniquely installed in public';
  END IF;
  IF EXISTS (
    SELECT 1
    FROM pg_catalog.pg_proc routine
    JOIN pg_catalog.pg_namespace namespace
      ON namespace.oid = routine.pronamespace
    WHERE namespace.nspname = 'public'
      AND routine.proname = ANY (routine_names)
      AND routine.proconfig IS DISTINCT FROM
            ARRAY['search_path=pg_catalog']::TEXT[]
  ) THEN
    RAISE EXCEPTION 'cutover routine lacks the exact pg_catalog search_path';
  END IF;
  IF EXISTS (
    SELECT 1
    FROM pg_catalog.pg_proc routine
    JOIN pg_catalog.pg_namespace namespace
      ON namespace.oid = routine.pronamespace
    WHERE namespace.nspname = 'public'
      AND routine.proname = ANY (routine_names)
      AND routine.proname <> 'aircraft_serial_natural_sort_key'
      AND NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_trigger trigger
        WHERE trigger.tgfoid = routine.oid
          AND NOT trigger.tgisinternal
      )
  ) THEN
    RAISE EXCEPTION 'cutover trigger routine is not installed on a relation';
  END IF;
  IF EXISTS (
    SELECT 1
    FROM pg_catalog.pg_trigger trigger
    JOIN pg_catalog.pg_proc routine ON routine.oid = trigger.tgfoid
    JOIN pg_catalog.pg_namespace routine_namespace
      ON routine_namespace.oid = routine.pronamespace
    JOIN pg_catalog.pg_class target ON target.oid = trigger.tgrelid
    JOIN pg_catalog.pg_namespace target_namespace
      ON target_namespace.oid = target.relnamespace
    WHERE routine.proname = ANY (routine_names)
      AND NOT trigger.tgisinternal
      AND (
        routine_namespace.nspname <> 'public'
        OR target_namespace.nspname <> 'public'
      )
  ) THEN
    RAISE EXCEPTION 'cutover trigger function or target is outside public';
  END IF;
  IF EXISTS (
    SELECT 1
    FROM pg_catalog.pg_proc routine
    JOIN pg_catalog.pg_namespace namespace
      ON namespace.oid = routine.pronamespace
    WHERE namespace.nspname = 'public'
      AND routine.proname = ANY (routine_names)
      AND pg_catalog.pg_get_functiondef(routine.oid) ~*
        '(FROM|JOIN|UPDATE|USING|TABLE)[[:space:]]+(aircraft_|avionics_|curation_|faa_|plugin_|rental_|official_)'
  ) THEN
    RAISE EXCEPTION 'cutover routine contains an unqualified app relation';
  END IF;
  IF pg_catalog.strpos(
    pg_catalog.pg_get_functiondef(
      'public.validate_aircraft_reference_child_insert()'::pg_catalog.regprocedure
    ),
    'public.aircraft_serial_natural_sort_key'
  ) = 0 THEN
    RAISE EXCEPTION 'reference child trigger does not call the public serial helper';
  END IF;
END
$namespace_contract$;

CREATE SCHEMA reference_catalog_shadow;
CREATE FUNCTION reference_catalog_shadow.aircraft_serial_natural_sort_key(TEXT)
RETURNS TEXT LANGUAGE SQL IMMUTABLE AS $$ SELECT 'shadow'::TEXT $$;
CREATE TABLE reference_catalog_shadow.aircraft_reference_configuration_versions (
  id BIGINT PRIMARY KEY,
  publication_state TEXT NOT NULL
);
INSERT INTO reference_catalog_shadow.aircraft_reference_configuration_versions
  (id, publication_state)
VALUES (1, 'building');
SET search_path = reference_catalog_shadow, public, pg_catalog;
DO $shadow_test$
DECLARE
  failure_message TEXT;
BEGIN
  IF public.aircraft_serial_natural_sort_key('S9') = 'shadow' THEN
    RAISE EXCEPTION 'public serial helper resolved to the shadow routine';
  END IF;
  BEGIN
    INSERT INTO public.aircraft_reference_fact_set_attestations (
      aircraft_reference_configuration_version_id, fact_set_kind,
      evidence_claim_id
    ) VALUES (1, 'avionics', 1);
    RAISE SQLSTATE 'P0002' USING MESSAGE =
      'reference child trigger read the shadow parent relation';
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS failure_message = MESSAGE_TEXT;
    IF failure_message <> 'reference profile children require a building version' THEN
      RAISE;
    END IF;
  END;
  BEGIN
    DELETE FROM public.aircraft_reference_fact_set_attestations
    WHERE aircraft_reference_configuration_version_id = 1
      AND fact_set_kind = 'features';
    RAISE SQLSTATE 'P0002' USING MESSAGE =
      'reference immutability trigger read the shadow parent relation';
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS failure_message = MESSAGE_TEXT;
    IF failure_message <> 'published reference profile facts are immutable' THEN
      RAISE;
    END IF;
  END;
END
$shadow_test$;
RESET search_path;
DROP SCHEMA reference_catalog_shadow CASCADE;

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
) VALUES
  (1, 'serial_scheme', 'approve_new', 'approved', '{}', '{}', TRUE,
    'natural serial scheme A', '2026-08-19'),
  (1, 'serial_scheme', 'approve_new', 'approved', '{}', '{}', TRUE,
    'natural serial scheme B', '2026-08-19');

INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT id, 1, 'identity'
FROM aircraft_identity_decisions
WHERE rationale IN ('natural serial scheme A', 'natural serial scheme B');

INSERT INTO aircraft_serial_number_schemes (
  aircraft_make_id, name, normalization_version,
  validation_pattern, approval_decision_id
)
SELECT 1, 'Natural A', 'natural_alphanumeric_segments_v1',
  '^[A-Z]+[0-9]+$', id
FROM aircraft_identity_decisions WHERE rationale = 'natural serial scheme A';

INSERT INTO aircraft_serial_number_schemes (
  aircraft_make_id, name, normalization_version,
  validation_pattern, approval_decision_id
)
SELECT 1, 'Natural B', 'natural_alphanumeric_segments_v1',
  '^[A-Z]+[0-9]+$', id
FROM aircraft_identity_decisions WHERE rationale = 'natural serial scheme B';

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
) VALUES (
  1, 'reference_profile', 'approve_new', 'approved', '{}', '{}', TRUE,
  'direct serial key guard', '2026-08-19'
);
INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT id, 1, 'identity' FROM aircraft_identity_decisions
WHERE rationale = 'direct serial key guard';
INSERT INTO aircraft_reference_configuration_versions (
  aircraft_reference_configuration_id, model_year, revision,
  approval_decision_id
)
SELECT 1, 2022, 1, id FROM aircraft_identity_decisions
WHERE rationale = 'direct serial key guard';

DO $serial_guard$
DECLARE
  failure_message TEXT;
BEGIN
  BEGIN
    INSERT INTO aircraft_reference_applicability_scopes (
      aircraft_reference_configuration_version_id, aircraft_market_id,
      applies_to_all_serials, aircraft_serial_number_scheme_id,
      serial_prefix, serial_from_display, serial_to_display,
      serial_from_sort_key, serial_to_sort_key, evidence_claim_id
    ) VALUES (
      (SELECT id FROM aircraft_reference_configuration_versions WHERE model_year = 2022),
      1, FALSE,
      (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
      'SR', 'SR100', 'SR199',
      '0110130020000000031000000000310000',
      '011013120020000000031990000000319900', 1
    );
    RAISE EXCEPTION 'caller-defined serial key domain unexpectedly succeeded';
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS failure_message = MESSAGE_TEXT;
    IF failure_message <> 'reference serial applicability requires canonical sort keys' THEN
      RAISE;
    END IF;
  END;
  BEGIN
    INSERT INTO aircraft_reference_applicability_scopes (
      aircraft_reference_configuration_version_id, aircraft_market_id,
      applies_to_all_serials, aircraft_serial_number_scheme_id,
      serial_prefix, serial_from_display, serial_to_display,
      serial_from_sort_key, serial_to_sort_key, evidence_claim_id
    ) VALUES (
      (SELECT id FROM aircraft_reference_configuration_versions WHERE model_year = 2022),
      1, FALSE,
      (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
      'ZZ', 'SR100', 'SR199',
      '011013120020000000031000000000310000',
      '011013120020000000031990000000319900', 1
    );
    RAISE EXCEPTION 'unrelated serial prefix unexpectedly succeeded';
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS failure_message = MESSAGE_TEXT;
    IF failure_message <> 'reference serial applicability requires canonical sort keys' THEN
      RAISE;
    END IF;
  END;
END
$serial_guard$;

CREATE OR REPLACE FUNCTION pg_temp.assert_reference_serial_pair(
  case_label TEXT,
  left_all BOOLEAN, left_scheme BIGINT, left_prefix TEXT,
  left_from_display TEXT, left_to_display TEXT,
  left_from_key TEXT, left_to_key TEXT,
  right_all BOOLEAN, right_scheme BIGINT, right_prefix TEXT,
  right_from_display TEXT, right_to_display TEXT,
  right_from_key TEXT, right_to_key TEXT,
  expects_overlap BOOLEAN
) RETURNS VOID
LANGUAGE plpgsql
AS $serial_test$
DECLARE
  profile_decision_id BIGINT;
  version_id BIGINT;
  failure_message TEXT;
BEGIN
  BEGIN
    INSERT INTO aircraft_identity_decisions (
      resolution_case_id, entity_kind, decision_action, decision_status,
      decision_payload_json, deterministic_validation_json,
      deterministic_validation_passed, rationale, decided_at
    ) VALUES (
      1, 'reference_profile', 'approve_new', 'approved', '{}', '{}', TRUE,
      case_label, '2026-08-19'
    ) RETURNING id INTO profile_decision_id;
    INSERT INTO aircraft_identity_decision_claims (
      decision_id, evidence_claim_id, evidence_role
    ) VALUES (profile_decision_id, 1, 'identity');
    INSERT INTO aircraft_reference_configuration_versions (
      aircraft_reference_configuration_id, model_year, revision,
      approval_decision_id
    ) VALUES (1, 2021, 1, profile_decision_id)
    RETURNING id INTO version_id;
    INSERT INTO aircraft_reference_applicability_scopes (
      aircraft_reference_configuration_version_id, aircraft_market_id,
      applies_to_all_serials, aircraft_serial_number_scheme_id,
      serial_prefix, serial_from_display, serial_to_display,
      serial_from_sort_key, serial_to_sort_key, evidence_claim_id
    ) VALUES
      (version_id, 1, left_all, left_scheme, left_prefix,
        left_from_display, left_to_display, left_from_key, left_to_key, 2),
      (version_id, 1, right_all, right_scheme, right_prefix,
        right_from_display, right_to_display, right_from_key, right_to_key, 2);
    INSERT INTO aircraft_reference_prices (
      aircraft_reference_configuration_version_id, price_kind, amount, currency,
      price_reference_year, configuration_basis, evidence_kind, evidence_claim_id
    ) VALUES (
      version_id, 'equipped_msrp', 799900, 'USD', 2021,
      'full_standard_configuration', 'direct_model_year', 3
    );
    INSERT INTO aircraft_reference_fact_set_attestations (
      aircraft_reference_configuration_version_id, fact_set_kind,
      evidence_claim_id
    ) VALUES
      (version_id, 'avionics', 4), (version_id, 'engines', 4),
      (version_id, 'propellers', 4), (version_id, 'features', 4);
    UPDATE aircraft_reference_configuration_versions
    SET publication_state = 'published', published_at = '2026-08-19'
    WHERE id = version_id;
    IF expects_overlap THEN
      RAISE EXCEPTION 'overlapping serial scopes unexpectedly published';
    END IF;
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS failure_message = MESSAGE_TEXT;
    IF NOT expects_overlap
      OR failure_message <> 'reference profile contains overlapping applicability scopes'
    THEN
      RAISE;
    END IF;
    RETURN;
  END;
END
$serial_test$;

SELECT pg_temp.assert_reference_serial_pair(
  'overlap across S and SR prefixes',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
  'S', 'S100', 'SR200',
  '0110130020000000031000000000310000',
  '011013120020000000032000000000320000',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
  'SR', 'SR100', 'SR300',
  '011013120020000000031000000000310000',
  '011013120020000000033000000000330000',
  TRUE
);
SELECT pg_temp.assert_reference_serial_pair(
  'overlap across null and SR prefixes',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
  NULL, 'S100', 'SZ999',
  '0110130020000000031000000000310000',
  '0110131A0020000000039990000000399900',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
  'SR', 'SR100', 'SR200',
  '011013120020000000031000000000310000',
  '011013120020000000032000000000320000',
  TRUE
);
SELECT pg_temp.assert_reference_serial_pair(
  'overlap across serial schemes',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
  'SR', 'SR100', 'SR200',
  '011013120020000000031000000000310000',
  '011013120020000000032000000000320000',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural B'),
  'SR', 'SR100', 'SR200',
  '011013120020000000031000000000310000',
  '011013120020000000032000000000320000',
  TRUE
);
SELECT pg_temp.assert_reference_serial_pair(
  'overlap at inclusive serial boundary',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
  'SR', 'SR100', 'SR200',
  '011013120020000000031000000000310000',
  '011013120020000000032000000000320000',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
  'SR', 'SR200', 'SR300',
  '011013120020000000032000000000320000',
  '011013120020000000033000000000330000',
  TRUE
);
SELECT pg_temp.assert_reference_serial_pair(
  'all serials overlaps bounded serials',
  TRUE, NULL, NULL, NULL, NULL, NULL, NULL,
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
  'SR', 'SR100', 'SR200',
  '011013120020000000031000000000310000',
  '011013120020000000032000000000320000',
  TRUE
);
SELECT pg_temp.assert_reference_serial_pair(
  'disjoint adjacent serial ranges',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural A'),
  'SR', 'SR100', 'SR199',
  '011013120020000000031000000000310000',
  '011013120020000000031990000000319900',
  FALSE, (SELECT id FROM aircraft_serial_number_schemes WHERE name = 'Natural B'),
  'SR', 'SR200', 'SR300',
  '011013120020000000032000000000320000',
  '011013120020000000033000000000330000',
  FALSE
);

DO $serial_test$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM aircraft_reference_configuration_versions
    WHERE model_year = 2021 AND publication_state = 'published'
  ) THEN
    RAISE EXCEPTION 'disjoint serial ranges did not publish';
  END IF;
END
$serial_test$;

DO $test$
DECLARE
  failure_message TEXT;
BEGIN
  BEGIN
    INSERT INTO aircraft_reference_configuration_versions (
      aircraft_reference_configuration_id, model_year, revision,
      supersedes_version_id, approval_decision_id
    ) VALUES (1, 2020, 3, 1, 12);
    RAISE SQLSTATE 'P0002' USING MESSAGE =
      'skipped reference revision unexpectedly succeeded';
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS failure_message = MESSAGE_TEXT;
    IF failure_message <>
      'reference profile predecessor must be the exact published prior revision of the same configuration/year'
    THEN
      RAISE;
    END IF;
  END;
END
$test$;

DO $test$
DECLARE
  failure_message TEXT;
BEGIN
  BEGIN
    UPDATE official_dollar_normalization_facts
    SET normalization_factor = 1
    WHERE source_year = 2019 AND target_year = 2026;
    RAISE SQLSTATE 'P0002' USING MESSAGE =
      'official dollar-normalization mutation unexpectedly succeeded';
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS failure_message = MESSAGE_TEXT;
    IF failure_message <> 'official dollar normalization facts are immutable' THEN
      RAISE;
    END IF;
  END;
END
$test$;
