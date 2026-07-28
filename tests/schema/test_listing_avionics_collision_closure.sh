#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
migration_database="$(mktemp /tmp/aircost-listing-avionics-closure.XXXXXX.sqlite3)"
schema_database="$(mktemp /tmp/aircost-listing-avionics-closure-schema.XXXXXX.sqlite3)"
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
  ".read $repository_root/migrations/20260805_listing_avionics_association_corroborations.sqlite.sql"
sqlite3 -bail "$migration_database" \
  ".read $repository_root/migrations/20260806_listing_avionics_collision_closure.sqlite.sql" \
  ".read $repository_root/migrations/20260806_listing_avionics_collision_closure.sqlite.sql"

sqlite3 -bail "$migration_database" <<'SQL'
INSERT INTO avionics_product_reuse_attestations
  (avionics_model_id, product_fingerprint)
VALUES (
  10, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
);
INSERT INTO aircraft_sale_listing_avionics (
  id, aircraft_sale_listing_id, avionics_model_id, quantity, source_notes,
  configuration_action, replaces_avionics_model_id
) VALUES (
  572, 2, 10, 1, 'Garmin unit P/N 011-00010-00', 'installed', NULL
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
INSERT INTO aircraft_sale_listing_avionics_corroboration_scopes (
  listing_link_id, association_role, collision_closure_sha256, policy_version
) VALUES (
  572, 'installed',
  'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
  'listing_avionics_collision_closure_v1'
);
SQL

expect_failure \
  "UPDATE aircraft_sale_listing_avionics_corroboration_scopes SET collision_closure_sha256='dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd' WHERE listing_link_id=572" \
  "collision-closure bindings are immutable"
expect_failure \
  "INSERT INTO aircraft_sale_listing_avionics_corroboration_scopes (listing_link_id,association_role,collision_closure_sha256,policy_version) VALUES (999,'installed','dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','listing_avionics_collision_closure_v1')" \
  "a scope cannot exist without its exact parent corroboration"

sqlite3 -bail "$migration_database" \
  "PRAGMA foreign_keys=ON" \
  "DELETE FROM aircraft_sale_listing_avionics_corroborations WHERE listing_link_id=572"
test "$(sqlite3 "$migration_database" \
  "SELECT count(*) FROM aircraft_sale_listing_avionics_corroboration_scopes")" = "0"
test "$(sqlite3 "$migration_database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260806_listing_avionics_collision_closure'")" = \
  "1:363fd039068667cca351c0009c0621e55942186a5d63804cf0e7da8212fa26b3"
test -z "$(sqlite3 "$migration_database" "PRAGMA foreign_key_check")"

sqlite3 -bail "$schema_database" ".read $repository_root/schema/sqlite.sql"
test "$(sqlite3 "$schema_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='aircraft_sale_listing_avionics_corroboration_scopes'")" = "1"
test "$(sqlite3 "$schema_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name='listing_avionics_corroboration_scopes_immutable_update'")" = "1"

for definition in \
  "$repository_root/migrations/20260806_listing_avionics_collision_closure.sqlite.sql" \
  "$repository_root/migrations/20260806_listing_avionics_collision_closure.postgres.sql" \
  "$repository_root/schema/sqlite.sql" \
  "$repository_root/schema/postgres.sql"
do
  rg -q 'aircraft_sale_listing_avionics_corroboration_scopes' "$definition"
  rg -q 'listing_avionics_collision_closure_v1' "$definition"
  rg -q '20260806_listing_avionics_collision_closure' "$definition"
done

echo "Listing-avionics collision-closure schema contract passed"
