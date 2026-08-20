-- Add resumable coordination for trusted-capture listing replay.

BEGIN;

DO $migration_guard$
DECLARE
  marker_is_present BOOLEAN;
  contract_is_exact BOOLEAN;
  check_signature TEXT;
  function_signature TEXT;
BEGIN
  SELECT EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_listing_replay_runs'
  ) INTO marker_is_present;

  IF marker_is_present AND NOT EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_listing_replay_runs'
      AND contract_version = 1
      AND contract_fingerprint =
        'ef344cdb9cf9a7ffcd0ae66e1c9cb3979afa07c1155377cee5dc1031dd0d47c1'
  ) THEN
    RAISE EXCEPTION 'installed listing replay runs migration has a different contract';
  END IF;

  IF marker_is_present THEN
                    WITH expected_columns(
                      relation_name, ordinal_position, column_name, column_type,
                      is_not_null, identity_kind, default_expression
                    ) AS (
                      VALUES
                        ('listing_replay_runs', 1, 'id', 'bigint', TRUE, 'd', ''),
                        ('listing_replay_runs', 2, 'manifest_version', 'bigint', TRUE, '', ''),
                        ('listing_replay_runs', 3, 'manifest_sha256', 'text', TRUE, '', ''),
                        ('listing_replay_runs', 4, 'manifest_capture_count', 'bigint', TRUE, '', ''),
                        ('listing_replay_runs', 5, 'status', 'text', TRUE, '', '''queued''::text'),
                        ('listing_replay_runs', 6, 'active_phase', 'text', FALSE, '', ''),
                        ('listing_replay_runs', 7, 'owner_token', 'text', FALSE, '', ''),
                        ('listing_replay_runs', 8, 'heartbeat_at_epoch_seconds', 'bigint', FALSE, '', ''),
                        ('listing_replay_runs', 9, 'started_at', 'text', FALSE, '', ''),
                        ('listing_replay_runs', 10, 'created_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP'),
                        ('listing_replay_runs', 11, 'updated_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP'),
                        ('listing_replay_runs', 12, 'completed_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 1, 'id', 'bigint', TRUE, 'd', ''),
                        ('listing_replay_run_items', 2, 'run_id', 'bigint', TRUE, '', ''),
                        ('listing_replay_run_items', 3, 'plugin_submission_id', 'bigint', TRUE, '', ''),
                        ('listing_replay_run_items', 4, 'position', 'bigint', TRUE, '', ''),
                        ('listing_replay_run_items', 5, 'expected_rendered_html_sha256', 'text', TRUE, '', ''),
                        ('listing_replay_run_items', 6, 'extracted_listing_sha256', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 7, 'extracted_listing_json', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 8, 'extraction_state', 'text', TRUE, '', '''queued''::text'),
                        ('listing_replay_run_items', 9, 'materialization_state', 'text', TRUE, '', '''blocked''::text'),
                        ('listing_replay_run_items', 10, 'resulting_listing_id', 'bigint', FALSE, '', ''),
                        ('listing_replay_run_items', 11, 'terminal_rejection_phase', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 12, 'terminal_rejection_stage', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 13, 'terminal_rejection_reason_code', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 14, 'last_failure_phase', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 15, 'last_failure_reason_code', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 16, 'extraction_attempt_count', 'bigint', TRUE, '', '0'),
                        ('listing_replay_run_items', 17, 'materialization_attempt_count', 'bigint', TRUE, '', '0'),
                        ('listing_replay_run_items', 18, 'extraction_started_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 19, 'extraction_completed_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 20, 'materialization_started_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 21, 'materialization_completed_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 22, 'created_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP'),
                        ('listing_replay_run_items', 23, 'updated_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP'),
                        ('plugin_submission_materialization_receipts', 1, 'plugin_submission_id', 'bigint', TRUE, '', ''),
                        ('plugin_submission_materialization_receipts', 2, 'aircraft_sale_listing_id', 'bigint', TRUE, '', ''),
                        ('plugin_submission_materialization_receipts', 3, 'rendered_html_sha256', 'text', TRUE, '', ''),
                        ('plugin_submission_materialization_receipts', 4, 'extracted_listing_sha256', 'text', TRUE, '', ''),
                        ('plugin_submission_materialization_receipts', 5, 'completed_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP')
                    ), actual_columns AS (
                      SELECT relation.relname::text AS relation_name,
                        attribute.attnum::integer AS ordinal_position,
                        attribute.attname::text AS column_name,
                        pg_catalog.format_type(attribute.atttypid, attribute.atttypmod)
                          AS column_type,
                        attribute.attnotnull AS is_not_null,
                        attribute.attidentity::text AS identity_kind,
                        COALESCE(pg_catalog.pg_get_expr(
                          default_value.adbin, default_value.adrelid
                        ), '') AS default_expression
                      FROM pg_catalog.pg_attribute attribute
                      JOIN pg_catalog.pg_class relation
                        ON relation.oid = attribute.attrelid
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      LEFT JOIN pg_catalog.pg_attrdef default_value
                        ON default_value.adrelid = attribute.attrelid
                       AND default_value.adnum = attribute.attnum
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND attribute.attnum > 0 AND NOT attribute.attisdropped
                    ), replay_relations AS (
                      SELECT relation.relname::text AS relation_name,
                        relation.oid AS relation_oid,
                        relation.relkind::text AS relation_kind,
                        relation.relpersistence::text AS persistence,
                        relation.relrowsecurity AS row_security,
                        relation.relforcerowsecurity AS force_row_security,
                        relation.relispartition AS is_partition,
                        relation.relhasrules AS has_rules,
                        relation.relhastriggers AS has_triggers,
                        relation.relpartbound IS NOT NULL AS has_partition_bound
                      FROM pg_catalog.pg_class relation
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                    ), replay_indexes AS (
                      SELECT
                        index_relation.relname AS index_name,
                        indexed_relation.oid AS relation_oid,
                        index_definition.indisunique AS is_unique,
                        index_definition.indisprimary AS is_primary,
                        index_definition.indisexclusion AS is_exclusion,
                        index_definition.indimmediate AS is_immediate,
                        index_definition.indisclustered AS is_clustered,
                        index_definition.indisvalid AS is_valid,
                        index_definition.indisready AS is_ready,
                        index_definition.indislive AS is_live,
                        index_definition.indisreplident AS is_replica_identity,
                        index_definition.indnullsnotdistinct AS nulls_not_distinct,
                        index_definition.indpred IS NOT NULL AS is_partial,
                        index_definition.indexprs IS NOT NULL AS has_expressions,
                        index_definition.indnkeyatts AS key_attribute_count,
                        index_definition.indnatts AS total_attribute_count,
                        index_definition.indkey::text AS index_keys,
                        index_definition.indcollation::text AS index_collations,
                        index_definition.indclass::text AS index_operator_classes,
                        index_definition.indoption::text AS index_options,
                        access_method.amname::text AS access_method,
                        lower(pg_catalog.pg_get_expr(
                          index_definition.indpred,
                          index_definition.indrelid
                        )) AS predicate,
                        (
                          SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                          FROM unnest(index_definition.indkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = index_definition.indrelid
                           AND attribute.attnum = key.attnum
                        ) AS columns
                        ,(
                          SELECT array_agg(
                            CASE WHEN collation_key.collation_oid = 0 THEN '0'
                              ELSE collation_namespace.nspname::text || '.' ||
                                collation_definition.collname::text END
                            ORDER BY collation_key.ordinality
                          )
                          FROM unnest(index_definition.indcollation) WITH ORDINALITY
                            AS collation_key(collation_oid, ordinality)
                          LEFT JOIN pg_catalog.pg_collation collation_definition
                            ON collation_definition.oid = collation_key.collation_oid
                          LEFT JOIN pg_catalog.pg_namespace collation_namespace
                            ON collation_namespace.oid = collation_definition.collnamespace
                        ) AS collations,
                        (
                          SELECT array_agg(
                            operator_namespace.nspname::text || '.' ||
                              operator_class.opcname::text
                            ORDER BY operator_key.ordinality
                          )
                          FROM unnest(index_definition.indclass) WITH ORDINALITY
                            AS operator_key(operator_class_oid, ordinality)
                          JOIN pg_catalog.pg_opclass operator_class
                            ON operator_class.oid = operator_key.operator_class_oid
                          JOIN pg_catalog.pg_namespace operator_namespace
                            ON operator_namespace.oid = operator_class.opcnamespace
                        ) AS operator_classes
                      FROM pg_catalog.pg_index index_definition
                      JOIN pg_catalog.pg_class index_relation
                        ON index_relation.oid = index_definition.indexrelid
                      JOIN pg_catalog.pg_am access_method
                        ON access_method.oid = index_relation.relam
                      JOIN pg_catalog.pg_namespace index_namespace
                        ON index_namespace.oid = index_relation.relnamespace
                      JOIN pg_catalog.pg_class indexed_relation
                        ON indexed_relation.oid = index_definition.indrelid
                      WHERE index_namespace.nspname = 'public'
                        AND index_relation.relname IN (
                          'idx_listing_replay_runs_one_running',
                          'idx_listing_replay_run_items_phase',
                          'uq_aircraft_sale_listings_owner_source'
                        )
                    ), replay_attached_indexes AS (
                      SELECT index_definition.indexrelid
                      FROM pg_catalog.pg_index index_definition
                      WHERE index_definition.indrelid IN (
                        pg_catalog.to_regclass('public.listing_replay_runs'),
                        pg_catalog.to_regclass('public.listing_replay_run_items'),
                        pg_catalog.to_regclass(
                          'public.plugin_submission_materialization_receipts'
                        )
                      )
                    ), replay_unique_constraints AS (
                      SELECT relation.relname::text AS relation_name, (
                        SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                        FROM unnest(constraint_definition.conkey) WITH ORDINALITY
                          AS key(attnum, ordinality)
                        JOIN pg_catalog.pg_attribute attribute
                          ON attribute.attrelid = constraint_definition.conrelid
                         AND attribute.attnum = key.attnum
                      ) AS columns,
                        constraint_definition.convalidated AS is_validated,
                        constraint_definition.condeferrable AS is_deferrable,
                        constraint_definition.condeferred AS is_initially_deferred,
                        backing_index.indisunique AS backing_is_unique,
                        backing_index.indisprimary AS backing_is_primary,
                        backing_index.indisexclusion AS backing_is_exclusion,
                        backing_index.indimmediate AS backing_is_immediate,
                        backing_index.indisclustered AS backing_is_clustered,
                        backing_index.indisvalid AS backing_is_valid,
                        backing_index.indisready AS backing_is_ready,
                        backing_index.indislive AS backing_is_live,
                        backing_index.indisreplident AS backing_is_replica_identity,
                        backing_index.indnullsnotdistinct AS backing_nulls_not_distinct,
                        backing_index.indpred IS NOT NULL AS backing_is_partial,
                        backing_index.indexprs IS NOT NULL AS backing_has_expressions,
                        backing_index.indnkeyatts AS backing_key_attribute_count,
                        backing_index.indnatts AS backing_total_attribute_count,
                        backing_index.indkey::text AS backing_index_keys,
                        backing_index.indcollation::text AS backing_index_collations,
                        backing_index.indclass::text AS backing_index_operator_classes,
                        backing_index.indoption::text AS backing_index_options,
                        backing_access_method.amname::text AS backing_access_method,
                        (
                          SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                          FROM unnest(backing_index.indkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = constraint_definition.conrelid
                           AND attribute.attnum = key.attnum
                        ) AS backing_columns,
                        (
                          SELECT array_agg(
                            CASE WHEN collation_key.collation_oid = 0 THEN '0'
                              ELSE collation_namespace.nspname::text || '.' ||
                                collation_definition.collname::text END
                            ORDER BY collation_key.ordinality
                          )
                          FROM unnest(backing_index.indcollation) WITH ORDINALITY
                            AS collation_key(collation_oid, ordinality)
                          LEFT JOIN pg_catalog.pg_collation collation_definition
                            ON collation_definition.oid = collation_key.collation_oid
                          LEFT JOIN pg_catalog.pg_namespace collation_namespace
                            ON collation_namespace.oid = collation_definition.collnamespace
                        ) AS backing_collations,
                        (
                          SELECT array_agg(
                            operator_namespace.nspname::text || '.' ||
                              operator_class.opcname::text
                            ORDER BY operator_key.ordinality
                          )
                          FROM unnest(backing_index.indclass) WITH ORDINALITY
                            AS operator_key(operator_class_oid, ordinality)
                          JOIN pg_catalog.pg_opclass operator_class
                            ON operator_class.oid = operator_key.operator_class_oid
                          JOIN pg_catalog.pg_namespace operator_namespace
                            ON operator_namespace.oid = operator_class.opcnamespace
                        ) AS backing_operator_classes
                      FROM pg_catalog.pg_constraint constraint_definition
                      JOIN pg_catalog.pg_class relation
                        ON relation.oid = constraint_definition.conrelid
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      JOIN pg_catalog.pg_index backing_index
                        ON backing_index.indexrelid = constraint_definition.conindid
                      JOIN pg_catalog.pg_class backing_index_relation
                        ON backing_index_relation.oid = backing_index.indexrelid
                      JOIN pg_catalog.pg_am backing_access_method
                        ON backing_access_method.oid = backing_index_relation.relam
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND constraint_definition.contype = 'u'
                    ), replay_primary_keys AS (
                      SELECT relation.relname::text AS relation_name, (
                        SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                        FROM unnest(constraint_definition.conkey) WITH ORDINALITY
                          AS key(attnum, ordinality)
                        JOIN pg_catalog.pg_attribute attribute
                          ON attribute.attrelid = constraint_definition.conrelid
                         AND attribute.attnum = key.attnum
                      ) AS columns,
                        constraint_definition.convalidated AS is_validated,
                        constraint_definition.condeferrable AS is_deferrable,
                        constraint_definition.condeferred AS is_initially_deferred,
                        backing_index.indisunique AS backing_is_unique,
                        backing_index.indisprimary AS backing_is_primary,
                        backing_index.indisexclusion AS backing_is_exclusion,
                        backing_index.indimmediate AS backing_is_immediate,
                        backing_index.indisclustered AS backing_is_clustered,
                        backing_index.indisvalid AS backing_is_valid,
                        backing_index.indisready AS backing_is_ready,
                        backing_index.indislive AS backing_is_live,
                        backing_index.indisreplident AS backing_is_replica_identity,
                        backing_index.indnullsnotdistinct AS backing_nulls_not_distinct,
                        backing_index.indpred IS NOT NULL AS backing_is_partial,
                        backing_index.indexprs IS NOT NULL AS backing_has_expressions,
                        backing_index.indnkeyatts AS backing_key_attribute_count,
                        backing_index.indnatts AS backing_total_attribute_count,
                        backing_index.indkey::text AS backing_index_keys,
                        backing_index.indcollation::text AS backing_index_collations,
                        backing_index.indclass::text AS backing_index_operator_classes,
                        backing_index.indoption::text AS backing_index_options,
                        backing_access_method.amname::text AS backing_access_method,
                        (
                          SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                          FROM unnest(backing_index.indkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = constraint_definition.conrelid
                           AND attribute.attnum = key.attnum
                        ) AS backing_columns,
                        (
                          SELECT array_agg(
                            CASE WHEN collation_key.collation_oid = 0 THEN '0'
                              ELSE collation_namespace.nspname::text || '.' ||
                                collation_definition.collname::text END
                            ORDER BY collation_key.ordinality
                          )
                          FROM unnest(backing_index.indcollation) WITH ORDINALITY
                            AS collation_key(collation_oid, ordinality)
                          LEFT JOIN pg_catalog.pg_collation collation_definition
                            ON collation_definition.oid = collation_key.collation_oid
                          LEFT JOIN pg_catalog.pg_namespace collation_namespace
                            ON collation_namespace.oid = collation_definition.collnamespace
                        ) AS backing_collations,
                        (
                          SELECT array_agg(
                            operator_namespace.nspname::text || '.' ||
                              operator_class.opcname::text
                            ORDER BY operator_key.ordinality
                          )
                          FROM unnest(backing_index.indclass) WITH ORDINALITY
                            AS operator_key(operator_class_oid, ordinality)
                          JOIN pg_catalog.pg_opclass operator_class
                            ON operator_class.oid = operator_key.operator_class_oid
                          JOIN pg_catalog.pg_namespace operator_namespace
                            ON operator_namespace.oid = operator_class.opcnamespace
                        ) AS backing_operator_classes
                      FROM pg_catalog.pg_constraint constraint_definition
                      JOIN pg_catalog.pg_class relation
                        ON relation.oid = constraint_definition.conrelid
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      JOIN pg_catalog.pg_index backing_index
                        ON backing_index.indexrelid = constraint_definition.conindid
                      JOIN pg_catalog.pg_class backing_index_relation
                        ON backing_index_relation.oid = backing_index.indexrelid
                      JOIN pg_catalog.pg_am backing_access_method
                        ON backing_access_method.oid = backing_index_relation.relam
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND constraint_definition.contype = 'p'
                    ), replay_foreign_keys AS (
                      SELECT child_namespace.nspname::text AS child_namespace,
                        child.relname::text AS child_relation,
                        child.oid AS child_oid,
                        parent_namespace.nspname::text AS parent_namespace,
                        parent.relname::text AS parent_relation,
                        parent.oid AS parent_oid,
                        (
                          SELECT string_agg(attribute.attname::text, ',' ORDER BY key.ordinality)
                          FROM unnest(constraint_definition.conkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = constraint_definition.conrelid
                           AND attribute.attnum = key.attnum
                        ) AS child_columns,
                        (
                          SELECT string_agg(attribute.attname::text, ',' ORDER BY key.ordinality)
                          FROM unnest(constraint_definition.confkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = constraint_definition.confrelid
                           AND attribute.attnum = key.attnum
                        ) AS parent_columns,
                        constraint_definition.convalidated AS is_validated,
                        constraint_definition.condeferrable AS is_deferrable,
                        constraint_definition.condeferred AS is_initially_deferred,
                        constraint_definition.confmatchtype::text AS match_type,
                        constraint_definition.confupdtype::text AS update_action,
                        constraint_definition.confdeltype::text AS delete_action
                      FROM pg_catalog.pg_constraint constraint_definition
                      JOIN pg_catalog.pg_class child
                        ON child.oid = constraint_definition.conrelid
                      JOIN pg_catalog.pg_namespace child_namespace
                        ON child_namespace.oid = child.relnamespace
                      JOIN pg_catalog.pg_class parent
                        ON parent.oid = constraint_definition.confrelid
                      JOIN pg_catalog.pg_namespace parent_namespace
                        ON parent_namespace.oid = parent.relnamespace
                      WHERE child_namespace.nspname = 'public'
                        AND child.relname IN (
                          'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND constraint_definition.contype = 'f'
                    ), required_check_fragments(relation_name, fragment) AS (
                      VALUES
                        ('listing_replay_runs', 'manifest_version > 0'),
                        ('listing_replay_runs', '^[0-9a-f]{64}$'),
                        ('listing_replay_runs', 'manifest_capture_count > 0'),
                        ('listing_replay_runs', 'status = ANY'),
                        ('listing_replay_runs', 'active_phase = ANY'),
                        ('listing_replay_runs', 'length(btrim(owner_token))'),
                        ('listing_replay_runs', 'heartbeat_at_epoch_seconds IS NOT NULL'),
                        ('listing_replay_run_items', '"position" >= 0'),
                        ('listing_replay_run_items', 'expected_rendered_html_sha256'),
                        ('listing_replay_run_items', 'extracted_listing_sha256'),
                        ('listing_replay_run_items', 'extracted_listing_json IS NOT NULL'),
                        ('listing_replay_run_items', 'extraction_state = ANY'),
                        ('listing_replay_run_items', 'materialization_state = ANY'),
                        ('listing_replay_run_items', 'terminal_rejection_phase = ANY'),
                        ('listing_replay_run_items', 'faa_aircraft_admission'),
                        ('listing_replay_run_items', 'capture_authentication_failed'),
                        ('listing_replay_run_items', 'last_failure_phase = ANY'),
                        ('listing_replay_run_items', 'faa_lookup_failed'),
                        ('listing_replay_run_items', 'extraction_attempt_count >= 0'),
                        ('listing_replay_run_items', 'materialization_attempt_count >= 0'),
                        ('listing_replay_run_items', 'terminal_rejection_phase IS NULL'),
                        ('listing_replay_run_items', 'last_failure_phase IS NULL'),
                        ('listing_replay_run_items', 'resulting_listing_id IS NOT NULL'),
                        ('listing_replay_run_items', 'extracted_listing_sha256 IS NOT NULL'),
                        ('listing_replay_run_items', 'extraction_state = ''succeeded'''),
                        ('listing_replay_run_items', 'extraction_started_at IS NOT NULL'),
                        ('listing_replay_run_items', 'materialization_started_at IS NOT NULL')
                        ,('plugin_submission_materialization_receipts', 'rendered_html_sha256')
                        ,('plugin_submission_materialization_receipts', 'extracted_listing_sha256')
                    ), replay_checks AS (
                      SELECT relation.relname::text AS relation_name,
                        pg_catalog.pg_get_constraintdef(constraint_definition.oid)
                          AS definition
                      FROM pg_catalog.pg_constraint constraint_definition
                      JOIN pg_catalog.pg_class relation
                        ON relation.oid = constraint_definition.conrelid
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND constraint_definition.contype = 'c'
                    )
                    SELECT
                      (SELECT COUNT(*) = 40 FROM actual_columns)
                      AND NOT EXISTS (
                        SELECT 1 FROM expected_columns expected
                        WHERE NOT EXISTS (
                          SELECT 1 FROM actual_columns actual
                          WHERE actual.relation_name = expected.relation_name
                            AND actual.ordinal_position = expected.ordinal_position
                            AND actual.column_name = expected.column_name
                            AND actual.column_type = expected.column_type
                            AND actual.is_not_null = expected.is_not_null
                            AND actual.identity_kind = expected.identity_kind
                            AND actual.default_expression = expected.default_expression
                        )
                      )
                      AND (SELECT COUNT(*) = 3 FROM replay_relations)
                      AND NOT EXISTS (
                        SELECT 1 FROM replay_relations
                        WHERE relation_oid IS DISTINCT FROM pg_catalog.to_regclass(
                          'public.' || relation_name
                        )
                          OR relation_kind <> 'r'
                          OR persistence <> 'p'
                          OR row_security OR force_row_security OR is_partition
                          OR has_rules OR NOT has_triggers OR has_partition_bound
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_trigger trigger_definition
                        WHERE trigger_definition.tgrelid IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        ) AND NOT trigger_definition.tgisinternal
                          AND trigger_definition.tgname NOT IN (
                            'listing_replay_run_items_checkpoint_exact',
                            'listing_replay_run_items_completed_immutable',
                            'plugin_submission_materialization_receipts_immutable'
                          )
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_trigger trigger_definition
                        WHERE NOT trigger_definition.tgisinternal
                          AND trigger_definition.tgrelid IN (
                            pg_catalog.to_regclass('public.plugin_submissions'),
                            pg_catalog.to_regclass('public.plugin_installs')
                          )
                          AND NOT (
                            (
                              trigger_definition.tgrelid = pg_catalog.to_regclass(
                                'public.plugin_submissions'
                              )
                              AND trigger_definition.tgname IN (
                                'plugin_submissions_replay_checkpoint_immutable',
                                'listing_avionics_authorizations_invalidate_capture_delete',
                                'listing_avionics_authorizations_invalidate_capture_update'
                              )
                            )
                            OR (
                              trigger_definition.tgrelid = pg_catalog.to_regclass(
                                'public.plugin_installs'
                              )
                              AND trigger_definition.tgname =
                                'plugin_installs_replay_identity_immutable'
                            )
                          )
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_policy policy_definition
                        WHERE policy_definition.polrelid IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        )
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_rewrite rule_definition
                        WHERE rule_definition.ev_class IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        )
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_inherits inheritance
                        WHERE inheritance.inhrelid IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        ) OR inheritance.inhparent IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        )
                      )
                      AND (SELECT COUNT(*) = 9 FROM replay_attached_indexes)
                      AND (SELECT COUNT(*) = 3 FROM pg_catalog.pg_trigger trigger_definition
                        WHERE trigger_definition.tgrelid IN (
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        ) AND NOT trigger_definition.tgisinternal)
                      AND (SELECT COUNT(*) = 1 FROM replay_indexes
                       WHERE index_name = 'idx_listing_replay_runs_one_running'
                         AND relation_oid = pg_catalog.to_regclass(
                           'public.listing_replay_runs'
                         )
                         AND is_unique AND NOT is_primary AND NOT is_exclusion
                         AND is_immediate AND NOT is_clustered
                         AND is_valid AND is_ready AND is_live
                         AND NOT is_replica_identity AND NOT nulls_not_distinct
                         AND is_partial
                         AND NOT has_expressions
                         AND key_attribute_count = 1 AND total_attribute_count = 1
                         AND access_method = 'btree'
                         AND index_options = '0'
                         AND columns = ARRAY['status']::text[]
                         AND collations = ARRAY['pg_catalog.default']::text[]
                         AND operator_classes = ARRAY['pg_catalog.text_ops']::text[]
                         AND pg_catalog.translate(
                           predicate, E' \n\r\t()', ''
                         ) IN ('status=''running''', 'status=''running''::text'))
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_indexes
                       WHERE index_name = 'idx_listing_replay_run_items_phase'
                         AND relation_oid = pg_catalog.to_regclass(
                           'public.listing_replay_run_items'
                         )
                         AND NOT is_unique AND NOT is_primary AND NOT is_exclusion
                         AND is_immediate AND NOT is_clustered
                         AND is_valid AND is_ready AND is_live
                         AND NOT is_replica_identity AND NOT nulls_not_distinct
                         AND NOT is_partial
                         AND NOT has_expressions
                         AND key_attribute_count = 4 AND total_attribute_count = 4
                         AND access_method = 'btree'
                         AND index_options = '0 0 0 0'
                         AND columns = ARRAY[
                           'run_id', 'extraction_state',
                           'materialization_state', 'position'
                         ]::text[]
                         AND collations = ARRAY[
                           '0', 'pg_catalog.default', 'pg_catalog.default', '0'
                         ]::text[]
                         AND operator_classes = ARRAY[
                           'pg_catalog.int8_ops', 'pg_catalog.text_ops',
                           'pg_catalog.text_ops', 'pg_catalog.int8_ops'
                         ]::text[])
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_indexes
                       WHERE index_name = 'uq_aircraft_sale_listings_owner_source'
                         AND relation_oid = pg_catalog.to_regclass(
                           'public.aircraft_sale_listings'
                         )
                         AND is_unique AND NOT is_primary AND NOT is_exclusion
                         AND is_immediate AND NOT is_clustered
                         AND is_valid AND is_ready AND is_live
                         AND NOT is_replica_identity AND NOT nulls_not_distinct
                         AND is_partial AND NOT has_expressions
                         AND key_attribute_count = 2 AND total_attribute_count = 2
                         AND access_method = 'btree' AND index_options = '0 0'
                         AND columns = ARRAY['created_by_user_id', 'source_url']::text[]
                         AND collations = ARRAY['0', 'pg_catalog.default']::text[]
                         AND operator_classes = ARRAY[
                           'pg_catalog.int8_ops', 'pg_catalog.text_ops'
                         ]::text[]
                         AND pg_catalog.translate(predicate, E' \n\r\t()', '') IN (
                           'source_urlisnotnullandlengthbtrimsource_url>0',
                           'source_urlisnotnullandlengthbtrimsource_url>0::integer'
                         ))
                      AND (SELECT COUNT(*) = 5
                        FROM pg_catalog.pg_trigger replay_trigger
                        JOIN pg_catalog.pg_proc routine
                          ON routine.oid = replay_trigger.tgfoid
                        JOIN pg_catalog.pg_namespace routine_namespace
                          ON routine_namespace.oid = routine.pronamespace
                        WHERE NOT replay_trigger.tgisinternal
                          AND replay_trigger.tgenabled = 'O'
                          AND replay_trigger.tgqual IS NULL
                          AND replay_trigger.tgnargs = 0
                          AND routine_namespace.nspname = 'public'
                          AND routine.proconfig = ARRAY['search_path=pg_catalog']::text[]
                          AND NOT routine.prosecdef AND NOT routine.proisstrict
                          AND NOT routine.proleakproof
                          AND routine.provolatile = 'v'
                          AND routine.proparallel = 'u'
                          AND routine.prokind = 'f'
                          AND routine.pronargs = 0
                          AND routine.prorettype = pg_catalog.to_regtype(
                            'pg_catalog.trigger'
                          )
                          AND routine.prolang = (
                            SELECT language.oid FROM pg_catalog.pg_language language
                            WHERE language.lanname = 'plpgsql'
                          )
                          AND (
                            (replay_trigger.tgname =
                               'listing_replay_run_items_checkpoint_exact'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.listing_replay_run_items'
                             ) AND replay_trigger.tgtype = 23
                             AND routine.proname =
                               'enforce_replay_extraction_checkpoint_exactness')
                            OR
                            (replay_trigger.tgname =
                               'listing_replay_run_items_completed_immutable'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.listing_replay_run_items'
                             ) AND replay_trigger.tgtype = 27
                             AND routine.proname = 'preserve_completed_replay_item')
                            OR
                            (replay_trigger.tgname =
                               'plugin_submission_materialization_receipts_immutable'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.plugin_submission_materialization_receipts'
                             ) AND replay_trigger.tgtype = 27
                             AND routine.proname =
                               'preserve_replay_materialization_receipt')
                            OR
                            (replay_trigger.tgname =
                               'plugin_submissions_replay_checkpoint_immutable'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.plugin_submissions'
                             ) AND replay_trigger.tgtype = 19
                             AND routine.proname =
                               'enforce_replay_checkpoint_capture_immutability')
                            OR
                            (replay_trigger.tgname =
                               'plugin_installs_replay_identity_immutable'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.plugin_installs'
                             ) AND replay_trigger.tgtype = 19
                             AND routine.proname =
                               'enforce_replay_plugin_identity_immutability')
                          ))
                      AND
                      (SELECT COUNT(*) = 4 FROM replay_unique_constraints)
                      AND NOT EXISTS (
                        SELECT 1 FROM replay_unique_constraints
                        WHERE NOT is_validated OR is_deferrable OR is_initially_deferred
                          OR NOT backing_is_unique OR backing_is_primary
                          OR backing_is_exclusion OR NOT backing_is_immediate
                          OR backing_is_clustered
                          OR NOT backing_is_valid OR NOT backing_is_ready
                          OR NOT backing_is_live OR backing_is_replica_identity
                          OR backing_nulls_not_distinct
                          OR backing_is_partial OR backing_has_expressions
                          OR backing_access_method <> 'btree'
                          OR backing_key_attribute_count <> cardinality(columns)
                          OR backing_total_attribute_count <> cardinality(columns)
                          OR backing_columns <> columns
                          OR backing_index_options <>
                            array_to_string(array_fill(
                              '0'::text, ARRAY[cardinality(columns)]
                            ), ' ')
                          OR backing_collations <> CASE
                            WHEN columns = ARRAY['manifest_sha256']::text[]
                              THEN ARRAY['pg_catalog.default']::text[]
                            ELSE array_fill(
                              '0'::text, ARRAY[cardinality(columns)]
                            ) END
                          OR backing_operator_classes <> CASE
                            WHEN columns = ARRAY['manifest_sha256']::text[]
                              THEN ARRAY['pg_catalog.text_ops']::text[]
                            ELSE array_fill(
                              'pg_catalog.int8_ops'::text,
                              ARRAY[cardinality(columns)]
                            ) END
                      )
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_unique_constraints
                       WHERE relation_name = 'listing_replay_runs'
                         AND columns = ARRAY['manifest_sha256']::text[])
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_unique_constraints
                       WHERE relation_name = 'listing_replay_run_items'
                         AND columns = ARRAY['run_id', 'position']::text[])
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_unique_constraints
                       WHERE relation_name = 'listing_replay_run_items'
                         AND columns = ARRAY[
                         'run_id', 'plugin_submission_id'
                       ]::text[])
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_unique_constraints
                       WHERE relation_name = 'plugin_submission_materialization_receipts'
                         AND columns = ARRAY['aircraft_sale_listing_id']::text[])
                      AND (SELECT COUNT(*) = 3 FROM replay_primary_keys)
                      AND NOT EXISTS (
                        SELECT 1 FROM replay_primary_keys
                        WHERE NOT is_validated OR is_deferrable OR is_initially_deferred
                          OR NOT backing_is_unique OR NOT backing_is_primary
                          OR backing_is_exclusion OR NOT backing_is_immediate
                          OR backing_is_clustered
                          OR NOT backing_is_valid OR NOT backing_is_ready
                          OR NOT backing_is_live OR backing_is_replica_identity
                          OR backing_nulls_not_distinct
                          OR backing_is_partial OR backing_has_expressions
                          OR backing_access_method <> 'btree'
                          OR backing_key_attribute_count <> cardinality(columns)
                          OR backing_total_attribute_count <> cardinality(columns)
                          OR backing_columns <> columns
                          OR backing_index_options <>
                            array_to_string(array_fill(
                              '0'::text, ARRAY[cardinality(columns)]
                            ), ' ')
                          OR backing_collations <>
                            array_fill('0'::text, ARRAY[cardinality(columns)])
                          OR backing_operator_classes <>
                            array_fill(
                              'pg_catalog.int8_ops'::text,
                              ARRAY[cardinality(columns)]
                            )
                      )
                      AND (SELECT COUNT(*) = 1 FROM replay_primary_keys
                           WHERE relation_name = 'listing_replay_runs'
                             AND columns = ARRAY['id']::text[])
                      AND (SELECT COUNT(*) = 1 FROM replay_primary_keys
                           WHERE relation_name = 'listing_replay_run_items'
                             AND columns = ARRAY['id']::text[])
                      AND (SELECT COUNT(*) = 1 FROM replay_primary_keys
                           WHERE relation_name = 'plugin_submission_materialization_receipts'
                             AND columns = ARRAY['plugin_submission_id']::text[])
                      AND (SELECT COUNT(*) = 5 FROM replay_foreign_keys)
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'listing_replay_run_items'
                          AND child_oid = pg_catalog.to_regclass('public.listing_replay_run_items')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'listing_replay_runs'
                          AND parent_oid = pg_catalog.to_regclass('public.listing_replay_runs')
                          AND child_columns = 'run_id' AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'c')
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'listing_replay_run_items'
                          AND child_oid = pg_catalog.to_regclass('public.listing_replay_run_items')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'plugin_submissions'
                          AND parent_oid = pg_catalog.to_regclass('public.plugin_submissions')
                          AND child_columns = 'plugin_submission_id'
                          AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'r')
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'listing_replay_run_items'
                          AND child_oid = pg_catalog.to_regclass('public.listing_replay_run_items')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'aircraft_sale_listings'
                          AND parent_oid = pg_catalog.to_regclass('public.aircraft_sale_listings')
                          AND child_columns = 'resulting_listing_id'
                          AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'r')
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'plugin_submission_materialization_receipts'
                          AND child_oid = pg_catalog.to_regclass('public.plugin_submission_materialization_receipts')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'plugin_submissions'
                          AND parent_oid = pg_catalog.to_regclass('public.plugin_submissions')
                          AND child_columns = 'plugin_submission_id' AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'c')
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'plugin_submission_materialization_receipts'
                          AND child_oid = pg_catalog.to_regclass('public.plugin_submission_materialization_receipts')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'aircraft_sale_listings'
                          AND parent_oid = pg_catalog.to_regclass('public.aircraft_sale_listings')
                          AND child_columns = 'aircraft_sale_listing_id' AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'r')
                      AND (SELECT COUNT(*) = 7 FROM replay_checks
                           WHERE relation_name = 'listing_replay_runs')
                      AND (SELECT COUNT(*) = 20 FROM replay_checks
                           WHERE relation_name = 'listing_replay_run_items')
                      AND (SELECT COUNT(*) = 2 FROM replay_checks
                           WHERE relation_name = 'plugin_submission_materialization_receipts')
                      AND NOT EXISTS (
                        SELECT 1 FROM required_check_fragments required
                        WHERE NOT EXISTS (
                          SELECT 1 FROM replay_checks actual
                          WHERE actual.relation_name = required.relation_name
                            AND position(required.fragment IN actual.definition) > 0
                        )
                      )
                      INTO contract_is_exact;
    SELECT pg_catalog.md5(pg_catalog.string_agg(
      relation.relname::text || '|' || constraint_definition.conname::text || '|' ||
      pg_catalog.pg_get_constraintdef(constraint_definition.oid) || E'\n',
      '' ORDER BY relation.relname, constraint_definition.conname
    ))
    INTO check_signature
    FROM pg_catalog.pg_constraint constraint_definition
    JOIN pg_catalog.pg_class relation
      ON relation.oid = constraint_definition.conrelid
    JOIN pg_catalog.pg_namespace namespace
      ON namespace.oid = relation.relnamespace
    WHERE namespace.nspname = 'public'
      AND relation.relname IN (
        'listing_replay_runs', 'listing_replay_run_items',
        'plugin_submission_materialization_receipts'
      )
      AND constraint_definition.contype = 'c';

    SELECT pg_catalog.md5(pg_catalog.string_agg(
      routine.proname::text || '|' || routine.prosrc || E'\n',
      '' ORDER BY routine.proname
    ))
    INTO function_signature
    FROM pg_catalog.pg_proc routine
    JOIN pg_catalog.pg_namespace namespace
      ON namespace.oid = routine.pronamespace
    WHERE namespace.nspname = 'public'
      AND routine.pronargs = 0
      AND routine.proname IN (
        'enforce_replay_extraction_checkpoint_exactness',
        'preserve_completed_replay_item',
        'preserve_replay_materialization_receipt',
        'enforce_replay_checkpoint_capture_immutability',
        'enforce_replay_plugin_identity_immutability'
      );

    IF NOT COALESCE(contract_is_exact, FALSE)
       OR check_signature IS DISTINCT FROM 'b33af6b1c9969a333dbdb7d8a5910e92'
       OR function_signature IS DISTINCT FROM '7e885abd1d361c7c831c84e5e3a58e1d' THEN
      RAISE EXCEPTION
        'installed listing replay migration contract has noncanonical objects';
    END IF;
  ELSIF (
    pg_catalog.to_regclass('public.listing_replay_runs') IS NOT NULL
    OR pg_catalog.to_regclass('public.listing_replay_run_items') IS NOT NULL
    OR pg_catalog.to_regclass('public.plugin_submission_materialization_receipts') IS NOT NULL
    OR pg_catalog.to_regclass('public.idx_listing_replay_runs_one_running') IS NOT NULL
    OR pg_catalog.to_regclass('public.idx_listing_replay_run_items_phase') IS NOT NULL
    OR pg_catalog.to_regclass('public.uq_aircraft_sale_listings_owner_source') IS NOT NULL
    OR pg_catalog.to_regprocedure(
      'public.enforce_replay_extraction_checkpoint_exactness()'
    ) IS NOT NULL
    OR pg_catalog.to_regprocedure('public.preserve_completed_replay_item()') IS NOT NULL
    OR pg_catalog.to_regprocedure(
      'public.preserve_replay_materialization_receipt()'
    ) IS NOT NULL
    OR pg_catalog.to_regprocedure(
      'public.enforce_replay_checkpoint_capture_immutability()'
    ) IS NOT NULL
    OR pg_catalog.to_regprocedure(
      'public.enforce_replay_plugin_identity_immutability()'
    ) IS NOT NULL
    OR EXISTS (
      SELECT 1 FROM pg_catalog.pg_trigger
      WHERE NOT tgisinternal AND tgname IN (
        'listing_replay_run_items_checkpoint_exact',
        'listing_replay_run_items_completed_immutable',
        'plugin_submission_materialization_receipts_immutable',
        'plugin_submissions_replay_checkpoint_immutable',
        'plugin_installs_replay_identity_immutable'
      )
    )
  ) THEN
    RAISE EXCEPTION
      'listing replay objects exist without the exact migration contract';
  END IF;
END
$migration_guard$;

CREATE TABLE IF NOT EXISTS public.listing_replay_runs (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  manifest_version BIGINT NOT NULL CHECK (manifest_version > 0),
  manifest_sha256 TEXT NOT NULL UNIQUE CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
  manifest_capture_count BIGINT NOT NULL CHECK (manifest_capture_count > 0),
  status TEXT NOT NULL DEFAULT 'queued'
    CHECK (status IN ('queued', 'running', 'completed')),
  active_phase TEXT CHECK (active_phase IN ('extraction', 'materialization')),
  owner_token TEXT,
  heartbeat_at_epoch_seconds BIGINT,
  started_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (owner_token IS NULL OR length(BTRIM(owner_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = 'running' AND active_phase IS NOT NULL AND owner_token IS NOT NULL
      AND heartbeat_at_epoch_seconds IS NOT NULL AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status = 'queued' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NULL)
    OR
    (status = 'completed' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NOT NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_listing_replay_runs_one_running
  ON public.listing_replay_runs (status) WHERE status = 'running';

CREATE TABLE IF NOT EXISTS public.listing_replay_run_items (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  run_id BIGINT NOT NULL REFERENCES public.listing_replay_runs(id) ON DELETE CASCADE,
  plugin_submission_id BIGINT NOT NULL
    REFERENCES public.plugin_submissions(id) ON DELETE RESTRICT,
  position BIGINT NOT NULL CHECK (position >= 0),
  expected_rendered_html_sha256 TEXT NOT NULL
    CHECK (expected_rendered_html_sha256 ~ '^[0-9a-f]{64}$'),
  extracted_listing_sha256 TEXT
    CHECK (extracted_listing_sha256 IS NULL OR extracted_listing_sha256 ~ '^[0-9a-f]{64}$'),
  extracted_listing_json TEXT,
  extraction_state TEXT NOT NULL DEFAULT 'queued'
    CHECK (extraction_state IN ('queued', 'running', 'succeeded', 'rejected', 'failed')),
  materialization_state TEXT NOT NULL DEFAULT 'blocked'
    CHECK (materialization_state IN ('blocked', 'queued', 'running', 'succeeded', 'rejected', 'failed')),
  resulting_listing_id BIGINT
    REFERENCES public.aircraft_sale_listings(id) ON DELETE RESTRICT,
  terminal_rejection_phase TEXT
    CHECK (terminal_rejection_phase IN ('extraction', 'materialization')),
  terminal_rejection_stage TEXT CHECK (terminal_rejection_stage IN (
    'capture_admission', 'faa_aircraft_admission'
  )),
  terminal_rejection_reason_code TEXT CHECK (terminal_rejection_reason_code IN (
    'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed',
    'missing_registration', 'non_n_registration',
    'invalid_n_number', 'serial_conflict'
  )),
  last_failure_phase TEXT CHECK (last_failure_phase IN ('extraction', 'materialization')),
  last_failure_reason_code TEXT
    CHECK (last_failure_reason_code IN (
      'database_error', 'operation_failed', 'faa_lookup_failed', 'faa_listing_not_found',
      'faa_registry_snapshot_unavailable', 'faa_registration_not_found',
      'faa_registration_not_covered', 'faa_ambiguous_registration',
      'faa_registry_aircraft_identity_unavailable', 'faa_aircraft_manufacturer_mismatch',
      'faa_aircraft_model_mismatch', 'faa_canonical_identity_assignment_missing',
      'faa_canonical_identity_assignment_mismatch'
    )),
  extraction_attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (extraction_attempt_count >= 0),
  materialization_attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (materialization_attempt_count >= 0),
  extraction_started_at TEXT,
  extraction_completed_at TEXT,
  materialization_started_at TEXT,
  materialization_completed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (run_id, position),
  UNIQUE (run_id, plugin_submission_id),
  CHECK (
    (extraction_state = 'rejected' AND materialization_state = 'blocked'
      AND terminal_rejection_phase = 'extraction'
      AND terminal_rejection_stage = 'capture_admission'
      AND terminal_rejection_reason_code IN (
        'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed'
      ))
    OR
    (extraction_state = 'succeeded' AND materialization_state = 'rejected'
      AND terminal_rejection_phase = 'materialization'
      AND (
        (terminal_rejection_stage = 'capture_admission'
          AND terminal_rejection_reason_code IN (
            'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed'
          ))
        OR
        (terminal_rejection_stage = 'faa_aircraft_admission'
          AND terminal_rejection_reason_code IN (
            'missing_registration', 'non_n_registration', 'invalid_n_number', 'serial_conflict'
          ))
      ))
    OR
    (extraction_state <> 'rejected' AND materialization_state <> 'rejected'
      AND terminal_rejection_phase IS NULL AND terminal_rejection_stage IS NULL
      AND terminal_rejection_reason_code IS NULL)
  ),
  CHECK (
    (extraction_state = 'failed' AND materialization_state = 'blocked'
      AND last_failure_phase = 'extraction'
      AND last_failure_reason_code IN ('database_error', 'operation_failed'))
    OR
    (extraction_state = 'succeeded' AND materialization_state = 'failed'
      AND last_failure_phase = 'materialization' AND last_failure_reason_code IS NOT NULL)
    OR
    (extraction_state <> 'failed' AND materialization_state <> 'failed'
      AND last_failure_phase IS NULL AND last_failure_reason_code IS NULL)
  ),
  CHECK ((materialization_state = 'succeeded') = (resulting_listing_id IS NOT NULL)),
  CHECK ((extraction_state = 'succeeded') = (extracted_listing_sha256 IS NOT NULL)),
  CHECK ((extraction_state = 'succeeded') = (extracted_listing_json IS NOT NULL)),
  CHECK (extraction_state = 'succeeded' OR materialization_state = 'blocked'),
  CHECK (extraction_state <> 'running' OR extraction_started_at IS NOT NULL),
  CHECK (materialization_state <> 'running' OR materialization_started_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_listing_replay_run_items_phase
  ON public.listing_replay_run_items
    (run_id, extraction_state, materialization_state, position);

CREATE OR REPLACE FUNCTION public.enforce_replay_extraction_checkpoint_exactness()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF NEW.extraction_state = 'succeeded' AND NOT EXISTS (
    SELECT 1
    FROM public.plugin_submissions submission
    JOIN public.plugin_installs install ON install.id = submission.plugin_install_id
    WHERE submission.id = NEW.plugin_submission_id
      AND submission.rendered_html_sha256 = NEW.expected_rendered_html_sha256
      AND submission.extracted_listing_json IS NOT DISTINCT FROM NEW.extracted_listing_json
      AND submission.extraction_error IS NULL
      AND CAST(submission.submitted_at AS TIMESTAMPTZ) IS NOT NULL
      AND (
        install.revoked_at IS NULL
        OR CAST(submission.submitted_at AS TIMESTAMPTZ)
          <= CAST(install.revoked_at AS TIMESTAMPTZ)
      )
  ) THEN
    RAISE EXCEPTION 'replay extraction transition does not match its exact checkpoint';
  END IF;
  RETURN NEW;
END
$function$;

DO $trigger_install$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger
    WHERE tgname = 'listing_replay_run_items_checkpoint_exact'
      AND tgrelid = pg_catalog.to_regclass('public.listing_replay_run_items')
      AND NOT tgisinternal
  ) THEN
    CREATE TRIGGER listing_replay_run_items_checkpoint_exact
    BEFORE INSERT OR UPDATE ON public.listing_replay_run_items
    FOR EACH ROW EXECUTE FUNCTION public.enforce_replay_extraction_checkpoint_exactness();
  END IF;
END
$trigger_install$;

CREATE OR REPLACE FUNCTION public.preserve_completed_replay_item()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF OLD.materialization_state = 'succeeded' THEN
    RAISE EXCEPTION 'completed replay item is immutable';
  END IF;
  RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$function$;

DO $trigger_install$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger
    WHERE tgname = 'listing_replay_run_items_completed_immutable'
      AND tgrelid = pg_catalog.to_regclass('public.listing_replay_run_items')
      AND NOT tgisinternal
  ) THEN
    CREATE TRIGGER listing_replay_run_items_completed_immutable
    BEFORE UPDATE OR DELETE ON public.listing_replay_run_items
    FOR EACH ROW EXECUTE FUNCTION public.preserve_completed_replay_item();
  END IF;
END
$trigger_install$;

CREATE TABLE IF NOT EXISTS public.plugin_submission_materialization_receipts (
  plugin_submission_id BIGINT PRIMARY KEY
    REFERENCES public.plugin_submissions(id) ON DELETE CASCADE,
  aircraft_sale_listing_id BIGINT NOT NULL UNIQUE
    REFERENCES public.aircraft_sale_listings(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL
    CHECK (rendered_html_sha256 ~ '^[0-9a-f]{64}$'),
  extracted_listing_sha256 TEXT NOT NULL
    CHECK (extracted_listing_sha256 ~ '^[0-9a-f]{64}$'),
  completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE OR REPLACE FUNCTION public.preserve_replay_materialization_receipt()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  RAISE EXCEPTION 'replay materialization receipt is immutable';
END
$function$;

DO $trigger_install$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger
    WHERE tgname = 'plugin_submission_materialization_receipts_immutable'
      AND tgrelid = pg_catalog.to_regclass(
        'public.plugin_submission_materialization_receipts'
      )
      AND NOT tgisinternal
  ) THEN
    CREATE TRIGGER plugin_submission_materialization_receipts_immutable
    BEFORE UPDATE OR DELETE ON public.plugin_submission_materialization_receipts
    FOR EACH ROW EXECUTE FUNCTION public.preserve_replay_materialization_receipt();
  END IF;
END
$trigger_install$;

CREATE UNIQUE INDEX IF NOT EXISTS uq_aircraft_sale_listings_owner_source
  ON public.aircraft_sale_listings (created_by_user_id, source_url)
  WHERE source_url IS NOT NULL AND length(BTRIM(source_url)) > 0;

CREATE OR REPLACE FUNCTION public.enforce_replay_checkpoint_capture_immutability()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF (
    EXISTS (
      SELECT 1 FROM public.listing_replay_run_items item
      WHERE item.plugin_submission_id = OLD.id
        AND item.extraction_state = 'succeeded'
    )
    OR EXISTS (
      SELECT 1 FROM public.plugin_submission_materialization_receipts receipt
      WHERE receipt.plugin_submission_id = OLD.id
    )
  ) AND (
    NEW.id IS DISTINCT FROM OLD.id
    OR NEW.user_id IS DISTINCT FROM OLD.user_id
    OR NEW.plugin_install_id IS DISTINCT FROM OLD.plugin_install_id
    OR NEW.source_url IS DISTINCT FROM OLD.source_url
    OR NEW.submitted_at IS DISTINCT FROM OLD.submitted_at
    OR NEW.rendered_html IS DISTINCT FROM OLD.rendered_html
    OR NEW.rendered_html_sha256 IS DISTINCT FROM OLD.rendered_html_sha256
    OR NEW.signature_base64 IS DISTINCT FROM OLD.signature_base64
    OR NEW.extracted_listing_json IS DISTINCT FROM OLD.extracted_listing_json
    OR NEW.extraction_error IS DISTINCT FROM OLD.extraction_error
    OR NOT (
      NEW.canonical_listing_id IS NOT DISTINCT FROM OLD.canonical_listing_id
      OR (
        OLD.canonical_listing_id IS NULL
        AND NEW.canonical_listing_id IS NOT NULL
        AND NOT EXISTS (
          SELECT 1 FROM public.plugin_submission_materialization_receipts receipt
          WHERE receipt.plugin_submission_id = OLD.id
        )
        AND EXISTS (
          SELECT 1 FROM public.aircraft_sale_listings listing
          WHERE listing.id = NEW.canonical_listing_id
            AND listing.created_by_user_id = OLD.user_id
            AND listing.source_url = OLD.source_url
        )
      )
    )
  ) THEN
    RAISE EXCEPTION 'replay checkpoint capture is immutable';
  END IF;
  RETURN NEW;
END
$function$;

DO $trigger_install$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger
    WHERE tgname = 'plugin_submissions_replay_checkpoint_immutable'
      AND tgrelid = pg_catalog.to_regclass('public.plugin_submissions')
      AND NOT tgisinternal
  ) THEN
    CREATE TRIGGER plugin_submissions_replay_checkpoint_immutable
    BEFORE UPDATE ON public.plugin_submissions
    FOR EACH ROW EXECUTE FUNCTION public.enforce_replay_checkpoint_capture_immutability();
  END IF;
END
$trigger_install$;

CREATE OR REPLACE FUNCTION public.enforce_replay_plugin_identity_immutability()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM public.plugin_submissions submission
    WHERE submission.plugin_install_id = OLD.id
      AND (
        EXISTS (
          SELECT 1 FROM public.listing_replay_run_items item
          WHERE item.plugin_submission_id = submission.id
            AND item.extraction_state = 'succeeded'
        )
        OR EXISTS (
          SELECT 1 FROM public.plugin_submission_materialization_receipts receipt
          WHERE receipt.plugin_submission_id = submission.id
        )
      )
  ) AND (
    NEW.id IS DISTINCT FROM OLD.id
    OR NEW.user_id IS DISTINCT FROM OLD.user_id
    OR NEW.public_key_base64 IS DISTINCT FROM OLD.public_key_base64
    OR NEW.created_at IS DISTINCT FROM OLD.created_at
    OR NOT (
      NEW.revoked_at IS NOT DISTINCT FROM OLD.revoked_at
      OR (
        OLD.revoked_at IS NULL
        AND NEW.revoked_at IS NOT NULL
        AND NOT EXISTS (
          SELECT 1
          FROM public.plugin_submissions submission
          WHERE submission.plugin_install_id = OLD.id
            AND (
              EXISTS (
                SELECT 1 FROM public.listing_replay_run_items item
                WHERE item.plugin_submission_id = submission.id
                  AND item.extraction_state = 'succeeded'
              )
              OR EXISTS (
                SELECT 1 FROM public.plugin_submission_materialization_receipts receipt
                WHERE receipt.plugin_submission_id = submission.id
              )
            )
            AND CAST(submission.submitted_at AS TIMESTAMPTZ)
              > CAST(NEW.revoked_at AS TIMESTAMPTZ)
        )
      )
    )
  ) THEN
    RAISE EXCEPTION 'replay checkpoint plugin identity is immutable';
  END IF;
  RETURN NEW;
END
$function$;

DO $trigger_install$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger
    WHERE tgname = 'plugin_installs_replay_identity_immutable'
      AND tgrelid = pg_catalog.to_regclass('public.plugin_installs')
      AND NOT tgisinternal
  ) THEN
    CREATE TRIGGER plugin_installs_replay_identity_immutable
    BEFORE UPDATE ON public.plugin_installs
    FOR EACH ROW EXECUTE FUNCTION public.enforce_replay_plugin_identity_immutability();
  END IF;
END
$trigger_install$;

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_listing_replay_runs', 1,
  'ef344cdb9cf9a7ffcd0ae66e1c9cb3979afa07c1155377cee5dc1031dd0d47c1',
  CURRENT_TIMESTAMP
) ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
