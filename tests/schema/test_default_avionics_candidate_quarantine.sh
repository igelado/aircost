#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
database="$(mktemp /tmp/aircost-default-avionics-candidates.XXXXXX.sqlite3)"
trap 'rm -f "$database"' EXIT

expect_failure() {
  local statement="$1"
  local description="$2"
  if sqlite3 -bail "$database" "PRAGMA foreign_keys=ON" "$statement" \
      >/dev/null 2>&1; then
    echo "Expected failure: $description" >&2
    exit 1
  fi
}

sqlite3 -bail "$database" \
  ".read $repository_root/schema/sqlite.sql"

# Recreate the immediately preceding schema: remove only the new candidate
# contract, table, and cross-table triggers before seeding migrated legacy rows.
sqlite3 -bail "$database" <<'SQL'
PRAGMA foreign_keys = ON;
DROP TRIGGER aircraft_default_avionics_candidate_admission_move;
DROP TRIGGER aircraft_default_avionics_candidate_admission_guard;
DROP TRIGGER aircraft_default_avionics_candidate_claim_immutable;
DROP TRIGGER aircraft_default_avionics_candidate_active_conflict_insert;
DROP TABLE aircraft_model_variant_default_avionics_candidates;
DELETE FROM schema_migration_contracts
WHERE migration_name = '20260802_default_avionics_candidate_quarantine';

INSERT INTO aircraft_manufacturers (name, normalized_name)
VALUES ('Cessna', 'cessna');
INSERT INTO aircraft_models (
  aircraft_manufacturer_id, name, normalized_name
)
SELECT id, '182', '182'
FROM aircraft_manufacturers
WHERE normalized_name = 'cessna';
INSERT INTO aircraft_model_variants (
  aircraft_model_id, name, normalized_name
)
SELECT id, '182T', '182t'
FROM aircraft_models
WHERE normalized_name = '182';

INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES ('Garmin', 'garmin');
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key, identity_evidence_kind,
  identity_source_url, identity_source_title, identity_evidence_text,
  identity_confidence
) VALUES (
  'Garmin', 'garmin', 'authoritative_reference',
  'https://www.garmin.com/en-US/p/588901/',
  'Garmin G1000 NXi | Integrated Flight Deck',
  'The Garmin G1000 NXi is an advanced integrated flight deck family designed and manufactured by Garmin',
  'very_high'
);
INSERT INTO avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id, avionics_manufacturer_identity_id,
  membership_basis, normalized_name_key, evidence_source_url,
  evidence_source_title, evidence_text, evidence_confidence
)
SELECT manufacturer.id, identity.id, 'authoritative_primary', 'garmin',
       identity.identity_source_url, identity.identity_source_title,
       identity.identity_evidence_text, 'very_high'
FROM avionics_manufacturers manufacturer
JOIN avionics_manufacturer_identities identity
  ON identity.normalized_identity_key = 'garmin'
WHERE manufacturer.normalized_name = 'garmin';

INSERT INTO avionics_types (name, normalized_name)
VALUES ('COM', 'com');
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text, identity_evidence_kind,
  identity_confidence, catalog_reviewed_at
)
SELECT id, 'GIA63W', 'gia63w',
       'manufacturer_model_number', 'GIA 63W', 'gia63w',
       'https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf',
       'Garmin GIA 63/GIA 63W Installation Manual',
       'GIA 63W Unit Only, (011-01105-00) 010-00386-00',
       'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers
WHERE normalized_name = 'garmin';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text, identity_evidence_kind,
  identity_confidence, catalog_reviewed_at
)
SELECT id, 'GFC 700', 'gfc700',
       'manufacturer_model_number', 'GFC 700', 'gfc700',
       'https://www.garmin.com/en-US/p/604257/',
       'Garmin GFC 700',
       'Garmin identifies the GFC 700 automatic flight control system.',
       'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers
WHERE normalized_name = 'garmin';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id
FROM avionics_models model
CROSS JOIN avionics_types type
WHERE model.normalized_name IN ('gia63w', 'gfc700')
  AND type.normalized_name = 'com';
UPDATE avionics_models
SET catalog_status = 'approved'
WHERE normalized_name = 'gfc700';

-- These rows model data that predates the approved-product insert guard.
DROP TRIGGER aircraft_model_variant_default_avionics_approved_insert;
INSERT INTO aircraft_model_variant_default_avionics (
  aircraft_model_variant_id, model_year, avionics_model_id, quantity,
  source_url, source_title, source_notes, source_confidence,
  created_at, updated_at
)
SELECT variant.id, 2010, model.id, 2,
       'https://example.test/gia63w-factory',
       '2010 Cessna factory equipment',
       'Exact legacy GIA 63W factory-default claim',
       'high', '2026-07-20 22:35:01', '2026-07-27 14:34:55'
FROM aircraft_model_variants variant
JOIN avionics_models model ON model.normalized_name = 'gia63w'
WHERE variant.normalized_name = '182t';
INSERT INTO aircraft_model_variant_default_avionics (
  aircraft_model_variant_id, model_year, avionics_model_id, quantity,
  source_url, source_title, source_notes, source_confidence,
  created_at, updated_at
)
SELECT variant.id, 2011, model.id, 1,
       'https://example.test/gfc700-factory',
       '2011 Cessna factory equipment',
       'Exact approved GFC 700 factory-default claim',
       'high', '2026-07-20 22:36:01', '2026-07-20 22:36:01'
FROM aircraft_model_variants variant
JOIN avionics_models model ON model.normalized_name = 'gfc700'
WHERE variant.normalized_name = '182t';
CREATE TRIGGER aircraft_model_variant_default_avionics_approved_insert
BEFORE INSERT ON aircraft_model_variant_default_avionics
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'default avionics association requires an approved catalog entry');
END;
SQL

legacy_default_id="$(sqlite3 "$database" \
  "SELECT default_avionics.id FROM aircraft_model_variant_default_avionics default_avionics JOIN avionics_models model ON model.id=default_avionics.avionics_model_id WHERE model.normalized_name='gia63w'")"

sqlite3 -bail "$database" \
  ".read $repository_root/migrations/20260802_default_avionics_candidate_quarantine.sqlite.sql"

# The revised migration explicitly upgrades the exact, already-installed v1
# SQLite contract; no separate SQLite repair migration is required.
sqlite3 -bail "$database" \
  "UPDATE schema_migration_contracts SET contract_version=1,contract_fingerprint='b50683c27b244cadf3cf88b226665f79051f678df9b30e0d01d0ca261464581f' WHERE migration_name='20260802_default_avionics_candidate_quarantine'"
sqlite3 -bail "$database" \
  ".read $repository_root/migrations/20260802_default_avionics_candidate_quarantine.sqlite.sql" \
  ".read $repository_root/migrations/20260802_default_avionics_candidate_quarantine.sqlite.sql"

test "$(sqlite3 "$database" \
  "SELECT count(*) FROM aircraft_model_variant_default_avionics default_avionics JOIN avionics_models model ON model.id=default_avionics.avionics_model_id WHERE model.catalog_status<>'approved'")" = "0"
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM aircraft_model_variant_default_avionics default_avionics JOIN avionics_models model ON model.id=default_avionics.avionics_model_id WHERE model.normalized_name='gfc700'")" = "1"
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM aircraft_model_variant_default_avionics_candidates")" = "1"
test "$(sqlite3 "$database" \
  "SELECT quarantined_default_avionics_id || ':' || pending_reason || ':' || quantity || ':' || source_url || ':' || source_title || ':' || source_notes || ':' || source_confidence || ':' || quarantined_created_at || ':' || quarantined_updated_at FROM aircraft_model_variant_default_avionics_candidates")" = \
  "$legacy_default_id:catalog_product_unverified:2:https://example.test/gia63w-factory:2010 Cessna factory equipment:Exact legacy GIA 63W factory-default claim:high:2026-07-20 22:35:01:2026-07-27 14:34:55"

# Pending rows are operationally separate from every valuation/default query.
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM aircraft_model_variant_default_avionics WHERE model_year=2010")" = "0"

# A generic future claim may remain pending even after its product is approved.
sqlite3 -bail "$database" \
  "INSERT INTO aircraft_model_variant_default_avionics_candidates (aircraft_model_variant_id,model_year,avionics_model_id,quantity,source_url,source_title,source_notes,source_confidence,pending_reason) SELECT variant.id,2012,model.id,1,'https://example.test/gfc700-pending','Pending 2012 factory claim','Needs primary corroboration','high','factory_default_claim_unverified' FROM aircraft_model_variants variant JOIN avionics_models model ON model.normalized_name='gfc700' WHERE variant.normalized_name='182t'"
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM aircraft_model_variant_default_avionics_candidates WHERE model_year=2012")" = "1"

expect_failure \
  "INSERT INTO aircraft_model_variant_default_avionics_candidates (aircraft_model_variant_id,model_year,avionics_model_id,quantity,source_url,source_title,source_notes,source_confidence,pending_reason) SELECT variant.id,2011,model.id,1,'https://example.test/duplicate','Duplicate claim','Duplicate claim','high','factory_default_claim_unverified' FROM aircraft_model_variants variant JOIN avionics_models model ON model.normalized_name='gfc700' WHERE variant.normalized_name='182t'" \
  "one claim cannot be pending and canonical simultaneously"
expect_failure \
  "UPDATE aircraft_model_variant_default_avionics_candidates SET quantity=3 WHERE model_year=2010" \
  "pending claim fields are immutable"

# Product approval and an exact canonical insert are the explicit admission
# operation. A non-exact insert is rejected; the exact insert moves the row.
sqlite3 -bail "$database" \
  "UPDATE avionics_models SET catalog_status='approved' WHERE normalized_name='gia63w'"
expect_failure \
  "INSERT INTO aircraft_model_variant_default_avionics (aircraft_model_variant_id,model_year,avionics_model_id,quantity,source_url,source_title,source_notes,source_confidence) SELECT aircraft_model_variant_id,model_year,avionics_model_id,1,source_url,source_title,source_notes,source_confidence FROM aircraft_model_variant_default_avionics_candidates WHERE model_year=2010" \
  "canonical admission must match every claim field"
sqlite3 -bail "$database" \
  "INSERT INTO aircraft_model_variant_default_avionics (aircraft_model_variant_id,model_year,avionics_model_id,quantity,source_url,source_title,source_notes,source_confidence,created_at,updated_at) SELECT aircraft_model_variant_id,model_year,avionics_model_id,quantity,source_url,source_title,source_notes,source_confidence,quarantined_created_at,quarantined_updated_at FROM aircraft_model_variant_default_avionics_candidates WHERE model_year=2010"
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM aircraft_model_variant_default_avionics_candidates WHERE model_year=2010")" = "0"
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM aircraft_model_variant_default_avionics WHERE model_year=2010")" = "1"

# Rejection is deletion; no rejected/garbage audit row remains.
sqlite3 -bail "$database" \
  "DELETE FROM aircraft_model_variant_default_avionics_candidates WHERE model_year=2012"
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM aircraft_model_variant_default_avionics_candidates")" = "0"

# Even exact-looking rows must never remain in both operational tables. Create
# deliberate drift with the normal insert guard temporarily absent and verify
# that rerunning the migration refuses to install its contract.
sqlite3 -bail "$database" <<'SQL'
DROP TRIGGER aircraft_default_avionics_candidate_active_conflict_insert;
INSERT INTO aircraft_model_variant_default_avionics_candidates (
  aircraft_model_variant_id, model_year, avionics_model_id, quantity,
  source_url, source_title, source_notes, source_confidence,
  pending_reason
)
SELECT aircraft_model_variant_id, model_year, avionics_model_id, quantity,
       source_url, source_title, source_notes, source_confidence,
       'factory_default_claim_unverified'
FROM aircraft_model_variant_default_avionics
WHERE model_year = 2011;
CREATE TRIGGER aircraft_default_avionics_candidate_active_conflict_insert
BEFORE INSERT ON aircraft_model_variant_default_avionics_candidates
WHEN EXISTS (
  SELECT 1
  FROM aircraft_model_variant_default_avionics active
  WHERE active.aircraft_model_variant_id = NEW.aircraft_model_variant_id
    AND active.model_year = NEW.model_year
    AND active.avionics_model_id = NEW.avionics_model_id
)
BEGIN
  SELECT RAISE(ABORT, 'default avionics claim already exists in the canonical table');
END;
SQL
if sqlite3 -bail "$database" \
    ".read $repository_root/migrations/20260802_default_avionics_candidate_quarantine.sqlite.sql" \
    >/dev/null 2>&1; then
  echo "Expected migration failure: canonical/pending semantic overlap" >&2
  exit 1
fi
sqlite3 -bail "$database" \
  "DELETE FROM aircraft_model_variant_default_avionics_candidates WHERE model_year=2011"

test "$(sqlite3 "$database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260802_default_avionics_candidate_quarantine'")" = \
  "2:b8a6ecd15acc0ce14f67bf37ff4387c0ded4d1c6669d2fc4698b6c0a6c209ba4"
test -z "$(sqlite3 "$database" "PRAGMA foreign_key_check")"
test "$(sqlite3 "$database" "PRAGMA integrity_check")" = "ok"

for required in \
  aircraft_model_variant_default_avionics_candidates \
  quarantined_default_avionics_id \
  catalog_product_unverified \
  factory_default_claim_unverified \
  aircraft_default_avionics_candidate_admission_guard \
  aircraft_default_avionics_candidate_admission_move \
  b8a6ecd15acc0ce14f67bf37ff4387c0ded4d1c6669d2fc4698b6c0a6c209ba4
do
  rg -q "$required" \
    "$repository_root/migrations/20260802_default_avionics_candidate_quarantine.sqlite.sql" \
    "$repository_root/migrations/20260802_default_avionics_candidate_quarantine.postgres.sql" \
    "$repository_root/schema/sqlite.sql" \
    "$repository_root/schema/postgres.sql"
done
