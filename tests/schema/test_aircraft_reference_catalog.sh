#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_database="$(mktemp /tmp/aircost-reference-schema.XXXXXX.sqlite3)"
approval_database="$(mktemp /tmp/aircost-reference-approval.XXXXXX.sqlite3)"
component_database="$(mktemp /tmp/aircost-reference-component.XXXXXX.sqlite3)"
duplicate_price_database="$(mktemp /tmp/aircost-reference-duplicate-price.XXXXXX.sqlite3)"
overlap_database="$(mktemp /tmp/aircost-reference-overlap.XXXXXX.sqlite3)"
serial_overlap_database="$(mktemp /tmp/aircost-reference-serial-overlap.XXXXXX.sqlite3)"
incomplete_database="$(mktemp /tmp/aircost-reference-incomplete.XXXXXX.sqlite3)"
cleanup_database="$(mktemp /tmp/aircost-reference-cleanup.XXXXXX.sqlite3)"
marker_rerun_database="$(mktemp /tmp/aircost-reference-marker-rerun.XXXXXX.sqlite3)"
marker_mismatch_database="$(mktemp /tmp/aircost-reference-marker-mismatch.XXXXXX.sqlite3)"
marker_damage_database="$(mktemp /tmp/aircost-reference-marker-damage.XXXXXX.sqlite3)"
marker_unexpected_database="$(mktemp /tmp/aircost-reference-marker-unexpected.XXXXXX.sqlite3)"
marker_run_check_database="$(mktemp /tmp/aircost-reference-marker-run-check.XXXXXX.sqlite3)"
upgrade_database="$(mktemp /tmp/aircost-reference-upgrade.XXXXXX.sqlite3)"
upgrade_unexpected_index_database="$(mktemp /tmp/aircost-reference-upgrade-index.XXXXXX.sqlite3)"
upgrade_unexpected_trigger_database="$(mktemp /tmp/aircost-reference-upgrade-trigger.XXXXXX.sqlite3)"
upgrade_unexpected_price_index_database="$(mktemp /tmp/aircost-reference-upgrade-price-index.XXXXXX.sqlite3)"
upgrade_unexpected_version_trigger_database="$(mktemp /tmp/aircost-reference-upgrade-version-trigger.XXXXXX.sqlite3)"
upgrade_unexpected_scope_index_database="$(mktemp /tmp/aircost-reference-upgrade-scope-index.XXXXXX.sqlite3)"
upgrade_altered_check_database="$(mktemp /tmp/aircost-reference-upgrade-check.XXXXXX.sqlite3)"
upgrade_finalization_scalar_database="$(mktemp /tmp/aircost-reference-upgrade-finalization-scalar.XXXXXX.sqlite3)"
upgrade_finalization_string_database="$(mktemp /tmp/aircost-reference-upgrade-finalization-string.XXXXXX.sqlite3)"
upgrade_finalization_null_database="$(mktemp /tmp/aircost-reference-upgrade-finalization-null.XXXXXX.sqlite3)"
upgrade_finalization_missing_database="$(mktemp /tmp/aircost-reference-upgrade-finalization-missing.XXXXXX.sqlite3)"
marker_null_post_database="$(mktemp /tmp/aircost-reference-marker-null-post.XXXXXX.sqlite3)"
marker_null_predecessor_database="$(mktemp /tmp/aircost-reference-marker-null-predecessor.XXXXXX.sqlite3)"
marker_null_work_database="$(mktemp /tmp/aircost-reference-marker-null-work.XXXXXX.sqlite3)"
trap 'rm -f "$test_database" "$approval_database" "$component_database" "$duplicate_price_database" "$overlap_database" "$serial_overlap_database" "$incomplete_database" "$cleanup_database" "$marker_rerun_database" "$marker_mismatch_database" "$marker_damage_database" "$marker_unexpected_database" "$marker_run_check_database" "$upgrade_database" "$upgrade_unexpected_index_database" "$upgrade_unexpected_trigger_database" "$upgrade_unexpected_price_index_database" "$upgrade_unexpected_version_trigger_database" "$upgrade_unexpected_scope_index_database" "$upgrade_altered_check_database" "$upgrade_finalization_scalar_database" "$upgrade_finalization_string_database" "$upgrade_finalization_null_database" "$upgrade_finalization_missing_database" "$marker_null_post_database" "$marker_null_predecessor_database" "$marker_null_work_database"' EXIT

sqlite3 -bail "$test_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/tests/schema/aircraft_reference_catalog.sqlite.sql"

published_state="$(sqlite3 "$test_database" \
  "SELECT publication_state FROM aircraft_reference_configuration_versions WHERE id = 1")"
test "$published_state" = "published"
test "$(sqlite3 "$test_database" \
  "SELECT model_year || ':' || price_reference_year || ':' || configuration_basis FROM aircraft_reference_configuration_versions version JOIN aircraft_reference_prices price ON price.aircraft_reference_configuration_version_id=version.id WHERE version.id=1")" = "2020:2019:full_standard_configuration"

engine_catalog_target="$(sqlite3 "$test_database" \
  "SELECT \"table\" FROM pragma_foreign_key_list('aircraft_reference_engines') WHERE \"from\" = 'aircraft_engine_catalog_model_id'")"
propeller_catalog_target="$(sqlite3 "$test_database" \
  "SELECT \"table\" FROM pragma_foreign_key_list('aircraft_reference_propellers') WHERE \"from\" = 'aircraft_propeller_catalog_model_id'")"
test "$engine_catalog_target" = "aircraft_engine_catalog_models"
test "$propeller_catalog_target" = "aircraft_propeller_catalog_models"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='aircraft_curation_interaction_runs'")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM pragma_table_info('aircraft_identity_decisions') WHERE name='interaction_run_id'")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM pragma_table_info('aircraft_reference_profile_proposals') WHERE name='interaction_run_id'")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('aircraft_model_spec_versions','aircraft_model_variant_price_points','aircraft_model_variant_default_avionics','aircraft_model_variant_default_avionics_candidates','depreciation_profiles','depreciation_profile_fit_metadata','component_depreciation_profiles')")" = "0"

expect_failure() {
  local database="$1"
  local statement="$2"
  local expected_message="$3"
  local output
  if output="$(sqlite3 -bail "$database" "$statement" 2>&1)"; then
    echo "expected schema invariant failure: $statement" >&2
    exit 1
  fi
  if [[ "$output" != *"$expected_message"* ]]; then
    echo "schema invariant failed for the wrong reason" >&2
    echo "expected: $expected_message" >&2
    echo "actual: $output" >&2
    exit 1
  fi
}

sqlite_schema_digest() {
  sqlite3 "$1" "SELECT lower(hex(sha3(group_concat(
    type || ':' || name || ':' || tbl_name || ':' || coalesce(sql, ''), '|'
  ), 256))) FROM (
    SELECT type, name, tbl_name, sql FROM sqlite_schema
    ORDER BY type, name, tbl_name
  )"
}

make_reference_cutover_marker_nullable() {
  sqlite3 -bail "$1" "
    PRAGMA foreign_keys = OFF;
    ALTER TABLE schema_migration_contracts
      RENAME TO reference_cutover_original_contracts;
    CREATE TABLE schema_migration_contracts (
      migration_name TEXT PRIMARY KEY,
      contract_version INTEGER,
      contract_fingerprint TEXT,
      installed_at TEXT
    );
    INSERT INTO schema_migration_contracts
    SELECT migration_name, contract_version, contract_fingerprint, installed_at
    FROM reference_cutover_original_contracts;
    DROP TABLE reference_cutover_original_contracts;
    PRAGMA foreign_keys = ON;
  "
}

assert_null_reference_cutover_marker_rejected() {
  local source_database="$1"
  local null_assignment="$2"
  local runner="$3"
  local label="$4"
  cp "$source_database" "$marker_null_work_database"
  make_reference_cutover_marker_nullable "$marker_null_work_database"
  sqlite3 -bail "$marker_null_work_database" "
    UPDATE schema_migration_contracts
    SET $null_assignment
    WHERE migration_name = '20260819_reference_catalog_cutover';
  "
  local marker_before
  local schema_before
  marker_before="$(sqlite3 "$marker_null_work_database" "
    SELECT quote(contract_version) || ':' || quote(contract_fingerprint) || ':' ||
           quote(installed_at)
    FROM schema_migration_contracts
    WHERE migration_name = '20260819_reference_catalog_cutover'
  ")"
  schema_before="$(sqlite_schema_digest "$marker_null_work_database")"
  expect_failure "$marker_null_work_database" \
    ".read $repository_root/$runner" \
    "CHECK constraint failed"
  test "$(sqlite_schema_digest "$marker_null_work_database")" = "$schema_before" || {
    echo "$label: rejected marker must not heal or mutate the schema" >&2
    exit 1
  }
  test "$(sqlite3 "$marker_null_work_database" "
    SELECT quote(contract_version) || ':' || quote(contract_fingerprint) || ':' ||
           quote(installed_at)
    FROM schema_migration_contracts
    WHERE migration_name = '20260819_reference_catalog_cutover'
  ")" = "$marker_before" || {
    echo "$label: rejected marker must remain byte-for-byte unchanged" >&2
    exit 1
  }
}

cp "$test_database" "$marker_null_post_database"
sqlite3 -bail "$marker_null_predecessor_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/tests/schema/reference_catalog_cutover_predecessor.sqlite.sql" \
  "INSERT INTO schema_migration_contracts (
     migration_name, contract_version, contract_fingerprint, installed_at
   ) VALUES (
     '20260819_reference_catalog_cutover', 1,
     'fe31ca0eaae57cfc4ba5c824679bd950fcb98e20d6dd3e686a477fd22d05aab5',
     '2000-01-01 00:00:00'
   );"
for null_case in version fingerprint both
do
  case "$null_case" in
    version) null_assignment="contract_version = NULL" ;;
    fingerprint) null_assignment="contract_fingerprint = NULL" ;;
    both) null_assignment="contract_version = NULL, contract_fingerprint = NULL" ;;
  esac
  assert_null_reference_cutover_marker_rejected \
    "$marker_null_post_database" "$null_assignment" "schema/sqlite.sql" \
    "canonical-post-$null_case"
  assert_null_reference_cutover_marker_rejected \
    "$marker_null_post_database" "$null_assignment" \
    "migrations/20260819_reference_catalog_cutover.sqlite.sql" \
    "migration-post-$null_case"
  assert_null_reference_cutover_marker_rejected \
    "$marker_null_predecessor_database" "$null_assignment" "schema/sqlite.sql" \
    "canonical-predecessor-$null_case"
  assert_null_reference_cutover_marker_rejected \
    "$marker_null_predecessor_database" "$null_assignment" \
    "migrations/20260819_reference_catalog_cutover.sqlite.sql" \
    "migration-predecessor-$null_case"
done

sqlite3 -bail "$upgrade_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/tests/schema/reference_catalog_cutover_predecessor.sqlite.sql" \
  "INSERT INTO users (email, display_name, auth_subject)
   VALUES ('run-history@example.test', 'Run history', 'run-history');
   INSERT INTO aircraft_sale_listings (
     aircraft_model_variant_id, created_by_user_id, source_url, model_year,
     asking_price_usd, registration_number, airframe_hours, ingestion_state
   ) SELECT placeholder.aircraft_model_variant_id, user.id,
       'https://listing.example/run-history', 2020, 100000, 'N12345', 100,
       'incomplete'
     FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,
          users user
     WHERE placeholder.singleton_id = 1
       AND user.auth_subject = 'run-history';
   INSERT INTO listing_verification_runs (
     owner_user_id, idempotency_key, request_fingerprint,
     status, completed_at
   ) SELECT id, 'historical-reference', printf('%064d', 0),
       'completed', CURRENT_TIMESTAMP
     FROM users WHERE auth_subject = 'run-history';
   INSERT INTO listing_verification_run_items (
     run_id, listing_id, position, status, outcome_json,
     reason_code, reason, created_at, updated_at, completed_at
   ) SELECT run.id, listing.id, 0, 'pending_reference', json_object(
       'status', 'pending_reference',
       'final_ingestion_state', 'incomplete',
       'reference', json_object('status', 'pending_reference'),
       'finalization', json_object(
         'status', 'pending_reference', 'reviewer_note', 'preserve me'
       )
     ), 'factory_reference_pending', 'Factory reference pending',
       '2000-01-01 00:00:00', '2000-01-01 00:00:00', CURRENT_TIMESTAMP
     FROM listing_verification_runs run, aircraft_sale_listings listing
     WHERE run.idempotency_key = 'historical-reference';
   INSERT INTO aircraft_sale_listings (
     aircraft_model_variant_id, created_by_user_id, source_url, model_year,
     asking_price_usd, registration_number, airframe_hours, ingestion_state
   ) SELECT placeholder.aircraft_model_variant_id, user.id,
       'https://listing.example/run-history-failed', 2020, 110000,
       'N54321', 110, 'incomplete'
     FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,
          users user
     WHERE placeholder.singleton_id = 1
       AND user.auth_subject = 'run-history';
   INSERT INTO listing_verification_run_items (
     run_id, listing_id, position, status, outcome_json,
     reason_code, reason, created_at, updated_at, completed_at
   ) SELECT run.id, listing.id, 1, 'failed', NULL,
       'historical_failure', 'Historical failure',
       '2001-01-01 00:00:00', '2001-01-01 00:00:00', CURRENT_TIMESTAMP
     FROM listing_verification_runs run, aircraft_sale_listings listing
     WHERE run.idempotency_key = 'historical-reference'
       AND listing.source_url = 'https://listing.example/run-history-failed';
   UPDATE sqlite_sequence SET seq = 41
   WHERE name = 'listing_verification_run_items';
   INSERT INTO sqlite_sequence (name, seq)
   SELECT 'aircraft_reference_prices', 37
   WHERE NOT EXISTS (
     SELECT 1 FROM sqlite_sequence WHERE name = 'aircraft_reference_prices'
   );
   UPDATE sqlite_sequence SET seq = 37
   WHERE name = 'aircraft_reference_prices';"

cp "$upgrade_database" "$upgrade_unexpected_index_database"
cp "$upgrade_database" "$upgrade_unexpected_trigger_database"
cp "$upgrade_database" "$upgrade_unexpected_price_index_database"
cp "$upgrade_database" "$upgrade_unexpected_version_trigger_database"
cp "$upgrade_database" "$upgrade_unexpected_scope_index_database"
cp "$upgrade_database" "$upgrade_finalization_scalar_database"
cp "$upgrade_database" "$upgrade_finalization_string_database"
cp "$upgrade_database" "$upgrade_finalization_null_database"
cp "$upgrade_database" "$upgrade_finalization_missing_database"

sqlite3 -bail "$upgrade_unexpected_index_database" \
  "CREATE INDEX unexpected_run_item_index
   ON listing_verification_run_items(reason_code);"
expect_failure "$upgrade_unexpected_index_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "CHECK constraint failed"
test "$(sqlite3 "$upgrade_unexpected_index_database" \
  "SELECT count(*) FROM sqlite_schema WHERE name='unexpected_run_item_index';
   SELECT count(*) FROM schema_migration_contracts
   WHERE migration_name='20260819_reference_catalog_cutover';")" = $'1\n0'

sqlite3 -bail "$upgrade_unexpected_trigger_database" \
  "CREATE TRIGGER unexpected_run_item_trigger
   BEFORE UPDATE ON listing_verification_run_items BEGIN SELECT 1; END;"
expect_failure "$upgrade_unexpected_trigger_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "CHECK constraint failed"
test "$(sqlite3 "$upgrade_unexpected_trigger_database" \
  "SELECT count(*) FROM sqlite_schema WHERE name='unexpected_run_item_trigger';
   SELECT count(*) FROM schema_migration_contracts
   WHERE migration_name='20260819_reference_catalog_cutover';")" = $'1\n0'

sqlite3 -bail "$upgrade_unexpected_price_index_database" \
  "CREATE INDEX unexpected_reference_price_upgrade_index
   ON aircraft_reference_prices(amount);"
expect_failure "$upgrade_unexpected_price_index_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "CHECK constraint failed"
test "$(sqlite3 "$upgrade_unexpected_price_index_database" \
  "SELECT count(*) FROM sqlite_schema
   WHERE name='unexpected_reference_price_upgrade_index';
   SELECT count(*) FROM schema_migration_contracts
   WHERE migration_name='20260819_reference_catalog_cutover';")" = $'1\n0'

sqlite3 -bail "$upgrade_unexpected_version_trigger_database" \
  "CREATE TRIGGER unexpected_reference_version_upgrade_trigger
   BEFORE INSERT ON aircraft_reference_configuration_versions
   BEGIN SELECT 1; END;"
expect_failure "$upgrade_unexpected_version_trigger_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "CHECK constraint failed"
test "$(sqlite3 "$upgrade_unexpected_version_trigger_database" \
  "SELECT count(*) FROM sqlite_schema
   WHERE name='unexpected_reference_version_upgrade_trigger';
   SELECT count(*) FROM schema_migration_contracts
   WHERE migration_name='20260819_reference_catalog_cutover';")" = $'1\n0'

sqlite3 -bail "$upgrade_unexpected_scope_index_database" \
  "CREATE INDEX unexpected_reference_scope_upgrade_index
   ON aircraft_reference_applicability_scopes(aircraft_market_id)
   WHERE applies_to_all_serials = 1;"
expect_failure "$upgrade_unexpected_scope_index_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "CHECK constraint failed"
test "$(sqlite3 "$upgrade_unexpected_scope_index_database" \
  "SELECT count(*) FROM sqlite_schema
   WHERE name='unexpected_reference_scope_upgrade_index';
   SELECT count(*) FROM schema_migration_contracts
   WHERE migration_name='20260819_reference_catalog_cutover';")" = $'1\n0'

sqlite3 -bail "$upgrade_finalization_scalar_database" \
  "UPDATE listing_verification_run_items
   SET outcome_json = json_set(outcome_json, '$.finalization', 7)
   WHERE status = 'pending_reference';"
sqlite3 -bail "$upgrade_finalization_string_database" \
  "UPDATE listing_verification_run_items
   SET outcome_json = json_set(outcome_json, '$.finalization', 'legacy')
   WHERE status = 'pending_reference';"
sqlite3 -bail "$upgrade_finalization_null_database" \
  "UPDATE listing_verification_run_items
   SET outcome_json = json_set(outcome_json, '$.finalization', json('null'))
   WHERE status = 'pending_reference';"
sqlite3 -bail "$upgrade_finalization_missing_database" \
  "UPDATE listing_verification_run_items
   SET outcome_json = json_remove(outcome_json, '$.finalization')
   WHERE status = 'pending_reference';"
for malformed_database in \
  "$upgrade_finalization_scalar_database" \
  "$upgrade_finalization_string_database" \
  "$upgrade_finalization_null_database" \
  "$upgrade_finalization_missing_database"
do
  malformed_outcome_before="$(sqlite3 "$malformed_database" \
    "SELECT outcome_json FROM listing_verification_run_items
     WHERE status='pending_reference';")"
  expect_failure "$malformed_database" \
    ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
    "CHECK constraint failed"
  test "$(sqlite3 "$malformed_database" \
    "SELECT outcome_json FROM listing_verification_run_items
     WHERE status='pending_reference';")" = "$malformed_outcome_before"
  test "$(sqlite3 "$malformed_database" \
    "SELECT count(*) FROM schema_migration_contracts
     WHERE migration_name='20260819_reference_catalog_cutover';")" = "0"
done

sqlite3 -bail "$upgrade_altered_check_database" \
  ".read $repository_root/schema/sqlite.sql"
sed "0,/'cancelled'/s/'cancelled'/'cancelled', 'hostile'/" \
  "$repository_root/tests/schema/reference_catalog_cutover_predecessor.sqlite.sql" \
  | sqlite3 -bail "$upgrade_altered_check_database"
expect_failure "$upgrade_altered_check_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "CHECK constraint failed"
test "$(sqlite3 "$upgrade_altered_check_database" \
  "SELECT instr(sql, '''hostile''') > 0 FROM sqlite_schema
   WHERE type='table' AND name='listing_verification_run_items';
   SELECT count(*) FROM schema_migration_contracts
   WHERE migration_name='20260819_reference_catalog_cutover';")" = $'1\n0'

sqlite3 -bail "$upgrade_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "PRAGMA foreign_key_check; PRAGMA integrity_check;"
test "$(sqlite3 "$upgrade_database" \
  "SELECT status || ':' || reason_code || ':' ||
     json_extract(outcome_json, '$.status') || ':' ||
     json_extract(outcome_json, '$.final_ingestion_state') || ':' ||
     json_extract(outcome_json, '$.reference.status') || ':' ||
     json_extract(outcome_json, '$.finalization.status') || ':' ||
     json_extract(outcome_json, '$.finalization.reviewer_note')
   FROM listing_verification_run_items WHERE position = 0;")" = \
  "blocked:legacy_reference_gate_removed:blocked:incomplete:pending_reference:not_attempted:preserve me"
test "$(sqlite3 "$upgrade_database" \
  "SELECT updated_at <> '2000-01-01 00:00:00'
   FROM listing_verification_run_items WHERE position = 0;
   SELECT updated_at FROM listing_verification_run_items WHERE position = 1;
   SELECT seq FROM sqlite_sequence
   WHERE name = 'listing_verification_run_items';
   SELECT seq FROM sqlite_sequence
   WHERE name = 'aircraft_reference_prices';")" = $'1\n2001-01-01 00:00:00\n41\n37'
expect_failure "$upgrade_database" \
  "UPDATE listing_verification_run_items SET status='pending_reference' WHERE id=1" \
  "CHECK constraint failed"

fresh_applicability_sql="$(sqlite3 "$test_database" \
  "SELECT lower(replace(replace(replace(replace(replace(
     sql, char(9), ''), char(10), ''), char(13), ''), ' ', ''),
     'ifnotexists', '')) FROM sqlite_schema
   WHERE type='table' AND name='aircraft_reference_applicability_scopes'")"
upgrade_applicability_sql="$(sqlite3 "$upgrade_database" \
  "SELECT lower(replace(replace(replace(replace(replace(
     sql, char(9), ''), char(10), ''), char(13), ''), ' ', ''),
     'ifnotexists', '')) FROM sqlite_schema
   WHERE type='table' AND name='aircraft_reference_applicability_scopes'")"
test "$upgrade_applicability_sql" = "$fresh_applicability_sql"

fresh_run_item_sql="$(sqlite3 "$test_database" \
  "SELECT lower(replace(replace(replace(replace(replace(
     sql, char(9), ''), char(10), ''), char(13), ''), ' ', ''),
     'ifnotexists', '')) FROM sqlite_schema
   WHERE type='table' AND name='listing_verification_run_items'")"
upgrade_run_item_sql="$(sqlite3 "$upgrade_database" \
  "SELECT lower(replace(replace(replace(replace(replace(
     sql, char(9), ''), char(10), ''), char(13), ''), ' ', ''),
     'ifnotexists', '')) FROM sqlite_schema
   WHERE type='table' AND name='listing_verification_run_items'")"
test "$upgrade_run_item_sql" = "$fresh_run_item_sql"

sqlite3 -bail "$marker_rerun_database" \
  ".read $repository_root/schema/sqlite.sql" \
  "UPDATE schema_migration_contracts
   SET installed_at = '2000-01-01 00:00:00'
   WHERE migration_name = '20260819_reference_catalog_cutover';" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/tests/schema/aircraft_reference_catalog.sqlite.sql" \
  "UPDATE sqlite_sequence SET seq = 41
   WHERE name = 'aircraft_reference_prices';" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "PRAGMA foreign_key_check;" \
  "PRAGMA integrity_check;"
test "$(sqlite3 "$marker_rerun_database" \
  "SELECT installed_at FROM schema_migration_contracts
   WHERE migration_name = '20260819_reference_catalog_cutover'")" = \
  "2000-01-01 00:00:00"
test "$(sqlite3 "$marker_rerun_database" \
  "SELECT id || ':' || configuration_basis
   FROM aircraft_reference_prices")" = "1:full_standard_configuration"
test "$(sqlite3 "$marker_rerun_database" \
  "SELECT seq FROM sqlite_sequence
   WHERE name = 'aircraft_reference_prices'")" = "41"

sqlite3 -bail "$marker_mismatch_database" \
  ".read $repository_root/schema/sqlite.sql" \
  "UPDATE schema_migration_contracts
   SET contract_fingerprint = '0000000000000000000000000000000000000000000000000000000000000000'
   WHERE migration_name = '20260819_reference_catalog_cutover';"
expect_failure "$marker_mismatch_database" \
  ".read $repository_root/schema/sqlite.sql" \
  "CHECK constraint failed"

sqlite3 -bail "$marker_damage_database" \
  ".read $repository_root/schema/sqlite.sql" \
  "DROP TABLE official_dollar_normalization_facts;"
expect_failure "$marker_damage_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "CHECK constraint failed"
test "$(sqlite3 "$marker_damage_database" \
  "SELECT count(*) FROM sqlite_schema
   WHERE type = 'table' AND name = 'official_dollar_normalization_facts'")" = "0"

sqlite3 -bail "$marker_unexpected_database" \
  ".read $repository_root/schema/sqlite.sql" \
  "CREATE TRIGGER unexpected_reference_price_trigger
     BEFORE INSERT ON aircraft_reference_prices BEGIN SELECT 1; END;
   CREATE TRIGGER unexpected_reference_fact_set_trigger
     BEFORE INSERT ON aircraft_reference_fact_set_attestations BEGIN SELECT 1; END;
   CREATE TRIGGER unexpected_reference_normalization_trigger
     BEFORE INSERT ON official_dollar_normalization_facts BEGIN SELECT 1; END;
   CREATE INDEX unexpected_reference_price_index
     ON aircraft_reference_prices(amount);
   CREATE INDEX unexpected_reference_fact_set_index
     ON aircraft_reference_fact_set_attestations(evidence_claim_id);
   CREATE INDEX unexpected_reference_normalization_index
     ON official_dollar_normalization_facts(index_series);
   CREATE TRIGGER unexpected_verification_run_trigger
     BEFORE INSERT ON listing_verification_run_items BEGIN SELECT 1; END;
   CREATE INDEX unexpected_verification_run_index
     ON listing_verification_run_items(reason_code);
   CREATE TRIGGER unexpected_reference_version_trigger
     BEFORE INSERT ON aircraft_reference_configuration_versions
     BEGIN SELECT 1; END;
   CREATE INDEX unexpected_reference_version_index
     ON aircraft_reference_configuration_versions(model_year);
   CREATE INDEX unexpected_reference_scope_partial_index
     ON aircraft_reference_applicability_scopes(aircraft_market_id)
     WHERE applies_to_all_serials = 1;"
expect_failure "$marker_unexpected_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "CHECK constraint failed"
expect_failure "$marker_unexpected_database" \
  ".read $repository_root/schema/sqlite.sql" \
  "CHECK constraint failed"
test "$(sqlite3 "$marker_unexpected_database" \
  "SELECT count(*) FROM sqlite_schema
   WHERE name IN (
     'unexpected_reference_price_trigger',
     'unexpected_reference_fact_set_trigger',
     'unexpected_reference_normalization_trigger',
     'unexpected_reference_price_index',
     'unexpected_reference_fact_set_index',
     'unexpected_reference_normalization_index',
     'unexpected_verification_run_trigger',
     'unexpected_verification_run_index',
     'unexpected_reference_version_trigger',
     'unexpected_reference_version_index',
     'unexpected_reference_scope_partial_index'
   )")" = "11"

sqlite3 -bail "$marker_run_check_database" \
  ".read $repository_root/schema/sqlite.sql" \
  "CREATE TEMP TABLE cutover_marker_copy AS
   SELECT * FROM schema_migration_contracts
   WHERE migration_name='20260819_reference_catalog_cutover';" \
  ".read $repository_root/tests/schema/reference_catalog_cutover_predecessor.sqlite.sql" \
  "INSERT INTO schema_migration_contracts
   SELECT * FROM cutover_marker_copy;"
expect_failure "$marker_run_check_database" \
  ".read $repository_root/migrations/20260819_reference_catalog_cutover.sqlite.sql" \
  "CHECK constraint failed"
test "$(sqlite3 "$marker_run_check_database" \
  "SELECT instr(sql, '''pending_reference''') > 0
   FROM sqlite_schema
   WHERE type='table' AND name='listing_verification_run_items';
   SELECT count(*) FROM schema_migration_contracts
   WHERE migration_name='20260819_reference_catalog_cutover';")" = $'1\n1'

expect_failure "$test_database" \
  "UPDATE aircraft_reference_prices SET amount = 1 WHERE id = 1" \
  "reference profile facts are immutable"
expect_failure "$test_database" \
  "UPDATE aircraft_engine_catalog_models SET model_name = 'mutated' WHERE id = 1" \
  "approved engine catalog models are immutable"
expect_failure "$test_database" \
  "INSERT INTO official_dollar_normalization_facts (source_year, target_year, index_series, source_index_value, target_index_value, normalization_factor, evidence_claim_id) VALUES (2019, 2026, 'unapproved index', 250, 300, 1.2, 1)" \
  "dollar normalization requires validated official regulator evidence"
sqlite3 -bail "$test_database" "
  INSERT INTO curation_evidence_sources (
    source_url, source_title, source_domain, source_tier, retrieved_at
  ) VALUES (
    'https://www.bls.gov/cpi/schema-test', 'Official CPI schema test',
    'bls.gov', 'regulator_primary', '2026-08-19'
  );
  INSERT INTO curation_evidence_claims (
    evidence_source_id, claim_kind, subject_text, predicate_text, object_text,
    quoted_evidence, validation_status, validated_at
  ) VALUES (
    2, 'price', 'official CPI', 'reports index values', '2019=250; 2026=300',
    'Official government series reports 250 and 300.',
    'validated', '2026-08-19'
  );
  INSERT INTO official_dollar_normalization_facts (
    source_year, target_year, index_series, source_index_value,
    target_index_value, normalization_factor, evidence_claim_id
  ) VALUES (2019, 2026, 'Official CPI', 250, 300, 1.2, 5);
"
test "$(sqlite3 "$test_database" \
  "SELECT normalization_factor FROM official_dollar_normalization_facts WHERE source_year=2019 AND target_year=2026")" = "1.2"
expect_failure "$test_database" \
  "UPDATE official_dollar_normalization_facts SET normalization_factor = 1 WHERE id = 1" \
  "official dollar normalization facts are immutable"

cp "$test_database" "$duplicate_price_database"
sqlite3 -bail "$duplicate_price_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (1, 'reference_profile', 'approve_new', 'approved', '{}', '{}', 1,
    'duplicate-price guard', '2026-08-19');
  INSERT INTO aircraft_identity_decision_claims
    (decision_id, evidence_claim_id, evidence_role)
  SELECT max(id), 1, 'identity' FROM aircraft_identity_decisions;
  INSERT INTO aircraft_reference_configuration_versions (
    aircraft_reference_configuration_id, model_year, revision,
    supersedes_version_id, approval_decision_id
  ) SELECT 1, 2020, 2, 1, max(id) FROM aircraft_identity_decisions;
  INSERT INTO aircraft_reference_prices (
    aircraft_reference_configuration_version_id, price_kind, amount, currency,
    price_reference_year, configuration_basis, evidence_kind, evidence_claim_id
  ) VALUES (2, 'equipped_msrp', 789900, 'USD', 2020,
    'full_standard_configuration', 'direct_model_year', 3);
"
expect_failure "$duplicate_price_database" \
  "INSERT INTO aircraft_reference_prices (aircraft_reference_configuration_version_id, price_kind, amount, currency, price_reference_year, configuration_basis, evidence_kind, evidence_claim_id) VALUES (2, 'equipped_msrp', 799900, 'USD', 2020, 'full_standard_configuration', 'direct_model_year', 3)" \
  "UNIQUE constraint failed: aircraft_reference_prices.aircraft_reference_configuration_version_id, aircraft_reference_prices.price_kind, aircraft_reference_prices.currency"

cp "$test_database" "$approval_database"
expect_failure "$approval_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    selected_entity_id, decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (1, 'make', 'match_existing', 'approved', 1, '{}', '{}', 1, 'match', '2026-07-21');
  INSERT INTO aircraft_identity_decision_claims
    (decision_id, evidence_claim_id, evidence_role)
  SELECT max(id), 1, 'identity' FROM aircraft_identity_decisions;
  INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id)
  SELECT 'Not New', 'not new', max(id) FROM aircraft_identity_decisions;
" "aircraft make requires an approved primary-source decision"

cp "$test_database" "$component_database"
expect_failure "$component_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (1, 'make', 'approve_new', 'approved', '{}', '{}', 1, 'wrong kind', '2026-07-21');
  INSERT INTO aircraft_identity_decision_claims
    (decision_id, evidence_claim_id, evidence_role)
  SELECT max(id), 1, 'identity' FROM aircraft_identity_decisions;
  INSERT INTO aircraft_engine_catalog_models (
    manufacturer_name, normalized_manufacturer_name,
    model_name, normalized_model_name,
    identifier_authority, normalized_identifier_authority,
    identifier_kind, authoritative_identifier,
    normalized_authoritative_identifier,
    approval_decision_id, identity_evidence_claim_id
  ) SELECT
    'Untrusted', 'untrusted', 'Bad Engine', 'bad engine',
    'Untrusted', 'untrusted', 'manufacturer_model_code',
    'BAD-1', 'bad-1', max(id), 1
  FROM aircraft_identity_decisions;
" "engine catalog model requires an approved primary-source identifier"

cp "$test_database" "$overlap_database"
expect_failure "$overlap_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (1, 'reference_profile', 'approve_new', 'approved', '{}', '{}', 1, 'replacement', '2026-07-21');
  INSERT INTO aircraft_identity_decision_claims
    (decision_id, evidence_claim_id, evidence_role)
  SELECT max(id), 1, 'identity' FROM aircraft_identity_decisions;
  INSERT INTO aircraft_reference_configuration_versions (
    aircraft_reference_configuration_id, model_year, revision,
    supersedes_version_id, approval_decision_id
  ) SELECT 1, 2020, 2, 1, max(id) FROM aircraft_identity_decisions;
  INSERT INTO aircraft_reference_applicability_scopes (
    aircraft_reference_configuration_version_id, aircraft_market_id,
    applies_to_all_serials, evidence_claim_id
  ) VALUES (2, (SELECT id FROM aircraft_markets WHERE code = 'US'), 1, 2);
  INSERT INTO aircraft_reference_prices (
    aircraft_reference_configuration_version_id, price_kind, amount, currency,
    price_reference_year, configuration_basis, evidence_kind, evidence_claim_id
  ) VALUES (2, 'equipped_msrp', 789900, 'USD', 2019,
    'full_standard_configuration', 'direct_model_year', 3);
  INSERT INTO aircraft_reference_fact_set_attestations (
    aircraft_reference_configuration_version_id, fact_set_kind, evidence_claim_id
  ) VALUES
    (2, 'avionics', 4), (2, 'engines', 4),
    (2, 'propellers', 4), (2, 'features', 4);
  UPDATE aircraft_reference_configuration_versions
  SET publication_state = 'published', published_at = '2026-07-21'
  WHERE id = 2;
" "published reference profile applicability overlaps an existing version"

cp "$test_database" "$serial_overlap_database"
sqlite3 -bail "$serial_overlap_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES
    (1, 'serial_scheme', 'approve_new', 'approved', '{}', '{}', 1,
      'natural serial scheme A', '2026-08-19'),
    (1, 'serial_scheme', 'approve_new', 'approved', '{}', '{}', 1,
      'natural serial scheme B', '2026-08-19');
  INSERT INTO aircraft_identity_decision_claims
    (decision_id, evidence_claim_id, evidence_role)
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
"

serial_profile_sql() {
  local label="$1"
  local left_scope="$2"
  local right_scope="$3"
  printf '%s\n' "
    BEGIN;
    INSERT INTO aircraft_identity_decisions (
      resolution_case_id, entity_kind, decision_action, decision_status,
      decision_payload_json, deterministic_validation_json,
      deterministic_validation_passed, rationale, decided_at
    ) VALUES (1, 'reference_profile', 'approve_new', 'approved', '{}', '{}', 1,
      '$label', '2026-08-19');
    INSERT INTO aircraft_identity_decision_claims
      (decision_id, evidence_claim_id, evidence_role)
    SELECT id, 1, 'identity' FROM aircraft_identity_decisions
    WHERE rationale = '$label';
    INSERT INTO aircraft_reference_configuration_versions (
      aircraft_reference_configuration_id, model_year, revision,
      approval_decision_id
    ) SELECT 1, 2021, 1, id FROM aircraft_identity_decisions
      WHERE rationale = '$label';
    INSERT INTO aircraft_reference_applicability_scopes (
      aircraft_reference_configuration_version_id, aircraft_market_id,
      applies_to_all_serials, aircraft_serial_number_scheme_id,
      serial_prefix, serial_from_display, serial_to_display,
      serial_from_sort_key, serial_to_sort_key, evidence_claim_id
    )
    SELECT max(id), $left_scope
      FROM aircraft_reference_configuration_versions
    UNION ALL
    SELECT max(id), $right_scope
      FROM aircraft_reference_configuration_versions;
    INSERT INTO aircraft_reference_prices (
      aircraft_reference_configuration_version_id, price_kind, amount, currency,
      price_reference_year, configuration_basis, evidence_kind, evidence_claim_id
    ) SELECT max(id), 'equipped_msrp', 799900, 'USD', 2021,
        'full_standard_configuration', 'direct_model_year', 3
      FROM aircraft_reference_configuration_versions;
    INSERT INTO aircraft_reference_fact_set_attestations (
      aircraft_reference_configuration_version_id, fact_set_kind, evidence_claim_id
    )
    SELECT max(id), 'avionics', 4 FROM aircraft_reference_configuration_versions
    UNION ALL SELECT max(id), 'engines', 4 FROM aircraft_reference_configuration_versions
    UNION ALL SELECT max(id), 'propellers', 4 FROM aircraft_reference_configuration_versions
    UNION ALL SELECT max(id), 'features', 4 FROM aircraft_reference_configuration_versions;
    UPDATE aircraft_reference_configuration_versions
    SET publication_state = 'published', published_at = '2026-08-19'
    WHERE approval_decision_id = (
      SELECT id FROM aircraft_identity_decisions WHERE rationale = '$label'
    );
    COMMIT;
  "
}

natural_scheme_a="(SELECT id FROM aircraft_serial_number_schemes WHERE name='Natural A')"
natural_scheme_b="(SELECT id FROM aircraft_serial_number_schemes WHERE name='Natural B')"
s100_key="0110130020000000031000000000310000"
sr100_key="011013120020000000031000000000310000"
sr199_key="011013120020000000031990000000319900"
sr200_key="011013120020000000032000000000320000"
sr300_key="011013120020000000033000000000330000"
sz999_key="0110131A0020000000039990000000399900"

expect_failure "$serial_overlap_database" "$(serial_profile_sql \
  'reject caller defined serial key domain' \
  "1, 0, $natural_scheme_a, 'SR', 'SR100', 'SR199', '$s100_key', '$sr199_key', 2" \
  "1, 0, $natural_scheme_a, 'SR', 'SR200', 'SR300', '$sr200_key', '$sr300_key', 2")" \
  "reference serial sort keys must be recomputed from canonical display values"
expect_failure "$serial_overlap_database" "$(serial_profile_sql \
  'reject unrelated serial prefix' \
  "1, 0, $natural_scheme_a, 'ZZ', 'SR100', 'SR199', '$sr100_key', '$sr199_key', 2" \
  "1, 0, $natural_scheme_a, 'SR', 'SR200', 'SR300', '$sr200_key', '$sr300_key', 2")" \
  "reference serial applicability requires the universal natural-order key"

expect_failure "$serial_overlap_database" "$(serial_profile_sql \
  'overlap across S and SR prefixes' \
  "1, 0, $natural_scheme_a, 'S', 'S100', 'SR200', '$s100_key', '$sr200_key', 2" \
  "1, 0, $natural_scheme_a, 'SR', 'SR100', 'SR300', '$sr100_key', '$sr300_key', 2")" \
  "reference profile contains overlapping applicability scopes"
expect_failure "$serial_overlap_database" "$(serial_profile_sql \
  'overlap across null and SR prefixes' \
  "1, 0, $natural_scheme_a, NULL, 'S100', 'SZ999', '$s100_key', '$sz999_key', 2" \
  "1, 0, $natural_scheme_a, 'SR', 'SR100', 'SR200', '$sr100_key', '$sr200_key', 2")" \
  "reference profile contains overlapping applicability scopes"
expect_failure "$serial_overlap_database" "$(serial_profile_sql \
  'overlap across serial schemes' \
  "1, 0, $natural_scheme_a, 'SR', 'SR100', 'SR200', '$sr100_key', '$sr200_key', 2" \
  "1, 0, $natural_scheme_b, 'SR', 'SR100', 'SR200', '$sr100_key', '$sr200_key', 2")" \
  "reference profile contains overlapping applicability scopes"
expect_failure "$serial_overlap_database" "$(serial_profile_sql \
  'overlap at inclusive serial boundary' \
  "1, 0, $natural_scheme_a, 'SR', 'SR100', 'SR200', '$sr100_key', '$sr200_key', 2" \
  "1, 0, $natural_scheme_a, 'SR', 'SR200', 'SR300', '$sr200_key', '$sr300_key', 2")" \
  "reference profile contains overlapping applicability scopes"
expect_failure "$serial_overlap_database" "$(serial_profile_sql \
  'all serials overlaps bounded serials' \
  "1, 1, NULL, NULL, NULL, NULL, NULL, NULL, 2" \
  "1, 0, $natural_scheme_a, 'SR', 'SR100', 'SR200', '$sr100_key', '$sr200_key', 2")" \
  "reference profile contains overlapping applicability scopes"

sqlite3 -bail "$serial_overlap_database" "$(serial_profile_sql \
  'disjoint adjacent serial ranges' \
  "1, 0, $natural_scheme_a, 'SR', 'SR100', 'SR199', '$sr100_key', '$sr199_key', 2" \
  "1, 0, $natural_scheme_b, 'SR', 'SR200', 'SR300', '$sr200_key', '$sr300_key', 2")"
test "$(sqlite3 "$serial_overlap_database" \
  "SELECT publication_state FROM aircraft_reference_configuration_versions WHERE model_year=2021")" = "published"

cp "$test_database" "$incomplete_database"
sqlite3 -bail "$incomplete_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (1, 'reference_profile', 'approve_new', 'approved', '{}', '{}', 1, 'next year', '2026-07-21');
  INSERT INTO aircraft_identity_decision_claims
    (decision_id, evidence_claim_id, evidence_role)
  SELECT max(id), 1, 'identity' FROM aircraft_identity_decisions;
  INSERT INTO aircraft_reference_configuration_versions (
    aircraft_reference_configuration_id, model_year, revision, approval_decision_id
  ) SELECT 1, 2021, 1, max(id) FROM aircraft_identity_decisions;
  INSERT INTO aircraft_reference_applicability_scopes (
    aircraft_reference_configuration_version_id, aircraft_market_id,
    applies_to_all_serials, evidence_claim_id
  ) VALUES (2, 1, 1, 2);
  INSERT INTO aircraft_reference_prices (
    aircraft_reference_configuration_version_id, price_kind, amount, currency,
    price_reference_year, configuration_basis, evidence_kind, evidence_claim_id
  ) VALUES (2, 'equipped_msrp', 799900, 'USD', 2020,
    'full_standard_configuration', 'direct_model_year', 3);
"
expect_failure "$incomplete_database" "
  UPDATE aircraft_reference_configuration_versions
  SET publication_state = 'published', published_at = '2026-07-21'
  WHERE id = 2;
" "published reference profile requires complete factory fact-set attestations"
test "$(sqlite3 "$incomplete_database" \
  "SELECT publication_state FROM aircraft_reference_configuration_versions WHERE id=2")" = "building"

cp "$test_database" "$cleanup_database"
sqlite3 -bail "$cleanup_database" "
  CREATE TABLE aircraft_curation_interaction_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_json TEXT NOT NULL,
    response_json TEXT
  );
  ALTER TABLE aircraft_identity_decisions
    ADD COLUMN interaction_run_id INTEGER
      REFERENCES aircraft_curation_interaction_runs(id) ON DELETE RESTRICT;
  ALTER TABLE aircraft_reference_profile_proposals
    ADD COLUMN interaction_run_id INTEGER
      REFERENCES aircraft_curation_interaction_runs(id) ON DELETE RESTRICT;
  INSERT INTO aircraft_curation_interaction_runs (request_json, response_json)
  VALUES ('{\"prompt\":\"unused\"}', '{\"response\":\"unused\"}');
  UPDATE aircraft_identity_decisions SET interaction_run_id = 1 WHERE id = 1;
  INSERT INTO aircraft_reference_profile_proposals (
    resolution_case_id, interaction_run_id, proposed_identity_json,
    proposed_profile_json, deterministic_validation_json, validation_status,
    catalog_revision
  ) VALUES (1, 1, '{}', '{}', '{}', 'valid', 'catalog-1');
" \
  ".read $repository_root/migrations/20260727_remove_unused_aircraft_curation_runs.sqlite.sql" \
  ".read $repository_root/migrations/20260727_remove_unused_aircraft_curation_runs.sqlite.sql"

test "$(sqlite3 "$cleanup_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='aircraft_curation_interaction_runs'")" = "0"
test "$(sqlite3 "$cleanup_database" \
  "SELECT count(*) FROM pragma_table_info('aircraft_identity_decisions') WHERE name='interaction_run_id'")" = "0"
test "$(sqlite3 "$cleanup_database" \
  "SELECT count(*) FROM pragma_table_info('aircraft_reference_profile_proposals') WHERE name='interaction_run_id'")" = "0"
test "$(sqlite3 "$cleanup_database" \
  "SELECT count(*) FROM aircraft_identity_decisions")" = "11"
test "$(sqlite3 "$cleanup_database" \
  "SELECT count(*) FROM aircraft_reference_profile_proposals")" = "1"
test "$(sqlite3 "$cleanup_database" \
  "SELECT count(*) FROM aircraft_identity_decision_claims")" = "11"
test -z "$(sqlite3 "$cleanup_database" "PRAGMA foreign_key_check")"

sqlite3 -bail "$test_database" "PRAGMA foreign_key_check"
echo "aircraft reference catalog SQLite schema contract passed"
