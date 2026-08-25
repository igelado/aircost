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
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          'a7a249e910f4c16530760d18786f106f11f3b36a25c6a3e80fa8adacd1b79b31'
      )
  ) THEN
    RAISE EXCEPTION
      'installed listing avionics grounded-capability migration has a different contract';
  END IF;
END
$migration_guard$;

CREATE TABLE IF NOT EXISTS public.aircraft_sale_listing_avionics_grounded_capabilities (
  listing_id BIGINT NOT NULL
    REFERENCES public.aircraft_sale_listings(id) ON DELETE CASCADE,
  plugin_submission_id BIGINT NOT NULL
    REFERENCES public.plugin_submissions(id) ON DELETE CASCADE,
  occurrence_index BIGINT NOT NULL CHECK (occurrence_index >= 0),
  occurrence_role TEXT NOT NULL
    CHECK (occurrence_role IN ('primary', 'replacement')),
  avionics_model_id BIGINT NOT NULL
    REFERENCES public.avionics_models(id) ON DELETE CASCADE,
  requested_quantity BIGINT NOT NULL CHECK (requested_quantity > 0),
  configuration_action TEXT NOT NULL
    CHECK (configuration_action IN ('installed', 'replaces', 'removes')),
  request_sha256 TEXT NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
  capability_sha256 TEXT NOT NULL
    CHECK (capability_sha256 ~ '^[0-9a-f]{64}$'),
  grounded_resolution_sha256 TEXT NOT NULL
    CHECK (grounded_resolution_sha256 ~ '^[0-9a-f]{64}$'),
  evidence_capture_sha256 TEXT NOT NULL
    CHECK (evidence_capture_sha256 ~ '^[0-9a-f]{64}$'),
  extracted_listing_sha256 TEXT NOT NULL
    CHECK (extracted_listing_sha256 ~ '^[0-9a-f]{64}$'),
  product_fingerprint TEXT NOT NULL
    CHECK (product_fingerprint ~ '^[0-9a-f]{64}$'),
  collision_closure_sha256 TEXT NOT NULL
    CHECK (collision_closure_sha256 ~ '^[0-9a-f]{64}$'),
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'listing_avionics_grounded_capability_v1'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (
    listing_id, plugin_submission_id, occurrence_index, occurrence_role
  ),
  CHECK (occurrence_role = 'primary' OR requested_quantity = 1),
  CHECK (
    occurrence_role = 'primary'
    OR configuration_action IN ('replaces', 'removes')
  )
);

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_grounded_capabilities_model
ON public.aircraft_sale_listing_avionics_grounded_capabilities (avionics_model_id);

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_grounded_capabilities_submission
ON public.aircraft_sale_listing_avionics_grounded_capabilities (plugin_submission_id);

CREATE OR REPLACE FUNCTION public.validate_listing_avionics_grounded_capability()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.plugin_submissions submission
    WHERE submission.id = NEW.plugin_submission_id
      AND submission.canonical_listing_id = NEW.listing_id
      AND submission.rendered_html_sha256 = NEW.evidence_capture_sha256
      AND submission.extracted_listing_json IS NOT NULL
      AND submission.extraction_error IS NULL
  ) OR NOT EXISTS (
    SELECT 1
    FROM public.avionics_approved_product_graph_identities approved
    WHERE approved.avionics_model_id = NEW.avionics_model_id
  ) THEN
    RAISE EXCEPTION 'grounded avionics capability requires its exact current capture-bound listing and approved product';
  END IF;
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS listing_avionics_grounded_capabilities_validate_insert
  ON public.aircraft_sale_listing_avionics_grounded_capabilities;
CREATE TRIGGER listing_avionics_grounded_capabilities_validate_insert
BEFORE INSERT ON public.aircraft_sale_listing_avionics_grounded_capabilities
FOR EACH ROW EXECUTE FUNCTION public.validate_listing_avionics_grounded_capability();

CREATE OR REPLACE FUNCTION public.reject_listing_avionics_grounded_capability_update()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  RAISE EXCEPTION 'grounded avionics capabilities are immutable';
END;
$function$;

DROP TRIGGER IF EXISTS listing_avionics_grounded_capabilities_immutable_update
  ON public.aircraft_sale_listing_avionics_grounded_capabilities;
CREATE TRIGGER listing_avionics_grounded_capabilities_immutable_update
BEFORE UPDATE ON public.aircraft_sale_listing_avionics_grounded_capabilities
FOR EACH ROW EXECUTE FUNCTION public.reject_listing_avionics_grounded_capability_update();

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260825_listing_avionics_grounded_capabilities',
  1,
  'a7a249e910f4c16530760d18786f106f11f3b36a25c6a3e80fa8adacd1b79b31',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
