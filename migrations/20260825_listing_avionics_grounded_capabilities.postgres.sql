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

LOCK TABLE ONLY public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

DO $migration_guard$
DECLARE
  capability_columns TEXT[];
  authorization_columns TEXT[];
  capability_checks TEXT[];
  authorization_checks TEXT[];
  capability_columns_are_exact BOOLEAN;
  authorization_columns_are_exact BOOLEAN;
  capability_relations_are_exact BOOLEAN;
  authorization_relations_are_exact BOOLEAN;
  capability_indexes_are_exact BOOLEAN;
  authorization_indexes_are_exact BOOLEAN;
  capability_checks_are_valid BOOLEAN;
  authorization_checks_are_valid BOOLEAN;
BEGIN
  IF EXISTS (
    SELECT 1
    FROM ONLY public.schema_migration_contracts
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          '682ca4e44ced30b0d14da879c31e0fa4b24cc1b6fceb9f213ecc39d9abca0338'
      )
  ) THEN
    RAISE EXCEPTION
      'installed listing avionics grounded-capability migration has a different contract';
  END IF;

  SELECT pg_catalog.array_agg(attribute.attname ORDER BY attribute.attnum)
  INTO capability_columns
  FROM pg_catalog.pg_attribute attribute
  WHERE attribute.attrelid = pg_catalog.to_regclass(
          'public.aircraft_sale_listing_avionics_grounded_capabilities'
        )
    AND attribute.attnum > 0
    AND NOT attribute.attisdropped;

  WITH expected(name, data_type, not_null, default_expression) AS (
    VALUES
      ('listing_id', 'bigint', TRUE, NULL::TEXT),
      ('plugin_submission_id', 'bigint', TRUE, NULL::TEXT),
      ('occurrence_index', 'bigint', TRUE, NULL::TEXT),
      ('occurrence_role', 'text', TRUE, NULL::TEXT),
      ('avionics_model_id', 'bigint', TRUE, NULL::TEXT),
      ('requested_quantity', 'bigint', TRUE, NULL::TEXT),
      ('configuration_action', 'text', TRUE, NULL::TEXT),
      ('request_sha256', 'text', TRUE, NULL::TEXT),
      ('capability_sha256', 'text', TRUE, NULL::TEXT),
      ('grounded_resolution_sha256', 'text', TRUE, NULL::TEXT),
      ('evidence_capture_sha256', 'text', TRUE, NULL::TEXT),
      ('extracted_listing_sha256', 'text', TRUE, NULL::TEXT),
      ('product_fingerprint', 'text', TRUE, NULL::TEXT),
      ('collision_closure_sha256', 'text', TRUE, NULL::TEXT),
      ('source_revocation_count', 'bigint', TRUE, NULL::TEXT),
      ('policy_version', 'text', TRUE, NULL::TEXT),
      ('created_at', 'text', TRUE, 'CURRENT_TIMESTAMP')
  )
  SELECT
    (SELECT COUNT(*)
     FROM pg_catalog.pg_attribute actual
     WHERE actual.attrelid = pg_catalog.to_regclass(
             'public.aircraft_sale_listing_avionics_grounded_capabilities'
           )
       AND actual.attnum > 0
       AND NOT actual.attisdropped) = 17
    AND NOT EXISTS (
      SELECT 1
      FROM expected
      WHERE NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_attribute actual
        LEFT JOIN pg_catalog.pg_attrdef default_value
          ON default_value.adrelid = actual.attrelid
         AND default_value.adnum = actual.attnum
        WHERE actual.attrelid = pg_catalog.to_regclass(
                'public.aircraft_sale_listing_avionics_grounded_capabilities'
              )
          AND actual.attname = expected.name
          AND pg_catalog.format_type(actual.atttypid, actual.atttypmod) =
                expected.data_type
          AND actual.attnotnull = expected.not_null
          AND actual.attidentity = ''
          AND actual.attgenerated = ''
          AND (
            (expected.default_expression IS NULL AND default_value.oid IS NULL)
            OR pg_catalog.pg_get_expr(
                 default_value.adbin, default_value.adrelid
               ) = expected.default_expression
          )
      )
    )
  INTO capability_columns_are_exact;

  SELECT pg_catalog.array_agg(attribute.attname ORDER BY attribute.attnum)
  INTO authorization_columns
  FROM pg_catalog.pg_attribute attribute
  WHERE attribute.attrelid = pg_catalog.to_regclass(
          'public.aircraft_sale_listing_avionics_link_authorizations'
        )
    AND attribute.attnum > 0
    AND NOT attribute.attisdropped;

  WITH expected(name, data_type, not_null, default_expression) AS (
    VALUES
      ('listing_link_id', 'bigint', TRUE, NULL::TEXT),
      ('association_role', 'text', TRUE, NULL::TEXT),
      ('avionics_model_id', 'bigint', TRUE, NULL::TEXT),
      ('authorization_kind', 'text', TRUE, NULL::TEXT),
      ('observation_sha256', 'text', TRUE, NULL::TEXT),
      ('product_fingerprint', 'text', TRUE, NULL::TEXT),
      ('grounded_resolution_sha256', 'text', FALSE, NULL::TEXT),
      ('evidence_capture_sha256', 'text', TRUE, NULL::TEXT),
      ('plugin_submission_id', 'bigint', FALSE, NULL::TEXT),
      ('extracted_listing_sha256', 'text', FALSE, NULL::TEXT),
      ('collision_closure_sha256', 'text', TRUE, NULL::TEXT),
      ('source_revocation_count', 'bigint', FALSE, NULL::TEXT),
      ('policy_version', 'text', TRUE, NULL::TEXT),
      ('authorized_at', 'text', TRUE, 'CURRENT_TIMESTAMP')
  )
  SELECT
    (SELECT COUNT(*)
     FROM pg_catalog.pg_attribute actual
     WHERE actual.attrelid = pg_catalog.to_regclass(
             'public.aircraft_sale_listing_avionics_link_authorizations'
           )
       AND actual.attnum > 0
       AND NOT actual.attisdropped) = 14
    AND NOT EXISTS (
      SELECT 1
      FROM expected
      WHERE NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_attribute actual
        LEFT JOIN pg_catalog.pg_attrdef default_value
          ON default_value.adrelid = actual.attrelid
         AND default_value.adnum = actual.attnum
        WHERE actual.attrelid = pg_catalog.to_regclass(
                'public.aircraft_sale_listing_avionics_link_authorizations'
              )
          AND actual.attname = expected.name
          AND pg_catalog.format_type(actual.atttypid, actual.atttypmod) =
                expected.data_type
          AND actual.attnotnull = expected.not_null
          AND actual.attidentity = ''
          AND actual.attgenerated = ''
          AND (
            (expected.default_expression IS NULL AND default_value.oid IS NULL)
            OR pg_catalog.pg_get_expr(
                 default_value.adbin, default_value.adrelid
               ) = expected.default_expression
          )
      )
    )
  INTO authorization_columns_are_exact;

  SELECT
    pg_catalog.array_agg(
      pg_catalog.pg_get_constraintdef(actual.oid)
      ORDER BY pg_catalog.pg_get_constraintdef(actual.oid)
    ),
    COALESCE(
      pg_catalog.bool_and(actual.convalidated AND NOT actual.connoinherit),
      FALSE
    )
  INTO capability_checks, capability_checks_are_valid
  FROM pg_catalog.pg_constraint actual
  WHERE actual.conrelid = pg_catalog.to_regclass(
          'public.aircraft_sale_listing_avionics_grounded_capabilities'
        )
    AND actual.contype = 'c';

  SELECT
    pg_catalog.array_agg(
      pg_catalog.pg_get_constraintdef(actual.oid)
      ORDER BY pg_catalog.pg_get_constraintdef(actual.oid)
    ),
    COALESCE(
      pg_catalog.bool_and(actual.convalidated AND NOT actual.connoinherit),
      FALSE
    )
  INTO authorization_checks, authorization_checks_are_valid
  FROM pg_catalog.pg_constraint actual
  WHERE actual.conrelid = pg_catalog.to_regclass(
          'public.aircraft_sale_listing_avionics_link_authorizations'
        )
    AND actual.contype = 'c';

  WITH expected(parent_name, child_column) AS (
    VALUES
      ('public.aircraft_sale_listings', 'listing_id'),
      ('public.plugin_submissions', 'plugin_submission_id'),
      ('public.avionics_models', 'avionics_model_id')
  )
  SELECT
    (SELECT COUNT(*)
     FROM pg_catalog.pg_constraint actual
     WHERE actual.conrelid = pg_catalog.to_regclass(
             'public.aircraft_sale_listing_avionics_grounded_capabilities'
           )
       AND actual.contype = 'f') = 3
    AND NOT EXISTS (
      SELECT 1 FROM expected
      WHERE NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint actual
        JOIN pg_catalog.pg_attribute child_attribute
          ON child_attribute.attrelid = actual.conrelid
         AND child_attribute.attnum = actual.conkey[1]
        WHERE actual.conrelid = pg_catalog.to_regclass(
                'public.aircraft_sale_listing_avionics_grounded_capabilities'
              )
          AND actual.contype = 'f'
          AND actual.confrelid = pg_catalog.to_regclass(expected.parent_name)
          AND child_attribute.attname = expected.child_column
          AND pg_catalog.array_length(actual.conkey, 1) = 1
          AND actual.confupdtype = 'a'
          AND actual.confdeltype = 'c'
          AND actual.confmatchtype = 's'
          AND NOT actual.condeferrable
          AND NOT actual.condeferred
          AND actual.convalidated
      )
    )
    AND (SELECT COUNT(*)
         FROM pg_catalog.pg_constraint actual
         WHERE actual.conrelid = pg_catalog.to_regclass(
                 'public.aircraft_sale_listing_avionics_grounded_capabilities'
               )
           AND actual.contype = 'p'
           AND NOT actual.condeferrable
           AND NOT actual.condeferred
           AND actual.convalidated
           AND pg_catalog.pg_get_constraintdef(actual.oid) =
                 'PRIMARY KEY (listing_id, plugin_submission_id, occurrence_index, occurrence_role)') = 1
  INTO capability_relations_are_exact;

  WITH expected(parent_name, child_column) AS (
    VALUES
      ('public.aircraft_sale_listing_avionics', 'listing_link_id'),
      ('public.avionics_models', 'avionics_model_id'),
      ('public.plugin_submissions', 'plugin_submission_id')
  )
  SELECT
    (SELECT COUNT(*)
     FROM pg_catalog.pg_constraint actual
     WHERE actual.conrelid = pg_catalog.to_regclass(
             'public.aircraft_sale_listing_avionics_link_authorizations'
           )
       AND actual.contype = 'f') = 3
    AND NOT EXISTS (
      SELECT 1 FROM expected
      WHERE NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_constraint actual
        JOIN pg_catalog.pg_attribute child_attribute
          ON child_attribute.attrelid = actual.conrelid
         AND child_attribute.attnum = actual.conkey[1]
        WHERE actual.conrelid = pg_catalog.to_regclass(
                'public.aircraft_sale_listing_avionics_link_authorizations'
              )
          AND actual.contype = 'f'
          AND actual.confrelid = pg_catalog.to_regclass(expected.parent_name)
          AND child_attribute.attname = expected.child_column
          AND pg_catalog.array_length(actual.conkey, 1) = 1
          AND actual.confupdtype = 'a'
          AND actual.confdeltype = 'c'
          AND actual.confmatchtype = 's'
          AND NOT actual.condeferrable
          AND NOT actual.condeferred
          AND actual.convalidated
      )
    )
    AND (SELECT COUNT(*)
         FROM pg_catalog.pg_constraint actual
         WHERE actual.conrelid = pg_catalog.to_regclass(
                 'public.aircraft_sale_listing_avionics_link_authorizations'
               )
           AND actual.contype = 'p'
           AND NOT actual.condeferrable
           AND NOT actual.condeferred
           AND actual.convalidated
           AND pg_catalog.pg_get_constraintdef(actual.oid) =
                 'PRIMARY KEY (listing_link_id, association_role)') = 1
  INTO authorization_relations_are_exact;

  WITH expected(index_name, table_name, column_name) AS (
    VALUES
      ('idx_listing_avionics_grounded_capabilities_model',
       'public.aircraft_sale_listing_avionics_grounded_capabilities',
       'avionics_model_id'),
      ('idx_listing_avionics_grounded_capabilities_submission',
       'public.aircraft_sale_listing_avionics_grounded_capabilities',
       'plugin_submission_id')
  )
  SELECT
    (SELECT COUNT(*)
     FROM pg_catalog.pg_index actual
     WHERE actual.indrelid = pg_catalog.to_regclass(
             'public.aircraft_sale_listing_avionics_grounded_capabilities'
           )
       AND NOT actual.indisprimary) = 2
    AND NOT EXISTS (
      SELECT 1 FROM expected
      WHERE NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_index actual
        JOIN pg_catalog.pg_class index_relation
          ON index_relation.oid = actual.indexrelid
        JOIN pg_catalog.pg_class table_relation
          ON table_relation.oid = actual.indrelid
        JOIN pg_catalog.pg_am access_method
          ON access_method.oid = index_relation.relam
        WHERE index_relation.relname = expected.index_name
          AND actual.indrelid = pg_catalog.to_regclass(expected.table_name)
          AND access_method.amname = 'btree'
          AND NOT actual.indisunique
          AND NOT actual.indisexclusion
          AND actual.indisvalid
          AND actual.indisready
          AND actual.indislive
          AND actual.indisreplident = FALSE
          AND actual.indnatts = 1
          AND actual.indnkeyatts = 1
          AND actual.indexprs IS NULL
          AND actual.indpred IS NULL
          AND pg_catalog.pg_get_indexdef(actual.indexrelid, 1, TRUE) =
                expected.column_name
      )
    )
  INTO capability_indexes_are_exact;

  WITH expected(index_name, table_name, column_name) AS (
    VALUES
      ('idx_listing_avionics_authorizations_model',
       'public.aircraft_sale_listing_avionics_link_authorizations',
       'avionics_model_id')
  )
  SELECT
    (SELECT COUNT(*)
     FROM pg_catalog.pg_index actual
     WHERE actual.indrelid = pg_catalog.to_regclass(
             'public.aircraft_sale_listing_avionics_link_authorizations'
           )
       AND NOT actual.indisprimary) = 1
    AND NOT EXISTS (
      SELECT 1 FROM expected
      WHERE NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_index actual
        JOIN pg_catalog.pg_class index_relation
          ON index_relation.oid = actual.indexrelid
        JOIN pg_catalog.pg_am access_method
          ON access_method.oid = index_relation.relam
        WHERE index_relation.relname = expected.index_name
          AND actual.indrelid = pg_catalog.to_regclass(expected.table_name)
          AND access_method.amname = 'btree'
          AND NOT actual.indisunique
          AND NOT actual.indisexclusion
          AND actual.indisvalid
          AND actual.indisready
          AND actual.indislive
          AND actual.indisreplident = FALSE
          AND actual.indnatts = 1
          AND actual.indnkeyatts = 1
          AND actual.indexprs IS NULL
          AND actual.indpred IS NULL
          AND pg_catalog.pg_get_indexdef(actual.indexrelid, 1, TRUE) =
                expected.column_name
      )
    )
  INTO authorization_indexes_are_exact;

  IF EXISTS (
    SELECT 1
    FROM ONLY public.schema_migration_contracts
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
  ) THEN
    IF capability_columns IS DISTINCT FROM ARRAY[
      'listing_id', 'plugin_submission_id', 'occurrence_index',
      'occurrence_role', 'avionics_model_id', 'requested_quantity',
      'configuration_action', 'request_sha256', 'capability_sha256',
      'grounded_resolution_sha256', 'evidence_capture_sha256',
      'extracted_listing_sha256', 'product_fingerprint',
      'collision_closure_sha256', 'source_revocation_count',
      'policy_version', 'created_at'
    ]::TEXT[] OR authorization_columns IS DISTINCT FROM ARRAY[
      'listing_link_id', 'association_role', 'avionics_model_id',
      'authorization_kind', 'observation_sha256', 'product_fingerprint',
      'grounded_resolution_sha256', 'evidence_capture_sha256',
      'plugin_submission_id', 'extracted_listing_sha256',
      'collision_closure_sha256', 'source_revocation_count',
      'policy_version', 'authorized_at'
    ]::TEXT[]
    OR NOT capability_columns_are_exact
    OR NOT authorization_columns_are_exact
    OR capability_checks IS DISTINCT FROM ARRAY[
      'CHECK (((occurrence_role = ''primary''::text) OR (configuration_action = ANY (ARRAY[''replaces''::text, ''removes''::text]))))',
      'CHECK (((occurrence_role = ''primary''::text) OR (requested_quantity = 1)))',
      'CHECK ((capability_sha256 ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((collision_closure_sha256 ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((configuration_action = ANY (ARRAY[''installed''::text, ''replaces''::text, ''removes''::text])))',
      'CHECK ((evidence_capture_sha256 ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((extracted_listing_sha256 ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((grounded_resolution_sha256 ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((occurrence_index >= 0))',
      'CHECK ((occurrence_role = ANY (ARRAY[''primary''::text, ''replacement''::text])))',
      'CHECK ((policy_version = ''listing_avionics_grounded_capability''::text))',
      'CHECK ((product_fingerprint ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((request_sha256 ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((requested_quantity > 0))',
      'CHECK ((source_revocation_count >= 0))'
    ]::TEXT[]
    OR authorization_checks IS DISTINCT FROM ARRAY[
      'CHECK ((((authorization_kind = ''manufacturer_reuse''::text) AND (grounded_resolution_sha256 IS NULL) AND (plugin_submission_id IS NULL) AND (extracted_listing_sha256 IS NULL) AND (source_revocation_count IS NULL)) OR ((authorization_kind = ''same_case_grounded''::text) AND (grounded_resolution_sha256 ~ ''^[0-9a-f]{64}$''::text) AND (plugin_submission_id IS NOT NULL) AND (extracted_listing_sha256 IS NOT NULL) AND (source_revocation_count IS NOT NULL) AND (source_revocation_count >= 0))))',
      'CHECK (((extracted_listing_sha256 IS NULL) OR (extracted_listing_sha256 ~ ''^[0-9a-f]{64}$''::text)))',
      'CHECK ((association_role = ANY (ARRAY[''installed''::text, ''replacement''::text])))',
      'CHECK ((authorization_kind = ANY (ARRAY[''manufacturer_reuse''::text, ''same_case_grounded''::text])))',
      'CHECK ((collision_closure_sha256 ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((evidence_capture_sha256 ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((observation_sha256 ~ ''^[0-9a-f]{64}$''::text))',
      'CHECK ((policy_version = ''listing_avionics_authorization''::text))',
      'CHECK ((product_fingerprint ~ ''^[0-9a-f]{64}$''::text))'
    ]::TEXT[]
    OR NOT capability_checks_are_valid
    OR NOT authorization_checks_are_valid
    OR NOT capability_relations_are_exact
    OR NOT authorization_relations_are_exact
    OR NOT capability_indexes_are_exact
    OR NOT authorization_indexes_are_exact THEN
      RAISE EXCEPTION
        'installed listing avionics grounded-capability migration has an unexpected table shape';
    END IF;
  ELSIF capability_columns IS NOT NULL OR authorization_columns IS NOT NULL THEN
    RAISE EXCEPTION
      'uninstalled listing avionics grounded-capability migration found a preexisting final table';
  END IF;
END
$migration_guard$;

CREATE TEMP VIEW listing_avionics_grounded_capability_current_objects AS
SELECT
  'index'::TEXT AS object_kind,
  index_relation.relname::TEXT AS object_name,
  pg_catalog.jsonb_build_object(
    'definition', pg_catalog.pg_get_indexdef(actual.indexrelid),
    'is_unique', actual.indisunique,
    'is_primary', actual.indisprimary,
    'is_exclusion', actual.indisexclusion,
    'is_immediate', actual.indimmediate,
    'is_clustered', actual.indisclustered,
    'is_valid', actual.indisvalid,
    'is_ready', actual.indisready,
    'is_live', actual.indislive,
    'is_replica_identity', actual.indisreplident
  )::TEXT AS object_definition
FROM pg_catalog.pg_index actual
JOIN pg_catalog.pg_class index_relation
  ON index_relation.oid = actual.indexrelid
JOIN pg_catalog.pg_namespace index_namespace
  ON index_namespace.oid = index_relation.relnamespace
WHERE index_namespace.nspname = 'public'
  AND index_relation.relname = ANY (ARRAY[
    'idx_listing_avionics_grounded_capabilities_model',
    'idx_listing_avionics_grounded_capabilities_submission',
    'idx_listing_avionics_authorizations_model'
  ]::TEXT[])

UNION ALL

SELECT
  'trigger'::TEXT AS object_kind,
  actual.tgname::TEXT AS object_name,
  pg_catalog.jsonb_build_object(
    'definition', pg_catalog.pg_get_triggerdef(actual.oid),
    'trigger_type', actual.tgtype,
    'enabled', actual.tgenabled,
    'has_when_clause', actual.tgqual IS NOT NULL,
    'argument_count', actual.tgnargs,
    'arguments', pg_catalog.encode(actual.tgargs, 'hex'),
    'parent_id', actual.tgparentid,
    'constraint_id', actual.tgconstraint,
    'old_table', actual.tgoldtable,
    'new_table', actual.tgnewtable,
    'update_columns', actual.tgattr::TEXT,
    'is_internal', actual.tgisinternal
  )::TEXT AS object_definition
FROM pg_catalog.pg_trigger actual
WHERE NOT actual.tgisinternal
  AND actual.tgname = ANY (ARRAY[
    'listing_avionics_grounded_capabilities_immutable_update',
    'listing_avionics_grounded_capabilities_validate_insert',
    'listing_avionics_authorizations_immutable_update',
    'listing_avionics_authorizations_invalidate_capture_delete',
    'listing_avionics_authorizations_invalidate_capture_update',
    'listing_avionics_authorizations_invalidate_graph_delete',
    'listing_avionics_authorizations_invalidate_graph_insert',
    'listing_avionics_authorizations_invalidate_graph_update',
    'listing_avionics_authorizations_invalidate_link_update',
    'listing_avionics_authorizations_invalidate_manufacturer_update',
    'listing_avionics_authorizations_invalidate_model_proof_update',
    'listing_avionics_authorizations_invalidate_model_type_delete',
    'listing_avionics_authorizations_invalidate_model_type_insert',
    'listing_avionics_authorizations_invalidate_model_type_update',
    'listing_avionics_authorizations_invalidate_origin_revocation',
    'listing_avionics_authorizations_invalidate_reuse_delete',
    'listing_avionics_authorizations_invalidate_type_update',
    'listing_avionics_authorizations_validate_insert'
  ]::TEXT[])

UNION ALL

SELECT
  'function'::TEXT AS object_kind,
  actual.proname::TEXT AS object_name,
  pg_catalog.jsonb_build_object(
    'definition', pg_catalog.regexp_replace(
      pg_catalog.pg_get_functiondef(actual.oid), '[[:space:]]+', '', 'g'
    ),
    'configuration', COALESCE(
      pg_catalog.array_to_string(actual.proconfig, E'\n'), ''
    ),
    'security_definer', actual.prosecdef,
    'kind', actual.prokind,
    'return_type', actual.prorettype,
    'argument_count', actual.pronargs,
    'language', language_row.lanname,
    'is_strict', actual.proisstrict,
    'volatility', actual.provolatile,
    'parallel_safety', actual.proparallel,
    'is_leakproof', actual.proleakproof
  )::TEXT AS object_definition
FROM pg_catalog.pg_proc actual
JOIN pg_catalog.pg_namespace function_namespace
  ON function_namespace.oid = actual.pronamespace
JOIN pg_catalog.pg_language language_row
  ON language_row.oid = actual.prolang
WHERE function_namespace.nspname = 'public'
  AND actual.pronargs = 0
  AND actual.proname = ANY (ARRAY[
    'validate_listing_avionics_grounded_capability',
    'reject_listing_avionics_grounded_capability_update',
    'validate_listing_avionics_authorization',
    'preserve_listing_avionics_authorization',
    'invalidate_listing_avionics_authorization_for_link',
    'invalidate_listing_avionics_authorization_for_reuse',
    'invalidate_listing_avionics_same_case_for_model_proof',
    'invalidate_listing_avionics_same_case_for_model_type',
    'invalidate_listing_avionics_same_case_for_type',
    'invalidate_listing_avionics_same_case_for_graph',
    'invalidate_listing_avionics_same_case_for_manufacturer',
    'invalidate_listing_avionics_same_case_for_origin_revocation',
    'invalidate_listing_avionics_authorization_for_capture'
  ]::TEXT[]);

CREATE TEMP TABLE listing_avionics_grounded_capability_preexisting_objects (
  object_kind TEXT NOT NULL,
  object_name TEXT NOT NULL,
  object_definition TEXT NOT NULL,
  PRIMARY KEY (object_kind, object_name)
) ON COMMIT DROP;

INSERT INTO listing_avionics_grounded_capability_preexisting_objects (
  object_kind, object_name, object_definition
)
SELECT object_kind, object_name, object_definition
FROM listing_avionics_grounded_capability_current_objects
WHERE EXISTS (
  SELECT 1
  FROM ONLY public.schema_migration_contracts
  WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
);

DO $object_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM ONLY public.schema_migration_contracts
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
  ) AND (
    (SELECT COUNT(*)
     FROM listing_avionics_grounded_capability_preexisting_objects) <> 34
    OR (SELECT COUNT(*)
        FROM pg_catalog.pg_trigger actual
        WHERE NOT actual.tgisinternal
          AND actual.tgrelid = pg_catalog.to_regclass(
                'public.aircraft_sale_listing_avionics_grounded_capabilities'
              )) <> 2
    OR (SELECT COUNT(*)
        FROM pg_catalog.pg_trigger actual
        JOIN pg_catalog.pg_proc routine ON routine.oid = actual.tgfoid
        WHERE NOT actual.tgisinternal
          AND (
            actual.tgrelid = pg_catalog.to_regclass(
              'public.aircraft_sale_listing_avionics_link_authorizations'
            )
            OR pg_catalog.strpos(
              routine.prosrc,
              'aircraft_sale_listing_avionics_link_authorizations'
            ) > 0
          )) <> 16
  ) THEN
    RAISE EXCEPTION
      'installed listing avionics grounded-capability migration has an unexpected object set';
  ELSIF NOT EXISTS (
    SELECT 1
    FROM ONLY public.schema_migration_contracts
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
  )
  AND EXISTS (
    SELECT 1 FROM listing_avionics_grounded_capability_current_objects
  ) THEN
    RAISE EXCEPTION
      'uninstalled listing avionics grounded-capability migration found protected objects';
  END IF;
END
$object_guard$;

CREATE TABLE IF NOT EXISTS public.aircraft_sale_listing_avionics_grounded_capabilities (
  listing_id BIGINT NOT NULL
    REFERENCES public.aircraft_sale_listings(id) ON DELETE CASCADE,
  plugin_submission_id BIGINT NOT NULL
    REFERENCES public.plugin_submissions(id) ON DELETE CASCADE,
  occurrence_index BIGINT NOT NULL CHECK (occurrence_index >= 0),
  occurrence_role TEXT NOT NULL
    CHECK (occurrence_role IN ('primary', 'replacement')),
  avionics_model_id BIGINT NOT NULL
    REFERENCES public.avionics_models(id) ON DELETE CASCADE,
  requested_quantity BIGINT NOT NULL CHECK (requested_quantity > 0),
  configuration_action TEXT NOT NULL
    CHECK (configuration_action IN ('installed', 'replaces', 'removes')),
  request_sha256 TEXT NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  capability_sha256 TEXT NOT NULL
    CHECK (capability_sha256 ~ '^[0-9a-f]{64}$'),
  grounded_resolution_sha256 TEXT NOT NULL
    CHECK (grounded_resolution_sha256 ~ '^[0-9a-f]{64}$'),
  evidence_capture_sha256 TEXT NOT NULL
    CHECK (evidence_capture_sha256 ~ '^[0-9a-f]{64}$'),
  extracted_listing_sha256 TEXT NOT NULL
    CHECK (extracted_listing_sha256 ~ '^[0-9a-f]{64}$'),
  product_fingerprint TEXT NOT NULL
    CHECK (product_fingerprint ~ '^[0-9a-f]{64}$'),
  collision_closure_sha256 TEXT NOT NULL
    CHECK (collision_closure_sha256 ~ '^[0-9a-f]{64}$'),
  source_revocation_count BIGINT NOT NULL
    CHECK (source_revocation_count >= 0),
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'listing_avionics_grounded_capability'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (
    listing_id, plugin_submission_id, occurrence_index, occurrence_role
  ),
  CHECK (occurrence_role = 'primary' OR requested_quantity = 1),
  CHECK (
    occurrence_role = 'primary'
    OR configuration_action IN ('replaces', 'removes')
  )
);

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_grounded_capabilities_model
ON public.aircraft_sale_listing_avionics_grounded_capabilities (avionics_model_id);

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_grounded_capabilities_submission
ON public.aircraft_sale_listing_avionics_grounded_capabilities (plugin_submission_id);

CREATE OR REPLACE FUNCTION public.validate_listing_avionics_grounded_capability()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.plugin_submissions submission
    WHERE submission.id = NEW.plugin_submission_id
      AND submission.canonical_listing_id = NEW.listing_id
      AND submission.rendered_html_sha256 = NEW.evidence_capture_sha256
      AND submission.extracted_listing_json IS NOT NULL
      AND submission.extraction_error IS NULL
  ) OR NOT EXISTS (
    SELECT 1
    FROM public.avionics_approved_product_graph_identities approved
    WHERE approved.avionics_model_id = NEW.avionics_model_id
  ) OR NEW.source_revocation_count <> (
    SELECT COUNT(*)
    FROM public.avionics_authoritative_source_origin_revocations
  ) THEN
    RAISE EXCEPTION 'grounded avionics capability requires its exact current capture-bound listing, approved product, and source-revocation epoch';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS listing_avionics_grounded_capabilities_validate_insert
  ON public.aircraft_sale_listing_avionics_grounded_capabilities;
CREATE TRIGGER listing_avionics_grounded_capabilities_validate_insert
BEFORE INSERT ON public.aircraft_sale_listing_avionics_grounded_capabilities
FOR EACH ROW EXECUTE FUNCTION public.validate_listing_avionics_grounded_capability();

CREATE OR REPLACE FUNCTION public.reject_listing_avionics_grounded_capability_update()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  RAISE EXCEPTION 'grounded avionics capabilities are immutable';
END;
$function$;

DROP TRIGGER IF EXISTS listing_avionics_grounded_capabilities_immutable_update
  ON public.aircraft_sale_listing_avionics_grounded_capabilities;
CREATE TRIGGER listing_avionics_grounded_capabilities_immutable_update
BEFORE UPDATE ON public.aircraft_sale_listing_avionics_grounded_capabilities
FOR EACH ROW EXECUTE FUNCTION public.reject_listing_avionics_grounded_capability_update();

CREATE TABLE IF NOT EXISTS public.aircraft_sale_listing_avionics_link_authorizations (
  listing_link_id BIGINT NOT NULL
    REFERENCES public.aircraft_sale_listing_avionics(id) ON DELETE CASCADE,
  association_role TEXT NOT NULL
    CHECK (association_role IN ('installed', 'replacement')),
  avionics_model_id BIGINT NOT NULL
    REFERENCES public.avionics_models(id) ON DELETE CASCADE,
  authorization_kind TEXT NOT NULL
    CHECK (authorization_kind IN ('manufacturer_reuse', 'same_case_grounded')),
  observation_sha256 TEXT NOT NULL
    CHECK (observation_sha256 ~ '^[0-9a-f]{64}$'),
  product_fingerprint TEXT NOT NULL
    CHECK (product_fingerprint ~ '^[0-9a-f]{64}$'),
  grounded_resolution_sha256 TEXT,
  evidence_capture_sha256 TEXT NOT NULL
    CHECK (evidence_capture_sha256 ~ '^[0-9a-f]{64}$'),
  plugin_submission_id BIGINT
    REFERENCES public.plugin_submissions(id) ON DELETE CASCADE,
  extracted_listing_sha256 TEXT
    CHECK (extracted_listing_sha256 IS NULL OR
           extracted_listing_sha256 ~ '^[0-9a-f]{64}$'),
  collision_closure_sha256 TEXT NOT NULL
    CHECK (collision_closure_sha256 ~ '^[0-9a-f]{64}$'),
  source_revocation_count BIGINT,
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'listing_avionics_authorization'),
  authorized_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (listing_link_id, association_role),
  CHECK (
    (authorization_kind = 'manufacturer_reuse'
      AND grounded_resolution_sha256 IS NULL
      AND plugin_submission_id IS NULL
      AND extracted_listing_sha256 IS NULL
      AND source_revocation_count IS NULL)
    OR
    (authorization_kind = 'same_case_grounded'
      AND grounded_resolution_sha256 ~ '^[0-9a-f]{64}$'
      AND plugin_submission_id IS NOT NULL
      AND extracted_listing_sha256 IS NOT NULL
      AND source_revocation_count IS NOT NULL
      AND source_revocation_count >= 0)
  )
);

CREATE INDEX IF NOT EXISTS idx_listing_avionics_authorizations_model
ON public.aircraft_sale_listing_avionics_link_authorizations (avionics_model_id);

CREATE OR REPLACE FUNCTION public.validate_listing_avionics_authorization()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.aircraft_sale_listing_avionics link
    WHERE link.id = NEW.listing_link_id
      AND link.source_confidence = 'high'
      AND length(BTRIM(COALESCE(link.source_notes, ''))) > 0
      AND (
        (NEW.association_role = 'installed'
          AND link.avionics_model_id = NEW.avionics_model_id)
        OR
        (NEW.association_role = 'replacement'
          AND link.configuration_action IN ('replaces', 'removes')
          AND link.replaces_avionics_model_id = NEW.avionics_model_id)
      )
      AND (
        (NEW.authorization_kind = 'manufacturer_reuse'
          AND EXISTS (
            SELECT 1 FROM public.plugin_submissions capture
            WHERE capture.canonical_listing_id = link.aircraft_sale_listing_id
              AND capture.rendered_html_sha256 = NEW.evidence_capture_sha256
              AND position(link.source_notes IN capture.rendered_html) > 0
          )
          AND EXISTS (
            SELECT 1 FROM public.avionics_product_reuse_attestations attestation
            WHERE attestation.avionics_model_id = NEW.avionics_model_id
              AND attestation.product_fingerprint = NEW.product_fingerprint
          ))
        OR
        (NEW.authorization_kind = 'same_case_grounded'
          AND EXISTS (
            SELECT 1 FROM public.plugin_submissions submission
            WHERE submission.id = NEW.plugin_submission_id
              AND submission.canonical_listing_id = link.aircraft_sale_listing_id
              AND submission.rendered_html_sha256 = NEW.evidence_capture_sha256
              AND submission.extracted_listing_json IS NOT NULL
              AND submission.extraction_error IS NULL
              AND position(link.source_notes IN submission.rendered_html) > 0
          )
          AND EXISTS (
            SELECT 1 FROM public.avionics_approved_product_graph_identities identity
            WHERE identity.avionics_model_id = NEW.avionics_model_id
          )
          AND NEW.source_revocation_count = (
            SELECT COUNT(*)
            FROM public.avionics_authoritative_source_origin_revocations
          ))
      )
  ) THEN
    RAISE EXCEPTION
      'listing avionics authorization requires the exact current link role, retained capture, and product proof';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS listing_avionics_authorizations_validate_insert
ON public.aircraft_sale_listing_avionics_link_authorizations;
CREATE TRIGGER listing_avionics_authorizations_validate_insert
BEFORE INSERT ON public.aircraft_sale_listing_avionics_link_authorizations
FOR EACH ROW EXECUTE FUNCTION public.validate_listing_avionics_authorization();

CREATE OR REPLACE FUNCTION public.preserve_listing_avionics_authorization()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  RAISE EXCEPTION 'listing avionics authorizations are replaced, never updated';
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS listing_avionics_authorizations_immutable_update
ON public.aircraft_sale_listing_avionics_link_authorizations;
CREATE TRIGGER listing_avionics_authorizations_immutable_update
BEFORE UPDATE ON public.aircraft_sale_listing_avionics_link_authorizations
FOR EACH ROW EXECUTE FUNCTION public.preserve_listing_avionics_authorization();

CREATE OR REPLACE FUNCTION public.invalidate_listing_avionics_authorization_for_link()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
  WHERE listing_link_id = NEW.id;
  RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION public.invalidate_listing_avionics_authorization_for_reuse()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'manufacturer_reuse'
    AND avionics_model_id = OLD.avionics_model_id;
  RETURN OLD;
END;
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_model_proof()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = OLD.id;
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_model_type()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF TG_OP IN ('DELETE', 'UPDATE') THEN
    DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = OLD.avionics_model_id;
  END IF;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = NEW.avionics_model_id;
  END IF;
  IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_type()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT avionics_model_id FROM public.avionics_model_types
      WHERE avionics_type_id = OLD.id
    );
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_graph()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF TG_OP IN ('DELETE', 'UPDATE') THEN
    DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = OLD.avionics_model_id;
  END IF;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = NEW.avionics_model_id;
  END IF;
  IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_manufacturer()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT id FROM public.avionics_models
      WHERE avionics_manufacturer_id = OLD.id
    );
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_origin_revocation()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_grounded_capabilities;
  DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded';
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_authorization_for_capture()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations authorization_row
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
  DELETE FROM public.aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND plugin_submission_id = OLD.id;
  IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_link_update
ON public.aircraft_sale_listing_avionics;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_link_update
AFTER UPDATE OF
  aircraft_sale_listing_id,
  avionics_model_id,
  quantity,
  source_notes,
  source_confidence,
  configuration_action,
  replaces_avionics_model_id
ON public.aircraft_sale_listing_avionics
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_authorization_for_link();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_reuse_delete
ON public.avionics_product_reuse_attestations;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_reuse_delete
AFTER DELETE ON public.avionics_product_reuse_attestations
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_authorization_for_reuse();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_model_proof_update
ON public.avionics_models;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_model_proof_update
AFTER UPDATE OF
  avionics_manufacturer_id, name, normalized_name, catalog_status,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text
ON public.avionics_models
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_same_case_for_model_proof();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_model_type_insert
ON public.avionics_model_types;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_model_type_insert
AFTER INSERT ON public.avionics_model_types
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_same_case_for_model_type();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_model_type_delete
ON public.avionics_model_types;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_model_type_delete
AFTER DELETE ON public.avionics_model_types
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_same_case_for_model_type();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_model_type_update
ON public.avionics_model_types;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_model_type_update
AFTER UPDATE OF avionics_model_id, avionics_type_id
ON public.avionics_model_types
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_same_case_for_model_type();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_type_update
ON public.avionics_types;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_type_update
AFTER UPDATE OF name, normalized_name ON public.avionics_types
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_same_case_for_type();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_graph_insert
ON public.avionics_approved_product_identities;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_graph_insert
AFTER INSERT ON public.avionics_approved_product_identities
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_same_case_for_graph();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_graph_delete
ON public.avionics_approved_product_identities;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_graph_delete
AFTER DELETE ON public.avionics_approved_product_identities
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_same_case_for_graph();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_graph_update
ON public.avionics_approved_product_identities;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_graph_update
AFTER UPDATE OF
  avionics_model_id, avionics_manufacturer_identity_id,
  canonical_product_key, manufacturer_identifier_kind,
  canonical_identifier_key
ON public.avionics_approved_product_identities
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_same_case_for_graph();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_manufacturer_update
ON public.avionics_manufacturers;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_manufacturer_update
AFTER UPDATE OF name, normalized_name ON public.avionics_manufacturers
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_same_case_for_manufacturer();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_origin_revocation
ON public.avionics_authoritative_source_origin_revocations;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_origin_revocation
AFTER INSERT ON public.avionics_authoritative_source_origin_revocations
FOR EACH ROW
EXECUTE FUNCTION
  public.invalidate_listing_avionics_same_case_for_origin_revocation();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_capture_delete
ON public.plugin_submissions;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_capture_delete
AFTER DELETE ON public.plugin_submissions
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_authorization_for_capture();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_capture_update
ON public.plugin_submissions;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_capture_update
AFTER UPDATE OF canonical_listing_id, rendered_html, rendered_html_sha256,
  extracted_listing_json, extraction_error
ON public.plugin_submissions
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_authorization_for_capture();

DO $exact_rerun_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM ONLY public.schema_migration_contracts
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
  ) AND (
    EXISTS (
      SELECT object_kind, object_name, object_definition
      FROM listing_avionics_grounded_capability_preexisting_objects
      EXCEPT
      SELECT object_kind, object_name, object_definition
      FROM listing_avionics_grounded_capability_current_objects
    )
    OR EXISTS (
      SELECT object_kind, object_name, object_definition
      FROM listing_avionics_grounded_capability_current_objects
      EXCEPT
      SELECT object_kind, object_name, object_definition
      FROM listing_avionics_grounded_capability_preexisting_objects
    )
  ) THEN
    RAISE EXCEPTION
      'installed listing avionics grounded-capability objects changed during exact migration rerun';
  END IF;
END
$exact_rerun_guard$;

DROP VIEW listing_avionics_grounded_capability_current_objects;

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260825_listing_avionics_grounded_capabilities',
  1,
  '682ca4e44ced30b0d14da879c31e0fa4b24cc1b6fceb9f213ecc39d9abca0338',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
