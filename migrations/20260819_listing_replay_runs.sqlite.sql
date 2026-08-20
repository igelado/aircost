-- Add resumable coordination for trusted-capture listing replay.

BEGIN IMMEDIATE;

CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL,
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

DROP TABLE IF EXISTS temp.listing_replay_runs_migration_guard;
CREATE TEMP TABLE listing_replay_runs_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO listing_replay_runs_migration_guard (accepted)
SELECT CASE
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260819_listing_replay_runs'
      AND contract_version = 1
      AND contract_fingerprint =
        '3a42bf7af89b641ec62cedf520dadc665be97acbc440bb73f438406c2d3e911d'
  ) AND EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'table' AND name = 'listing_replay_runs'
  ) AND EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'table' AND name = 'listing_replay_run_items'
  ) AND EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'index' AND name = 'idx_listing_replay_runs_one_running'
  ) AND EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'index' AND name = 'idx_listing_replay_run_items_phase'
  ) THEN 1
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260819_listing_replay_runs'
  ) AND NOT EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE name IN (
      'listing_replay_runs', 'listing_replay_run_items',
      'idx_listing_replay_runs_one_running', 'idx_listing_replay_run_items_phase'
    )
  ) THEN 1 ELSE 0 END;
DROP TABLE listing_replay_runs_migration_guard;

CREATE TABLE IF NOT EXISTS listing_replay_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manifest_version INTEGER NOT NULL CHECK (manifest_version > 0),
  manifest_sha256 TEXT NOT NULL UNIQUE,
  manifest_capture_count INTEGER NOT NULL CHECK (manifest_capture_count > 0),
  status TEXT NOT NULL DEFAULT 'queued'
    CHECK (status IN ('queued', 'running', 'completed')),
  active_phase TEXT CHECK (active_phase IN ('extraction', 'materialization')),
  owner_token TEXT,
  heartbeat_at_epoch_seconds INTEGER,
  started_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (length(manifest_sha256) = 64),
  CHECK (manifest_sha256 = lower(manifest_sha256)),
  CHECK (manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (owner_token IS NULL OR length(trim(owner_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = 'running' AND active_phase IS NOT NULL AND owner_token IS NOT NULL
      AND heartbeat_at_epoch_seconds IS NOT NULL AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status = 'queued' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NULL)
    OR
    (status = 'completed' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NOT NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_listing_replay_runs_one_running
  ON listing_replay_runs (status) WHERE status = 'running';

CREATE TABLE IF NOT EXISTS listing_replay_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES listing_replay_runs(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL CHECK (position >= 0),
  expected_rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT,
  extraction_state TEXT NOT NULL DEFAULT 'queued'
    CHECK (extraction_state IN ('queued', 'running', 'succeeded', 'rejected', 'failed')),
  materialization_state TEXT NOT NULL DEFAULT 'blocked'
    CHECK (materialization_state IN ('blocked', 'queued', 'running', 'succeeded', 'rejected', 'failed')),
  resulting_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL,
  terminal_rejection_phase TEXT
    CHECK (terminal_rejection_phase IN ('extraction', 'materialization')),
  terminal_rejection_stage TEXT CHECK (terminal_rejection_stage IN (
    'capture_admission', 'faa_aircraft_admission'
  )),
  terminal_rejection_reason_code TEXT CHECK (terminal_rejection_reason_code IN (
    'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed',
    'missing_registration', 'non_n_registration',
    'invalid_n_number', 'serial_conflict'
  )),
  last_failure_phase TEXT CHECK (last_failure_phase IN ('extraction', 'materialization')),
  last_failure_reason_code TEXT
    CHECK (last_failure_reason_code IN (
      'database_error', 'operation_failed', 'faa_lookup_failed', 'faa_listing_not_found',
      'faa_registry_snapshot_unavailable', 'faa_registration_not_found',
      'faa_registration_not_covered', 'faa_ambiguous_registration',
      'faa_registry_aircraft_identity_unavailable', 'faa_aircraft_manufacturer_mismatch',
      'faa_aircraft_model_mismatch', 'faa_canonical_identity_assignment_missing',
      'faa_canonical_identity_assignment_mismatch'
    )),
  extraction_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (extraction_attempt_count >= 0),
  materialization_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (materialization_attempt_count >= 0),
  extraction_started_at TEXT,
  extraction_completed_at TEXT,
  materialization_started_at TEXT,
  materialization_completed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (run_id, position),
  UNIQUE (run_id, plugin_submission_id),
  CHECK (length(expected_rendered_html_sha256) = 64),
  CHECK (expected_rendered_html_sha256 = lower(expected_rendered_html_sha256)),
  CHECK (expected_rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (extracted_listing_sha256 IS NULL OR (
    length(extracted_listing_sha256) = 64
    AND extracted_listing_sha256 = lower(extracted_listing_sha256)
    AND extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*'
  )),
  CHECK (
    (terminal_rejection_phase IS NULL AND terminal_rejection_stage IS NULL
      AND terminal_rejection_reason_code IS NULL)
    OR
    (terminal_rejection_phase IS NOT NULL AND terminal_rejection_stage IS NOT NULL
      AND terminal_rejection_reason_code IS NOT NULL)
  ),
  CHECK (
    (last_failure_phase IS NULL AND last_failure_reason_code IS NULL)
    OR (last_failure_phase IS NOT NULL AND last_failure_reason_code IS NOT NULL)
  ),
  CHECK (resulting_listing_id IS NULL OR materialization_state = 'succeeded'),
  CHECK ((extraction_state = 'succeeded') = (extracted_listing_sha256 IS NOT NULL)),
  CHECK (extraction_state = 'succeeded' OR materialization_state = 'blocked'),
  CHECK (extraction_state <> 'running' OR extraction_started_at IS NOT NULL),
  CHECK (materialization_state <> 'running' OR materialization_started_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_listing_replay_run_items_phase
  ON listing_replay_run_items (run_id, extraction_state, materialization_state, position);

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_listing_replay_runs', 1,
  '3a42bf7af89b641ec62cedf520dadc665be97acbc440bb73f438406c2d3e911d',
  CURRENT_TIMESTAMP
) ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = excluded.contract_version,
  contract_fingerprint = excluded.contract_fingerprint,
  installed_at = excluded.installed_at;

COMMIT;
