#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_database="$(mktemp /tmp/aircost-listing-aircraft-identity.XXXXXX.sqlite3)"
old_v1_database="$(mktemp /tmp/aircost-listing-aircraft-identity-v1.XXXXXX.sqlite3)"
old_v1_error="$(mktemp /tmp/aircost-listing-aircraft-identity-v1.XXXXXX.log)"
trap 'rm -f "$test_database" "$old_v1_database" "$old_v1_error"' EXIT

expect_failure() {
  local statement="$1"
  local description="$2"
  if sqlite3 -bail "$test_database" "PRAGMA foreign_keys=ON" "$statement" >/dev/null 2>&1; then
    echo "Expected failure: $description" >&2
    exit 1
  fi
}

# A draft v1 contract is not an accepted predecessor. The provenance guard
# must reject it before inspecting or changing the legacy domain tables.
sqlite3 -bail "$old_v1_database" <<'SQL'
CREATE TABLE schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL,
  contract_fingerprint TEXT NOT NULL,
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint
) VALUES (
  '20260725_listing_aircraft_identity',
  1,
  '305f5d269aa5561fad6845bcb9a76bd68e856a994ea528e585f6d32051adc968'
);
CREATE TABLE aircraft_markets (
  id INTEGER PRIMARY KEY,
  code TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  parent_market_id INTEGER
);
INSERT INTO aircraft_markets (id, code, name)
VALUES (1, 'GLOBAL', 'Global');
CREATE TABLE aircraft_sale_listing_identity_assignments (
  id INTEGER PRIMARY KEY,
  aircraft_sale_listing_id INTEGER NOT NULL,
  supersedes_assignment_id INTEGER UNIQUE,
  UNIQUE (id, aircraft_sale_listing_id),
  FOREIGN KEY (supersedes_assignment_id, aircraft_sale_listing_id)
    REFERENCES aircraft_sale_listing_identity_assignments(
      id, aircraft_sale_listing_id
    )
    ON DELETE RESTRICT
);
SQL
if sqlite3 -bail "$old_v1_database" \
    ".read $repository_root/migrations/20260725_listing_aircraft_identity.sqlite.sql" \
    2>"$old_v1_error"; then
  echo "Expected the draft v1 SQLite contract to fail closed" >&2
  exit 1
fi
grep -q "CHECK constraint failed: accepted = 1" "$old_v1_error"
test "$(sqlite3 "$old_v1_database" \
  "SELECT contract_version FROM schema_migration_contracts WHERE migration_name='20260725_listing_aircraft_identity'")" = "1"

# The migration is safe after baseline initialization and remains idempotent.
sqlite3 -bail "$test_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/migrations/20260725_listing_aircraft_identity.sqlite.sql" \
  ".read $repository_root/migrations/20260725_listing_aircraft_identity.sqlite.sql"

test "$(sqlite3 "$test_database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260725_listing_aircraft_identity'")" = \
  "2:63fb5b5213fc9eb2b7b4dcb2b0be3a9f22a80d4acae49f64e68ec1302c1437be"
test "$(sqlite3 "$test_database" \
  "SELECT count(DISTINCT foreign_key.id) FROM pragma_foreign_key_list('aircraft_sale_listing_identity_assignments') foreign_key WHERE foreign_key.\"table\"='aircraft_sale_listing_identity_assignments' AND upper(foreign_key.on_delete)='CASCADE'")" = "1"

sqlite3 -bail "$test_database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO users (email, display_name, auth_subject)
VALUES ('identity@example.test', 'Identity Test', 'identity-test');
INSERT INTO aircraft_manufacturers (name, normalized_name)
VALUES ('Legacy Label', 'legacy label');
INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name)
SELECT id, '182', '182' FROM aircraft_manufacturers WHERE normalized_name='legacy label';
INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name)
SELECT id, 'Skylane', 'skylane' FROM aircraft_models WHERE normalized_name='182';
INSERT INTO aircraft_sale_listings (
  aircraft_model_variant_id, created_by_user_id, source_url, model_year,
  asking_price_usd, airframe_hours, registration_number, serial_number
)
SELECT variant.id, user.id, 'https://listing.example/identity', 2006,
  250000, 1000, 'N123AB', '182-01234'
FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,
     aircraft_model_variants variant,
     users user
WHERE variant.id = placeholder.aircraft_model_variant_id
  AND placeholder.singleton_id = 1
  AND user.auth_subject='identity-test';

INSERT INTO curation_evidence_sources (
  source_url, resolved_url, source_title, publisher, source_domain,
  source_tier, content_sha256, retrieved_at
) VALUES (
  'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
  'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
  'FAA registry test release', 'Federal Aviation Administration', 'faa.gov',
  'regulator_primary', printf('%064d', 0), CURRENT_TIMESTAMP
);
INSERT INTO faa_registry_snapshots (
  evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256,
  master_member_name, master_member_sha256,
  aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256, record_hash_domain
)
SELECT id, '2026-07-22', source_url, content_sha256,
  printf('%064d', 1), printf('%064d', 2),
  'MASTER.txt', printf('%064d', 3),
  'ACFTREF.txt', printf('%064d', 4),
  'ENGINE.txt', printf('%064d', 5),
  'aircost-faa-master-retained-aircraft-projection-v1'
FROM curation_evidence_sources WHERE source_title='FAA registry test release';
INSERT INTO faa_registry_aircraft (
  snapshot_id, n_number, manufacturer_serial_raw, manufacturer_serial_key,
  aircraft_code, year_manufactured, source_record_sha256
)
SELECT id, 'N123AB', '182-01234', '18201234', '2072738', 2006, printf('%064d', 6)
FROM faa_registry_snapshots WHERE snapshot_date='2026-07-22';
INSERT INTO faa_registry_aircraft_references (
  snapshot_id, aircraft_code, manufacturer_name, model_name
)
SELECT id, '2072738', 'CESSNA AIRCRAFT CO', '18/2.T'
FROM faa_registry_snapshots WHERE snapshot_date='2026-07-22';
INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status)
SELECT id, 'N123AB', 'matched' FROM faa_registry_snapshots
WHERE snapshot_date='2026-07-22';

INSERT INTO curation_evidence_claims (
  evidence_source_id, claim_kind, subject_text, predicate_text,
  object_text, quoted_evidence, validation_status, validated_at
)
SELECT evidence_source_id, 'identity', 'N123AB', 'FAA registered aircraft identity',
  '{"aircraft_code":"2072738","manufacturer":"CESSNA AIRCRAFT CO","model":"18/2.T"}',
  'FAA ACFTREF 2072738 identifies CESSNA AIRCRAFT CO 18/2.T',
  'validated', CURRENT_TIMESTAMP
FROM faa_registry_snapshots WHERE snapshot_date='2026-07-22';
INSERT INTO aircraft_identity_observations (
  aircraft_sale_listing_id, observed_make, observed_family,
  observed_designation, exact_source_evidence, observation_sha256
)
SELECT id, 'CESSNA AIRCRAFT CO', '182', '182T',
  'Schema fixture with exact FAA identity evidence', printf('%064d', 7)
FROM aircraft_sale_listings WHERE source_url='https://listing.example/identity';
INSERT INTO aircraft_identity_resolution_cases (
  observation_id, resolution_scope, job_fingerprint, catalog_revision, case_status
)
SELECT id, scope, 'identity-schema-' || scope, 'schema-v1', 'resolved'
FROM aircraft_identity_observations,
  (SELECT 'make' AS scope UNION ALL SELECT 'family' UNION ALL SELECT 'designation');
INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT id, resolution_scope, 'approve_new', 'approved', '{}', '{"passed":true}',
  1, 'Schema test approved identity', CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases WHERE job_fingerprint LIKE 'identity-schema-%';
INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT id, 'alias', 'approve_new', 'approved', '{}', '{"passed":true}',
  1, 'Schema test approved FAA manufacturer alias', CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases WHERE resolution_scope='make';
INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT decision.id, claim.id, 'identity'
FROM aircraft_identity_decisions decision, curation_evidence_claims claim
WHERE claim.subject_text='N123AB';

INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id)
SELECT 'Cessna', 'cessna', id FROM aircraft_identity_decisions
WHERE entity_kind='make';
INSERT INTO aircraft_make_aliases (
  aircraft_make_id, alias, normalized_alias,
  valid_from_model_year, valid_to_model_year,
  aircraft_market_id, approval_decision_id
)
SELECT make.id, 'CESSNA AIRCRAFT CO', 'cessna aircraft co',
  2000, 2010, market.id, decision.id
FROM aircraft_makes make, aircraft_markets market, aircraft_identity_decisions decision
WHERE make.normalized_name='cessna' AND market.code='US'
  AND decision.entity_kind='alias';
INSERT INTO aircraft_model_families (
  aircraft_make_id, name, normalized_name, approval_decision_id
)
SELECT make.id, '182', '182', decision.id
FROM aircraft_makes make, aircraft_identity_decisions decision
WHERE make.normalized_name='cessna' AND decision.entity_kind='family';
INSERT INTO aircraft_designations (
  aircraft_model_family_id, official_designation,
  normalized_official_designation, display_name, approval_decision_id
)
SELECT family.id, '18-2 T', '182t', 'Cessna 18-2 T', decision.id
FROM aircraft_model_families family, aircraft_identity_decisions decision
WHERE family.normalized_name='182' AND decision.entity_kind='designation';
INSERT INTO aircraft_designation_faa_bindings (
  faa_snapshot_date, faa_archive_sha256, faa_aircraft_code,
  aircraft_designation_id, representative_faa_registry_snapshot_id,
  identity_evidence_claim_id
)
SELECT snapshot.snapshot_date, snapshot.archive_sha256, '2072738',
  designation.id, snapshot.id, claim.id
FROM faa_registry_snapshots snapshot, aircraft_designations designation,
     curation_evidence_claims claim
WHERE snapshot.snapshot_date='2026-07-22'
  AND designation.normalized_official_designation='182t'
  AND claim.subject_text='N123AB';
INSERT INTO aircraft_sale_listing_identity_assignments (
  aircraft_sale_listing_id, aircraft_make_id, aircraft_model_family_id,
  aircraft_designation_id, identity_decision_id, identity_evidence_claim_id,
  faa_registry_snapshot_id, faa_n_number, faa_source_record_sha256
)
SELECT listing.id, make.id, family.id, designation.id,
  decision.id, claim.id, snapshot.id, 'N123AB', aircraft.source_record_sha256
FROM aircraft_sale_listings listing, aircraft_makes make,
     aircraft_model_families family, aircraft_designations designation,
     aircraft_identity_decisions decision, curation_evidence_claims claim,
     faa_registry_snapshots snapshot, faa_registry_aircraft aircraft
WHERE listing.source_url='https://listing.example/identity'
  AND make.normalized_name='cessna' AND family.normalized_name='182'
  AND designation.normalized_official_designation='182t'
  AND decision.id=designation.approval_decision_id
  AND claim.subject_text='N123AB' AND snapshot.snapshot_date='2026-07-22'
  AND aircraft.snapshot_id=snapshot.id AND aircraft.n_number='N123AB';
INSERT INTO aircraft_valuation_projection_transitions (
  aircraft_sale_listing_id, identity_assignment_id,
  transition_kind, selected_at
)
SELECT aircraft_sale_listing_id, id, 'initial',
  'z00000000000000000001.000000000'
FROM aircraft_sale_listing_identity_assignments;
UPDATE aircraft_sale_listings
SET ingestion_state='ready', ingestion_completed_at=CURRENT_TIMESTAMP, is_verified=1
WHERE source_url='https://listing.example/identity';
SQL

test "$(sqlite3 "$test_database" "SELECT ingestion_state FROM aircraft_sale_listings WHERE source_url='https://listing.example/identity'")" = "ready"
test "$(sqlite3 "$test_database" "SELECT identity_key FROM aircraft_designation_identity_keys")" = "182t"
test "$(sqlite3 "$test_database" "SELECT identity_key FROM faa_registry_aircraft_reference_identity_keys WHERE faa_aircraft_code='2072738'")" = "182t"

expect_failure \
  "UPDATE aircraft_sale_listings SET model_year=2011 WHERE source_url='https://listing.example/identity'" \
  "ready assignment manufacturer aliases honor model-year applicability"
expect_failure \
  "UPDATE aircraft_make_aliases SET valid_to_model_year=2200 WHERE normalized_alias='cessna aircraft co'" \
  "approved FAA manufacturer aliases are immutable"

sqlite3 -bail "$test_database" <<'SQL'
INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT id, 'make', 'approve_new', 'approved', '{}', '{"passed":true}',
  1, 'Schema collision test make', CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases WHERE resolution_scope='make';
INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT id, 'alias', 'approve_new', 'approved', '{}', '{"passed":true}',
  1, 'Schema collision test alias', CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases WHERE resolution_scope='make';
INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT decision.id, claim.id, 'identity'
FROM aircraft_identity_decisions decision, curation_evidence_claims claim
WHERE decision.rationale LIKE 'Schema collision test %'
  AND claim.subject_text='N123AB';
INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id)
SELECT 'Unrelated Aircraft', 'unrelated aircraft', id
FROM aircraft_identity_decisions WHERE rationale='Schema collision test make';
SQL
expect_failure \
  "INSERT INTO aircraft_make_aliases (aircraft_make_id,alias,normalized_alias,valid_from_model_year,valid_to_model_year,aircraft_market_id,approval_decision_id) SELECT make.id,'CESSNA AIRCRAFT CO','cessna aircraft co',2005,2015,market.id,decision.id FROM aircraft_makes make,aircraft_markets market,aircraft_identity_decisions decision WHERE make.normalized_name='unrelated aircraft' AND market.code='US' AND decision.rationale='Schema collision test alias'" \
  "overlapping US/year aliases cannot resolve one FAA manufacturer to two makes"

# Build a three-version history so parent deletion exercises an arbitrary
# successor chain rather than only a root assignment.
sqlite3 -bail "$test_database" <<'SQL'
PRAGMA foreign_keys = ON;
UPDATE aircraft_sale_listings
SET ingestion_state = 'incomplete', ingestion_completed_at = NULL,
    is_verified = 0
WHERE source_url = 'https://listing.example/identity';
INSERT INTO aircraft_sale_listing_identity_assignments (
  aircraft_sale_listing_id, supersedes_assignment_id,
  aircraft_make_id, aircraft_model_family_id, aircraft_designation_id,
  aircraft_generation_id, aircraft_factory_package_id,
  identity_decision_id, identity_evidence_claim_id,
  faa_registry_snapshot_id, faa_n_number, faa_source_record_sha256
)
SELECT current_assignment.aircraft_sale_listing_id,
       current_assignment.identity_assignment_id,
       prior.aircraft_make_id, prior.aircraft_model_family_id,
       prior.aircraft_designation_id, prior.aircraft_generation_id,
       prior.aircraft_factory_package_id, prior.identity_decision_id,
       prior.identity_evidence_claim_id, prior.faa_registry_snapshot_id,
       prior.faa_n_number, prior.faa_source_record_sha256
FROM aircraft_sale_listing_current_identity_assignments current_assignment
JOIN aircraft_sale_listing_identity_assignments prior
  ON prior.id = current_assignment.identity_assignment_id;
INSERT INTO aircraft_valuation_projection_transitions (
  aircraft_sale_listing_id, identity_assignment_id,
  transition_kind, selected_at
)
SELECT current_assignment.aircraft_sale_listing_id, successor.id,
  'successor', 'z00000000000000000002.000000000'
FROM aircraft_sale_listing_current_identity_assignments current_assignment
JOIN aircraft_sale_listing_identity_assignments successor
  ON successor.supersedes_assignment_id =
       current_assignment.identity_assignment_id;

INSERT INTO aircraft_sale_listing_identity_assignments (
  aircraft_sale_listing_id, supersedes_assignment_id,
  aircraft_make_id, aircraft_model_family_id, aircraft_designation_id,
  aircraft_generation_id, aircraft_factory_package_id,
  identity_decision_id, identity_evidence_claim_id,
  faa_registry_snapshot_id, faa_n_number, faa_source_record_sha256
)
SELECT current_assignment.aircraft_sale_listing_id,
       current_assignment.identity_assignment_id,
       prior.aircraft_make_id, prior.aircraft_model_family_id,
       prior.aircraft_designation_id, prior.aircraft_generation_id,
       prior.aircraft_factory_package_id, prior.identity_decision_id,
       prior.identity_evidence_claim_id, prior.faa_registry_snapshot_id,
       prior.faa_n_number, prior.faa_source_record_sha256
FROM aircraft_sale_listing_current_identity_assignments current_assignment
JOIN aircraft_sale_listing_identity_assignments prior
  ON prior.id = current_assignment.identity_assignment_id;
INSERT INTO aircraft_valuation_projection_transitions (
  aircraft_sale_listing_id, identity_assignment_id,
  transition_kind, selected_at
)
SELECT current_assignment.aircraft_sale_listing_id, successor.id,
  'successor', 'z00000000000000000003.000000000'
FROM aircraft_sale_listing_current_identity_assignments current_assignment
JOIN aircraft_sale_listing_identity_assignments successor
  ON successor.supersedes_assignment_id =
       current_assignment.identity_assignment_id;
SQL
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_sale_listing_identity_assignments")" = "3"

expect_failure \
  "UPDATE aircraft_sale_listing_identity_assignments SET faa_n_number=faa_n_number" \
  "assignment versions are immutable"
expect_failure \
  "DELETE FROM aircraft_sale_listing_identity_assignments" \
  "assignment history cannot be deleted while its listing exists"
expect_failure \
  "DELETE FROM aircraft_sale_listing_current_identity_assignments" \
  "a live listing cannot lose its current pointer"
expect_failure \
  "UPDATE aircraft_designations SET display_name='mutated'" \
  "assigned hierarchy rows are immutable"
expect_failure \
  "INSERT INTO aircraft_designation_faa_bindings (faa_snapshot_date,faa_archive_sha256,faa_aircraft_code,aircraft_designation_id,representative_faa_registry_snapshot_id,identity_evidence_claim_id) SELECT faa_snapshot_date,faa_archive_sha256,faa_aircraft_code,aircraft_designation_id,representative_faa_registry_snapshot_id,identity_evidence_claim_id FROM aircraft_designation_faa_bindings" \
  "one FAA code has one designation within a release"

# A listing without an assignment cannot be promoted directly.
sqlite3 -bail "$test_database" <<'SQL'
INSERT INTO aircraft_sale_listings (
  aircraft_model_variant_id, created_by_user_id, source_url, model_year,
  asking_price_usd, airframe_hours, registration_number, serial_number
)
SELECT variant.id, user.id, 'https://listing.example/unassigned', 2006,
  200000, 900, 'N123AB', '182-01234'
FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,
     aircraft_model_variants variant,
     users user
WHERE variant.id = placeholder.aircraft_model_variant_id
  AND placeholder.singleton_id = 1
  AND user.auth_subject='identity-test';
SQL
expect_failure \
  "UPDATE aircraft_sale_listings SET ingestion_state='ready',ingestion_completed_at=CURRENT_TIMESTAMP WHERE source_url='https://listing.example/unassigned'" \
  "ready state requires an exact current assignment"

# Parent deletion is the only legal cascade for pointer/history deletion.
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "DELETE FROM aircraft_sale_listings WHERE source_url='https://listing.example/identity'"
test "$(sqlite3 "$test_database" "SELECT count(*) FROM aircraft_sale_listing_identity_assignments")" = "0"
test "$(sqlite3 "$test_database" "SELECT count(*) FROM aircraft_sale_listing_current_identity_assignments")" = "0"

# Upgrade postcondition: a grandfathered ready row is retained but quarantined.
sqlite3 -bail "$test_database" \
  "DROP TRIGGER listing_ready_requires_canonical_aircraft_update" \
  "DROP TRIGGER listing_ready_requires_aircraft_projection" \
  "DROP TRIGGER listing_ready_rejects_pending_aircraft_placeholder" \
  "UPDATE aircraft_sale_listings SET ingestion_state='ready',ingestion_completed_at=CURRENT_TIMESTAMP WHERE source_url='https://listing.example/unassigned'" \
  ".read $repository_root/migrations/20260725_listing_aircraft_identity.sqlite.sql"
test "$(sqlite3 "$test_database" "SELECT ingestion_state FROM aircraft_sale_listings WHERE source_url='https://listing.example/unassigned'")" = "quarantined"
[[ "$(sqlite3 "$test_database" "SELECT ingestion_error FROM aircraft_sale_listings WHERE source_url='https://listing.example/unassigned'")" == *"canonical aircraft identity migration"* ]]

test -z "$(sqlite3 "$test_database" "PRAGMA foreign_key_check")"
test "$(sqlite3 "$test_database" "PRAGMA integrity_check")" = "ok"

# Static Postgres parity for the same trust-boundary concepts.
postgres_migration="$repository_root/migrations/20260725_listing_aircraft_identity.postgres.sql"
for token in \
  aircraft_designation_faa_bindings \
  aircraft_sale_listing_identity_assignments \
  aircraft_sale_listing_current_identity_assignments \
  validate_aircraft_make_identity_alias \
  validate_listing_aircraft_identity_assignment \
  validate_ready_listing_aircraft_identity \
  preserve_listing_aircraft_identity_assignment \
  prevent_new_unresolved_aircraft_dimension; do
  grep -q "$token" "$postgres_migration"
done
grep -q "PRIMARY KEY (faa_snapshot_date, faa_archive_sha256, faa_aircraft_code)" "$postgres_migration"
grep -q "CONSTRAINT aircraft_listing_identity_assignment_supersedes_fk" "$postgres_migration"
grep -q "pg_get_constraintdef" "$postgres_migration"
grep -q "BEGIN;" "$postgres_migration"
grep -q "COMMIT;" "$postgres_migration"

echo "Listing aircraft identity migration contract passed"
