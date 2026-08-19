-- The reference catalog is the only aircraft configuration/value authority.
-- Replace surviving functions first so PostgreSQL releases their dependencies
-- on the relations removed below.
CREATE OR REPLACE FUNCTION prevent_referenced_avionics_catalog_downgrade()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NEW.catalog_status <> 'approved' AND (
    EXISTS (
      SELECT 1
      FROM aircraft_sale_listing_avionics listing_link
      WHERE listing_link.avionics_model_id = OLD.id
         OR listing_link.replaces_avionics_model_id = OLD.id
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
  ) THEN
    RAISE EXCEPTION 'referenced avionics catalog entry cannot be unapproved';
  END IF;
  RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION
  invalidate_listing_avionics_authorization_for_capture()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations authorization_row
  USING aircraft_sale_listing_avionics link
  WHERE link.id = authorization_row.listing_link_id
    AND authorization_row.evidence_capture_sha256 = OLD.rendered_html_sha256
    AND link.aircraft_sale_listing_id = OLD.canonical_listing_id
    AND length(BTRIM(COALESCE(link.source_notes, ''))) > 0
    AND position(link.source_notes IN OLD.rendered_html) > 0
    AND NOT EXISTS (
      SELECT 1 FROM plugin_submissions retained_capture
      WHERE retained_capture.canonical_listing_id =
              link.aircraft_sale_listing_id
        AND retained_capture.rendered_html_sha256 =
              authorization_row.evidence_capture_sha256
        AND position(link.source_notes IN retained_capture.rendered_html) > 0
    );
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION validate_aircraft_valuation_compatibility_projection()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM aircraft_valuation_projection_transitions transition
    JOIN aircraft_sale_listing_identity_assignments assignment
      ON assignment.id = transition.identity_assignment_id
     AND assignment.aircraft_sale_listing_id = transition.aircraft_sale_listing_id
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
      AND assignment.aircraft_generation_id IS NOT DISTINCT FROM NEW.aircraft_generation_id
      AND assignment.aircraft_factory_package_id IS NOT DISTINCT FROM NEW.aircraft_factory_package_id
      AND assignment.aircraft_sale_listing_id = NEW.created_from_aircraft_sale_listing_id
      AND assignment.id = NEW.created_from_identity_assignment_id
      AND assignment.identity_decision_id = NEW.identity_decision_id
      AND assignment.identity_evidence_claim_id = NEW.identity_evidence_claim_id
      AND assignment.faa_registry_snapshot_id = NEW.faa_registry_snapshot_id
      AND assignment.faa_n_number = NEW.faa_n_number
      AND assignment.faa_source_record_sha256 = NEW.faa_source_record_sha256
      AND legacy_manufacturer.name = make.name
      AND legacy_manufacturer.normalized_name =
            '__aircost_projection_make_' || make.id::TEXT || '__'
      AND legacy_model.name = family.name
      AND legacy_model.normalized_name =
            '__aircost_projection_family_' || family.id::TEXT || '__'
      AND legacy_variant.name =
        designation.official_designation
        || CASE WHEN generation.id IS NULL THEN '' ELSE ' / ' || generation.name END
        || CASE WHEN package.id IS NULL THEN '' ELSE ' / ' || package.name END
      AND legacy_variant.normalized_name =
        '__aircost_projection_identity_'
        || designation.id::TEXT || '_'
        || coalesce(generation.id, 0)::TEXT || '_'
        || coalesce(package.id, 0)::TEXT || '__'
      AND (
        assignment.aircraft_generation_id IS NULL
        OR EXISTS (
          SELECT 1 FROM aircraft_generation_designations applicability
          WHERE applicability.aircraft_generation_id = assignment.aircraft_generation_id
            AND applicability.aircraft_designation_id = assignment.aircraft_designation_id
        )
      )
      AND (
        assignment.aircraft_factory_package_id IS NULL
        OR EXISTS (
          SELECT 1 FROM aircraft_package_applicability applicability
          WHERE applicability.aircraft_factory_package_id = assignment.aircraft_factory_package_id
            AND applicability.aircraft_designation_id = assignment.aircraft_designation_id
            AND (
              applicability.aircraft_generation_id IS NULL
              OR applicability.aircraft_generation_id IS NOT DISTINCT FROM assignment.aircraft_generation_id
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
  ) THEN
    RAISE EXCEPTION 'aircraft compatibility projection requires the active command, exact copied assignment provenance, and its fresh reserved hierarchy';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER aircraft_model_variant_default_avionics_approved_insert
  ON aircraft_model_variant_default_avionics;
DROP TRIGGER aircraft_model_variant_default_avionics_approved_update
  ON aircraft_model_variant_default_avionics;
DROP TRIGGER aircraft_default_avionics_candidate_admission_guard
  ON aircraft_model_variant_default_avionics;
DROP TRIGGER aircraft_default_avionics_candidate_admission_move
  ON aircraft_model_variant_default_avionics;
DROP TRIGGER projected_aircraft_default_avionics_variant_move
  ON aircraft_model_variant_default_avionics;
DROP TRIGGER aircraft_default_avionics_candidate_active_conflict_insert
  ON aircraft_model_variant_default_avionics_candidates;
DROP TRIGGER aircraft_default_avionics_candidate_claim_immutable
  ON aircraft_model_variant_default_avionics_candidates;
DROP TRIGGER projected_aircraft_spec_variant_move
  ON aircraft_model_spec_versions;
DROP TRIGGER projected_aircraft_price_variant_move
  ON aircraft_model_variant_price_points;

DROP FUNCTION require_approved_default_avionics_model();
DROP FUNCTION reject_active_default_avionics_candidate();
DROP FUNCTION preserve_pending_default_avionics_claim();
DROP FUNCTION require_exact_pending_default_avionics_admission();
DROP FUNCTION move_admitted_default_avionics_candidate();
DROP FUNCTION prevent_projected_aircraft_evidence_variant_move();

DROP TABLE aircraft_model_variant_default_avionics_candidates;
DROP TABLE aircraft_model_variant_default_avionics;
DROP TABLE aircraft_model_variant_price_points;
DROP TABLE aircraft_model_spec_versions;
DROP TABLE depreciation_profile_fit_metadata;
DROP TABLE component_depreciation_profiles;
DROP TABLE depreciation_profiles;

ALTER TABLE aircraft_reference_prices
  ADD COLUMN configuration_basis TEXT NOT NULL DEFAULT 'unknown'
  CHECK (configuration_basis IN (
    'full_standard_configuration', 'base_aircraft_only', 'unknown'
  ));

-- PostgreSQL resolves NEW fields while planning boolean expressions, even when
-- a TG_TABLE_NAME predicate would make the field unreachable. Keep the exact
-- component-identifier check inside its table-specific branch so this shared
-- trigger also works for canonical tables without identity_evidence_claim_id.
CREATE OR REPLACE FUNCTION require_aircraft_catalog_approval()
RETURNS TRIGGER AS $$
DECLARE
  expected_kind TEXT;
  require_claim BOOLEAN := TRUE;
  require_primary BOOLEAN := FALSE;
BEGIN
  CASE TG_TABLE_NAME
    WHEN 'aircraft_engine_catalog_models' THEN expected_kind := 'engine_model'; require_primary := TRUE;
    WHEN 'aircraft_propeller_catalog_models' THEN expected_kind := 'propeller_model'; require_primary := TRUE;
    WHEN 'aircraft_makes' THEN expected_kind := 'make'; require_primary := TRUE;
    WHEN 'aircraft_model_families' THEN expected_kind := 'family'; require_primary := TRUE;
    WHEN 'aircraft_designations' THEN expected_kind := 'designation'; require_primary := TRUE;
    WHEN 'aircraft_make_aliases' THEN expected_kind := 'alias';
    WHEN 'aircraft_family_aliases' THEN expected_kind := 'alias';
    WHEN 'aircraft_designation_aliases' THEN expected_kind := 'alias';
    WHEN 'aircraft_designation_identifiers' THEN expected_kind := 'identifier'; require_primary := TRUE;
    WHEN 'aircraft_generations' THEN expected_kind := 'generation'; require_primary := TRUE;
    WHEN 'aircraft_generation_designations' THEN expected_kind := 'generation_designation'; require_claim := FALSE;
    WHEN 'aircraft_factory_packages' THEN expected_kind := 'package'; require_primary := TRUE;
    WHEN 'aircraft_package_applicability' THEN expected_kind := 'package_applicability'; require_claim := FALSE;
    WHEN 'aircraft_reference_configurations' THEN expected_kind := 'reference_configuration'; require_primary := TRUE;
    WHEN 'aircraft_serial_number_schemes' THEN expected_kind := 'serial_scheme';
    WHEN 'aircraft_feature_definitions' THEN expected_kind := 'feature_definition'; require_claim := FALSE;
    WHEN 'aircraft_reference_configuration_versions' THEN expected_kind := 'reference_profile'; require_primary := TRUE;
    ELSE RAISE EXCEPTION 'unsupported canonical aircraft table %', TG_TABLE_NAME;
  END CASE;

  IF NOT EXISTS (
    SELECT 1
    FROM aircraft_identity_decisions decision
    WHERE decision.id = NEW.approval_decision_id
      AND decision.decision_status = 'approved'
      AND decision.decision_action = 'approve_new'
      AND decision.entity_kind = expected_kind
  ) THEN
    RAISE EXCEPTION '% requires an approved % decision', TG_TABLE_NAME, expected_kind;
  END IF;

  IF require_claim AND NOT EXISTS (
    SELECT 1
    FROM aircraft_identity_decision_claims decision_claim
    JOIN curation_evidence_claims claim
      ON claim.id = decision_claim.evidence_claim_id
    JOIN curation_evidence_sources source
      ON source.id = claim.evidence_source_id
    WHERE decision_claim.decision_id = NEW.approval_decision_id
      AND claim.validation_status = 'validated'
      AND (
        NOT require_primary
        OR source.source_tier IN ('manufacturer_primary', 'regulator_primary')
      )
  ) THEN
    RAISE EXCEPTION '% requires validated evidence for its approved decision', TG_TABLE_NAME;
  END IF;

  IF TG_TABLE_NAME IN (
    'aircraft_engine_catalog_models', 'aircraft_propeller_catalog_models'
  ) THEN
    IF NOT EXISTS (
      SELECT 1
      FROM aircraft_identity_decision_claims decision_claim
      JOIN curation_evidence_claims claim
        ON claim.id = decision_claim.evidence_claim_id
      JOIN curation_evidence_sources source
        ON source.id = claim.evidence_source_id
      WHERE decision_claim.decision_id = NEW.approval_decision_id
        AND decision_claim.evidence_claim_id = NEW.identity_evidence_claim_id
        AND decision_claim.evidence_role IN ('identity', 'specification')
        AND claim.claim_kind IN ('identity', 'specification')
        AND claim.validation_status = 'validated'
        AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
    ) THEN
      RAISE EXCEPTION '% requires its exact primary-source identifier claim', TG_TABLE_NAME;
    END IF;
  END IF;

  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION validate_aircraft_reference_version_insert()
RETURNS TRIGGER AS $$
BEGIN
  IF NEW.publication_state <> 'building' THEN
    RAISE EXCEPTION 'reference profile versions must be assembled in building state';
  END IF;
  IF (NEW.revision = 1) <> (NEW.supersedes_version_id IS NULL) THEN
    RAISE EXCEPTION 'reference profile revisions require their exact predecessor';
  END IF;
  IF NEW.supersedes_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1
    FROM aircraft_reference_configuration_versions previous
    WHERE previous.id = NEW.supersedes_version_id
      AND previous.aircraft_reference_configuration_id = NEW.aircraft_reference_configuration_id
      AND previous.model_year = NEW.model_year
      AND previous.revision = NEW.revision - 1
      AND previous.publication_state = 'published'
  ) THEN
    RAISE EXCEPTION 'reference profile predecessor must be the exact published prior revision of the same configuration/year';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Keep shared trigger bodies table-specific. PostgreSQL's NEW/OLD records only
-- expose columns from the relation that fired the trigger.
CREATE OR REPLACE FUNCTION validate_faa_reference_reachability()
RETURNS TRIGGER AS $$
BEGIN
  IF TG_TABLE_NAME = 'faa_registry_aircraft_references' THEN
    IF NOT EXISTS (
      SELECT 1 FROM faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = NEW.snapshot_id
        AND aircraft.aircraft_code = NEW.aircraft_code
    ) THEN
      RAISE EXCEPTION 'FAA aircraft reference must be reachable from a target match';
    END IF;
  END IF;
  IF TG_TABLE_NAME = 'faa_registry_engine_references' THEN
    IF NOT EXISTS (
      SELECT 1 FROM faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = NEW.snapshot_id
        AND aircraft.engine_code = NEW.engine_code
    ) THEN
      RAISE EXCEPTION 'FAA engine reference must be reachable from a target match';
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION preserve_assigned_aircraft_applicability()
RETURNS TRIGGER AS $$
BEGIN
  IF TG_TABLE_NAME = 'aircraft_generation_designations' THEN
    IF EXISTS (
      SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
      WHERE assignment.aircraft_generation_id = OLD.aircraft_generation_id
        AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
    ) THEN
      RAISE EXCEPTION 'assigned generation/designation applicability is immutable';
    END IF;
  END IF;
  IF TG_TABLE_NAME = 'aircraft_package_applicability' THEN
    IF EXISTS (
      SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
      WHERE assignment.aircraft_factory_package_id = OLD.aircraft_factory_package_id
        AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
        AND (OLD.aircraft_generation_id IS NULL
          OR assignment.aircraft_generation_id = OLD.aircraft_generation_id)
    ) THEN
      RAISE EXCEPTION 'assigned package applicability is immutable';
    END IF;
  END IF;
  RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION prevent_new_unresolved_aircraft_dimension()
RETURNS TRIGGER AS $$
BEGIN
  IF TG_TABLE_NAME = 'aircraft_generation_designations' THEN
    IF EXISTS (
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
    ) THEN
      RAISE EXCEPTION 'adding a generation dimension requires resolving affected ready listing assignments first';
    END IF;
  END IF;
  IF TG_TABLE_NAME = 'aircraft_package_applicability' THEN
    IF EXISTS (
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
    ) THEN
      RAISE EXCEPTION 'adding a trim-tier dimension requires resolving affected ready listing assignments first';
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TABLE aircraft_reference_fact_set_attestations (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  aircraft_reference_configuration_version_id BIGINT NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  fact_set_kind TEXT NOT NULL CHECK (fact_set_kind IN (
    'avionics', 'engines', 'propellers', 'features'
  )),
  evidence_claim_id BIGINT NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, fact_set_kind)
);

CREATE TABLE official_dollar_normalization_facts (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  source_year BIGINT NOT NULL CHECK (source_year BETWEEN 1900 AND 2200),
  target_year BIGINT NOT NULL CHECK (target_year BETWEEN 1900 AND 2200),
  index_series TEXT NOT NULL CHECK (length(BTRIM(index_series)) > 0),
  source_index_value DOUBLE PRECISION NOT NULL CHECK (source_index_value > 0),
  target_index_value DOUBLE PRECISION NOT NULL CHECK (target_index_value > 0),
  normalization_factor DOUBLE PRECISION NOT NULL CHECK (normalization_factor > 0),
  evidence_claim_id BIGINT NOT NULL UNIQUE
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (source_year, target_year),
  CHECK (source_year <> target_year),
  CHECK (
    abs(normalization_factor - (target_index_value / source_index_value))
      <= 0.000000001
  )
);

CREATE OR REPLACE FUNCTION validate_official_dollar_normalization_fact()
RETURNS TRIGGER AS $$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM curation_evidence_claims claim
    JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
    WHERE claim.id = NEW.evidence_claim_id
      AND claim.validation_status = 'validated'
      AND claim.claim_kind IN ('price', 'specification')
      AND source.source_tier = 'regulator_primary'
  ) THEN
    RAISE EXCEPTION 'dollar normalization requires validated official regulator evidence';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER official_dollar_normalization_require_evidence
BEFORE INSERT ON official_dollar_normalization_facts
FOR EACH ROW EXECUTE FUNCTION validate_official_dollar_normalization_fact();
CREATE OR REPLACE FUNCTION prevent_official_dollar_normalization_mutation()
RETURNS TRIGGER AS $$
BEGIN
  RAISE EXCEPTION 'official dollar normalization facts are immutable';
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER official_dollar_normalization_immutable
BEFORE UPDATE OR DELETE ON official_dollar_normalization_facts
FOR EACH ROW EXECUTE FUNCTION prevent_official_dollar_normalization_mutation();

CREATE OR REPLACE FUNCTION validate_aircraft_reference_child_insert()
RETURNS TRIGGER AS $$
DECLARE
  parent_state TEXT;
  expected_value_type TEXT;
BEGIN
  SELECT publication_state INTO parent_state
  FROM aircraft_reference_configuration_versions
  WHERE id = NEW.aircraft_reference_configuration_version_id;
  IF parent_state IS DISTINCT FROM 'building' THEN
    RAISE EXCEPTION 'reference profile children require a building version';
  END IF;
  IF TG_TABLE_NAME = 'aircraft_reference_applicability_scopes' THEN
    IF NOT NEW.applies_to_all_serials AND (
      NEW.serial_from_sort_key !~ '^[A-Z0-9]+$'
      OR NEW.serial_to_sort_key !~ '^[A-Z0-9]+$'
      OR (NEW.serial_prefix IS NOT NULL AND NEW.serial_prefix !~ '^[A-Z0-9]+$')
    ) THEN
      RAISE EXCEPTION 'reference serial applicability requires canonical sort keys';
    END IF;
  ELSIF TG_TABLE_NAME = 'aircraft_reference_avionics' THEN
    IF NOT EXISTS (
      SELECT 1 FROM avionics_models
      WHERE id = NEW.avionics_model_id AND catalog_status = 'approved'
    ) THEN
      RAISE EXCEPTION 'reference avionics requires an approved catalog product';
    END IF;
  ELSIF TG_TABLE_NAME = 'aircraft_reference_engines' THEN
    IF NOT EXISTS (
      SELECT 1 FROM aircraft_engine_catalog_models
      WHERE id = NEW.aircraft_engine_catalog_model_id AND catalog_status = 'approved'
    ) THEN
      RAISE EXCEPTION 'reference engine requires an approved catalog model';
    END IF;
  ELSIF TG_TABLE_NAME = 'aircraft_reference_propellers' THEN
    IF NOT EXISTS (
      SELECT 1 FROM aircraft_propeller_catalog_models
      WHERE id = NEW.aircraft_propeller_catalog_model_id AND catalog_status = 'approved'
    ) THEN
      RAISE EXCEPTION 'reference propeller requires an approved catalog model';
    END IF;
  ELSIF TG_TABLE_NAME = 'aircraft_reference_features' THEN
    SELECT value_type INTO expected_value_type FROM aircraft_feature_definitions
    WHERE id = NEW.aircraft_feature_definition_id;
    IF (expected_value_type = 'boolean' AND NEW.boolean_value IS NULL)
       OR (expected_value_type = 'number' AND NEW.number_value IS NULL)
       OR (expected_value_type = 'text' AND NEW.text_value IS NULL)
       OR expected_value_type IS NULL THEN
      RAISE EXCEPTION 'reference feature value does not match its definition';
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER aircraft_reference_fact_set_building_insert
BEFORE INSERT ON aircraft_reference_fact_set_attestations
FOR EACH ROW EXECUTE FUNCTION validate_aircraft_reference_child_insert();
CREATE TRIGGER aircraft_reference_fact_set_immutable
BEFORE UPDATE OR DELETE ON aircraft_reference_fact_set_attestations
FOR EACH ROW EXECUTE FUNCTION prevent_aircraft_reference_fact_mutation();

CREATE OR REPLACE FUNCTION validate_aircraft_reference_version_update()
RETURNS TRIGGER AS $$
BEGIN
  IF OLD.publication_state IN ('published', 'superseded') THEN
    IF NOT (OLD.publication_state = 'published' AND NEW.publication_state = 'superseded'
      AND NEW.superseded_at IS NOT NULL AND NEW.id = OLD.id
      AND NEW.aircraft_reference_configuration_id = OLD.aircraft_reference_configuration_id
      AND NEW.model_year = OLD.model_year AND NEW.revision = OLD.revision
      AND NEW.approval_decision_id = OLD.approval_decision_id
      AND NEW.published_at = OLD.published_at
      AND NEW.supersedes_version_id IS NOT DISTINCT FROM OLD.supersedes_version_id)
    THEN RAISE EXCEPTION 'published reference profile versions are immutable'; END IF;
    RETURN NEW;
  END IF;
  IF NEW.publication_state = 'published' THEN
    IF OLD.publication_state <> 'building' OR NEW.published_at IS NULL THEN
      RAISE EXCEPTION 'only a building profile with published_at can be published';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM aircraft_reference_applicability_scopes WHERE aircraft_reference_configuration_version_id = NEW.id) THEN
      RAISE EXCEPTION 'published reference profile requires applicability';
    END IF;
    IF 4 <> (SELECT COUNT(*) FROM aircraft_reference_fact_set_attestations WHERE aircraft_reference_configuration_version_id = NEW.id) THEN
      RAISE EXCEPTION 'published reference profile requires complete factory fact-set attestations';
    END IF;
    IF NOT EXISTS (
      SELECT 1 FROM aircraft_reference_prices price
      JOIN curation_evidence_claims claim ON claim.id = price.evidence_claim_id
      JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
      WHERE price.aircraft_reference_configuration_version_id = NEW.id
        AND price.currency = 'USD' AND price.evidence_kind = 'direct_model_year'
        AND price.configuration_basis = 'full_standard_configuration'
        AND claim.validation_status = 'validated'
        AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
    ) THEN RAISE EXCEPTION 'published profile requires direct exact-model-year full-configuration primary price evidence'; END IF;
    IF EXISTS (
      SELECT 1
      FROM aircraft_reference_avionics fact
      JOIN avionics_models model ON model.id = fact.avionics_model_id
      WHERE fact.aircraft_reference_configuration_version_id = NEW.id
        AND (model.catalog_status <> 'approved'
          OR model.introduced_year IS NULL
          OR model.estimated_unit_value_usd IS NULL
          OR model.estimated_unit_value_usd < 0
          OR model.value_basis <> 'installed_contribution'
          OR model.replacement_cost_usd IS NULL
          OR model.replacement_cost_usd < model.estimated_unit_value_usd
          OR model.value_reference_year NOT BETWEEN 1900 AND 2200
          OR BTRIM(COALESCE(model.value_source, '')) = '')
    ) THEN RAISE EXCEPTION 'published reference profile requires valuation-ready factory avionics'; END IF;
    IF EXISTS (
      SELECT 1 FROM (
        SELECT evidence_claim_id FROM aircraft_reference_applicability_scopes WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id FROM aircraft_reference_prices WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id FROM aircraft_reference_avionics WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id FROM aircraft_reference_engines WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id FROM aircraft_reference_propellers WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id FROM aircraft_reference_features WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id FROM aircraft_reference_fact_set_attestations WHERE aircraft_reference_configuration_version_id = NEW.id
      ) fact
      JOIN curation_evidence_claims claim ON claim.id = fact.evidence_claim_id
      JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
      WHERE claim.validation_status <> 'validated' OR source.source_tier NOT IN ('manufacturer_primary', 'regulator_primary')
    ) THEN RAISE EXCEPTION 'published reference profile facts require validated primary evidence'; END IF;
    IF EXISTS (
      SELECT 1 FROM aircraft_reference_applicability_scopes left_scope
      JOIN aircraft_reference_applicability_scopes right_scope
        ON right_scope.aircraft_reference_configuration_version_id = left_scope.aircraft_reference_configuration_version_id
       AND right_scope.id > left_scope.id AND right_scope.aircraft_market_id = left_scope.aircraft_market_id
      WHERE left_scope.aircraft_reference_configuration_version_id = NEW.id
        AND (left_scope.applies_to_all_serials OR right_scope.applies_to_all_serials OR (
          left_scope.aircraft_serial_number_scheme_id = right_scope.aircraft_serial_number_scheme_id
          AND COALESCE(left_scope.serial_prefix, '') = COALESCE(right_scope.serial_prefix, '')
          AND left_scope.serial_from_sort_key <= right_scope.serial_to_sort_key
          AND right_scope.serial_from_sort_key <= left_scope.serial_to_sort_key)))
    THEN RAISE EXCEPTION 'reference profile contains overlapping applicability scopes'; END IF;
    IF EXISTS (
      SELECT 1 FROM aircraft_reference_applicability_scopes candidate
      JOIN aircraft_reference_applicability_scopes existing ON existing.aircraft_market_id = candidate.aircraft_market_id
      JOIN aircraft_reference_configuration_versions existing_version ON existing_version.id = existing.aircraft_reference_configuration_version_id
      WHERE candidate.aircraft_reference_configuration_version_id = NEW.id
        AND existing_version.id <> NEW.id
        AND existing_version.aircraft_reference_configuration_id = NEW.aircraft_reference_configuration_id
        AND existing_version.model_year = NEW.model_year AND existing_version.publication_state = 'published'
        AND (candidate.applies_to_all_serials OR existing.applies_to_all_serials OR (
          candidate.aircraft_serial_number_scheme_id = existing.aircraft_serial_number_scheme_id
          AND COALESCE(candidate.serial_prefix, '') = COALESCE(existing.serial_prefix, '')
          AND candidate.serial_from_sort_key <= existing.serial_to_sort_key
          AND existing.serial_from_sort_key <= candidate.serial_to_sort_key)))
    THEN RAISE EXCEPTION 'reference profile applicability overlaps an existing published version'; END IF;
  ELSIF NEW.publication_state <> 'building' THEN
    RAISE EXCEPTION 'invalid building profile state transition';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_reference_catalog_cutover', 2,
  'e3b9d29ec2b2a7b8139b8e46cd2d69c00f91513ec9c79588b6b10dde1771ec0f', CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = EXCLUDED.contract_version,
  contract_fingerprint = EXCLUDED.contract_fingerprint,
  installed_at = EXCLUDED.installed_at;
