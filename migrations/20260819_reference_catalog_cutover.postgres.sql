BEGIN;

-- A marker-present rerun is an attestation before it is a migration. Reject
-- mismatched provenance or any changed/missing/extra canonical object before
-- CREATE OR REPLACE and other transition DDL can heal it.
DO $reference_catalog_cutover_rerun_preflight$
DECLARE
  exact_marker BOOLEAN;
  actual_object_count BIGINT;
  actual_definition_digest TEXT;
BEGIN
  IF EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_reference_catalog_cutover'
      AND (
        contract_version <> 1
        OR contract_fingerprint <>
          '039f72c03b3d2ba9538a4705ce7bda744fe02a322d018895c536604d280fe647'
      )
  ) THEN
    RAISE EXCEPTION 'reference catalog cutover contract marker mismatch';
  END IF;

  SELECT EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_reference_catalog_cutover'
      AND contract_version = 1
      AND contract_fingerprint =
        '039f72c03b3d2ba9538a4705ce7bda744fe02a322d018895c536604d280fe647'
  ) INTO exact_marker;

  IF exact_marker THEN
    WITH routine_names(name) AS (
      VALUES
        ('aircraft_serial_natural_sort_key'),
        ('validate_aircraft_serial_scheme_ordering'),
        ('prevent_referenced_avionics_catalog_downgrade'),
        ('invalidate_listing_avionics_authorization_for_capture'),
        ('validate_aircraft_valuation_compatibility_projection'),
        ('require_aircraft_catalog_approval'),
        ('validate_aircraft_reference_version_insert'),
        ('preserve_assigned_aircraft_applicability'),
        ('prevent_new_unresolved_aircraft_dimension'),
        ('validate_official_dollar_normalization_fact'),
        ('prevent_official_dollar_normalization_mutation'),
        ('validate_aircraft_reference_child_insert'),
        ('prevent_aircraft_reference_fact_mutation'),
        ('validate_aircraft_reference_version_update')
    ),
    relation_names(name) AS (
      VALUES
        ('aircraft_reference_prices'),
        ('aircraft_reference_fact_set_attestations'),
        ('official_dollar_normalization_facts')
    ),
    objects(object_key, definition) AS (
      SELECT
        'routine:' || routine.proname || ':' ||
          pg_catalog.pg_get_function_identity_arguments(routine.oid),
        lower(pg_catalog.regexp_replace(
          routine.prosrc, '[[:space:]]', '', 'g'
        )) || ':' ||
          COALESCE(
            pg_catalog.array_to_string(routine.proconfig, E'\n'), ''
          ) || ':' || language.lanname || ':' ||
          pg_catalog.format_type(routine.prorettype, NULL) || ':' ||
          pg_catalog.pg_get_function_identity_arguments(routine.oid) || ':' ||
          routine.prosecdef::TEXT || ':' || routine.proisstrict::TEXT || ':' ||
          routine.provolatile::TEXT || ':' || routine.proparallel::TEXT
      FROM pg_catalog.pg_proc routine
      JOIN pg_catalog.pg_namespace namespace
        ON namespace.oid = routine.pronamespace
      JOIN pg_catalog.pg_language language
        ON language.oid = routine.prolang
      JOIN routine_names expected ON expected.name = routine.proname
      WHERE namespace.nspname = 'public'
      UNION ALL
      SELECT
        'trigger:' || relation.relname || ':' || trigger_row.tgname,
        trigger_row.tgenabled::TEXT || ':' ||
          replace(
            pg_catalog.pg_get_triggerdef(trigger_row.oid, TRUE), 'public.', ''
          )
      FROM pg_catalog.pg_trigger trigger_row
      JOIN pg_catalog.pg_class relation
        ON relation.oid = trigger_row.tgrelid
      JOIN pg_catalog.pg_namespace namespace
        ON namespace.oid = relation.relnamespace
      JOIN pg_catalog.pg_proc routine ON routine.oid = trigger_row.tgfoid
      LEFT JOIN routine_names expected_routine
        ON expected_routine.name = routine.proname
      LEFT JOIN relation_names expected_relation
        ON expected_relation.name = relation.relname
      WHERE NOT trigger_row.tgisinternal
        AND namespace.nspname = 'public'
        AND (
          expected_routine.name IS NOT NULL
          OR expected_relation.name IS NOT NULL
        )
      UNION ALL
      SELECT
        'column:' || relation.relname || ':' || attribute.attnum::TEXT,
        attribute.attname || ':' ||
          pg_catalog.format_type(attribute.atttypid, attribute.atttypmod) || ':' ||
          attribute.attnotnull::TEXT || ':' || attribute.attidentity::TEXT || ':' ||
          COALESCE(
            pg_catalog.pg_get_expr(default_row.adbin, default_row.adrelid), ''
          )
      FROM pg_catalog.pg_class relation
      JOIN pg_catalog.pg_namespace namespace
        ON namespace.oid = relation.relnamespace
      JOIN relation_names expected ON expected.name = relation.relname
      JOIN pg_catalog.pg_attribute attribute
        ON attribute.attrelid = relation.oid
       AND attribute.attnum > 0
       AND NOT attribute.attisdropped
      LEFT JOIN pg_catalog.pg_attrdef default_row
        ON default_row.adrelid = relation.oid
       AND default_row.adnum = attribute.attnum
      WHERE namespace.nspname = 'public'
      UNION ALL
      SELECT
        'constraint:' || relation.relname || ':' ||
          constraint_row.contype::TEXT || ':' ||
          pg_catalog.md5(replace(
            pg_catalog.pg_get_constraintdef(constraint_row.oid, TRUE),
            'public.', ''
          )),
        constraint_row.contype::TEXT || ':' ||
          replace(
            pg_catalog.pg_get_constraintdef(constraint_row.oid, TRUE),
            'public.', ''
          )
      FROM pg_catalog.pg_constraint constraint_row
      JOIN pg_catalog.pg_class relation
        ON relation.oid = constraint_row.conrelid
      JOIN pg_catalog.pg_namespace namespace
        ON namespace.oid = relation.relnamespace
      JOIN relation_names expected ON expected.name = relation.relname
      WHERE namespace.nspname = 'public'
      UNION ALL
      SELECT
        'index:' || relation.relname || ':' || index_relation.relname,
        replace(
          pg_catalog.pg_get_indexdef(index_row.indexrelid), 'public.', ''
        ) || ':' ||
          index_row.indisunique::TEXT || ':' ||
          index_row.indisprimary::TEXT || ':' ||
          index_row.indisvalid::TEXT || ':' ||
          index_row.indisready::TEXT || ':' ||
          index_row.indislive::TEXT || ':' ||
          index_row.indisreplident::TEXT || ':' ||
          index_row.indimmediate::TEXT || ':' ||
          index_row.indnullsnotdistinct::TEXT || ':' ||
          index_row.indnatts::TEXT || ':' || index_row.indnkeyatts::TEXT || ':' ||
          index_relation.relpersistence::TEXT || ':' ||
          COALESCE(backing_constraint.contype::TEXT, '') || ':' ||
          COALESCE(backing_constraint.conname, '') || ':' ||
          COALESCE(
            replace(
              pg_catalog.pg_get_constraintdef(backing_constraint.oid, TRUE),
              'public.', ''
            ), ''
          )
      FROM pg_catalog.pg_index index_row
      JOIN pg_catalog.pg_class relation
        ON relation.oid = index_row.indrelid
      JOIN pg_catalog.pg_class index_relation
        ON index_relation.oid = index_row.indexrelid
      JOIN pg_catalog.pg_namespace namespace
        ON namespace.oid = relation.relnamespace
      JOIN relation_names expected ON expected.name = relation.relname
      LEFT JOIN pg_catalog.pg_constraint backing_constraint
        ON backing_constraint.conindid = index_row.indexrelid
      WHERE namespace.nspname = 'public'
    )
    SELECT
      count(*),
      pg_catalog.md5(pg_catalog.string_agg(
        object_key || '=' || definition,
        E'\n' ORDER BY object_key
      ))
    INTO actual_object_count, actual_definition_digest
    FROM objects;

    IF actual_object_count <> 116
       OR actual_definition_digest <>
            'db6081595739bd253ec04772246e23b9' THEN
      RAISE EXCEPTION
        'reference catalog cutover marker-present definition mismatch (% objects, digest %)',
        actual_object_count, actual_definition_digest;
    END IF;
  END IF;
END
$reference_catalog_cutover_rerun_preflight$;

CREATE OR REPLACE FUNCTION public.aircraft_serial_natural_sort_key(serial_value TEXT)
RETURNS TEXT
LANGUAGE plpgsql
IMMUTABLE
STRICT
PARALLEL SAFE
SET search_path = pg_catalog
AS $function$
DECLARE
  normalized TEXT := UPPER(regexp_replace(serial_value, '[^A-Za-z0-9]', '', 'g'));
  encoded TEXT := '01';
  segment TEXT;
  significant TEXT;
  segment_is_numeric BOOLEAN;
  position INTEGER := 1;
  segment_end INTEGER;
  alpha_position INTEGER;
BEGIN
  IF normalized = '' THEN RETURN ''; END IF;
  WHILE position <= length(normalized) LOOP
    segment_is_numeric := substr(normalized, position, 1) ~ '^[0-9]$';
    segment_end := position + 1;
    WHILE segment_end <= length(normalized)
      AND (substr(normalized, segment_end, 1) ~ '^[0-9]$') = segment_is_numeric
    LOOP
      segment_end := segment_end + 1;
    END LOOP;
    segment := substr(normalized, position, segment_end - position);
    IF segment_is_numeric THEN
      significant := ltrim(segment, '0');
      IF significant = '' THEN significant := '0'; END IF;
      encoded := encoded || '20'
        || lpad(upper(to_hex(length(significant))), 8, '0') || significant
        || lpad(upper(to_hex(length(segment))), 8, '0') || segment;
    ELSE
      encoded := encoded || '10';
      alpha_position := 1;
      WHILE alpha_position <= length(segment) LOOP
        encoded := encoded || lpad(upper(to_hex(
          ascii(substr(segment, alpha_position, 1)) - ascii('A') + 1
        )), 2, '0');
        alpha_position := alpha_position + 1;
      END LOOP;
      encoded := encoded || '00';
    END IF;
    position := segment_end;
  END LOOP;
  RETURN encoded || '00';
END
$function$;

DO $serial_preflight$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM public.aircraft_reference_applicability_scopes scope
    LEFT JOIN public.aircraft_serial_number_schemes scheme
      ON scheme.id = scope.aircraft_serial_number_scheme_id
    WHERE NOT scope.applies_to_all_serials
      AND (
        scheme.normalization_version IS DISTINCT FROM 'natural_alphanumeric_segments_v1'
        OR scope.serial_from_display !~ '^[A-Z0-9]+$'
        OR scope.serial_to_display !~ '^[A-Z0-9]+$'
        OR scope.serial_from_sort_key IS DISTINCT FROM
             public.aircraft_serial_natural_sort_key(scope.serial_from_display)
        OR scope.serial_to_sort_key IS DISTINCT FROM
             public.aircraft_serial_natural_sort_key(scope.serial_to_display)
      )
  ) THEN
    RAISE EXCEPTION 'bounded reference applicability must be republished with universal natural-order serial keys before cutover';
  END IF;
END
$serial_preflight$;

DELETE FROM public.aircraft_serial_number_schemes old_scheme
WHERE old_scheme.normalization_version <> 'natural_alphanumeric_segments_v1'
  AND NOT EXISTS (
    SELECT 1 FROM public.aircraft_reference_applicability_scopes scope
    WHERE scope.aircraft_serial_number_scheme_id = old_scheme.id
  )
  AND EXISTS (
    SELECT 1 FROM public.aircraft_serial_number_schemes replacement
    WHERE replacement.aircraft_make_id = old_scheme.aircraft_make_id
      AND replacement.name = old_scheme.name
      AND replacement.normalization_version = 'natural_alphanumeric_segments_v1'
  );
UPDATE public.aircraft_serial_number_schemes
SET normalization_version = 'natural_alphanumeric_segments_v1'
WHERE normalization_version <> 'natural_alphanumeric_segments_v1';

CREATE OR REPLACE FUNCTION public.validate_aircraft_serial_scheme_ordering()
RETURNS TRIGGER AS $function$
BEGIN
  IF NEW.normalization_version <> 'natural_alphanumeric_segments_v1' THEN
    RAISE EXCEPTION 'serial schemes require the universal ordering version';
  END IF;
  RETURN NEW;
END;
$function$ LANGUAGE plpgsql
SET search_path = pg_catalog;
DROP TRIGGER IF EXISTS aircraft_serial_schemes_universal_order
  ON public.aircraft_serial_number_schemes;
CREATE TRIGGER aircraft_serial_schemes_universal_order
BEFORE INSERT OR UPDATE OF normalization_version ON public.aircraft_serial_number_schemes
FOR EACH ROW EXECUTE FUNCTION public.validate_aircraft_serial_scheme_ordering();

-- The reference catalog is the only aircraft configuration/value authority.
-- Replace surviving functions first so PostgreSQL releases their dependencies
-- on the relations removed below.
CREATE OR REPLACE FUNCTION public.prevent_referenced_avionics_catalog_downgrade()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF NEW.catalog_status <> 'approved' AND (
    EXISTS (
      SELECT 1
      FROM public.aircraft_sale_listing_avionics listing_link
      WHERE listing_link.avionics_model_id = OLD.id
         OR listing_link.replaces_avionics_model_id = OLD.id
    )
    OR EXISTS (
      SELECT 1
      FROM public.avionics_suite_components suite_link
      WHERE suite_link.suite_model_id = OLD.id
         OR suite_link.component_model_id = OLD.id
    )
    OR EXISTS (
      SELECT 1
      FROM public.aircraft_reference_avionics reference_link
      WHERE reference_link.avionics_model_id = OLD.id
    )
  ) THEN
    RAISE EXCEPTION 'referenced avionics catalog entry cannot be unapproved';
  END IF;
  RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_authorization_for_capture()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_authorizations authorization_row
  USING public.aircraft_sale_listing_avionics link
  WHERE link.id = authorization_row.listing_link_id
    AND authorization_row.evidence_capture_sha256 = OLD.rendered_html_sha256
    AND link.aircraft_sale_listing_id = OLD.canonical_listing_id
    AND length(BTRIM(COALESCE(link.source_notes, ''))) > 0
    AND position(link.source_notes IN OLD.rendered_html) > 0
    AND NOT EXISTS (
      SELECT 1 FROM public.plugin_submissions retained_capture
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

CREATE OR REPLACE FUNCTION public.validate_aircraft_valuation_compatibility_projection()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.aircraft_valuation_projection_transitions transition
    JOIN public.aircraft_sale_listing_identity_assignments assignment
      ON assignment.id = transition.identity_assignment_id
     AND assignment.aircraft_sale_listing_id = transition.aircraft_sale_listing_id
    JOIN public.aircraft_makes make ON make.id = assignment.aircraft_make_id
    JOIN public.aircraft_model_families family
      ON family.id = assignment.aircraft_model_family_id
     AND family.aircraft_make_id = make.id
    JOIN public.aircraft_designations designation
      ON designation.id = assignment.aircraft_designation_id
     AND designation.aircraft_model_family_id = family.id
    LEFT JOIN public.aircraft_generations generation
      ON generation.id = assignment.aircraft_generation_id
     AND generation.aircraft_model_family_id = family.id
    LEFT JOIN public.aircraft_factory_packages package
      ON package.id = assignment.aircraft_factory_package_id
     AND package.aircraft_model_family_id = family.id
    JOIN public.aircraft_model_variants legacy_variant
      ON legacy_variant.id = NEW.aircraft_model_variant_id
    JOIN public.aircraft_models legacy_model
      ON legacy_model.id = legacy_variant.aircraft_model_id
    JOIN public.aircraft_manufacturers legacy_manufacturer
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
          SELECT 1 FROM public.aircraft_generation_designations applicability
          WHERE applicability.aircraft_generation_id = assignment.aircraft_generation_id
            AND applicability.aircraft_designation_id = assignment.aircraft_designation_id
        )
      )
      AND (
        assignment.aircraft_factory_package_id IS NULL
        OR EXISTS (
          SELECT 1 FROM public.aircraft_package_applicability applicability
          WHERE applicability.aircraft_factory_package_id = assignment.aircraft_factory_package_id
            AND applicability.aircraft_designation_id = assignment.aircraft_designation_id
            AND (
              applicability.aircraft_generation_id IS NULL
              OR applicability.aircraft_generation_id IS NOT DISTINCT FROM assignment.aircraft_generation_id
            )
        )
      )
      AND NOT EXISTS (
        SELECT 1 FROM public.aircraft_sale_listings child
        WHERE child.aircraft_model_variant_id = legacy_variant.id
      )
      AND NOT EXISTS (
        SELECT 1 FROM public.rental_aircraft_offerings child
        WHERE child.aircraft_model_variant_id = legacy_variant.id
      )
  ) THEN
    RAISE EXCEPTION 'aircraft compatibility projection requires the active command, exact copied assignment provenance, and its fresh reserved hierarchy';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TABLE IF EXISTS public.aircraft_model_variant_default_avionics_candidates;
DROP TABLE IF EXISTS public.aircraft_model_variant_default_avionics;
DROP TABLE IF EXISTS public.aircraft_model_variant_price_points;
DROP TABLE IF EXISTS public.aircraft_model_spec_versions;
DROP TABLE IF EXISTS public.depreciation_profile_fit_metadata;
DROP TABLE IF EXISTS public.component_depreciation_profiles;
DROP TABLE IF EXISTS public.depreciation_profiles;

DROP FUNCTION IF EXISTS public.require_approved_default_avionics_model();
DROP FUNCTION IF EXISTS public.reject_active_default_avionics_candidate();
DROP FUNCTION IF EXISTS public.preserve_pending_default_avionics_claim();
DROP FUNCTION IF EXISTS public.require_exact_pending_default_avionics_admission();
DROP FUNCTION IF EXISTS public.move_admitted_default_avionics_candidate();
DROP FUNCTION IF EXISTS public.prevent_projected_aircraft_evidence_variant_move();

ALTER TABLE public.aircraft_reference_prices
  ADD COLUMN IF NOT EXISTS configuration_basis TEXT NOT NULL DEFAULT 'unknown'
  CHECK (configuration_basis IN (
    'full_standard_configuration', 'base_aircraft_only', 'unknown'
  ));

-- PostgreSQL resolves NEW fields while planning boolean expressions, even when
-- a TG_TABLE_NAME predicate would make the field unreachable. Keep the exact
-- component-identifier check inside its table-specific branch so this shared
-- trigger also works for canonical tables without identity_evidence_claim_id.
CREATE OR REPLACE FUNCTION public.require_aircraft_catalog_approval()
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
    FROM public.aircraft_identity_decisions decision
    WHERE decision.id = NEW.approval_decision_id
      AND decision.decision_status = 'approved'
      AND decision.decision_action = 'approve_new'
      AND decision.entity_kind = expected_kind
  ) THEN
    RAISE EXCEPTION '% requires an approved % decision', TG_TABLE_NAME, expected_kind;
  END IF;

  IF require_claim AND NOT EXISTS (
    SELECT 1
    FROM public.aircraft_identity_decision_claims decision_claim
    JOIN public.curation_evidence_claims claim
      ON claim.id = decision_claim.evidence_claim_id
    JOIN public.curation_evidence_sources source
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
      FROM public.aircraft_identity_decision_claims decision_claim
      JOIN public.curation_evidence_claims claim
        ON claim.id = decision_claim.evidence_claim_id
      JOIN public.curation_evidence_sources source
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
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;

CREATE OR REPLACE FUNCTION public.validate_aircraft_reference_version_insert()
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
    FROM public.aircraft_reference_configuration_versions previous
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
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;

CREATE OR REPLACE FUNCTION public.preserve_assigned_aircraft_applicability()
RETURNS TRIGGER AS $$
BEGIN
  IF TG_TABLE_NAME = 'aircraft_generation_designations' THEN
    IF EXISTS (
      SELECT 1 FROM public.aircraft_sale_listing_identity_assignments assignment
      WHERE assignment.aircraft_generation_id = OLD.aircraft_generation_id
        AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
    ) THEN
      RAISE EXCEPTION 'assigned generation/designation applicability is immutable';
    END IF;
  END IF;
  IF TG_TABLE_NAME = 'aircraft_package_applicability' THEN
    IF EXISTS (
      SELECT 1 FROM public.aircraft_sale_listing_identity_assignments assignment
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
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;

CREATE OR REPLACE FUNCTION public.prevent_new_unresolved_aircraft_dimension()
RETURNS TRIGGER AS $$
BEGIN
  IF TG_TABLE_NAME = 'aircraft_generation_designations' THEN
    IF EXISTS (
      SELECT 1
      FROM public.aircraft_sale_listing_current_identity_assignments current_assignment
      JOIN public.aircraft_sale_listing_identity_assignments assignment
        ON assignment.id = current_assignment.identity_assignment_id
       AND assignment.aircraft_sale_listing_id = current_assignment.aircraft_sale_listing_id
      JOIN public.aircraft_sale_listings listing
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
      FROM public.aircraft_factory_packages package
      CROSS JOIN public.aircraft_sale_listing_current_identity_assignments current_assignment
      JOIN public.aircraft_sale_listing_identity_assignments assignment
        ON assignment.id = current_assignment.identity_assignment_id
       AND assignment.aircraft_sale_listing_id = current_assignment.aircraft_sale_listing_id
      JOIN public.aircraft_sale_listings listing
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
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;

CREATE TABLE IF NOT EXISTS public.aircraft_reference_fact_set_attestations (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  aircraft_reference_configuration_version_id BIGINT NOT NULL
    REFERENCES public.aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  fact_set_kind TEXT NOT NULL CHECK (fact_set_kind IN (
    'avionics', 'engines', 'propellers', 'features'
  )),
  evidence_claim_id BIGINT NOT NULL
    REFERENCES public.curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, fact_set_kind)
);

CREATE TABLE IF NOT EXISTS public.official_dollar_normalization_facts (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  source_year BIGINT NOT NULL CHECK (source_year BETWEEN 1900 AND 2200),
  target_year BIGINT NOT NULL CHECK (target_year BETWEEN 1900 AND 2200),
  index_series TEXT NOT NULL CHECK (length(BTRIM(index_series)) > 0),
  source_index_value DOUBLE PRECISION NOT NULL CHECK (source_index_value > 0),
  target_index_value DOUBLE PRECISION NOT NULL CHECK (target_index_value > 0),
  normalization_factor DOUBLE PRECISION NOT NULL CHECK (normalization_factor > 0),
  evidence_claim_id BIGINT NOT NULL UNIQUE
    REFERENCES public.curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (source_year, target_year),
  CHECK (source_year <> target_year),
  CHECK (
    abs(normalization_factor - (target_index_value / source_index_value))
      <= 0.000000001
  )
);

CREATE OR REPLACE FUNCTION public.validate_official_dollar_normalization_fact()
RETURNS TRIGGER AS $$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.curation_evidence_claims claim
    JOIN public.curation_evidence_sources source ON source.id = claim.evidence_source_id
    WHERE claim.id = NEW.evidence_claim_id
      AND claim.validation_status = 'validated'
      AND claim.claim_kind IN ('price', 'specification')
      AND source.source_tier = 'regulator_primary'
  ) THEN
    RAISE EXCEPTION 'dollar normalization requires validated official regulator evidence';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;
DROP TRIGGER IF EXISTS official_dollar_normalization_require_evidence
  ON public.official_dollar_normalization_facts;
CREATE TRIGGER official_dollar_normalization_require_evidence
BEFORE INSERT ON public.official_dollar_normalization_facts
FOR EACH ROW EXECUTE FUNCTION public.validate_official_dollar_normalization_fact();
CREATE OR REPLACE FUNCTION public.prevent_official_dollar_normalization_mutation()
RETURNS TRIGGER AS $$
BEGIN
  RAISE EXCEPTION 'official dollar normalization facts are immutable';
END;
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;
DROP TRIGGER IF EXISTS official_dollar_normalization_immutable
  ON public.official_dollar_normalization_facts;
CREATE TRIGGER official_dollar_normalization_immutable
BEFORE UPDATE OR DELETE ON public.official_dollar_normalization_facts
FOR EACH ROW EXECUTE FUNCTION public.prevent_official_dollar_normalization_mutation();

CREATE OR REPLACE FUNCTION public.validate_aircraft_reference_child_insert()
RETURNS TRIGGER AS $$
DECLARE
  parent_state TEXT;
  parent_model_year BIGINT;
  expected_value_type TEXT;
BEGIN
  SELECT publication_state, model_year INTO parent_state, parent_model_year
  FROM public.aircraft_reference_configuration_versions
  WHERE id = NEW.aircraft_reference_configuration_version_id;
  IF parent_state IS DISTINCT FROM 'building' THEN
    RAISE EXCEPTION 'reference profile children require a building version';
  END IF;
  IF TG_TABLE_NAME = 'aircraft_reference_applicability_scopes' THEN
    IF NOT NEW.applies_to_all_serials AND (
      NEW.serial_from_display !~ '^[A-Z0-9]+$'
      OR NEW.serial_to_display !~ '^[A-Z0-9]+$'
      OR NEW.serial_from_sort_key IS DISTINCT FROM
           public.aircraft_serial_natural_sort_key(NEW.serial_from_display)
      OR NEW.serial_to_sort_key IS DISTINCT FROM
           public.aircraft_serial_natural_sort_key(NEW.serial_to_display)
      OR NEW.serial_from_sort_key COLLATE "C"
           > NEW.serial_to_sort_key COLLATE "C"
      OR (NEW.serial_prefix IS NOT NULL AND (
        NEW.serial_prefix !~ '^[A-Z0-9]+$'
        OR NEW.serial_from_display NOT LIKE NEW.serial_prefix || '%'
        OR NEW.serial_to_display NOT LIKE NEW.serial_prefix || '%'
      ))
      OR NOT EXISTS (
        SELECT 1 FROM public.aircraft_serial_number_schemes scheme
        WHERE scheme.id = NEW.aircraft_serial_number_scheme_id
          AND scheme.normalization_version = 'natural_alphanumeric_segments_v1'
      )
    ) THEN
      RAISE EXCEPTION 'reference serial applicability requires canonical sort keys';
    END IF;
  ELSIF TG_TABLE_NAME = 'aircraft_reference_avionics' THEN
    IF NOT EXISTS (
      SELECT 1 FROM public.avionics_models model
      WHERE model.id = NEW.avionics_model_id AND model.catalog_status = 'approved'
    ) THEN
      RAISE EXCEPTION 'reference avionics requires an approved catalog product';
    END IF;
  ELSIF TG_TABLE_NAME = 'aircraft_reference_engines' THEN
    IF NOT EXISTS (
      SELECT 1 FROM public.aircraft_engine_catalog_models model
      WHERE model.id = NEW.aircraft_engine_catalog_model_id
        AND model.catalog_status = 'approved'
    ) THEN
      RAISE EXCEPTION 'reference engine requires an approved catalog model';
    END IF;
  ELSIF TG_TABLE_NAME = 'aircraft_reference_propellers' THEN
    IF NOT EXISTS (
      SELECT 1 FROM public.aircraft_propeller_catalog_models model
      WHERE model.id = NEW.aircraft_propeller_catalog_model_id
        AND model.catalog_status = 'approved'
    ) THEN
      RAISE EXCEPTION 'reference propeller requires an approved catalog model';
    END IF;
  ELSIF TG_TABLE_NAME = 'aircraft_reference_features' THEN
    SELECT value_type INTO expected_value_type
    FROM public.aircraft_feature_definitions
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
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;

CREATE OR REPLACE FUNCTION public.prevent_aircraft_reference_fact_mutation()
RETURNS TRIGGER AS $$
DECLARE
  parent_id BIGINT;
  parent_state TEXT;
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF TG_TABLE_NAME = 'aircraft_reference_avionics'
       AND NEW.id = OLD.id
       AND NEW.aircraft_reference_configuration_version_id
         = OLD.aircraft_reference_configuration_version_id
       AND NEW.avionics_model_id IS DISTINCT FROM OLD.avionics_model_id
       AND NEW.quantity = OLD.quantity
       AND NEW.equipment_role = OLD.equipment_role
       AND NEW.evidence_claim_id = OLD.evidence_claim_id
       AND NEW.created_at = OLD.created_at
       AND EXISTS (
         SELECT 1
         FROM public.avionics_catalog_authorized_consolidations guard
         JOIN public.avionics_models survivor
           ON survivor.id = guard.survivor_model_id
         JOIN public.avionics_models legacy
           ON legacy.id = guard.duplicate_model_id
         WHERE guard.duplicate_model_id = OLD.avionics_model_id
           AND guard.survivor_model_id = NEW.avionics_model_id
       ) THEN
      RETURN NEW;
    END IF;
    RAISE EXCEPTION 'reference profile facts are immutable; publish a replacement version';
  END IF;
  parent_id := OLD.aircraft_reference_configuration_version_id;
  SELECT publication_state INTO parent_state
  FROM public.aircraft_reference_configuration_versions
  WHERE id = parent_id;
  IF parent_state IS DISTINCT FROM 'building' THEN
    RAISE EXCEPTION 'published reference profile facts are immutable';
  END IF;
  RETURN OLD;
END;
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;

DROP TRIGGER IF EXISTS aircraft_reference_fact_set_building_insert
  ON public.aircraft_reference_fact_set_attestations;
CREATE TRIGGER aircraft_reference_fact_set_building_insert
BEFORE INSERT ON public.aircraft_reference_fact_set_attestations
FOR EACH ROW EXECUTE FUNCTION public.validate_aircraft_reference_child_insert();
DROP TRIGGER IF EXISTS aircraft_reference_fact_set_immutable
  ON public.aircraft_reference_fact_set_attestations;
CREATE TRIGGER aircraft_reference_fact_set_immutable
BEFORE UPDATE OR DELETE ON public.aircraft_reference_fact_set_attestations
FOR EACH ROW EXECUTE FUNCTION public.prevent_aircraft_reference_fact_mutation();

CREATE OR REPLACE FUNCTION public.validate_aircraft_reference_version_update()
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
    IF NOT EXISTS (
      SELECT 1 FROM public.aircraft_reference_applicability_scopes scope
      WHERE scope.aircraft_reference_configuration_version_id = NEW.id
    ) THEN
      RAISE EXCEPTION 'published reference profile requires applicability';
    END IF;
    IF 4 <> (
      SELECT COUNT(*) FROM public.aircraft_reference_fact_set_attestations attestation
      WHERE attestation.aircraft_reference_configuration_version_id = NEW.id
    ) THEN
      RAISE EXCEPTION 'published reference profile requires complete factory fact-set attestations';
    END IF;
    IF 1 <> (
      SELECT COUNT(*) FROM public.aircraft_reference_prices price
      JOIN public.curation_evidence_claims claim ON claim.id = price.evidence_claim_id
      JOIN public.curation_evidence_sources source ON source.id = claim.evidence_source_id
      WHERE price.aircraft_reference_configuration_version_id = NEW.id
        AND price.price_kind = 'equipped_msrp'
        AND price.currency = 'USD' AND price.evidence_kind = 'direct_model_year'
        AND price.configuration_basis = 'full_standard_configuration'
        AND claim.claim_kind = 'price'
        AND claim.validation_status = 'validated'
        AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
    ) THEN RAISE EXCEPTION 'published profile requires exactly one direct exact-model-year full-configuration equipped MSRP with primary price evidence'; END IF;
    IF EXISTS (
      SELECT 1
      FROM public.aircraft_reference_engines engine
      LEFT JOIN public.aircraft_engine_catalog_models model
        ON model.id = engine.aircraft_engine_catalog_model_id
       AND model.catalog_status = 'approved'
      WHERE engine.aircraft_reference_configuration_version_id = NEW.id
        AND model.id IS NULL
    ) THEN RAISE EXCEPTION 'published profile requires approved engine catalog models'; END IF;
    IF EXISTS (
      SELECT 1
      FROM public.aircraft_reference_propellers propeller
      LEFT JOIN public.aircraft_propeller_catalog_models model
        ON model.id = propeller.aircraft_propeller_catalog_model_id
       AND model.catalog_status = 'approved'
      WHERE propeller.aircraft_reference_configuration_version_id = NEW.id
        AND model.id IS NULL
    ) THEN RAISE EXCEPTION 'published profile requires approved propeller catalog models'; END IF;
    IF EXISTS (
      SELECT 1 FROM (
        SELECT evidence_claim_id, 'applicability' AS evidence_domain FROM public.aircraft_reference_applicability_scopes WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id, 'price' FROM public.aircraft_reference_prices WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id, 'factory' FROM public.aircraft_reference_avionics WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id, 'factory' FROM public.aircraft_reference_engines WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id, 'factory' FROM public.aircraft_reference_propellers WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id, 'factory' FROM public.aircraft_reference_features WHERE aircraft_reference_configuration_version_id = NEW.id
        UNION ALL SELECT evidence_claim_id, 'factory' FROM public.aircraft_reference_fact_set_attestations WHERE aircraft_reference_configuration_version_id = NEW.id
      ) fact
      JOIN public.curation_evidence_claims claim ON claim.id = fact.evidence_claim_id
      JOIN public.curation_evidence_sources source ON source.id = claim.evidence_source_id
      WHERE claim.validation_status <> 'validated'
         OR source.source_tier NOT IN ('manufacturer_primary', 'regulator_primary')
         OR (fact.evidence_domain = 'applicability' AND claim.claim_kind <> 'applicability')
         OR (fact.evidence_domain = 'price' AND claim.claim_kind <> 'price')
         OR (fact.evidence_domain = 'factory' AND claim.claim_kind NOT IN ('standard_equipment', 'package_composition', 'specification'))
    ) THEN RAISE EXCEPTION 'published reference profile facts require validated primary evidence'; END IF;
    IF EXISTS (
      SELECT 1 FROM public.aircraft_reference_applicability_scopes left_scope
      JOIN public.aircraft_reference_applicability_scopes right_scope
        ON right_scope.aircraft_reference_configuration_version_id = left_scope.aircraft_reference_configuration_version_id
       AND right_scope.id > left_scope.id AND right_scope.aircraft_market_id = left_scope.aircraft_market_id
      WHERE left_scope.aircraft_reference_configuration_version_id = NEW.id
        AND (left_scope.applies_to_all_serials OR right_scope.applies_to_all_serials OR (
          left_scope.serial_from_sort_key COLLATE "C"
            <= right_scope.serial_to_sort_key COLLATE "C"
          AND right_scope.serial_from_sort_key COLLATE "C"
            <= left_scope.serial_to_sort_key COLLATE "C")))
    THEN RAISE EXCEPTION 'reference profile contains overlapping applicability scopes'; END IF;
    IF EXISTS (
      SELECT 1 FROM public.aircraft_reference_applicability_scopes candidate
      JOIN public.aircraft_markets candidate_market ON candidate_market.id = candidate.aircraft_market_id
      JOIN public.aircraft_reference_applicability_scopes existing
        ON existing.aircraft_market_id = candidate.aircraft_market_id
        OR candidate_market.code = 'GLOBAL'
        OR EXISTS (SELECT 1 FROM public.aircraft_markets existing_market WHERE existing_market.id = existing.aircraft_market_id AND existing_market.code = 'GLOBAL')
      JOIN public.aircraft_reference_configuration_versions existing_version ON existing_version.id = existing.aircraft_reference_configuration_version_id
      WHERE candidate.aircraft_reference_configuration_version_id = NEW.id
        AND existing_version.id <> NEW.id
        AND existing_version.aircraft_reference_configuration_id = NEW.aircraft_reference_configuration_id
        AND existing_version.model_year = NEW.model_year AND existing_version.publication_state = 'published'
        AND (candidate.applies_to_all_serials OR existing.applies_to_all_serials OR (
          candidate.serial_from_sort_key COLLATE "C"
            <= existing.serial_to_sort_key COLLATE "C"
          AND existing.serial_from_sort_key COLLATE "C"
            <= candidate.serial_to_sort_key COLLATE "C")))
    THEN RAISE EXCEPTION 'reference profile applicability overlaps an existing published version'; END IF;
  ELSIF NEW.publication_state <> 'building' THEN
    RAISE EXCEPTION 'invalid building profile state transition';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_reference_catalog_cutover', 1,
  '039f72c03b3d2ba9538a4705ce7bda744fe02a322d018895c536604d280fe647', CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
