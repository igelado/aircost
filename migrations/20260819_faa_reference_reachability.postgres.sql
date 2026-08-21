BEGIN;

SET LOCAL search_path = public, pg_catalog, pg_temp;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

LOCK TABLE public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

-- The historical and installed contracts share the same five projection
-- relations. Refuse any relation, column, constraint, or index drift before
-- replacing even one trigger or function. Exact string comparison is used
-- instead of an extension-provided digest so the guard has no extension or
-- caller-search-path dependency.
DO $projection_shape_guard$
DECLARE
  relation_signature TEXT;
  column_signature TEXT;
  constraint_signature TEXT;
  foreign_key_signature TEXT;
  index_signature TEXT;
BEGIN
  SELECT
    (
      SELECT pg_catalog.string_agg(
        pg_catalog.format(
          '%s|%s|%s|%s|%s|%s|%s', relation.relname,
          relation.relkind, relation.relpersistence,
          relation.relrowsecurity, relation.relforcerowsecurity,
          relation.relispartition, relation.relhasrules
        ), E'\n' ORDER BY relation.relname
      )
      FROM pg_catalog.pg_class relation
      JOIN pg_catalog.pg_namespace relation_namespace
        ON relation_namespace.oid = relation.relnamespace
      WHERE relation_namespace.nspname = 'public'
        AND relation.relname IN (
          'faa_registry_aircraft',
          'faa_registry_aircraft_references',
          'faa_registry_coverage',
          'faa_registry_engine_references',
          'faa_registry_snapshots'
        )
    ),
    (
      SELECT pg_catalog.string_agg(
        pg_catalog.format(
          '%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s',
          relation.relname,
          constraint_namespace.nspname,
          constraint_row.contype,
          constraint_row.conname,
          constraint_row.convalidated,
          constraint_row.condeferrable,
          constraint_row.condeferred,
          constraint_row.connoinherit,
          constraint_row.conislocal,
          constraint_row.coninhcount,
          referenced_namespace.nspname,
          referenced_relation.relname,
          referenced_index_namespace.nspname,
          referenced_index.relname,
          constraint_row.contypid = 0,
          constraint_row.conparentid = 0,
          constraint_row.confupdtype::text,
          constraint_row.confdeltype::text,
          constraint_row.confmatchtype::text,
          COALESCE(constraint_row.conkey::text, ''),
          COALESCE(constraint_row.confkey::text, ''),
          COALESCE((
            SELECT pg_catalog.string_agg(
              operator_namespace.nspname || '.' || operator_row.oprname || '(' ||
              left_namespace.nspname || '.' || left_type.typname || ',' ||
              right_namespace.nspname || '.' || right_type.typname || ')',
              ',' ORDER BY operator_key.ordinality
            )
            FROM pg_catalog.unnest(constraint_row.conpfeqop) WITH ORDINALITY
              AS operator_key(operator_oid, ordinality)
            JOIN pg_catalog.pg_operator operator_row
              ON operator_row.oid = operator_key.operator_oid
            JOIN pg_catalog.pg_namespace operator_namespace
              ON operator_namespace.oid = operator_row.oprnamespace
            JOIN pg_catalog.pg_type left_type
              ON left_type.oid = operator_row.oprleft
            JOIN pg_catalog.pg_namespace left_namespace
              ON left_namespace.oid = left_type.typnamespace
            JOIN pg_catalog.pg_type right_type
              ON right_type.oid = operator_row.oprright
            JOIN pg_catalog.pg_namespace right_namespace
              ON right_namespace.oid = right_type.typnamespace
          ), ''),
          COALESCE((
            SELECT pg_catalog.string_agg(
              operator_namespace.nspname || '.' || operator_row.oprname || '(' ||
              left_namespace.nspname || '.' || left_type.typname || ',' ||
              right_namespace.nspname || '.' || right_type.typname || ')',
              ',' ORDER BY operator_key.ordinality
            )
            FROM pg_catalog.unnest(constraint_row.conppeqop) WITH ORDINALITY
              AS operator_key(operator_oid, ordinality)
            JOIN pg_catalog.pg_operator operator_row
              ON operator_row.oid = operator_key.operator_oid
            JOIN pg_catalog.pg_namespace operator_namespace
              ON operator_namespace.oid = operator_row.oprnamespace
            JOIN pg_catalog.pg_type left_type
              ON left_type.oid = operator_row.oprleft
            JOIN pg_catalog.pg_namespace left_namespace
              ON left_namespace.oid = left_type.typnamespace
            JOIN pg_catalog.pg_type right_type
              ON right_type.oid = operator_row.oprright
            JOIN pg_catalog.pg_namespace right_namespace
              ON right_namespace.oid = right_type.typnamespace
          ), ''),
          COALESCE((
            SELECT pg_catalog.string_agg(
              operator_namespace.nspname || '.' || operator_row.oprname || '(' ||
              left_namespace.nspname || '.' || left_type.typname || ',' ||
              right_namespace.nspname || '.' || right_type.typname || ')',
              ',' ORDER BY operator_key.ordinality
            )
            FROM pg_catalog.unnest(constraint_row.conffeqop) WITH ORDINALITY
              AS operator_key(operator_oid, ordinality)
            JOIN pg_catalog.pg_operator operator_row
              ON operator_row.oid = operator_key.operator_oid
            JOIN pg_catalog.pg_namespace operator_namespace
              ON operator_namespace.oid = operator_row.oprnamespace
            JOIN pg_catalog.pg_type left_type
              ON left_type.oid = operator_row.oprleft
            JOIN pg_catalog.pg_namespace left_namespace
              ON left_namespace.oid = left_type.typnamespace
            JOIN pg_catalog.pg_type right_type
              ON right_type.oid = operator_row.oprright
            JOIN pg_catalog.pg_namespace right_namespace
              ON right_namespace.oid = right_type.typnamespace
          ), ''),
          COALESCE(constraint_row.confdelsetcols::text, ''),
          constraint_row.conexclop IS NULL,
          constraint_row.conbin IS NULL,
          ''
        ), E'\n' ORDER BY relation.relname, constraint_row.conname
      )
      FROM pg_catalog.pg_constraint constraint_row
      JOIN pg_catalog.pg_namespace constraint_namespace
        ON constraint_namespace.oid = constraint_row.connamespace
      JOIN pg_catalog.pg_class relation
        ON relation.oid = constraint_row.conrelid
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
      WHERE relation_namespace.nspname = 'public'
        AND relation.relname IN (
          'faa_registry_aircraft',
          'faa_registry_aircraft_references',
          'faa_registry_coverage',
          'faa_registry_engine_references',
          'faa_registry_snapshots'
        )
        AND constraint_row.contype = 'f'
    ),
    (
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
          'faa_registry_aircraft',
          'faa_registry_aircraft_references',
          'faa_registry_coverage',
          'faa_registry_engine_references',
          'faa_registry_snapshots'
        )
    ),
    (
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
          'faa_registry_aircraft',
          'faa_registry_aircraft_references',
          'faa_registry_coverage',
          'faa_registry_engine_references',
          'faa_registry_snapshots'
        )
    ),
    (
      SELECT pg_catalog.string_agg(
        pg_catalog.format(
          '%s|%s|%s|%s|%s|%s|%s', relation.relname,
          index_relation.relname, index_row.indisunique,
          index_row.indisprimary, index_row.indisvalid,
          index_row.indisready,
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
      WHERE relation_namespace.nspname = 'public'
        AND relation.relname IN (
          'faa_registry_aircraft',
          'faa_registry_aircraft_references',
          'faa_registry_coverage',
          'faa_registry_engine_references',
          'faa_registry_snapshots'
        )
    )
  INTO relation_signature, foreign_key_signature, column_signature,
       constraint_signature, index_signature;

  IF relation_signature IS DISTINCT FROM $expected_relations$faa_registry_aircraft|r|p|f|f|f|f
faa_registry_aircraft_references|r|p|f|f|f|f
faa_registry_coverage|r|p|f|f|f|f
faa_registry_engine_references|r|p|f|f|f|f
faa_registry_snapshots|r|p|f|f|f|f$expected_relations$
  THEN
    RAISE EXCEPTION 'FAA registry projection relations have an unexpected shape';
  END IF;

  IF column_signature IS DISTINCT FROM $expected_columns$faa_registry_aircraft|1|snapshot_id|bigint|t||
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
faa_registry_snapshots|14|imported_at|text|t||CURRENT_TIMESTAMP$expected_columns$
  THEN
    RAISE EXCEPTION 'FAA registry projection columns have an unexpected shape';
  END IF;

  IF constraint_signature IS DISTINCT FROM $expected_constraints$faa_registry_aircraft|c|faa_registry_aircraft_aircraft_code_check|t|CHECK ((length(TRIM(BOTH FROM aircraft_code)) > 0))
faa_registry_aircraft|c|faa_registry_aircraft_engine_code_check|t|CHECK (((engine_code IS NULL) OR (length(TRIM(BOTH FROM engine_code)) > 0)))
faa_registry_aircraft|c|faa_registry_aircraft_manufacturer_serial_key_check|t|CHECK (((manufacturer_serial_key IS NULL) OR (length(manufacturer_serial_key) > 0)))
faa_registry_aircraft|c|faa_registry_aircraft_manufacturer_serial_raw_check|t|CHECK (((manufacturer_serial_raw IS NULL) OR (length(TRIM(BOTH FROM manufacturer_serial_raw)) > 0)))
faa_registry_aircraft|c|faa_registry_aircraft_n_number_check|t|CHECK ((("left"(n_number, 1) = 'N'::text) AND ((length(n_number) >= 2) AND (length(n_number) <= 6))))
faa_registry_aircraft|c|faa_registry_aircraft_source_record_sha256_check|t|CHECK ((source_record_sha256 ~ '^[0-9a-f]{64}$'::text))
faa_registry_aircraft|c|faa_registry_aircraft_year_manufactured_check|t|CHECK (((year_manufactured IS NULL) OR ((year_manufactured >= 1900) AND (year_manufactured <= 2200))))
faa_registry_aircraft|f|faa_registry_aircraft_snapshot_id_fkey|t|FOREIGN KEY (snapshot_id) REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT
faa_registry_aircraft|p|faa_registry_aircraft_pkey|t|PRIMARY KEY (snapshot_id, n_number)
faa_registry_aircraft|u|faa_registry_aircraft_snapshot_id_source_record_sha256_key|t|UNIQUE (snapshot_id, source_record_sha256)
faa_registry_aircraft_references|c|faa_registry_aircraft_references_aircraft_code_check|t|CHECK ((length(TRIM(BOTH FROM aircraft_code)) > 0))
faa_registry_aircraft_references|c|faa_registry_aircraft_references_cruise_speed_mph_check|t|CHECK (((cruise_speed_mph IS NULL) OR (cruise_speed_mph >= 0)))
faa_registry_aircraft_references|c|faa_registry_aircraft_references_engine_count_check|t|CHECK (((engine_count IS NULL) OR (engine_count >= 0)))
faa_registry_aircraft_references|c|faa_registry_aircraft_references_seat_count_check|t|CHECK (((seat_count IS NULL) OR (seat_count >= 0)))
faa_registry_aircraft_references|f|faa_registry_aircraft_references_snapshot_id_fkey|t|FOREIGN KEY (snapshot_id) REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT
faa_registry_aircraft_references|p|faa_registry_aircraft_references_pkey|t|PRIMARY KEY (snapshot_id, aircraft_code)
faa_registry_coverage|c|faa_registry_coverage_lookup_status_check|t|CHECK ((lookup_status = ANY (ARRAY['matched'::text, 'absent'::text])))
faa_registry_coverage|c|faa_registry_coverage_n_number_check|t|CHECK ((("left"(n_number, 1) = 'N'::text) AND ((length(n_number) >= 2) AND (length(n_number) <= 6))))
faa_registry_coverage|f|faa_registry_coverage_snapshot_id_fkey|t|FOREIGN KEY (snapshot_id) REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT
faa_registry_coverage|p|faa_registry_coverage_pkey|t|PRIMARY KEY (snapshot_id, n_number)
faa_registry_engine_references|c|faa_registry_engine_references_engine_code_check|t|CHECK ((length(TRIM(BOTH FROM engine_code)) > 0))
faa_registry_engine_references|c|faa_registry_engine_references_horsepower_check|t|CHECK (((horsepower IS NULL) OR (horsepower >= 0)))
faa_registry_engine_references|c|faa_registry_engine_references_thrust_pounds_check|t|CHECK (((thrust_pounds IS NULL) OR (thrust_pounds >= 0)))
faa_registry_engine_references|f|faa_registry_engine_references_snapshot_id_fkey|t|FOREIGN KEY (snapshot_id) REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT
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
faa_registry_snapshots|f|faa_registry_snapshots_evidence_source_id_fkey|t|FOREIGN KEY (evidence_source_id) REFERENCES curation_evidence_sources(id) ON DELETE RESTRICT
faa_registry_snapshots|p|faa_registry_snapshots_pkey|t|PRIMARY KEY (id)
faa_registry_snapshots|u|faa_registry_snapshots_archive_sha256_target_set_sha256_key|t|UNIQUE (archive_sha256, target_set_sha256)$expected_constraints$
  THEN
    RAISE EXCEPTION 'FAA registry projection constraints have an unexpected shape';
  END IF;

  IF foreign_key_signature IS DISTINCT FROM $expected_foreign_keys$faa_registry_aircraft|public|f|faa_registry_aircraft_snapshot_id_fkey|t|f|f|t|t|0|public|faa_registry_snapshots|public|faa_registry_snapshots_pkey|t|t|a|r|s|{1}|{1}|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)||t|t|
faa_registry_aircraft_references|public|f|faa_registry_aircraft_references_snapshot_id_fkey|t|f|f|t|t|0|public|faa_registry_snapshots|public|faa_registry_snapshots_pkey|t|t|a|r|s|{1}|{1}|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)||t|t|
faa_registry_coverage|public|f|faa_registry_coverage_snapshot_id_fkey|t|f|f|t|t|0|public|faa_registry_snapshots|public|faa_registry_snapshots_pkey|t|t|a|r|s|{1}|{1}|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)||t|t|
faa_registry_engine_references|public|f|faa_registry_engine_references_snapshot_id_fkey|t|f|f|t|t|0|public|faa_registry_snapshots|public|faa_registry_snapshots_pkey|t|t|a|r|s|{1}|{1}|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)||t|t|
faa_registry_snapshots|public|f|faa_registry_snapshots_evidence_source_id_fkey|t|f|f|t|t|0|public|curation_evidence_sources|public|curation_evidence_sources_pkey|t|t|a|r|s|{2}|{1}|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)|pg_catalog.=(pg_catalog.int8,pg_catalog.int8)||t|t|$expected_foreign_keys$
  THEN
    RAISE EXCEPTION 'FAA registry projection foreign keys have an unexpected shape';
  END IF;

  IF index_signature IS DISTINCT FROM $expected_indexes$faa_registry_aircraft|faa_registry_aircraft_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_aircraft_pkey ON public.faa_registry_aircraft USING btree (snapshot_id, n_number)
faa_registry_aircraft|faa_registry_aircraft_snapshot_id_source_record_sha256_key|t|f|t|t|CREATE UNIQUE INDEX faa_registry_aircraft_snapshot_id_source_record_sha256_key ON public.faa_registry_aircraft USING btree (snapshot_id, source_record_sha256)
faa_registry_aircraft|idx_faa_registry_aircraft_code|f|f|t|t|CREATE INDEX idx_faa_registry_aircraft_code ON public.faa_registry_aircraft USING btree (snapshot_id, aircraft_code)
faa_registry_aircraft|idx_faa_registry_aircraft_lineage_record|t|f|t|t|CREATE UNIQUE INDEX idx_faa_registry_aircraft_lineage_record ON public.faa_registry_aircraft USING btree (snapshot_id, n_number, source_record_sha256, manufacturer_serial_key, aircraft_code)
faa_registry_aircraft|idx_faa_registry_engine_code|f|f|t|t|CREATE INDEX idx_faa_registry_engine_code ON public.faa_registry_aircraft USING btree (snapshot_id, engine_code)
faa_registry_aircraft_references|faa_registry_aircraft_references_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_aircraft_references_pkey ON public.faa_registry_aircraft_references USING btree (snapshot_id, aircraft_code)
faa_registry_coverage|faa_registry_coverage_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_coverage_pkey ON public.faa_registry_coverage USING btree (snapshot_id, n_number)
faa_registry_coverage|idx_faa_registry_coverage_lookup|f|f|t|t|CREATE INDEX idx_faa_registry_coverage_lookup ON public.faa_registry_coverage USING btree (n_number, snapshot_id)
faa_registry_engine_references|faa_registry_engine_references_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_engine_references_pkey ON public.faa_registry_engine_references USING btree (snapshot_id, engine_code)
faa_registry_snapshots|faa_registry_snapshots_archive_sha256_target_set_sha256_key|t|f|t|t|CREATE UNIQUE INDEX faa_registry_snapshots_archive_sha256_target_set_sha256_key ON public.faa_registry_snapshots USING btree (archive_sha256, target_set_sha256)
faa_registry_snapshots|faa_registry_snapshots_pkey|t|t|t|t|CREATE UNIQUE INDEX faa_registry_snapshots_pkey ON public.faa_registry_snapshots USING btree (id)
faa_registry_snapshots|idx_faa_registry_snapshots_current|f|f|t|t|CREATE INDEX idx_faa_registry_snapshots_current ON public.faa_registry_snapshots USING btree (snapshot_date DESC, id DESC)$expected_indexes$
  THEN
    RAISE EXCEPTION 'FAA registry projection indexes have an unexpected shape';
  END IF;
END
$projection_shape_guard$;

-- Validate one and only one supported state before any FAA object is replaced:
-- the exact historical contract when the marker is absent, or the exact
-- installed contract when it is present. This makes reruns idempotent without
-- turning the migration into a repair path for tampered current objects.
DO $exact_state_guard$
DECLARE
  marker_installed BOOLEAN;
  trigger_matches BIGINT;
  trigger_name_count BIGINT;
  function_matches BIGINT;
BEGIN
  SELECT EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ) INTO marker_installed;

  IF pg_catalog.to_regclass('public.faa_registry_snapshots') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_aircraft') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_aircraft_references') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_engine_references') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_coverage') IS NULL
  THEN
    RAISE EXCEPTION 'FAA registry projection relations are incomplete';
  END IF;

  WITH expected(
    trigger_name, relation_name, function_name, trigger_type
  ) AS (
    VALUES
      ('faa_registry_snapshots_require_exact_evidence',
       'faa_registry_snapshots', 'validate_faa_snapshot_evidence', 7),
      ('faa_registry_aircraft_references_reachable',
       'faa_registry_aircraft_references',
       CASE WHEN marker_installed
         THEN 'validate_faa_aircraft_reference_reachability'
         ELSE 'validate_faa_reference_reachability'
       END, 7),
      ('faa_registry_engine_references_reachable',
       'faa_registry_engine_references',
       CASE WHEN marker_installed
         THEN 'validate_faa_engine_reference_reachability'
         ELSE 'validate_faa_reference_reachability'
       END, 7),
      ('faa_registry_coverage_consistent',
       'faa_registry_coverage', 'validate_faa_coverage', 7),
      ('faa_registry_snapshots_immutable',
       'faa_registry_snapshots', 'preserve_faa_registry_data', 27),
      ('faa_registry_aircraft_immutable',
       'faa_registry_aircraft', 'preserve_faa_registry_data', 27),
      ('faa_registry_aircraft_references_immutable',
       'faa_registry_aircraft_references', 'preserve_faa_registry_data', 27),
      ('faa_registry_engine_references_immutable',
       'faa_registry_engine_references', 'preserve_faa_registry_data', 27),
      ('faa_registry_coverage_immutable',
       'faa_registry_coverage', 'preserve_faa_registry_data', 27)
  )
  SELECT COUNT(*) INTO trigger_matches
  FROM expected
  JOIN pg_catalog.pg_trigger trigger_row
    ON trigger_row.tgname = expected.trigger_name
   AND NOT trigger_row.tgisinternal
  JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
  JOIN pg_catalog.pg_namespace relation_namespace
    ON relation_namespace.oid = relation.relnamespace
  JOIN pg_catalog.pg_proc routine ON routine.oid = trigger_row.tgfoid
  JOIN pg_catalog.pg_namespace routine_namespace
    ON routine_namespace.oid = routine.pronamespace
  WHERE relation_namespace.nspname = 'public'
    AND relation.relname = expected.relation_name
    AND routine_namespace.nspname = 'public'
    AND routine.proname = expected.function_name
    AND routine.pronargs = 0
    AND trigger_row.tgtype = expected.trigger_type
    AND trigger_row.tgenabled = 'O'
    AND trigger_row.tgqual IS NULL
    AND pg_catalog.cardinality(trigger_row.tgattr) = 0
    AND trigger_row.tgnargs = 0;

  SELECT COUNT(*) INTO trigger_name_count
  FROM pg_catalog.pg_trigger trigger_row
  JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
  JOIN pg_catalog.pg_namespace relation_namespace
    ON relation_namespace.oid = relation.relnamespace
  WHERE NOT trigger_row.tgisinternal
    AND relation_namespace.nspname = 'public'
    AND relation.relname IN (
      'faa_registry_aircraft',
      'faa_registry_aircraft_references',
      'faa_registry_coverage',
      'faa_registry_engine_references',
      'faa_registry_snapshots'
    );

  IF marker_installed THEN
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
$engine_function$),
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
$immutability_function$)
    )
    SELECT COUNT(*) INTO function_matches
    FROM expected
    JOIN pg_catalog.pg_proc routine ON routine.proname = expected.function_name
    JOIN pg_catalog.pg_namespace routine_namespace
      ON routine_namespace.oid = routine.pronamespace
    JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
    WHERE routine_namespace.nspname = 'public'
      AND routine.prosrc = expected.function_source
      AND routine.proconfig = ARRAY['search_path=pg_catalog']
      AND language.lanname = 'plpgsql'
      AND routine.prorettype = 'trigger'::pg_catalog.regtype
      AND routine.pronargs = 0
      AND routine.prokind = 'f'
      AND NOT routine.prosecdef
      AND NOT routine.proisstrict
      AND routine.provolatile = 'v'
      AND routine.proparallel = 'u';

    IF trigger_matches <> 9 OR trigger_name_count <> 9
      OR function_matches <> 5
      OR pg_catalog.to_regprocedure(
        'public.validate_faa_reference_reachability()'
      ) IS NOT NULL
    THEN
      RAISE EXCEPTION
        'installed FAA reference reachability objects have an unexpected shape';
    END IF;
  ELSE
    WITH expected(function_name, function_source) AS (
      VALUES
        ('validate_faa_snapshot_evidence', $old_snapshot_function$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM curation_evidence_sources source
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
$old_snapshot_function$),
        ('validate_faa_reference_reachability', $old_reference_function$
BEGIN
  IF TG_TABLE_NAME = 'faa_registry_aircraft_references' AND NOT EXISTS (
    SELECT 1 FROM faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.aircraft_code = NEW.aircraft_code
  ) THEN
    RAISE EXCEPTION 'FAA aircraft reference must be reachable from a target match';
  END IF;
  IF TG_TABLE_NAME = 'faa_registry_engine_references' AND NOT EXISTS (
    SELECT 1 FROM faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.engine_code = NEW.engine_code
  ) THEN
    RAISE EXCEPTION 'FAA engine reference must be reachable from a target match';
  END IF;
  RETURN NEW;
END;
$old_reference_function$),
        ('validate_faa_coverage', $old_coverage_function$
BEGIN
  IF (NEW.lookup_status = 'matched' AND NOT EXISTS (
        SELECT 1 FROM faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id AND aircraft.n_number = NEW.n_number
      )) OR (NEW.lookup_status = 'absent' AND EXISTS (
        SELECT 1 FROM faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id AND aircraft.n_number = NEW.n_number
      )) THEN
    RAISE EXCEPTION 'FAA coverage must agree with its target match';
  END IF;
  RETURN NEW;
END;
$old_coverage_function$),
        ('preserve_faa_registry_data', $old_immutability_function$
BEGIN
  RAISE EXCEPTION 'FAA registry snapshots and projections are immutable';
END;
$old_immutability_function$)
    )
    SELECT COUNT(*) INTO function_matches
    FROM expected
    JOIN pg_catalog.pg_proc routine ON routine.proname = expected.function_name
    JOIN pg_catalog.pg_namespace routine_namespace
      ON routine_namespace.oid = routine.pronamespace
    JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
    WHERE routine_namespace.nspname = 'public'
      AND routine.prosrc = expected.function_source
      AND routine.proconfig IS NULL
      AND language.lanname = 'plpgsql'
      AND routine.prorettype = 'trigger'::pg_catalog.regtype
      AND routine.pronargs = 0
      AND routine.prokind = 'f'
      AND NOT routine.prosecdef
      AND NOT routine.proisstrict
      AND routine.provolatile = 'v'
      AND routine.proparallel = 'u';

    IF trigger_matches <> 9 OR trigger_name_count <> 9
      OR function_matches <> 4
      OR pg_catalog.to_regprocedure(
        'public.validate_faa_aircraft_reference_reachability()'
      ) IS NOT NULL
      OR pg_catalog.to_regprocedure(
        'public.validate_faa_engine_reference_reachability()'
      ) IS NOT NULL
    THEN
      RAISE EXCEPTION
        'pre-migration FAA reference reachability objects have an unexpected shape';
    END IF;
  END IF;
END
$exact_state_guard$;

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          'fc6451ffe8e1ee2034e76480767d16d6c37463461d9e684687448b4d43f96bef'
      )
  ) THEN
    RAISE EXCEPTION
      'installed FAA reference reachability migration has a different contract';
  END IF;

  IF NOT EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ) THEN
    IF (
    pg_catalog.to_regclass('public.faa_registry_aircraft') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_aircraft_references') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_engine_references') IS NULL
    OR pg_catalog.to_regprocedure(
         'public.validate_faa_reference_reachability()'
       ) IS NULL
    OR pg_catalog.to_regprocedure(
         'public.validate_faa_aircraft_reference_reachability()'
       ) IS NOT NULL
    OR pg_catalog.to_regprocedure(
         'public.validate_faa_engine_reference_reachability()'
       ) IS NOT NULL
    OR NOT EXISTS (
      SELECT 1
      FROM pg_catalog.pg_proc routine
      JOIN pg_catalog.pg_namespace routine_namespace
        ON routine_namespace.oid = routine.pronamespace
      JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
      WHERE routine.oid = pg_catalog.to_regprocedure(
        'public.validate_faa_reference_reachability()'
      )
        AND routine_namespace.nspname = 'public'
        AND routine.prosrc = $old_function$
BEGIN
  IF TG_TABLE_NAME = 'faa_registry_aircraft_references' AND NOT EXISTS (
    SELECT 1 FROM faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.aircraft_code = NEW.aircraft_code
  ) THEN
    RAISE EXCEPTION 'FAA aircraft reference must be reachable from a target match';
  END IF;
  IF TG_TABLE_NAME = 'faa_registry_engine_references' AND NOT EXISTS (
    SELECT 1 FROM faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.engine_code = NEW.engine_code
  ) THEN
    RAISE EXCEPTION 'FAA engine reference must be reachable from a target match';
  END IF;
  RETURN NEW;
END;
$old_function$
        AND routine.proconfig IS NULL
        AND language.lanname = 'plpgsql'
        AND routine.prorettype = 'trigger'::pg_catalog.regtype
        AND routine.pronargs = 0
        AND routine.prokind = 'f'
        AND NOT routine.prosecdef
        AND NOT routine.proisstrict
        AND routine.provolatile = 'v'
        AND routine.proparallel = 'u'
    )
    OR (
      SELECT COUNT(*)
      FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname IN (
          'faa_registry_aircraft_references_reachable',
          'faa_registry_engine_references_reachable'
        )
        AND trigger_row.tgrelid IN (
          pg_catalog.to_regclass('public.faa_registry_aircraft_references'),
          pg_catalog.to_regclass('public.faa_registry_engine_references')
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_reference_reachability()'
        )
        AND trigger_row.tgtype = 7
        AND trigger_row.tgenabled = 'O'
        AND trigger_row.tgqual IS NULL
        AND pg_catalog.cardinality(trigger_row.tgattr) = 0
        AND trigger_row.tgnargs = 0
    ) <> 2
    OR NOT EXISTS (
      SELECT 1 FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname =
          'faa_registry_aircraft_references_reachable'
        AND trigger_row.tgrelid = pg_catalog.to_regclass(
          'public.faa_registry_aircraft_references'
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_reference_reachability()'
        )
    )
    OR NOT EXISTS (
      SELECT 1 FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname =
          'faa_registry_engine_references_reachable'
        AND trigger_row.tgrelid = pg_catalog.to_regclass(
          'public.faa_registry_engine_references'
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_reference_reachability()'
        )
    )
    ) THEN
      RAISE EXCEPTION
        'pre-migration FAA reference reachability objects have an unexpected shape';
    END IF;
  ELSE
    IF (
      SELECT COUNT(*)
      FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname IN (
          'faa_registry_aircraft_references_reachable',
          'faa_registry_engine_references_reachable'
        )
        AND trigger_row.tgrelid IN (
          pg_catalog.to_regclass('public.faa_registry_aircraft_references'),
          pg_catalog.to_regclass('public.faa_registry_engine_references')
        )
        AND trigger_row.tgtype = 7
        AND trigger_row.tgenabled = 'O'
        AND trigger_row.tgqual IS NULL
        AND pg_catalog.cardinality(trigger_row.tgattr) = 0
        AND trigger_row.tgnargs = 0
    ) <> 2
    OR NOT EXISTS (
      SELECT 1 FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname =
          'faa_registry_aircraft_references_reachable'
        AND trigger_row.tgrelid = pg_catalog.to_regclass(
          'public.faa_registry_aircraft_references'
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_aircraft_reference_reachability()'
        )
    )
    OR NOT EXISTS (
      SELECT 1 FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname =
          'faa_registry_engine_references_reachable'
        AND trigger_row.tgrelid = pg_catalog.to_regclass(
          'public.faa_registry_engine_references'
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_engine_reference_reachability()'
        )
    )
    OR NOT EXISTS (
      SELECT 1
      FROM pg_catalog.pg_proc routine
      JOIN pg_catalog.pg_namespace routine_namespace
        ON routine_namespace.oid = routine.pronamespace
      JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
      WHERE routine.oid = pg_catalog.to_regprocedure(
        'public.validate_faa_aircraft_reference_reachability()'
      )
        AND routine_namespace.nspname = 'public'
        AND routine.prosrc = $aircraft_function$
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
$aircraft_function$
        AND routine.proconfig = ARRAY['search_path=pg_catalog']
        AND language.lanname = 'plpgsql'
        AND routine.prorettype = 'trigger'::pg_catalog.regtype
        AND routine.pronargs = 0
        AND routine.prokind = 'f'
        AND NOT routine.prosecdef
        AND NOT routine.proisstrict
        AND routine.provolatile = 'v'
        AND routine.proparallel = 'u'
    )
    OR NOT EXISTS (
      SELECT 1
      FROM pg_catalog.pg_proc routine
      JOIN pg_catalog.pg_namespace routine_namespace
        ON routine_namespace.oid = routine.pronamespace
      JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
      WHERE routine.oid = pg_catalog.to_regprocedure(
        'public.validate_faa_engine_reference_reachability()'
      )
        AND routine_namespace.nspname = 'public'
        AND routine.prosrc = $engine_function$
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
$engine_function$
        AND routine.proconfig = ARRAY['search_path=pg_catalog']
        AND language.lanname = 'plpgsql'
        AND routine.prorettype = 'trigger'::pg_catalog.regtype
        AND routine.pronargs = 0
        AND routine.prokind = 'f'
        AND NOT routine.prosecdef
        AND NOT routine.proisstrict
        AND routine.provolatile = 'v'
        AND routine.proparallel = 'u'
    ) THEN
      RAISE EXCEPTION
        'installed FAA reference reachability objects have an unexpected shape';
    END IF;
  END IF;

  IF EXISTS (
    SELECT 1
    FROM public.faa_registry_aircraft_references reference
    WHERE NOT EXISTS (
      SELECT 1 FROM public.faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = reference.snapshot_id
        AND aircraft.aircraft_code = reference.aircraft_code
    )
  ) OR EXISTS (
    SELECT 1
    FROM public.faa_registry_engine_references reference
    WHERE NOT EXISTS (
      SELECT 1 FROM public.faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = reference.snapshot_id
        AND aircraft.engine_code = reference.engine_code
    )
  ) THEN
    RAISE EXCEPTION
      'existing FAA reference rows are unreachable from target matches';
  END IF;
END
$migration_guard$;

DROP TRIGGER IF EXISTS faa_registry_snapshots_require_exact_evidence
  ON public.faa_registry_snapshots;
CREATE OR REPLACE FUNCTION public.validate_faa_snapshot_evidence()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
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
$function$
SET search_path = pg_catalog;
CREATE TRIGGER faa_registry_snapshots_require_exact_evidence
BEFORE INSERT ON public.faa_registry_snapshots
FOR EACH ROW
EXECUTE FUNCTION public.validate_faa_snapshot_evidence();

DROP TRIGGER IF EXISTS faa_registry_aircraft_references_reachable
  ON public.faa_registry_aircraft_references;
DROP TRIGGER IF EXISTS faa_registry_engine_references_reachable
  ON public.faa_registry_engine_references;
DROP FUNCTION IF EXISTS public.validate_faa_reference_reachability();

CREATE OR REPLACE FUNCTION public.validate_faa_aircraft_reference_reachability()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
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
$function$
SET search_path = pg_catalog;

CREATE OR REPLACE FUNCTION public.validate_faa_engine_reference_reachability()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
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
$function$
SET search_path = pg_catalog;

CREATE TRIGGER faa_registry_aircraft_references_reachable
BEFORE INSERT ON public.faa_registry_aircraft_references
FOR EACH ROW
EXECUTE FUNCTION public.validate_faa_aircraft_reference_reachability();

CREATE TRIGGER faa_registry_engine_references_reachable
BEFORE INSERT ON public.faa_registry_engine_references
FOR EACH ROW
EXECUTE FUNCTION public.validate_faa_engine_reference_reachability();

DROP TRIGGER IF EXISTS faa_registry_coverage_consistent
  ON public.faa_registry_coverage;
CREATE OR REPLACE FUNCTION public.validate_faa_coverage()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
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
$function$
SET search_path = pg_catalog;
CREATE TRIGGER faa_registry_coverage_consistent
BEFORE INSERT ON public.faa_registry_coverage
FOR EACH ROW
EXECUTE FUNCTION public.validate_faa_coverage();

DROP TRIGGER IF EXISTS faa_registry_snapshots_immutable
  ON public.faa_registry_snapshots;
DROP TRIGGER IF EXISTS faa_registry_aircraft_immutable
  ON public.faa_registry_aircraft;
DROP TRIGGER IF EXISTS faa_registry_aircraft_references_immutable
  ON public.faa_registry_aircraft_references;
DROP TRIGGER IF EXISTS faa_registry_engine_references_immutable
  ON public.faa_registry_engine_references;
DROP TRIGGER IF EXISTS faa_registry_coverage_immutable
  ON public.faa_registry_coverage;
CREATE OR REPLACE FUNCTION public.preserve_faa_registry_data()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  RAISE EXCEPTION 'FAA registry snapshots and projections are immutable';
END;
$function$
SET search_path = pg_catalog;
CREATE TRIGGER faa_registry_snapshots_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_snapshots
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();
CREATE TRIGGER faa_registry_aircraft_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_aircraft
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();
CREATE TRIGGER faa_registry_aircraft_references_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_aircraft_references
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();
CREATE TRIGGER faa_registry_engine_references_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_engine_references
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();
CREATE TRIGGER faa_registry_coverage_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_coverage
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();

DO $post_provenance_guard$
DECLARE
  trigger_matches BIGINT;
  trigger_name_count BIGINT;
  function_matches BIGINT;
BEGIN
  WITH expected(
    trigger_name, relation_name, function_name, trigger_type
  ) AS (
    VALUES
      ('faa_registry_snapshots_require_exact_evidence',
       'faa_registry_snapshots', 'validate_faa_snapshot_evidence', 7),
      ('faa_registry_coverage_consistent',
       'faa_registry_coverage', 'validate_faa_coverage', 7),
      ('faa_registry_snapshots_immutable',
       'faa_registry_snapshots', 'preserve_faa_registry_data', 27),
      ('faa_registry_aircraft_immutable',
       'faa_registry_aircraft', 'preserve_faa_registry_data', 27),
      ('faa_registry_aircraft_references_immutable',
       'faa_registry_aircraft_references', 'preserve_faa_registry_data', 27),
      ('faa_registry_engine_references_immutable',
       'faa_registry_engine_references', 'preserve_faa_registry_data', 27),
      ('faa_registry_coverage_immutable',
       'faa_registry_coverage', 'preserve_faa_registry_data', 27)
  )
  SELECT COUNT(*) INTO trigger_matches
  FROM expected
  JOIN pg_catalog.pg_trigger trigger_row
    ON trigger_row.tgname = expected.trigger_name
   AND NOT trigger_row.tgisinternal
  JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
  JOIN pg_catalog.pg_namespace relation_namespace
    ON relation_namespace.oid = relation.relnamespace
  JOIN pg_catalog.pg_proc routine ON routine.oid = trigger_row.tgfoid
  JOIN pg_catalog.pg_namespace routine_namespace
    ON routine_namespace.oid = routine.pronamespace
  WHERE relation_namespace.nspname = 'public'
    AND relation.relname = expected.relation_name
    AND routine_namespace.nspname = 'public'
    AND routine.proname = expected.function_name
    AND routine.pronargs = 0
    AND trigger_row.tgtype = expected.trigger_type
    AND trigger_row.tgenabled = 'O'
    AND trigger_row.tgqual IS NULL
    AND pg_catalog.cardinality(trigger_row.tgattr) = 0
    AND trigger_row.tgnargs = 0;

  SELECT COUNT(*) INTO trigger_name_count
  FROM pg_catalog.pg_trigger trigger_row
  JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
  JOIN pg_catalog.pg_namespace relation_namespace
    ON relation_namespace.oid = relation.relnamespace
  WHERE NOT trigger_row.tgisinternal
    AND relation_namespace.nspname = 'public'
    AND relation.relname IN (
      'faa_registry_aircraft',
      'faa_registry_aircraft_references',
      'faa_registry_coverage',
      'faa_registry_engine_references',
      'faa_registry_snapshots'
    );

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
$immutability_function$)
  )
  SELECT COUNT(*) INTO function_matches
  FROM expected
  JOIN pg_catalog.pg_proc routine ON routine.proname = expected.function_name
  JOIN pg_catalog.pg_namespace routine_namespace
    ON routine_namespace.oid = routine.pronamespace
  JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
  WHERE routine_namespace.nspname = 'public'
    AND routine.prosrc = expected.function_source
    AND routine.proconfig = ARRAY['search_path=pg_catalog']
    AND language.lanname = 'plpgsql'
    AND routine.prorettype = 'trigger'::pg_catalog.regtype
    AND routine.pronargs = 0
    AND routine.prokind = 'f'
    AND NOT routine.prosecdef
    AND NOT routine.proisstrict
    AND routine.provolatile = 'v'
    AND routine.proparallel = 'u';

  IF trigger_matches <> 7 OR trigger_name_count <> 9 OR function_matches <> 3
  THEN
    RAISE EXCEPTION
      'post-migration FAA provenance objects have an unexpected shape';
  END IF;
END
$post_provenance_guard$;

DO $post_migration_guard$
BEGIN
  IF (
    SELECT COUNT(*)
    FROM pg_catalog.pg_trigger trigger_row
    JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
    JOIN pg_catalog.pg_namespace relation_namespace
      ON relation_namespace.oid = relation.relnamespace
    WHERE NOT trigger_row.tgisinternal
      AND relation_namespace.nspname = 'public'
      AND relation.relname IN (
        'faa_registry_aircraft',
        'faa_registry_aircraft_references',
        'faa_registry_coverage',
        'faa_registry_engine_references',
        'faa_registry_snapshots'
      )
  ) <> 9
  OR (
    SELECT COUNT(*)
    FROM pg_catalog.pg_trigger trigger_row
    WHERE NOT trigger_row.tgisinternal
      AND trigger_row.tgname IN (
        'faa_registry_aircraft_references_reachable',
        'faa_registry_engine_references_reachable'
      )
      AND trigger_row.tgrelid IN (
        pg_catalog.to_regclass('public.faa_registry_aircraft_references'),
        pg_catalog.to_regclass('public.faa_registry_engine_references')
      )
      AND trigger_row.tgtype = 7
      AND trigger_row.tgenabled = 'O'
      AND trigger_row.tgqual IS NULL
      AND pg_catalog.cardinality(trigger_row.tgattr) = 0
      AND trigger_row.tgnargs = 0
  ) <> 2
  OR NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger trigger_row
    WHERE trigger_row.tgname = 'faa_registry_aircraft_references_reachable'
      AND trigger_row.tgrelid = pg_catalog.to_regclass(
        'public.faa_registry_aircraft_references'
      )
      AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
        'public.validate_faa_aircraft_reference_reachability()'
      )
  )
  OR NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger trigger_row
    WHERE trigger_row.tgname = 'faa_registry_engine_references_reachable'
      AND trigger_row.tgrelid = pg_catalog.to_regclass(
        'public.faa_registry_engine_references'
      )
      AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
        'public.validate_faa_engine_reference_reachability()'
      )
  )
  OR NOT EXISTS (
    SELECT 1
    FROM pg_catalog.pg_proc routine
    JOIN pg_catalog.pg_namespace routine_namespace
      ON routine_namespace.oid = routine.pronamespace
    JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
    WHERE routine.oid = pg_catalog.to_regprocedure(
      'public.validate_faa_aircraft_reference_reachability()'
    )
      AND routine_namespace.nspname = 'public'
      AND routine.prosrc = $aircraft_function$
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
$aircraft_function$
      AND routine.proconfig = ARRAY['search_path=pg_catalog']
      AND language.lanname = 'plpgsql'
      AND routine.prorettype = 'trigger'::pg_catalog.regtype
      AND routine.pronargs = 0
      AND routine.prokind = 'f'
      AND NOT routine.prosecdef
      AND NOT routine.proisstrict
      AND routine.provolatile = 'v'
      AND routine.proparallel = 'u'
  )
  OR NOT EXISTS (
    SELECT 1
    FROM pg_catalog.pg_proc routine
    JOIN pg_catalog.pg_namespace routine_namespace
      ON routine_namespace.oid = routine.pronamespace
    JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
    WHERE routine.oid = pg_catalog.to_regprocedure(
      'public.validate_faa_engine_reference_reachability()'
    )
      AND routine_namespace.nspname = 'public'
      AND routine.prosrc = $engine_function$
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
$engine_function$
      AND routine.proconfig = ARRAY['search_path=pg_catalog']
      AND language.lanname = 'plpgsql'
      AND routine.prorettype = 'trigger'::pg_catalog.regtype
      AND routine.pronargs = 0
      AND routine.prokind = 'f'
      AND NOT routine.prosecdef
      AND NOT routine.proisstrict
      AND routine.provolatile = 'v'
      AND routine.proparallel = 'u'
  )
  OR EXISTS (
    SELECT 1
    FROM public.faa_registry_aircraft_references reference
    WHERE NOT EXISTS (
      SELECT 1 FROM public.faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = reference.snapshot_id
        AND aircraft.aircraft_code = reference.aircraft_code
    )
  )
  OR EXISTS (
    SELECT 1
    FROM public.faa_registry_engine_references reference
    WHERE NOT EXISTS (
      SELECT 1 FROM public.faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = reference.snapshot_id
        AND aircraft.engine_code = reference.engine_code
    )
  ) THEN
    RAISE EXCEPTION
      'post-migration FAA reference reachability objects have an unexpected shape';
  END IF;
END
$post_migration_guard$;

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_faa_reference_reachability',
  1,
  'fc6451ffe8e1ee2034e76480767d16d6c37463461d9e684687448b4d43f96bef',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
