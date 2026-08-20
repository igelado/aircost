-- Reconstruct the exact historical run-item and applicability contracts in a
-- disposable current-schema database for cutover upgrade tests.
PRAGMA foreign_keys = ON;

ALTER TABLE listing_verification_run_items
  RENAME TO reference_catalog_current_run_items;
CREATE TABLE listing_verification_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES listing_verification_runs(id) ON DELETE CASCADE,
  listing_id INTEGER NOT NULL REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN (
    'queued', 'running', 'verified', 'pending_review',
    'pending_reference', 'blocked', 'failed', 'cancelled'
  )),
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  lease_token TEXT,
  lease_expires_at_epoch_seconds INTEGER,
  outcome_json TEXT,
  reason_code TEXT,
  reason TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  started_at TEXT,
  completed_at TEXT,
  UNIQUE (run_id, position),
  UNIQUE (run_id, listing_id),
  CHECK (lease_token IS NULL OR length(trim(lease_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = 'running' AND lease_token IS NOT NULL
      AND lease_expires_at_epoch_seconds IS NOT NULL
      AND started_at IS NOT NULL AND completed_at IS NULL)
    OR
    (status <> 'running' AND lease_token IS NULL
      AND lease_expires_at_epoch_seconds IS NULL)
  ),
  CHECK (
    (status IN ('queued', 'running') AND completed_at IS NULL)
    OR
    (status IN ('verified', 'pending_review', 'pending_reference',
      'blocked', 'failed', 'cancelled') AND completed_at IS NOT NULL)
  ),
  CHECK (outcome_json IS NULL OR (
    length(outcome_json) BETWEEN 2 AND 65536
    AND json_valid(outcome_json) AND json_type(outcome_json) = 'object'
  )),
  CHECK (status NOT IN (
    'verified', 'pending_review', 'pending_reference', 'blocked'
  ) OR outcome_json IS NOT NULL),
  CHECK (reason_code IS NULL OR length(trim(reason_code)) BETWEEN 1 AND 100),
  CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 2000)
);
INSERT INTO listing_verification_run_items
SELECT * FROM reference_catalog_current_run_items;
DROP TABLE reference_catalog_current_run_items;
CREATE UNIQUE INDEX idx_listing_verification_run_items_one_active_listing
  ON listing_verification_run_items (listing_id)
  WHERE status IN ('queued', 'running');
CREATE UNIQUE INDEX idx_listing_verification_run_items_one_running_per_run
  ON listing_verification_run_items (run_id)
  WHERE status = 'running';
CREATE INDEX idx_listing_verification_run_items_claim
  ON listing_verification_run_items (run_id, status, position, id);

DELETE FROM schema_migration_contracts
WHERE migration_name = '20260819_reference_catalog_cutover';
