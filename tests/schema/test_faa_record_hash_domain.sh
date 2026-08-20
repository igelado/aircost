#!/usr/bin/env bash
set -euo pipefail

database_url="${AIRCOST_TEST_POSTGRES_URL:?AIRCOST_TEST_POSTGRES_URL is required}"
export PGOPTIONS='-c client_min_messages=warning'

sqlite_db="$(mktemp /tmp/aircost-faa-hash-domain.XXXXXX.sqlite3)"
trap 'rm -f "$sqlite_db" "$sqlite_db-wal" "$sqlite_db-shm"' EXIT

reset_sqlite() {
  rm -f "$sqlite_db" "$sqlite_db-wal" "$sqlite_db-shm"
  sqlite3 -bail "$sqlite_db" '.read schema/sqlite.sql'
}

downgrade_sqlite() {
  sqlite3 -bail "$sqlite_db" <<'SQL'
PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;
DELETE FROM schema_migration_contracts
WHERE migration_name = '20260820_faa_record_hash_domain';
DROP INDEX idx_faa_registry_snapshots_current;
DROP TRIGGER faa_registry_snapshots_require_exact_evidence;
DROP TRIGGER faa_registry_snapshots_immutable_update;
DROP TRIGGER faa_registry_snapshots_immutable_delete;
DROP TABLE faa_registry_snapshots;
CREATE TABLE faa_registry_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  evidence_source_id INTEGER NOT NULL
    REFERENCES curation_evidence_sources(id) ON DELETE RESTRICT,
  snapshot_date TEXT NOT NULL,
  source_url TEXT NOT NULL,
  archive_sha256 TEXT NOT NULL,
  source_manifest_sha256 TEXT NOT NULL,
  target_set_sha256 TEXT NOT NULL,
  master_member_name TEXT NOT NULL CHECK (master_member_name = 'MASTER.txt'),
  master_member_sha256 TEXT NOT NULL,
  aircraft_member_name TEXT NOT NULL CHECK (aircraft_member_name = 'ACFTREF.txt'),
  aircraft_member_sha256 TEXT NOT NULL,
  engine_member_name TEXT NOT NULL CHECK (engine_member_name = 'ENGINE.txt'),
  engine_member_sha256 TEXT NOT NULL,
  imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (archive_sha256, target_set_sha256),
  CHECK (snapshot_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  CHECK (source_url LIKE 'https://faa.gov/%' OR source_url LIKE 'https://%.faa.gov/%'),
  CHECK (length(archive_sha256) = 64 AND archive_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(source_manifest_sha256) = 64 AND source_manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(target_set_sha256) = 64 AND target_set_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(master_member_sha256) = 64 AND master_member_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(aircraft_member_sha256) = 64 AND aircraft_member_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(engine_member_sha256) = 64 AND engine_member_sha256 NOT GLOB '*[^0-9a-f]*')
);
CREATE INDEX idx_faa_registry_snapshots_current
  ON faa_registry_snapshots (snapshot_date DESC, id DESC);
CREATE TRIGGER faa_registry_snapshots_require_exact_evidence
BEFORE INSERT ON faa_registry_snapshots
WHEN NOT EXISTS (
  SELECT 1 FROM curation_evidence_sources source
  WHERE source.id = NEW.evidence_source_id
    AND source.source_domain = 'faa.gov'
    AND source.source_tier = 'regulator_primary'
    AND source.source_url = NEW.source_url
    AND source.content_sha256 = NEW.archive_sha256
)
BEGIN SELECT RAISE(ABORT, 'FAA snapshot requires exact regulator evidence provenance'); END;
CREATE TRIGGER faa_registry_snapshots_immutable_update
BEFORE UPDATE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;
CREATE TRIGGER faa_registry_snapshots_immutable_delete
BEFORE DELETE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;
COMMIT;
PRAGMA foreign_keys = ON;
SQL
}

expect_sqlite_failure() {
  if sqlite3 -bail "$sqlite_db" \
    '.read migrations/20260820_faa_record_hash_domain.sqlite.sql' \
    >/dev/null 2>&1; then
    echo "SQLite FAA hash-domain migration unexpectedly accepted $1" >&2
    exit 1
  fi
}

reset_sqlite
sqlite3 "$sqlite_db" \
  "UPDATE schema_migration_contracts SET installed_at='2000-01-01' WHERE migration_name='20260820_faa_record_hash_domain'"
sqlite3 -bail "$sqlite_db" \
  '.read migrations/20260820_faa_record_hash_domain.sqlite.sql'
sqlite3 -batch -noheader "$sqlite_db" \
  "SELECT installed_at||':'||(SELECT count(*) FROM pragma_foreign_key_check)||':'||(SELECT count(*) FROM pragma_table_info('faa_registry_snapshots') WHERE name='record_hash_domain') FROM schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain'" \
  | grep -qx '2000-01-01:0:1'
sqlite3 "$sqlite_db" \
  "UPDATE schema_migration_contracts SET contract_fingerprint='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' WHERE migration_name='20260820_faa_record_hash_domain'"
expect_sqlite_failure mismatched-current-marker
sqlite3 -batch -noheader "$sqlite_db" \
  "SELECT contract_fingerprint||':'||(SELECT count(*) FROM pragma_table_info('faa_registry_snapshots') WHERE name='record_hash_domain') FROM schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain'" \
  | grep -qx 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:1'

reset_sqlite
sqlite3 "$sqlite_db" <<'SQL'
INSERT INTO curation_evidence_sources (
  source_url, source_title, source_domain, source_tier,
  content_sha256, retrieved_at
) VALUES (
  'https://www.faa.gov/registry/current-release.zip', 'corrupt current fixture',
  'faa.gov', 'regulator_primary', printf('%064d', 1), '2026-08-20'
);
PRAGMA ignore_check_constraints = ON;
INSERT INTO faa_registry_snapshots (
  evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256,
  master_member_name, master_member_sha256,
  aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256, record_hash_domain
) SELECT id, '2026-08-20', source_url, content_sha256,
  printf('%064d', 2), printf('%064d', 3),
  'MASTER.txt', printf('%064d', 4),
  'ACFTREF.txt', printf('%064d', 5),
  'ENGINE.txt', printf('%064d', 6), 'wrong-domain'
FROM curation_evidence_sources WHERE source_title = 'corrupt current fixture';
PRAGMA ignore_check_constraints = OFF;
SQL
expect_sqlite_failure mismatched-current-row-domain
sqlite3 -batch -noheader "$sqlite_db" \
  "SELECT record_hash_domain||':'||(SELECT count(*) FROM schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain') FROM faa_registry_snapshots" \
  | grep -qx 'wrong-domain:1'

reset_sqlite
sqlite3 "$sqlite_db" <<'SQL'
DROP TRIGGER faa_registry_snapshots_immutable_update;
CREATE TRIGGER faa_registry_snapshots_immutable_update
BEFORE UPDATE ON faa_registry_snapshots
BEGIN SELECT 1; END;
SQL
expect_sqlite_failure replaced-current-trigger
sqlite3 -batch -noheader "$sqlite_db" \
  "SELECT (instr((SELECT lower(sql) FROM sqlite_schema WHERE name='faa_registry_snapshots_immutable_update'), 'select 1') > 0)||':'||(SELECT count(*) FROM schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain')" \
  | grep -qx '1:1'

reset_sqlite
downgrade_sqlite
sqlite3 -bail "$sqlite_db" \
  '.read migrations/20260820_faa_record_hash_domain.sqlite.sql'
sqlite3 -batch -noheader "$sqlite_db" \
  "SELECT count(*) FROM pragma_table_info('faa_registry_snapshots') WHERE name='record_hash_domain'" \
  | grep -qx '1'

reset_sqlite
downgrade_sqlite
sqlite3 "$sqlite_db" <<'SQL'
INSERT INTO curation_evidence_sources (
  source_url, source_title, source_domain, source_tier,
  content_sha256, retrieved_at
) VALUES (
  'https://www.faa.gov/registry/release.zip', 'legacy fixture',
  'faa.gov', 'regulator_primary', printf('%064d', 1), '2026-08-20'
);
INSERT INTO faa_registry_snapshots (
  evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256,
  master_member_name, master_member_sha256,
  aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256
) SELECT id, '2026-08-20', source_url, content_sha256,
  printf('%064d', 2), printf('%064d', 3),
  'MASTER.txt', printf('%064d', 4),
  'ACFTREF.txt', printf('%064d', 5),
  'ENGINE.txt', printf('%064d', 6)
FROM curation_evidence_sources WHERE source_title = 'legacy fixture';
SQL
expect_sqlite_failure nonempty-legacy-projection
sqlite3 -batch -noheader "$sqlite_db" \
  "SELECT (SELECT count(*) FROM faa_registry_snapshots)||':'||(SELECT count(*) FROM schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain')" \
  | grep -qx '1:0'

reset_sqlite
downgrade_sqlite
sqlite3 "$sqlite_db" \
  'ALTER TABLE faa_registry_snapshots ADD COLUMN record_hash_domain TEXT'
expect_sqlite_failure unmarked-domain-column
sqlite3 -batch -noheader "$sqlite_db" \
  "SELECT (SELECT count(*) FROM pragma_table_info('faa_registry_snapshots') WHERE name='record_hash_domain')||':'||(SELECT count(*) FROM schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain')" \
  | grep -qx '1:0'

reset_postgres() {
  psql "$database_url" -v ON_ERROR_STOP=1 -q \
    -c 'DROP SCHEMA public CASCADE; CREATE SCHEMA public;'
  psql "$database_url" -v ON_ERROR_STOP=1 -q -f schema/postgres.sql
}

downgrade_postgres() {
  psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
BEGIN;
DELETE FROM public.schema_migration_contracts
WHERE migration_name = '20260820_faa_record_hash_domain';
ALTER TABLE public.faa_registry_aircraft
  DROP CONSTRAINT faa_registry_aircraft_snapshot_id_fkey;
ALTER TABLE public.faa_registry_aircraft_references
  DROP CONSTRAINT faa_registry_aircraft_references_snapshot_id_fkey;
ALTER TABLE public.faa_registry_coverage
  DROP CONSTRAINT faa_registry_coverage_snapshot_id_fkey;
ALTER TABLE public.faa_registry_engine_references
  DROP CONSTRAINT faa_registry_engine_references_snapshot_id_fkey;
ALTER TABLE public.aircraft_listing_identity_correction_decisions
  DROP CONSTRAINT aircraft_listing_identity_correct_faa_registry_snapshot_id_fkey;
ALTER TABLE public.aircraft_sale_listing_identity_assignments
  DROP CONSTRAINT aircraft_sale_listing_identity_as_faa_registry_snapshot_id_fkey;
ALTER TABLE public.aircraft_valuation_compatibility_projections
  DROP CONSTRAINT aircraft_valuation_compatibility__faa_registry_snapshot_id_fkey;
DROP TABLE public.faa_registry_snapshots;
CREATE TABLE public.faa_registry_snapshots (
  id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  evidence_source_id BIGINT NOT NULL REFERENCES public.curation_evidence_sources(id) ON DELETE RESTRICT,
  snapshot_date TEXT NOT NULL CHECK (snapshot_date ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'),
  source_url TEXT NOT NULL CHECK (source_url ~ '^https://([^. /]+[.])*faa[.]gov/'),
  archive_sha256 TEXT NOT NULL CHECK (archive_sha256 ~ '^[0-9a-f]{64}$'),
  source_manifest_sha256 TEXT NOT NULL CHECK (source_manifest_sha256 ~ '^[0-9a-f]{64}$'),
  target_set_sha256 TEXT NOT NULL CHECK (target_set_sha256 ~ '^[0-9a-f]{64}$'),
  master_member_name TEXT NOT NULL CHECK (master_member_name = 'MASTER.txt'),
  master_member_sha256 TEXT NOT NULL CHECK (master_member_sha256 ~ '^[0-9a-f]{64}$'),
  aircraft_member_name TEXT NOT NULL CHECK (aircraft_member_name = 'ACFTREF.txt'),
  aircraft_member_sha256 TEXT NOT NULL CHECK (aircraft_member_sha256 ~ '^[0-9a-f]{64}$'),
  engine_member_name TEXT NOT NULL CHECK (engine_member_name = 'ENGINE.txt'),
  engine_member_sha256 TEXT NOT NULL CHECK (engine_member_sha256 ~ '^[0-9a-f]{64}$'),
  imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (archive_sha256, target_set_sha256)
);
CREATE INDEX idx_faa_registry_snapshots_current
  ON public.faa_registry_snapshots (snapshot_date DESC, id DESC);
ALTER TABLE public.faa_registry_aircraft ADD CONSTRAINT faa_registry_aircraft_snapshot_id_fkey
  FOREIGN KEY (snapshot_id) REFERENCES public.faa_registry_snapshots(id) ON DELETE RESTRICT;
ALTER TABLE public.faa_registry_aircraft_references ADD CONSTRAINT faa_registry_aircraft_references_snapshot_id_fkey
  FOREIGN KEY (snapshot_id) REFERENCES public.faa_registry_snapshots(id) ON DELETE RESTRICT;
ALTER TABLE public.faa_registry_coverage ADD CONSTRAINT faa_registry_coverage_snapshot_id_fkey
  FOREIGN KEY (snapshot_id) REFERENCES public.faa_registry_snapshots(id) ON DELETE RESTRICT;
ALTER TABLE public.faa_registry_engine_references ADD CONSTRAINT faa_registry_engine_references_snapshot_id_fkey
  FOREIGN KEY (snapshot_id) REFERENCES public.faa_registry_snapshots(id) ON DELETE RESTRICT;
ALTER TABLE public.aircraft_listing_identity_correction_decisions
  ADD CONSTRAINT aircraft_listing_identity_correct_faa_registry_snapshot_id_fkey
  FOREIGN KEY (faa_registry_snapshot_id) REFERENCES public.faa_registry_snapshots(id) ON DELETE RESTRICT;
ALTER TABLE public.aircraft_sale_listing_identity_assignments
  ADD CONSTRAINT aircraft_sale_listing_identity_as_faa_registry_snapshot_id_fkey
  FOREIGN KEY (faa_registry_snapshot_id) REFERENCES public.faa_registry_snapshots(id) ON DELETE RESTRICT;
ALTER TABLE public.aircraft_valuation_compatibility_projections
  ADD CONSTRAINT aircraft_valuation_compatibility__faa_registry_snapshot_id_fkey
  FOREIGN KEY (faa_registry_snapshot_id) REFERENCES public.faa_registry_snapshots(id) ON DELETE RESTRICT;
CREATE TRIGGER faa_registry_snapshots_require_exact_evidence BEFORE INSERT
ON public.faa_registry_snapshots FOR EACH ROW
EXECUTE FUNCTION public.validate_faa_snapshot_evidence();
CREATE TRIGGER faa_registry_snapshots_immutable BEFORE UPDATE OR DELETE
ON public.faa_registry_snapshots FOR EACH ROW
EXECUTE FUNCTION public.preserve_faa_registry_data();
COMMIT;
SQL
}

expect_postgres_failure() {
  if psql "$database_url" -v ON_ERROR_STOP=1 -q \
    -f migrations/20260820_faa_record_hash_domain.postgres.sql \
    >/dev/null 2>&1; then
    echo "PostgreSQL FAA hash-domain migration unexpectedly accepted $1" >&2
    exit 1
  fi
}

reset_postgres
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c "UPDATE public.schema_migration_contracts SET installed_at='2000-01-01' WHERE migration_name='20260820_faa_record_hash_domain'"
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -f migrations/20260820_faa_record_hash_domain.postgres.sql
psql "$database_url" -v ON_ERROR_STOP=1 -qAt \
  -c "SELECT installed_at FROM public.schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain'" \
  | grep -qx '2000-01-01'
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c "UPDATE public.schema_migration_contracts SET contract_fingerprint='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' WHERE migration_name='20260820_faa_record_hash_domain'"
expect_postgres_failure mismatched-current-marker
psql "$database_url" -v ON_ERROR_STOP=1 -qAt \
  -c "SELECT contract_fingerprint||':'||(pg_catalog.to_regclass('public.faa_registry_snapshots') IS NOT NULL)::int FROM public.schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain'" \
  | grep -qx 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:1'

reset_postgres
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
DROP INDEX public.idx_faa_registry_snapshots_current;
CREATE INDEX idx_faa_registry_snapshots_current
  ON public.faa_registry_snapshots (source_url);
SQL
expect_postgres_failure replaced-current-index
psql "$database_url" -v ON_ERROR_STOP=1 -qAt \
  -c "SELECT (pg_catalog.pg_get_indexdef('public.idx_faa_registry_snapshots_current'::pg_catalog.regclass) LIKE '%(source_url)')::int || ':' || (EXISTS (SELECT 1 FROM public.schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain'))::int" \
  | grep -qx '1:1'

reset_postgres
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
CREATE OR REPLACE FUNCTION public.preserve_faa_registry_data()
RETURNS TRIGGER AS $function$
BEGIN
  RAISE EXCEPTION 'tampered FAA immutability function';
END;
$function$ LANGUAGE plpgsql
SET search_path = pg_catalog;
SQL
expect_postgres_failure replaced-current-function
psql "$database_url" -v ON_ERROR_STOP=1 -qAt \
  -c "SELECT (position('tampered FAA' IN prosrc) > 0)::int || ':' || (EXISTS (SELECT 1 FROM public.schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain'))::int FROM pg_catalog.pg_proc WHERE oid=pg_catalog.to_regprocedure('public.preserve_faa_registry_data()')" \
  | grep -qx '1:1'

reset_postgres
downgrade_postgres
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -f migrations/20260820_faa_record_hash_domain.postgres.sql
psql "$database_url" -v ON_ERROR_STOP=1 -qAt \
  -c "SELECT attnum FROM pg_catalog.pg_attribute WHERE attrelid=pg_catalog.to_regclass('public.faa_registry_snapshots') AND attname='record_hash_domain' AND NOT attisdropped" \
  | grep -qx '15'

reset_postgres
downgrade_postgres
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
INSERT INTO public.curation_evidence_sources (
  source_url, source_title, source_domain, source_tier,
  content_sha256, retrieved_at
) VALUES (
  'https://www.faa.gov/registry/release.zip', 'legacy fixture',
  'faa.gov', 'regulator_primary', repeat('1', 64), '2026-08-20'
);
INSERT INTO public.faa_registry_snapshots (
  evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256,
  master_member_name, master_member_sha256,
  aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256
) SELECT id, '2026-08-20', source_url, content_sha256,
  repeat('2', 64), repeat('3', 64),
  'MASTER.txt', repeat('4', 64),
  'ACFTREF.txt', repeat('5', 64),
  'ENGINE.txt', repeat('6', 64)
FROM public.curation_evidence_sources WHERE source_title = 'legacy fixture';
SQL
expect_postgres_failure nonempty-legacy-projection
psql "$database_url" -v ON_ERROR_STOP=1 -qAt \
  -c "SELECT (SELECT count(*) FROM public.faa_registry_snapshots)||':'||(SELECT count(*) FROM public.schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain')" \
  | grep -qx '1:0'

reset_postgres
downgrade_postgres
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
DROP INDEX public.idx_faa_registry_snapshots_current;
CREATE INDEX idx_faa_registry_snapshots_current
  ON public.faa_registry_snapshots (source_url);
SQL
expect_postgres_failure replaced-old-index
psql "$database_url" -v ON_ERROR_STOP=1 -qAt \
  -c "SELECT (pg_catalog.pg_get_indexdef('public.idx_faa_registry_snapshots_current'::pg_catalog.regclass) LIKE '%(source_url)')::int || ':' || (EXISTS (SELECT 1 FROM public.schema_migration_contracts WHERE migration_name='20260820_faa_record_hash_domain'))::int" \
  | grep -qx '1:0'
