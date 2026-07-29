#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
upgrade_database="$(mktemp /tmp/aircost-avionics-reuse-v2-upgrade.XXXXXX.sqlite3)"
fresh_database="$(mktemp /tmp/aircost-avionics-reuse-v2-fresh.XXXXXX.sqlite3)"
trap 'rm -f "$upgrade_database" "$fresh_database"' EXIT

sqlite3 -bail "$upgrade_database" <<SQL
.read $repository_root/schema/sqlite.sql
PRAGMA foreign_keys = ON;

INSERT INTO users (email, display_name, auth_subject)
VALUES ('reuse-v2@example.test', 'Reuse V2', 'reuse-v2');
INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES ('Garmin', 'garmin');
INSERT INTO avionics_types (name, normalized_name)
VALUES ('Navigator', 'navigator');
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key, identity_evidence_kind,
  identity_source_url, identity_source_title, identity_evidence_text,
  identity_confidence
) VALUES (
  'Garmin', 'garmin', 'authoritative_reference',
  'https://www.garmin.com/en-US/p/reuse-v2-fixture',
  'Garmin reuse-v2 fixture',
  'Garmin identifies this manufacturer.',
  'very_high'
);
INSERT INTO avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id, avionics_manufacturer_identity_id,
  membership_basis, normalized_name_key, evidence_source_url,
  evidence_source_title, evidence_text, evidence_confidence
)
SELECT manufacturer.id, identity.id, 'authoritative_primary',
       'garmin', identity.identity_source_url,
       identity.identity_source_title,
       identity.identity_evidence_text, 'very_high'
FROM avionics_manufacturers manufacturer,
     avionics_manufacturer_identities identity
WHERE manufacturer.normalized_name = 'garmin'
  AND identity.normalized_identity_key = 'garmin';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text,
  identity_evidence_kind, identity_confidence, catalog_reviewed_at
)
SELECT id, 'TestNav 100', 'testnav 100',
       'manufacturer_model_number', 'TN-100', 'tn-100',
       'https://static.garmin.com/testnav-100.pdf',
       'Garmin TestNav 100 Manual', 'TestNav 100 TN-100',
       'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
FROM avionics_manufacturers
WHERE normalized_name = 'garmin';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, capability.id
FROM avionics_models model, avionics_types capability
WHERE model.normalized_name = 'testnav 100'
  AND capability.normalized_name = 'navigator';
UPDATE avionics_models
SET catalog_status = 'approved'
WHERE normalized_name = 'testnav 100';

INSERT INTO aircraft_sale_listings (
  aircraft_model_variant_id, created_by_user_id, source_url, model_year,
  asking_price_usd, registration_number, airframe_hours, ingestion_state
)
SELECT placeholder.aircraft_model_variant_id, reviewer.id,
       'https://listing.example/reuse-v2', 2020, 100000, 'N12345', 100,
       'pending_review'
FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder,
     users reviewer
WHERE placeholder.singleton_id = 1
  AND reviewer.auth_subject = 'reuse-v2';
INSERT INTO aircraft_sale_listing_pending_reviews (
  listing_id, extraction_sha256, catalog_revision_sha256,
  pending_aspect_count, review_payload_json, review_payload_sha256
)
SELECT id,
       '0000000000000000000000000000000000000000000000000000000000000000',
       '1111111111111111111111111111111111111111111111111111111111111111',
       1, '{}',
       '2222222222222222222222222222222222222222222222222222222222222222'
FROM aircraft_sale_listings
WHERE source_url = 'https://listing.example/reuse-v2';
INSERT INTO aircraft_sale_listing_avionics (
  aircraft_sale_listing_id, avionics_model_id, quantity, source,
  source_notes, configuration_action, source_confidence
)
SELECT listing.id, model.id, 2, 'listing', 'TestNav 100 shown twice',
       'installed', 'high'
FROM aircraft_sale_listings listing, avionics_models model
WHERE listing.source_url = 'https://listing.example/reuse-v2'
  AND model.normalized_name = 'testnav 100';

-- Reconstruct the exact v1 parent contract. Rerunning the canonical v1 and
-- association migrations below restores every predecessor trigger and index.
PRAGMA foreign_keys = OFF;
DROP TRIGGER IF EXISTS avionics_product_reuse_attestations_validate_insert;
DROP TRIGGER IF EXISTS avionics_product_reuse_attestations_immutable_update;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_insert;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_delete;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_update;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_capability_update;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_identity_update;
DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_origin_revocation;
DROP TRIGGER IF EXISTS listing_avionics_corroborations_validate_insert;
DROP TABLE avionics_product_reuse_attestations;
CREATE TABLE avionics_product_reuse_attestations (
  avionics_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_authoritative_source_origin_id INTEGER NOT NULL
    REFERENCES avionics_authoritative_source_origins(id) ON DELETE RESTRICT,
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'avionics_reuse_v1'),
  product_fingerprint TEXT NOT NULL,
  attested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(product_fingerprint) = 64),
  CHECK (product_fingerprint = lower(product_fingerprint)),
  CHECK (product_fingerprint NOT GLOB '*[^0-9a-f]*')
);
DELETE FROM schema_migration_contracts
WHERE migration_name = '20260807_avionics_product_reuse_v2';

.read $repository_root/migrations/20260803_avionics_product_reuse_attestations.sqlite.sql
.read $repository_root/migrations/20260805_listing_avionics_association_corroborations.sqlite.sql

INSERT INTO avionics_product_reuse_attestations (
  avionics_model_id, avionics_authoritative_source_origin_id,
  policy_version, product_fingerprint
)
SELECT model.id, source_origin.id, 'avionics_reuse_v1',
       'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
FROM avionics_models model
JOIN avionics_approved_product_identities product_identity
  ON product_identity.avionics_model_id = model.id
JOIN avionics_active_authoritative_source_origins source_origin
  ON source_origin.authority_kind = 'manufacturer_primary'
JOIN avionics_manufacturer_effective_identities origin_identity
  ON origin_identity.identity_id =
     source_origin.avionics_manufacturer_identity_id
 AND origin_identity.avionics_manufacturer_identity_id =
     product_identity.avionics_manufacturer_identity_id
WHERE model.normalized_name = 'testnav 100'
ORDER BY source_origin.id
LIMIT 1;
INSERT INTO aircraft_sale_listing_avionics_corroborations (
  listing_link_id, association_role, avionics_model_id, observation_sha256,
  product_fingerprint, policy_version
)
SELECT link.id, 'installed', link.avionics_model_id,
       'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
       attestation.product_fingerprint, 'listing_avionics_association_v1'
FROM aircraft_sale_listing_avionics link
JOIN avionics_product_reuse_attestations attestation
  ON attestation.avionics_model_id = link.avionics_model_id;
INSERT INTO aircraft_sale_listing_avionics_corroboration_scopes (
  listing_link_id, association_role, collision_closure_sha256, policy_version
)
SELECT listing_link_id, association_role,
       'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
       'listing_avionics_collision_closure_v1'
FROM aircraft_sale_listing_avionics_corroborations;
SQL

test "$(sqlite3 "$upgrade_database" \
  "SELECT (SELECT count(*) FROM avionics_product_reuse_attestations) || ':' || (SELECT count(*) FROM aircraft_sale_listing_avionics_corroborations) || ':' || (SELECT count(*) FROM aircraft_sale_listing_avionics_corroboration_scopes)")" = \
  "1:1:1"

preserved_counts="$(sqlite3 "$upgrade_database" \
  "SELECT (SELECT count(*) FROM avionics_models) || ':' || (SELECT count(*) FROM aircraft_sale_listings) || ':' || (SELECT count(*) FROM aircraft_sale_listing_pending_reviews) || ':' || (SELECT count(*) FROM aircraft_sale_listing_avionics)")"

sqlite3 -bail "$upgrade_database" \
  ".read $repository_root/migrations/20260807_avionics_product_reuse_v2.sqlite.sql"
test "$(sqlite3 "$upgrade_database" \
  "SELECT (SELECT count(*) FROM avionics_product_reuse_attestations) || ':' || (SELECT count(*) FROM aircraft_sale_listing_avionics_corroborations) || ':' || (SELECT count(*) FROM aircraft_sale_listing_avionics_corroboration_scopes)")" = \
  "0:0:0"

# Reapplying the migration must retain conclusions that were freshly earned
# under v2; it may never turn the migration itself into an issuance path.
sqlite3 -bail "$upgrade_database" <<'SQL'
INSERT INTO avionics_product_reuse_attestations (
  avionics_model_id, avionics_authoritative_source_origin_id,
  policy_version, product_fingerprint
)
SELECT model.id, source_origin.id, 'avionics_reuse_v2',
       'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd'
FROM avionics_models model
JOIN avionics_approved_product_identities product_identity
  ON product_identity.avionics_model_id = model.id
JOIN avionics_active_authoritative_source_origins source_origin
  ON source_origin.authority_kind = 'manufacturer_primary'
JOIN avionics_manufacturer_effective_identities origin_identity
  ON origin_identity.identity_id =
     source_origin.avionics_manufacturer_identity_id
 AND origin_identity.avionics_manufacturer_identity_id =
     product_identity.avionics_manufacturer_identity_id
WHERE model.normalized_name = 'testnav 100'
ORDER BY source_origin.id
LIMIT 1;
INSERT INTO aircraft_sale_listing_avionics_corroborations (
  listing_link_id, association_role, avionics_model_id, observation_sha256,
  product_fingerprint, policy_version
)
SELECT link.id, 'installed', link.avionics_model_id,
       'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
       attestation.product_fingerprint, 'listing_avionics_association_v1'
FROM aircraft_sale_listing_avionics link
JOIN avionics_product_reuse_attestations attestation
  ON attestation.avionics_model_id = link.avionics_model_id;
INSERT INTO aircraft_sale_listing_avionics_corroboration_scopes (
  listing_link_id, association_role, collision_closure_sha256, policy_version
)
SELECT listing_link_id, association_role,
       'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
       'listing_avionics_collision_closure_v1'
FROM aircraft_sale_listing_avionics_corroborations;
SQL
sqlite3 -bail "$upgrade_database" \
  ".read $repository_root/migrations/20260807_avionics_product_reuse_v2.sqlite.sql"
test "$(sqlite3 "$upgrade_database" \
  "SELECT (SELECT count(*) FROM avionics_product_reuse_attestations WHERE policy_version='avionics_reuse_v2') || ':' || (SELECT count(*) FROM aircraft_sale_listing_avionics_corroborations) || ':' || (SELECT count(*) FROM aircraft_sale_listing_avionics_corroboration_scopes)")" = \
  "1:1:1"
test "$(sqlite3 "$upgrade_database" \
  "SELECT count(*) FROM avionics_product_reuse_attestations WHERE policy_version='avionics_reuse_v1'")" = "0"
test "$(sqlite3 "$upgrade_database" \
  "SELECT (SELECT count(*) FROM avionics_models) || ':' || (SELECT count(*) FROM aircraft_sale_listings) || ':' || (SELECT count(*) FROM aircraft_sale_listing_pending_reviews) || ':' || (SELECT count(*) FROM aircraft_sale_listing_avionics)")" = \
  "$preserved_counts"
test "$(sqlite3 "$upgrade_database" \
  "SELECT count(*) FROM schema_migration_contracts WHERE migration_name='20260807_avionics_product_reuse_v2' AND contract_version=1 AND contract_fingerprint='efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc'")" = \
  "1"
test -z "$(sqlite3 "$upgrade_database" "PRAGMA foreign_key_check")"
test "$(sqlite3 "$upgrade_database" "PRAGMA integrity_check")" = "ok"

sqlite3 -bail "$fresh_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/migrations/20260807_avionics_product_reuse_v2.sqlite.sql"
test "$(sqlite3 "$fresh_database" \
  "SELECT instr(lower(sql),\"check (policy_version = 'avionics_reuse_v2')\") FROM sqlite_schema WHERE type='table' AND name='avionics_product_reuse_attestations'")" -gt 0
test "$(sqlite3 "$fresh_database" \
  "SELECT count(*) FROM schema_migration_contracts WHERE migration_name='20260807_avionics_product_reuse_v2'")" = "1"
test -z "$(sqlite3 "$fresh_database" "PRAGMA foreign_key_check")"

for definition in \
  "$repository_root/schema/sqlite.sql" \
  "$repository_root/schema/postgres.sql" \
  "$repository_root/migrations/20260807_avionics_product_reuse_v2.sqlite.sql" \
  "$repository_root/migrations/20260807_avionics_product_reuse_v2.postgres.sql"
do
  grep -q 'avionics_reuse_v2' "$definition"
  grep -q '20260807_avionics_product_reuse_v2' "$definition"
done

echo "Avionics product reuse-v2 migration contract passed"
