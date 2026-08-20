BEGIN;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

-- Validate one and only one supported state before any FAA object is replaced:
-- the exact historical contract when the marker is absent, or the exact
-- installed contract when it is present. This makes reruns idempotent without
-- turning the migration into a repair path for tampered current objects.
DO $exact_state_guard$
DECLARE
  marker_installed BOOLEAN;
  trigger_matches BIGINT;
  trigger_name_count BIGINT;
  function_matches BIGINT;
BEGIN
  SELECT EXISTS (
    SELECT 1 FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ) INTO marker_installed;

  IF pg_catalog.to_regclass('public.faa_registry_snapshots') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_aircraft') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_aircraft_references') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_engine_references') IS NULL
    OR pg_catalog.to_regclass('public.faa_registry_coverage') IS NULL
  THEN
    RAISE EXCEPTION 'FAA registry projection relations are incomplete';
  END IF;

  WITH expected(
    trigger_name, relation_name, function_name, trigger_type
  ) AS (
    VALUES
      ('faa_registry_snapshots_require_exact_evidence',
       'faa_registry_snapshots', 'validate_faa_snapshot_evidence', 7),
      ('faa_registry_aircraft_references_reachable',
       'faa_registry_aircraft_references',
       CASE WHEN marker_installed
         THEN 'validate_faa_aircraft_reference_reachability'
         ELSE 'validate_faa_reference_reachability'
       END, 7),
      ('faa_registry_engine_references_reachable',
       'faa_registry_engine_references',
       CASE WHEN marker_installed
         THEN 'validate_faa_engine_reference_reachability'
         ELSE 'validate_faa_reference_reachability'
       END, 7),
      ('faa_registry_coverage_consistent',
       'faa_registry_coverage', 'validate_faa_coverage', 7),
      ('faa_registry_snapshots_immutable',
       'faa_registry_snapshots', 'preserve_faa_registry_data', 27),
      ('faa_registry_aircraft_immutable',
       'faa_registry_aircraft', 'preserve_faa_registry_data', 27),
      ('faa_registry_aircraft_references_immutable',
       'faa_registry_aircraft_references', 'preserve_faa_registry_data', 27),
      ('faa_registry_engine_references_immutable',
       'faa_registry_engine_references', 'preserve_faa_registry_data', 27),
      ('faa_registry_coverage_immutable',
       'faa_registry_coverage', 'preserve_faa_registry_data', 27)
  )
  SELECT COUNT(*) INTO trigger_matches
  FROM expected
  JOIN pg_catalog.pg_trigger trigger_row
    ON trigger_row.tgname = expected.trigger_name
   AND NOT trigger_row.tgisinternal
  JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
  JOIN pg_catalog.pg_namespace relation_namespace
    ON relation_namespace.oid = relation.relnamespace
  JOIN pg_catalog.pg_proc routine ON routine.oid = trigger_row.tgfoid
  JOIN pg_catalog.pg_namespace routine_namespace
    ON routine_namespace.oid = routine.pronamespace
  WHERE relation_namespace.nspname = 'public'
    AND relation.relname = expected.relation_name
    AND routine_namespace.nspname = 'public'
    AND routine.proname = expected.function_name
    AND routine.pronargs = 0
    AND trigger_row.tgtype = expected.trigger_type
    AND trigger_row.tgenabled = 'O'
    AND trigger_row.tgqual IS NULL
    AND pg_catalog.cardinality(trigger_row.tgattr) = 0
    AND trigger_row.tgnargs = 0;

  SELECT COUNT(*) INTO trigger_name_count
  FROM pg_catalog.pg_trigger trigger_row
  WHERE NOT trigger_row.tgisinternal
    AND trigger_row.tgname IN (
      'faa_registry_snapshots_require_exact_evidence',
      'faa_registry_aircraft_references_reachable',
      'faa_registry_engine_references_reachable',
      'faa_registry_coverage_consistent',
      'faa_registry_snapshots_immutable',
      'faa_registry_aircraft_immutable',
      'faa_registry_aircraft_references_immutable',
      'faa_registry_engine_references_immutable',
      'faa_registry_coverage_immutable'
    );

  IF marker_installed THEN
    WITH expected(function_name, function_source) AS (
      VALUES
        ('validate_faa_snapshot_evidence', $snapshot_function$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM public.curation_evidence_sources source
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
$snapshot_function$),
        ('validate_faa_aircraft_reference_reachability', $aircraft_function$
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
$aircraft_function$),
        ('validate_faa_engine_reference_reachability', $engine_function$
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
$engine_function$),
        ('validate_faa_coverage', $coverage_function$
BEGIN
  IF (NEW.lookup_status = 'matched' AND NOT EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) OR (NEW.lookup_status = 'absent' AND EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) THEN
    RAISE EXCEPTION 'FAA coverage must agree with its target match';
  END IF;
  RETURN NEW;
END;
$coverage_function$),
        ('preserve_faa_registry_data', $immutability_function$
BEGIN
  RAISE EXCEPTION 'FAA registry snapshots and projections are immutable';
END;
$immutability_function$)
    )
    SELECT COUNT(*) INTO function_matches
    FROM expected
    JOIN pg_catalog.pg_proc routine ON routine.proname = expected.function_name
    JOIN pg_catalog.pg_namespace routine_namespace
      ON routine_namespace.oid = routine.pronamespace
    JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
    WHERE routine_namespace.nspname = 'public'
      AND routine.prosrc = expected.function_source
      AND routine.proconfig = ARRAY['search_path=pg_catalog']
      AND language.lanname = 'plpgsql'
      AND routine.prorettype = 'trigger'::pg_catalog.regtype
      AND routine.pronargs = 0
      AND routine.prokind = 'f'
      AND NOT routine.prosecdef
      AND NOT routine.proisstrict
      AND routine.provolatile = 'v'
      AND routine.proparallel = 'u';

    IF trigger_matches <> 9 OR trigger_name_count <> 9
      OR function_matches <> 5
      OR pg_catalog.to_regprocedure(
        'public.validate_faa_reference_reachability()'
      ) IS NOT NULL
    THEN
      RAISE EXCEPTION
        'installed FAA reference reachability objects have an unexpected shape';
    END IF;
  ELSE
    WITH expected(function_name, function_source) AS (
      VALUES
        ('validate_faa_snapshot_evidence', $old_snapshot_function$
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
$old_snapshot_function$),
        ('validate_faa_reference_reachability', $old_reference_function$
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
$old_reference_function$),
        ('validate_faa_coverage', $old_coverage_function$
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
$old_coverage_function$),
        ('preserve_faa_registry_data', $old_immutability_function$
BEGIN
  RAISE EXCEPTION 'FAA registry snapshots and projections are immutable';
END;
$old_immutability_function$)
    )
    SELECT COUNT(*) INTO function_matches
    FROM expected
    JOIN pg_catalog.pg_proc routine ON routine.proname = expected.function_name
    JOIN pg_catalog.pg_namespace routine_namespace
      ON routine_namespace.oid = routine.pronamespace
    JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
    WHERE routine_namespace.nspname = 'public'
      AND routine.prosrc = expected.function_source
      AND routine.proconfig IS NULL
      AND language.lanname = 'plpgsql'
      AND routine.prorettype = 'trigger'::pg_catalog.regtype
      AND routine.pronargs = 0
      AND routine.prokind = 'f'
      AND NOT routine.prosecdef
      AND NOT routine.proisstrict
      AND routine.provolatile = 'v'
      AND routine.proparallel = 'u';

    IF trigger_matches <> 9 OR trigger_name_count <> 9
      OR function_matches <> 4
      OR pg_catalog.to_regprocedure(
        'public.validate_faa_aircraft_reference_reachability()'
      ) IS NOT NULL
      OR pg_catalog.to_regprocedure(
        'public.validate_faa_engine_reference_reachability()'
      ) IS NOT NULL
    THEN
      RAISE EXCEPTION
        'pre-migration FAA reference reachability objects have an unexpected shape';
    END IF;
  END IF;
END
$exact_state_guard$;

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
      AND NOT (
        contract_version = 1
        AND contract_fingerprint =
          'fc6451ffe8e1ee2034e76480767d16d6c37463461d9e684687448b4d43f96bef'
      )
  ) THEN
    RAISE EXCEPTION
      'installed FAA reference reachability migration has a different contract';
  END IF;

  IF NOT EXISTS (
    SELECT 1
    FROM public.schema_migration_contracts
    WHERE migration_name = '20260819_faa_reference_reachability'
  ) THEN
    IF (
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
    OR NOT EXISTS (
      SELECT 1
      FROM pg_catalog.pg_proc routine
      JOIN pg_catalog.pg_namespace routine_namespace
        ON routine_namespace.oid = routine.pronamespace
      JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
      WHERE routine.oid = pg_catalog.to_regprocedure(
        'public.validate_faa_reference_reachability()'
      )
        AND routine_namespace.nspname = 'public'
        AND routine.prosrc = $old_function$
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
$old_function$
        AND routine.proconfig IS NULL
        AND language.lanname = 'plpgsql'
        AND routine.prorettype = 'trigger'::pg_catalog.regtype
        AND routine.pronargs = 0
        AND routine.prokind = 'f'
        AND NOT routine.prosecdef
        AND NOT routine.proisstrict
        AND routine.provolatile = 'v'
        AND routine.proparallel = 'u'
    )
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
        AND trigger_row.tgtype = 7
        AND trigger_row.tgenabled = 'O'
        AND trigger_row.tgqual IS NULL
        AND pg_catalog.cardinality(trigger_row.tgattr) = 0
        AND trigger_row.tgnargs = 0
    ) <> 2
    OR NOT EXISTS (
      SELECT 1 FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname =
          'faa_registry_aircraft_references_reachable'
        AND trigger_row.tgrelid = pg_catalog.to_regclass(
          'public.faa_registry_aircraft_references'
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_reference_reachability()'
        )
    )
    OR NOT EXISTS (
      SELECT 1 FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname =
          'faa_registry_engine_references_reachable'
        AND trigger_row.tgrelid = pg_catalog.to_regclass(
          'public.faa_registry_engine_references'
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_reference_reachability()'
        )
    )
    ) THEN
      RAISE EXCEPTION
        'pre-migration FAA reference reachability objects have an unexpected shape';
    END IF;
  ELSE
    IF (
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
        AND trigger_row.tgtype = 7
        AND trigger_row.tgenabled = 'O'
        AND trigger_row.tgqual IS NULL
        AND pg_catalog.cardinality(trigger_row.tgattr) = 0
        AND trigger_row.tgnargs = 0
    ) <> 2
    OR NOT EXISTS (
      SELECT 1 FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname =
          'faa_registry_aircraft_references_reachable'
        AND trigger_row.tgrelid = pg_catalog.to_regclass(
          'public.faa_registry_aircraft_references'
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_aircraft_reference_reachability()'
        )
    )
    OR NOT EXISTS (
      SELECT 1 FROM pg_catalog.pg_trigger trigger_row
      WHERE NOT trigger_row.tgisinternal
        AND trigger_row.tgname =
          'faa_registry_engine_references_reachable'
        AND trigger_row.tgrelid = pg_catalog.to_regclass(
          'public.faa_registry_engine_references'
        )
        AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
          'public.validate_faa_engine_reference_reachability()'
        )
    )
    OR NOT EXISTS (
      SELECT 1
      FROM pg_catalog.pg_proc routine
      JOIN pg_catalog.pg_namespace routine_namespace
        ON routine_namespace.oid = routine.pronamespace
      JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
      WHERE routine.oid = pg_catalog.to_regprocedure(
        'public.validate_faa_aircraft_reference_reachability()'
      )
        AND routine_namespace.nspname = 'public'
        AND routine.prosrc = $aircraft_function$
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
$aircraft_function$
        AND routine.proconfig = ARRAY['search_path=pg_catalog']
        AND language.lanname = 'plpgsql'
        AND routine.prorettype = 'trigger'::pg_catalog.regtype
        AND routine.pronargs = 0
        AND routine.prokind = 'f'
        AND NOT routine.prosecdef
        AND NOT routine.proisstrict
        AND routine.provolatile = 'v'
        AND routine.proparallel = 'u'
    )
    OR NOT EXISTS (
      SELECT 1
      FROM pg_catalog.pg_proc routine
      JOIN pg_catalog.pg_namespace routine_namespace
        ON routine_namespace.oid = routine.pronamespace
      JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
      WHERE routine.oid = pg_catalog.to_regprocedure(
        'public.validate_faa_engine_reference_reachability()'
      )
        AND routine_namespace.nspname = 'public'
        AND routine.prosrc = $engine_function$
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
$engine_function$
        AND routine.proconfig = ARRAY['search_path=pg_catalog']
        AND language.lanname = 'plpgsql'
        AND routine.prorettype = 'trigger'::pg_catalog.regtype
        AND routine.pronargs = 0
        AND routine.prokind = 'f'
        AND NOT routine.prosecdef
        AND NOT routine.proisstrict
        AND routine.provolatile = 'v'
        AND routine.proparallel = 'u'
    ) THEN
      RAISE EXCEPTION
        'installed FAA reference reachability objects have an unexpected shape';
    END IF;
  END IF;

  IF EXISTS (
    SELECT 1
    FROM public.faa_registry_aircraft_references reference
    WHERE NOT EXISTS (
      SELECT 1 FROM public.faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = reference.snapshot_id
        AND aircraft.aircraft_code = reference.aircraft_code
    )
  ) OR EXISTS (
    SELECT 1
    FROM public.faa_registry_engine_references reference
    WHERE NOT EXISTS (
      SELECT 1 FROM public.faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = reference.snapshot_id
        AND aircraft.engine_code = reference.engine_code
    )
  ) THEN
    RAISE EXCEPTION
      'existing FAA reference rows are unreachable from target matches';
  END IF;
END
$migration_guard$;

DROP TRIGGER IF EXISTS faa_registry_snapshots_require_exact_evidence
  ON public.faa_registry_snapshots;
CREATE OR REPLACE FUNCTION public.validate_faa_snapshot_evidence()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM public.curation_evidence_sources source
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
$function$
SET search_path = pg_catalog;
CREATE TRIGGER faa_registry_snapshots_require_exact_evidence
BEFORE INSERT ON public.faa_registry_snapshots
FOR EACH ROW
EXECUTE FUNCTION public.validate_faa_snapshot_evidence();

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

DROP TRIGGER IF EXISTS faa_registry_coverage_consistent
  ON public.faa_registry_coverage;
CREATE OR REPLACE FUNCTION public.validate_faa_coverage()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF (NEW.lookup_status = 'matched' AND NOT EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) OR (NEW.lookup_status = 'absent' AND EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) THEN
    RAISE EXCEPTION 'FAA coverage must agree with its target match';
  END IF;
  RETURN NEW;
END;
$function$
SET search_path = pg_catalog;
CREATE TRIGGER faa_registry_coverage_consistent
BEFORE INSERT ON public.faa_registry_coverage
FOR EACH ROW
EXECUTE FUNCTION public.validate_faa_coverage();

DROP TRIGGER IF EXISTS faa_registry_snapshots_immutable
  ON public.faa_registry_snapshots;
DROP TRIGGER IF EXISTS faa_registry_aircraft_immutable
  ON public.faa_registry_aircraft;
DROP TRIGGER IF EXISTS faa_registry_aircraft_references_immutable
  ON public.faa_registry_aircraft_references;
DROP TRIGGER IF EXISTS faa_registry_engine_references_immutable
  ON public.faa_registry_engine_references;
DROP TRIGGER IF EXISTS faa_registry_coverage_immutable
  ON public.faa_registry_coverage;
CREATE OR REPLACE FUNCTION public.preserve_faa_registry_data()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  RAISE EXCEPTION 'FAA registry snapshots and projections are immutable';
END;
$function$
SET search_path = pg_catalog;
CREATE TRIGGER faa_registry_snapshots_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_snapshots
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();
CREATE TRIGGER faa_registry_aircraft_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_aircraft
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();
CREATE TRIGGER faa_registry_aircraft_references_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_aircraft_references
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();
CREATE TRIGGER faa_registry_engine_references_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_engine_references
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();
CREATE TRIGGER faa_registry_coverage_immutable
BEFORE UPDATE OR DELETE ON public.faa_registry_coverage
FOR EACH ROW EXECUTE FUNCTION public.preserve_faa_registry_data();

DO $post_provenance_guard$
DECLARE
  trigger_matches BIGINT;
  trigger_name_count BIGINT;
  function_matches BIGINT;
BEGIN
  WITH expected(
    trigger_name, relation_name, function_name, trigger_type
  ) AS (
    VALUES
      ('faa_registry_snapshots_require_exact_evidence',
       'faa_registry_snapshots', 'validate_faa_snapshot_evidence', 7),
      ('faa_registry_coverage_consistent',
       'faa_registry_coverage', 'validate_faa_coverage', 7),
      ('faa_registry_snapshots_immutable',
       'faa_registry_snapshots', 'preserve_faa_registry_data', 27),
      ('faa_registry_aircraft_immutable',
       'faa_registry_aircraft', 'preserve_faa_registry_data', 27),
      ('faa_registry_aircraft_references_immutable',
       'faa_registry_aircraft_references', 'preserve_faa_registry_data', 27),
      ('faa_registry_engine_references_immutable',
       'faa_registry_engine_references', 'preserve_faa_registry_data', 27),
      ('faa_registry_coverage_immutable',
       'faa_registry_coverage', 'preserve_faa_registry_data', 27)
  )
  SELECT COUNT(*) INTO trigger_matches
  FROM expected
  JOIN pg_catalog.pg_trigger trigger_row
    ON trigger_row.tgname = expected.trigger_name
   AND NOT trigger_row.tgisinternal
  JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
  JOIN pg_catalog.pg_namespace relation_namespace
    ON relation_namespace.oid = relation.relnamespace
  JOIN pg_catalog.pg_proc routine ON routine.oid = trigger_row.tgfoid
  JOIN pg_catalog.pg_namespace routine_namespace
    ON routine_namespace.oid = routine.pronamespace
  WHERE relation_namespace.nspname = 'public'
    AND relation.relname = expected.relation_name
    AND routine_namespace.nspname = 'public'
    AND routine.proname = expected.function_name
    AND routine.pronargs = 0
    AND trigger_row.tgtype = expected.trigger_type
    AND trigger_row.tgenabled = 'O'
    AND trigger_row.tgqual IS NULL
    AND pg_catalog.cardinality(trigger_row.tgattr) = 0
    AND trigger_row.tgnargs = 0;

  SELECT COUNT(*) INTO trigger_name_count
  FROM pg_catalog.pg_trigger trigger_row
  WHERE NOT trigger_row.tgisinternal
    AND trigger_row.tgname IN (
      'faa_registry_snapshots_require_exact_evidence',
      'faa_registry_coverage_consistent',
      'faa_registry_snapshots_immutable',
      'faa_registry_aircraft_immutable',
      'faa_registry_aircraft_references_immutable',
      'faa_registry_engine_references_immutable',
      'faa_registry_coverage_immutable'
    );

  WITH expected(function_name, function_source) AS (
    VALUES
      ('validate_faa_snapshot_evidence', $snapshot_function$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM public.curation_evidence_sources source
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
$snapshot_function$),
      ('validate_faa_coverage', $coverage_function$
BEGIN
  IF (NEW.lookup_status = 'matched' AND NOT EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) OR (NEW.lookup_status = 'absent' AND EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) THEN
    RAISE EXCEPTION 'FAA coverage must agree with its target match';
  END IF;
  RETURN NEW;
END;
$coverage_function$),
      ('preserve_faa_registry_data', $immutability_function$
BEGIN
  RAISE EXCEPTION 'FAA registry snapshots and projections are immutable';
END;
$immutability_function$)
  )
  SELECT COUNT(*) INTO function_matches
  FROM expected
  JOIN pg_catalog.pg_proc routine ON routine.proname = expected.function_name
  JOIN pg_catalog.pg_namespace routine_namespace
    ON routine_namespace.oid = routine.pronamespace
  JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
  WHERE routine_namespace.nspname = 'public'
    AND routine.prosrc = expected.function_source
    AND routine.proconfig = ARRAY['search_path=pg_catalog']
    AND language.lanname = 'plpgsql'
    AND routine.prorettype = 'trigger'::pg_catalog.regtype
    AND routine.pronargs = 0
    AND routine.prokind = 'f'
    AND NOT routine.prosecdef
    AND NOT routine.proisstrict
    AND routine.provolatile = 'v'
    AND routine.proparallel = 'u';

  IF trigger_matches <> 7 OR trigger_name_count <> 7 OR function_matches <> 3
  THEN
    RAISE EXCEPTION
      'post-migration FAA provenance objects have an unexpected shape';
  END IF;
END
$post_provenance_guard$;

DO $post_migration_guard$
BEGIN
  IF (
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
      AND trigger_row.tgtype = 7
      AND trigger_row.tgenabled = 'O'
      AND trigger_row.tgqual IS NULL
      AND pg_catalog.cardinality(trigger_row.tgattr) = 0
      AND trigger_row.tgnargs = 0
  ) <> 2
  OR NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger trigger_row
    WHERE trigger_row.tgname = 'faa_registry_aircraft_references_reachable'
      AND trigger_row.tgrelid = pg_catalog.to_regclass(
        'public.faa_registry_aircraft_references'
      )
      AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
        'public.validate_faa_aircraft_reference_reachability()'
      )
  )
  OR NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger trigger_row
    WHERE trigger_row.tgname = 'faa_registry_engine_references_reachable'
      AND trigger_row.tgrelid = pg_catalog.to_regclass(
        'public.faa_registry_engine_references'
      )
      AND trigger_row.tgfoid = pg_catalog.to_regprocedure(
        'public.validate_faa_engine_reference_reachability()'
      )
  )
  OR NOT EXISTS (
    SELECT 1
    FROM pg_catalog.pg_proc routine
    JOIN pg_catalog.pg_namespace routine_namespace
      ON routine_namespace.oid = routine.pronamespace
    JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
    WHERE routine.oid = pg_catalog.to_regprocedure(
      'public.validate_faa_aircraft_reference_reachability()'
    )
      AND routine_namespace.nspname = 'public'
      AND routine.prosrc = $aircraft_function$
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
$aircraft_function$
      AND routine.proconfig = ARRAY['search_path=pg_catalog']
      AND language.lanname = 'plpgsql'
      AND routine.prorettype = 'trigger'::pg_catalog.regtype
      AND routine.pronargs = 0
      AND routine.prokind = 'f'
      AND NOT routine.prosecdef
      AND NOT routine.proisstrict
      AND routine.provolatile = 'v'
      AND routine.proparallel = 'u'
  )
  OR NOT EXISTS (
    SELECT 1
    FROM pg_catalog.pg_proc routine
    JOIN pg_catalog.pg_namespace routine_namespace
      ON routine_namespace.oid = routine.pronamespace
    JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
    WHERE routine.oid = pg_catalog.to_regprocedure(
      'public.validate_faa_engine_reference_reachability()'
    )
      AND routine_namespace.nspname = 'public'
      AND routine.prosrc = $engine_function$
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
$engine_function$
      AND routine.proconfig = ARRAY['search_path=pg_catalog']
      AND language.lanname = 'plpgsql'
      AND routine.prorettype = 'trigger'::pg_catalog.regtype
      AND routine.pronargs = 0
      AND routine.prokind = 'f'
      AND NOT routine.prosecdef
      AND NOT routine.proisstrict
      AND routine.provolatile = 'v'
      AND routine.proparallel = 'u'
  )
  OR EXISTS (
    SELECT 1
    FROM public.faa_registry_aircraft_references reference
    WHERE NOT EXISTS (
      SELECT 1 FROM public.faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = reference.snapshot_id
        AND aircraft.aircraft_code = reference.aircraft_code
    )
  )
  OR EXISTS (
    SELECT 1
    FROM public.faa_registry_engine_references reference
    WHERE NOT EXISTS (
      SELECT 1 FROM public.faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = reference.snapshot_id
        AND aircraft.engine_code = reference.engine_code
    )
  ) THEN
    RAISE EXCEPTION
      'post-migration FAA reference reachability objects have an unexpected shape';
  END IF;
END
$post_migration_guard$;

INSERT INTO public.schema_migration_contracts AS installed_contract (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_faa_reference_reachability',
  1,
  'fc6451ffe8e1ee2034e76480767d16d6c37463461d9e684687448b4d43f96bef',
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
