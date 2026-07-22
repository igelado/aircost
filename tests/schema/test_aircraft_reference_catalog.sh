#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_database="$(mktemp /tmp/aircost-reference-schema.XXXXXX.sqlite3)"
approval_database="$(mktemp /tmp/aircost-reference-approval.XXXXXX.sqlite3)"
component_database="$(mktemp /tmp/aircost-reference-component.XXXXXX.sqlite3)"
overlap_database="$(mktemp /tmp/aircost-reference-overlap.XXXXXX.sqlite3)"
trap 'rm -f "$test_database" "$approval_database" "$component_database" "$overlap_database"' EXIT

sqlite3 -bail "$test_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/tests/schema/aircraft_reference_catalog.sqlite.sql"

published_state="$(sqlite3 "$test_database" \
  "SELECT publication_state FROM aircraft_reference_configuration_versions WHERE id = 1")"
test "$published_state" = "published"

engine_catalog_target="$(sqlite3 "$test_database" \
  "SELECT \"table\" FROM pragma_foreign_key_list('aircraft_reference_engines') WHERE \"from\" = 'aircraft_engine_catalog_model_id'")"
propeller_catalog_target="$(sqlite3 "$test_database" \
  "SELECT \"table\" FROM pragma_foreign_key_list('aircraft_reference_propellers') WHERE \"from\" = 'aircraft_propeller_catalog_model_id'")"
test "$engine_catalog_target" = "aircraft_engine_catalog_models"
test "$propeller_catalog_target" = "aircraft_propeller_catalog_models"

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

expect_failure "$test_database" \
  "UPDATE aircraft_reference_prices SET amount = 1 WHERE id = 1" \
  "reference profile facts are immutable"
expect_failure "$test_database" \
  "UPDATE aircraft_engine_catalog_models SET model_name = 'mutated' WHERE id = 1" \
  "approved engine catalog models are immutable"

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
  ) VALUES (2, 1, 1, 1);
  INSERT INTO aircraft_reference_prices (
    aircraft_reference_configuration_version_id, price_kind, amount, currency,
    price_reference_year, evidence_kind, evidence_claim_id
  ) VALUES (2, 'equipped_msrp', 789900, 'USD', 2020, 'direct_model_year', 1);
  UPDATE aircraft_reference_configuration_versions
  SET publication_state = 'published', published_at = '2026-07-21'
  WHERE id = 2;
" "published reference profile applicability overlaps an existing version"

sqlite3 -bail "$test_database" "PRAGMA foreign_key_check"
echo "aircraft reference catalog SQLite schema contract passed"
