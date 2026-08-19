BEGIN;

INSERT INTO public.curation_evidence_sources (
  source_url, source_title, source_domain, source_tier,
  content_sha256, retrieved_at
) VALUES (
  'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
  'FAA releasable aircraft fixture', 'faa.gov', 'regulator_primary',
  repeat('1', 64), '2026-08-19'
)
ON CONFLICT (source_url, content_sha256) DO NOTHING;

INSERT INTO public.faa_registry_snapshots (
  evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256,
  master_member_name, master_member_sha256,
  aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256
) VALUES (
  (
    SELECT id FROM public.curation_evidence_sources
    WHERE source_url =
      'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download'
      AND content_sha256 = repeat('1', 64)
  ),
  '2026-08-19',
  'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
  repeat('1', 64), repeat('2', 64), repeat('3', 64),
  'MASTER.txt', repeat('4', 64),
  'ACFTREF.txt', repeat('5', 64),
  'ENGINE.txt', repeat('6', 64)
)
ON CONFLICT (archive_sha256, target_set_sha256) DO NOTHING;

INSERT INTO public.faa_registry_aircraft (
  snapshot_id, n_number, manufacturer_serial_raw, manufacturer_serial_key,
  aircraft_code, engine_code, year_manufactured, source_record_sha256
) VALUES (
  (
    SELECT id FROM public.faa_registry_snapshots
    WHERE archive_sha256 = repeat('1', 64)
      AND target_set_sha256 = repeat('3', 64)
  ),
  'N123AB', '182-01234', '18201234', '2072738', '41528', 2006,
  repeat('7', 64)
)
ON CONFLICT (snapshot_id, n_number) DO NOTHING;

INSERT INTO public.faa_registry_aircraft_references (
  snapshot_id, aircraft_code, manufacturer_name, model_name
)
SELECT id, '2072738', 'CESSNA AIRCRAFT CO', '182T'
FROM public.faa_registry_snapshots
WHERE archive_sha256 = repeat('1', 64)
  AND target_set_sha256 = repeat('3', 64)
ON CONFLICT (snapshot_id, aircraft_code) DO NOTHING;

INSERT INTO public.faa_registry_engine_references (
  snapshot_id, engine_code, manufacturer_name, model_name
)
SELECT id, '41528', 'LYCOMING', 'IO-540-AB1A5'
FROM public.faa_registry_snapshots
WHERE archive_sha256 = repeat('1', 64)
  AND target_set_sha256 = repeat('3', 64)
ON CONFLICT (snapshot_id, engine_code) DO NOTHING;

DO $assertions$
DECLARE
  fixture_snapshot_id BIGINT;
BEGIN
  SELECT id INTO STRICT fixture_snapshot_id
  FROM public.faa_registry_snapshots
  WHERE archive_sha256 = repeat('1', 64)
    AND target_set_sha256 = repeat('3', 64);

  IF NOT EXISTS (
    SELECT 1 FROM public.faa_registry_aircraft_references
    WHERE snapshot_id = fixture_snapshot_id AND aircraft_code = '2072738'
  ) OR NOT EXISTS (
    SELECT 1 FROM public.faa_registry_engine_references
    WHERE snapshot_id = fixture_snapshot_id AND engine_code = '41528'
  ) THEN
    RAISE EXCEPTION 'FAA reference reachability fixture was not stored';
  END IF;
END;
$assertions$;

ROLLBACK;
