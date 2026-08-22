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

DROP TABLE IF EXISTS temp.aircraft_visual_source_corrections_migration_guard;
CREATE TEMP TABLE aircraft_visual_source_corrections_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO aircraft_visual_source_corrections_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260821_aircraft_visual_source_corrections'
  ) AND NOT EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'table'
      AND name = 'aircraft_source_visual_correction_artifacts'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260821_aircraft_visual_source_corrections'
      AND contract_version = 1
      AND contract_fingerprint =
        'ccc63aa23f2579ec5cec682bf1493a13eb73829718936b5890bd84de51bb828a'
  ) THEN 1
  ELSE 0
END;
DROP TABLE aircraft_visual_source_corrections_migration_guard;

CREATE TABLE IF NOT EXISTS aircraft_source_visual_correction_artifacts (
  plugin_submission_id INTEGER PRIMARY KEY
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL,
  observed_registration_number TEXT NOT NULL,
  corrected_registration_number TEXT NOT NULL,
  corrected_serial_number TEXT,
  faa_registry_snapshot_id INTEGER NOT NULL
    REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  faa_snapshot_archive_sha256 TEXT NOT NULL,
  faa_source_record_sha256 TEXT NOT NULL,
  primary_photo_asset_id TEXT NOT NULL,
  primary_photo_url TEXT NOT NULL,
  primary_photo_sha256 TEXT NOT NULL,
  visual_resolution_sha256 TEXT NOT NULL,
  visual_resolution_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(rendered_html_sha256) = 64 AND rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(faa_snapshot_archive_sha256) = 64 AND faa_snapshot_archive_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(faa_source_record_sha256) = 64 AND faa_source_record_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(primary_photo_sha256) = 64 AND primary_photo_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(visual_resolution_sha256) = 64 AND visual_resolution_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (observed_registration_number <> corrected_registration_number),
  CHECK (length(observed_registration_number) BETWEEN 2 AND 6),
  CHECK (length(corrected_registration_number) BETWEEN 2 AND 6),
  CHECK (corrected_serial_number IS NULL OR length(corrected_serial_number) BETWEEN 1 AND 128),
  CHECK (length(primary_photo_asset_id) BETWEEN 1 AND 256),
  CHECK (length(primary_photo_url) BETWEEN 1 AND 4096),
  CHECK (length(visual_resolution_json) BETWEEN 2 AND 65536 AND json_valid(visual_resolution_json) AND json_type(visual_resolution_json) = 'object'),
  FOREIGN KEY (faa_registry_snapshot_id, observed_registration_number)
    REFERENCES faa_registry_coverage(snapshot_id, n_number) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, corrected_registration_number)
    REFERENCES faa_registry_aircraft(snapshot_id, n_number) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, faa_source_record_sha256)
    REFERENCES faa_registry_aircraft(snapshot_id, source_record_sha256) ON DELETE RESTRICT
);

DROP TRIGGER IF EXISTS aircraft_source_visual_artifacts_validate_insert;
CREATE TRIGGER aircraft_source_visual_artifacts_validate_insert
BEFORE INSERT ON aircraft_source_visual_correction_artifacts
WHEN NOT EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  JOIN faa_registry_snapshots snapshot ON snapshot.id = NEW.faa_registry_snapshot_id
  JOIN faa_registry_coverage observed
    ON observed.snapshot_id = snapshot.id
   AND observed.n_number = NEW.observed_registration_number
   AND observed.lookup_status = 'absent'
  JOIN faa_registry_coverage corrected
    ON corrected.snapshot_id = snapshot.id
   AND corrected.n_number = NEW.corrected_registration_number
   AND corrected.lookup_status = 'matched'
  JOIN faa_registry_aircraft aircraft
    ON aircraft.snapshot_id = snapshot.id
   AND aircraft.n_number = corrected.n_number
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.rendered_html_sha256 = NEW.rendered_html_sha256
    AND snapshot.id = (
      SELECT id FROM faa_registry_snapshots
      ORDER BY snapshot_date DESC, id DESC LIMIT 1
    )
    AND snapshot.archive_sha256 = NEW.faa_snapshot_archive_sha256
    AND aircraft.source_record_sha256 = NEW.faa_source_record_sha256
    AND aircraft.manufacturer_serial_raw IS NEW.corrected_serial_number
)
BEGIN SELECT RAISE(ABORT, 'source visual correction artifact requires one exact current FAA absence/match pair'); END;

DROP TRIGGER IF EXISTS aircraft_source_visual_artifacts_immutable_update;
CREATE TRIGGER aircraft_source_visual_artifacts_immutable_update
BEFORE UPDATE ON aircraft_source_visual_correction_artifacts
BEGIN SELECT RAISE(ABORT, 'aircraft source visual correction artifacts are immutable'); END;
DROP TRIGGER IF EXISTS aircraft_source_visual_artifacts_immutable_delete;
CREATE TRIGGER aircraft_source_visual_artifacts_immutable_delete
BEFORE DELETE ON aircraft_source_visual_correction_artifacts
BEGIN SELECT RAISE(ABORT, 'aircraft source visual correction artifacts are immutable'); END;

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
     AND decision.correction_kind IN ('faa_serial', 'visual_identifier')
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
  '20260821_aircraft_visual_source_corrections',
  1,
  'ccc63aa23f2579ec5cec682bf1493a13eb73829718936b5890bd84de51bb828a',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
