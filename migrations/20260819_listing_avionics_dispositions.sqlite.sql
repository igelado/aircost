PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics_dispositions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  extraction_sha256 TEXT NOT NULL,
  occurrence_index INTEGER NOT NULL CHECK (occurrence_index >= 0),
  occurrence_role TEXT NOT NULL CHECK (occurrence_role IN ('primary', 'replacement')),
  occurrence_fingerprint TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK (outcome IN ('linked', 'discarded')),
  avionics_model_id INTEGER REFERENCES avionics_models(id) ON DELETE RESTRICT,
  reason_code TEXT NOT NULL CHECK (length(trim(reason_code)) BETWEEN 1 AND 100),
  decision_reason TEXT NOT NULL CHECK (length(trim(decision_reason)) BETWEEN 1 AND 500),
  decision_source TEXT NOT NULL CHECK (decision_source IN ('automatic', 'manual')),
  actor_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  policy_version TEXT NOT NULL CHECK (length(trim(policy_version)) BETWEEN 1 AND 100),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (plugin_submission_id, extraction_sha256, occurrence_index, occurrence_role),
  CHECK (length(extraction_sha256) = 64
    AND extraction_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(occurrence_fingerprint) = 64
    AND occurrence_fingerprint NOT GLOB '*[^0-9a-f]*'),
  CHECK (
    (outcome = 'linked' AND avionics_model_id IS NOT NULL)
    OR (outcome = 'discarded' AND avionics_model_id IS NULL)
  )
);

CREATE INDEX IF NOT EXISTS idx_listing_avionics_dispositions_listing
  ON aircraft_sale_listing_avionics_dispositions (aircraft_sale_listing_id, occurrence_index);

CREATE TRIGGER IF NOT EXISTS trg_listing_avionics_dispositions_immutable
BEFORE UPDATE ON aircraft_sale_listing_avionics_dispositions
BEGIN
  SELECT RAISE(ABORT, 'avionics occurrence dispositions are immutable');
END;
