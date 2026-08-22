PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;

CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL,
  contract_fingerprint TEXT NOT NULL,
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(migration_name)) > 0),
  CHECK (typeof(contract_version) = 'integer' AND contract_version > 0),
  CHECK (length(contract_fingerprint) = 64),
  CHECK (contract_fingerprint = lower(contract_fingerprint)),
  CHECK (contract_fingerprint NOT GLOB '*[^0-9a-f]*')
);

DROP TABLE IF EXISTS temp.avionics_approved_concrete_model_migration_guard;
CREATE TEMP TABLE avionics_approved_concrete_model_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_approved_concrete_model_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260821_avionics_approved_concrete_model'
  )
  OR EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260821_avionics_approved_concrete_model'
      AND contract_version = 1
      AND contract_fingerprint =
        '1305564519a99b0ecdfb85a045b9924bf90a33b2914bb6822a219170d541a5f6'
  ) THEN 1
  ELSE 0
END;
DROP TABLE avionics_approved_concrete_model_migration_guard;

DROP TRIGGER IF EXISTS avionics_models_approved_concrete_model_insert;
CREATE TRIGGER avionics_models_approved_concrete_model_insert
BEFORE INSERT ON avionics_models
WHEN NEW.catalog_status = 'approved'
 AND (
  NEW.normalized_name <> lower(trim(NEW.normalized_name))
  OR NEW.normalized_name GLOB '*[^a-z0-9 ]*'
  OR instr(NEW.normalized_name, '  ') > 0
  OR NEW.normalized_name IN (
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
 )
BEGIN
  SELECT RAISE(ABORT, 'approved avionics normalized_name must be canonical and concrete; canonicalize, correct, or demote it before retrying migration');
END;

DROP TRIGGER IF EXISTS avionics_models_approved_concrete_model_update;
CREATE TRIGGER avionics_models_approved_concrete_model_update
BEFORE UPDATE OF catalog_status, normalized_name ON avionics_models
WHEN NEW.catalog_status = 'approved'
 AND (
  NEW.normalized_name <> lower(trim(NEW.normalized_name))
  OR NEW.normalized_name GLOB '*[^a-z0-9 ]*'
  OR instr(NEW.normalized_name, '  ') > 0
  OR NEW.normalized_name IN (
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
 )
BEGIN
  SELECT RAISE(ABORT, 'approved avionics normalized_name must be canonical and concrete; canonicalize, correct, or demote it before retrying migration');
END;

-- Audit through the newly installed invariant. The update is a no-op when the
-- database is clean and aborts the whole migration when correction is needed.
UPDATE avionics_models
SET normalized_name = normalized_name
WHERE catalog_status = 'approved'
  AND (
    normalized_name <> lower(trim(normalized_name))
    OR normalized_name GLOB '*[^a-z0-9 ]*'
    OR instr(normalized_name, '  ') > 0
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

INSERT INTO schema_migration_contracts (
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
PRAGMA foreign_keys = ON;
