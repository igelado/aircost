PRAGMA foreign_keys = ON;

-- The reference catalog is the only aircraft configuration/value authority.
-- Drop dependent triggers first so the surviving trigger bodies cannot retain
-- references to the removed relations.
DROP TRIGGER IF EXISTS avionics_models_referenced_status_update;
DROP TRIGGER IF EXISTS aircraft_valuation_projection_validate_insert;

DROP TABLE IF EXISTS aircraft_model_variant_default_avionics_candidates;
DROP TABLE IF EXISTS aircraft_model_variant_default_avionics;
DROP TABLE IF EXISTS aircraft_model_variant_price_points;
DROP TABLE IF EXISTS aircraft_model_spec_versions;
DROP TABLE IF EXISTS depreciation_profile_fit_metadata;
DROP TABLE IF EXISTS component_depreciation_profiles;
DROP TABLE IF EXISTS depreciation_profiles;

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

CREATE TRIGGER aircraft_valuation_projection_validate_insert
BEFORE INSERT ON aircraft_valuation_compatibility_projections
WHEN NOT EXISTS (
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
    AND assignment.aircraft_generation_id IS NEW.aircraft_generation_id
    AND assignment.aircraft_factory_package_id IS NEW.aircraft_factory_package_id
    AND assignment.aircraft_sale_listing_id = NEW.created_from_aircraft_sale_listing_id
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
            OR applicability.aircraft_generation_id IS assignment.aircraft_generation_id
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
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft compatibility projection requires the active command, exact copied assignment provenance, and its fresh reserved hierarchy');
END;

ALTER TABLE aircraft_reference_prices
  ADD COLUMN configuration_basis TEXT NOT NULL DEFAULT 'unknown'
  CHECK (configuration_basis IN (
    'full_standard_configuration', 'base_aircraft_only', 'unknown'
  ));

DROP TRIGGER aircraft_reference_versions_require_approval;
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
OR (NEW.revision = 1) <> (NEW.supersedes_version_id IS NULL)
OR (
  NEW.supersedes_version_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_reference_configuration_versions previous
    WHERE previous.id = NEW.supersedes_version_id
      AND previous.aircraft_reference_configuration_id = NEW.aircraft_reference_configuration_id
      AND previous.model_year = NEW.model_year
      AND previous.revision = NEW.revision - 1
      AND previous.publication_state = 'published'
  )
)
BEGIN
  SELECT RAISE(ABORT, 'reference profile requires building state, approved evidence, and its exact predecessor');
END;

CREATE TABLE aircraft_reference_fact_set_attestations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  fact_set_kind TEXT NOT NULL CHECK (fact_set_kind IN (
    'avionics', 'engines', 'propellers', 'features'
  )),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, fact_set_kind)
);

CREATE TRIGGER aircraft_reference_scope_canonical_insert
BEFORE INSERT ON aircraft_reference_applicability_scopes
WHEN NEW.applies_to_all_serials = 0 AND (
  NEW.serial_from_sort_key <> upper(NEW.serial_from_sort_key)
  OR NEW.serial_to_sort_key <> upper(NEW.serial_to_sort_key)
  OR NEW.serial_from_sort_key GLOB '*[^A-Z0-9]*'
  OR NEW.serial_to_sort_key GLOB '*[^A-Z0-9]*'
  OR (
    NEW.serial_prefix IS NOT NULL
    AND (
      NEW.serial_prefix <> upper(NEW.serial_prefix)
      OR NEW.serial_prefix GLOB '*[^A-Z0-9]*'
    )
  )
)
BEGIN SELECT RAISE(ABORT, 'reference serial applicability requires canonical sort keys'); END;

CREATE TABLE official_dollar_normalization_facts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_year INTEGER NOT NULL CHECK (source_year BETWEEN 1900 AND 2200),
  target_year INTEGER NOT NULL CHECK (target_year BETWEEN 1900 AND 2200),
  index_series TEXT NOT NULL CHECK (length(trim(index_series)) > 0),
  source_index_value REAL NOT NULL CHECK (source_index_value > 0),
  target_index_value REAL NOT NULL CHECK (target_index_value > 0),
  normalization_factor REAL NOT NULL CHECK (normalization_factor > 0),
  evidence_claim_id INTEGER NOT NULL UNIQUE
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (source_year, target_year),
  CHECK (source_year <> target_year),
  CHECK (
    abs(normalization_factor - (target_index_value / source_index_value))
      <= 0.000000001
  )
);

CREATE TRIGGER official_dollar_normalization_require_evidence
BEFORE INSERT ON official_dollar_normalization_facts
WHEN NOT EXISTS (
  SELECT 1
  FROM curation_evidence_claims claim
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE claim.id = NEW.evidence_claim_id
    AND claim.validation_status = 'validated'
    AND claim.claim_kind IN ('price', 'specification')
    AND source.source_tier = 'regulator_primary'
)
BEGIN SELECT RAISE(ABORT, 'dollar normalization requires validated official regulator evidence'); END;
CREATE TRIGGER official_dollar_normalization_immutable_update
BEFORE UPDATE ON official_dollar_normalization_facts
BEGIN SELECT RAISE(ABORT, 'official dollar normalization facts are immutable'); END;
CREATE TRIGGER official_dollar_normalization_immutable_delete
BEFORE DELETE ON official_dollar_normalization_facts
BEGIN SELECT RAISE(ABORT, 'official dollar normalization facts are immutable'); END;

DROP TRIGGER aircraft_reference_price_building_insert;
CREATE TRIGGER aircraft_reference_price_building_insert
BEFORE INSERT ON aircraft_reference_prices
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
BEGIN SELECT RAISE(ABORT, 'reference price requires a building version'); END;

CREATE TRIGGER aircraft_reference_fact_set_building_insert
BEFORE INSERT ON aircraft_reference_fact_set_attestations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
BEGIN SELECT RAISE(ABORT, 'reference fact-set attestation requires a building version'); END;
CREATE TRIGGER aircraft_reference_fact_set_immutable_update
BEFORE UPDATE ON aircraft_reference_fact_set_attestations
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_fact_set_immutable_delete
BEFORE DELETE ON aircraft_reference_fact_set_attestations
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;

DROP TRIGGER aircraft_reference_versions_publish;
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
  SELECT RAISE(ABORT, 'published reference profile requires complete factory fact-set attestations')
  WHERE 4 <> (
    SELECT COUNT(*) FROM aircraft_reference_fact_set_attestations attestation
    WHERE attestation.aircraft_reference_configuration_version_id = NEW.id
  );
  SELECT RAISE(ABORT, 'published reference profile requires direct exact-model-year full-configuration primary price evidence')
  WHERE NOT EXISTS (
    SELECT 1
    FROM aircraft_reference_prices price
    JOIN curation_evidence_claims claim ON claim.id = price.evidence_claim_id
    JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
    WHERE price.aircraft_reference_configuration_version_id = NEW.id
      AND price.currency = 'USD'
      AND price.evidence_kind = 'direct_model_year'
      AND price.configuration_basis = 'full_standard_configuration'
      AND claim.validation_status = 'validated'
      AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
  );
  SELECT RAISE(ABORT, 'published reference profile requires valuation-ready factory avionics')
  WHERE EXISTS (
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
        OR TRIM(COALESCE(model.value_source, '')) = '')
  );
  SELECT RAISE(ABORT, 'published reference profile facts require validated primary evidence')
  WHERE EXISTS (
    SELECT 1 FROM (
      SELECT evidence_claim_id FROM aircraft_reference_applicability_scopes
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id FROM aircraft_reference_prices
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id FROM aircraft_reference_avionics
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id FROM aircraft_reference_engines
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id FROM aircraft_reference_propellers
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id FROM aircraft_reference_features
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id FROM aircraft_reference_fact_set_attestations
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
      AND (left_scope.applies_to_all_serials = 1 OR right_scope.applies_to_all_serials = 1 OR (
        left_scope.aircraft_serial_number_scheme_id = right_scope.aircraft_serial_number_scheme_id
        AND coalesce(left_scope.serial_prefix, '') = coalesce(right_scope.serial_prefix, '')
        AND left_scope.serial_from_sort_key <= right_scope.serial_to_sort_key
        AND right_scope.serial_from_sort_key <= left_scope.serial_to_sort_key
      ))
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
      AND (candidate.applies_to_all_serials = 1 OR existing.applies_to_all_serials = 1 OR (
        candidate.aircraft_serial_number_scheme_id = existing.aircraft_serial_number_scheme_id
        AND coalesce(candidate.serial_prefix, '') = coalesce(existing.serial_prefix, '')
        AND candidate.serial_from_sort_key <= existing.serial_to_sort_key
        AND existing.serial_from_sort_key <= candidate.serial_to_sort_key
      ))
  );
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_reference_catalog_cutover', 2,
  'e3b9d29ec2b2a7b8139b8e46cd2d69c00f91513ec9c79588b6b10dde1771ec0f', CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = excluded.contract_version,
  contract_fingerprint = excluded.contract_fingerprint,
  installed_at = excluded.installed_at;
