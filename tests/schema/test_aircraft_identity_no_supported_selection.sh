#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
migration="$repository_root/migrations/20260728_aircraft_identity_no_supported_selection.sqlite.sql"
postgres_migration="$repository_root/migrations/20260728_aircraft_identity_no_supported_selection.postgres.sql"
fresh_database="$(mktemp /tmp/aircost-no-supported-fresh.XXXXXX.sqlite3)"
legacy_database="$(mktemp /tmp/aircost-no-supported-legacy.XXXXXX.sqlite3)"
malformed_database="$(mktemp /tmp/aircost-no-supported-malformed.XXXXXX.sqlite3)"
trap 'rm -f "$fresh_database" "$legacy_database" "$malformed_database"' EXIT

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

# The fresh schema contains only the clean action and accepts a validated,
# affirmative absence for the two optional hierarchy dimensions.
sqlite3 -bail "$fresh_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/tests/schema/aircraft_reference_catalog.sqlite.sql" \
  "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    selected_entity_id, decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (
    1, 'generation', 'no_supported_selection', 'approved',
    NULL, '{}', '{\"passed\":true}', 1,
    'No supported generation is identified', '2026-07-23'
  );
  "

test "$(sqlite3 "$fresh_database" \
  "SELECT count(*) FROM aircraft_identity_decisions WHERE decision_action='no_supported_selection' AND decision_status='approved' AND selected_entity_id IS NULL")" = "1"
test "$(sqlite3 "$fresh_database" \
  "SELECT instr(sql, 'not_an_entity') FROM sqlite_schema WHERE type='table' AND name='aircraft_identity_decisions'")" = "0"
test "$(sqlite3 "$fresh_database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260728_aircraft_identity_no_supported_selection'")" = \
  "2:2c61547aae5158dd0a5393ca49218f0f3aada7d9b87caf950fa27fe2953d7dee"

expect_failure "$fresh_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (
    1, 'make', 'no_supported_selection', 'approved',
    '{}', '{\"passed\":true}', 1, 'invalid dimension', '2026-07-23'
  )
" "CHECK constraint failed"
expect_failure "$fresh_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (
    1, 'package', 'no_supported_selection', 'rejected',
    '{}', '{\"passed\":true}', 1, 'invalid status', '2026-07-23'
  )
" "CHECK constraint failed"
expect_failure "$fresh_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (
    1, 'package', 'not_an_entity', 'rejected',
    '{}', '{\"passed\":true}', 1, 'legacy action', '2026-07-23'
  )
" "CHECK constraint failed"
expect_failure "$fresh_database" "
  INSERT INTO aircraft_identity_decision_claims (
    decision_id, evidence_claim_id, evidence_role
  )
  SELECT max(id), 1, 'identity'
  FROM aircraft_identity_decisions
  WHERE decision_action = 'no_supported_selection'
" "no-supported-selection decision cannot have evidence claims"

# Minimal canonical legacy catalog. Two valid optional negative decisions are
# retained as generic rejections, never promoted to the new affirmative
# no-supported-selection state. Evidence links and sequence state remain intact.
sqlite3 -bail "$legacy_database" "
  PRAGMA foreign_keys = ON;
  CREATE TABLE users (id INTEGER PRIMARY KEY);
  CREATE TABLE aircraft_identity_observations (id INTEGER PRIMARY KEY);
  CREATE TABLE aircraft_identity_resolution_cases (
    id INTEGER PRIMARY KEY,
    observation_id INTEGER NOT NULL
      REFERENCES aircraft_identity_observations(id) ON DELETE CASCADE
  );
  CREATE TABLE curation_evidence_claims (id INTEGER PRIMARY KEY);
  CREATE TABLE aircraft_identity_decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    resolution_case_id INTEGER NOT NULL
      REFERENCES aircraft_identity_resolution_cases(id) ON DELETE RESTRICT,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN (
      'make', 'family', 'designation', 'alias', 'identifier', 'generation',
      'generation_designation', 'package', 'package_applicability',
      'engine_model', 'propeller_model', 'reference_configuration',
      'serial_scheme', 'feature_definition', 'reference_profile'
    )),
    decision_action TEXT NOT NULL CHECK (decision_action IN (
      'match_existing', 'approve_new', 'ambiguous', 'not_an_entity', 'reject'
    )),
    decision_status TEXT NOT NULL CHECK (decision_status IN (
      'approved', 'rejected', 'ambiguous'
    )),
    selected_entity_id INTEGER,
    decision_payload_json TEXT NOT NULL,
    deterministic_validation_json TEXT NOT NULL,
    deterministic_validation_passed INTEGER NOT NULL
      CHECK (deterministic_validation_passed IN (0, 1)),
    rationale TEXT NOT NULL,
    decided_by_user_id INTEGER REFERENCES users(id) ON DELETE RESTRICT,
    decided_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
      (decision_status = 'approved'
        AND decision_action IN ('match_existing', 'approve_new')
        AND deterministic_validation_passed = 1)
      OR (decision_status = 'rejected'
        AND decision_action IN ('not_an_entity', 'reject'))
      OR (decision_status = 'ambiguous' AND decision_action = 'ambiguous')
    ),
    CHECK (
      (decision_action = 'match_existing' AND selected_entity_id IS NOT NULL)
      OR (decision_action <> 'match_existing' AND selected_entity_id IS NULL)
    )
  );
  CREATE INDEX idx_aircraft_identity_decisions_case
    ON aircraft_identity_decisions (resolution_case_id, decision_status);
  CREATE TABLE aircraft_identity_decision_claims (
    decision_id INTEGER NOT NULL
      REFERENCES aircraft_identity_decisions(id) ON DELETE CASCADE,
    evidence_claim_id INTEGER NOT NULL
      REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
    evidence_role TEXT NOT NULL,
    PRIMARY KEY (decision_id, evidence_claim_id, evidence_role)
  );
  CREATE TABLE schema_migration_contracts (
    migration_name TEXT PRIMARY KEY,
    contract_version INTEGER NOT NULL,
    contract_fingerprint TEXT NOT NULL,
    installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  );
  INSERT INTO aircraft_identity_observations (id) VALUES (1);
  INSERT INTO aircraft_identity_resolution_cases (id, observation_id)
  VALUES (1, 1), (2, 1), (3, 1), (4, 1);
  INSERT INTO curation_evidence_claims (id) VALUES (1), (2);
  INSERT INTO aircraft_identity_decisions (
    id, resolution_case_id, entity_kind, decision_action, decision_status,
    selected_entity_id, decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at, created_at
  ) VALUES
    (5, 1, 'generation', 'not_an_entity', 'rejected', NULL,
      '{\"legacy\":5}', '{\"passed\":true}', 1, 'no generation',
      '2026-07-22T01:00:00Z', '2026-07-22T01:01:00Z'),
    (6, 2, 'package', 'not_an_entity', 'rejected', NULL,
      '{\"legacy\":6}', '{\"passed\":true}', 1, 'no package',
      '2026-07-22T02:00:00Z', '2026-07-22T02:01:00Z'),
    (7, 3, 'make', 'approve_new', 'approved', NULL,
      '{\"legacy\":7}', '{\"passed\":true}', 1, 'approved make',
      '2026-07-22T03:00:00Z', '2026-07-22T03:01:00Z');
  INSERT INTO aircraft_identity_decision_claims (
    decision_id, evidence_claim_id, evidence_role
  ) VALUES (5, 1, 'identity'), (7, 2, 'identity');
  UPDATE sqlite_sequence
  SET seq = 50
  WHERE name = 'aircraft_identity_decisions';
"
cp "$legacy_database" "$malformed_database"

sqlite3 -bail "$legacy_database" \
  ".read $migration" \
  ".read $migration"

test "$(sqlite3 "$legacy_database" \
  "SELECT group_concat(id || ':' || entity_kind || ':' || decision_action || ':' || decision_status, ',') FROM aircraft_identity_decisions ORDER BY id")" = \
  "5:generation:reject:rejected,6:package:reject:rejected,7:make:approve_new:approved"
test "$(sqlite3 "$legacy_database" \
  "SELECT decision_payload_json || ':' || decided_at || ':' || created_at FROM aircraft_identity_decisions WHERE id=5")" = \
  "{\"legacy\":5}:2026-07-22T01:00:00Z:2026-07-22T01:01:00Z"
test "$(sqlite3 "$legacy_database" \
  "SELECT group_concat(decision_id || ':' || evidence_claim_id, ',') FROM aircraft_identity_decision_claims ORDER BY decision_id, evidence_claim_id")" = "5:1,7:2"
test "$(sqlite3 "$legacy_database" \
  "SELECT count(*) FROM curation_evidence_claims")" = "2"
test "$(sqlite3 "$legacy_database" \
  "SELECT seq FROM sqlite_sequence WHERE name='aircraft_identity_decisions'")" = "50"
test "$(sqlite3 "$legacy_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name IN ('aircraft_identity_no_supported_selection_claim_insert','aircraft_identity_no_supported_selection_claim_update','aircraft_identity_no_supported_selection_decision_update')")" = "3"
test -z "$(sqlite3 "$legacy_database" "PRAGMA foreign_key_check")"
test "$(sqlite3 "$legacy_database" "PRAGMA quick_check")" = "ok"

sqlite3 -bail "$legacy_database" "
  INSERT INTO aircraft_identity_decisions (
    resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (
    4, 'make', 'reject', 'rejected', '{}', '{}', 0,
    'rejected candidate', '2026-07-23'
  );
"
test "$(sqlite3 "$legacy_database" \
  "SELECT id FROM aircraft_identity_decisions WHERE rationale='rejected candidate'")" = "51"
expect_failure "$legacy_database" "
  INSERT INTO aircraft_identity_decisions (
    id, resolution_case_id, entity_kind, decision_action, decision_status,
    decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (
    8, 4, 'generation', 'no_supported_selection', 'approved',
    '{}', '{\"passed\":true}', 1, 'current operational null', '2026-07-23'
  );
  INSERT INTO aircraft_identity_decision_claims (
    decision_id, evidence_claim_id, evidence_role
  ) VALUES (8, 1, 'identity')
" "no-supported-selection decision cannot have evidence claims"
expect_failure "$legacy_database" "
  UPDATE aircraft_identity_decisions
  SET entity_kind='generation', decision_action='no_supported_selection'
  WHERE id=7
" "decision with evidence claims cannot become no-supported-selection"

# A malformed legacy use is rejected before claim cleanup or table replacement.
sqlite3 -bail "$malformed_database" "
  INSERT INTO aircraft_identity_decisions (
    id, resolution_case_id, entity_kind, decision_action, decision_status,
    selected_entity_id, decision_payload_json, deterministic_validation_json,
    deterministic_validation_passed, rationale, decided_at
  ) VALUES (
    8, 4, 'make', 'not_an_entity', 'rejected', NULL,
    '{}', '{\"passed\":true}', 1, 'malformed legacy action', '2026-07-23'
  );
"
expect_failure "$malformed_database" \
  ".read $migration" \
  "CHECK constraint failed"
test "$(sqlite3 "$malformed_database" \
  "SELECT decision_action || ':' || decision_status FROM aircraft_identity_decisions WHERE id=8")" = \
  "not_an_entity:rejected"
test "$(sqlite3 "$malformed_database" \
  "SELECT count(*) FROM aircraft_identity_decision_claims WHERE decision_id=5")" = "1"
test "$(sqlite3 "$malformed_database" \
  "SELECT count(*) FROM schema_migration_contracts WHERE migration_name='20260728_aircraft_identity_no_supported_selection'")" = "0"

# Backend parity: both contracts carry the same action/status/dimension and
# cross-table evidence guards. The old literal is absent from fresh schemas.
for required in \
  "no_supported_selection" \
  "aircraft_identity_no_supported_selection_claim_insert" \
  "aircraft_identity_no_supported_selection_claim_update" \
  "aircraft_identity_no_supported_selection_decision_update" \
  "no-supported-selection decision cannot have evidence claims"; do
  rg -q "$required" "$repository_root/schema/sqlite.sql"
  rg -q "$required" "$repository_root/schema/postgres.sql"
  rg -q "$required" "$migration"
  rg -q "$required" "$postgres_migration"
done
if rg -q "not_an_entity" \
  "$repository_root/schema/sqlite.sql" \
  "$repository_root/schema/postgres.sql"; then
  echo "fresh schemas still expose the legacy not_an_entity action" >&2
  exit 1
fi

echo "aircraft identity no-supported-selection schema contract passed"
