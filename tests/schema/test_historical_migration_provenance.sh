#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
database_url="${AIRCOST_TEST_POSTGRES_URL:?AIRCOST_TEST_POSTGRES_URL is required}"
export PGOPTIONS='-c client_min_messages=warning'
sentinel_installed_at='2000-01-01 00:00:00'
hostile_fingerprint='ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff'
sqlite_database="$(mktemp /tmp/aircost-historical-provenance.XXXXXX.sqlite3)"
trap 'rm -f "$sqlite_database"' EXIT

strict_migrations=(
  20260725_identity_deduplication_postconditions
  20260725_listing_aircraft_identity
  20260726_listing_aircraft_compatibility_projection
  20260728_aircraft_identity_no_supported_selection
  20260729_aircraft_catalog_retrieval_keys
  20260730_aircraft_tcds_make_lineage
  20260731_avionics_human_reviewed_consolidation
  20260801_avionics_authoritative_source_origins
  20260804_avionics_grounded_evidence_refresh
  20260805_listing_avionics_association_corroborations
  20260806_listing_avionics_collision_closure
  20260807_avionics_product_reuse_v2
  20260808_avionics_descriptive_consolidation
  20260809_listing_verification_runs
  20260810_avionics_grounded_exact_model_consolidation
  20260819_aircraft_listing_identity_corrections
  20260821_aircraft_visual_source_corrections
  20260821_avionics_approved_concrete_model
  20260824_avionics_generic_feature_labels
)
object_attested_migrations=(
  20260825_listing_avionics_grounded_capabilities
)
transition_migrations=(
  20260802_default_avionics_candidate_quarantine
  20260803_avionics_product_reuse_attestations
)
all_migrations=(
  "${strict_migrations[@]}"
  "${object_attested_migrations[@]}"
  "${transition_migrations[@]}"
)
specialized_postgres_migrations=(
  20260819_faa_reference_reachability
  20260819_listing_replay_runs
  20260819_reference_catalog_cutover
  20260820_faa_record_hash_domain
)
all_postgres_migrations=(
  "${all_migrations[@]}"
  "${specialized_postgres_migrations[@]}"
)

extract_marker_insert() {
  awk '
    /^INSERT INTO (public[.])?schema_migration_contracts/ { capture = 1 }
    capture { print }
    capture && /^ON CONFLICT / { conflict = 1 }
    capture && conflict && /;$/ { exit }
  ' "$1"
}

extract_sqlite_guard() {
  awk '
    /^CREATE TEMP TABLE .*guard/ { capture = 1 }
    capture { print }
    capture && /^DROP TABLE .*guard;$/ { exit }
  ' "$1"
}

extract_postgres_guard() {
  awk '
    /^DO [$]migration(_contract)?_guard[$]$/ { capture = 1 }
    capture { print }
    capture && /^[$]migration(_contract)?_guard[$];$/ { exit }
  ' "$1"
}

extract_postgres_ledger_lock() {
  awk '
    /^LOCK TABLE ONLY public[.]schema_migration_contracts$/ { capture = 1 }
    capture { print }
    capture && /;$/ { exit }
  ' "$1"
}

extract_postgres_search_path() {
  grep -m1 '^SET LOCAL search_path = public, pg_catalog, pg_temp;$' "$1"
}

contract_version() {
  extract_marker_insert "$1" | awk -v migration="$2" '
    index($0, "\047" migration "\047") {
      getline
      gsub(/[^0-9]/, "")
      print
      exit
    }
  '
}

contract_fingerprint() {
  extract_marker_insert "$1" | awk -v migration="$2" '
    index($0, "\047" migration "\047") {
      getline
      getline
      gsub(/[^0-9a-f]/, "")
      print
      exit
    }
  '
}

contract_tuple() {
  local migration="$1"
  local backend="$2"
  local file="$repository_root/migrations/$migration.$backend.sql"
  local version fingerprint
  version="$(contract_version "$file" "$migration")"
  fingerprint="$(contract_fingerprint "$file" "$migration")"
  [[ "$version" =~ ^[0-9]+$ ]]
  [[ "$fingerprint" =~ ^[0-9a-f]{64}$ ]]
  printf '%s:%s' "$version" "$fingerprint"
}

assert_postgres_ledger_lock_placement() {
  local file="$1"
  local transaction_line search_path_line create_line lock_line first_guard_line marker_line
  local lock_statement
  test "$(grep -c '^SET LOCAL search_path = public, pg_catalog, pg_temp;$' "$file")" = 1
  test "$(grep -c '^LOCK TABLE ONLY public[.]schema_migration_contracts$' "$file")" = 1
  transaction_line="$(grep -n -m1 '^BEGIN;$' "$file" | cut -d: -f1)"
  search_path_line="$(grep -n -m1 \
    '^SET LOCAL search_path = public, pg_catalog, pg_temp;$' "$file" | cut -d: -f1)"
  create_line="$(grep -n -m1 \
    '^CREATE TABLE IF NOT EXISTS \(public[.]\)\?schema_migration_contracts' \
    "$file" | cut -d: -f1 || true)"
  lock_line="$(grep -n -m1 '^LOCK TABLE ONLY public[.]schema_migration_contracts$' \
    "$file" | cut -d: -f1)"
  first_guard_line="$(grep -n -m1 '^DO [$]' "$file" | cut -d: -f1)"
  marker_line="$(grep -n -m1 \
    '^INSERT INTO public[.]schema_migration_contracts' "$file" | \
    cut -d: -f1)"
  lock_statement="$(sed -n "${lock_line},$((lock_line + 1))p" "$file")"
  [[ "$lock_statement" == $'LOCK TABLE ONLY public.schema_migration_contracts\nIN SHARE ROW EXCLUSIVE MODE;' ]]
  (( transaction_line < search_path_line ))
  if [[ -n "$create_line" ]]; then
    (( search_path_line < create_line ))
    (( create_line < lock_line ))
  else
    (( search_path_line < lock_line ))
  fi
  (( lock_line < first_guard_line ))
  (( first_guard_line < marker_line ))
}

assert_postgres_parent_only_ledger_references() {
  local file="$1"
  local unexpected_references

  # INSERT has no ONLY syntax and already targets exactly the named table.
  # Reads and locks require ONLY so inherited descendants cannot participate.
  test "$(grep -c '^INSERT INTO public[.]schema_migration_contracts' "$file")" = 1
  unexpected_references="$(
    grep -n 'schema_migration_contracts' "$file" |
      grep -Ev \
        'CREATE TABLE IF NOT EXISTS public[.]schema_migration_contracts|LOCK TABLE ONLY public[.]schema_migration_contracts|FROM ONLY public[.]schema_migration_contracts|INSERT INTO public[.]schema_migration_contracts' || true
  )"
  if [[ -n "$unexpected_references" ]]; then
    printf 'non-parent-only ledger reference in %s:\n%s\n' \
      "$file" "$unexpected_references" >&2
    return 1
  fi
}

execute_sql() {
  local backend="$1"
  local statement="$2"
  if [[ "$backend" == sqlite ]]; then
    sqlite3 -bail "$sqlite_database" "$statement"
  else
    psql "$database_url" -v ON_ERROR_STOP=1 -q -c "$statement"
  fi
}

query_sql() {
  local backend="$1"
  local statement="$2"
  if [[ "$backend" == sqlite ]]; then
    sqlite3 -batch -noheader "$sqlite_database" "$statement"
  else
    psql "$database_url" -v ON_ERROR_STOP=1 -qAt -c "$statement"
  fi
}

execute_guard_then_domain_probe() {
  local backend="$1"
  local file="$2"
  if [[ "$backend" == sqlite ]]; then
    {
      extract_sqlite_guard "$file"
      echo 'UPDATE historical_migration_domain_probe SET mutation_count = mutation_count + 1 WHERE id = 1;'
    } | sqlite3 -bail "$sqlite_database"
  else
    {
      extract_postgres_guard "$file"
      echo 'UPDATE historical_migration_domain_probe SET mutation_count = mutation_count + 1 WHERE id = 1;'
    } | psql "$database_url" -v ON_ERROR_STOP=1 -q
  fi
}

execute_contract() {
  local backend="$1"
  local file="$2"
  if [[ "$backend" == sqlite ]]; then
    { extract_sqlite_guard "$file"; extract_marker_insert "$file"; } | \
      sqlite3 -bail "$sqlite_database"
  else
    { extract_postgres_guard "$file"; extract_marker_insert "$file"; } | \
      psql "$database_url" -v ON_ERROR_STOP=1 -q
  fi
}

execute_postgres_transaction_contract() {
  local file="$1"
  {
    echo 'BEGIN;'
    extract_postgres_search_path "$file"
    extract_postgres_ledger_lock "$file"
    extract_postgres_guard "$file"
    echo 'UPDATE historical_migration_domain_probe SET mutation_count = mutation_count + 1 WHERE id = 1;'
    extract_marker_insert "$file"
    echo 'COMMIT;'
  } | psql "$database_url" -v ON_ERROR_STOP=1 -q
}

execute_postgres_hostile_search_path_contract() {
  local file="$1"
  {
    echo 'SET search_path = historical_provenance_attacker, public;'
    echo 'BEGIN;'
    extract_postgres_search_path "$file"
    extract_postgres_ledger_lock "$file"
    extract_postgres_guard "$file"
    echo 'UPDATE historical_migration_domain_probe SET mutation_count = mutation_count + 1 WHERE id = 1;'
    extract_marker_insert "$file"
    echo 'COMMIT;'
    echo 'SHOW search_path;'
  } | psql "$database_url" -v ON_ERROR_STOP=1 -qAt
}

expect_guard_failure() {
  local backend="$1"
  local file="$2"
  local domain_probe_before domain_probe_after
  domain_probe_before="$(query_sql "$backend" \
    'SELECT mutation_count FROM historical_migration_domain_probe WHERE id=1')"
  if execute_guard_then_domain_probe "$backend" "$file" >/dev/null 2>&1; then
    echo "$backend guard unexpectedly accepted $(basename "$file")" >&2
    exit 1
  fi
  domain_probe_after="$(query_sql "$backend" \
    'SELECT mutation_count FROM historical_migration_domain_probe WHERE id=1')"
  if [[ "$domain_probe_after" != "$domain_probe_before" ]]; then
    echo "$backend guard changed domain state for $(basename "$file")" >&2
    exit 1
  fi
}

# Every receipt-bearing PostgreSQL migration has the same transaction-local
# namespace and ledger-serialization envelope. The four specialized migrations
# keep their dedicated state-shape tests outside this general behavior matrix.
mapfile -t receipt_postgres_migrations < <(
  grep -El '^INSERT INTO public[.]schema_migration_contracts' \
    "$repository_root"/migrations/*.postgres.sql |
    sed -E 's|.*/([^/]+)[.]postgres[.]sql|\1|' | sort
)
test "${#receipt_postgres_migrations[@]}" = 26
test "${#all_postgres_migrations[@]}" = 26
diff -u \
  <(printf '%s\n' "${all_postgres_migrations[@]}" | sort) \
  <(printf '%s\n' "${receipt_postgres_migrations[@]}")
for migration in "${all_postgres_migrations[@]}"; do
  file="$repository_root/migrations/$migration.postgres.sql"
  assert_postgres_ledger_lock_placement "$file"
  assert_postgres_parent_only_ledger_references "$file"
done

# Strict receipts are literal conflict no-ops. Object-attested migrations share
# that receipt contract, but their guards require the complete canonical domain
# shape and are exercised by their dedicated backend tests. Only the two
# explicit v1-to-v2 migrations may assign installed_at.
for migration in "${strict_migrations[@]}" "${object_attested_migrations[@]}"; do
  for backend in sqlite postgres; do
    file="$repository_root/migrations/$migration.$backend.sql"
    marker_insert="$(extract_marker_insert "$file")"
    if [[ "$backend" == sqlite ]]; then
      guard="$(extract_sqlite_guard "$file")"
    else
      guard="$(extract_postgres_guard "$file")"
    fi
    [[ -n "$guard" ]]
    [[ "$guard" == *"$migration"* ]]
    if [[ "$backend" == postgres ]]; then
      [[ "$guard" == *"contract_version IS DISTINCT FROM"* ]]
      [[ "$guard" == *"contract_fingerprint IS DISTINCT FROM"* ]]
      assert_postgres_ledger_lock_placement "$file"
    fi
    [[ "$marker_insert" == *"ON CONFLICT (migration_name) DO NOTHING;"* ]]
    [[ "$marker_insert" != *"installed_at ="* ]]
  done
done

mapfile -t installed_at_rewriters < <(
  grep -El 'installed_at = (EXCLUDED|excluded)[.]installed_at' \
    "$repository_root"/migrations/*.sql | sort
)
test "${#installed_at_rewriters[@]}" = 4
for migration in "${transition_migrations[@]}"; do
  for backend in sqlite postgres; do
    file="$repository_root/migrations/$migration.$backend.sql"
    printf '%s\n' "${installed_at_rewriters[@]}" | grep -Fxq "$file"
    marker_insert="$(extract_marker_insert "$file")"
    [[ "$marker_insert" == *"installed_at ="* ]]
    [[ "$marker_insert" == *"contract_version = 1"* ]]
    if [[ "$backend" == postgres ]]; then
      guard="$(extract_postgres_guard "$file")"
      [[ "$guard" == *"contract_version IS NOT DISTINCT FROM"* ]]
      [[ "$guard" == *"contract_fingerprint IS NOT DISTINCT FROM"* ]]
      assert_postgres_ledger_lock_placement "$file"
    fi
  done
done

initialize_backend() {
  local backend="$1"
  if [[ "$backend" == sqlite ]]; then
    sqlite3 -bail "$sqlite_database" <<'SQL'
CREATE TABLE schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER CHECK (contract_version > 0),
  contract_fingerprint TEXT,
  installed_at TEXT NOT NULL,
  CHECK (length(contract_fingerprint) = 64),
  CHECK (contract_fingerprint = lower(contract_fingerprint)),
  CHECK (contract_fingerprint NOT GLOB '*[^0-9a-f]*')
);
CREATE TABLE avionics_catalog_consolidation_guard (
  authorization_id INTEGER PRIMARY KEY
);
CREATE TABLE historical_migration_domain_probe (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  mutation_count INTEGER NOT NULL
);
INSERT INTO historical_migration_domain_probe VALUES (1, 0);
SQL
  else
    psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
DROP SCHEMA public CASCADE;
CREATE SCHEMA public;
CREATE TABLE public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version BIGINT CHECK (contract_version > 0),
  contract_fingerprint TEXT
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL
);
CREATE TABLE public.avionics_catalog_consolidation_guard (
  authorization_id BIGINT PRIMARY KEY
);
CREATE TABLE public.historical_migration_domain_probe (
  id BIGINT PRIMARY KEY CHECK (id = 1),
  mutation_count BIGINT NOT NULL
);
INSERT INTO public.historical_migration_domain_probe VALUES (1, 0);
SQL
  fi

  for migration in "${all_migrations[@]}"; do
    tuple="$(contract_tuple "$migration" "$backend")"
    version="${tuple%%:*}"
    fingerprint="${tuple#*:}"
    execute_sql "$backend" \
      "INSERT INTO schema_migration_contracts VALUES ('$migration',$version,'$fingerprint','$sentinel_installed_at')"
  done
}

test_strict_contracts() {
  local backend="$1"
  for migration in "${strict_migrations[@]}"; do
    file="$repository_root/migrations/$migration.$backend.sql"
    tuple="$(contract_tuple "$migration" "$backend")"
    version="${tuple%%:*}"
    fingerprint="${tuple#*:}"

    execute_contract "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "$sentinel_installed_at"

    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=99 WHERE migration_name='$migration'"
    expect_guard_failure "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT contract_version||':'||installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "99:$sentinel_installed_at"
    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=$version WHERE migration_name='$migration'"

    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_fingerprint='$hostile_fingerprint' WHERE migration_name='$migration'"
    expect_guard_failure "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT contract_fingerprint||':'||installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "$hostile_fingerprint:$sentinel_installed_at"
    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_fingerprint='$fingerprint' WHERE migration_name='$migration'"

    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=NULL WHERE migration_name='$migration'"
    expect_guard_failure "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT count(*) FROM schema_migration_contracts WHERE migration_name='$migration' AND contract_version IS NULL AND contract_fingerprint='$fingerprint' AND installed_at='$sentinel_installed_at'")" = 1
    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=$version,contract_fingerprint=NULL WHERE migration_name='$migration'"
    expect_guard_failure "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT count(*) FROM schema_migration_contracts WHERE migration_name='$migration' AND contract_version=$version AND contract_fingerprint IS NULL AND installed_at='$sentinel_installed_at'")" = 1
    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_fingerprint='$fingerprint' WHERE migration_name='$migration'"

    execute_sql "$backend" \
      "DELETE FROM schema_migration_contracts WHERE migration_name='$migration'"
    execute_contract "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT contract_version||':'||contract_fingerprint FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "$tuple"
    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET installed_at='$sentinel_installed_at' WHERE migration_name='$migration'"
  done
}

test_transition_contracts() {
  local backend="$1"
  for migration in "${transition_migrations[@]}"; do
    file="$repository_root/migrations/$migration.$backend.sql"
    tuple="$(contract_tuple "$migration" "$backend")"
    current_version="${tuple%%:*}"
    current_fingerprint="${tuple#*:}"
    if [[ "$migration" == 20260802_default_avionics_candidate_quarantine ]]; then
      old_fingerprint='b50683c27b244cadf3cf88b226665f79051f678df9b30e0d01d0ca261464581f'
    else
      old_fingerprint='edfe54b792fa91890bd1708ad23b58f4fd9f9c717b42147f5edb948d67ccd837'
    fi

    execute_contract "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "$sentinel_installed_at"

    execute_sql "$backend" \
      "DELETE FROM schema_migration_contracts WHERE migration_name='$migration'"
    execute_contract "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT contract_version||':'||contract_fingerprint FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "$current_version:$current_fingerprint"
    test "$(query_sql "$backend" \
      "SELECT installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" != \
      "$sentinel_installed_at"

    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=1,contract_fingerprint='$old_fingerprint',installed_at='$sentinel_installed_at' WHERE migration_name='$migration'"
    execute_contract "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT contract_version||':'||contract_fingerprint FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "$current_version:$current_fingerprint"
    test "$(query_sql "$backend" \
      "SELECT installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" != \
      "$sentinel_installed_at"

    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=99,contract_fingerprint='$current_fingerprint',installed_at='$sentinel_installed_at' WHERE migration_name='$migration'"
    expect_guard_failure "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT contract_version||':'||installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "99:$sentinel_installed_at"

    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=1,contract_fingerprint='$hostile_fingerprint' WHERE migration_name='$migration'"
    expect_guard_failure "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT contract_version||':'||contract_fingerprint||':'||installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "1:$hostile_fingerprint:$sentinel_installed_at"

    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=$current_version,contract_fingerprint='$hostile_fingerprint' WHERE migration_name='$migration'"
    expect_guard_failure "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT contract_version||':'||contract_fingerprint||':'||installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
      "$current_version:$hostile_fingerprint:$sentinel_installed_at"

    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=NULL,contract_fingerprint='$current_fingerprint' WHERE migration_name='$migration'"
    expect_guard_failure "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT count(*) FROM schema_migration_contracts WHERE migration_name='$migration' AND contract_version IS NULL AND contract_fingerprint='$current_fingerprint' AND installed_at='$sentinel_installed_at'")" = 1
    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=$current_version,contract_fingerprint=NULL WHERE migration_name='$migration'"
    expect_guard_failure "$backend" "$file"
    test "$(query_sql "$backend" \
      "SELECT count(*) FROM schema_migration_contracts WHERE migration_name='$migration' AND contract_version=$current_version AND contract_fingerprint IS NULL AND installed_at='$sentinel_installed_at'")" = 1
    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=$current_version,contract_fingerprint='$current_fingerprint' WHERE migration_name='$migration'"
  done
}

wait_for_postgres_writer_lock() {
  local application_name="$1"
  local attempt lock_count
  for ((attempt = 0; attempt < 100; attempt += 1)); do
    lock_count="$(query_sql postgres \
      "SELECT count(*) FROM pg_catalog.pg_locks lock_row JOIN pg_catalog.pg_stat_activity activity ON activity.pid=lock_row.pid WHERE activity.application_name='$application_name' AND lock_row.relation='public.schema_migration_contracts'::regclass AND lock_row.mode='RowExclusiveLock' AND lock_row.granted")"
    if [[ "$lock_count" = 1 ]]; then
      return
    fi
    sleep 0.05
  done
  echo "PostgreSQL writer $application_name never acquired the ledger lock" >&2
  return 1
}

start_postgres_receipt_writer() {
  local application_name="$1"
  local mutation="$2"
  {
    printf "SET application_name='%s';\n" "$application_name"
    echo 'BEGIN;'
    printf '%s;\n' "$mutation"
    echo 'SELECT pg_sleep(2);'
    echo 'COMMIT;'
  } | psql "$database_url" -v ON_ERROR_STOP=1 -q >/dev/null &
  postgres_writer_pid=$!
  wait_for_postgres_writer_lock "$application_name"
}

expect_postgres_raced_rejection() {
  local file="$1"
  local expected_error="$2"
  local domain_probe_before domain_probe_after rejection
  domain_probe_before="$(query_sql postgres \
    'SELECT mutation_count FROM historical_migration_domain_probe WHERE id=1')"
  if rejection="$(execute_postgres_transaction_contract "$file" 2>&1)"; then
    echo "PostgreSQL raced guard unexpectedly accepted $(basename "$file")" >&2
    return 1
  fi
  wait "$postgres_writer_pid"
  [[ "$rejection" == *"$expected_error"* ]]
  domain_probe_after="$(query_sql postgres \
    'SELECT mutation_count FROM historical_migration_domain_probe WHERE id=1')"
  test "$domain_probe_after" = "$domain_probe_before"
}

test_postgres_concurrent_receipt_writers() {
  local migration file tuple version fingerprint installed_at_before

  migration=20260804_avionics_grounded_evidence_refresh
  file="$repository_root/migrations/$migration.postgres.sql"
  tuple="$(contract_tuple "$migration" postgres)"
  version="${tuple%%:*}"
  fingerprint="${tuple#*:}"
  execute_sql postgres \
    "UPDATE schema_migration_contracts SET contract_version=$version,contract_fingerprint='$fingerprint',installed_at='$sentinel_installed_at' WHERE migration_name='$migration'"
  start_postgres_receipt_writer strict_existing_writer \
    "UPDATE schema_migration_contracts SET contract_version=99 WHERE migration_name='$migration'"
  expect_postgres_raced_rejection "$file" \
    'installed avionics grounded-evidence refresh migration has a different contract'
  test "$(query_sql postgres \
    "SELECT contract_version||':'||installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
    "99:$sentinel_installed_at"

  migration=20260805_listing_avionics_association_corroborations
  file="$repository_root/migrations/$migration.postgres.sql"
  execute_sql postgres \
    "DELETE FROM schema_migration_contracts WHERE migration_name='$migration'"
  start_postgres_receipt_writer strict_absent_writer \
    "INSERT INTO schema_migration_contracts VALUES ('$migration',99,'$hostile_fingerprint','$sentinel_installed_at')"
  expect_postgres_raced_rejection "$file" \
    'installed listing-avionics corroboration migration has a different contract'
  test "$(query_sql postgres \
    "SELECT contract_version||':'||contract_fingerprint||':'||installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
    "99:$hostile_fingerprint:$sentinel_installed_at"

  migration=20260802_default_avionics_candidate_quarantine
  file="$repository_root/migrations/$migration.postgres.sql"
  tuple="$(contract_tuple "$migration" postgres)"
  version="${tuple%%:*}"
  fingerprint="${tuple#*:}"
  execute_sql postgres \
    "DELETE FROM schema_migration_contracts WHERE migration_name='$migration'"
  execute_sql postgres \
    'UPDATE historical_migration_domain_probe SET mutation_count=0 WHERE id=1'
  start_postgres_receipt_writer transition_predecessor_writer \
    "INSERT INTO schema_migration_contracts VALUES ('$migration',1,'b50683c27b244cadf3cf88b226665f79051f678df9b30e0d01d0ca261464581f','$sentinel_installed_at')"
  execute_postgres_transaction_contract "$file"
  wait "$postgres_writer_pid"
  test "$(query_sql postgres \
    'SELECT mutation_count FROM historical_migration_domain_probe WHERE id=1')" = 1
  test "$(query_sql postgres \
    "SELECT contract_version||':'||contract_fingerprint FROM schema_migration_contracts WHERE migration_name='$migration'")" = \
    "$version:$fingerprint"
  installed_at_before="$(query_sql postgres \
    "SELECT installed_at FROM schema_migration_contracts WHERE migration_name='$migration'")"
  test "$installed_at_before" != "$sentinel_installed_at"
}

test_postgres_hostile_search_path_is_transaction_local() {
  local migration file tuple version fingerprint caller_search_path
  migration=20260804_avionics_grounded_evidence_refresh
  file="$repository_root/migrations/$migration.postgres.sql"
  tuple="$(contract_tuple "$migration" postgres)"
  version="${tuple%%:*}"
  fingerprint="${tuple#*:}"

  execute_sql postgres \
    'DROP SCHEMA IF EXISTS historical_provenance_attacker CASCADE; CREATE SCHEMA historical_provenance_attacker'
  execute_sql postgres \
    'CREATE TABLE historical_provenance_attacker.schema_migration_contracts (LIKE public.schema_migration_contracts INCLUDING ALL); CREATE TABLE historical_provenance_attacker.historical_migration_domain_probe (LIKE public.historical_migration_domain_probe INCLUDING ALL); INSERT INTO historical_provenance_attacker.historical_migration_domain_probe VALUES (1,0)'
  execute_sql postgres \
    "INSERT INTO historical_provenance_attacker.schema_migration_contracts VALUES ('$migration',99,'$hostile_fingerprint','$sentinel_installed_at')"
  execute_sql postgres \
    "UPDATE public.schema_migration_contracts SET contract_version=$version,contract_fingerprint='$fingerprint',installed_at='$sentinel_installed_at' WHERE migration_name='$migration'; UPDATE public.historical_migration_domain_probe SET mutation_count=0 WHERE id=1"

  caller_search_path="$(execute_postgres_hostile_search_path_contract "$file")"
  test "$caller_search_path" = 'historical_provenance_attacker, public'
  test "$(query_sql postgres \
    'SELECT mutation_count FROM public.historical_migration_domain_probe WHERE id=1')" = 1
  test "$(query_sql postgres \
    'SELECT mutation_count FROM historical_provenance_attacker.historical_migration_domain_probe WHERE id=1')" = 0
  test "$(query_sql postgres \
    "SELECT contract_version||':'||contract_fingerprint||':'||installed_at FROM public.schema_migration_contracts WHERE migration_name='$migration'")" = \
    "$version:$fingerprint:$sentinel_installed_at"
  test "$(query_sql postgres \
    "SELECT contract_version||':'||contract_fingerprint||':'||installed_at FROM historical_provenance_attacker.schema_migration_contracts WHERE migration_name='$migration'")" = \
    "99:$hostile_fingerprint:$sentinel_installed_at"
  execute_sql postgres 'DROP SCHEMA historical_provenance_attacker CASCADE'
}

test_postgres_temporary_shadows_are_searched_last() {
  local migration file tuple version fingerprint session_output
  migration=20260804_avionics_grounded_evidence_refresh
  file="$repository_root/migrations/$migration.postgres.sql"
  tuple="$(contract_tuple "$migration" postgres)"
  version="${tuple%%:*}"
  fingerprint="${tuple#*:}"

  session_output="$({
    echo 'CREATE TEMP TABLE schema_migration_contracts (LIKE public.schema_migration_contracts INCLUDING ALL);'
    echo 'CREATE TEMP TABLE historical_migration_domain_probe (LIKE public.historical_migration_domain_probe INCLUDING ALL);'
    echo 'INSERT INTO pg_temp.historical_migration_domain_probe VALUES (1,0);'
    printf "INSERT INTO pg_temp.schema_migration_contracts VALUES ('%s',%s,'%s','%s');\n" \
      "$migration" "$version" "$fingerprint" "$sentinel_installed_at"
    printf "UPDATE public.schema_migration_contracts SET contract_version=NULL,contract_fingerprint='%s',installed_at='%s' WHERE migration_name='%s';\n" \
      "$fingerprint" "$sentinel_installed_at" "$migration"
    echo 'UPDATE public.historical_migration_domain_probe SET mutation_count=0 WHERE id=1;'
    echo 'BEGIN;'
    extract_postgres_search_path "$file"
    extract_postgres_ledger_lock "$file"
    extract_postgres_guard "$file"
    echo 'UPDATE historical_migration_domain_probe SET mutation_count = mutation_count + 1 WHERE id = 1;'
    extract_marker_insert "$file"
    echo 'ROLLBACK;'
    printf "SELECT 'reject_public_receipt|' || COALESCE(contract_version::text,'NULL') || '|' || COALESCE(contract_fingerprint,'NULL') || '|' || installed_at FROM public.schema_migration_contracts WHERE migration_name='%s';\n" "$migration"
    printf "SELECT 'reject_temp_receipt|' || COALESCE(contract_version::text,'NULL') || '|' || COALESCE(contract_fingerprint,'NULL') || '|' || installed_at FROM pg_temp.schema_migration_contracts WHERE migration_name='%s';\n" "$migration"
    echo "SELECT 'reject_public_probe|' || mutation_count FROM public.historical_migration_domain_probe WHERE id=1;"
    echo "SELECT 'reject_temp_probe|' || mutation_count FROM pg_temp.historical_migration_domain_probe WHERE id=1;"

    printf "UPDATE public.schema_migration_contracts SET contract_version=%s,contract_fingerprint='%s',installed_at='%s' WHERE migration_name='%s';\n" \
      "$version" "$fingerprint" "$sentinel_installed_at" "$migration"
    printf "UPDATE pg_temp.schema_migration_contracts SET contract_version=99,contract_fingerprint='%s',installed_at='%s' WHERE migration_name='%s';\n" \
      "$hostile_fingerprint" "$sentinel_installed_at" "$migration"
    echo 'UPDATE public.historical_migration_domain_probe SET mutation_count=0 WHERE id=1;'
    echo 'UPDATE pg_temp.historical_migration_domain_probe SET mutation_count=0 WHERE id=1;'
    echo 'BEGIN;'
    extract_postgres_search_path "$file"
    extract_postgres_ledger_lock "$file"
    extract_postgres_guard "$file"
    echo 'UPDATE historical_migration_domain_probe SET mutation_count = mutation_count + 1 WHERE id = 1;'
    extract_marker_insert "$file"
    echo 'COMMIT;'
    printf "SELECT 'accept_public_receipt|' || contract_version || '|' || contract_fingerprint || '|' || installed_at FROM public.schema_migration_contracts WHERE migration_name='%s';\n" "$migration"
    printf "SELECT 'accept_temp_receipt|' || contract_version || '|' || contract_fingerprint || '|' || installed_at FROM pg_temp.schema_migration_contracts WHERE migration_name='%s';\n" "$migration"
    echo "SELECT 'accept_public_probe|' || mutation_count FROM public.historical_migration_domain_probe WHERE id=1;"
    echo "SELECT 'accept_temp_probe|' || mutation_count FROM pg_temp.historical_migration_domain_probe WHERE id=1;"
  } | psql "$database_url" -v ON_ERROR_STOP=0 -qAt 2>&1)"

  [[ "$session_output" == *'installed avionics grounded-evidence refresh migration has a different contract'* ]]
  [[ "$session_output" == *"reject_public_receipt|NULL|$fingerprint|$sentinel_installed_at"* ]]
  [[ "$session_output" == *"reject_temp_receipt|$version|$fingerprint|$sentinel_installed_at"* ]]
  [[ "$session_output" == *'reject_public_probe|0'* ]]
  [[ "$session_output" == *'reject_temp_probe|0'* ]]
  [[ "$session_output" == *"accept_public_receipt|$version|$fingerprint|$sentinel_installed_at"* ]]
  [[ "$session_output" == *"accept_temp_receipt|99|$hostile_fingerprint|$sentinel_installed_at"* ]]
  [[ "$session_output" == *'accept_public_probe|1'* ]]
  [[ "$session_output" == *'accept_temp_probe|0'* ]]
}

test_postgres_inherited_receipts_cannot_spoof_parent_ledger() {
  local migration file tuple version fingerprint
  migration=20260804_avionics_grounded_evidence_refresh
  file="$repository_root/migrations/$migration.postgres.sql"
  tuple="$(contract_tuple "$migration" postgres)"
  version="${tuple%%:*}"
  fingerprint="${tuple#*:}"

  execute_sql postgres \
    'DROP TABLE IF EXISTS public.schema_migration_contracts_inheritance_probe; CREATE TABLE public.schema_migration_contracts_inheritance_probe () INHERITS (public.schema_migration_contracts)'

  # A hostile descendant receipt must not turn an exact canonical receipt into
  # a rejection, and the canonical no-op must preserve its installation time.
  execute_sql postgres \
    "UPDATE ONLY public.schema_migration_contracts SET contract_version=$version,contract_fingerprint='$fingerprint',installed_at='$sentinel_installed_at' WHERE migration_name='$migration'; DELETE FROM public.schema_migration_contracts_inheritance_probe; INSERT INTO public.schema_migration_contracts_inheritance_probe VALUES ('$migration',99,'$hostile_fingerprint','$sentinel_installed_at'); UPDATE public.historical_migration_domain_probe SET mutation_count=0 WHERE id=1"
  execute_postgres_transaction_contract "$file"
  test "$(query_sql postgres \
    'SELECT mutation_count FROM public.historical_migration_domain_probe WHERE id=1')" = 1
  test "$(query_sql postgres \
    "SELECT contract_version||':'||contract_fingerprint||':'||installed_at FROM ONLY public.schema_migration_contracts WHERE migration_name='$migration'")" = \
    "$version:$fingerprint:$sentinel_installed_at"
  test "$(query_sql postgres \
    "SELECT contract_version||':'||contract_fingerprint||':'||installed_at FROM public.schema_migration_contracts_inheritance_probe WHERE migration_name='$migration'")" = \
    "99:$hostile_fingerprint:$sentinel_installed_at"

  # Conversely, an exact descendant receipt must not hide or heal a hostile
  # canonical receipt. The guard rejects before the domain probe or either row
  # can change.
  execute_sql postgres \
    "UPDATE ONLY public.schema_migration_contracts SET contract_version=99,contract_fingerprint='$hostile_fingerprint',installed_at='$sentinel_installed_at' WHERE migration_name='$migration'; UPDATE public.schema_migration_contracts_inheritance_probe SET contract_version=$version,contract_fingerprint='$fingerprint' WHERE migration_name='$migration'; UPDATE public.historical_migration_domain_probe SET mutation_count=0 WHERE id=1"
  expect_guard_failure postgres "$file"
  test "$(query_sql postgres \
    'SELECT mutation_count FROM public.historical_migration_domain_probe WHERE id=1')" = 0
  test "$(query_sql postgres \
    "SELECT contract_version||':'||contract_fingerprint||':'||installed_at FROM ONLY public.schema_migration_contracts WHERE migration_name='$migration'")" = \
    "99:$hostile_fingerprint:$sentinel_installed_at"
  test "$(query_sql postgres \
    "SELECT contract_version||':'||contract_fingerprint||':'||installed_at FROM public.schema_migration_contracts_inheritance_probe WHERE migration_name='$migration'")" = \
    "$version:$fingerprint:$sentinel_installed_at"

  execute_sql postgres \
    'DROP TABLE public.schema_migration_contracts_inheritance_probe'
}

for backend in sqlite postgres; do
  initialize_backend "$backend"
  test_strict_contracts "$backend"
  test_transition_contracts "$backend"
done

test_postgres_concurrent_receipt_writers
test_postgres_hostile_search_path_is_transaction_local
test_postgres_temporary_shadows_are_searched_last
test_postgres_inherited_receipts_cannot_spoof_parent_ledger

echo 'Historical migration provenance contracts passed'
