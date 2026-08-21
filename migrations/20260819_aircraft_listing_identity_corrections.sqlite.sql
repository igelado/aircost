PRAGMA foreign_keys = ON;
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

DROP TABLE IF EXISTS temp.aircraft_listing_identity_corrections_migration_guard;
CREATE TEMP TABLE aircraft_listing_identity_corrections_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO aircraft_listing_identity_corrections_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260819_aircraft_listing_identity_corrections'
  ) AND NOT EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'table'
      AND name = 'aircraft_listing_identity_correction_decisions'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260819_aircraft_listing_identity_corrections'
      AND contract_version = 1
      AND contract_fingerprint =
        '589a0716726d2ffd34bf84c08583198383c003228b769c88f094ac6bd9f677b8'
  ) THEN 1
  ELSE 0
END;
DROP TABLE aircraft_listing_identity_corrections_migration_guard;

DROP INDEX IF EXISTS uq_plugin_submissions_signed_capture;
CREATE UNIQUE INDEX uq_plugin_submissions_signed_capture
  ON plugin_submissions (
    user_id, plugin_install_id, source_url, rendered_html_sha256
  );

CREATE TABLE IF NOT EXISTS aircraft_listing_identity_correction_decisions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  observation_id INTEGER NOT NULL
    REFERENCES aircraft_identity_observations(id) ON DELETE RESTRICT,
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  correction_kind TEXT NOT NULL CHECK (correction_kind IN (
    'visual_identifier', 'faa_serial', 'publisher_hierarchy'
  )),
  expected_state_sha256 TEXT NOT NULL,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL,
  prior_registration_number TEXT,
  prior_serial_number TEXT,
  corrected_registration_number TEXT,
  corrected_serial_number TEXT,
  faa_registry_snapshot_id INTEGER
    REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  faa_source_record_sha256 TEXT,
  visual_resolution_json TEXT,
  decision_payload_json TEXT NOT NULL,
  decided_by_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  decided_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(expected_state_sha256) = 64 AND expected_state_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(rendered_html_sha256) = 64 AND rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (
    (correction_kind = 'visual_identifier'
      AND corrected_registration_number IS NOT NULL
      AND faa_registry_snapshot_id IS NOT NULL
      AND faa_source_record_sha256 IS NOT NULL
      AND visual_resolution_json IS NOT NULL)
    OR
    (correction_kind = 'faa_serial'
      AND corrected_registration_number IS NOT NULL
      AND corrected_serial_number IS NOT NULL
      AND faa_registry_snapshot_id IS NOT NULL
      AND faa_source_record_sha256 IS NOT NULL
      AND visual_resolution_json IS NULL)
    OR
    (correction_kind = 'publisher_hierarchy'
      AND faa_registry_snapshot_id IS NULL
      AND faa_source_record_sha256 IS NULL
      AND visual_resolution_json IS NULL
      AND corrected_registration_number IS prior_registration_number
      AND corrected_serial_number IS prior_serial_number)
  )
);

DROP INDEX IF EXISTS idx_aircraft_listing_identity_corrections_listing;
CREATE INDEX idx_aircraft_listing_identity_corrections_listing
  ON aircraft_listing_identity_correction_decisions (
    aircraft_sale_listing_id, correction_kind, id
  );
DROP INDEX IF EXISTS uq_aircraft_listing_identity_correction_receipt;
CREATE UNIQUE INDEX uq_aircraft_listing_identity_correction_receipt
  ON aircraft_listing_identity_correction_decisions (
    plugin_submission_id, correction_kind
  );

DROP TRIGGER IF EXISTS aircraft_listing_identity_corrections_immutable_update;
CREATE TRIGGER aircraft_listing_identity_corrections_immutable_update
BEFORE UPDATE ON aircraft_listing_identity_correction_decisions
BEGIN SELECT RAISE(ABORT, 'aircraft listing identity correction decisions are immutable'); END;
DROP TRIGGER IF EXISTS aircraft_listing_identity_corrections_immutable_delete;
CREATE TRIGGER aircraft_listing_identity_corrections_immutable_delete
BEFORE DELETE ON aircraft_listing_identity_correction_decisions
BEGIN SELECT RAISE(ABORT, 'aircraft listing identity correction decisions are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_identity_correction_observation_immutable_update;
CREATE TRIGGER aircraft_identity_correction_observation_immutable_update
BEFORE UPDATE ON aircraft_identity_observations
WHEN EXISTS (
  SELECT 1 FROM aircraft_listing_identity_correction_decisions decision
  WHERE decision.observation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'aircraft identity observations referenced by correction decisions are immutable'); END;
DROP TRIGGER IF EXISTS aircraft_identity_correction_observation_immutable_delete;
CREATE TRIGGER aircraft_identity_correction_observation_immutable_delete
BEFORE DELETE ON aircraft_identity_observations
WHEN EXISTS (
  SELECT 1 FROM aircraft_listing_identity_correction_decisions decision
  WHERE decision.observation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'aircraft identity observations referenced by correction decisions are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_source_identity_receipt_gate;
CREATE TRIGGER aircraft_source_identity_receipt_gate
BEFORE UPDATE OF ingestion_state, ingestion_error, is_verified
ON aircraft_sale_listings
WHEN OLD.ingestion_error = 'source_identity_correction_receipt_pending'
 AND (
   NEW.ingestion_error IS NOT OLD.ingestion_error
   OR NEW.ingestion_state IS NOT OLD.ingestion_state
   OR NEW.is_verified IS NOT OLD.is_verified
 )
 AND NOT EXISTS (
   SELECT 1
   FROM aircraft_listing_identity_correction_decisions decision
   JOIN plugin_submissions submission
     ON submission.id = decision.plugin_submission_id
   WHERE decision.aircraft_sale_listing_id = OLD.id
     AND decision.correction_kind = 'faa_serial'
     AND decision.rendered_html_sha256 = submission.rendered_html_sha256
     AND submission.user_id = OLD.created_by_user_id
     AND submission.canonical_listing_id = OLD.id
     AND submission.extraction_error IS NULL
     AND NEW.registration_number IS decision.corrected_registration_number
     AND NEW.serial_number IS decision.corrected_serial_number
 )
BEGIN SELECT RAISE(ABORT, 'source identity correction receipt is required before leaving the receipt gate'); END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_aircraft_listing_identity_corrections',
  1,
  '589a0716726d2ffd34bf84c08583198383c003228b769c88f094ac6bd9f677b8',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
