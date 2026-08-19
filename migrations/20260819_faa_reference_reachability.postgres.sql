BEGIN;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
      AND NOT (
        contract_version = 1
        AND contract_fingerprint =
          'dccd6ae9208a650fe5381000fc485e7700fb113bc23520a183e305b49d64ec15'
      )
  ) THEN
    RAISE EXCEPTION
      'installed FAA reference reachability migration has a different contract';
  END IF;

  IF NOT EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ) AND (
    pg_catalog.to_regclass('public.faa_registry_aircraft') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_aircraft_references') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_engine_references') IS NULL
    OR pg_catalog.to_regprocedure(
         'public.validate_faa_reference_reachability()'
       ) IS NULL
    OR pg_catalog.to_regprocedure(
         'public.validate_faa_aircraft_reference_reachability()'
       ) IS NOT NULL
    OR pg_catalog.to_regprocedure(
         'public.validate_faa_engine_reference_reachability()'
       ) IS NOT NULL
    OR (
      SELECT COUNT(*)
      FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname IN (
          'faa_registry_aircraft_references_reachable',
          'faa_registry_engine_references_reachable'
        )
        AND trigger_row.tgrelid IN (
          pg_catalog.to_regclass('public.faa_registry_aircraft_references'),
          pg_catalog.to_regclass('public.faa_registry_engine_references')
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_reference_reachability()'
        )
    ) <> 2
  ) THEN
    RAISE EXCEPTION
      'pre-migration FAA reference reachability objects have an unexpected shape';
  END IF;
END
$migration_guard$;

DROP TRIGGER IF EXISTS faa_registry_aircraft_references_reachable
  ON public.faa_registry_aircraft_references;
DROP TRIGGER IF EXISTS faa_registry_engine_references_reachable
  ON public.faa_registry_engine_references;
DROP FUNCTION IF EXISTS public.validate_faa_reference_reachability();

CREATE OR REPLACE FUNCTION public.validate_faa_aircraft_reference_reachability()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.aircraft_code = NEW.aircraft_code
  ) THEN
    RAISE EXCEPTION
      'FAA aircraft reference must be reachable from a target match';
  END IF;
  RETURN NEW;
END;
$function$
SET search_path = pg_catalog;

CREATE OR REPLACE FUNCTION public.validate_faa_engine_reference_reachability()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.engine_code = NEW.engine_code
  ) THEN
    RAISE EXCEPTION
      'FAA engine reference must be reachable from a target match';
  END IF;
  RETURN NEW;
END;
$function$
SET search_path = pg_catalog;

CREATE TRIGGER faa_registry_aircraft_references_reachable
BEFORE INSERT ON public.faa_registry_aircraft_references
FOR EACH ROW
EXECUTE FUNCTION public.validate_faa_aircraft_reference_reachability();

CREATE TRIGGER faa_registry_engine_references_reachable
BEFORE INSERT ON public.faa_registry_engine_references
FOR EACH ROW
EXECUTE FUNCTION public.validate_faa_engine_reference_reachability();

INSERT INTO public.schema_migration_contracts AS installed_contract (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_faa_reference_reachability',
  1,
  'dccd6ae9208a650fe5381000fc485e7700fb113bc23520a183e305b49d64ec15',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = EXCLUDED.contract_version,
  contract_fingerprint = EXCLUDED.contract_fingerprint,
  installed_at = EXCLUDED.installed_at
WHERE installed_contract.contract_version = EXCLUDED.contract_version
  AND installed_contract.contract_fingerprint =
      EXCLUDED.contract_fingerprint;

COMMIT;
