BEGIN;

SET LOCAL search_path = public, pg_catalog, pg_temp;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
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
    WHERE migration_name = '20260821_aircraft_visual_source_corrections'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          'ccc63aa23f2579ec5cec682bf1493a13eb73829718936b5890bd84de51bb828a'
      )
  ) THEN
    RAISE EXCEPTION
      'installed aircraft visual source corrections migration has a different contract';
  END IF;
  IF pg_catalog.to_regclass(
       'public.aircraft_source_visual_correction_artifacts'
     ) IS NOT NULL
     AND NOT EXISTS (
       SELECT 1 FROM ONLY public.schema_migration_contracts
       WHERE migration_name = '20260821_aircraft_visual_source_corrections'
     ) THEN
    RAISE EXCEPTION
      'preexisting aircraft visual source correction table has no verified contract';
  END IF;
END
$migration_guard$;

CREATE TABLE IF NOT EXISTS public.aircraft_source_visual_correction_artifacts (
  plugin_submission_id BIGINT
    CONSTRAINT aircraft_source_visual_correction_artifacts_pkey PRIMARY KEY
    CONSTRAINT visual_source_artifact_submission_fk
    REFERENCES public.plugin_submissions(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL
    CONSTRAINT visual_source_artifact_rendered_sha256_check
    CHECK (rendered_html_sha256 ~ '^[0-9a-f]{64}$'),
  observed_registration_number TEXT NOT NULL,
  corrected_registration_number TEXT NOT NULL,
  corrected_serial_number TEXT,
  faa_registry_snapshot_id BIGINT NOT NULL
    CONSTRAINT visual_source_artifact_snapshot_fk
    REFERENCES public.faa_registry_snapshots(id) ON DELETE RESTRICT,
  faa_snapshot_archive_sha256 TEXT NOT NULL
    CONSTRAINT visual_source_artifact_archive_sha256_check
    CHECK (faa_snapshot_archive_sha256 ~ '^[0-9a-f]{64}$'),
  faa_source_record_sha256 TEXT NOT NULL
    CONSTRAINT visual_source_artifact_record_sha256_check
    CHECK (faa_source_record_sha256 ~ '^[0-9a-f]{64}$'),
  primary_photo_asset_id TEXT NOT NULL,
  primary_photo_url TEXT NOT NULL,
  primary_photo_sha256 TEXT NOT NULL
    CONSTRAINT visual_source_artifact_photo_sha256_check
    CHECK (primary_photo_sha256 ~ '^[0-9a-f]{64}$'),
  visual_resolution_sha256 TEXT NOT NULL
    CONSTRAINT visual_source_artifact_resolution_sha256_check
    CHECK (visual_resolution_sha256 ~ '^[0-9a-f]{64}$'),
  visual_resolution_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT visual_source_artifact_registration_distinct_check
    CHECK (observed_registration_number <> corrected_registration_number),
  CONSTRAINT visual_source_artifact_observed_registration_length_check
    CHECK (length(observed_registration_number) BETWEEN 2 AND 6),
  CONSTRAINT visual_source_artifact_corrected_registration_length_check
    CHECK (length(corrected_registration_number) BETWEEN 2 AND 6),
  CONSTRAINT visual_source_artifact_corrected_serial_length_check
    CHECK (corrected_serial_number IS NULL OR length(corrected_serial_number) BETWEEN 1 AND 128),
  CONSTRAINT visual_source_artifact_asset_id_length_check
    CHECK (length(primary_photo_asset_id) BETWEEN 1 AND 256),
  CONSTRAINT visual_source_artifact_url_length_check
    CHECK (length(primary_photo_url) BETWEEN 1 AND 4096),
  CONSTRAINT visual_source_artifact_resolution_json_check
    CHECK (length(visual_resolution_json) BETWEEN 2 AND 65536 AND jsonb_typeof(visual_resolution_json::jsonb) = 'object'),
  CONSTRAINT visual_source_artifact_observed_coverage_fk
    FOREIGN KEY (faa_registry_snapshot_id, observed_registration_number)
    REFERENCES public.faa_registry_coverage(snapshot_id, n_number) ON DELETE RESTRICT,
  CONSTRAINT visual_source_artifact_corrected_aircraft_fk
    FOREIGN KEY (faa_registry_snapshot_id, corrected_registration_number)
    REFERENCES public.faa_registry_aircraft(snapshot_id, n_number) ON DELETE RESTRICT,
  CONSTRAINT visual_source_artifact_record_aircraft_fk
    FOREIGN KEY (faa_registry_snapshot_id, faa_source_record_sha256)
    REFERENCES public.faa_registry_aircraft(snapshot_id, source_record_sha256) ON DELETE RESTRICT
);

CREATE OR REPLACE FUNCTION public.validate_aircraft_source_visual_correction_artifact()
RETURNS TRIGGER AS $$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.plugin_submissions submission
    JOIN public.faa_registry_snapshots snapshot ON snapshot.id = NEW.faa_registry_snapshot_id
    JOIN public.faa_registry_coverage observed
      ON observed.snapshot_id = snapshot.id
     AND observed.n_number = NEW.observed_registration_number
     AND observed.lookup_status = 'absent'
    JOIN public.faa_registry_coverage corrected
      ON corrected.snapshot_id = snapshot.id
     AND corrected.n_number = NEW.corrected_registration_number
     AND corrected.lookup_status = 'matched'
    JOIN public.faa_registry_aircraft aircraft
      ON aircraft.snapshot_id = snapshot.id
     AND aircraft.n_number = corrected.n_number
    WHERE submission.id = NEW.plugin_submission_id
      AND submission.rendered_html_sha256 = NEW.rendered_html_sha256
      AND snapshot.id = (
        SELECT id FROM public.faa_registry_snapshots
        ORDER BY snapshot_date DESC, id DESC LIMIT 1
      )
      AND snapshot.archive_sha256 = NEW.faa_snapshot_archive_sha256
      AND aircraft.source_record_sha256 = NEW.faa_source_record_sha256
      AND aircraft.manufacturer_serial_raw IS NOT DISTINCT FROM NEW.corrected_serial_number
  ) THEN
    RAISE EXCEPTION 'source visual correction artifact requires one exact current FAA absence/match pair';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql SET search_path = pg_catalog;
DROP TRIGGER IF EXISTS aircraft_source_visual_artifacts_validate_insert ON public.aircraft_source_visual_correction_artifacts;
CREATE TRIGGER aircraft_source_visual_artifacts_validate_insert
BEFORE INSERT ON public.aircraft_source_visual_correction_artifacts
FOR EACH ROW EXECUTE FUNCTION public.validate_aircraft_source_visual_correction_artifact();

CREATE OR REPLACE FUNCTION public.preserve_aircraft_source_visual_correction_artifact()
RETURNS TRIGGER AS $$
BEGIN
  RAISE EXCEPTION 'aircraft source visual correction artifacts are immutable';
END;
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;

DROP TRIGGER IF EXISTS aircraft_source_visual_artifacts_immutable
  ON public.aircraft_source_visual_correction_artifacts;
CREATE TRIGGER aircraft_source_visual_artifacts_immutable
BEFORE UPDATE OR DELETE ON public.aircraft_source_visual_correction_artifacts
FOR EACH ROW EXECUTE FUNCTION public.preserve_aircraft_source_visual_correction_artifact();

CREATE OR REPLACE FUNCTION public.require_source_identity_correction_receipt()
RETURNS TRIGGER AS $$
BEGIN
  IF OLD.ingestion_error = 'source_identity_correction_receipt_pending'
     AND (
       NEW.ingestion_error IS DISTINCT FROM OLD.ingestion_error
       OR NEW.ingestion_state IS DISTINCT FROM OLD.ingestion_state
       OR NEW.is_verified IS DISTINCT FROM OLD.is_verified
     )
     AND NOT EXISTS (
       SELECT 1
       FROM public.aircraft_listing_identity_correction_decisions decision
       JOIN public.plugin_submissions submission
         ON submission.id = decision.plugin_submission_id
       WHERE decision.aircraft_sale_listing_id = OLD.id
         AND decision.correction_kind IN ('faa_serial', 'visual_identifier')
         AND decision.rendered_html_sha256 = submission.rendered_html_sha256
         AND submission.user_id = OLD.created_by_user_id
         AND submission.canonical_listing_id = OLD.id
         AND submission.extraction_error IS NULL
         AND NEW.registration_number IS NOT DISTINCT FROM decision.corrected_registration_number
         AND NEW.serial_number IS NOT DISTINCT FROM decision.corrected_serial_number
     ) THEN
    RAISE EXCEPTION 'source identity correction receipt is required before leaving the receipt gate';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql
SET search_path = pg_catalog;

DROP TRIGGER IF EXISTS aircraft_source_identity_receipt_gate
  ON public.aircraft_sale_listings;
CREATE TRIGGER aircraft_source_identity_receipt_gate
BEFORE UPDATE OF ingestion_state, ingestion_error, is_verified
ON public.aircraft_sale_listings
FOR EACH ROW EXECUTE FUNCTION public.require_source_identity_correction_receipt();

INSERT INTO public.schema_migration_contracts AS installed_contract (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260821_aircraft_visual_source_corrections',
  1,
  'ccc63aa23f2579ec5cec682bf1493a13eb73829718936b5890bd84de51bb828a',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
