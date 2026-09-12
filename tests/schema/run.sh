#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

sqlite_tests=(
  test_aircraft_catalog_retrieval_keys.sh
  test_aircraft_identity_no_supported_selection.sh
  test_aircraft_reference_catalog.sh
  test_aircraft_tcds_make_lineage.sh
  test_avionics_authoritative_source_origins.sh
  test_avionics_grounded_exact_consolidation.sh
  test_avionics_product_reuse_v2.sh
  test_avionics_verification_provenance.sh
  test_identity_deduplication_postconditions.sh
  test_listing_aircraft_compatibility_projection.sh
  test_listing_aircraft_identity.sh
  test_listing_avionics_association_corroborations.sh
  test_listing_avionics_collision_closure.sh
  test_listing_pending_reviews.sh
  test_versioned_migration_manifest.sh
)

postgres_tests=(
  test_faa_record_hash_domain.sh
  test_faa_reference_reachability_postgres.sh
  test_historical_migration_provenance.sh
)

registered_tests() {
  printf '%s\n' "${sqlite_tests[@]}" "${postgres_tests[@]}"
}

discovered_tests() {
  find "$script_directory" -maxdepth 1 -type f -name 'test_*.sh' -print \
    | while IFS= read -r test_path; do basename "$test_path"; done \
    | LC_ALL=C sort
}

check_inventory() {
  local duplicates missing unregistered
  duplicates="$(registered_tests | LC_ALL=C sort | uniq -d)"
  missing="$(comm -23 \
    <(registered_tests | LC_ALL=C sort -u) \
    <(discovered_tests))"
  unregistered="$(comm -13 \
    <(registered_tests | LC_ALL=C sort -u) \
    <(discovered_tests))"

  if [[ -n "$duplicates" || -n "$missing" || -n "$unregistered" ]]; then
    [[ -z "$duplicates" ]] || printf 'duplicate registered schema tests:\n%s\n' "$duplicates" >&2
    [[ -z "$missing" ]] || printf 'registered schema tests missing from disk:\n%s\n' "$missing" >&2
    [[ -z "$unregistered" ]] || printf 'unregistered schema tests:\n%s\n' "$unregistered" >&2
    return 1
  fi

  printf 'Schema test inventory: %d SQLite/text, %d PostgreSQL\n' \
    "${#sqlite_tests[@]}" "${#postgres_tests[@]}"
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'required command is unavailable: %s\n' "$1" >&2
    return 1
  fi
}

run_tests() {
  local test_script
  for test_script in "$@"; do
    printf '\n==> %s\n' "$test_script"
    bash "$script_directory/$test_script"
  done
}

usage() {
  printf 'usage: %s inventory|sqlite|postgres\n' "${0##*/}" >&2
}

category="${1:-}"
if [[ "$#" -ne 1 ]]; then
  usage
  exit 2
fi

check_inventory

case "$category" in
  inventory)
    ;;
  sqlite)
    require_command sqlite3
    require_command rg
    run_tests "${sqlite_tests[@]}"
    ;;
  postgres)
    : "${AIRCOST_TEST_POSTGRES_URL:?AIRCOST_TEST_POSTGRES_URL is required}"
    require_command psql
    require_command sqlite3
    run_tests "${postgres_tests[@]}"
    ;;
  *)
    usage
    exit 2
    ;;
esac
