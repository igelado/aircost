#!/usr/bin/env bash
set -euo pipefail

database_url="${AIRCOST_TEST_POSTGRES_URL:?AIRCOST_TEST_POSTGRES_URL is required}"
export PGOPTIONS='-c client_min_messages=warning'

reset_current_schema() {
  psql "$database_url" -v ON_ERROR_STOP=1 -q \
    -c 'DROP SCHEMA IF EXISTS attacker CASCADE; DROP SCHEMA public CASCADE; CREATE SCHEMA public;'
  psql "$database_url" -v ON_ERROR_STOP=1 -q -f schema/postgres.sql
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
