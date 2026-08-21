#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
migration="$repository_root/migrations/20260726_listing_aircraft_compatibility_projection.sqlite.sql"
fresh_database="$(mktemp /tmp/aircost-aircraft-projection-fresh.XXXXXX.sqlite3)"
test_database="$(mktemp /tmp/aircost-aircraft-projection-adversarial.XXXXXX.sqlite3)"
trap 'rm -f "$fresh_database" "$test_database"' EXIT

expect_failure() {
  local database="$1"
  local statement="$2"
  local description="$3"
  if sqlite3 -bail "$database" \
      "PRAGMA foreign_keys=ON" "$statement" >/dev/null 2>&1; then
    echo "Expected failure: $description" >&2
    exit 1
  fi
}

load_pre_projection_schema() {
  local database="$1"
  sed '/^-- FAA-backed aircraft valuation compatibility projection\.$/,$d' \
    "$repository_root/schema/sqlite.sql" | sqlite3 -bail "$database"
  sqlite3 -bail "$database" \
    ".read $repository_root/tests/schema/reference_catalog_pre_cutover.sqlite.sql"
}

# A fresh install is complete and a repeated install is idempotent.
sqlite3 -bail "$fresh_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/tests/schema/reference_catalog_pre_cutover.sqlite.sql" \
  ".read $migration" \
  ".read $migration"

test "$(sqlite3 "$fresh_database" \
  "SELECT aircraft_manufacturer_id || ':' || aircraft_model_id || ':' || aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id=1")" = "-1:-1:-1"
test "$(sqlite3 "$fresh_database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260726_listing_aircraft_compatibility_projection'")" = \
  "2:0a182d5972d62be3d906395df8d08b741bc3e23d713badf7596b360048aa45ba"

for object in \
  aircraft_sale_listing_pending_compatibility_placeholder \
  aircraft_listing_identity_input_observations \
  aircraft_valuation_compatibility_projections \
  aircraft_valuation_projection_transitions \
  aircraft_sale_listing_exact_compatibility_projections \
  aircraft_valuation_transition_validate_insert \
  aircraft_valuation_transition_execute \
  aircraft_valuation_projection_validate_insert \
  listing_insert_requires_aircraft_projection_or_placeholder \
  listing_aircraft_projection_transition_update \
  listing_current_identity_projection_insert \
  listing_current_identity_projection_update \
  listing_ready_requires_aircraft_projection \
  listing_ready_insert_requires_aircraft_projection; do
  test "$(sqlite3 "$fresh_database" \
    "SELECT count(*) FROM sqlite_schema WHERE name='$object'")" = "1"
done

projection_contract="$(sqlite3 "$fresh_database" \
  "SELECT sql FROM sqlite_schema WHERE type='table' AND name='aircraft_valuation_compatibility_projections'")"
[[ "$projection_contract" == *"CHECK (aircraft_make_id > 0)"* ]]
[[ "$projection_contract" == *"CHECK (aircraft_model_family_id > 0)"* ]]
[[ "$projection_contract" == *"CHECK (aircraft_designation_id > 0)"* ]]
[[ "$projection_contract" == *"CHECK (aircraft_generation_id IS NULL OR aircraft_generation_id > 0)"* ]]
[[ "$projection_contract" == *"CHECK (aircraft_factory_package_id IS NULL OR aircraft_factory_package_id > 0)"* ]]
test "$(sqlite3 "$fresh_database" "PRAGMA foreign_key_check")" = ""
test "$(sqlite3 "$fresh_database" "PRAGMA integrity_check")" = "ok"

# Build a valid pre-migration identity graph. Four retained legacy listings
# exercise initial, current-repair, reuse, and stale-assignment transitions.
load_pre_projection_schema "$test_database"
sqlite3 -bail "$test_database" <<'SQL'
PRAGMA foreign_keys = ON;

INSERT INTO users (email, display_name, auth_subject)
VALUES ('projection@example.test', 'Projection Test', 'projection-test');

INSERT INTO aircraft_manufacturers (name, normalized_name)
VALUES ('Raw Duplicate Maker', 'raw duplicate maker');
INSERT INTO aircraft_models (
  aircraft_manufacturer_id, name, normalized_name
)
SELECT id, 'Raw Duplicate Family', 'raw duplicate family'
FROM aircraft_manufacturers
WHERE normalized_name = 'raw duplicate maker';
INSERT INTO aircraft_model_variants (
  aircraft_model_id, name, normalized_name
)
SELECT id, 'Raw Duplicate Variant', 'raw duplicate variant'
FROM aircraft_models
WHERE normalized_name = 'raw duplicate family';

INSERT INTO aircraft_sale_listings (
  aircraft_model_variant_id, created_by_user_id, source_url,
  model_year, asking_price_usd, airframe_hours,
  registration_number, serial_number
)
SELECT variant.id, user.id, source_url, 2023, 525000, 400,
       n_number, serial_number
FROM aircraft_model_variants variant
CROSS JOIN users user
CROSS JOIN (
  SELECT 'https://listing.example/projection-creator' AS source_url,
         'N200AA' AS n_number, 'SERIAL-200' AS serial_number
  UNION ALL
  SELECT 'https://listing.example/current-repair',
         'N201AA', 'SERIAL-201'
  UNION ALL
  SELECT 'https://listing.example/projection-reuse',
         'N202AA', 'SERIAL-202'
  UNION ALL
  SELECT 'https://listing.example/stale-assignment',
         'N100AA', 'SERIAL-100'
) fixture
WHERE variant.normalized_name = 'raw duplicate variant'
  AND user.auth_subject = 'projection-test';

INSERT INTO curation_evidence_sources (
  source_url, resolved_url, source_title, publisher, source_domain,
  source_tier, content_sha256, retrieved_at
) VALUES
  (
    'https://faa.gov/registry/projection-release-1',
    'https://faa.gov/registry/projection-release-1',
    'FAA projection release 1', 'Federal Aviation Administration',
    'faa.gov', 'regulator_primary', printf('%064d', 10),
    CURRENT_TIMESTAMP
  ),
  (
    'https://faa.gov/registry/projection-release-2',
    'https://faa.gov/registry/projection-release-2',
    'FAA projection release 2', 'Federal Aviation Administration',
    'faa.gov', 'regulator_primary', printf('%064d', 20),
    CURRENT_TIMESTAMP
  );

INSERT INTO faa_registry_snapshots (
  evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256,
  master_member_name, master_member_sha256,
  aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256, record_hash_domain
)
SELECT id,
       CASE source_title
         WHEN 'FAA projection release 1' THEN '2026-07-20'
         ELSE '2026-07-21'
       END,
       source_url, content_sha256,
       CASE source_title
         WHEN 'FAA projection release 1' THEN printf('%064d', 11)
         ELSE printf('%064d', 21)
       END,
       CASE source_title
         WHEN 'FAA projection release 1' THEN printf('%064d', 12)
         ELSE printf('%064d', 22)
       END,
       'MASTER.txt',
       CASE source_title
         WHEN 'FAA projection release 1' THEN printf('%064d', 13)
         ELSE printf('%064d', 23)
       END,
       'ACFTREF.txt',
       CASE source_title
         WHEN 'FAA projection release 1' THEN printf('%064d', 14)
         ELSE printf('%064d', 24)
       END,
       'ENGINE.txt',
       CASE source_title
         WHEN 'FAA projection release 1' THEN printf('%064d', 15)
         ELSE printf('%064d', 25)
       END,
       'aircost-faa-master-retained-aircraft-projection-v1'
FROM curation_evidence_sources
WHERE source_title LIKE 'FAA projection release %';

INSERT INTO faa_registry_aircraft (
  snapshot_id, n_number, manufacturer_serial_raw,
  manufacturer_serial_key, aircraft_code, year_manufactured,
  source_record_sha256
)
SELECT snapshot.id, fixture.n_number, fixture.serial_number,
       replace(fixture.serial_number, '-', ''), '2072738', 2023,
       printf('%064d', fixture.record_number)
FROM faa_registry_snapshots snapshot
JOIN (
  SELECT '2026-07-20' AS snapshot_date, 'N100AA' AS n_number,
         'SERIAL-100' AS serial_number, 101 AS record_number
  UNION ALL
  SELECT '2026-07-21', 'N200AA', 'SERIAL-200', 201
  UNION ALL
  SELECT '2026-07-21', 'N201AA', 'SERIAL-201', 202
  UNION ALL
  SELECT '2026-07-21', 'N202AA', 'SERIAL-202', 203
) fixture ON fixture.snapshot_date = snapshot.snapshot_date;

INSERT INTO faa_registry_aircraft_references (
  snapshot_id, aircraft_code, manufacturer_name, model_name
)
SELECT id, '2072738', 'Cessna', '182T'
FROM faa_registry_snapshots;

INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status)
SELECT snapshot_id, n_number, 'matched'
FROM faa_registry_aircraft;

INSERT INTO curation_evidence_claims (
  evidence_source_id, claim_kind, subject_text, predicate_text,
  object_text, quoted_evidence, validation_status, validated_at
)
SELECT evidence_source_id, 'identity',
       CASE snapshot_date
         WHEN '2026-07-20' THEN 'projection-snapshot-1'
         ELSE 'projection-snapshot-2'
       END,
       'FAA registered aircraft identity',
       '{"aircraft_code":"2072738","manufacturer":"Cessna","model":"182T"}',
       'FAA ACFTREF 2072738 identifies Cessna 182T',
       'validated', CURRENT_TIMESTAMP
FROM faa_registry_snapshots;

INSERT INTO aircraft_identity_observations (
  observed_make, observed_family, observed_designation,
  exact_source_evidence, observation_sha256
) VALUES (
  'Cessna', '182', '182T',
  'Exact FAA-backed schema fixture evidence', printf('%064d', 301)
);

INSERT INTO aircraft_identity_resolution_cases (
  observation_id, resolution_scope, job_fingerprint,
  catalog_revision, case_status
)
SELECT observation.id, scope.name, 'projection-schema-' || scope.name,
       'projection-v1', 'resolved'
FROM aircraft_identity_observations observation
CROSS JOIN (
  SELECT 'make' AS name
  UNION ALL SELECT 'family'
  UNION ALL SELECT 'designation'
  UNION ALL SELECT 'generation'
  UNION ALL SELECT 'package'
) scope
WHERE observation.observation_sha256 = printf('%064d', 301);

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT id, resolution_scope, 'approve_new', 'approved',
       '{}', '{"passed":true}', 1,
       'projection fixture ' || resolution_scope, CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases
WHERE job_fingerprint LIKE 'projection-schema-%';

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT id, 'generation_designation', 'approve_new', 'approved',
       '{}', '{"passed":true}', 1,
       'projection fixture generation designation', CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases
WHERE resolution_scope = 'generation';

INSERT INTO aircraft_identity_decisions (
  resolution_case_id, entity_kind, decision_action, decision_status,
  decision_payload_json, deterministic_validation_json,
  deterministic_validation_passed, rationale, decided_at
)
SELECT id, 'package_applicability', 'approve_new', 'approved',
       '{}', '{"passed":true}', 1,
       'projection fixture package applicability', CURRENT_TIMESTAMP
FROM aircraft_identity_resolution_cases
WHERE resolution_scope = 'package';

INSERT INTO aircraft_identity_decision_claims (
  decision_id, evidence_claim_id, evidence_role
)
SELECT decision.id, claim.id, 'identity'
FROM aircraft_identity_decisions decision
CROSS JOIN curation_evidence_claims claim
WHERE decision.rationale LIKE 'projection fixture %'
  AND claim.subject_text = 'projection-snapshot-2';

INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id)
SELECT 'Cessna', 'cessna', id
FROM aircraft_identity_decisions
WHERE rationale = 'projection fixture make';

INSERT INTO aircraft_model_families (
  aircraft_make_id, name, normalized_name, approval_decision_id
)
SELECT make.id, '182', '182', decision.id
FROM aircraft_makes make
CROSS JOIN aircraft_identity_decisions decision
WHERE make.normalized_name = 'cessna'
  AND decision.rationale = 'projection fixture family';

INSERT INTO aircraft_designations (
  aircraft_model_family_id, official_designation,
  normalized_official_designation, display_name, approval_decision_id
)
SELECT family.id, '182T', '182t', 'Cessna 182T', decision.id
FROM aircraft_model_families family
CROSS JOIN aircraft_identity_decisions decision
WHERE family.normalized_name = '182'
  AND decision.rationale = 'projection fixture designation';

INSERT INTO aircraft_generations (
  aircraft_model_family_id, name, normalized_name, ordinal,
  approval_decision_id
)
SELECT family.id, 'G6', 'g6', 6, decision.id
FROM aircraft_model_families family
CROSS JOIN aircraft_identity_decisions decision
WHERE family.normalized_name = '182'
  AND decision.rationale = 'projection fixture generation';

INSERT INTO aircraft_generation_designations (
  aircraft_generation_id, aircraft_designation_id, approval_decision_id
)
SELECT generation.id, designation.id, decision.id
FROM aircraft_generations generation
CROSS JOIN aircraft_designations designation
CROSS JOIN aircraft_identity_decisions decision
WHERE generation.normalized_name = 'g6'
  AND designation.normalized_official_designation = '182t'
  AND decision.rationale = 'projection fixture generation designation';

INSERT INTO aircraft_factory_packages (
  aircraft_model_family_id, name, normalized_name, package_kind,
  exclusivity_group, approval_decision_id
)
SELECT family.id, 'GTS', 'gts', 'trim_tier',
       'projection-tier', decision.id
FROM aircraft_model_families family
CROSS JOIN aircraft_identity_decisions decision
WHERE family.normalized_name = '182'
  AND decision.rationale = 'projection fixture package';

INSERT INTO aircraft_package_applicability (
  aircraft_factory_package_id, aircraft_designation_id,
  aircraft_generation_id, valid_from_model_year, valid_to_model_year,
  approval_decision_id
)
SELECT package.id, designation.id, generation.id,
       2020, 2030, decision.id
FROM aircraft_factory_packages package
CROSS JOIN aircraft_designations designation
CROSS JOIN aircraft_generations generation
CROSS JOIN aircraft_identity_decisions decision
WHERE package.normalized_name = 'gts'
  AND designation.normalized_official_designation = '182t'
  AND generation.normalized_name = 'g6'
  AND decision.rationale = 'projection fixture package applicability';

INSERT INTO aircraft_designation_faa_bindings (
  faa_snapshot_date, faa_archive_sha256, faa_aircraft_code,
  aircraft_designation_id, representative_faa_registry_snapshot_id,
  identity_evidence_claim_id
)
SELECT snapshot.snapshot_date, snapshot.archive_sha256, '2072738',
       designation.id, snapshot.id, claim.id
FROM faa_registry_snapshots snapshot
CROSS JOIN aircraft_designations designation
JOIN curation_evidence_claims claim
  ON claim.evidence_source_id = snapshot.evidence_source_id
 AND claim.claim_kind = 'identity'
WHERE designation.normalized_official_designation = '182t';

INSERT INTO aircraft_sale_listing_identity_assignments (
  aircraft_sale_listing_id, aircraft_make_id, aircraft_model_family_id,
  aircraft_designation_id, aircraft_generation_id,
  aircraft_factory_package_id, identity_decision_id,
  identity_evidence_claim_id, faa_registry_snapshot_id, faa_n_number,
  faa_source_record_sha256
)
SELECT listing.id, make.id, family.id, designation.id,
       generation.id, package.id, designation.approval_decision_id,
       claim.id, snapshot.id, aircraft.n_number,
       aircraft.source_record_sha256
FROM aircraft_sale_listings listing
CROSS JOIN aircraft_makes make
CROSS JOIN aircraft_model_families family
CROSS JOIN aircraft_designations designation
CROSS JOIN aircraft_generations generation
CROSS JOIN aircraft_factory_packages package
JOIN faa_registry_aircraft aircraft
  ON aircraft.n_number = listing.registration_number
JOIN faa_registry_snapshots snapshot
  ON snapshot.id = aircraft.snapshot_id
 AND snapshot.snapshot_date = '2026-07-21'
JOIN curation_evidence_claims claim
  ON claim.evidence_source_id = snapshot.evidence_source_id
 AND claim.claim_kind = 'identity'
WHERE listing.source_url IN (
    'https://listing.example/projection-creator',
    'https://listing.example/current-repair',
    'https://listing.example/projection-reuse'
  )
  AND make.normalized_name = 'cessna'
  AND family.normalized_name = '182'
  AND designation.normalized_official_designation = '182t'
  AND generation.normalized_name = 'g6'
  AND package.normalized_name = 'gts';

INSERT INTO aircraft_sale_listing_identity_assignments (
  aircraft_sale_listing_id, aircraft_make_id, aircraft_model_family_id,
  aircraft_designation_id, aircraft_generation_id,
  aircraft_factory_package_id, identity_decision_id,
  identity_evidence_claim_id, faa_registry_snapshot_id, faa_n_number,
  faa_source_record_sha256
)
SELECT listing.id, make.id, family.id, designation.id,
       generation.id, package.id, designation.approval_decision_id,
       claim.id, snapshot.id, aircraft.n_number,
       aircraft.source_record_sha256
FROM aircraft_sale_listings listing
CROSS JOIN aircraft_makes make
CROSS JOIN aircraft_model_families family
CROSS JOIN aircraft_designations designation
CROSS JOIN aircraft_generations generation
CROSS JOIN aircraft_factory_packages package
JOIN faa_registry_aircraft aircraft
  ON aircraft.n_number = listing.registration_number
JOIN faa_registry_snapshots snapshot
  ON snapshot.id = aircraft.snapshot_id
 AND snapshot.snapshot_date = '2026-07-20'
JOIN curation_evidence_claims claim
  ON claim.evidence_source_id = snapshot.evidence_source_id
 AND claim.claim_kind = 'identity'
WHERE listing.source_url = 'https://listing.example/stale-assignment'
  AND make.normalized_name = 'cessna'
  AND family.normalized_name = '182'
  AND designation.normalized_official_designation = '182t'
  AND generation.normalized_name = 'g6'
  AND package.normalized_name = 'gts';

INSERT INTO aircraft_sale_listing_current_identity_assignments (
  aircraft_sale_listing_id, identity_assignment_id, selected_at
)
SELECT listing.id, assignment.id, 'z00000000000000000001.000000000'
FROM aircraft_sale_listings listing
JOIN aircraft_sale_listing_identity_assignments assignment
  ON assignment.aircraft_sale_listing_id = listing.id
WHERE listing.source_url = 'https://listing.example/current-repair';
SQL

sqlite3 -bail "$test_database" ".read $migration"

test "$(sqlite3 "$test_database" \
  "SELECT aircraft_manufacturer_id || ':' || aircraft_model_id || ':' || aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id=1")" = "-1:-1:-1"

# The placeholder and every row in its schema-owned legacy hierarchy are
# immutable. Raw same-name rows cannot be adopted by a new listing.
expect_failure "$test_database" \
  "UPDATE aircraft_sale_listing_pending_compatibility_placeholder SET aircraft_model_variant_id=aircraft_model_variant_id" \
  "the pending placeholder row is immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_sale_listing_pending_compatibility_placeholder" \
  "the pending placeholder row cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_manufacturers SET name='forged' WHERE id=-1" \
  "the pending manufacturer is immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_manufacturers WHERE id=-1" \
  "the pending manufacturer cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_models SET name='forged' WHERE id=-1" \
  "the pending model is immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_models WHERE id=-1" \
  "the pending model cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_model_variants SET name='forged' WHERE id=-1" \
  "the pending variant is immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_model_variants WHERE id=-1" \
  "the pending variant cannot be deleted"
expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listings (aircraft_model_variant_id,created_by_user_id,source_url,model_year,asking_price_usd,airframe_hours) SELECT variant.id,user.id,'https://listing.example/raw-rejected',2023,525000,400 FROM aircraft_model_variants variant,users user WHERE variant.normalized_name='raw duplicate variant' AND user.auth_subject='projection-test'" \
  "a new listing cannot adopt a raw same-name aircraft row"
expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listings (aircraft_model_variant_id,created_by_user_id,source_url,model_year,asking_price_usd,airframe_hours,ingestion_state,ingestion_completed_at) SELECT placeholder.aircraft_model_variant_id,user.id,'https://listing.example/insert-ready',2023,525000,400,'ready',CURRENT_TIMESTAMP FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,users user WHERE placeholder.singleton_id=1 AND user.auth_subject='projection-test'" \
  "a listing cannot bypass assignment and projection by inserting ready"

# Placeholder insertion is the only unresolved write boundary. Raw input
# observations are append-only and are not deleted with their parent listing.
sqlite3 -bail "$test_database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO aircraft_sale_listings (
  aircraft_model_variant_id, created_by_user_id, source_url,
  model_year, asking_price_usd, airframe_hours,
  registration_number, serial_number
)
SELECT placeholder.aircraft_model_variant_id, user.id,
       'https://listing.example/pending-placeholder',
       2023, 525000, 400, 'N299AA', 'SERIAL-299'
FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
CROSS JOIN users user
WHERE placeholder.singleton_id = 1
  AND user.auth_subject = 'projection-test';

INSERT INTO aircraft_listing_identity_input_observations (
  aircraft_sale_listing_id, source_url, observed_make, observed_family,
  observed_designation, model_year, serial_number, registration_number,
  input_json, observation_sha256
)
SELECT id, source_url, 'Raw Duplicate Maker', 'Raw Duplicate Family',
       'Raw Duplicate Variant', model_year, serial_number,
       registration_number, '{"observation_kind":"literal_listing_input"}',
       printf('%064d', 401)
FROM aircraft_sale_listings
WHERE source_url = 'https://listing.example/pending-placeholder';
SQL

test "$(sqlite3 "$test_database" \
  "SELECT aircraft_model_variant_id FROM aircraft_sale_listings WHERE source_url='https://listing.example/pending-placeholder'")" = "-1"
expect_failure "$test_database" \
  "UPDATE aircraft_listing_identity_input_observations SET observed_make='forged' WHERE observation_sha256=printf('%064d',401)" \
  "raw input observations are append-only"
expect_failure "$test_database" \
  "DELETE FROM aircraft_listing_identity_input_observations WHERE observation_sha256=printf('%064d',401)" \
  "raw input observations cannot be erased directly"
expect_failure "$test_database" \
  "UPDATE aircraft_sale_listings SET ingestion_state='ready',ingestion_completed_at=CURRENT_TIMESTAMP WHERE source_url='https://listing.example/pending-placeholder'" \
  "the pending placeholder cannot become ready"

# No caller can insert a projection, select a current assignment, or repoint a
# listing without the exact short-lived transition command.
expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_compatibility_projections (aircraft_model_variant_id,aircraft_make_id,aircraft_model_family_id,aircraft_designation_id,aircraft_generation_id,aircraft_factory_package_id,created_from_aircraft_sale_listing_id,created_from_identity_assignment_id,identity_decision_id,identity_evidence_claim_id,faa_registry_snapshot_id,faa_n_number,faa_source_record_sha256) SELECT variant.id,assignment.aircraft_make_id,assignment.aircraft_model_family_id,assignment.aircraft_designation_id,assignment.aircraft_generation_id,assignment.aircraft_factory_package_id,listing.id,assignment.id,assignment.identity_decision_id,assignment.identity_evidence_claim_id,assignment.faa_registry_snapshot_id,assignment.faa_n_number,assignment.faa_source_record_sha256 FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id JOIN aircraft_model_variants variant ON variant.normalized_name='raw duplicate variant' WHERE listing.source_url='https://listing.example/projection-creator'" \
  "a caller cannot directly insert a compatibility projection"
expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listing_current_identity_assignments (aircraft_sale_listing_id,identity_assignment_id,selected_at) SELECT listing.id,assignment.id,'z00000000000000000002.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id WHERE listing.source_url='https://listing.example/projection-creator'" \
  "a caller cannot directly select an assignment before projection"
expect_failure "$test_database" \
  "UPDATE aircraft_sale_listings SET aircraft_model_variant_id=-1 WHERE source_url='https://listing.example/projection-creator'" \
  "a caller cannot directly change the compatibility foreign key"
expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT creator.id,reuse_assignment.id,'initial','z00000000000000000003.000000000' FROM aircraft_sale_listings creator CROSS JOIN aircraft_sale_listings reuse JOIN aircraft_sale_listing_identity_assignments reuse_assignment ON reuse_assignment.aircraft_sale_listing_id=reuse.id WHERE creator.source_url='https://listing.example/projection-creator' AND reuse.source_url='https://listing.example/projection-reuse'" \
  "a transition cannot borrow another listing's assignment"

# Stale assignments and wrong transition kinds fail before any durable command
# or compatibility row is created.
expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,assignment.id,'initial','z00000000000000000004.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id WHERE listing.source_url='https://listing.example/stale-assignment'" \
  "a transition cannot use an assignment from an older FAA snapshot"
expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,assignment.id,'successor','z00000000000000000005.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id WHERE listing.source_url='https://listing.example/projection-creator'" \
  "a root assignment is not a successor transition"
expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,assignment.id,'current_repair','z00000000000000000006.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id WHERE listing.source_url='https://listing.example/projection-creator'" \
  "current repair requires an existing current pointer"
expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,assignment.id,'initial','z00000000000000000007.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id WHERE listing.source_url='https://listing.example/current-repair'" \
  "initial transition cannot replace an existing current pointer"

make_id="$(sqlite3 "$test_database" \
  "SELECT id FROM aircraft_makes WHERE normalized_name='cessna'")"
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "INSERT INTO aircraft_manufacturers (name,normalized_name) VALUES ('Forged Reserved Make','__aircost_projection_make_${make_id}__')"
expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,assignment.id,'initial','z00000000000000000008.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id WHERE listing.source_url='https://listing.example/projection-creator'" \
  "a caller-preseeded reserved hierarchy key cannot be adopted"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_valuation_projection_transitions")" = "0"
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "DELETE FROM aircraft_manufacturers WHERE normalized_name='__aircost_projection_make_${make_id}__'"

# Exact initial, current-repair, and second initial commands all close
# themselves. One canonical tuple always reuses one compatibility projection.
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,assignment.id,'initial','z00000000000000000010.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id WHERE listing.source_url='https://listing.example/projection-creator'"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_valuation_projection_transitions")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_valuation_compatibility_projections")" = "1"

sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,assignment.id,'current_repair','z00000000000000000011.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id WHERE listing.source_url='https://listing.example/current-repair'"
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,assignment.id,'initial','z00000000000000000012.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments assignment ON assignment.aircraft_sale_listing_id=listing.id WHERE listing.source_url='https://listing.example/projection-reuse'"

test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_valuation_projection_transitions")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_valuation_compatibility_projections")" = "1"
test "$(sqlite3 "$test_database" \
  "SELECT count(DISTINCT aircraft_model_variant_id) FROM aircraft_sale_listings WHERE source_url IN ('https://listing.example/projection-creator','https://listing.example/current-repair','https://listing.example/projection-reuse')")" = "1"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_sale_listing_exact_compatibility_projections exact_projection JOIN aircraft_sale_listings listing ON listing.id=exact_projection.listing_id WHERE listing.source_url IN ('https://listing.example/projection-creator','https://listing.example/current-repair','https://listing.example/projection-reuse')")" = "3"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_valuation_compatibility_projections WHERE aircraft_make_id>0 AND aircraft_model_family_id>0 AND aircraft_designation_id>0 AND aircraft_generation_id>0 AND aircraft_factory_package_id>0 AND created_from_aircraft_sale_listing_id>0 AND created_from_identity_assignment_id>0")" = "1"

# Publication requires the exact projection. A correctly projected listing can
# become ready, while stale and placeholder listings cannot.
expect_failure "$test_database" \
  "UPDATE aircraft_sale_listings SET ingestion_state='ready',ingestion_completed_at=CURRENT_TIMESTAMP WHERE source_url='https://listing.example/stale-assignment'" \
  "a stale unprojected listing cannot become ready"
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "UPDATE aircraft_sale_listings SET ingestion_state='ready',ingestion_error=NULL,ingestion_completed_at=CURRENT_TIMESTAMP,is_verified=1 WHERE source_url='https://listing.example/projection-creator'"
test "$(sqlite3 "$test_database" \
  "SELECT ingestion_state FROM aircraft_sale_listings WHERE source_url='https://listing.example/projection-creator'")" = "ready"

# A successor cannot run while ready, cannot bypass the current-pointer guard,
# and must use the successor transition kind. It reuses the same projection.
sqlite3 -bail "$test_database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO aircraft_sale_listing_identity_assignments (
  aircraft_sale_listing_id, supersedes_assignment_id,
  aircraft_make_id, aircraft_model_family_id, aircraft_designation_id,
  aircraft_generation_id, aircraft_factory_package_id,
  identity_decision_id, identity_evidence_claim_id,
  faa_registry_snapshot_id, faa_n_number, faa_source_record_sha256
)
SELECT listing.id, root.id, root.aircraft_make_id,
       root.aircraft_model_family_id, root.aircraft_designation_id,
       root.aircraft_generation_id, root.aircraft_factory_package_id,
       root.identity_decision_id, root.identity_evidence_claim_id,
       root.faa_registry_snapshot_id, root.faa_n_number,
       root.faa_source_record_sha256
FROM aircraft_sale_listings listing
JOIN aircraft_sale_listing_identity_assignments root
  ON root.aircraft_sale_listing_id = listing.id
 AND root.supersedes_assignment_id IS NULL
WHERE listing.source_url = 'https://listing.example/projection-creator';
SQL

expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,successor.id,'successor','z00000000000000000020.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments successor ON successor.aircraft_sale_listing_id=listing.id AND successor.supersedes_assignment_id IS NOT NULL WHERE listing.source_url='https://listing.example/projection-creator'" \
  "a ready listing cannot run an identity transition"
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "UPDATE aircraft_sale_listings SET ingestion_state='incomplete',ingestion_completed_at=NULL,is_verified=0 WHERE source_url='https://listing.example/projection-creator'"
expect_failure "$test_database" \
  "UPDATE aircraft_sale_listing_current_identity_assignments SET identity_assignment_id=(SELECT successor.id FROM aircraft_sale_listing_identity_assignments successor JOIN aircraft_sale_listings listing ON listing.id=successor.aircraft_sale_listing_id WHERE listing.source_url='https://listing.example/projection-creator' AND successor.supersedes_assignment_id IS NOT NULL),selected_at='z00000000000000000020.000000000' WHERE aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/projection-creator')" \
  "a caller cannot directly advance the current assignment"
expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,successor.id,'initial','z00000000000000000020.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments successor ON successor.aircraft_sale_listing_id=listing.id AND successor.supersedes_assignment_id IS NOT NULL WHERE listing.source_url='https://listing.example/projection-creator'" \
  "a successor assignment cannot use initial transition kind"
expect_failure "$test_database" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,successor.id,'current_repair','z00000000000000000020.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments successor ON successor.aircraft_sale_listing_id=listing.id AND successor.supersedes_assignment_id IS NOT NULL WHERE listing.source_url='https://listing.example/projection-creator'" \
  "a successor assignment cannot use current-repair transition kind"

projection_id_before_successor="$(sqlite3 "$test_database" \
  "SELECT aircraft_model_variant_id FROM aircraft_sale_listings WHERE source_url='https://listing.example/projection-creator'")"
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "INSERT INTO aircraft_valuation_projection_transitions (aircraft_sale_listing_id,identity_assignment_id,transition_kind,selected_at) SELECT listing.id,successor.id,'successor','z00000000000000000020.000000000' FROM aircraft_sale_listings listing JOIN aircraft_sale_listing_identity_assignments successor ON successor.aircraft_sale_listing_id=listing.id AND successor.supersedes_assignment_id IS NOT NULL WHERE listing.source_url='https://listing.example/projection-creator'"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_valuation_projection_transitions")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT aircraft_model_variant_id FROM aircraft_sale_listings WHERE source_url='https://listing.example/projection-creator'")" = "$projection_id_before_successor"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_sale_listing_current_identity_assignments current_assignment JOIN aircraft_sale_listing_identity_assignments successor ON successor.id=current_assignment.identity_assignment_id AND successor.supersedes_assignment_id IS NOT NULL JOIN aircraft_sale_listings listing ON listing.id=current_assignment.aircraft_sale_listing_id WHERE listing.source_url='https://listing.example/projection-creator'")" = "1"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_valuation_compatibility_projections")" = "1"
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "UPDATE aircraft_sale_listings SET ingestion_state='ready',ingestion_error=NULL,ingestion_completed_at=CURRENT_TIMESTAMP,is_verified=1 WHERE source_url='https://listing.example/projection-creator'"

# The immutable projection freezes both canonical and generated legacy
# hierarchy rows, including selected generation/package applicability.
expect_failure "$test_database" \
  "UPDATE aircraft_valuation_compatibility_projections SET created_at=created_at" \
  "compatibility projections are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_valuation_compatibility_projections" \
  "compatibility projections cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_model_variants SET name=name WHERE id=$projection_id_before_successor" \
  "projected legacy variants are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_model_variants WHERE id=$projection_id_before_successor" \
  "projected legacy variants cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_models SET name=name WHERE id=(SELECT aircraft_model_id FROM aircraft_model_variants WHERE id=$projection_id_before_successor)" \
  "projected legacy models are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_models WHERE id=(SELECT aircraft_model_id FROM aircraft_model_variants WHERE id=$projection_id_before_successor)" \
  "projected legacy models cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_manufacturers SET name=name WHERE id=(SELECT model.aircraft_manufacturer_id FROM aircraft_model_variants variant JOIN aircraft_models model ON model.id=variant.aircraft_model_id WHERE variant.id=$projection_id_before_successor)" \
  "projected legacy manufacturers are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_manufacturers WHERE id=(SELECT model.aircraft_manufacturer_id FROM aircraft_model_variants variant JOIN aircraft_models model ON model.id=variant.aircraft_model_id WHERE variant.id=$projection_id_before_successor)" \
  "projected legacy manufacturers cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_makes SET name=name WHERE normalized_name='cessna'" \
  "projected canonical makes are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_makes WHERE normalized_name='cessna'" \
  "projected canonical makes cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_model_families SET name=name WHERE normalized_name='182'" \
  "projected canonical families are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_model_families WHERE normalized_name='182'" \
  "projected canonical families cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_designations SET display_name=display_name WHERE normalized_official_designation='182t'" \
  "projected canonical designations are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_designations WHERE normalized_official_designation='182t'" \
  "projected canonical designations cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_generations SET name=name WHERE normalized_name='g6'" \
  "projected canonical generations are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_generations WHERE normalized_name='g6'" \
  "projected canonical generations cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_factory_packages SET name=name WHERE normalized_name='gts'" \
  "projected canonical packages are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_factory_packages WHERE normalized_name='gts'" \
  "projected canonical packages cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_generation_designations SET created_at=created_at" \
  "projected generation applicability is immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_generation_designations" \
  "projected generation applicability cannot be deleted"
expect_failure "$test_database" \
  "UPDATE aircraft_package_applicability SET valid_to_model_year=valid_to_model_year" \
  "projected package applicability is immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_package_applicability" \
  "projected package applicability cannot be deleted"

# Parent deletion may null the raw-observation FK but must retain the input
# evidence. Projection provenance deliberately survives deletion of its source
# listing and immutable assignment history.
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "DELETE FROM aircraft_sale_listings WHERE source_url='https://listing.example/pending-placeholder'"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_listing_identity_input_observations WHERE observation_sha256=printf('%064d',401) AND aircraft_sale_listing_id IS NULL")" = "1"
expect_failure "$test_database" \
  "DELETE FROM aircraft_listing_identity_input_observations WHERE observation_sha256=printf('%064d',401)" \
  "orphaned raw input observations remain append-only"

creator_listing_id="$(sqlite3 "$test_database" \
  "SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/projection-creator'")"
creator_root_assignment_id="$(sqlite3 "$test_database" \
  "SELECT assignment.id FROM aircraft_sale_listing_identity_assignments assignment WHERE assignment.aircraft_sale_listing_id=$creator_listing_id AND assignment.supersedes_assignment_id IS NULL")"
test "$(sqlite3 "$test_database" \
  "SELECT created_from_aircraft_sale_listing_id || ':' || created_from_identity_assignment_id FROM aircraft_valuation_compatibility_projections")" = \
  "$creator_listing_id:$creator_root_assignment_id"

sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "DELETE FROM aircraft_sale_listings WHERE id=$creator_listing_id"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_sale_listings WHERE id=$creator_listing_id")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_sale_listing_identity_assignments WHERE aircraft_sale_listing_id=$creator_listing_id")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_sale_listing_current_identity_assignments WHERE aircraft_sale_listing_id=$creator_listing_id")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT aircraft_model_variant_id || ':' || created_from_aircraft_sale_listing_id || ':' || created_from_identity_assignment_id FROM aircraft_valuation_compatibility_projections")" = \
  "$projection_id_before_successor:$creator_listing_id:$creator_root_assignment_id"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_sale_listing_exact_compatibility_projections exact_projection JOIN aircraft_sale_listings listing ON listing.id=exact_projection.listing_id WHERE listing.source_url IN ('https://listing.example/current-repair','https://listing.example/projection-reuse')")" = "2"

# Reapplication cannot rewrite the preserved projection or leave a reusable
# command row. The completed database remains internally consistent.
sqlite3 -bail "$test_database" ".read $migration"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM aircraft_valuation_projection_transitions")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT aircraft_model_variant_id || ':' || created_from_aircraft_sale_listing_id || ':' || created_from_identity_assignment_id FROM aircraft_valuation_compatibility_projections")" = \
  "$projection_id_before_successor:$creator_listing_id:$creator_root_assignment_id"
test "$(sqlite3 "$test_database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260726_listing_aircraft_compatibility_projection'")" = \
  "2:0a182d5972d62be3d906395df8d08b741bc3e23d713badf7596b360048aa45ba"
test "$(sqlite3 "$test_database" "PRAGMA foreign_key_check")" = ""
test "$(sqlite3 "$test_database" "PRAGMA integrity_check")" = "ok"

echo "Listing aircraft compatibility projection SQLite contract passed"
