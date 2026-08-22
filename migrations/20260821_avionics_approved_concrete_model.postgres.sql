BEGIN;

SET LOCAL search_path = public, pg_catalog, pg_temp;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

LOCK TABLE ONLY public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM ONLY public.schema_migration_contracts
    WHERE migration_name = '20260821_avionics_approved_concrete_model'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          '1305564519a99b0ecdfb85a045b9924bf90a33b2914bb6822a219170d541a5f6'
      )
  ) THEN
    RAISE EXCEPTION
      'installed avionics approved concrete model migration has a different contract';
  END IF;
END
$migration_guard$;

CREATE OR REPLACE FUNCTION public.enforce_avionics_approved_concrete_model()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  IF NEW.catalog_status = 'approved' AND (
    NEW.normalized_name <> LOWER(BTRIM(NEW.normalized_name))
    OR NEW.normalized_name !~ '^[a-z0-9]+( [a-z0-9]+)*$'
  ) THEN
    RAISE EXCEPTION 'approved avionics normalized_name is not canonical; canonicalize, correct, or demote it before retrying migration';
  END IF;
  IF NEW.catalog_status = 'approved' AND NEW.normalized_name IN (
    '', 'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
    'avionics', 'avionics suite', 'integrated avionics', 'integrated avionics suite',
    'glass panel', 'flight instruments', 'standard flight instruments',
    'standard vfr avionics', 'standard ifr avionics', 'radio', 'radios', 'nav',
    'com', 'nav com', 'gps nav com', 'navigation system', 'gps', 'autopilot',
    'flight director', 'transponder', 'ads b', 'weather radar', 'audio panel',
    'display', 'flight display', 'pfd', 'mfd', 'pfd mfd', 'navigation indicator',
    'traffic', 'active traffic', 'traffic advisory system', 'datalink', 'xm',
    'xm weather', 'xm radio', 'xm weather radio', 'lightning detection',
    'terrain awareness', 'terrain awareness system', 'terrain avoidance system',
    'taws', 'engine monitor', 'standby instrument', 'elt', 'adf', 'dme', 'ahrs',
    'air data computer', 'radar altimeter', 'magnetometer', 'clock timer', 'equipment'
  ) THEN
    RAISE EXCEPTION 'approved avionics model is a generic category; canonicalize, correct, or demote it before retrying migration';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS avionics_models_approved_concrete_model
  ON public.avionics_models;
CREATE TRIGGER avionics_models_approved_concrete_model
BEFORE INSERT OR UPDATE OF catalog_status, normalized_name
ON public.avionics_models
FOR EACH ROW EXECUTE FUNCTION public.enforce_avionics_approved_concrete_model();

-- Audit through the newly installed invariant. PostgreSQL rolls the function,
-- trigger, and marker back together if any approved row needs correction.
UPDATE public.avionics_models
SET normalized_name = normalized_name
WHERE catalog_status = 'approved'
  AND (
    normalized_name <> LOWER(BTRIM(normalized_name))
    OR normalized_name !~ '^[a-z0-9]+( [a-z0-9]+)*$'
    OR normalized_name IN (
    '', 'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
    'avionics', 'avionics suite', 'integrated avionics', 'integrated avionics suite',
    'glass panel', 'flight instruments', 'standard flight instruments',
    'standard vfr avionics', 'standard ifr avionics', 'radio', 'radios', 'nav',
    'com', 'nav com', 'gps nav com', 'navigation system', 'gps', 'autopilot',
    'flight director', 'transponder', 'ads b', 'weather radar', 'audio panel',
    'display', 'flight display', 'pfd', 'mfd', 'pfd mfd', 'navigation indicator',
    'traffic', 'active traffic', 'traffic advisory system', 'datalink', 'xm',
    'xm weather', 'xm radio', 'xm weather radio', 'lightning detection',
    'terrain awareness', 'terrain awareness system', 'terrain avoidance system',
    'taws', 'engine monitor', 'standby instrument', 'elt', 'adf', 'dme', 'ahrs',
    'air data computer', 'radar altimeter', 'magnetometer', 'clock timer', 'equipment'
    )
  );

INSERT INTO public.schema_migration_contracts (
  migration_name,
  contract_version,
  contract_fingerprint,
  installed_at
) VALUES (
  '20260821_avionics_approved_concrete_model',
  1,
  '1305564519a99b0ecdfb85a045b9924bf90a33b2914bb6822a219170d541a5f6',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
