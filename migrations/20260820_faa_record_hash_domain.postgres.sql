BEGIN;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

-- A legacy projection does not say which field set/domain produced its record
-- hashes. Never infer that metadata or mechanically relabel existing hashes.
-- Operators must discard a nonempty legacy projection and regenerate it from
-- the exact retained FAA archive after this schema transition.
DO $pre_domain_guard$
DECLARE
  marker_installed BOOLEAN;
BEGIN
  IF pg_catalog.to_regclass('public.faa_registry_snapshots') IS NULL THEN
    RAISE EXCEPTION 'FAA registry snapshot relation is missing';
  END IF;

  IF NOT EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
      AND contract_version = 1
      AND contract_fingerprint =
        'fc6451ffe8e1ee2034e76480767d16d6c37463461d9e684687448b4d43f96bef'
  ) THEN
    RAISE EXCEPTION 'exact FAA reference reachability prerequisite is missing';
  END IF;

  IF EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260820_faa_record_hash_domain'
      AND NOT (
        contract_version = 1
        AND contract_fingerprint =
          'f124f573bf705da6c1e4b0a5c7a8df45ea5a4a5dc009a28eee012be42c691502'
      )
  ) THEN
    RAISE EXCEPTION 'installed FAA record hash domain migration has a different contract';
  END IF;

  SELECT EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260820_faa_record_hash_domain'
  ) INTO marker_installed;

  IF (
    SELECT count(*)
    FROM pg_catalog.pg_class relation
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    WHERE relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft', 'faa_registry_aircraft_references',
        'faa_registry_coverage', 'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
      AND relation.relkind = 'r'
      AND relation.relpersistence = 'p'
      AND NOT relation.relrowsecurity
      AND NOT relation.relforcerowsecurity
      AND NOT relation.relispartition
      AND NOT relation.relhasrules
  ) <> 5 OR (
    SELECT count(*)
    FROM pg_catalog.pg_attribute attribute
    JOIN pg_catalog.pg_class relation ON relation.oid = attribute.attrelid
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    WHERE relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft', 'faa_registry_aircraft_references',
        'faa_registry_coverage', 'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
      AND attribute.attnum > 0
      AND NOT attribute.attisdropped
  ) <> (CASE WHEN marker_installed THEN 47 ELSE 46 END) OR (
    SELECT count(*)
    FROM pg_catalog.pg_constraint constraint_row
    JOIN pg_catalog.pg_namespace constraint_namespace
      ON constraint_namespace.oid = constraint_row.connamespace
    JOIN pg_catalog.pg_class relation ON relation.oid = constraint_row.conrelid
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    WHERE constraint_namespace.nspname = 'public'
      AND relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft', 'faa_registry_aircraft_references',
        'faa_registry_coverage', 'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
  ) <> (CASE WHEN marker_installed THEN 40 ELSE 39 END) OR (
    SELECT count(*)
    FROM pg_catalog.pg_index index_row
    JOIN pg_catalog.pg_class relation ON relation.oid = index_row.indrelid
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    WHERE relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft', 'faa_registry_aircraft_references',
        'faa_registry_coverage', 'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
  ) <> 12 OR (
    SELECT count(*)
    FROM pg_catalog.pg_trigger trigger_row
    JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    WHERE relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft', 'faa_registry_aircraft_references',
        'faa_registry_coverage', 'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
      AND NOT trigger_row.tgisinternal
  ) <> 9 OR (
    SELECT count(*)
    FROM pg_catalog.pg_proc procedure
    JOIN pg_catalog.pg_namespace procedure_namespace
      ON procedure_namespace.oid = procedure.pronamespace
    WHERE procedure_namespace.nspname = 'public'
      AND procedure.proname IN (
        'preserve_faa_registry_data',
        'validate_faa_aircraft_reference_reachability',
        'validate_faa_coverage',
        'validate_faa_engine_reference_reachability',
        'validate_faa_snapshot_evidence'
      )
      AND procedure.prokind = 'f'
      AND procedure.pronargs = 0
      AND procedure.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype
      AND NOT procedure.prosecdef
      AND NOT procedure.proisstrict
      AND procedure.provolatile = 'v'
      AND procedure.proparallel = 'u'
      AND procedure.proconfig = ARRAY['search_path=pg_catalog']::TEXT[]
  ) <> 5 THEN
    RAISE EXCEPTION 'FAA projection is not the exact reachability prerequisite shape';
  END IF;

  IF (
    WITH expected(function_name, function_source) AS (
      VALUES
        ('validate_faa_snapshot_evidence', $snapshot_function$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM public.curation_evidence_sources source
    WHERE source.id = NEW.evidence_source_id
      AND source.source_domain = 'faa.gov'
      AND source.source_tier = 'regulator_primary'
      AND source.source_url = NEW.source_url
      AND source.content_sha256 = NEW.archive_sha256
  ) THEN
    RAISE EXCEPTION 'FAA snapshot requires exact regulator evidence provenance';
  END IF;
  RETURN NEW;
END;
$snapshot_function$),
        ('validate_faa_coverage', $coverage_function$
BEGIN
  IF (NEW.lookup_status = 'matched' AND NOT EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) OR (NEW.lookup_status = 'absent' AND EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) THEN
    RAISE EXCEPTION 'FAA coverage must agree with its target match';
  END IF;
  RETURN NEW;
END;
$coverage_function$),
        ('preserve_faa_registry_data', $immutability_function$
BEGIN
  RAISE EXCEPTION 'FAA registry snapshots and projections are immutable';
END;
$immutability_function$),
        ('validate_faa_aircraft_reference_reachability', $aircraft_function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.aircraft_code = NEW.aircraft_code
  ) THEN
    RAISE EXCEPTION
      'FAA aircraft reference must be reachable from a target match';
  END IF;
  RETURN NEW;
END;
$aircraft_function$),
        ('validate_faa_engine_reference_reachability', $engine_function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.engine_code = NEW.engine_code
  ) THEN
    RAISE EXCEPTION
      'FAA engine reference must be reachable from a target match';
  END IF;
  RETURN NEW;
END;
$engine_function$)
    )
    SELECT count(*)
    FROM expected
    JOIN pg_catalog.pg_proc procedure
      ON procedure.proname = expected.function_name
    JOIN pg_catalog.pg_namespace procedure_namespace
      ON procedure_namespace.oid = procedure.pronamespace
    JOIN pg_catalog.pg_language language
      ON language.oid = procedure.prolang
    WHERE procedure_namespace.nspname = 'public'
      AND procedure.prosrc = expected.function_source
      AND procedure.prokind = 'f'
      AND procedure.pronargs = 0
      AND procedure.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype
      AND language.lanname = 'plpgsql'
      AND NOT procedure.prosecdef
      AND NOT procedure.proisstrict
      AND procedure.provolatile = 'v'
      AND procedure.proparallel = 'u'
      AND procedure.proconfig = ARRAY['search_path=pg_catalog']::TEXT[]
  ) <> 5 THEN
    RAISE EXCEPTION 'FAA projection functions are not the exact prerequisite contract';
  END IF;

  IF (
    SELECT pg_catalog.string_agg(
      pg_catalog.format(
        '%s|%s|%s|%s|%s', relation.relname,
        constraint_row.contype, constraint_row.conname,
        constraint_row.convalidated,
        pg_catalog.pg_get_constraintdef(constraint_row.oid, FALSE)
      ), E'\n' ORDER BY relation.relname,
        constraint_row.contype, constraint_row.conname
    )
    FROM pg_catalog.pg_constraint constraint_row
    JOIN pg_catalog.pg_class relation
      ON relation.oid = constraint_row.conrelid
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    WHERE relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft', 'faa_registry_aircraft_references',
        'faa_registry_coverage', 'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
      AND constraint_row.contype <> 'f'
  ) IS DISTINCT FROM (CASE WHEN marker_installed THEN
    $current_constraints$faa_registry_aircraft|c|faa_registry_aircraft_aircraft_code_check|t|CHECK ((length(TRIM(BOTH FROM aircraft_code)) > 0))
faa_registry_aircraft|c|faa_registry_aircraft_engine_code_check|t|CHECK (((engine_code IS NULL) OR (length(TRIM(BOTH FROM engine_code)) > 0)))
faa_registry_aircraft|c|faa_registry_aircraft_manufacturer_serial_key_check|t|CHECK (((manufacturer_serial_key IS NULL) OR (length(manufacturer_serial_key) > 0)))
faa_registry_aircraft|c|faa_registry_aircraft_manufacturer_serial_raw_check|t|CHECK (((manufacturer_serial_raw IS NULL) OR (length(TRIM(BOTH FROM manufacturer_serial_raw)) > 0)))
faa_registry_aircraft|c|faa_registry_aircraft_n_number_check|t|CHECK ((("left"(n_number, 1) = 'N'::text) AND ((length(n_number) >= 2) AND (length(n_number) <= 6))))
faa_registry_aircraft|c|faa_registry_aircraft_source_record_sha256_check|t|CHECK ((source_record_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_aircraft|c|faa_registry_aircraft_year_manufactured_check|t|CHECK (((year_manufactured IS NULL) OR ((year_manufactured >= 1900) AND (year_manufactured <= 2200))))
faa_registry_aircraft|p|faa_registry_aircraft_pkey|t|PRIMARY KEY (snapshot_id, n_number)
faa_registry_aircraft|u|faa_registry_aircraft_snapshot_id_source_record_sha256_key|t|UNIQUE (snapshot_id, source_record_sha256)
faa_registry_aircraft_references|c|faa_registry_aircraft_references_aircraft_code_check|t|CHECK ((length(TRIM(BOTH FROM aircraft_code)) > 0))
faa_registry_aircraft_references|c|faa_registry_aircraft_references_cruise_speed_mph_check|t|CHECK (((cruise_speed_mph IS NULL) OR (cruise_speed_mph >= 0)))
faa_registry_aircraft_references|c|faa_registry_aircraft_references_engine_count_check|t|CHECK (((engine_count IS NULL) OR (engine_count >= 0)))
faa_registry_aircraft_references|c|faa_registry_aircraft_references_seat_count_check|t|CHECK (((seat_count IS NULL) OR (seat_count >= 0)))
faa_registry_aircraft_references|p|faa_registry_aircraft_references_pkey|t|PRIMARY KEY (snapshot_id, aircraft_code)
faa_registry_coverage|c|faa_registry_coverage_lookup_status_check|t|CHECK ((lookup_status = ANY (ARRAY['matched'::text, 'absent'::text])))
faa_registry_coverage|c|faa_registry_coverage_n_number_check|t|CHECK ((("left"(n_number, 1) = 'N'::text) AND ((length(n_number) >= 2) AND (length(n_number) <= 6))))
faa_registry_coverage|p|faa_registry_coverage_pkey|t|PRIMARY KEY (snapshot_id, n_number)
faa_registry_engine_references|c|faa_registry_engine_references_engine_code_check|t|CHECK ((length(TRIM(BOTH FROM engine_code)) > 0))
faa_registry_engine_references|c|faa_registry_engine_references_horsepower_check|t|CHECK (((horsepower IS NULL) OR (horsepower >= 0)))
faa_registry_engine_references|c|faa_registry_engine_references_thrust_pounds_check|t|CHECK (((thrust_pounds IS NULL) OR (thrust_pounds >= 0)))
faa_registry_engine_references|p|faa_registry_engine_references_pkey|t|PRIMARY KEY (snapshot_id, engine_code)
faa_registry_snapshots|c|faa_registry_snapshots_aircraft_member_name_check|t|CHECK ((aircraft_member_name = 'ACFTREF.txt'::text))
faa_registry_snapshots|c|faa_registry_snapshots_aircraft_member_sha256_check|t|CHECK ((aircraft_member_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_archive_sha256_check|t|CHECK ((archive_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_engine_member_name_check|t|CHECK ((engine_member_name = 'ENGINE.txt'::text))
faa_registry_snapshots|c|faa_registry_snapshots_engine_member_sha256_check|t|CHECK ((engine_member_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_master_member_name_check|t|CHECK ((master_member_name = 'MASTER.txt'::text))
faa_registry_snapshots|c|faa_registry_snapshots_master_member_sha256_check|t|CHECK ((master_member_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_record_hash_domain_check|t|CHECK ((record_hash_domain = 'aircost-faa-master-retained-aircraft-projection-v1'::text))
faa_registry_snapshots|c|faa_registry_snapshots_snapshot_date_check|t|CHECK ((snapshot_date ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_source_manifest_sha256_check|t|CHECK ((source_manifest_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_source_url_check|t|CHECK ((source_url ~ '^https://([^. /]+[.])*faa[.]gov/'::text))
faa_registry_snapshots|c|faa_registry_snapshots_target_set_sha256_check|t|CHECK ((target_set_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|p|faa_registry_snapshots_pkey|t|PRIMARY KEY (id)
faa_registry_snapshots|u|faa_registry_snapshots_archive_sha256_target_set_sha256_key|t|UNIQUE (archive_sha256, target_set_sha256)$current_constraints$
  ELSE
    $legacy_constraints$faa_registry_aircraft|c|faa_registry_aircraft_aircraft_code_check|t|CHECK ((length(TRIM(BOTH FROM aircraft_code)) > 0))
faa_registry_aircraft|c|faa_registry_aircraft_engine_code_check|t|CHECK (((engine_code IS NULL) OR (length(TRIM(BOTH FROM engine_code)) > 0)))
faa_registry_aircraft|c|faa_registry_aircraft_manufacturer_serial_key_check|t|CHECK (((manufacturer_serial_key IS NULL) OR (length(manufacturer_serial_key) > 0)))
faa_registry_aircraft|c|faa_registry_aircraft_manufacturer_serial_raw_check|t|CHECK (((manufacturer_serial_raw IS NULL) OR (length(TRIM(BOTH FROM manufacturer_serial_raw)) > 0)))
faa_registry_aircraft|c|faa_registry_aircraft_n_number_check|t|CHECK ((("left"(n_number, 1) = 'N'::text) AND ((length(n_number) >= 2) AND (length(n_number) <= 6))))
faa_registry_aircraft|c|faa_registry_aircraft_source_record_sha256_check|t|CHECK ((source_record_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_aircraft|c|faa_registry_aircraft_year_manufactured_check|t|CHECK (((year_manufactured IS NULL) OR ((year_manufactured >= 1900) AND (year_manufactured <= 2200))))
faa_registry_aircraft|p|faa_registry_aircraft_pkey|t|PRIMARY KEY (snapshot_id, n_number)
faa_registry_aircraft|u|faa_registry_aircraft_snapshot_id_source_record_sha256_key|t|UNIQUE (snapshot_id, source_record_sha256)
faa_registry_aircraft_references|c|faa_registry_aircraft_references_aircraft_code_check|t|CHECK ((length(TRIM(BOTH FROM aircraft_code)) > 0))
faa_registry_aircraft_references|c|faa_registry_aircraft_references_cruise_speed_mph_check|t|CHECK (((cruise_speed_mph IS NULL) OR (cruise_speed_mph >= 0)))
faa_registry_aircraft_references|c|faa_registry_aircraft_references_engine_count_check|t|CHECK (((engine_count IS NULL) OR (engine_count >= 0)))
faa_registry_aircraft_references|c|faa_registry_aircraft_references_seat_count_check|t|CHECK (((seat_count IS NULL) OR (seat_count >= 0)))
faa_registry_aircraft_references|p|faa_registry_aircraft_references_pkey|t|PRIMARY KEY (snapshot_id, aircraft_code)
faa_registry_coverage|c|faa_registry_coverage_lookup_status_check|t|CHECK ((lookup_status = ANY (ARRAY['matched'::text, 'absent'::text])))
faa_registry_coverage|c|faa_registry_coverage_n_number_check|t|CHECK ((("left"(n_number, 1) = 'N'::text) AND ((length(n_number) >= 2) AND (length(n_number) <= 6))))
faa_registry_coverage|p|faa_registry_coverage_pkey|t|PRIMARY KEY (snapshot_id, n_number)
faa_registry_engine_references|c|faa_registry_engine_references_engine_code_check|t|CHECK ((length(TRIM(BOTH FROM engine_code)) > 0))
faa_registry_engine_references|c|faa_registry_engine_references_horsepower_check|t|CHECK (((horsepower IS NULL) OR (horsepower >= 0)))
faa_registry_engine_references|c|faa_registry_engine_references_thrust_pounds_check|t|CHECK (((thrust_pounds IS NULL) OR (thrust_pounds >= 0)))
faa_registry_engine_references|p|faa_registry_engine_references_pkey|t|PRIMARY KEY (snapshot_id, engine_code)
faa_registry_snapshots|c|faa_registry_snapshots_aircraft_member_name_check|t|CHECK ((aircraft_member_name = 'ACFTREF.txt'::text))
faa_registry_snapshots|c|faa_registry_snapshots_aircraft_member_sha256_check|t|CHECK ((aircraft_member_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_archive_sha256_check|t|CHECK ((archive_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_engine_member_name_check|t|CHECK ((engine_member_name = 'ENGINE.txt'::text))
faa_registry_snapshots|c|faa_registry_snapshots_engine_member_sha256_check|t|CHECK ((engine_member_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_master_member_name_check|t|CHECK ((master_member_name = 'MASTER.txt'::text))
faa_registry_snapshots|c|faa_registry_snapshots_master_member_sha256_check|t|CHECK ((master_member_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_snapshot_date_check|t|CHECK ((snapshot_date ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_source_manifest_sha256_check|t|CHECK ((source_manifest_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|c|faa_registry_snapshots_source_url_check|t|CHECK ((source_url ~ '^https://([^. /]+[.])*faa[.]gov/'::text))
faa_registry_snapshots|c|faa_registry_snapshots_target_set_sha256_check|t|CHECK ((target_set_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_snapshots|p|faa_registry_snapshots_pkey|t|PRIMARY KEY (id)
faa_registry_snapshots|u|faa_registry_snapshots_archive_sha256_target_set_sha256_key|t|UNIQUE (archive_sha256, target_set_sha256)$legacy_constraints$
  END) THEN
    RAISE EXCEPTION 'FAA projection constraints are not the exact prerequisite contract';
  END IF;

  IF (
    SELECT pg_catalog.string_agg(
      pg_catalog.format(
        '%s|%s|%s|%s|%s|%s|%s', relation.relname,
        attribute.attnum, attribute.attname,
        pg_catalog.format_type(attribute.atttypid, attribute.atttypmod),
        attribute.attnotnull, attribute.attidentity,
        COALESCE(
          pg_catalog.pg_get_expr(
            attribute_default.adbin, attribute_default.adrelid
          ), ''
        )
      ), E'\n' ORDER BY relation.relname, attribute.attnum
    )
    FROM pg_catalog.pg_class relation
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    JOIN pg_catalog.pg_attribute attribute
      ON attribute.attrelid = relation.oid
     AND attribute.attnum > 0
     AND NOT attribute.attisdropped
    LEFT JOIN pg_catalog.pg_attrdef attribute_default
      ON attribute_default.adrelid = relation.oid
     AND attribute_default.adnum = attribute.attnum
    WHERE relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft', 'faa_registry_aircraft_references',
        'faa_registry_coverage', 'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
  ) IS DISTINCT FROM (CASE WHEN marker_installed THEN
    $current_columns$faa_registry_aircraft|1|snapshot_id|bigint|t||
faa_registry_aircraft|2|n_number|text|t||
faa_registry_aircraft|3|manufacturer_serial_raw|text|f||
faa_registry_aircraft|4|manufacturer_serial_key|text|f||
faa_registry_aircraft|5|aircraft_code|text|t||
faa_registry_aircraft|6|engine_code|text|f||
faa_registry_aircraft|7|year_manufactured|bigint|f||
faa_registry_aircraft|8|source_record_sha256|text|t||
faa_registry_aircraft_references|1|snapshot_id|bigint|t||
faa_registry_aircraft_references|2|aircraft_code|text|t||
faa_registry_aircraft_references|3|manufacturer_name|text|f||
faa_registry_aircraft_references|4|model_name|text|f||
faa_registry_aircraft_references|5|aircraft_type_code|text|f||
faa_registry_aircraft_references|6|engine_type_code|text|f||
faa_registry_aircraft_references|7|category_code|text|f||
faa_registry_aircraft_references|8|certification_indicator_code|text|f||
faa_registry_aircraft_references|9|engine_count|bigint|f||
faa_registry_aircraft_references|10|seat_count|bigint|f||
faa_registry_aircraft_references|11|weight_class_code|text|f||
faa_registry_aircraft_references|12|cruise_speed_mph|bigint|f||
faa_registry_aircraft_references|13|type_certificate_data_sheet|text|f||
faa_registry_aircraft_references|14|type_certificate_holder|text|f||
faa_registry_coverage|1|snapshot_id|bigint|t||
faa_registry_coverage|2|n_number|text|t||
faa_registry_coverage|3|lookup_status|text|t||
faa_registry_engine_references|1|snapshot_id|bigint|t||
faa_registry_engine_references|2|engine_code|text|t||
faa_registry_engine_references|3|manufacturer_name|text|f||
faa_registry_engine_references|4|model_name|text|f||
faa_registry_engine_references|5|engine_type_code|text|f||
faa_registry_engine_references|6|horsepower|bigint|f||
faa_registry_engine_references|7|thrust_pounds|bigint|f||
faa_registry_snapshots|1|id|bigint|t|d|
faa_registry_snapshots|2|evidence_source_id|bigint|t||
faa_registry_snapshots|3|snapshot_date|text|t||
faa_registry_snapshots|4|source_url|text|t||
faa_registry_snapshots|5|archive_sha256|text|t||
faa_registry_snapshots|6|source_manifest_sha256|text|t||
faa_registry_snapshots|7|target_set_sha256|text|t||
faa_registry_snapshots|8|master_member_name|text|t||
faa_registry_snapshots|9|master_member_sha256|text|t||
faa_registry_snapshots|10|aircraft_member_name|text|t||
faa_registry_snapshots|11|aircraft_member_sha256|text|t||
faa_registry_snapshots|12|engine_member_name|text|t||
faa_registry_snapshots|13|engine_member_sha256|text|t||
faa_registry_snapshots|14|imported_at|text|t||CURRENT_TIMESTAMP
faa_registry_snapshots|15|record_hash_domain|text|t||$current_columns$
  ELSE
    $legacy_columns$faa_registry_aircraft|1|snapshot_id|bigint|t||
faa_registry_aircraft|2|n_number|text|t||
faa_registry_aircraft|3|manufacturer_serial_raw|text|f||
faa_registry_aircraft|4|manufacturer_serial_key|text|f||
faa_registry_aircraft|5|aircraft_code|text|t||
faa_registry_aircraft|6|engine_code|text|f||
faa_registry_aircraft|7|year_manufactured|bigint|f||
faa_registry_aircraft|8|source_record_sha256|text|t||
faa_registry_aircraft_references|1|snapshot_id|bigint|t||
faa_registry_aircraft_references|2|aircraft_code|text|t||
faa_registry_aircraft_references|3|manufacturer_name|text|f||
faa_registry_aircraft_references|4|model_name|text|f||
faa_registry_aircraft_references|5|aircraft_type_code|text|f||
faa_registry_aircraft_references|6|engine_type_code|text|f||
faa_registry_aircraft_references|7|category_code|text|f||
faa_registry_aircraft_references|8|certification_indicator_code|text|f||
faa_registry_aircraft_references|9|engine_count|bigint|f||
faa_registry_aircraft_references|10|seat_count|bigint|f||
faa_registry_aircraft_references|11|weight_class_code|text|f||
faa_registry_aircraft_references|12|cruise_speed_mph|bigint|f||
faa_registry_aircraft_references|13|type_certificate_data_sheet|text|f||
faa_registry_aircraft_references|14|type_certificate_holder|text|f||
faa_registry_coverage|1|snapshot_id|bigint|t||
faa_registry_coverage|2|n_number|text|t||
faa_registry_coverage|3|lookup_status|text|t||
faa_registry_engine_references|1|snapshot_id|bigint|t||
faa_registry_engine_references|2|engine_code|text|t||
faa_registry_engine_references|3|manufacturer_name|text|f||
faa_registry_engine_references|4|model_name|text|f||
faa_registry_engine_references|5|engine_type_code|text|f||
faa_registry_engine_references|6|horsepower|bigint|f||
faa_registry_engine_references|7|thrust_pounds|bigint|f||
faa_registry_snapshots|1|id|bigint|t|d|
faa_registry_snapshots|2|evidence_source_id|bigint|t||
faa_registry_snapshots|3|snapshot_date|text|t||
faa_registry_snapshots|4|source_url|text|t||
faa_registry_snapshots|5|archive_sha256|text|t||
faa_registry_snapshots|6|source_manifest_sha256|text|t||
faa_registry_snapshots|7|target_set_sha256|text|t||
faa_registry_snapshots|8|master_member_name|text|t||
faa_registry_snapshots|9|master_member_sha256|text|t||
faa_registry_snapshots|10|aircraft_member_name|text|t||
faa_registry_snapshots|11|aircraft_member_sha256|text|t||
faa_registry_snapshots|12|engine_member_name|text|t||
faa_registry_snapshots|13|engine_member_sha256|text|t||
faa_registry_snapshots|14|imported_at|text|t||CURRENT_TIMESTAMP$legacy_columns$
  END) THEN
    RAISE EXCEPTION 'FAA projection columns are not the exact prerequisite contract';
  END IF;

  IF (
    SELECT pg_catalog.string_agg(
      relation.relname || '|' || trigger_row.tgname || '|' ||
      function_namespace.nspname || '|' || function_row.proname || '|' ||
      trigger_row.tgtype::TEXT || '|' || trigger_row.tgenabled::TEXT || '|' ||
      trigger_row.tgnargs::TEXT || '|' || (trigger_row.tgqual IS NULL)::TEXT || '|' ||
      (trigger_row.tgattr::TEXT = '')::TEXT,
      E'\n' ORDER BY relation.relname, trigger_row.tgname
    )
    FROM pg_catalog.pg_trigger trigger_row
    JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    JOIN pg_catalog.pg_proc function_row ON function_row.oid = trigger_row.tgfoid
    JOIN pg_catalog.pg_namespace function_namespace
      ON function_namespace.oid = function_row.pronamespace
    WHERE relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft', 'faa_registry_aircraft_references',
        'faa_registry_coverage', 'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
      AND NOT trigger_row.tgisinternal
  ) IS DISTINCT FROM $expected_triggers$faa_registry_aircraft|faa_registry_aircraft_immutable|public|preserve_faa_registry_data|27|O|0|true|true
faa_registry_aircraft_references|faa_registry_aircraft_references_immutable|public|preserve_faa_registry_data|27|O|0|true|true
faa_registry_aircraft_references|faa_registry_aircraft_references_reachable|public|validate_faa_aircraft_reference_reachability|7|O|0|true|true
faa_registry_coverage|faa_registry_coverage_consistent|public|validate_faa_coverage|7|O|0|true|true
faa_registry_coverage|faa_registry_coverage_immutable|public|preserve_faa_registry_data|27|O|0|true|true
faa_registry_engine_references|faa_registry_engine_references_immutable|public|preserve_faa_registry_data|27|O|0|true|true
faa_registry_engine_references|faa_registry_engine_references_reachable|public|validate_faa_engine_reference_reachability|7|O|0|true|true
faa_registry_snapshots|faa_registry_snapshots_immutable|public|preserve_faa_registry_data|27|O|0|true|true
faa_registry_snapshots|faa_registry_snapshots_require_exact_evidence|public|validate_faa_snapshot_evidence|7|O|0|true|true$expected_triggers$ THEN
    RAISE EXCEPTION 'FAA projection triggers are not the exact namespace-locked contract';
  END IF;

  IF (
    SELECT pg_catalog.string_agg(
      pg_catalog.format(
        '%s|%s|%s|%s|%s|%s|%s', relation.relname,
        index_namespace.nspname || '.' || index_relation.relname,
        index_row.indisunique, index_row.indisprimary,
        index_row.indisvalid, index_row.indisready,
        pg_catalog.pg_get_indexdef(index_relation.oid)
      ), E'\n' ORDER BY relation.relname, index_relation.relname
    )
    FROM pg_catalog.pg_index index_row
    JOIN pg_catalog.pg_class relation
      ON relation.oid = index_row.indrelid
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    JOIN pg_catalog.pg_class index_relation
      ON index_relation.oid = index_row.indexrelid
    JOIN pg_catalog.pg_namespace index_namespace
      ON index_namespace.oid = index_relation.relnamespace
    WHERE relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft', 'faa_registry_aircraft_references',
        'faa_registry_coverage', 'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
  ) IS DISTINCT FROM $expected_indexes$faa_registry_aircraft|public.faa_registry_aircraft_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_aircraft_pkey ON public.faa_registry_aircraft USING btree (snapshot_id, n_number)
faa_registry_aircraft|public.faa_registry_aircraft_snapshot_id_source_record_sha256_key|t|f|t|t|CREATE UNIQUE INDEX faa_registry_aircraft_snapshot_id_source_record_sha256_key ON public.faa_registry_aircraft USING btree (snapshot_id, source_record_sha256)
faa_registry_aircraft|public.idx_faa_registry_aircraft_code|f|f|t|t|CREATE INDEX idx_faa_registry_aircraft_code ON public.faa_registry_aircraft USING btree (snapshot_id, aircraft_code)
faa_registry_aircraft|public.idx_faa_registry_aircraft_lineage_record|t|f|t|t|CREATE UNIQUE INDEX idx_faa_registry_aircraft_lineage_record ON public.faa_registry_aircraft USING btree (snapshot_id, n_number, source_record_sha256, manufacturer_serial_key, aircraft_code)
faa_registry_aircraft|public.idx_faa_registry_engine_code|f|f|t|t|CREATE INDEX idx_faa_registry_engine_code ON public.faa_registry_aircraft USING btree (snapshot_id, engine_code)
faa_registry_aircraft_references|public.faa_registry_aircraft_references_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_aircraft_references_pkey ON public.faa_registry_aircraft_references USING btree (snapshot_id, aircraft_code)
faa_registry_coverage|public.faa_registry_coverage_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_coverage_pkey ON public.faa_registry_coverage USING btree (snapshot_id, n_number)
faa_registry_coverage|public.idx_faa_registry_coverage_lookup|f|f|t|t|CREATE INDEX idx_faa_registry_coverage_lookup ON public.faa_registry_coverage USING btree (n_number, snapshot_id)
faa_registry_engine_references|public.faa_registry_engine_references_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_engine_references_pkey ON public.faa_registry_engine_references USING btree (snapshot_id, engine_code)
faa_registry_snapshots|public.faa_registry_snapshots_archive_sha256_target_set_sha256_key|t|f|t|t|CREATE UNIQUE INDEX faa_registry_snapshots_archive_sha256_target_set_sha256_key ON public.faa_registry_snapshots USING btree (archive_sha256, target_set_sha256)
faa_registry_snapshots|public.faa_registry_snapshots_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_snapshots_pkey ON public.faa_registry_snapshots USING btree (id)
faa_registry_snapshots|public.idx_faa_registry_snapshots_current|f|f|t|t|CREATE INDEX idx_faa_registry_snapshots_current ON public.faa_registry_snapshots USING btree (snapshot_date DESC, id DESC)$expected_indexes$ THEN
    RAISE EXCEPTION 'FAA projection indexes are not the exact prerequisite contract';
  END IF;

  IF (
    SELECT count(*)
    FROM pg_catalog.pg_constraint constraint_row
    JOIN pg_catalog.pg_namespace constraint_namespace
      ON constraint_namespace.oid = constraint_row.connamespace
    JOIN pg_catalog.pg_class relation ON relation.oid = constraint_row.conrelid
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    JOIN pg_catalog.pg_class referenced_relation
      ON referenced_relation.oid = constraint_row.confrelid
    JOIN pg_catalog.pg_namespace referenced_namespace
      ON referenced_namespace.oid = referenced_relation.relnamespace
    JOIN pg_catalog.pg_class referenced_index
      ON referenced_index.oid = constraint_row.conindid
    JOIN pg_catalog.pg_namespace referenced_index_namespace
      ON referenced_index_namespace.oid = referenced_index.relnamespace
    WHERE constraint_namespace.nspname = 'public'
      AND relation_namespace.nspname = 'public'
      AND referenced_namespace.nspname = 'public'
      AND referenced_index_namespace.nspname = 'public'
      AND constraint_row.contype = 'f'
      AND constraint_row.convalidated
      AND NOT constraint_row.condeferrable
      AND NOT constraint_row.condeferred
      AND constraint_row.connoinherit
      AND constraint_row.conislocal
      AND constraint_row.coninhcount = 0
      AND constraint_row.contypid = 0
      AND constraint_row.conparentid = 0
      AND constraint_row.confupdtype = 'a'
      AND constraint_row.confdeltype = 'r'
      AND constraint_row.confmatchtype = 's'
      AND (
        constraint_row.conkey = ARRAY[1]::SMALLINT[]
        OR (
          relation.relname = 'faa_registry_snapshots'
          AND constraint_row.conkey = ARRAY[2]::SMALLINT[]
        )
      )
      AND constraint_row.confkey = ARRAY[1]::SMALLINT[]
      AND constraint_row.conpfeqop =
            ARRAY['pg_catalog.=(bigint,bigint)'::pg_catalog.regoperator::OID]
      AND constraint_row.conppeqop =
            ARRAY['pg_catalog.=(bigint,bigint)'::pg_catalog.regoperator::OID]
      AND constraint_row.conffeqop =
            ARRAY['pg_catalog.=(bigint,bigint)'::pg_catalog.regoperator::OID]
      AND constraint_row.confdelsetcols IS NULL
      AND constraint_row.conexclop IS NULL
      AND constraint_row.conbin IS NULL
      AND (
        (relation.relname = 'faa_registry_aircraft'
          AND constraint_row.conname = 'faa_registry_aircraft_snapshot_id_fkey'
          AND referenced_relation.relname = 'faa_registry_snapshots'
          AND referenced_index.relname = 'faa_registry_snapshots_pkey')
        OR (relation.relname = 'faa_registry_aircraft_references'
          AND constraint_row.conname = 'faa_registry_aircraft_references_snapshot_id_fkey'
          AND referenced_relation.relname = 'faa_registry_snapshots'
          AND referenced_index.relname = 'faa_registry_snapshots_pkey')
        OR (relation.relname = 'faa_registry_coverage'
          AND constraint_row.conname = 'faa_registry_coverage_snapshot_id_fkey'
          AND referenced_relation.relname = 'faa_registry_snapshots'
          AND referenced_index.relname = 'faa_registry_snapshots_pkey')
        OR (relation.relname = 'faa_registry_engine_references'
          AND constraint_row.conname = 'faa_registry_engine_references_snapshot_id_fkey'
          AND referenced_relation.relname = 'faa_registry_snapshots'
          AND referenced_index.relname = 'faa_registry_snapshots_pkey')
        OR (relation.relname = 'faa_registry_snapshots'
          AND constraint_row.conname = 'faa_registry_snapshots_evidence_source_id_fkey'
          AND constraint_row.conkey = ARRAY[2]::SMALLINT[]
          AND referenced_relation.relname = 'curation_evidence_sources'
          AND referenced_index.relname = 'curation_evidence_sources_pkey')
      )
  ) <> 5 THEN
    RAISE EXCEPTION 'FAA projection foreign keys are not the exact namespace-locked contract';
  END IF;

  IF marker_installed THEN
    IF NOT EXISTS (
      SELECT 1
      FROM pg_catalog.pg_attribute attribute
      WHERE attribute.attrelid =
              pg_catalog.to_regclass('public.faa_registry_snapshots')
        AND attribute.attname = 'record_hash_domain'
        AND attribute.attnum = 15
        AND attribute.atttypid = 'pg_catalog.text'::pg_catalog.regtype
        AND attribute.atttypmod = -1
        AND attribute.attnotnull
        AND attribute.attidentity = ''
        AND attribute.attgenerated = ''
        AND NOT attribute.attisdropped
        AND NOT EXISTS (
          SELECT 1
          FROM pg_catalog.pg_attrdef attribute_default
          WHERE attribute_default.adrelid = attribute.attrelid
            AND attribute_default.adnum = attribute.attnum
        )
    ) OR NOT EXISTS (
      SELECT 1
      FROM pg_catalog.pg_constraint constraint_row
      WHERE constraint_row.conrelid =
              pg_catalog.to_regclass('public.faa_registry_snapshots')
        AND constraint_row.conname =
              'faa_registry_snapshots_record_hash_domain_check'
        AND constraint_row.contype = 'c'
        AND constraint_row.convalidated
        AND NOT constraint_row.condeferrable
        AND NOT constraint_row.condeferred
        AND constraint_row.conkey = ARRAY[15]::SMALLINT[]
        AND pg_catalog.pg_get_constraintdef(constraint_row.oid, FALSE) =
              $definition$CHECK ((record_hash_domain = 'aircost-faa-master-retained-aircraft-projection-v1'::text))$definition$
    ) OR EXISTS (
      SELECT 1
      FROM public.faa_registry_snapshots snapshot
      WHERE snapshot.record_hash_domain IS DISTINCT FROM
              'aircost-faa-master-retained-aircraft-projection-v1'
    ) THEN
      RAISE EXCEPTION 'installed FAA record hash domain has an unexpected shape or value';
    END IF;
  ELSE
    IF EXISTS (
      SELECT 1
      FROM pg_catalog.pg_attribute attribute
      WHERE attribute.attrelid =
              pg_catalog.to_regclass('public.faa_registry_snapshots')
        AND attribute.attname = 'record_hash_domain'
        AND NOT attribute.attisdropped
    ) THEN
      RAISE EXCEPTION 'unmarked FAA record hash domain column is not accepted';
    END IF;
    IF EXISTS (SELECT 1 FROM public.faa_registry_snapshots) THEN
      RAISE EXCEPTION
        'nonempty legacy FAA projections must be discarded and regenerated from the exact release archive';
    END IF;
  END IF;
END
$pre_domain_guard$;

DO $install_domain$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260820_faa_record_hash_domain'
  ) THEN
    ALTER TABLE public.faa_registry_snapshots
      ADD COLUMN record_hash_domain TEXT NOT NULL
      CONSTRAINT faa_registry_snapshots_record_hash_domain_check
      CHECK (
        record_hash_domain =
          'aircost-faa-master-retained-aircraft-projection-v1'
      );
  END IF;
END
$install_domain$;

DO $post_domain_guard$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM pg_catalog.pg_attribute attribute
    WHERE attribute.attrelid =
            pg_catalog.to_regclass('public.faa_registry_snapshots')
      AND attribute.attname = 'record_hash_domain'
      AND attribute.attnum = 15
      AND attribute.atttypid = 'pg_catalog.text'::pg_catalog.regtype
      AND attribute.atttypmod = -1
      AND attribute.attnotnull
      AND attribute.attidentity = ''
      AND attribute.attgenerated = ''
      AND NOT attribute.attisdropped
      AND NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_attrdef attribute_default
        WHERE attribute_default.adrelid = attribute.attrelid
          AND attribute_default.adnum = attribute.attnum
      )
  ) OR NOT EXISTS (
    SELECT 1
    FROM pg_catalog.pg_constraint constraint_row
    WHERE constraint_row.conrelid =
            pg_catalog.to_regclass('public.faa_registry_snapshots')
      AND constraint_row.conname =
            'faa_registry_snapshots_record_hash_domain_check'
      AND constraint_row.contype = 'c'
      AND constraint_row.convalidated
      AND NOT constraint_row.condeferrable
      AND NOT constraint_row.condeferred
      AND constraint_row.conkey = ARRAY[15]::SMALLINT[]
      AND pg_catalog.pg_get_constraintdef(constraint_row.oid, FALSE) =
            $definition$CHECK ((record_hash_domain = 'aircost-faa-master-retained-aircraft-projection-v1'::text))$definition$
  ) THEN
    RAISE EXCEPTION 'FAA record hash domain installation failed exact attestation';
  END IF;
END
$post_domain_guard$;

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260820_faa_record_hash_domain',
  1,
  'f124f573bf705da6c1e4b0a5c7a8df45ea5a4a5dc009a28eee012be42c691502',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
