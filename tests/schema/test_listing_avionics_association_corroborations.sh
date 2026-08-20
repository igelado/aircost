#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
migration_database="$(mktemp /tmp/aircost-listing-avionics-corroboration.XXXXXX.sqlite3)"
schema_database="$(mktemp /tmp/aircost-listing-avionics-corroboration-schema.XXXXXX.sqlite3)"
trap 'rm -f "$migration_database" "$schema_database"' EXIT

expect_failure() {
  local statement="$1"
  local description="$2"
  if sqlite3 -bail "$migration_database" \
      "PRAGMA foreign_keys=ON" "$statement" >/dev/null 2>&1; then
    echo "Expected failure: $description" >&2
    exit 1
  fi
}

sqlite3 -bail "$migration_database" <<'SQL'
PRAGMA foreign_keys = ON;
CREATE TABLE aircraft_sale_listing_avionics (
  id INTEGER PRIMARY KEY,
  aircraft_sale_listing_id INTEGER NOT NULL,
  avionics_model_id INTEGER NOT NULL,
  quantity INTEGER NOT NULL,
  source_notes TEXT,
  configuration_action TEXT NOT NULL,
  replaces_avionics_model_id INTEGER
);
CREATE TABLE avionics_product_reuse_attestations (
  avionics_model_id INTEGER PRIMARY KEY,
  product_fingerprint TEXT NOT NULL
);
SQL

sqlite3 -bail "$migration_database" \
  ".read $repository_root/migrations/20260805_listing_avionics_association_corroborations.sqlite.sql" \
  ".read $repository_root/migrations/20260805_listing_avionics_association_corroborations.sqlite.sql"

sqlite3 -bail "$migration_database" <<'SQL'
INSERT INTO avionics_product_reuse_attestations
  (avionics_model_id, product_fingerprint)
VALUES
  (10, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'),
  (20, 'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc');
INSERT INTO aircraft_sale_listing_avionics (
  id, aircraft_sale_listing_id, avionics_model_id, quantity, source_notes,
  configuration_action, replaces_avionics_model_id
) VALUES (
  572, 2, 10, 1, 'Garmin GDL 69A shown in the listing', 'installed', NULL
);
INSERT INTO aircraft_sale_listing_avionics_corroborations (
  listing_link_id, association_role, avionics_model_id, observation_sha256,
  product_fingerprint, policy_version
) VALUES (
  572, 'installed', 10,
  'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
  'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  'listing_avionics_association_v1'
);
SQL

test "$(sqlite3 "$migration_database" \
  "SELECT count(*) FROM aircraft_sale_listing_avionics_corroborations")" = "1"

expect_failure \
  "UPDATE aircraft_sale_listing_avionics_corroborations SET observation_sha256='dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd' WHERE listing_link_id=572" \
  "corroboration conclusions are immutable"
expect_failure \
  "INSERT INTO aircraft_sale_listing_avionics_corroborations (listing_link_id,association_role,avionics_model_id,observation_sha256,product_fingerprint,policy_version) VALUES (572,'replacement',20,'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc','listing_avionics_association_v1')" \
  "a replacement corroboration requires the exact current replacement role"

# Moving a link between listings changes the observation identity even when
# every avionics field is unchanged.
sqlite3 -bail "$migration_database" \
  "UPDATE aircraft_sale_listing_avionics SET aircraft_sale_listing_id=3 WHERE id=572"
test "$(sqlite3 "$migration_database" \
  "SELECT count(*) FROM aircraft_sale_listing_avionics_corroborations")" = "0"

sqlite3 -bail "$migration_database" <<'SQL'
UPDATE aircraft_sale_listing_avionics
SET configuration_action = 'replaces',
    replaces_avionics_model_id = 20
WHERE id = 572;
INSERT INTO aircraft_sale_listing_avionics_corroborations (
  listing_link_id, association_role, avionics_model_id, observation_sha256,
  product_fingerprint, policy_version
) VALUES (
  572, 'replacement', 20,
  'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
  'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
  'listing_avionics_association_v1'
);
UPDATE aircraft_sale_listing_avionics
SET source_notes = 'changed retained listing evidence'
WHERE id = 572;
SQL
test "$(sqlite3 "$migration_database" \
  "SELECT count(*) FROM aircraft_sale_listing_avionics_corroborations")" = "0"

test "$(sqlite3 "$migration_database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260805_listing_avionics_association_corroborations'")" = \
  "1:2c4661b8bf76e1a28d5ab5c636ed100f5d73f845c44b9515e5f46c5827e66fc9"
test -z "$(sqlite3 "$migration_database" "PRAGMA foreign_key_check")"

sqlite3 -bail "$schema_database" \
  ".read $repository_root/schema/sqlite.sql"

# This migration remains executable as the verified predecessor for upgrades,
# but the canonical schema completed the authorization cutover and must not
# recreate its retired corroboration objects.
test "$(sqlite3 "$schema_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='aircraft_sale_listing_avionics_corroborations'")" = "0"
test "$(sqlite3 "$schema_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name LIKE 'listing_avionics_corroborations_%'")" = "0"

for definition in \
  "$repository_root/migrations/20260805_listing_avionics_association_corroborations.sqlite.sql" \
  "$repository_root/migrations/20260805_listing_avionics_association_corroborations.postgres.sql"
do
  rg -Uq \
    'AFTER UPDATE OF[[:space:]]+aircraft_sale_listing_id,[[:space:]]+avionics_model_id' \
    "$definition"
done

for definition in \
  "$repository_root/migrations/20260805_listing_avionics_association_corroborations.sqlite.sql" \
  "$repository_root/migrations/20260805_listing_avionics_association_corroborations.postgres.sql"
do
  rg -q \
    '20260805_listing_avionics_association_corroborations' \
    "$definition"
  rg -q \
    'listing_avionics_association_v1' \
    "$definition"
done

echo "Listing-avionics association corroboration schema contract passed"
