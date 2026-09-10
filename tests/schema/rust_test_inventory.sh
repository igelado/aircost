#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
mode="${1:-verify}"

manual_fixture_tests=(
  cached_official_archive_derives_intrinsic_release_date
  controller_real_corpus_contract_audit
  downloaded_g1000_nxi_text_form_regression
  downloaded_official_oem_pdf_regressions
)

empty_postgres_contract_tests=(
  aircraft::reference::persistence::tests::postgres_publishes_and_selects_null_and_non_null_configuration_dimensions
  catalog::projection::seed::tests::postgres_round_trip_fences_writers_preserves_replay_and_rejects_rerun
  catalog::projection::seed::tests::postgres_sequence_reset_covers_identity_and_serial_owned_ids
  db::tests::diagnostic_postgres_connections_default_to_read_only
  db::tests::postgres_active_replay_freeze_rejects_truncate_cascade
  db::tests::postgres_all_receipt_timestamps_survive_two_normal_startups
  db::tests::postgres_correction_validation_rejects_altered_search_path_and_namespace
  db::tests::postgres_database_identity_recognizes_aliases_for_ordinary_role
  db::tests::postgres_faa_reference_startup_attests_exact_objects
  db::tests::postgres_generic_feature_label_migration_audits_without_model_updates
  db::tests::postgres_grounded_capability_functions_resist_hostile_search_path_and_tampering
  db::tests::postgres_inherited_child_receipt_cannot_satisfy_parent_ledger
  db::tests::postgres_late_initialization_failure_rolls_back_all_ddl
  db::tests::postgres_listing_replay_fresh_migrated_idempotent_and_hostile_search_path
  db::tests::postgres_listing_replay_installed_at_survives_two_normal_startups
  db::tests::postgres_listing_replay_rerun_rejects_unexpected_indexes_and_triggers
  db::tests::postgres_listing_replay_startup_rejects_weakened_column_constraint_and_index
  db::tests::postgres_reference_cutover_rejects_null_marker_fields_without_healing
  db::tests::postgres_replay_inventory_orders_repeatable_read_and_read_committed_writers
  db::tests::postgres_replay_ledger_rejects_invalid_state_outcome_pairings
  db::tests::postgres_startup_pins_search_path_and_ignores_attacker_shadows
  db::tests::postgres_startup_rejects_anchor_receipt_xor_and_hostile_markers_without_mutation
  db::tests::postgres_startup_rejects_noncanonical_ledger_storage_without_mutation
  db::tests::postgres_startup_rejects_same_named_noop_approved_concrete_model_function
  db::tests::postgres_startup_rejects_visual_artifact_constraint_tampering
  db::tests::postgres_startup_waits_for_writer_and_fresh_startups_serialize
  listing::replay::export::tests::postgres_export_matches_the_sqlite_readiness_contract
  listing::replay::run::tests::postgres_production_acquire_and_release_uses_backend_placeholders
  listings::tests::postgres_visual_artifact_bind_waits_for_newer_faa_snapshot_and_refuses_stale_pair
)

canonical_postgres_contract_tests=(
  db::tests::postgres_canonical_schema_passes_end_to_end_startup
  db::tests::postgres_reference_cutover_validation_rejects_adversarial_mutations
  tests::import_faa_registry_dry_run_keeps_postgres_rows_and_markers_unchanged
)

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'required command is unavailable: %s\n' "$1" >&2
    return 1
  fi
}

require_command cargo
require_command diff
require_command rg

case "$mode" in
  verify | run-postgres) ;;
  *)
    printf 'usage: bash tests/schema/rust_test_inventory.sh [verify|run-postgres]\n' >&2
    exit 2
    ;;
esac

list_ignored_tests() {
  local filter="${1:-}"
  if [[ -n "$filter" ]]; then
    cargo test --locked "$filter" -- --ignored --list
  else
    cargo test --locked -- --ignored --list
  fi
}

ignored_tests="$({
  cd "$repository_root"
  list_ignored_tests
} | sed -n 's/: test$//p' | LC_ALL=C sort -u)"

if [[ -z "$ignored_tests" ]]; then
  echo 'cargo did not report any ignored Rust tests' >&2
  exit 1
fi

postgres_tests="$(printf '%s\n' "$ignored_tests" \
  | rg 'postgres' \
  | sed 's/.*:://' \
  | LC_ALL=C sort)"
manual_tests="$(printf '%s\n' "$ignored_tests" \
  | rg -v 'postgres' \
  | sed 's/.*:://' \
  | LC_ALL=C sort)"
selected_postgres_qualified_tests="$({
  cd "$repository_root"
  list_ignored_tests postgres
} | sed -n 's/: test$//p' | LC_ALL=C sort -u)"
selected_postgres_tests="$(printf '%s\n' "$selected_postgres_qualified_tests" \
  | sed 's/.*:://' \
  | LC_ALL=C sort)"
selected_postgres_lib_tests="$({
  cd "$repository_root"
  cargo test --locked --lib postgres -- --ignored --list
} | sed -n 's/: test$//p' | LC_ALL=C sort -u)"
selected_postgres_admin_tests="$({
  cd "$repository_root"
  cargo test --locked --bin aircost-admin postgres -- --ignored --list
} | sed -n 's/: test$//p' | LC_ALL=C sort -u)"
targeted_postgres_tests="$(printf '%s\n%s\n' \
  "$selected_postgres_lib_tests" \
  "$selected_postgres_admin_tests" \
  | sed '/^$/d' \
  | LC_ALL=C sort -u)"
classified_postgres_tests="$(printf '%s\n' \
  "${empty_postgres_contract_tests[@]}" \
  "${canonical_postgres_contract_tests[@]}" \
  | LC_ALL=C sort)"
duplicate_fixture_tests="$(printf '%s\n' \
  "${empty_postgres_contract_tests[@]}" \
  "${canonical_postgres_contract_tests[@]}" \
  | LC_ALL=C sort \
  | uniq -d)"
expected_postgres_tests="$(printf '%s\n' "$classified_postgres_tests" \
  | sed 's/.*:://' \
  | LC_ALL=C sort)"
expected_manual_tests="$(printf '%s\n' "${manual_fixture_tests[@]}" | LC_ALL=C sort)"

if ! diff -u \
    <(printf '%s\n' "$expected_manual_tests") \
    <(printf '%s\n' "$manual_tests"); then
  echo 'ignored Rust tests must be PostgreSQL contracts or registered manual fixtures' >&2
  exit 1
fi

if ! diff -u \
    <(printf '%s\n' "$expected_postgres_tests") \
    <(printf '%s\n' "$postgres_tests"); then
  echo 'ignored PostgreSQL tests differ from the registered contract inventory' >&2
  exit 1
fi

if ! diff -u \
    <(printf '%s\n' "$expected_postgres_tests") \
    <(printf '%s\n' "$selected_postgres_tests"); then
  echo 'the PostgreSQL test selector does not cover the registered ignored-test set' >&2
  exit 1
fi

if ! diff -u \
    <(printf '%s\n' "$selected_postgres_qualified_tests") \
    <(printf '%s\n' "$targeted_postgres_tests"); then
  echo 'ignored PostgreSQL tests must belong to the library or aircost-admin target' >&2
  exit 1
fi

if [[ -n "$duplicate_fixture_tests" ]]; then
  printf 'PostgreSQL tests have duplicate fixture classifications:\n%s\n' \
    "$duplicate_fixture_tests" >&2
  exit 1
fi

if ! diff -u \
    <(printf '%s\n' "$selected_postgres_qualified_tests") \
    <(printf '%s\n' "$classified_postgres_tests"); then
  echo 'PostgreSQL tests must have exactly one registered fixture classification' >&2
  exit 1
fi

postgres_test_count="$((${#empty_postgres_contract_tests[@]} + ${#canonical_postgres_contract_tests[@]}))"
if [[ "$postgres_test_count" -ne 32 ]]; then
  printf 'expected the registered PostgreSQL inventory to contain 32 tests, found %d\n' \
    "$postgres_test_count" >&2
  exit 1
fi

printf 'Ignored Rust test inventory: %d PostgreSQL (%d empty/self-initializing, %d canonical), %d manual fixture\n' \
  "$postgres_test_count" \
  "${#empty_postgres_contract_tests[@]}" \
  "${#canonical_postgres_contract_tests[@]}" \
  "$(printf '%s\n' "$manual_tests" | wc -l)"

if [[ "$mode" == verify ]]; then
  exit 0
fi

require_command psql
: "${AIRCOST_TEST_POSTGRES_URL:?AIRCOST_TEST_POSTGRES_URL is required}"
: "${AIRCOST_TEST_POSTGRES_ADMIN_URL:?AIRCOST_TEST_POSTGRES_ADMIN_URL is required}"
: "${AIRCOST_TEST_POSTGRES_DATABASE:?AIRCOST_TEST_POSTGRES_DATABASE is required}"

if [[ ! "$AIRCOST_TEST_POSTGRES_DATABASE" =~ ^[a-zA-Z_][a-zA-Z0-9_]*$ ]]; then
  printf 'invalid PostgreSQL test database name: %s\n' \
    "$AIRCOST_TEST_POSTGRES_DATABASE" >&2
  exit 1
fi

reset_postgres_database() {
  psql "$AIRCOST_TEST_POSTGRES_ADMIN_URL" \
    --no-psqlrc \
    --set=ON_ERROR_STOP=1 \
    --set=database_name="$AIRCOST_TEST_POSTGRES_DATABASE" <<'SQL'
DROP DATABASE IF EXISTS :"database_name" WITH (FORCE);
CREATE DATABASE :"database_name";
SQL
}

postgres_fixture_for_test() {
  local test_name="$1"
  local registered_test

  for registered_test in "${empty_postgres_contract_tests[@]}"; do
    if [[ "$registered_test" == "$test_name" ]]; then
      printf 'empty\n'
      return 0
    fi
  done
  for registered_test in "${canonical_postgres_contract_tests[@]}"; do
    if [[ "$registered_test" == "$test_name" ]]; then
      printf 'canonical\n'
      return 0
    fi
  done
  return 1
}

run_postgres_contracts() {
  local target="$1"
  local test_names="$2"
  local test_name
  local fixture

  while IFS= read -r test_name; do
    [[ -n "$test_name" ]] || continue
    fixture="$(postgres_fixture_for_test "$test_name")"
    printf 'Running isolated PostgreSQL Rust contract (%s, %s fixture): %s\n' \
      "$target" "$fixture" "$test_name"
    reset_postgres_database
    if [[ "$fixture" == canonical ]]; then
      psql "$AIRCOST_TEST_POSTGRES_URL" \
        --no-psqlrc \
        --set=ON_ERROR_STOP=1 \
        --file="$repository_root/schema/postgres.sql" \
        >/dev/null
    fi

    (
      cd "$repository_root"
      if [[ "$target" == lib ]]; then
        cargo test --locked --lib "$test_name" -- \
          --ignored --exact --nocapture --test-threads=1
      else
        cargo test --locked --bin aircost-admin "$test_name" -- \
          --ignored --exact --nocapture --test-threads=1
      fi
    )
  done <<< "$test_names"
}

run_postgres_contracts lib "$selected_postgres_lib_tests"
run_postgres_contracts aircost-admin "$selected_postgres_admin_tests"
