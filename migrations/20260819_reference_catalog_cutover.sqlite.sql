PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

-- Reject mismatched provenance and marker-present structural damage before any
-- transition DDL can replace or recreate cutover objects.
CREATE TEMP TABLE reference_catalog_cutover_rerun_preflight (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO reference_catalog_cutover_rerun_preflight (valid)
SELECT CASE WHEN EXISTS (
  SELECT 1
  FROM schema_migration_contracts
  WHERE migration_name = '20260819_reference_catalog_cutover'
    AND (
      contract_version <> 1
      OR contract_fingerprint <>
        '8e5a542d55319ee8a4ba4e31a5d67de3b2ec827c93063b457ba46236bb622455'
    )
) OR (
  EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260819_reference_catalog_cutover'
      AND contract_version = 1
      AND contract_fingerprint =
        '8e5a542d55319ee8a4ba4e31a5d67de3b2ec827c93063b457ba46236bb622455'
  )
  AND (
    (SELECT count(*) FROM sqlite_schema
     WHERE type = 'table' AND name IN (
       'aircraft_reference_fact_set_attestations',
       'official_dollar_normalization_facts'
     )) <> 2
    OR (SELECT count(*) FROM sqlite_schema
        WHERE type = 'trigger' AND name IN (
          'avionics_models_referenced_status_update',
          'aircraft_valuation_projection_validate_insert',
          'aircraft_reference_scope_canonical_insert',
          'aircraft_reference_scope_key_recompute_insert',
          'aircraft_reference_versions_require_approval',
          'official_dollar_normalization_require_evidence',
          'official_dollar_normalization_immutable_update',
          'official_dollar_normalization_immutable_delete',
          'aircraft_reference_price_building_insert',
          'aircraft_reference_price_immutable_update',
          'aircraft_reference_price_immutable_delete',
          'aircraft_reference_fact_set_building_insert',
          'aircraft_reference_fact_set_immutable_update',
          'aircraft_reference_fact_set_immutable_delete',
          'aircraft_reference_versions_publish',
          'aircraft_serial_schemes_require_approval',
          'aircraft_serial_schemes_preserve_ordering'
        )) <> 17
    OR (SELECT count(*) FROM pragma_table_info('aircraft_reference_prices')
        WHERE name = 'configuration_basis'
          AND upper(type) = 'TEXT'
          AND "notnull" = 1
          AND dflt_value = '''unknown'''
          AND pk = 0) <> 1
    OR (SELECT lower(hex(sha3(group_concat(
          type || ':' || name || ':' || normalized_sql, '|'
        ), 256)))
        FROM (
          SELECT type, name, normalized_sql
          FROM (
            SELECT
              type,
              name,
              lower(replace(replace(replace(replace(
                sql, char(9), ''
              ), char(10), ''), char(13), ''), ' ', '')) AS normalized_sql
            FROM sqlite_schema
            WHERE (type = 'table' AND name IN (
              'aircraft_reference_prices',
              'aircraft_reference_fact_set_attestations',
              'official_dollar_normalization_facts'
            )) OR (type = 'trigger' AND (
              name IN (
                'avionics_models_referenced_status_update',
                'aircraft_valuation_projection_validate_insert',
                'aircraft_reference_scope_canonical_insert',
                'aircraft_reference_scope_key_recompute_insert',
                'aircraft_reference_versions_require_approval',
                'official_dollar_normalization_require_evidence',
                'official_dollar_normalization_immutable_update',
                'official_dollar_normalization_immutable_delete',
                'aircraft_reference_price_building_insert',
                'aircraft_reference_price_immutable_update',
                'aircraft_reference_price_immutable_delete',
                'aircraft_reference_fact_set_building_insert',
                'aircraft_reference_fact_set_immutable_update',
                'aircraft_reference_fact_set_immutable_delete',
                'aircraft_reference_versions_publish',
                'aircraft_serial_schemes_require_approval',
                'aircraft_serial_schemes_preserve_ordering'
              )
              OR tbl_name IN (
                'aircraft_reference_prices',
                'aircraft_reference_fact_set_attestations',
                'official_dollar_normalization_facts'
              )
            ))
            UNION ALL
            SELECT
              'index' AS type,
              protected_relation.relation_name || ':' || index_row.name AS name,
              index_row.[unique] || ':' || index_row.origin || ':' ||
                index_row.partial || ':' || COALESCE((
                  SELECT group_concat(index_column.signature, ',')
                  FROM (
                    SELECT
                      xinfo.seqno || ':' || xinfo.cid || ':' ||
                      COALESCE(xinfo.name, '') || ':' || xinfo.desc || ':' ||
                      xinfo.coll || ':' || xinfo.key AS signature
                    FROM pragma_index_xinfo(index_row.name) xinfo
                    ORDER BY xinfo.seqno
                  ) index_column
                ), '') AS normalized_sql
            FROM (
              SELECT 'aircraft_reference_prices' AS relation_name
              UNION ALL
              SELECT 'aircraft_reference_fact_set_attestations'
              UNION ALL
              SELECT 'official_dollar_normalization_facts'
            ) protected_relation
            JOIN pragma_index_list(protected_relation.relation_name) index_row
          ) exact_object
          ORDER BY type, name
        )) <> '9ef50133da8e63ad020fb3fc74ec3c66230f200616783315e96cf6dd9912acb1'
    OR (SELECT count(*) FROM sqlite_schema
        WHERE (type = 'table' AND name = 'listing_verification_run_items')
           OR (type IN ('index', 'trigger')
               AND tbl_name = 'listing_verification_run_items')) <> 6
    OR (SELECT lower(hex(sha3(group_concat(
          type || ':' || name || ':' || normalized_sql, '|'
        ), 256)))
        FROM (
          SELECT type, name, lower(replace(replace(replace(replace(
            sql, char(9), ''
          ), char(10), ''), char(13), ''), ' ', '')) AS normalized_sql
          FROM sqlite_schema
          WHERE (
            type = 'table' AND name = 'listing_verification_run_items'
          ) OR (
            type IN ('index', 'trigger')
            AND tbl_name = 'listing_verification_run_items'
            AND sql IS NOT NULL
          )
          ORDER BY type, name
        )) <> 'dab93c6d4c1d7f80e7297e528419ee99a0d6b5e1f9ad971bc4155ddd3f418e81'
    OR EXISTS (
      SELECT 1 FROM sqlite_schema
      WHERE type = 'table' AND name IN (
        'aircraft_model_spec_versions',
        'aircraft_model_variant_price_points',
        'aircraft_model_variant_default_avionics',
        'aircraft_model_variant_default_avionics_candidates',
        'depreciation_profiles',
        'depreciation_profile_fit_metadata',
        'component_depreciation_profiles'
      )
    )
  )
) THEN 0 ELSE 1 END;
DROP TABLE reference_catalog_cutover_rerun_preflight;

-- A marker-absent upgrade may rebuild the historical run-item table only from
-- its exact owned schema. Unexpected constraints, indexes, or triggers are a
-- conflict to investigate, never objects for this migration to erase.
CREATE TEMP TABLE reference_catalog_cutover_run_item_preflight (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO reference_catalog_cutover_run_item_preflight (valid)
SELECT CASE WHEN NOT EXISTS (
  SELECT 1 FROM schema_migration_contracts
  WHERE migration_name = '20260819_reference_catalog_cutover'
) AND (
  (SELECT count(*) FROM sqlite_schema
   WHERE (type = 'table' AND name = 'listing_verification_run_items')
      OR (type IN ('index', 'trigger')
          AND tbl_name = 'listing_verification_run_items')) <> 6
  OR (SELECT lower(hex(sha3(group_concat(
        type || ':' || name || ':' || normalized_sql, '|'
      ), 256)))
      FROM (
        SELECT type, name, lower(replace(replace(replace(replace(
          sql, char(9), ''
        ), char(10), ''), char(13), ''), ' ', '')) AS normalized_sql
        FROM sqlite_schema
        WHERE (
          type = 'table' AND name = 'listing_verification_run_items'
        ) OR (
          type IN ('index', 'trigger')
          AND tbl_name = 'listing_verification_run_items'
          AND sql IS NOT NULL
        )
        ORDER BY type, name
      )) <> '9995284c615eea7e025a34ee4f7e6fb24fc5f7e9c40414cb158879d6f227b00f'
) THEN 0 ELSE 1 END;
DROP TABLE reference_catalog_cutover_run_item_preflight;

-- Reference readiness is independent from listing verification. Rewrite the
-- obsolete terminal run-item outcome before tightening the durable queue
-- contract. It is not evidence that the listing passed verification: preserve
-- the incomplete listing state and nested reference stage, and require a new
-- run to determine the current outcome.
CREATE TEMP TABLE reference_catalog_cutover_run_item_sequence (
  sequence_value INTEGER NOT NULL
);
INSERT INTO reference_catalog_cutover_run_item_sequence (sequence_value)
SELECT COALESCE((
  SELECT seq FROM sqlite_sequence
  WHERE name = 'listing_verification_run_items'
), 0);

ALTER TABLE listing_verification_run_items
  RENAME TO reference_catalog_cutover_legacy_run_items;

CREATE TABLE listing_verification_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL
    REFERENCES listing_verification_runs(id) ON DELETE CASCADE,
  listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  status TEXT NOT NULL DEFAULT 'queued'
    CONSTRAINT listing_verification_run_items_status_check CHECK (status IN (
      'queued', 'running', 'verified', 'pending_review',
      'blocked', 'failed', 'cancelled'
    )),
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  lease_token TEXT,
  lease_expires_at_epoch_seconds INTEGER,
  outcome_json TEXT,
  reason_code TEXT,
  reason TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  started_at TEXT,
  completed_at TEXT,
  UNIQUE (run_id, position),
  UNIQUE (run_id, listing_id),
  CHECK (lease_token IS NULL OR length(trim(lease_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = 'running'
      AND lease_token IS NOT NULL
      AND lease_expires_at_epoch_seconds IS NOT NULL
      AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status <> 'running'
      AND lease_token IS NULL
      AND lease_expires_at_epoch_seconds IS NULL)
  ),
  CONSTRAINT listing_verification_run_items_completion_check CHECK (
    (status IN ('queued', 'running') AND completed_at IS NULL)
    OR
    (status IN (
      'verified', 'pending_review',
      'blocked', 'failed', 'cancelled'
    ) AND completed_at IS NOT NULL)
  ),
  CHECK (
    outcome_json IS NULL
    OR (
      length(outcome_json) BETWEEN 2 AND 65536
      AND json_valid(outcome_json)
      AND json_type(outcome_json) = 'object'
    )
  ),
  CONSTRAINT listing_verification_run_items_outcome_required_check CHECK (
    status NOT IN (
      'verified', 'pending_review', 'blocked'
    )
    OR outcome_json IS NOT NULL
  ),
  CHECK (reason_code IS NULL OR length(trim(reason_code)) BETWEEN 1 AND 100),
  CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 2000)
);

INSERT INTO listing_verification_run_items (
  id, run_id, listing_id, position, status, attempt_count, lease_token,
  lease_expires_at_epoch_seconds, outcome_json, reason_code, reason,
  created_at, updated_at, started_at, completed_at
)
SELECT
  id, run_id, listing_id, position,
  CASE status WHEN 'pending_reference' THEN 'blocked' ELSE status END,
  attempt_count, lease_token, lease_expires_at_epoch_seconds,
  CASE status WHEN 'pending_reference' THEN json_set(
    outcome_json,
    '$.status', 'blocked',
    '$.finalization.status', 'not_attempted'
  ) ELSE outcome_json END,
  CASE status WHEN 'pending_reference' THEN
    'legacy_reference_gate_removed' ELSE reason_code END,
  CASE status WHEN 'pending_reference' THEN
    'Historical reference gating was removed; run verification again.'
    ELSE reason END,
  created_at, updated_at, started_at, completed_at
FROM reference_catalog_cutover_legacy_run_items;

DROP TABLE reference_catalog_cutover_legacy_run_items;
UPDATE sqlite_sequence
SET seq = MAX(seq, (
  SELECT sequence_value FROM reference_catalog_cutover_run_item_sequence
))
WHERE name = 'listing_verification_run_items';
INSERT INTO sqlite_sequence (name, seq)
SELECT 'listing_verification_run_items', sequence_value
FROM reference_catalog_cutover_run_item_sequence
WHERE sequence_value > 0
  AND NOT EXISTS (
    SELECT 1 FROM sqlite_sequence
    WHERE name = 'listing_verification_run_items'
  );
DROP TABLE reference_catalog_cutover_run_item_sequence;

CREATE UNIQUE INDEX idx_listing_verification_run_items_one_active_listing
  ON listing_verification_run_items (listing_id)
  WHERE status IN ('queued', 'running');
CREATE UNIQUE INDEX idx_listing_verification_run_items_one_running_per_run
  ON listing_verification_run_items (run_id)
  WHERE status = 'running';
CREATE INDEX idx_listing_verification_run_items_claim
  ON listing_verification_run_items (run_id, status, position, id);

DROP VIEW IF EXISTS aircraft_reference_serial_key_errors;
CREATE VIEW aircraft_reference_serial_key_errors AS
WITH RECURSIVE
bounds(scope_id, bound_name, serial_value, stored_key) AS (
  SELECT id, 'from', serial_from_display, serial_from_sort_key
  FROM aircraft_reference_applicability_scopes
  WHERE applies_to_all_serials = 0
  UNION ALL
  SELECT id, 'to', serial_to_display, serial_to_sort_key
  FROM aircraft_reference_applicability_scopes
  WHERE applies_to_all_serials = 0
),
state(
  scope_id, bound_name, serial_value, stored_key,
  position, segment, alpha_hex, numeric_segment, encoded
) AS (
  SELECT
    scope_id, bound_name, serial_value, stored_key, 2,
    substr(serial_value, 1, 1),
    CASE WHEN substr(serial_value, 1, 1) GLOB '[0-9]' THEN ''
      ELSE printf('%02X', instr(
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', substr(serial_value, 1, 1)
      )) END,
    substr(serial_value, 1, 1) GLOB '[0-9]', '01'
  FROM bounds
  UNION ALL
  SELECT
    scope_id, bound_name, serial_value, stored_key, position + 1,
    CASE WHEN (substr(serial_value, position, 1) GLOB '[0-9]') = numeric_segment
      THEN segment || substr(serial_value, position, 1)
      ELSE substr(serial_value, position, 1) END,
    CASE WHEN (substr(serial_value, position, 1) GLOB '[0-9]') = numeric_segment
      THEN alpha_hex || CASE WHEN numeric_segment THEN '' ELSE printf(
        '%02X', instr('ABCDEFGHIJKLMNOPQRSTUVWXYZ', substr(serial_value, position, 1))
      ) END
      ELSE CASE WHEN substr(serial_value, position, 1) GLOB '[0-9]' THEN ''
        ELSE printf('%02X', instr(
          'ABCDEFGHIJKLMNOPQRSTUVWXYZ', substr(serial_value, position, 1)
        )) END END,
    substr(serial_value, position, 1) GLOB '[0-9]',
    CASE WHEN (substr(serial_value, position, 1) GLOB '[0-9]') = numeric_segment
      THEN encoded
      ELSE encoded || CASE WHEN numeric_segment THEN
        '20'
        || printf('%08X', length(CASE WHEN trim(segment, '0') = ''
          THEN '0' ELSE ltrim(segment, '0') END))
        || CASE WHEN trim(segment, '0') = '' THEN '0' ELSE ltrim(segment, '0') END
        || printf('%08X', length(segment)) || segment
      ELSE '10' || alpha_hex || '00' END END
  FROM state
  WHERE position <= length(serial_value)
),
expected(scope_id, bound_name, expected_key) AS (
  SELECT scope_id, bound_name,
    encoded || CASE WHEN numeric_segment THEN
      '20'
      || printf('%08X', length(CASE WHEN trim(segment, '0') = ''
        THEN '0' ELSE ltrim(segment, '0') END))
      || CASE WHEN trim(segment, '0') = '' THEN '0' ELSE ltrim(segment, '0') END
      || printf('%08X', length(segment)) || segment
    ELSE '10' || alpha_hex || '00' END || '00'
  FROM state
  WHERE position = length(serial_value) + 1
)
SELECT
  bounds.scope_id, bounds.bound_name, bounds.serial_value,
  bounds.stored_key, expected.expected_key
FROM bounds
LEFT JOIN expected
  ON expected.scope_id = bounds.scope_id
 AND expected.bound_name = bounds.bound_name
WHERE bounds.serial_value IS NULL
   OR bounds.serial_value = ''
   OR bounds.serial_value <> upper(bounds.serial_value)
   OR bounds.serial_value GLOB '*[^A-Z0-9]*'
   OR expected.expected_key IS NULL
   OR bounds.stored_key IS NOT expected.expected_key;

CREATE TEMP TABLE reference_catalog_cutover_serial_preflight (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO reference_catalog_cutover_serial_preflight (valid)
SELECT CASE WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_serial_key_errors
) OR EXISTS (
  SELECT 1
  FROM aircraft_reference_applicability_scopes scope
  LEFT JOIN aircraft_serial_number_schemes scheme
    ON scheme.id = scope.aircraft_serial_number_scheme_id
  WHERE scope.applies_to_all_serials = 0
    AND (
      scheme.normalization_version IS NOT 'natural_alphanumeric_segments_v1'
      OR scope.serial_from_sort_key <> upper(scope.serial_from_sort_key)
      OR scope.serial_to_sort_key <> upper(scope.serial_to_sort_key)
      OR scope.serial_from_sort_key GLOB '*[^A-F0-9]*'
      OR scope.serial_to_sort_key GLOB '*[^A-F0-9]*'
      OR substr(scope.serial_from_sort_key, 1, 2) <> '01'
      OR substr(scope.serial_to_sort_key, 1, 2) <> '01'
      OR substr(scope.serial_from_sort_key, -2) <> '00'
      OR substr(scope.serial_to_sort_key, -2) <> '00'
    )
) THEN 0 ELSE 1 END;
DROP TABLE reference_catalog_cutover_serial_preflight;

DELETE FROM aircraft_serial_number_schemes AS old_scheme
WHERE old_scheme.normalization_version <> 'natural_alphanumeric_segments_v1'
  AND NOT EXISTS (
    SELECT 1 FROM aircraft_reference_applicability_scopes scope
    WHERE scope.aircraft_serial_number_scheme_id = old_scheme.id
  )
  AND EXISTS (
    SELECT 1 FROM aircraft_serial_number_schemes replacement
    WHERE replacement.aircraft_make_id = old_scheme.aircraft_make_id
      AND replacement.name = old_scheme.name
      AND replacement.normalization_version = 'natural_alphanumeric_segments_v1'
  );
UPDATE aircraft_serial_number_schemes
SET normalization_version = 'natural_alphanumeric_segments_v1'
WHERE normalization_version <> 'natural_alphanumeric_segments_v1';

DROP TRIGGER IF EXISTS aircraft_serial_schemes_require_approval;
CREATE TRIGGER aircraft_serial_schemes_require_approval
BEFORE INSERT ON aircraft_serial_number_schemes
WHEN NEW.normalization_version <> 'natural_alphanumeric_segments_v1'
OR NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'serial_scheme'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'serial scheme requires the universal ordering and an approved evidence-backed decision'); END;

DROP TRIGGER IF EXISTS aircraft_serial_schemes_preserve_ordering;
CREATE TRIGGER aircraft_serial_schemes_preserve_ordering
BEFORE UPDATE OF normalization_version ON aircraft_serial_number_schemes
WHEN NEW.normalization_version <> 'natural_alphanumeric_segments_v1'
BEGIN SELECT RAISE(ABORT, 'serial scheme ordering version is immutable'); END;

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

-- Rebuild through a projection that supplies `unknown` only when the legacy
-- table lacks configuration_basis. SQLite renames the appended duplicate on a
-- current table, so the existing canonical value wins on exact reruns.
CREATE TEMP TABLE reference_catalog_cutover_prices AS
SELECT
  id,
  aircraft_reference_configuration_version_id,
  price_kind,
  amount,
  currency,
  price_reference_year,
  configuration_basis,
  evidence_kind,
  evidence_claim_id,
  created_at
FROM (
  SELECT *, 'unknown' AS configuration_basis
  FROM aircraft_reference_prices
);
CREATE TEMP TABLE reference_catalog_cutover_price_sequence AS
SELECT seq
FROM sqlite_sequence
WHERE name = 'aircraft_reference_prices';
DROP TABLE aircraft_reference_prices;
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
  configuration_basis TEXT NOT NULL DEFAULT 'unknown' CHECK (configuration_basis IN (
    'full_standard_configuration', 'base_aircraft_only', 'unknown'
  )),
  evidence_kind TEXT NOT NULL CHECK (evidence_kind IN (
    'direct_model_year', 'direct_other_year', 'interpolated', 'inferred'
  )),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, price_kind, currency)
);
INSERT INTO aircraft_reference_prices (
  id,
  aircraft_reference_configuration_version_id,
  price_kind,
  amount,
  currency,
  price_reference_year,
  configuration_basis,
  evidence_kind,
  evidence_claim_id,
  created_at
)
SELECT
  id,
  aircraft_reference_configuration_version_id,
  price_kind,
  amount,
  currency,
  price_reference_year,
  configuration_basis,
  evidence_kind,
  evidence_claim_id,
  created_at
FROM reference_catalog_cutover_prices;
DELETE FROM sqlite_sequence WHERE name = 'aircraft_reference_prices';
INSERT INTO sqlite_sequence (name, seq)
SELECT 'aircraft_reference_prices', seq
FROM reference_catalog_cutover_price_sequence;
DROP TABLE reference_catalog_cutover_prices;
DROP TABLE reference_catalog_cutover_price_sequence;

CREATE TRIGGER aircraft_reference_price_immutable_update
BEFORE UPDATE ON aircraft_reference_prices
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER aircraft_reference_price_immutable_delete
BEFORE DELETE ON aircraft_reference_prices
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_reference_scope_canonical_insert;
CREATE TRIGGER aircraft_reference_scope_canonical_insert
BEFORE INSERT ON aircraft_reference_applicability_scopes
WHEN NEW.applies_to_all_serials = 0 AND (
  NEW.serial_from_sort_key <> upper(NEW.serial_from_sort_key)
  OR NEW.serial_to_sort_key <> upper(NEW.serial_to_sort_key)
  OR NEW.serial_from_sort_key GLOB '*[^A-F0-9]*'
  OR NEW.serial_to_sort_key GLOB '*[^A-F0-9]*'
  OR substr(NEW.serial_from_sort_key, 1, 2) <> '01'
  OR substr(NEW.serial_to_sort_key, 1, 2) <> '01'
  OR substr(NEW.serial_from_sort_key, -2) <> '00'
  OR substr(NEW.serial_to_sort_key, -2) <> '00'
  OR NEW.serial_from_sort_key COLLATE BINARY
       > NEW.serial_to_sort_key COLLATE BINARY
  OR NOT EXISTS (
    SELECT 1 FROM aircraft_serial_number_schemes scheme
    WHERE scheme.id = NEW.aircraft_serial_number_scheme_id
      AND scheme.normalization_version = 'natural_alphanumeric_segments_v1'
  )
  OR (
    NEW.serial_prefix IS NOT NULL
    AND (
      NEW.serial_prefix <> upper(NEW.serial_prefix)
      OR NEW.serial_prefix GLOB '*[^A-Z0-9]*'
      OR substr(NEW.serial_from_display, 1, length(NEW.serial_prefix))
           <> NEW.serial_prefix
      OR substr(NEW.serial_to_display, 1, length(NEW.serial_prefix))
           <> NEW.serial_prefix
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'reference serial applicability requires the universal natural-order key');
END;

DROP TRIGGER IF EXISTS aircraft_reference_scope_key_recompute_insert;
CREATE TRIGGER aircraft_reference_scope_key_recompute_insert
AFTER INSERT ON aircraft_reference_applicability_scopes
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_serial_key_errors error
  WHERE error.scope_id = NEW.id
)
BEGIN
  SELECT RAISE(ABORT, 'reference serial sort keys must be recomputed from canonical display values');
END;

DROP TRIGGER IF EXISTS aircraft_reference_versions_require_approval;
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

CREATE TABLE IF NOT EXISTS aircraft_reference_fact_set_attestations (
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

CREATE TABLE IF NOT EXISTS official_dollar_normalization_facts (
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

DROP TRIGGER IF EXISTS official_dollar_normalization_require_evidence;
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
DROP TRIGGER IF EXISTS official_dollar_normalization_immutable_update;
CREATE TRIGGER official_dollar_normalization_immutable_update
BEFORE UPDATE ON official_dollar_normalization_facts
BEGIN SELECT RAISE(ABORT, 'official dollar normalization facts are immutable'); END;
DROP TRIGGER IF EXISTS official_dollar_normalization_immutable_delete;
CREATE TRIGGER official_dollar_normalization_immutable_delete
BEFORE DELETE ON official_dollar_normalization_facts
BEGIN SELECT RAISE(ABORT, 'official dollar normalization facts are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_reference_price_building_insert;
CREATE TRIGGER aircraft_reference_price_building_insert
BEFORE INSERT ON aircraft_reference_prices
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
BEGIN SELECT RAISE(ABORT, 'reference price requires a building version'); END;

DROP TRIGGER IF EXISTS aircraft_reference_fact_set_building_insert;
CREATE TRIGGER aircraft_reference_fact_set_building_insert
BEFORE INSERT ON aircraft_reference_fact_set_attestations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
BEGIN SELECT RAISE(ABORT, 'reference fact-set attestation requires a building version'); END;
DROP TRIGGER IF EXISTS aircraft_reference_fact_set_immutable_update;
CREATE TRIGGER aircraft_reference_fact_set_immutable_update
BEFORE UPDATE ON aircraft_reference_fact_set_attestations
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
DROP TRIGGER IF EXISTS aircraft_reference_fact_set_immutable_delete;
CREATE TRIGGER aircraft_reference_fact_set_immutable_delete
BEFORE DELETE ON aircraft_reference_fact_set_attestations
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_reference_versions_publish;
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
  SELECT RAISE(ABORT, 'published reference profile requires exactly one direct exact-model-year full-configuration equipped MSRP with primary price evidence')
  WHERE 1 <> (
    SELECT COUNT(*)
    FROM aircraft_reference_prices price
    JOIN curation_evidence_claims claim ON claim.id = price.evidence_claim_id
    JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
    WHERE price.aircraft_reference_configuration_version_id = NEW.id
      AND price.currency = 'USD'
      AND price.price_kind = 'equipped_msrp'
      AND price.evidence_kind = 'direct_model_year'
      AND price.configuration_basis = 'full_standard_configuration'
      AND claim.claim_kind = 'price'
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
    SELECT 1 FROM (
      SELECT evidence_claim_id, 'applicability' AS evidence_domain FROM aircraft_reference_applicability_scopes
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id, 'price' FROM aircraft_reference_prices
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id, 'factory' FROM aircraft_reference_avionics
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id, 'factory' FROM aircraft_reference_engines
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id, 'factory' FROM aircraft_reference_propellers
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id, 'factory' FROM aircraft_reference_features
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL SELECT evidence_claim_id, 'factory' FROM aircraft_reference_fact_set_attestations
      WHERE aircraft_reference_configuration_version_id = NEW.id
    ) fact
    JOIN curation_evidence_claims claim ON claim.id = fact.evidence_claim_id
    JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
    WHERE claim.validation_status <> 'validated'
       OR source.source_tier NOT IN ('manufacturer_primary', 'regulator_primary')
       OR (fact.evidence_domain = 'applicability' AND claim.claim_kind <> 'applicability')
       OR (fact.evidence_domain = 'price' AND claim.claim_kind <> 'price')
       OR (fact.evidence_domain = 'factory' AND claim.claim_kind NOT IN (
         'standard_equipment', 'package_composition', 'specification'
       ))
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
        left_scope.serial_from_sort_key COLLATE BINARY
          <= right_scope.serial_to_sort_key COLLATE BINARY
        AND right_scope.serial_from_sort_key COLLATE BINARY
          <= left_scope.serial_to_sort_key COLLATE BINARY
      ))
  );
  SELECT RAISE(ABORT, 'published reference profile applicability overlaps an existing version')
  WHERE EXISTS (
    SELECT 1
    FROM aircraft_reference_applicability_scopes candidate
    JOIN aircraft_markets candidate_market
      ON candidate_market.id = candidate.aircraft_market_id
    JOIN aircraft_reference_applicability_scopes existing
      ON existing.aircraft_market_id = candidate.aircraft_market_id
      OR candidate_market.code = 'GLOBAL'
      OR EXISTS (
        SELECT 1 FROM aircraft_markets existing_market
        WHERE existing_market.id = existing.aircraft_market_id
          AND existing_market.code = 'GLOBAL'
      )
    JOIN aircraft_reference_configuration_versions existing_version
      ON existing_version.id = existing.aircraft_reference_configuration_version_id
    WHERE candidate.aircraft_reference_configuration_version_id = NEW.id
      AND existing_version.id <> NEW.id
      AND existing_version.aircraft_reference_configuration_id = NEW.aircraft_reference_configuration_id
      AND existing_version.model_year = NEW.model_year
      AND existing_version.publication_state = 'published'
      AND (candidate.applies_to_all_serials = 1 OR existing.applies_to_all_serials = 1 OR (
        candidate.serial_from_sort_key COLLATE BINARY
          <= existing.serial_to_sort_key COLLATE BINARY
        AND existing.serial_from_sort_key COLLATE BINARY
          <= candidate.serial_to_sort_key COLLATE BINARY
      ))
  );
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_reference_catalog_cutover', 1,
  '8e5a542d55319ee8a4ba4e31a5d67de3b2ec827c93063b457ba46236bb622455', CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
