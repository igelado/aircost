CREATE TABLE schema_migration_history (
  sequence_number INTEGER PRIMARY KEY
    CHECK (typeof(sequence_number) = 'integer' AND sequence_number > 0),
  migration_name TEXT NOT NULL UNIQUE
    CHECK (length(trim(migration_name)) > 0),
  backend_applicability TEXT NOT NULL
    CHECK (backend_applicability IN ('both', 'postgres_only')),
  script_sha256 TEXT NOT NULL
    CHECK (
      length(script_sha256) = 64
      AND script_sha256 = lower(script_sha256)
      AND script_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
  provenance TEXT NOT NULL
    CHECK (provenance IN (
      'applied', 'canonical', 'shape_attested_legacy_adoption'
    )),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    CHECK (julianday(installed_at) IS NOT NULL)
);

CREATE TRIGGER schema_migration_history_immutable_insert_conflict
BEFORE INSERT ON schema_migration_history
WHEN EXISTS (
  SELECT 1 FROM schema_migration_history
  WHERE sequence_number = NEW.sequence_number
     OR migration_name = NEW.migration_name
)
BEGIN
  SELECT RAISE(ABORT, 'schema migration history is immutable');
END;

CREATE TRIGGER schema_migration_history_immutable_update
BEFORE UPDATE ON schema_migration_history
BEGIN
  SELECT RAISE(ABORT, 'schema migration history is immutable');
END;

CREATE TRIGGER schema_migration_history_immutable_delete
BEFORE DELETE ON schema_migration_history
BEGIN
  SELECT RAISE(ABORT, 'schema migration history is immutable');
END;

-- This separately attested marker makes deletion of the history table
-- distinguishable from a database that genuinely predates ordered history.
INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260910_versioned_migration_history', 1,
  '399f98f5fccc7696423622617a673161fdb2d86ac1fb4a4f776060476baf347f',
  CURRENT_TIMESTAMP
);
