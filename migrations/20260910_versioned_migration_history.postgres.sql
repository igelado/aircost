CREATE TABLE public.schema_migration_history (
  sequence_number BIGINT PRIMARY KEY CHECK (sequence_number > 0),
  migration_name TEXT NOT NULL UNIQUE CHECK (length(trim(migration_name)) > 0),
  backend_applicability TEXT NOT NULL
    CHECK (backend_applicability IN ('both', 'postgres_only')),
  script_sha256 TEXT NOT NULL CHECK (script_sha256 ~ '^[0-9a-f]{64}$'),
  provenance TEXT NOT NULL
    CHECK (provenance IN (
      'applied', 'canonical', 'shape_attested_legacy_adoption'
    )),
  installed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE FUNCTION public.reject_schema_migration_history_mutation()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, public, pg_temp
AS $schema_migration_history_immutable$
BEGIN
  RAISE EXCEPTION 'schema migration history is immutable';
END;
$schema_migration_history_immutable$;

CREATE TRIGGER schema_migration_history_immutable_update
BEFORE UPDATE ON public.schema_migration_history
FOR EACH ROW
EXECUTE FUNCTION public.reject_schema_migration_history_mutation();

CREATE TRIGGER schema_migration_history_immutable_delete
BEFORE DELETE ON public.schema_migration_history
FOR EACH ROW
EXECUTE FUNCTION public.reject_schema_migration_history_mutation();

CREATE TRIGGER schema_migration_history_immutable_truncate
BEFORE TRUNCATE ON public.schema_migration_history
FOR EACH STATEMENT
EXECUTE FUNCTION public.reject_schema_migration_history_mutation();

-- This separately attested marker makes deletion of the history table
-- distinguishable from a database that genuinely predates ordered history.
INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260910_versioned_migration_history', 1,
  '399f98f5fccc7696423622617a673161fdb2d86ac1fb4a4f776060476baf347f',
  CURRENT_TIMESTAMP
);
