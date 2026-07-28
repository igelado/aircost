#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
database="$(mktemp /tmp/aircost-avionics-origins.XXXXXX.sqlite3)"
wrong_domain_database="$(mktemp /tmp/aircost-avionics-origins-wrong-domain.XXXXXX.sqlite3)"
wrong_identity_database="$(mktemp /tmp/aircost-avionics-origins-wrong-identity.XXXXXX.sqlite3)"
trap 'rm -f "$database" "$wrong_domain_database" "$wrong_identity_database"' EXIT

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

# A Garmin-looking identity with evidence on an unrelated origin must not
# activate either curated Garmin origin.
sqlite3 -bail "$wrong_domain_database" \
  ".read $repository_root/schema/sqlite.sql"
sqlite3 -bail "$wrong_domain_database" <<'SQL'
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key, identity_evidence_kind,
  identity_source_url, identity_source_title, identity_evidence_text,
  identity_confidence
) VALUES (
  'Garmin', 'garmin', 'authoritative_reference',
  'https://garmin.example/about',
  'Unrelated Garmin-looking identity fixture',
  'This fixture must not authorize any Garmin source origin.',
  'very_high'
);
SQL
test "$(sqlite3 "$wrong_domain_database" \
  "SELECT count(*) FROM avionics_authoritative_source_origins")" = "0"

# An unrelated canonical identity must not activate curated Garmin origins,
# even if its key and evidence URL are made to look like the reviewed identity.
sqlite3 -bail "$wrong_identity_database" \
  ".read $repository_root/schema/sqlite.sql"
sqlite3 -bail "$wrong_identity_database" <<'SQL'
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key, identity_evidence_kind,
  identity_source_url, identity_source_title, identity_evidence_text,
  identity_confidence
) VALUES (
  'Honeywell', 'garmin', 'authoritative_reference',
  'https://www.garmin.com/en-US/p/588901/',
  'Contradictory manufacturer identity fixture',
  'This fixture must not authorize any Garmin source origin.',
  'very_high'
);
SQL
test "$(sqlite3 "$wrong_identity_database" \
  "SELECT count(*) FROM avionics_authoritative_source_origins")" = "0"

sqlite3 -bail "$database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO users (email, display_name, auth_subject)
VALUES ('origin-reviewer@example.test', 'Origin Reviewer', 'origin-reviewer');
INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES ('Garmin', 'garmin');
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key, identity_evidence_kind,
  identity_source_url, identity_source_title, identity_evidence_text,
  identity_confidence
) VALUES (
  '  gArMiN  ', 'garmin', 'authoritative_reference',
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
SQL

# The schema was installed while the catalog was empty. Inserting the exact
# reviewed Garmin identity later must activate only the two curated origins,
# without waiting for a restart or rerunning the schema.
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM avionics_active_authoritative_source_origins WHERE authority_kind='manufacturer_primary'")" = "2"

sqlite3 -bail "$database" \
  ".read $repository_root/migrations/20260801_avionics_authoritative_source_origins.sqlite.sql" \
  ".read $repository_root/migrations/20260801_avionics_authoritative_source_origins.sqlite.sql"

test "$(sqlite3 "$database" \
  "SELECT count(*) FROM avionics_active_authoritative_source_origins WHERE authority_kind='manufacturer_primary'")" = "2"
test "$(sqlite3 "$database" \
  "SELECT group_concat(https_origin, ',') FROM (SELECT https_origin FROM avionics_active_authoritative_source_origins ORDER BY https_origin)")" = \
  "https://static.garmin.com,https://www.garmin.com"

expect_failure \
  "INSERT INTO avionics_authoritative_source_origins (authority_kind,avionics_manufacturer_identity_id,https_origin,evidence_source_url,evidence_source_title,evidence_text,approval_basis,approval_reason) SELECT 'manufacturer_primary',id,'https://*.garmin.com','https://*.garmin.com/catalog','Invalid wildcard origin','Wildcard authority must never be accepted as exact manufacturer evidence.','curated_bootstrap','Invalid wildcard authority fixture.' FROM avionics_manufacturer_identities WHERE normalized_identity_key='garmin'" \
  "wildcard origins cannot be approved"

origin_id="$(sqlite3 "$database" \
  "SELECT id FROM avionics_authoritative_source_origins WHERE https_origin='https://static.garmin.com'")"
reviewer_id="$(sqlite3 "$database" \
  "SELECT id FROM users WHERE auth_subject='origin-reviewer'")"
sqlite3 -bail "$database" \
  "INSERT INTO avionics_authoritative_source_origin_revocations (avionics_authoritative_source_origin_id,revoked_by_user_id,reason) VALUES ($origin_id,$reviewer_id,'Official source ownership or integrity can no longer be trusted.')"

test "$(sqlite3 "$database" \
  "SELECT count(*) FROM avionics_active_authoritative_source_origins WHERE https_origin='https://static.garmin.com'")" = "0"
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM avionics_authoritative_source_origins WHERE https_origin='https://static.garmin.com'")" = "1"
expect_failure \
  "DELETE FROM avionics_authoritative_source_origin_revocations WHERE avionics_authoritative_source_origin_id=$origin_id" \
  "revocation audit cannot be deleted"
expect_failure \
  "UPDATE avionics_authoritative_source_origins SET https_origin='https://cdn.garmin.com' WHERE id=$origin_id" \
  "authority approval cannot be rewritten"

test "$(sqlite3 "$database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260801_avionics_authoritative_source_origins'")" = \
  "2:f78087f6354d93d78dc8cebc895f285e38a91ca6f72dc2351acaaa88b49f9620"
test -z "$(sqlite3 "$database" "PRAGMA foreign_key_check")"

for required in \
  avionics_authoritative_source_origins \
  avionics_authoritative_source_origin_revocations \
  avionics_active_authoritative_source_origins \
  https://www.garmin.com \
  https://static.garmin.com \
  f78087f6354d93d78dc8cebc895f285e38a91ca6f72dc2351acaaa88b49f9620
do
  rg -q "$required" \
    "$repository_root/migrations/20260801_avionics_authoritative_source_origins.sqlite.sql" \
    "$repository_root/migrations/20260801_avionics_authoritative_source_origins.postgres.sql" \
    "$repository_root/schema/sqlite.sql" \
    "$repository_root/schema/postgres.sql"
done

for sqlite_definition in \
  "$repository_root/migrations/20260801_avionics_authoritative_source_origins.sqlite.sql" \
  "$repository_root/schema/sqlite.sql"
do
  rg -Fq "AND lower(trim(NEW.canonical_name)) = 'garmin'" \
    "$sqlite_definition"
done

for postgres_definition in \
  "$repository_root/migrations/20260801_avionics_authoritative_source_origins.postgres.sql" \
  "$repository_root/schema/postgres.sql"
do
  rg -Fq "AND LOWER(BTRIM(NEW.canonical_name)) = 'garmin'" \
    "$postgres_definition"
done
