#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_database="$(mktemp /tmp/aircost-grounded-exact.XXXXXX.sqlite3)"
trap 'rm -f "$test_database"' EXIT

expect_failure() {
  local statement="$1"
  local description="$2"
  if sqlite3 -bail "$test_database" \
      "PRAGMA foreign_keys=ON" "$statement" >/dev/null 2>&1; then
    echo "Expected failure: $description" >&2
    exit 1
  fi
}

sqlite3 -bail "$test_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/migrations/20260810_avionics_grounded_exact_model_consolidation.sqlite.sql" \
  ".read $repository_root/migrations/20260810_avionics_grounded_exact_model_consolidation.sqlite.sql"

sqlite3 -bail "$test_database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES ('Schema Test', 'schema test');
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key, identity_evidence_kind,
  identity_source_url, identity_source_title, identity_evidence_text,
  identity_confidence
) VALUES (
  'Schema Test', 'schematest', 'authoritative_reference',
  'https://schema.example.test/manufacturer', 'Schema manufacturer',
  'Schema Test identifies itself as the manufacturer.', 'very_high'
);
INSERT INTO avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id, avionics_manufacturer_identity_id,
  membership_basis, normalized_name_key, evidence_source_url,
  evidence_source_title, evidence_text, evidence_confidence
)
SELECT manufacturer.id, identity.id, 'deterministic_exact', 'schematest',
       'urn:aircost:deterministic:avionics-manufacturer-normalization:v1',
       'Aircost exact manufacturer normalization v1',
       'The stored manufacturer spelling has the same exact deterministic normalization key as this evidence-backed identity.',
       'very_high'
FROM avionics_manufacturers manufacturer,
     avionics_manufacturer_identities identity
WHERE manufacturer.normalized_name = 'schema test'
  AND identity.normalized_identity_key = 'schematest';
INSERT INTO avionics_models (avionics_manufacturer_id, name, normalized_name)
SELECT id, 'Unit 100', 'unit 100'
FROM avionics_manufacturers WHERE normalized_name = 'schema test';
INSERT INTO avionics_models (avionics_manufacturer_id, name, normalized_name)
SELECT id, 'Unit 100', 'unit 100'
FROM avionics_manufacturers WHERE normalized_name = 'schema test';
INSERT INTO avionics_models (avionics_manufacturer_id, name, normalized_name)
SELECT id, 'Unit 100', 'unit 100'
FROM avionics_manufacturers WHERE normalized_name = 'schema test';
SQL

survivor_id="$(sqlite3 "$test_database" \
  "SELECT min(id) FROM avionics_models WHERE normalized_name='unit 100'")"
duplicate_one="$(sqlite3 "$test_database" \
  "SELECT id FROM avionics_models WHERE normalized_name='unit 100' ORDER BY id LIMIT 1 OFFSET 1")"
duplicate_two="$(sqlite3 "$test_database" \
  "SELECT max(id) FROM avionics_models WHERE normalized_name='unit 100'")"
manufacturer_identity_id="$(sqlite3 "$test_database" \
  "SELECT id FROM avionics_manufacturer_identities WHERE normalized_identity_key='schematest'")"

expect_failure \
  "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id,survivor_model_id,purpose) VALUES ($duplicate_one,$survivor_id,'grounded_exact_model_consolidation')" \
  "a grounded pair cannot forge authority through the ordinary guard"

sqlite3 -bail "$test_database" "PRAGMA foreign_keys=ON" \
  "INSERT INTO avionics_catalog_grounded_consolidation_authorizations (authorization_sha256,survivor_model_id,effective_manufacturer_identity_id,normalized_model_key,expected_member_count,reviewed_catalog_fingerprint,manufacturer_collision_snapshot_sha256) VALUES ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',$survivor_id,$manufacturer_identity_id,'unit 100',3,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc')" \
  "INSERT INTO avionics_catalog_grounded_consolidation_guard (duplicate_model_id,survivor_model_id,authorization_sha256) VALUES ($duplicate_one,$survivor_id,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')"

expect_failure \
  "INSERT INTO avionics_catalog_grounded_consolidation_claim (authorization_sha256,survivor_model_id) VALUES ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',$survivor_id)" \
  "a claim omitting the third exact-model member"

sqlite3 -bail "$test_database" "PRAGMA foreign_keys=ON" \
  "INSERT INTO avionics_catalog_grounded_consolidation_guard (duplicate_model_id,survivor_model_id,authorization_sha256) VALUES ($duplicate_two,$survivor_id,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')" \
  "INSERT INTO avionics_catalog_grounded_consolidation_claim (authorization_sha256,survivor_model_id) VALUES ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',$survivor_id)"

test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM avionics_catalog_authorized_consolidations WHERE survivor_model_id=$survivor_id")" = "2"
expect_failure \
  "UPDATE avionics_models SET normalized_name='unit 100 changed' WHERE id=$survivor_id" \
  "a claimed endpoint identity mutation"

sqlite3 -bail "$test_database" "PRAGMA foreign_keys=ON" \
  "DELETE FROM avionics_models WHERE id=$duplicate_one" \
  "DELETE FROM avionics_models WHERE id=$duplicate_two" \
  "DELETE FROM avionics_catalog_grounded_consolidation_claim WHERE authorization_sha256='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'" \
  "DELETE FROM avionics_catalog_grounded_consolidation_authorizations WHERE authorization_sha256='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'"

test "$(sqlite3 "$test_database" \
  "SELECT (SELECT count(*) FROM avionics_catalog_grounded_consolidation_authorizations) + (SELECT count(*) FROM avionics_catalog_grounded_consolidation_guard) + (SELECT count(*) FROM avionics_catalog_grounded_consolidation_claim)")" = "0"
test -z "$(sqlite3 "$test_database" "PRAGMA foreign_key_check")"
test "$(sqlite3 "$test_database" "PRAGMA integrity_check")" = "ok"

for definition in \
  "$repository_root/schema/sqlite.sql" \
  "$repository_root/schema/postgres.sql" \
  "$repository_root/migrations/20260810_avionics_grounded_exact_model_consolidation.sqlite.sql" \
  "$repository_root/migrations/20260810_avionics_grounded_exact_model_consolidation.postgres.sql"
do
  rg -q 'avionics_catalog_grounded_consolidation_authorizations' "$definition"
  rg -q 'avionics_catalog_grounded_consolidation_guard' "$definition"
  rg -q 'avionics_catalog_grounded_consolidation_claim' "$definition"
  rg -q 'avionics_catalog_valid_grounded_consolidation_pairs' "$definition"
  rg -q '20260810_avionics_grounded_exact_model_consolidation' "$definition"
done

echo "Grounded exact-model avionics consolidation schema contract passed"
