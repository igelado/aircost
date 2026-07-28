#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
migration="$repository_root/migrations/20260729_aircraft_catalog_retrieval_keys.sqlite.sql"
postgres_migration="$repository_root/migrations/20260729_aircraft_catalog_retrieval_keys.postgres.sql"
legacy_database="$(mktemp /tmp/aircost-aircraft-keys-legacy.XXXXXX.sqlite3)"
collision_database="$(mktemp /tmp/aircost-aircraft-keys-collision.XXXXXX.sqlite3)"
fresh_database="$(mktemp /tmp/aircost-aircraft-keys-fresh.XXXXXX.sqlite3)"
trap 'rm -f "$legacy_database" "$collision_database" "$fresh_database"' EXIT

expect_failure() {
  local database="$1"
  local statement="$2"
  local expected_message="$3"
  local output
  if output="$(sqlite3 -bail "$database" "$statement" 2>&1)"; then
    echo "expected schema invariant failure: $statement" >&2
    exit 1
  fi
  if [[ "$output" != *"$expected_message"* ]]; then
    echo "schema invariant failed for the wrong reason" >&2
    echo "expected: $expected_message" >&2
    echo "actual: $output" >&2
    exit 1
  fi
}

create_legacy_schema() {
  local database="$1"
  sqlite3 -bail "$database" <<'SQL'
PRAGMA foreign_keys = ON;
CREATE TABLE aircraft_makes (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  approval_decision_id INTEGER NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE aircraft_model_families (
  id INTEGER PRIMARY KEY,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_make_id, normalized_name)
);
CREATE TABLE aircraft_generations (
  id INTEGER PRIMARY KEY,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_name)
);
CREATE TABLE aircraft_factory_packages (
  id INTEGER PRIMARY KEY,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_name)
);
CREATE TABLE aircraft_make_aliases (
  id INTEGER PRIMARY KEY,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id),
  alias TEXT NOT NULL,
  normalized_alias TEXT NOT NULL
);
CREATE TABLE aircraft_sale_listing_identity_assignments (
  id INTEGER PRIMARY KEY,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id),
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id),
  aircraft_generation_id INTEGER REFERENCES aircraft_generations(id),
  aircraft_factory_package_id INTEGER REFERENCES aircraft_factory_packages(id)
);
CREATE TABLE aircraft_valuation_compatibility_projections (
  id INTEGER PRIMARY KEY,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id),
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id),
  aircraft_generation_id INTEGER REFERENCES aircraft_generations(id),
  aircraft_factory_package_id INTEGER REFERENCES aircraft_factory_packages(id)
);

CREATE TRIGGER assigned_aircraft_make_immutable_update
BEFORE UPDATE ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments
  WHERE aircraft_make_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft makes are immutable'); END;
CREATE TRIGGER assigned_aircraft_family_immutable_update
BEFORE UPDATE ON aircraft_model_families
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments
  WHERE aircraft_model_family_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft model families are immutable'); END;
CREATE TRIGGER assigned_aircraft_generation_immutable_update
BEFORE UPDATE ON aircraft_generations
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments
  WHERE aircraft_generation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft generations are immutable'); END;
CREATE TRIGGER assigned_aircraft_package_immutable_update
BEFORE UPDATE ON aircraft_factory_packages
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments
  WHERE aircraft_factory_package_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft factory packages are immutable'); END;
CREATE TRIGGER assigned_aircraft_make_immutable_delete
BEFORE DELETE ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments
  WHERE aircraft_make_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft makes are immutable'); END;

CREATE TRIGGER compatibility_projected_make_immutable_update
BEFORE UPDATE ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections
  WHERE aircraft_make_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected make is immutable'); END;
CREATE TRIGGER compatibility_projected_family_immutable_update
BEFORE UPDATE ON aircraft_model_families
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections
  WHERE aircraft_model_family_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected family is immutable'); END;
CREATE TRIGGER compatibility_projected_generation_immutable_update
BEFORE UPDATE ON aircraft_generations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections
  WHERE aircraft_generation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected generation is immutable'); END;
CREATE TRIGGER compatibility_projected_package_immutable_update
BEFORE UPDATE ON aircraft_factory_packages
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections
  WHERE aircraft_factory_package_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected package is immutable'); END;
SQL
}

create_legacy_schema "$legacy_database"
sqlite3 -bail "$legacy_database" <<'SQL'
INSERT INTO aircraft_makes (
  id, name, normalized_name, approval_decision_id
) VALUES
  (1, 'TEXTRON AVIATION INC', 'cessna', 101),
  (2, 'Piper Aircraft, Inc.', 'piper', 102);
INSERT INTO aircraft_model_families (
  id, aircraft_make_id, name, normalized_name, approval_decision_id
) VALUES
  (1, 1, '182 Skylane', 'skylane', 201),
  (2, 2, 'PA-28', 'pa28', 202);
INSERT INTO aircraft_generations (
  id, aircraft_model_family_id, name, normalized_name, approval_decision_id
) VALUES
  (1, 1, 'G-6', 'g6', 301),
  (2, 2, 'Generation III', 'generation iii', 302);
INSERT INTO aircraft_factory_packages (
  id, aircraft_model_family_id, name, normalized_name, approval_decision_id
) VALUES
  (1, 1, 'GTS/Carbon', 'gtscarbon', 401),
  (2, 2, 'Archer LX', 'archer lx', 402);
INSERT INTO aircraft_make_aliases (
  id, aircraft_make_id, alias, normalized_alias
) VALUES (1, 1, 'Cessna', 'cessna');
INSERT INTO aircraft_sale_listing_identity_assignments (
  id, aircraft_make_id, aircraft_model_family_id,
  aircraft_generation_id, aircraft_factory_package_id
) VALUES (1, 1, 1, 1, 1);
INSERT INTO aircraft_valuation_compatibility_projections (
  id, aircraft_make_id, aircraft_model_family_id,
  aircraft_generation_id, aircraft_factory_package_id
) VALUES (1, 1, 1, 1, 1);
SQL

expect_failure "$legacy_database" \
  "UPDATE aircraft_makes SET normalized_name='textron aviation inc' WHERE id=1;" \
  "immutable"

sqlite3 -bail "$legacy_database" \
  ".read $migration" \
  ".read $migration"

test "$(sqlite3 "$legacy_database" \
  "SELECT id || ':' || name || ':' || normalized_name || ':' || approval_decision_id FROM aircraft_makes ORDER BY id")" = \
  $'1:TEXTRON AVIATION INC:textron aviation inc:101\n2:Piper Aircraft, Inc.:piper aircraft inc:102'
test "$(sqlite3 "$legacy_database" \
  "SELECT id || ':' || normalized_name || ':' || approval_decision_id FROM aircraft_model_families ORDER BY id")" = \
  $'1:182 skylane:201\n2:pa 28:202'
test "$(sqlite3 "$legacy_database" \
  "SELECT id || ':' || normalized_name || ':' || approval_decision_id FROM aircraft_generations ORDER BY id")" = \
  $'1:g 6:301\n2:generation iii:302'
test "$(sqlite3 "$legacy_database" \
  "SELECT id || ':' || normalized_name || ':' || approval_decision_id FROM aircraft_factory_packages ORDER BY id")" = \
  $'1:gts carbon:401\n2:archer lx:402'
test "$(sqlite3 "$legacy_database" \
  "SELECT aircraft_make_id || ':' || normalized_alias FROM aircraft_make_aliases")" = \
  "1:cessna"
test "$(sqlite3 "$legacy_database" \
  "SELECT aircraft_make_id || ':' || aircraft_model_family_id || ':' || aircraft_generation_id || ':' || aircraft_factory_package_id FROM aircraft_sale_listing_identity_assignments")" = \
  "1:1:1:1"
test "$(sqlite3 "$legacy_database" \
  "SELECT aircraft_make_id || ':' || aircraft_model_family_id || ':' || aircraft_generation_id || ':' || aircraft_factory_package_id FROM aircraft_valuation_compatibility_projections")" = \
  "1:1:1:1"
test "$(sqlite3 "$legacy_database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260729_aircraft_catalog_retrieval_keys'")" = \
  "1:b40b266fc450810cf89acc78c9405f4cd7d816ea38d389114e93a20cfea6901d"
test "$(sqlite3 "$legacy_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name LIKE 'aircraft_%_retrieval_key_validate_%'")" = \
  "8"
test "$(sqlite3 "$legacy_database" "PRAGMA foreign_key_check;")" = ""
test "$(sqlite3 "$legacy_database" "PRAGMA quick_check;")" = "ok"

# The migration restores both catalog immutability barriers. Updating only
# updated_at avoids the deterministic-key validator and proves immutability.
expect_failure "$legacy_database" \
  "UPDATE aircraft_makes SET updated_at=CURRENT_TIMESTAMP WHERE id=1;" \
  "immutable"
expect_failure "$legacy_database" \
  "DELETE FROM aircraft_makes WHERE id=1;" \
  "immutable"

# The persistent schema invariant rejects future mismatched keys while allowing
# the exact ASCII-only mechanical result. Non-ASCII code points are separators.
expect_failure "$legacy_database" \
  "INSERT INTO aircraft_makes (id,name,normalized_name,approval_decision_id) VALUES (3,'Daher–Socata','daher–socata',103);" \
  "deterministic retrieval key"
sqlite3 -bail "$legacy_database" \
  "INSERT INTO aircraft_makes (id,name,normalized_name,approval_decision_id) VALUES (3,'Daher–Socata','daher socata',103);"
sqlite3 -bail "$legacy_database" \
  "INSERT INTO aircraft_makes (id,name,normalized_name,approval_decision_id) VALUES (4,'TEXTRONKAVIATION İNC','textron aviation nc',104);"

# A final-state collision fails before any key or immutable trigger changes.
create_legacy_schema "$collision_database"
sqlite3 -bail "$collision_database" <<'SQL'
INSERT INTO aircraft_makes (
  id, name, normalized_name, approval_decision_id
) VALUES
  (1, 'Foo-Bar', 'foo legacy one', 101),
  (2, 'Foo Bar', 'foo legacy two', 102);
SQL
expect_failure "$collision_database" \
  ".read $migration" \
  "CHECK constraint failed"
test "$(sqlite3 "$collision_database" \
  "SELECT group_concat(normalized_name, ':') FROM (SELECT normalized_name FROM aircraft_makes ORDER BY id)")" = \
  "foo legacy one:foo legacy two"
test "$(sqlite3 "$collision_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name='assigned_aircraft_make_immutable_update'")" = \
  "1"
test "$(sqlite3 "$collision_database" \
  "SELECT count(*) FROM schema_migration_contracts WHERE migration_name='20260729_aircraft_catalog_retrieval_keys'")" = \
  "0"

# Fresh schemas carry the same validator and migration contract without needing
# a data rewrite.
sqlite3 -bail "$fresh_database" \
  ".read $repository_root/schema/sqlite.sql" \
  ".read $repository_root/schema/sqlite.sql"
test "$(sqlite3 "$fresh_database" \
  "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name LIKE 'aircraft_%_retrieval_key_validate_%'")" = \
  "8"
test "$(sqlite3 "$fresh_database" \
  "SELECT contract_version || ':' || contract_fingerprint FROM schema_migration_contracts WHERE migration_name='20260729_aircraft_catalog_retrieval_keys'")" = \
  "1:b40b266fc450810cf89acc78c9405f4cd7d816ea38d389114e93a20cfea6901d"
test "$(sqlite3 "$fresh_database" "PRAGMA foreign_key_check;")" = ""

# Backend parity: both migrations cover the same four hierarchy levels, carry
# the same contract, preflight collisions, and keep designations untouched.
for table in \
  aircraft_makes \
  aircraft_model_families \
  aircraft_generations \
  aircraft_factory_packages; do
  rg -q "$table" "$migration"
  rg -q "$table" "$postgres_migration"
done
rg -Fq "GLOB '[A-Za-z0-9]'" "$migration"
rg -Fq "regexp_replace(value, '[^A-Za-z0-9]+'" "$postgres_migration"
rg -q "preserve_assigned_aircraft_entity" "$postgres_migration"
rg -q "preserve_compatibility_projected_aircraft_entity" "$postgres_migration"
if rg -q "UPDATE aircraft_designations" "$migration" "$postgres_migration"; then
  echo "retrieval-key migration must not rewrite aircraft designations" >&2
  exit 1
fi

echo "aircraft catalog retrieval-key migration checks passed"
