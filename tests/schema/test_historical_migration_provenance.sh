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
  20260807_avionics_product_reuse_v2
  20260808_avionics_descriptive_consolidation
  20260809_listing_verification_runs
  20260810_avionics_grounded_exact_model_consolidation
  20260818_listing_avionics_association_authorizations
  20260818_listing_avionics_authorization_hash_domain_reset
  20260819_aircraft_listing_identity_corrections
)
transition_migrations=(
  20260802_default_avionics_candidate_quarantine
  20260803_avionics_product_reuse_attestations
)
support_migrations=(
  20260804_avionics_grounded_evidence_refresh
  20260805_listing_avionics_association_corroborations
  20260806_listing_avionics_collision_closure
)
all_migrations=(
  "${strict_migrations[@]}"
  "${transition_migrations[@]}"
  "${support_migrations[@]}"
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

execute_guard() {
  local backend="$1"
  local file="$2"
  if [[ "$backend" == sqlite ]]; then
    extract_sqlite_guard "$file" | sqlite3 -bail "$sqlite_database"
  else
    extract_postgres_guard "$file" | \
      psql "$database_url" -v ON_ERROR_STOP=1 -q
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

expect_guard_failure() {
  local backend="$1"
  local file="$2"
  if execute_guard "$backend" "$file" >/dev/null 2>&1; then
    echo "$backend guard unexpectedly accepted $(basename "$file")" >&2
    exit 1
  fi
}

# Strict receipts are literal conflict no-ops. Only the two explicit v1-to-v2
# migrations may assign installed_at.
for migration in "${strict_migrations[@]}"; do
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
    fi
  done
done

initialize_backend() {
  local backend="$1"
  if [[ "$backend" == sqlite ]]; then
    sqlite3 -bail "$sqlite_database" <<'SQL'
CREATE TABLE schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL,
  installed_at TEXT NOT NULL,
  CHECK (length(contract_fingerprint) = 64),
  CHECK (contract_fingerprint = lower(contract_fingerprint)),
  CHECK (contract_fingerprint NOT GLOB '*[^0-9a-f]*')
);
CREATE TABLE avionics_catalog_consolidation_guard (
  authorization_id INTEGER PRIMARY KEY
);
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

    if [[ "$backend" == postgres ]]; then
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
    fi

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

    if [[ "$backend" == postgres ]]; then
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
    fi
    execute_sql "$backend" \
      "UPDATE schema_migration_contracts SET contract_version=$current_version,contract_fingerprint='$current_fingerprint' WHERE migration_name='$migration'"
  done
}

for backend in sqlite postgres; do
  initialize_backend "$backend"
  test_strict_contracts "$backend"
  test_transition_contracts "$backend"
done

echo 'Historical migration provenance contracts passed'
