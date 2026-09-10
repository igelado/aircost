#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

postgres_contract_tests=(
  diagnostic_postgres_connections_default_to_read_only
  import_faa_registry_dry_run_keeps_postgres_rows_and_markers_unchanged
  postgres_active_replay_freeze_rejects_truncate_cascade
  postgres_all_receipt_timestamps_survive_two_normal_startups
  postgres_canonical_schema_passes_end_to_end_startup
  postgres_correction_validation_rejects_altered_search_path_and_namespace
  postgres_database_identity_recognizes_aliases_for_ordinary_role
  postgres_export_matches_the_sqlite_readiness_contract
  postgres_faa_reference_startup_attests_exact_objects
  postgres_generic_feature_label_migration_audits_without_model_updates
  postgres_grounded_capability_functions_resist_hostile_search_path_and_tampering
  postgres_inherited_child_receipt_cannot_satisfy_parent_ledger
  postgres_late_initialization_failure_rolls_back_all_ddl
  postgres_listing_replay_fresh_migrated_idempotent_and_hostile_search_path
  postgres_listing_replay_installed_at_survives_two_normal_startups
  postgres_listing_replay_rerun_rejects_unexpected_indexes_and_triggers
  postgres_listing_replay_startup_rejects_weakened_column_constraint_and_index
  postgres_production_acquire_and_release_uses_backend_placeholders
  postgres_publishes_and_selects_null_and_non_null_configuration_dimensions
  postgres_reference_cutover_rejects_null_marker_fields_without_healing
  postgres_reference_cutover_validation_rejects_adversarial_mutations
  postgres_replay_inventory_orders_repeatable_read_and_read_committed_writers
  postgres_replay_ledger_rejects_invalid_state_outcome_pairings
  postgres_round_trip_fences_writers_preserves_replay_and_rejects_rerun
  postgres_sequence_reset_covers_identity_and_serial_owned_ids
  postgres_startup_pins_search_path_and_ignores_attacker_shadows
  postgres_startup_rejects_anchor_receipt_xor_and_hostile_markers_without_mutation
  postgres_startup_rejects_noncanonical_ledger_storage_without_mutation
  postgres_startup_rejects_same_named_noop_approved_concrete_model_function
  postgres_startup_rejects_visual_artifact_constraint_tampering
  postgres_startup_waits_for_writer_and_fresh_startups_serialize
  postgres_visual_artifact_bind_waits_for_newer_faa_snapshot_and_refuses_stale_pair
)

manual_fixture_tests=(
  cached_official_archive_derives_intrinsic_release_date
  controller_real_corpus_contract_audit
  downloaded_g1000_nxi_text_form_regression
  downloaded_official_oem_pdf_regressions
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
selected_postgres_tests="$({
  cd "$repository_root"
  list_ignored_tests postgres
} | sed -n 's/: test$//p' \
  | sed 's/.*:://' \
  | LC_ALL=C sort -u)"
expected_postgres_tests="$(printf '%s\n' "${postgres_contract_tests[@]}" | LC_ALL=C sort)"
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

postgres_test_count="${#postgres_contract_tests[@]}"
if [[ "$postgres_test_count" -ne 32 ]]; then
  printf 'expected the registered PostgreSQL inventory to contain 32 tests, found %d\n' \
    "$postgres_test_count" >&2
  exit 1
fi

printf 'Ignored Rust test inventory: %d PostgreSQL, %d manual fixture\n' \
  "$postgres_test_count" \
  "$(printf '%s\n' "$manual_tests" | wc -l)"
