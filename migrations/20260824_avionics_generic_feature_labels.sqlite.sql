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

DROP TABLE IF EXISTS temp.avionics_generic_feature_labels_migration_guard;
CREATE TEMP TABLE avionics_generic_feature_labels_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_generic_feature_labels_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260824_avionics_generic_feature_labels'
  )
  OR EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260824_avionics_generic_feature_labels'
      AND contract_version = 1
      AND contract_fingerprint =
        '4df9f28d6d4ef22245cf1fe0dd4124573a1a03ef36371aba4cbd9d485bc94163'
  ) THEN 1
  ELSE 0
END;
DROP TABLE avionics_generic_feature_labels_migration_guard;

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
  'flight director', 'transponder', 'ads b', 'ads b in', 'ads b out',
  'ads b in out', 'ads b in and out', 'weather radar', 'audio panel',
  'standard audio panel', 'audio controller', 'audio control panel',
  'display', 'flight display', 'pfd', 'mfd', 'pfd mfd', 'navigation indicator',
  'traffic', 'active traffic', 'traffic advisory system', 'datalink',
  'datalink weather', 'xm', 'xm weather', 'xm radio', 'xm weather radio',
  'lightning detection', 'terrain awareness', 'terrain awareness system',
  'terrain avoidance system', 'taws', 'synthetic vision',
  'synthetic vision system', 'svt', 'safetaxi', 'safe taxi', 'flitecharts',
  'flite charts', 'charts', 'electronic charts',
  'electronic stability and protection', 'electronic stability protection',
  'stability and protection', 'wireless data loading',
  'wireless database loading', 'engine monitor', 'engine fuel monitoring',
  'standby instrument', 'backup instruments', 'elt', 'adf', 'dme', 'ahrs',
  'air data computer', 'radar altimeter', 'magnetometer', 'clock timer', 'waas',
  'waas gps', 'dual waas', 'remote transponder', 'transponder ads b',
  'stormscope', 'standard radio navigation', 'equipment'
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
  'flight director', 'transponder', 'ads b', 'ads b in', 'ads b out',
  'ads b in out', 'ads b in and out', 'weather radar', 'audio panel',
  'standard audio panel', 'audio controller', 'audio control panel',
  'display', 'flight display', 'pfd', 'mfd', 'pfd mfd', 'navigation indicator',
  'traffic', 'active traffic', 'traffic advisory system', 'datalink',
  'datalink weather', 'xm', 'xm weather', 'xm radio', 'xm weather radio',
  'lightning detection', 'terrain awareness', 'terrain awareness system',
  'terrain avoidance system', 'taws', 'synthetic vision',
  'synthetic vision system', 'svt', 'safetaxi', 'safe taxi', 'flitecharts',
  'flite charts', 'charts', 'electronic charts',
  'electronic stability and protection', 'electronic stability protection',
  'stability and protection', 'wireless data loading',
  'wireless database loading', 'engine monitor', 'engine fuel monitoring',
  'standby instrument', 'backup instruments', 'elt', 'adf', 'dme', 'ahrs',
  'air data computer', 'radar altimeter', 'magnetometer', 'clock timer', 'waas',
  'waas gps', 'dual waas', 'remote transponder', 'transponder ads b',
  'stormscope', 'standard radio navigation', 'equipment'
  )
 )
BEGIN
  SELECT RAISE(ABORT, 'approved avionics normalized_name must be canonical and concrete; canonicalize, correct, or demote it before retrying migration');
END;

-- Audit through the new invariant. Existing feature-only approved rows must be
-- corrected or demoted explicitly before this migration can be installed.
UPDATE avionics_models
SET normalized_name = normalized_name
WHERE catalog_status = 'approved'
  AND normalized_name IN (
    'ads b in', 'ads b out', 'ads b in out', 'ads b in and out',
    'standard audio panel', 'audio controller', 'audio control panel',
    'datalink weather', 'synthetic vision', 'synthetic vision system', 'svt',
    'safetaxi', 'safe taxi', 'flitecharts', 'flite charts', 'charts',
    'electronic charts', 'electronic stability and protection',
    'electronic stability protection', 'stability and protection',
    'wireless data loading', 'wireless database loading',
    'engine fuel monitoring', 'backup instruments', 'waas', 'waas gps',
    'dual waas', 'remote transponder', 'transponder ads b', 'stormscope',
    'standard radio navigation'
  );

INSERT INTO schema_migration_contracts (
  migration_name,
  contract_version,
  contract_fingerprint,
  installed_at
) VALUES (
  '20260824_avionics_generic_feature_labels',
  1,
  '4df9f28d6d4ef22245cf1fe0dd4124573a1a03ef36371aba4cbd9d485bc94163',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
PRAGMA foreign_keys = ON;
