#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sqlite_migration="$repository_root/migrations/20260730_aircraft_tcds_make_lineage.sqlite.sql"
postgres_migration="$repository_root/migrations/20260730_aircraft_tcds_make_lineage.postgres.sql"
baseline_database="$(mktemp /tmp/aircost-lineage-baseline.XXXXXX.sqlite3)"
upgrade_database="$(mktemp /tmp/aircost-lineage-upgrade.XXXXXX.sqlite3)"
guard_database="$(mktemp /tmp/aircost-lineage-guard.XXXXXX.sqlite3)"
fresh_database="$(mktemp /tmp/aircost-lineage-fresh.XXXXXX.sqlite3)"
sqlite_columns="$(mktemp /tmp/aircost-lineage-sqlite-columns.XXXXXX)"
postgres_columns="$(mktemp /tmp/aircost-lineage-postgres-columns.XXXXXX)"
trap 'rm -f "$baseline_database" "$upgrade_database" "$guard_database" \
  "$fresh_database" "$sqlite_columns" "$postgres_columns"' EXIT

expect_migration_failure() {
  local database="$1"
  if sqlite3 -bail "$database" ".read $sqlite_migration" >/dev/null 2>&1; then
    echo "expected lineage migration contract guard failure" >&2
    exit 1
  fi
}

expect_statement_failure() {
  local database="$1"
  local statement="$2"
  local expected_message="$3"
  local output
  if output="$(sqlite3 -bail "$database" \
      "PRAGMA foreign_keys=ON" "$statement" 2>&1)"; then
    echo "expected lineage invariant failure: $statement" >&2
    exit 1
  fi
  if [[ "$output" != *"$expected_message"* ]]; then
    echo "lineage invariant failed for the wrong reason" >&2
    echo "expected: $expected_message" >&2
    echo "actual: $output" >&2
    exit 1
  fi
}

execute_lineage_insert() {
  local database="$1"
  local rationale="$2"
  local n_number="$3"
  local source_record_digit="$4"
  local serial_key="$5"
  local pdf_digest_digit="$6"
  local first_serial="$7"
  local last_serial="$8"
  sqlite3 -bail "$database" <<SQL
PRAGMA foreign_keys = ON;
INSERT INTO aircraft_tcds_make_lineage_bindings (
  faa_snapshot_date, faa_archive_sha256, faa_aircraft_code,
  representative_faa_registry_snapshot_id, representative_faa_n_number,
  representative_faa_source_record_sha256,
  representative_faa_manufacturer_serial_key,
  faa_manufacturer_name, faa_model, aircraft_make_id,
  aircraft_designation_id, tcds_number, tcds_document_guid,
  tcds_pdf_sha256, tcds_former_holder_name, tcds_current_holder_name,
  tcds_manufacturer_name, tcds_selection_basis, serial_scope_kind,
  serial_prefix, serial_digits_width, first_serial_number,
  last_serial_number, approval_decision_id, faa_make_evidence_claim_id,
  tcds_model_identity_evidence_claim_id,
  tcds_serial_applicability_evidence_claim_id,
  tcds_holder_transfer_evidence_claim_id,
  tcds_manufacturer_range_evidence_claim_id
)
SELECT
  snapshot.snapshot_date, snapshot.archive_sha256, reference.aircraft_code,
  snapshot.id, '$n_number', printf('%064d', $source_record_digit),
  '$serial_key', reference.manufacturer_name, reference.model_name,
  make.id, designation.id, '3A13',
  'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
  printf('%064d', $pdf_digest_digit),
  'Cessna Aircraft Company', 'Textron Aviation Inc',
  'Cessna Aircraft Company', 'operator_validated_exact_model_serial',
  'manufacturer', '', 8, $first_serial, $last_serial, decision.id,
  faa_claim.id, model_claim.id, serial_claim.id, holder_claim.id,
  manufacturer_claim.id
FROM faa_registry_snapshots snapshot
JOIN faa_registry_aircraft_references reference
  ON reference.snapshot_id = snapshot.id
 AND reference.aircraft_code = '2072703'
JOIN aircraft_makes make
  ON make.normalized_name = 'textron aviation inc'
JOIN aircraft_model_families family
  ON family.aircraft_make_id = make.id
 AND family.normalized_name = 'skylane'
JOIN aircraft_designations designation
  ON designation.aircraft_model_family_id = family.id
 AND designation.normalized_official_designation = '182t'
JOIN aircraft_identity_decisions decision
  ON decision.rationale = '$rationale'
JOIN curation_evidence_claims faa_claim
  ON faa_claim.predicate_text = 'FAA legal make'
JOIN curation_evidence_claims model_claim
  ON model_claim.predicate_text = 'TCDS model identity'
JOIN curation_evidence_claims serial_claim
  ON serial_claim.predicate_text = 'TCDS serial applicability'
JOIN curation_evidence_claims holder_claim
  ON holder_claim.predicate_text = 'TCDS holder transfer'
JOIN curation_evidence_claims manufacturer_claim
  ON manufacturer_claim.predicate_text = 'TCDS manufacturer range'
WHERE snapshot.snapshot_date = '2026-07-22';
SQL
}

# Build a pre-lineage schema, then prove upgrade idempotency. A forged contract
# must fail before any lineage object is installed.
sed '/^CREATE UNIQUE INDEX IF NOT EXISTS idx_faa_registry_aircraft_lineage_record/,$d' \
  "$repository_root/schema/sqlite.sql" | sqlite3 -bail "$baseline_database"
cp "$baseline_database" "$upgrade_database"
cp "$baseline_database" "$guard_database"
sqlite3 -bail "$guard_database" <<'SQL'
INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint
) VALUES (
  '20260730_aircraft_tcds_make_lineage', 99,
  'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff'
);
SQL
expect_migration_failure "$guard_database"
test "$(sqlite3 "$guard_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='aircraft_tcds_make_lineage_bindings'")" = "0"

sqlite3 -bail "$upgrade_database" \
  ".read $sqlite_migration" \
  ".read $sqlite_migration"
sqlite3 -bail "$fresh_database" \
  ".read $repository_root/schema/sqlite.sql"

expected_contract="1:566485027d3df81bb5a90abcc0ce2b707e565bcbdc92ae3f007f527832fae735"
for database in "$upgrade_database" "$fresh_database"; do
  test "$(sqlite3 "$database" \
    "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260730_aircraft_tcds_make_lineage'")" = \
    "$expected_contract"
  test "$(sqlite3 "$database" \
    "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='aircraft_tcds_make_lineage_bindings'")" = "1"
  test "$(sqlite3 "$database" \
    "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name IN ('aircraft_tcds_make_lineage_requires_provenance','aircraft_tcds_make_lineage_no_overlap','aircraft_tcds_make_lineage_no_catalog_collision','aircraft_tcds_make_lineage_immutable_update','aircraft_tcds_make_lineage_immutable_delete','aircraft_make_tcds_lineage_collision_insert','aircraft_make_tcds_lineage_collision_update','aircraft_make_alias_tcds_lineage_collision','aircraft_designation_faa_binding_requires_provenance','listing_identity_assignment_requires_faa_identity','listing_ready_requires_canonical_aircraft_update')")" = "11"
  test "$(sqlite3 "$database" \
    "SELECT count(*) FROM (SELECT id FROM pragma_foreign_key_list('aircraft_tcds_make_lineage_bindings') WHERE \"table\"='faa_registry_aircraft' GROUP BY id HAVING count(*)=5 AND sum(\"from\"='representative_faa_registry_snapshot_id')=1 AND sum(\"from\"='representative_faa_n_number')=1 AND sum(\"from\"='representative_faa_source_record_sha256')=1 AND sum(\"from\"='representative_faa_manufacturer_serial_key')=1 AND sum(\"from\"='faa_aircraft_code')=1)")" = "1"
  test "$(sqlite3 "$database" "PRAGMA foreign_key_check;")" = ""
  test "$(sqlite3 "$database" "PRAGMA quick_check;")" = "ok"
done

# Install one complete, exact FAA + TCDS evidence graph. The canonical make is
# the current TCDS holder, while ACFTREF retains the former legal make.
sqlite3 -bail "$upgrade_database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO curation_evidence_sources (
  source_url, resolved_url, source_title, publisher, source_domain,
  source_tier, content_sha256, retrieved_at
) VALUES
  (
    'https://www.faa.gov/registry/releasable',
    'https://www.faa.gov/registry/releasable',
    'FAA registry fixture', 'Federal Aviation Administration', 'faa.gov',
    'regulator_primary', printf('%064d', 1), CURRENT_TIMESTAMP
  ),
  (
    'https://drs.faa.gov/api/drs/data-pull/download/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
    'https://drs.faa.gov/api/drs/data-pull/download/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee',
    'FAA TCDS 3A13 fixture', 'Federal Aviation Administration',
    'drs.faa.gov', 'regulator_primary', printf('%064d', 2),
    CURRENT_TIMESTAMP
  );
INSERT INTO faa_registry_snapshots (
  evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256, master_member_name,
  master_member_sha256, aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256, record_hash_domain
)
SELECT id, '2026-07-22', source_url, content_sha256,
  printf('%064d', 3), printf('%064d', 4), 'MASTER.txt',
  printf('%064d', 5), 'ACFTREF.txt', printf('%064d', 6),
  'ENGINE.txt', printf('%064d', 7),
  'aircost-faa-master-retained-aircraft-projection-v1'
FROM curation_evidence_sources WHERE source_title='FAA registry fixture';
INSERT INTO faa_registry_aircraft (
  snapshot_id, n_number, manufacturer_serial_raw, manufacturer_serial_key,
  aircraft_code, year_manufactured, source_record_sha256
)
SELECT id, 'N123AB', '18201234', '18201234', '2072703', 2006,
  printf('%064d', 6)
FROM faa_registry_snapshots WHERE snapshot_date='2026-07-22'
UNION ALL
SELECT id, 'N124AB', '18201235', '18201235', '2072703', 2006,
  printf('%064d', 7)
FROM faa_registry_snapshots WHERE snapshot_date='2026-07-22';
INSERT INTO faa_registry_aircraft_references (
  snapshot_id, aircraft_code, manufacturer_name, model_name
)
SELECT id, '2072703', 'CESSNA AIRCRAFT COMPANY', '182T'
FROM faa_registry_snapshots WHERE snapshot_date='2026-07-22';

INSERT INTO curation_evidence_claims (
  evidence_source_id, claim_kind, subject_text, predicate_text,
  object_text, quoted_evidence, validation_status, validated_at
)
SELECT id, 'identity', '2072703', 'FAA legal make',
  'CESSNA AIRCRAFT COMPANY', 'ACFTREF legal make', 'validated',
  CURRENT_TIMESTAMP
FROM curation_evidence_sources WHERE source_title='FAA registry fixture'
UNION ALL
SELECT id, 'identity', '182T', 'TCDS model identity',
  '182T', 'Model 182T', 'validated', CURRENT_TIMESTAMP
FROM curation_evidence_sources WHERE source_title='FAA TCDS 3A13 fixture'
UNION ALL
SELECT id, 'applicability', '182T', 'TCDS serial applicability',
  '18201234', '18201234 and up', 'validated', CURRENT_TIMESTAMP
FROM curation_evidence_sources WHERE source_title='FAA TCDS 3A13 fixture'
UNION ALL
SELECT id, 'identity', '3A13', 'TCDS holder transfer',
  'Cessna Aircraft Company to Textron Aviation Inc',
  'Cessna Aircraft Company transferred to Textron Aviation Inc',
  'validated', CURRENT_TIMESTAMP
FROM curation_evidence_sources WHERE source_title='FAA TCDS 3A13 fixture'
UNION ALL
SELECT id, 'applicability', '182T', 'TCDS manufacturer range',
  '18201234 through 18201240', 'Cessna Aircraft Company serial range',
  'validated', CURRENT_TIMESTAMP
FROM curation_evidence_sources WHERE source_title='FAA TCDS 3A13 fixture';

INSERT INTO aircraft_identity_observations (
  observed_make, observed_family, observed_designation,
  exact_source_evidence, observation_sha256
) VALUES (
  'CESSNA AIRCRAFT COMPANY', 'Skylane', '182T',
  'Exact schema lineage fixture', printf('%064d', 8)
);
INSERT INTO aircraft_identity_resolution_cases (
  observation_id, resolution_scope, job_fingerprint, catalog_revision
)
SELECT id, scope, 'lineage-schema-' || scope, 'schema-v1'
FROM aircraft_identity_observations,
  (SELECT 'make' AS scope UNION ALL SELECT 'family'
   UNION ALL SELECT 'designation');
INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT id, resolution_scope, 'approve_new', 'approved', '{}',
  '{"passed":true}', 1, 'Catalog ' || resolution_scope, CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases;
INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT decision.id, claim.id, 'identity'
FROM aircraft_identity_decisions decision
JOIN curation_evidence_claims claim
  ON claim.predicate_text = CASE decision.entity_kind
    WHEN 'make' THEN 'TCDS holder transfer'
    ELSE 'TCDS model identity'
  END
WHERE decision.rationale LIKE 'Catalog %';
INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id)
SELECT 'Textron Aviation Inc', 'textron aviation inc', id
FROM aircraft_identity_decisions WHERE rationale='Catalog make';
INSERT INTO aircraft_model_families (
  aircraft_make_id, name, normalized_name, approval_decision_id
)
SELECT make.id, 'Skylane', 'skylane', decision.id
FROM aircraft_makes make, aircraft_identity_decisions decision
WHERE make.normalized_name='textron aviation inc'
  AND decision.rationale='Catalog family';
INSERT INTO aircraft_designations (
  aircraft_model_family_id, official_designation,
  normalized_official_designation, display_name, approval_decision_id
)
SELECT family.id, '182T', '182t', 'Cessna 182T', decision.id
FROM aircraft_model_families family, aircraft_identity_decisions decision
WHERE family.normalized_name='skylane'
  AND decision.rationale='Catalog designation';

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  selected_entity_id, decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT case_row.id, 'make', 'match_existing', 'approved', make.id, '{}',
  '{"passed":true}', 1, rationale, CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases case_row
JOIN aircraft_makes make ON make.normalized_name='textron aviation inc'
JOIN (
  SELECT 'Lineage exact record mismatch' AS rationale
  UNION ALL SELECT 'Lineage valid'
  UNION ALL SELECT 'Lineage wrong PDF digest'
  UNION ALL SELECT 'Lineage overlap'
) rationales
WHERE case_row.resolution_scope='make';
INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT decision.id, claim.id,
  CASE claim.claim_kind WHEN 'applicability' THEN 'applicability'
       ELSE 'identity' END
FROM aircraft_identity_decisions decision
CROSS JOIN curation_evidence_claims claim
WHERE decision.rationale LIKE 'Lineage %';
SQL

if output="$(execute_lineage_insert "$upgrade_database" \
    "Lineage exact record mismatch" N124AB 6 18201234 2 \
    18201234 18201234 2>&1)"; then
  echo "expected mixed representative FAA record to fail" >&2
  exit 1
fi
test "$output" != ""

execute_lineage_insert "$upgrade_database" \
  "Lineage valid" N123AB 6 18201234 2 18201234 18201234
test "$(sqlite3 "$upgrade_database" \
  "SELECT representative_faa_n_number || ':' || representative_faa_manufacturer_serial_key FROM aircraft_tcds_make_lineage_bindings")" = \
  "N123AB:18201234"

if output="$(execute_lineage_insert "$upgrade_database" \
    "Lineage wrong PDF digest" N124AB 7 18201235 9 \
    18201235 18201235 2>&1)"; then
  echo "expected mismatched TCDS digest provenance to fail" >&2
  exit 1
fi
[[ "$output" == *"requires distinct FAA make"* ]]

if output="$(execute_lineage_insert "$upgrade_database" \
    "Lineage overlap" N123AB 6 18201234 2 \
    18201230 18201240 2>&1)"; then
  echo "expected overlapping lineage range to fail" >&2
  exit 1
fi
[[ "$output" == *"serial ranges cannot overlap"* ]]

sqlite3 -bail "$upgrade_database" <<'SQL'
INSERT INTO aircraft_designation_faa_bindings (
  faa_snapshot_date, faa_archive_sha256, faa_aircraft_code,
  aircraft_designation_id, representative_faa_registry_snapshot_id,
  identity_evidence_claim_id
)
SELECT snapshot.snapshot_date, snapshot.archive_sha256, reference.aircraft_code,
  designation.id, snapshot.id, claim.id
FROM faa_registry_snapshots snapshot
JOIN faa_registry_aircraft_references reference
  ON reference.snapshot_id=snapshot.id
JOIN aircraft_designations designation
  ON designation.normalized_official_designation='182t'
JOIN curation_evidence_claims claim
  ON claim.predicate_text='FAA legal make';

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT id, 'make', 'approve_new', 'approved', '{}', '{"passed":true}', 1,
  'Collision make', CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases WHERE resolution_scope='make';
INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT decision.id, claim.id, 'identity'
FROM aircraft_identity_decisions decision
JOIN curation_evidence_claims claim ON claim.predicate_text='FAA legal make'
WHERE decision.rationale='Collision make';
SQL

expect_statement_failure "$upgrade_database" \
  "INSERT INTO aircraft_makes (name,normalized_name,approval_decision_id) SELECT 'CESSNA AIRCRAFT COMPANY','cessna aircraft company',id FROM aircraft_identity_decisions WHERE rationale='Collision make'" \
  "collides with an approved FAA/TCDS lineage label"
expect_statement_failure "$upgrade_database" \
  "UPDATE aircraft_tcds_make_lineage_bindings SET last_serial_number=18201235" \
  "bindings are immutable"
expect_statement_failure "$upgrade_database" \
  "DELETE FROM aircraft_tcds_make_lineage_bindings" \
  "bindings are immutable"

test "$(sqlite3 "$upgrade_database" \
  "SELECT count(*) FROM aircraft_designation_faa_bindings WHERE faa_aircraft_code='2072703'")" = "1"
test "$(sqlite3 "$upgrade_database" "PRAGMA foreign_key_check;")" = ""
test "$(sqlite3 "$upgrade_database" "PRAGMA quick_check;")" = "ok"

# Backend parity: exact table columns and every security-relevant concept must
# exist in both implementations and both canonical fresh schemas.
sed -n \
  '/^CREATE TABLE IF NOT EXISTS aircraft_tcds_make_lineage_bindings/,/^);/p' \
  "$sqlite_migration" |
  sed -nE 's/^  ([a-z][a-z0-9_]*) .*/\1/p' >"$sqlite_columns"
sed -n \
  '/^CREATE TABLE IF NOT EXISTS aircraft_tcds_make_lineage_bindings/,/^);/p' \
  "$postgres_migration" |
  sed -nE 's/^  ([a-z][a-z0-9_]*) .*/\1/p' >"$postgres_columns"
diff -u "$sqlite_columns" "$postgres_columns"

for required in \
  representative_faa_n_number \
  representative_faa_source_record_sha256 \
  representative_faa_manufacturer_serial_key \
  tcds_model_identity_evidence_claim_id \
  tcds_serial_applicability_evidence_claim_id \
  tcds_holder_transfer_evidence_claim_id \
  tcds_manufacturer_range_evidence_claim_id \
  registry_reference \
  drs_unique_current_exact_model \
  operator_validated_exact_model_serial; do
  rg -q "$required" "$sqlite_migration" "$repository_root/schema/sqlite.sql"
  rg -q "$required" "$postgres_migration" "$repository_root/schema/postgres.sql"
done

echo "aircraft TCDS make-lineage schema checks passed"
