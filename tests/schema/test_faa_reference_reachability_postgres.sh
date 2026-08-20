#!/usr/bin/env bash
set -euo pipefail

database_url="${AIRCOST_TEST_POSTGRES_URL:?AIRCOST_TEST_POSTGRES_URL is required}"
export PGOPTIONS='-c client_min_messages=warning'

reset_current_schema() {
  psql "$database_url" -v ON_ERROR_STOP=1 -q \
    -c 'DROP SCHEMA IF EXISTS attacker CASCADE; DROP SCHEMA public CASCADE; CREATE SCHEMA public;'
  psql "$database_url" -v ON_ERROR_STOP=1 -q -f schema/postgres.sql
  psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
DELETE FROM public.schema_migration_contracts
WHERE migration_name = '20260820_faa_record_hash_domain';
ALTER TABLE public.faa_registry_snapshots
  DROP CONSTRAINT faa_registry_snapshots_record_hash_domain_check;
ALTER TABLE public.faa_registry_snapshots DROP COLUMN record_hash_domain;
SQL
}

downgrade_to_pre_v1() {
  psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
DROP TRIGGER faa_registry_aircraft_references_reachable
  ON public.faa_registry_aircraft_references;
DROP TRIGGER faa_registry_engine_references_reachable
  ON public.faa_registry_engine_references;
DROP FUNCTION public.validate_faa_aircraft_reference_reachability();
DROP FUNCTION public.validate_faa_engine_reference_reachability();
DELETE FROM public.schema_migration_contracts
WHERE migration_name = '20260819_faa_reference_reachability';

CREATE OR REPLACE FUNCTION public.validate_faa_snapshot_evidence()
RETURNS TRIGGER AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM curation_evidence_sources source
    WHERE source.id = NEW.evidence_source_id
      AND source.source_domain = 'faa.gov'
      AND source.source_tier = 'regulator_primary'
      AND source.source_url = NEW.source_url
      AND source.content_sha256 = NEW.archive_sha256
  ) THEN
    RAISE EXCEPTION 'FAA snapshot requires exact regulator evidence provenance';
  END IF;
  RETURN NEW;
END;
$function$ LANGUAGE plpgsql;
ALTER FUNCTION public.validate_faa_snapshot_evidence() RESET ALL;

CREATE OR REPLACE FUNCTION public.validate_faa_coverage()
RETURNS TRIGGER AS $function$
BEGIN
  IF (NEW.lookup_status = 'matched' AND NOT EXISTS (
        SELECT 1 FROM faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id AND aircraft.n_number = NEW.n_number
      )) OR (NEW.lookup_status = 'absent' AND EXISTS (
        SELECT 1 FROM faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id AND aircraft.n_number = NEW.n_number
      )) THEN
    RAISE EXCEPTION 'FAA coverage must agree with its target match';
  END IF;
  RETURN NEW;
END;
$function$ LANGUAGE plpgsql;
ALTER FUNCTION public.validate_faa_coverage() RESET ALL;

CREATE OR REPLACE FUNCTION public.preserve_faa_registry_data()
RETURNS TRIGGER AS $function$
BEGIN
  RAISE EXCEPTION 'FAA registry snapshots and projections are immutable';
END;
$function$ LANGUAGE plpgsql;
ALTER FUNCTION public.preserve_faa_registry_data() RESET ALL;

CREATE FUNCTION public.validate_faa_reference_reachability()
RETURNS TRIGGER AS $function$
BEGIN
  IF TG_TABLE_NAME = 'faa_registry_aircraft_references' AND NOT EXISTS (
    SELECT 1 FROM faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.aircraft_code = NEW.aircraft_code
  ) THEN
    RAISE EXCEPTION 'FAA aircraft reference must be reachable from a target match';
  END IF;
  IF TG_TABLE_NAME = 'faa_registry_engine_references' AND NOT EXISTS (
    SELECT 1 FROM faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.engine_code = NEW.engine_code
  ) THEN
    RAISE EXCEPTION 'FAA engine reference must be reachable from a target match';
  END IF;
  RETURN NEW;
END;
$function$ LANGUAGE plpgsql;

CREATE TRIGGER faa_registry_aircraft_references_reachable
BEFORE INSERT ON public.faa_registry_aircraft_references
FOR EACH ROW EXECUTE FUNCTION public.validate_faa_reference_reachability();
CREATE TRIGGER faa_registry_engine_references_reachable
BEFORE INSERT ON public.faa_registry_engine_references
FOR EACH ROW EXECUTE FUNCTION public.validate_faa_reference_reachability();
SQL
}

expect_migration_failure() {
  if psql "$database_url" -v ON_ERROR_STOP=1 -q \
    -f migrations/20260819_faa_reference_reachability.postgres.sql >/dev/null 2>&1; then
    echo "FAA reference migration unexpectedly accepted invalid pre-state: $1" >&2
    exit 1
  fi
}

assert_pre_v1_survived_rollback() {
  psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '1:0:0'
SELECT
  (pg_catalog.to_regprocedure(
    'public.validate_faa_reference_reachability()'
  ) IS NOT NULL)::int || ':' ||
  (pg_catalog.to_regprocedure(
    'public.validate_faa_aircraft_reference_reachability()'
  ) IS NOT NULL)::int || ':' ||
  (EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ))::int;
SQL
}

insert_snapshot_fixture() {
  psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
INSERT INTO public.curation_evidence_sources (
  source_url, source_title, source_domain, source_tier,
  content_sha256, retrieved_at
) VALUES (
  'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
  'FAA releasable aircraft migration fixture', 'faa.gov',
  'regulator_primary', repeat('8', 64), '2026-08-19'
);
INSERT INTO public.faa_registry_snapshots (
  evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256,
  master_member_name, master_member_sha256,
  aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256
) SELECT
  id, '2026-08-19', source_url, content_sha256,
  repeat('9', 64), repeat('a', 64),
  'MASTER.txt', repeat('b', 64),
  'ACFTREF.txt', repeat('c', 64),
  'ENGINE.txt', repeat('d', 64)
FROM public.curation_evidence_sources
WHERE content_sha256 = repeat('8', 64);
SQL
}

# Fresh canonical schema and its insertion fixture.
reset_current_schema
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -f tests/schema/faa_reference_reachability.postgres.sql
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -f migrations/20260819_faa_reference_reachability.postgres.sql

# Exact reruns preserve the original installation timestamp.
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c "UPDATE public.schema_migration_contracts SET installed_at = '2000-01-01' WHERE migration_name = '20260819_faa_reference_reachability'"
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -f migrations/20260819_faa_reference_reachability.postgres.sql
psql "$database_url" -v ON_ERROR_STOP=1 -qAt \
  -c "SELECT installed_at FROM public.schema_migration_contracts WHERE migration_name = '20260819_faa_reference_reachability'" \
  | grep -qx '2000-01-01'

# A hostile caller search_path cannot make a same-named attacker parent satisfy
# the marker-present FK contract, and the failed migration changes nothing.
reset_current_schema
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
CREATE SCHEMA attacker;
CREATE TABLE attacker.faa_registry_snapshots (id BIGINT PRIMARY KEY);
ALTER TABLE public.faa_registry_coverage
  DROP CONSTRAINT faa_registry_coverage_snapshot_id_fkey;
SET search_path = attacker, public;
ALTER TABLE public.faa_registry_coverage
  ADD CONSTRAINT faa_registry_coverage_snapshot_id_fkey
  FOREIGN KEY (snapshot_id) REFERENCES faa_registry_snapshots(id)
  ON DELETE RESTRICT;
RESET search_path;
SQL
expect_migration_failure attacker-parent-current
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx 'attacker:1'
SELECT referenced_namespace.nspname || ':' || (EXISTS (
  SELECT 1 FROM public.schema_migration_contracts
  WHERE migration_name = '20260819_faa_reference_reachability'
))::int
FROM pg_catalog.pg_constraint constraint_row
JOIN pg_catalog.pg_class referenced_relation
  ON referenced_relation.oid = constraint_row.confrelid
JOIN pg_catalog.pg_namespace referenced_namespace
  ON referenced_namespace.oid = referenced_relation.relnamespace
WHERE constraint_row.conrelid = pg_catalog.to_regclass('public.faa_registry_coverage')
  AND constraint_row.conname = 'faa_registry_coverage_snapshot_id_fkey';
SQL

reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
CREATE SCHEMA attacker;
CREATE TABLE attacker.faa_registry_snapshots (id BIGINT PRIMARY KEY);
ALTER TABLE public.faa_registry_coverage
  DROP CONSTRAINT faa_registry_coverage_snapshot_id_fkey;
SET search_path = attacker, public;
ALTER TABLE public.faa_registry_coverage
  ADD CONSTRAINT faa_registry_coverage_snapshot_id_fkey
  FOREIGN KEY (snapshot_id) REFERENCES faa_registry_snapshots(id)
  ON DELETE RESTRICT;
RESET search_path;
SQL
expect_migration_failure attacker-parent-old
assert_pre_v1_survived_rollback
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx 'attacker'
SELECT referenced_namespace.nspname
FROM pg_catalog.pg_constraint constraint_row
JOIN pg_catalog.pg_class referenced_relation
  ON referenced_relation.oid = constraint_row.confrelid
JOIN pg_catalog.pg_namespace referenced_namespace
  ON referenced_namespace.oid = referenced_relation.relnamespace
WHERE constraint_row.conrelid = pg_catalog.to_regclass('public.faa_registry_coverage')
  AND constraint_row.conname = 'faa_registry_coverage_snapshot_id_fkey';
SQL

# A marker-present current schema is accepted only while its objects remain
# exact. Tampering fails before the migration can replace the damaged object.
reset_current_schema
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c 'ALTER TABLE public.faa_registry_coverage DISABLE TRIGGER faa_registry_coverage_consistent'
expect_migration_failure disabled-current-trigger
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx 'D:1'
SELECT trigger_row.tgenabled::text || ':' || (EXISTS (
  SELECT 1 FROM public.schema_migration_contracts
  WHERE migration_name = '20260819_faa_reference_reachability'
))::int
FROM pg_catalog.pg_trigger trigger_row
WHERE trigger_row.tgname = 'faa_registry_coverage_consistent'
  AND trigger_row.tgrelid = pg_catalog.to_regclass('public.faa_registry_coverage');
SQL

reset_current_schema
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
CREATE OR REPLACE FUNCTION public.validate_faa_coverage()
RETURNS TRIGGER LANGUAGE plpgsql
AS $function$BEGIN RETURN NEW; END;$function$
SET search_path = pg_catalog;
SQL
expect_migration_failure altered-current-function
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '1:1'
SELECT
  (routine.prosrc = 'BEGIN RETURN NEW; END;')::int || ':' || (EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ))::int
FROM pg_catalog.pg_proc routine
WHERE routine.oid = pg_catalog.to_regprocedure('public.validate_faa_coverage()');
SQL

# Projection-table drift is rejected before any current object is replaced.
reset_current_schema
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c 'ALTER TABLE public.faa_registry_coverage DROP CONSTRAINT faa_registry_coverage_lookup_status_check'
expect_migration_failure dropped-current-coverage-constraint
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '0:1'
SELECT
  (EXISTS (
    SELECT 1 FROM pg_catalog.pg_constraint
    WHERE conrelid = pg_catalog.to_regclass('public.faa_registry_coverage')
      AND conname = 'faa_registry_coverage_lookup_status_check'
  ))::int || ':' || (EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ))::int;
SQL

reset_current_schema
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
CREATE FUNCTION public.unexpected_faa_trigger_function()
RETURNS TRIGGER LANGUAGE plpgsql
AS $function$BEGIN RETURN NEW; END;$function$
SET search_path = pg_catalog;
CREATE TRIGGER unexpected_faa_trigger BEFORE INSERT
ON public.faa_registry_aircraft FOR EACH ROW
EXECUTE FUNCTION public.unexpected_faa_trigger_function();
SQL
expect_migration_failure unexpected-current-trigger
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '1:1'
SELECT
  (EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger
    WHERE tgrelid = pg_catalog.to_regclass('public.faa_registry_aircraft')
      AND tgname = 'unexpected_faa_trigger'
      AND NOT tgisinternal
  ))::int || ':' || (EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ))::int;
SQL

reset_current_schema
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c 'CREATE INDEX unexpected_faa_index ON public.faa_registry_engine_references (model_name)'
expect_migration_failure unexpected-current-index
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '1:1'
SELECT
  (pg_catalog.to_regclass('public.unexpected_faa_index') IS NOT NULL)::int || ':' ||
  (EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ))::int;
SQL

reset_current_schema
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c 'ALTER TABLE public.faa_registry_coverage ADD CONSTRAINT unexpected_faa_constraint CHECK (length(lookup_status) > 0)'
expect_migration_failure unexpected-current-constraint
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '1:1'
SELECT
  (EXISTS (
    SELECT 1 FROM pg_catalog.pg_constraint
    WHERE conrelid = pg_catalog.to_regclass('public.faa_registry_coverage')
      AND conname = 'unexpected_faa_constraint'
  ))::int || ':' || (EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ))::int;
SQL

# Exact main/pre-v1 shape upgrades, remains usable, and is idempotent.
reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -f migrations/20260819_faa_reference_reachability.postgres.sql
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -f tests/schema/faa_reference_reachability.postgres.sql
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -f migrations/20260819_faa_reference_reachability.postgres.sql

# A mismatched ledger contract is rejected without replacing old objects.
reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint
) VALUES (
  '20260819_faa_reference_reachability', 1, repeat('0', 64)
);
SQL
expect_migration_failure mismatched-contract
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c "DELETE FROM public.schema_migration_contracts WHERE migration_name = '20260819_faa_reference_reachability'"
assert_pre_v1_survived_rollback

# A disabled old trigger is not silently blessed or partly migrated.
reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c 'ALTER TABLE public.faa_registry_aircraft_references DISABLE TRIGGER faa_registry_aircraft_references_reachable'
expect_migration_failure disabled-old-trigger
assert_pre_v1_survived_rollback

# Marker absence does not authorize repairing unrelated weakened provenance
# functions; the entire historical FAA object contract must be exact.
reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
CREATE OR REPLACE FUNCTION public.validate_faa_coverage()
RETURNS TRIGGER LANGUAGE plpgsql
AS $function$BEGIN RETURN NEW; END;$function$;
ALTER FUNCTION public.validate_faa_coverage() RESET ALL;
SQL
expect_migration_failure altered-old-coverage-function
assert_pre_v1_survived_rollback
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '1'
SELECT (routine.prosrc = 'BEGIN RETURN NEW; END;')::int
FROM pg_catalog.pg_proc routine
WHERE routine.oid = pg_catalog.to_regprocedure('public.validate_faa_coverage()');
SQL

# Marker absence authorizes only the exact historical shape; unrelated table
# and attached-object drift remains intact after the rejected transaction.
reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c 'ALTER TABLE public.faa_registry_coverage DROP CONSTRAINT faa_registry_coverage_lookup_status_check'
expect_migration_failure dropped-old-coverage-constraint
assert_pre_v1_survived_rollback
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '0'
SELECT (EXISTS (
  SELECT 1 FROM pg_catalog.pg_constraint
  WHERE conrelid = pg_catalog.to_regclass('public.faa_registry_coverage')
    AND conname = 'faa_registry_coverage_lookup_status_check'
))::int;
SQL

reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
CREATE FUNCTION public.unexpected_faa_trigger_function()
RETURNS TRIGGER LANGUAGE plpgsql
AS $function$BEGIN RETURN NEW; END;$function$;
CREATE TRIGGER unexpected_faa_trigger BEFORE INSERT
ON public.faa_registry_aircraft FOR EACH ROW
EXECUTE FUNCTION public.unexpected_faa_trigger_function();
SQL
expect_migration_failure unexpected-old-trigger
assert_pre_v1_survived_rollback
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '1'
SELECT (EXISTS (
  SELECT 1 FROM pg_catalog.pg_trigger
  WHERE tgrelid = pg_catalog.to_regclass('public.faa_registry_aircraft')
    AND tgname = 'unexpected_faa_trigger'
    AND NOT tgisinternal
))::int;
SQL

reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c 'CREATE INDEX unexpected_faa_index ON public.faa_registry_engine_references (model_name)'
expect_migration_failure unexpected-old-index
assert_pre_v1_survived_rollback
psql "$database_url" -v ON_ERROR_STOP=1 -qAt \
  -c "SELECT (pg_catalog.to_regclass('public.unexpected_faa_index') IS NOT NULL)::int" \
  | grep -qx '1'

reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -c 'ALTER TABLE public.faa_registry_coverage ADD CONSTRAINT unexpected_faa_constraint CHECK (length(lookup_status) > 0)'
expect_migration_failure unexpected-old-constraint
assert_pre_v1_survived_rollback
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx '1'
SELECT (EXISTS (
  SELECT 1 FROM pg_catalog.pg_constraint
  WHERE conrelid = pg_catalog.to_regclass('public.faa_registry_coverage')
    AND conname = 'unexpected_faa_constraint'
))::int;
SQL

# Shadow objects and caller search_path cannot redirect the migration.
reset_current_schema
downgrade_to_pre_v1
psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
CREATE SCHEMA attacker;
CREATE FUNCTION attacker.validate_faa_aircraft_reference_reachability()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$BEGIN RETURN NEW; END;$function$;
SET search_path = attacker, public;
SQL
PGOPTIONS='-c client_min_messages=warning -c search_path=attacker,public' \
  psql "$database_url" -v ON_ERROR_STOP=1 -q \
  -f migrations/20260819_faa_reference_reachability.postgres.sql
psql "$database_url" -v ON_ERROR_STOP=1 -qAt <<'SQL' | grep -qx 'public:public'
SELECT aircraft_namespace.nspname || ':' || engine_namespace.nspname
FROM pg_catalog.pg_trigger aircraft_trigger
JOIN pg_catalog.pg_proc aircraft_function
  ON aircraft_function.oid = aircraft_trigger.tgfoid
JOIN pg_catalog.pg_namespace aircraft_namespace
  ON aircraft_namespace.oid = aircraft_function.pronamespace
JOIN pg_catalog.pg_trigger engine_trigger
  ON engine_trigger.tgname = 'faa_registry_engine_references_reachable'
JOIN pg_catalog.pg_proc engine_function ON engine_function.oid = engine_trigger.tgfoid
JOIN pg_catalog.pg_namespace engine_namespace
  ON engine_namespace.oid = engine_function.pronamespace
WHERE aircraft_trigger.tgname = 'faa_registry_aircraft_references_reachable';
SQL

# Existing unreachable aircraft and engine projections are rejected atomically.
for reference_kind in aircraft engine; do
  reset_current_schema
  downgrade_to_pre_v1
  insert_snapshot_fixture
  if [[ "$reference_kind" == aircraft ]]; then
    psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
ALTER TABLE public.faa_registry_aircraft_references
  DISABLE TRIGGER faa_registry_aircraft_references_reachable;
INSERT INTO public.faa_registry_aircraft_references (
  snapshot_id, aircraft_code, manufacturer_name, model_name
) SELECT id, 'UNREACH', 'INVALID', 'INVALID'
  FROM public.faa_registry_snapshots WHERE archive_sha256 = repeat('8', 64);
ALTER TABLE public.faa_registry_aircraft_references
  ENABLE TRIGGER faa_registry_aircraft_references_reachable;
SQL
  else
    psql "$database_url" -v ON_ERROR_STOP=1 -q <<'SQL'
ALTER TABLE public.faa_registry_engine_references
  DISABLE TRIGGER faa_registry_engine_references_reachable;
INSERT INTO public.faa_registry_engine_references (
  snapshot_id, engine_code, manufacturer_name, model_name
) SELECT id, 'BAD01', 'INVALID', 'INVALID'
  FROM public.faa_registry_snapshots WHERE archive_sha256 = repeat('8', 64);
ALTER TABLE public.faa_registry_engine_references
  ENABLE TRIGGER faa_registry_engine_references_reachable;
SQL
  fi
  expect_migration_failure "unreachable-$reference_kind-reference"
  assert_pre_v1_survived_rollback
done
