#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_database="$(mktemp /tmp/aircost-identity-postconditions.XXXXXX.sqlite3)"
upgrade_database="$(mktemp /tmp/aircost-identity-upgrade.XXXXXX.sqlite3)"
invalid_membership_database="$(mktemp /tmp/aircost-identity-invalid-maker.XXXXXX.sqlite3)"
invalid_product_database="$(mktemp /tmp/aircost-identity-invalid-product.XXXXXX.sqlite3)"
unauthorized_scope_database="$(mktemp /tmp/aircost-identity-unauthorized-scope.XXXXXX.sqlite3)"
trap 'rm -f "$test_database" "$upgrade_database" "$invalid_membership_database" "$invalid_product_database" "$unauthorized_scope_database"' EXIT

expect_failure() {
  local database="$1"
  local statement="$2"
  local description="$3"
  if sqlite3 -bail "$database" "PRAGMA foreign_keys=ON" "$statement" >/dev/null 2>&1; then
    echo "Expected failure: $description" >&2
    exit 1
  fi
}

expect_migration_failure() {
  local database="$1"
  local description="$2"
  if sqlite3 -bail "$database" \
      ".read $repository_root/migrations/20260725_identity_deduplication_postconditions.sqlite.sql" \
      >/dev/null 2>&1; then
    echo "Expected migration failure: $description" >&2
    exit 1
  fi
}

sqlite3 -bail "$test_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/tests/schema/reference_catalog_pre_cutover.sqlite.sql" \
  ".read $repository_root/migrations/20260725_identity_deduplication_postconditions.sqlite.sql" \
  ".read $repository_root/migrations/20260725_identity_deduplication_postconditions.sqlite.sql"

# Rehearse a pre-contract database that could contain two exact identifiers
# under one raw manufacturer row. Sharing that row is not identity evidence:
# both products must have evidence-backed effective manufacturer memberships.
sqlite3 -bail "$unauthorized_scope_database" <<SQL
.read $repository_root/schema/sqlite.sql
.read $repository_root/tests/schema/reference_catalog_pre_cutover.sqlite.sql
PRAGMA foreign_keys = ON;
DROP INDEX idx_avionics_models_manufacturer_identifier;
INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES ('Uncurated Maker', 'uncurated maker');
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Legacy Product A', 'legacy product a',
       'manufacturer_part_number', 'RAW-100', 'raw-100'
FROM avionics_manufacturers WHERE normalized_name='uncurated maker';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Legacy Product B', 'legacy product b',
       'manufacturer_part_number', 'RAW 100', 'raw 100'
FROM avionics_manufacturers WHERE normalized_name='uncurated maker';
SQL
expect_failure "$unauthorized_scope_database" \
  "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id,survivor_model_id) SELECT duplicate.id,survivor.id FROM avionics_models duplicate,avionics_models survivor WHERE duplicate.name='Legacy Product A' AND survivor.name='Legacy Product B'" \
  "sharing a raw manufacturer row is not consolidation authorization"

sqlite3 -bail "$test_database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES ('Acme Aero', 'acme aero'), ('Acme-Aero', 'acme-aero');
INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES ('Mutable Key Co', 'mutable key co');
INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES
  ('Garmin', 'garmin'),
  ('Garmin', 'garmin-forged'),
  ('Aircraft Radio Corporation', 'aircraft radio');
INSERT INTO avionics_types (name, normalized_name) VALUES ('Navigator', 'navigator');
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key, verification_method,
  identity_evidence_kind,
  identity_source_url, identity_source_title, identity_evidence_text,
  identity_confidence
) VALUES (
  'Acme Aero', 'acmeaero', 'automated', 'authoritative_reference',
  'https://manufacturer.example/about', 'Acme manufacturer profile',
  'The manufacturer identifies Acme Aero as its product brand.', 'very_high'
);
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key, verification_method,
  identity_evidence_kind,
  identity_source_url, identity_source_title, identity_evidence_text,
  identity_confidence
) VALUES
  (
    'Garmin', 'garmin', 'automated', 'authoritative_reference',
    'https://manufacturer.example/garmin', 'Garmin manufacturer profile',
    'The manufacturer identifies Garmin as its product brand.', 'very_high'
  ),
  (
    'Forged Garmin', 'garminforged', 'automated', 'authoritative_reference',
    'https://manufacturer.example/forged', 'Forged manufacturer profile',
    'Evidence fixture for a forged stored normalization.', 'very_high'
  ),
  (
    'Aircraft Radio', 'aircraftradio', 'automated', 'authoritative_reference',
    'https://manufacturer.example/aircraft-radio', 'Aircraft Radio profile',
    'The manufacturer identifies its full corporate display name.', 'very_high'
  );
INSERT INTO avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id, avionics_manufacturer_identity_id,
  membership_basis, normalized_name_key, verification_method,
  evidence_source_url,
  evidence_source_title, evidence_text, evidence_confidence
)
SELECT manufacturer.id, identity.id,
       CASE WHEN manufacturer.normalized_name = 'acme aero'
         THEN 'authoritative_primary' ELSE 'deterministic_exact' END,
       'acmeaero', 'automated',
       CASE WHEN manufacturer.normalized_name = 'acme aero'
         THEN 'https://manufacturer.example/about'
         ELSE 'urn:aircost:deterministic:avionics-manufacturer-normalization:v1' END,
       CASE WHEN manufacturer.normalized_name = 'acme aero'
         THEN 'Acme manufacturer profile'
         ELSE 'Aircost exact manufacturer normalization v1' END,
       CASE WHEN manufacturer.normalized_name = 'acme aero'
         THEN 'The manufacturer identifies Acme Aero as its product brand.'
         ELSE 'The stored manufacturer spelling has the same exact deterministic normalization key as this evidence-backed identity.' END,
       'very_high'
FROM avionics_manufacturers manufacturer, avionics_manufacturer_identities identity
WHERE manufacturer.normalized_name IN ('acme aero', 'acme-aero')
  AND identity.normalized_identity_key = 'acmeaero';
INSERT INTO avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id, avionics_manufacturer_identity_id,
  membership_basis, normalized_name_key, verification_method,
  evidence_source_url,
  evidence_source_title, evidence_text, evidence_confidence
)
SELECT manufacturer.id, identity.id, 'authoritative_primary',
       identity.normalized_identity_key, 'automated',
       identity.identity_source_url, identity.identity_source_title,
       identity.identity_evidence_text, 'very_high'
FROM avionics_manufacturers manufacturer, avionics_manufacturer_identities identity
WHERE (manufacturer.normalized_name = 'garmin'
    AND identity.normalized_identity_key = 'garmin')
   OR (manufacturer.normalized_name = 'aircraft radio'
    AND identity.normalized_identity_key = 'aircraftradio');

INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text, identity_evidence_kind,
  identity_confidence, catalog_reviewed_at
)
SELECT id, 'NavOne 100', 'nav one 100',
  'manufacturer_model_number', 'NO-100', 'no-100',
  'https://manufacturer.example/navone-100', 'NavOne 100 data sheet',
  'Manufacturer identifies model and model number.',
  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id FROM avionics_models model, avionics_types type
WHERE model.name = 'NavOne 100' AND type.normalized_name = 'navigator';
UPDATE avionics_models
SET catalog_status = 'approved', verification_method = 'automated'
WHERE name = 'NavOne 100';

-- Same canonical product through a punctuation-only manufacturer alias.
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text, identity_evidence_kind,
  identity_confidence, catalog_reviewed_at
)
SELECT id, 'Nav-One 100', 'nav-one-100',
  'manufacturer_model_number', 'NO-100-B', 'no-100-b',
  'https://manufacturer.example/navone-100-b', 'NavOne alias data sheet',
  'Manufacturer identifies the alias candidate.',
  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers WHERE normalized_name = 'acme-aero';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id FROM avionics_models model, avionics_types type
WHERE model.name = 'Nav-One 100' AND type.normalized_name = 'navigator';

-- Different product name but the same canonical manufacturer identifier.
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text, identity_evidence_kind,
  identity_confidence, catalog_reviewed_at
)
SELECT id, 'NavTwo', 'nav two',
  'manufacturer_model_number', 'NO 100', 'no 100',
  'https://manufacturer.example/navtwo', 'NavTwo data sheet',
  'Manufacturer identifier intentionally collides for this test.',
  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers WHERE normalized_name = 'acme-aero';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id FROM avionics_models model, avionics_types type
WHERE model.name = 'NavTwo' AND type.normalized_name = 'navigator';

-- The same value in a different identifier namespace is not the same stable
-- identifier. Product-name uniqueness remains independent.
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text, identity_evidence_kind,
  identity_confidence, catalog_reviewed_at
)
SELECT id, 'Sku Product', 'sku product',
  'sku', 'NO 100', 'no 100',
  'https://manufacturer.example/sku-product', 'SKU product data sheet',
  'Manufacturer identifies a distinct SKU namespace.',
  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id FROM avionics_models model, avionics_types type
WHERE model.name = 'Sku Product' AND type.normalized_name = 'navigator';
UPDATE avionics_models
SET catalog_status = 'approved', verification_method = 'automated'
WHERE name = 'Sku Product';

-- A third approved product supports complete listing action-graph tests.
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text, identity_evidence_kind,
  identity_confidence, catalog_reviewed_at
)
SELECT id, 'Aux Product', 'aux product',
  'manufacturer_model_number', 'AUX-200', 'aux-200',
  'https://manufacturer.example/aux-product', 'Aux product data sheet',
  'Manufacturer identifies a distinct auxiliary product.',
  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id FROM avionics_models model, avionics_types type
WHERE model.name = 'Aux Product' AND type.normalized_name = 'navigator';
UPDATE avionics_models
SET catalog_status = 'approved', verification_method = 'automated'
WHERE name = 'Aux Product';

-- Raw and persisted product identity keys must agree before approval.
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text, identity_evidence_kind,
  identity_confidence, catalog_reviewed_at
)
SELECT id, 'GTX 345', 'not-the-normalization',
  'manufacturer_model_number', '011-03520-00', '011-03520-00',
  'https://manufacturer.example/gtx-345', 'GTX 345 data sheet',
  'Manufacturer identifies model and model number.',
  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id FROM avionics_models model, avionics_types type
WHERE model.name = 'GTX 345' AND type.normalized_name = 'navigator';

INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text, identity_evidence_kind,
  identity_confidence, catalog_reviewed_at
)
SELECT id, 'GTX 345R', 'gtx 345r',
  'manufacturer_model_number', '011-03521-00', 'forged-identifier',
  'https://manufacturer.example/gtx-345r', 'GTX 345R data sheet',
  'Manufacturer identifies model and model number.',
  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id FROM avionics_models model, avionics_types type
WHERE model.name = 'GTX 345R' AND type.normalized_name = 'navigator';

-- Identifier values are unique only inside their manufacturer-assigned
-- namespace. The same maker may reuse one value as a model number and a SKU.
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Namespace Model', 'namespace model',
  'manufacturer_model_number', 'SHARED-NS', 'shared-ns'
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Namespace SKU', 'namespace sku',
  'sku', 'SHARED-NS', 'shared-ns'
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';

-- Two exact unreviewed legacy candidates and one unrelated candidate.
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Legacy Nav A', 'legacy nav',
  'manufacturer_model_number', 'LEG-A', 'leg-a'
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Legacy Nav B', 'legacy-nav',
  'manufacturer_model_number', 'LEG-A', 'leg-a'
FROM avionics_manufacturers WHERE normalized_name = 'acme-aero';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Legacy Nav Name Only', 'legacy/nav',
  'manufacturer_model_number', 'LEG-C', 'leg-c'
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Other Legacy', 'other legacy',
  'manufacturer_model_number', 'OTHER-1', 'other-1'
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Kind Part Candidate', 'kind part candidate',
  'manufacturer_part_number', 'SHARED-KIND', 'shared-kind'
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Kind Sku Candidate', 'kind sku candidate',
  'sku', 'SHARED KIND', 'shared kind'
FROM avionics_manufacturers WHERE normalized_name = 'acme-aero';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Corrupt Identifier A', 'corrupt identifier a',
  'manufacturer_model_number', 'ACTUAL-A', 'forged-key'
FROM avionics_manufacturers WHERE normalized_name = 'acme aero';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Corrupt Identifier B', 'corrupt identifier b',
  'manufacturer_model_number', 'ACTUAL-B', 'forged-key'
FROM avionics_manufacturers WHERE normalized_name = 'acme-aero';

INSERT INTO users (email, display_name, auth_subject)
VALUES ('schema@example.test', 'Schema Test', 'schema-test');
INSERT INTO aircraft_manufacturers (name, normalized_name)
VALUES ('Test Aircraft', 'test aircraft');
INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name)
SELECT id, 'Model', 'model' FROM aircraft_manufacturers
WHERE normalized_name = 'test aircraft';
INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name)
SELECT id, 'Variant', 'variant' FROM aircraft_models WHERE normalized_name = 'model';
INSERT INTO aircraft_sale_listings (
  aircraft_model_variant_id, created_by_user_id, source_url, model_year,
  asking_price_usd, registration_number, airframe_hours
)
SELECT placeholder.aircraft_model_variant_id, user.id,
  'https://listing.example/legacy', 2020,
  150000, 'N12345', 500
FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,
     users user
WHERE placeholder.singleton_id = 1 AND user.auth_subject = 'schema-test';
INSERT INTO aircraft_sale_listings (
  aircraft_model_variant_id, created_by_user_id, source_url, model_year,
  asking_price_usd, registration_number, airframe_hours
)
SELECT placeholder.aircraft_model_variant_id, user.id,
  'https://listing.example/actions', 2020,
  150000, 'N23456', 500
FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,
     users user
WHERE placeholder.singleton_id = 1 AND user.auth_subject = 'schema-test';
SQL

canonical_manufacturers="$(sqlite3 "$test_database" \
  "SELECT count(DISTINCT canonical_manufacturer_key) FROM avionics_manufacturer_canonical_keys WHERE avionics_manufacturer_id IN (SELECT id FROM avionics_manufacturers WHERE normalized_name IN ('acme aero','acme-aero'))")"
test "$canonical_manufacturers" = "1"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM avionics_models WHERE normalized_manufacturer_identifier='shared-ns'")" = "2"
test "$(sqlite3 "$test_database" \
  "SELECT deterministic_name_key || ':' || stored_name_key || ':' || uses_supported_ascii FROM avionics_manufacturer_normalization_contract WHERE avionics_manufacturer_id=(SELECT id FROM avionics_manufacturers WHERE normalized_name='aircraft radio')")" = "aircraftradio:aircraftradio:1"
expect_failure "$test_database" \
  "INSERT INTO avionics_manufacturer_identity_memberships (avionics_manufacturer_id,avionics_manufacturer_identity_id,membership_basis,normalized_name_key,verification_method,evidence_source_url,evidence_source_title,evidence_text,evidence_confidence) SELECT manufacturer.id,identity.id,'authoritative_primary','garminforged','automated',identity.identity_source_url,identity.identity_source_title,identity.identity_evidence_text,'very_high' FROM avionics_manufacturers manufacturer,avionics_manufacturer_identities identity WHERE manufacturer.normalized_name='garmin-forged' AND identity.normalized_identity_key='garminforged'" \
  "manufacturer membership cannot trust a forged stored normalization"
expect_failure "$test_database" \
  "UPDATE avionics_models SET catalog_status='approved',verification_method='automated' WHERE name='GTX 345'" \
  "approved product names require deterministic raw-to-stored normalization"
expect_failure "$test_database" \
  "UPDATE avionics_models SET catalog_status='approved',verification_method='automated' WHERE name='GTX 345R'" \
  "approved manufacturer identifiers require deterministic raw-to-stored normalization"

# SQLite can defer an otherwise immediate child FK and preinsert a capability
# row for a not-yet-existing product. Direct approval must still be rejected;
# staged unreviewed -> approved UPDATE is the only catalog admission path.
expect_failure "$test_database" \
  "PRAGMA defer_foreign_keys=ON; BEGIN; INSERT INTO avionics_model_types (avionics_model_id,avionics_type_id) SELECT 900001,id FROM avionics_types WHERE normalized_name='navigator'; INSERT INTO avionics_models (id,avionics_manufacturer_id,name,normalized_name,catalog_status,verification_method,manufacturer_identifier_kind,manufacturer_identifier,normalized_manufacturer_identifier,identity_source_url,identity_source_title,identity_evidence_text,identity_evidence_kind,identity_confidence,catalog_reviewed_at) SELECT 900001,id,'GTX 345','direct-forged-key','approved','automated','manufacturer_model_number','DIRECT-100','direct-forged-identifier','https://manufacturer.example/direct-forged','Direct forged fixture','Manufacturer identifies the product and identifier.','authoritative_reference','very_high',CURRENT_TIMESTAMP FROM avionics_manufacturers WHERE normalized_name='acme aero'; COMMIT;" \
  "deferred foreign keys cannot bypass staged avionics approval"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM avionics_models WHERE id=900001")" = "0"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM avionics_model_types WHERE avionics_model_id=900001")" = "0"
expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,source,source_confidence) SELECT listing.id,900001,'listing','high' FROM aircraft_sale_listings listing WHERE listing.source_url='https://listing.example/legacy'" \
  "a rejected direct-approved product cannot subsequently enter a listing"
expect_failure "$test_database" \
  "INSERT INTO avionics_manufacturer_alias_candidates (avionics_manufacturer_id,candidate_manufacturer_identity_id,candidate_basis,reason,confidence,review_status,decision_reason,decision_evidence_source_url,decision_evidence_source_title,decision_evidence_text,reviewed_by_user_id,reviewed_at) SELECT manufacturer.id,identity.id,'grounded_alias','Attempt to bypass staged review.','very_high','approved','Direct approval','https://manufacturer.example/about','Acme manufacturer profile','Acme identifies this brand.',user.id,CURRENT_TIMESTAMP FROM avionics_manufacturers manufacturer,avionics_manufacturer_identities identity,users user WHERE manufacturer.normalized_name='acme-aero' AND identity.normalized_identity_key='acmeaero' AND user.auth_subject='schema-test'" \
  "manufacturer alias candidates must enter through pending review"
expect_failure "$test_database" \
  "UPDATE avionics_manufacturers SET name='Rewritten Acme' WHERE normalized_name='acme aero'" \
  "evidence-backed manufacturer display names are immutable"
expect_failure "$test_database" \
  "INSERT INTO avionics_models (avionics_manufacturer_id,name,normalized_name,manufacturer_identifier_kind,manufacturer_identifier,normalized_manufacturer_identifier) SELECT id,'Namespace Model Duplicate','namespace model duplicate','manufacturer_model_number','SHARED-NS','shared-ns' FROM avionics_manufacturers WHERE normalized_name='acme aero'" \
  "one manufacturer cannot reuse an identifier within the same namespace"
expect_failure "$test_database" \
  "DELETE FROM avionics_manufacturer_canonical_keys WHERE avionics_manufacturer_id=(SELECT id FROM avionics_manufacturers WHERE normalized_name='mutable key co')" \
  "canonical manufacturer keys cannot be deleted and reinserted under a new group"
test "$(sqlite3 "$test_database" \
  "SELECT canonical_manufacturer_key FROM avionics_manufacturer_canonical_keys WHERE avionics_manufacturer_id=(SELECT id FROM avionics_manufacturers WHERE normalized_name='mutable key co')")" = "mutablekeyco"
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "DELETE FROM avionics_manufacturers WHERE normalized_name='mutable key co'"
test "$(sqlite3 "$test_database" \
  "SELECT count(*) FROM avionics_manufacturer_canonical_keys WHERE canonical_manufacturer_key='mutablekeyco'")" = "0"

expect_failure "$test_database" \
  "UPDATE avionics_models SET catalog_status='approved',verification_method='automated' WHERE name='Nav-One 100'" \
  "manufacturer aliases must not approve the same canonical product twice"
expect_failure "$test_database" \
  "UPDATE avionics_models SET catalog_status='approved',verification_method='automated' WHERE name='NavTwo'" \
  "manufacturer aliases must not approve the same canonical identifier twice"
expect_failure "$test_database" \
  "UPDATE avionics_models SET identity_evidence_text='changed' WHERE name='NavOne 100'" \
  "approved identity evidence is immutable"
expect_failure "$test_database" \
  "UPDATE avionics_models SET catalog_status='unreviewed' WHERE name='NavOne 100'" \
  "approved catalog products cannot be demoted"
expect_failure "$test_database" \
  "DELETE FROM avionics_models WHERE name='NavOne 100'" \
  "approved catalog products cannot be deleted without exact consolidation authorization"
sqlite3 -bail "$test_database" \
  "UPDATE avionics_models SET introduced_year=2019 WHERE name='NavOne 100'"
expect_failure "$test_database" \
  "UPDATE aircraft_sale_listings SET is_verified=1 WHERE source_url='https://listing.example/legacy'" \
  "verified listings must already be in the ready ingestion state"
expect_failure "$test_database" \
  "DELETE FROM avionics_approved_product_identities WHERE avionics_model_id=(SELECT id FROM avionics_models WHERE name='NavOne 100')" \
  "an approved model must retain its identity row"

expect_failure "$test_database" \
  "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id,survivor_model_id) SELECT duplicate.id,survivor.id FROM avionics_models duplicate,avionics_models survivor WHERE duplicate.name='Legacy Nav A' AND survivor.name='Other Legacy'" \
  "guard pairs require an exact identity match"
expect_failure "$test_database" \
  "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id,survivor_model_id) SELECT duplicate.id,survivor.id FROM avionics_models duplicate,avionics_models survivor WHERE duplicate.name='Legacy Nav A' AND survivor.name='Legacy Nav Name Only'" \
  "mechanically equivalent product names cannot authorize a destructive merge"
expect_failure "$test_database" \
  "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id,survivor_model_id) SELECT duplicate.id,survivor.id FROM avionics_models duplicate,avionics_models survivor WHERE duplicate.name='Kind Part Candidate' AND survivor.name='Kind Sku Candidate'" \
  "guard identifier matches require the same non-null identifier kind"
expect_failure "$test_database" \
  "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id,survivor_model_id) SELECT duplicate.id,survivor.id FROM avionics_models duplicate,avionics_models survivor WHERE duplicate.name='Corrupt Identifier A' AND survivor.name='Corrupt Identifier B'" \
  "persisted normalized identifiers cannot authorize a merge when they disagree with the raw manufacturer identifiers"
sqlite3 -bail "$test_database" \
  "INSERT INTO avionics_catalog_consolidation_guard (duplicate_model_id,survivor_model_id) SELECT duplicate.id,survivor.id FROM avionics_models duplicate,avionics_models survivor WHERE duplicate.name='Legacy Nav A' AND survivor.name='Legacy Nav B'"
expect_failure "$test_database" \
  "UPDATE avionics_catalog_consolidation_guard SET survivor_model_id=survivor_model_id" \
  "guard pairs are immutable"
expect_failure "$test_database" \
  "UPDATE avionics_models SET normalized_name='changed' WHERE name='Legacy Nav A'" \
  "guarded endpoint identities are immutable"
test "$(sqlite3 "$test_database" "SELECT count(*) FROM avionics_catalog_authorized_consolidations")" = "1"
sqlite3 -bail "$test_database" "DELETE FROM avionics_catalog_consolidation_guard"

# Isolate the avionics ready-state contract from the independent FAA aircraft
# identity contract in this schema test.
sqlite3 -bail "$test_database" <<'SQL'
PRAGMA foreign_keys = ON;
DROP TRIGGER IF EXISTS listing_ready_requires_canonical_aircraft_update;
DROP TRIGGER IF EXISTS listing_ready_requires_aircraft_projection;
DROP TRIGGER IF EXISTS listing_ready_rejects_pending_aircraft_placeholder;
INSERT INTO aircraft_sale_listings (
  aircraft_model_variant_id, created_by_user_id, source_url, model_year,
  asking_price_usd, registration_number, airframe_hours
)
SELECT placeholder.aircraft_model_variant_id, user.id,
  'https://listing.example/approved', 2021,
  175000, 'N54321', 400
FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,
     users user
WHERE placeholder.singleton_id = 1 AND user.auth_subject = 'schema-test';
INSERT INTO aircraft_sale_listing_avionics (
  aircraft_sale_listing_id, avionics_model_id, source, source_confidence
)
SELECT listing.id, model.id, 'listing', 'medium'
FROM aircraft_sale_listings listing, avionics_models model
WHERE listing.source_url='https://listing.example/approved'
  AND model.name='NavOne 100';
SQL
expect_failure "$test_database" \
  "UPDATE aircraft_sale_listings SET ingestion_completed_at=CURRENT_TIMESTAMP,ingestion_state='ready' WHERE source_url='https://listing.example/approved'" \
  "ready listings require high-confidence listing or reviewer evidence"
sqlite3 -bail "$test_database" \
  "UPDATE aircraft_sale_listing_avionics SET source_confidence='high' WHERE aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/approved')" \
  "UPDATE aircraft_sale_listings SET ingestion_completed_at=CURRENT_TIMESTAMP,ingestion_state='ready' WHERE source_url='https://listing.example/approved'"
sqlite3 -bail "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,source,source_confidence) SELECT listing.id,model.id,'listing','high' FROM aircraft_sale_listings listing,avionics_models model WHERE listing.source_url='https://listing.example/legacy' AND model.name='NavOne 100'"
expect_failure "$test_database" \
  "UPDATE aircraft_sale_listing_avionics SET aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/approved') WHERE aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/legacy')" \
  "an association cannot be moved from a mutable listing into a ready listing"
expect_failure "$test_database" \
  "UPDATE aircraft_sale_listing_avionics SET quantity=2 WHERE aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/approved')" \
  "ready listing associations are immutable"
expect_failure "$test_database" \
  "DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/approved')" \
  "ready listing associations cannot be deleted directly"
sqlite3 -bail "$test_database" \
  "PRAGMA foreign_keys=ON" \
  "DELETE FROM aircraft_sale_listings WHERE source_url='https://listing.example/approved'"

expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,replaces_avionics_model_id,source_confidence) SELECT listing.id,model.id,'replaces',model.id,'high' FROM aircraft_sale_listings listing,avionics_models model WHERE listing.source_url='https://listing.example/legacy' AND model.name='NavOne 100'" \
  "an association cannot install and replace the same canonical product"

# Complete per-listing action graphs are enforced in either insertion order.
sqlite3 -bail "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,replaces_avionics_model_id,source_confidence) SELECT listing.id,model.id,'removes',model.id,'high' FROM aircraft_sale_listings listing,avionics_models model WHERE listing.source_url='https://listing.example/actions' AND model.name='NavOne 100'" \
  "DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/actions')"
expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,replaces_avionics_model_id,source_confidence) SELECT listing.id,model.id,'replaces',model.id,'high' FROM aircraft_sale_listings listing,avionics_models model WHERE listing.source_url='https://listing.example/actions' AND model.name='NavOne 100'" \
  "a replacement cannot target its own subject"
sqlite3 -bail "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,source_confidence) SELECT listing.id,model.id,'installed','high' FROM aircraft_sale_listings listing,avionics_models model WHERE listing.source_url='https://listing.example/actions' AND model.name='NavOne 100'"
expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,replaces_avionics_model_id,source_confidence) SELECT listing.id,subject.id,'replaces',target.id,'high' FROM aircraft_sale_listings listing,avionics_models subject,avionics_models target WHERE listing.source_url='https://listing.example/actions' AND subject.name='Sku Product' AND target.name='NavOne 100'" \
  "an installed subject cannot also be a displacement target"
sqlite3 -bail "$test_database" \
  "DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/actions')" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,replaces_avionics_model_id,source_confidence) SELECT listing.id,subject.id,'replaces',target.id,'high' FROM aircraft_sale_listings listing,avionics_models subject,avionics_models target WHERE listing.source_url='https://listing.example/actions' AND subject.name='Sku Product' AND target.name='NavOne 100'"
expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,source_confidence) SELECT listing.id,model.id,'installed','high' FROM aircraft_sale_listings listing,avionics_models model WHERE listing.source_url='https://listing.example/actions' AND model.name='NavOne 100'" \
  "a displacement target cannot later be installed"
expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,replaces_avionics_model_id,source_confidence) SELECT listing.id,subject.id,'replaces',target.id,'high' FROM aircraft_sale_listings listing,avionics_models subject,avionics_models target WHERE listing.source_url='https://listing.example/actions' AND subject.name='Aux Product' AND target.name='NavOne 100'" \
  "a listing cannot displace one target more than once"
expect_failure "$test_database" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,replaces_avionics_model_id,source_confidence) SELECT listing.id,subject.id,'replaces',target.id,'high' FROM aircraft_sale_listings listing,avionics_models subject,avionics_models target WHERE listing.source_url='https://listing.example/actions' AND subject.name='NavOne 100' AND target.name='Sku Product'" \
  "replacement cycles and chains cannot be stored"
sqlite3 -bail "$test_database" \
  "DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/actions')" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,replaces_avionics_model_id,source_confidence) SELECT listing.id,subject.id,'replaces',target.id,'high' FROM aircraft_sale_listings listing,avionics_models subject,avionics_models target WHERE listing.source_url='https://listing.example/actions' AND subject.name='Sku Product' AND target.name='NavOne 100'" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,configuration_action,source_confidence) SELECT listing.id,model.id,'installed','high' FROM aircraft_sale_listings listing,avionics_models model WHERE listing.source_url='https://listing.example/actions' AND model.name='Aux Product'" \
  "DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id=(SELECT id FROM aircraft_sale_listings WHERE source_url='https://listing.example/actions')"

# Upgrade rehearsal: preserve an invalid legacy ready row, then verify the
# migration quarantines it with a precise review reason instead of deleting it.
sqlite3 -bail "$upgrade_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/tests/schema/reference_catalog_pre_cutover.sqlite.sql" \
  "DROP TRIGGER IF EXISTS aircraft_sale_listings_ready_semantic_avionics" \
  "DROP TRIGGER IF EXISTS aircraft_sale_listings_ready_semantic_avionics_insert" \
  "DROP TRIGGER IF EXISTS listing_ready_requires_canonical_aircraft_update" \
  "DROP TRIGGER IF EXISTS listing_ready_requires_canonical_aircraft_insert" \
  "DROP TRIGGER IF EXISTS listing_ready_requires_aircraft_projection" \
  "DROP TRIGGER IF EXISTS listing_ready_rejects_pending_aircraft_placeholder" \
  "DROP TRIGGER IF EXISTS listing_verified_requires_ready_insert" \
  "DROP TRIGGER IF EXISTS listing_verified_requires_ready_update" \
  "DROP TRIGGER IF EXISTS aircraft_sale_listing_avionics_approved_insert" \
  "INSERT INTO users (email,display_name,auth_subject) VALUES ('upgrade@example.test','Upgrade','upgrade')" \
  "INSERT INTO aircraft_manufacturers (name,normalized_name) VALUES ('Upgrade Aircraft','upgrade aircraft')" \
  "INSERT INTO aircraft_models (aircraft_manufacturer_id,name,normalized_name) SELECT id,'Model','model' FROM aircraft_manufacturers" \
  "INSERT INTO aircraft_model_variants (aircraft_model_id,name,normalized_name) SELECT id,'Variant','variant' FROM aircraft_models" \
  "INSERT INTO avionics_manufacturers (name,normalized_name) VALUES ('Legacy','legacy')" \
  "INSERT INTO avionics_models (avionics_manufacturer_id,name,normalized_name) SELECT id,'Garbage','garbage' FROM avionics_manufacturers" \
  "INSERT INTO avionics_manufacturers (name,normalized_name) VALUES ('Seeded Approved Maker','seeded approved maker')" \
  "INSERT INTO avionics_types (name,normalized_name) VALUES ('Upgrade Navigator','upgrade navigator')" \
  "INSERT INTO avionics_models (avionics_manufacturer_id,name,normalized_name,manufacturer_identifier_kind,manufacturer_identifier,normalized_manufacturer_identifier,identity_source_url,identity_source_title,identity_evidence_text,identity_evidence_kind,identity_confidence,catalog_reviewed_at) SELECT id,'Seeded Navigator','seeded navigator','manufacturer_model_number','SN-100','sn-100','https://manufacturer.example/seeded','Seeded Navigator data sheet','The manufacturer identifies the maker, model, and model number.','authoritative_reference','very_high',CURRENT_TIMESTAMP FROM avionics_manufacturers WHERE normalized_name='seeded approved maker'" \
  "INSERT INTO avionics_model_types (avionics_model_id,avionics_type_id) SELECT model.id,type.id FROM avionics_models model,avionics_types type WHERE model.normalized_name='seeded navigator' AND type.normalized_name='upgrade navigator'" \
  "DROP TRIGGER IF EXISTS avionics_models_canonical_identity_validate_update" \
  "DROP TRIGGER IF EXISTS avionics_models_canonical_identity_sync_update" \
  "UPDATE avionics_models SET catalog_status='approved',verification_method='automated' WHERE normalized_name='seeded navigator'" \
  "INSERT INTO aircraft_sale_listings (aircraft_model_variant_id,created_by_user_id,source_url,model_year,asking_price_usd,registration_number,airframe_hours) SELECT placeholder.aircraft_model_variant_id,user.id,'https://listing.example/invalid',2020,100000,'N11111',100 FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,users user WHERE placeholder.singleton_id=1" \
  "INSERT INTO aircraft_sale_listings (aircraft_model_variant_id,created_by_user_id,source_url,model_year,asking_price_usd,registration_number,airframe_hours) SELECT placeholder.aircraft_model_variant_id,user.id,'https://listing.example/invalid-verified',2020,100000,'N22222',100 FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,users user WHERE placeholder.singleton_id=1" \
  "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id,avionics_model_id,source,source_confidence) SELECT listing.id,model.id,'listing','medium' FROM aircraft_sale_listings listing,avionics_models model" \
  "UPDATE aircraft_sale_listings SET is_verified=1,ingestion_state='ready',ingestion_completed_at=CURRENT_TIMESTAMP WHERE source_url='https://listing.example/invalid'" \
  "UPDATE aircraft_sale_listings SET is_verified=1 WHERE source_url='https://listing.example/invalid-verified'" \
  ".read $repository_root/migrations/20260725_identity_deduplication_postconditions.sqlite.sql"

test "$(sqlite3 "$upgrade_database" "SELECT ingestion_state || ':' || is_verified FROM aircraft_sale_listings WHERE source_url='https://listing.example/invalid'")" = "quarantined:0"
upgrade_error="$(sqlite3 "$upgrade_database" "SELECT ingestion_error FROM aircraft_sale_listings WHERE source_url='https://listing.example/invalid'")"
[[ "$upgrade_error" == *"unapproved catalog product"* ]]
test "$(sqlite3 "$upgrade_database" "SELECT ingestion_state || ':' || is_verified FROM aircraft_sale_listings WHERE source_url='https://listing.example/invalid-verified'")" = "quarantined:0"
verified_upgrade_error="$(sqlite3 "$upgrade_database" "SELECT ingestion_error FROM aircraft_sale_listings WHERE source_url='https://listing.example/invalid-verified'")"
[[ "$verified_upgrade_error" == *"verified listing was not in the ready ingestion state"* ]]
test "$(sqlite3 "$upgrade_database" "SELECT count(*) FROM avionics_manufacturer_identities WHERE normalized_identity_key='seededapprovedmaker'")" = "1"
test "$(sqlite3 "$upgrade_database" "SELECT count(*) FROM avionics_manufacturer_identity_memberships membership JOIN avionics_manufacturers manufacturer ON manufacturer.id=membership.avionics_manufacturer_id WHERE manufacturer.normalized_name='seeded approved maker'")" = "1"
test "$(sqlite3 "$upgrade_database" "SELECT count(*) FROM avionics_approved_product_identities identity JOIN avionics_models model ON model.id=identity.avionics_model_id WHERE model.normalized_name='seeded navigator'")" = "1"

# Migration completion is fail-closed for corrupt identities that predate the
# v6 triggers; it must not bless them by merely writing a new contract marker.
sqlite3 -bail "$invalid_membership_database" <<SQL
.read $repository_root/schema/sqlite.sql
.read $repository_root/tests/schema/reference_catalog_pre_cutover.sqlite.sql
DROP TRIGGER avionics_manufacturer_membership_validate_insert;
INSERT INTO avionics_manufacturers (name,normalized_name)
VALUES ('Garmin','forged-garmin-key');
INSERT INTO avionics_manufacturer_identities (
  canonical_name,normalized_identity_key,verification_method,
  identity_evidence_kind,
  identity_source_url,identity_source_title,identity_evidence_text,
  identity_confidence
) VALUES (
  'Forged Garmin','forgedgarminkey','automated','authoritative_reference',
  'https://manufacturer.example/forged','Forged fixture',
  'A deliberately corrupt pre-v6 identity fixture.','very_high'
);
INSERT INTO avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id,avionics_manufacturer_identity_id,
  membership_basis,normalized_name_key,verification_method,
  evidence_source_url,
  evidence_source_title,evidence_text,evidence_confidence
)
SELECT manufacturer.id,identity.id,'authoritative_primary','forgedgarminkey',
       'automated',
       identity.identity_source_url,identity.identity_source_title,
       identity.identity_evidence_text,'very_high'
FROM avionics_manufacturers manufacturer,avionics_manufacturer_identities identity
WHERE manufacturer.normalized_name='forged-garmin-key'
  AND identity.normalized_identity_key='forgedgarminkey';
SQL
expect_migration_failure "$invalid_membership_database" \
  "v6 rejects pre-existing manufacturer memberships with forged normalization keys"

sqlite3 -bail "$invalid_product_database" <<SQL
.read $repository_root/schema/sqlite.sql
.read $repository_root/tests/schema/reference_catalog_pre_cutover.sqlite.sql
INSERT INTO avionics_manufacturers (name,normalized_name) VALUES ('Garmin','garmin');
INSERT INTO avionics_manufacturer_identities (
  canonical_name,normalized_identity_key,verification_method,
  identity_evidence_kind,
  identity_source_url,identity_source_title,identity_evidence_text,
  identity_confidence
) VALUES (
  'Garmin','garmin','automated','authoritative_reference',
  'https://manufacturer.example/garmin','Garmin profile',
  'The manufacturer identifies its product brand.','very_high'
);
INSERT INTO avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id,avionics_manufacturer_identity_id,
  membership_basis,normalized_name_key,verification_method,
  evidence_source_url,
  evidence_source_title,evidence_text,evidence_confidence
)
SELECT manufacturer.id,identity.id,'authoritative_primary','garmin','automated',
       identity.identity_source_url,identity.identity_source_title,
       identity.identity_evidence_text,'very_high'
FROM avionics_manufacturers manufacturer,avionics_manufacturer_identities identity;
INSERT INTO avionics_types (name,normalized_name) VALUES ('Navigator','navigator');
INSERT INTO avionics_models (
  avionics_manufacturer_id,name,normalized_name,
  manufacturer_identifier_kind,manufacturer_identifier,
  normalized_manufacturer_identifier,identity_source_url,
  identity_source_title,identity_evidence_text,identity_evidence_kind,
  identity_confidence,catalog_reviewed_at
)
SELECT id,'GTX 345','forged-product-key',
       'manufacturer_model_number','011-03520-00','forged-identifier-key',
       'https://manufacturer.example/gtx-345','GTX 345 data sheet',
       'Manufacturer identifies model and identifier.',
       'authoritative_reference','very_high',CURRENT_TIMESTAMP
FROM avionics_manufacturers;
INSERT INTO avionics_model_types (avionics_model_id,avionics_type_id)
SELECT model.id,type.id FROM avionics_models model,avionics_types type;
DROP TRIGGER avionics_models_canonical_identity_validate_update;
DROP TRIGGER avionics_models_canonical_identity_sync_update;
DROP TRIGGER avionics_models_approved_concrete_model_update;
UPDATE avionics_models
SET catalog_status='approved',verification_method='automated';
SQL
expect_migration_failure "$invalid_product_database" \
  "v6 rejects pre-existing approved products with forged normalization keys"

for database in "$test_database" "$upgrade_database"; do
  test -z "$(sqlite3 "$database" "PRAGMA foreign_key_check")"
  test "$(sqlite3 "$database" "PRAGMA integrity_check")" = "ok"
done

# PostgreSQL is not required for the local test runner, so keep a focused
# parity contract that catches missing tables, views, guard hardening, and
# ready-state enforcement in either migration.
for migration in \
  "$repository_root/migrations/20260725_identity_deduplication_postconditions.sqlite.sql" \
  "$repository_root/migrations/20260725_identity_deduplication_postconditions.postgres.sql"; do
  grep -q "avionics_manufacturer_canonical_keys" "$migration"
  grep -q "avionics_approved_product_identities" "$migration"
  grep -q "avionics_catalog_consolidation_guard" "$migration"
  grep -q "avionics_catalog_authorized_consolidations" "$migration"
  grep -q "avionics_manufacturer_alias_candidate_pending_insert" "$migration"
  grep -q "avionics_manufacturer_identity_name_immutable" "$migration"
  grep -q "avionics_models_canonical_identity_sync_update" "$migration"
  grep -q "avionics_models_approved_delete_guard" "$migration"
  grep -q "approved avionics product cannot be demoted" "$migration"
  grep -q "avionics approval must be staged from an unreviewed product" "$migration"
  grep -q "avionics_approved_registry_completeness_guard" "$migration"
  grep -q "idx_avionics_models_manufacturer_identifier" "$migration"
  grep -q "manufacturer_identifier_kind" "$migration"
  grep -q "avionics_manufacturer_canonical_key_delete" "$migration"
  grep -q "listing_verified_requires_ready_update" "$migration"
  grep -q "guarded avionics consolidation identities are immutable" "$migration"
  grep -q "avionics_semantic_duplicate_listing_links" "$migration"
  grep -q "avionics_approved_product_graph_identities" "$migration"
  grep -q "avionics_semantic_invalid_listing_action_graphs" "$migration"
  grep -q "aircraft_sale_listing_avionics_action_graph_insert" "$migration"
  grep -q "idx_aircraft_sale_listing_avionics_unique_displacement" "$migration"
  grep -q "ready listing requires unique approved canonical avionics" "$migration"
done

grep -q "ShareRowExclusiveLock" \
  "$repository_root/migrations/20260725_identity_deduplication_postconditions.postgres.sql"
grep -q "pg_locks" \
  "$repository_root/migrations/20260725_identity_deduplication_postconditions.postgres.sql"
grep -q "ShareRowExclusiveLock" "$repository_root/schema/postgres.sql"
grep -q "pg_locks" "$repository_root/schema/postgres.sql"
grep -q "FOR UPDATE" \
  "$repository_root/migrations/20260725_identity_deduplication_postconditions.postgres.sql"
grep -q "FOR UPDATE" "$repository_root/schema/postgres.sql"

echo "Identity deduplication postconditions passed"
