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
        contract_version IS DISTINCT FROM 2
        OR contract_fingerprint IS DISTINCT FROM
          'fa4cbe0caefd049a712d3b7bdbc593a1984171d2a663b70a49591ccfb1d7ca30'
      )
  ) THEN
    RAISE EXCEPTION
      'installed listing avionics grounded-capability migration has a different contract';
  END IF;
END
$migration_guard$;

DROP TABLE IF EXISTS
  public.aircraft_sale_listing_avionics_grounded_capabilities;

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
    CHECK (policy_version = 'listing_avionics_grounded_capability_v2'),
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

DROP TABLE IF EXISTS public.aircraft_sale_listing_avionics_authorizations;

CREATE TABLE public.aircraft_sale_listing_avionics_authorizations (
  listing_link_id BIGINT NOT NULL
    REFERENCES public.aircraft_sale_listing_avionics(id) ON DELETE CASCADE,
  association_role TEXT NOT NULL
    CHECK (association_role IN ('installed', 'replacement')),
  avionics_model_id BIGINT NOT NULL
    REFERENCES public.avionics_models(id) ON DELETE CASCADE,
  authorization_kind TEXT NOT NULL
    CHECK (authorization_kind IN ('manufacturer_reuse', 'same_case_grounded')),
  observation_sha256 TEXT NOT NULL
    CHECK (observation_sha256 ~ '^[0-9a-f]{64}$'),
  product_fingerprint TEXT NOT NULL
    CHECK (product_fingerprint ~ '^[0-9a-f]{64}$'),
  grounded_resolution_sha256 TEXT,
  evidence_capture_sha256 TEXT NOT NULL
    CHECK (evidence_capture_sha256 ~ '^[0-9a-f]{64}$'),
  plugin_submission_id BIGINT
    REFERENCES public.plugin_submissions(id) ON DELETE CASCADE,
  extracted_listing_sha256 TEXT
    CHECK (extracted_listing_sha256 IS NULL OR
           extracted_listing_sha256 ~ '^[0-9a-f]{64}$'),
  collision_closure_sha256 TEXT NOT NULL
    CHECK (collision_closure_sha256 ~ '^[0-9a-f]{64}$'),
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'listing_avionics_authorization_v2'),
  authorized_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (listing_link_id, association_role),
  CHECK (
    (authorization_kind = 'manufacturer_reuse'
      AND grounded_resolution_sha256 IS NULL
      AND plugin_submission_id IS NULL
      AND extracted_listing_sha256 IS NULL)
    OR
    (authorization_kind = 'same_case_grounded'
      AND grounded_resolution_sha256 ~ '^[0-9a-f]{64}$'
      AND plugin_submission_id IS NOT NULL
      AND extracted_listing_sha256 IS NOT NULL)
  )
);

CREATE INDEX idx_listing_avionics_authorizations_model
ON public.aircraft_sale_listing_avionics_authorizations (avionics_model_id);

CREATE OR REPLACE FUNCTION public.validate_listing_avionics_authorization()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM public.aircraft_sale_listing_avionics link
    WHERE link.id = NEW.listing_link_id
      AND link.source_confidence = 'high'
      AND length(BTRIM(COALESCE(link.source_notes, ''))) > 0
      AND (
        (NEW.association_role = 'installed'
          AND link.avionics_model_id = NEW.avionics_model_id)
        OR
        (NEW.association_role = 'replacement'
          AND link.configuration_action IN ('replaces', 'removes')
          AND link.replaces_avionics_model_id = NEW.avionics_model_id)
      )
      AND (
        (NEW.authorization_kind = 'manufacturer_reuse'
          AND EXISTS (
            SELECT 1 FROM public.plugin_submissions capture
            WHERE capture.canonical_listing_id = link.aircraft_sale_listing_id
              AND capture.rendered_html_sha256 = NEW.evidence_capture_sha256
              AND position(link.source_notes IN capture.rendered_html) > 0
          )
          AND EXISTS (
            SELECT 1 FROM public.avionics_product_reuse_attestations attestation
            WHERE attestation.avionics_model_id = NEW.avionics_model_id
              AND attestation.product_fingerprint = NEW.product_fingerprint
          ))
        OR
        (NEW.authorization_kind = 'same_case_grounded'
          AND EXISTS (
            SELECT 1 FROM public.plugin_submissions submission
            WHERE submission.id = NEW.plugin_submission_id
              AND submission.canonical_listing_id = link.aircraft_sale_listing_id
              AND submission.rendered_html_sha256 = NEW.evidence_capture_sha256
              AND submission.extracted_listing_json IS NOT NULL
              AND submission.extraction_error IS NULL
              AND position(link.source_notes IN submission.rendered_html) > 0
          )
          AND EXISTS (
            SELECT 1 FROM public.avionics_approved_product_graph_identities identity
            WHERE identity.avionics_model_id = NEW.avionics_model_id
          ))
      )
  ) THEN
    RAISE EXCEPTION
      'listing avionics authorization requires the exact current link role, retained capture, and product proof';
  END IF;
  RETURN NEW;
END;
$function$;

CREATE TRIGGER listing_avionics_authorizations_validate_insert
BEFORE INSERT ON public.aircraft_sale_listing_avionics_authorizations
FOR EACH ROW EXECUTE FUNCTION public.validate_listing_avionics_authorization();

CREATE OR REPLACE FUNCTION public.preserve_listing_avionics_authorization()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  RAISE EXCEPTION 'listing avionics authorizations are replaced, never updated';
  RETURN NEW;
END;
$function$;

CREATE TRIGGER listing_avionics_authorizations_immutable_update
BEFORE UPDATE ON public.aircraft_sale_listing_avionics_authorizations
FOR EACH ROW EXECUTE FUNCTION public.preserve_listing_avionics_authorization();

CREATE OR REPLACE FUNCTION public.invalidate_listing_avionics_authorization_for_link()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_authorizations
  WHERE listing_link_id = NEW.id;
  RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION public.invalidate_listing_avionics_authorization_for_reuse()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'manufacturer_reuse'
    AND avionics_model_id = OLD.avionics_model_id;
  RETURN OLD;
END;
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_model_proof()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = OLD.id;
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_model_type()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF TG_OP IN ('DELETE', 'UPDATE') THEN
    DELETE FROM public.aircraft_sale_listing_avionics_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = OLD.avionics_model_id;
  END IF;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    DELETE FROM public.aircraft_sale_listing_avionics_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = NEW.avionics_model_id;
  END IF;
  IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_type()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT avionics_model_id FROM public.avionics_model_types
      WHERE avionics_type_id = OLD.id
    );
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_graph()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  IF TG_OP IN ('DELETE', 'UPDATE') THEN
    DELETE FROM public.aircraft_sale_listing_avionics_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = OLD.avionics_model_id;
  END IF;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    DELETE FROM public.aircraft_sale_listing_avionics_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = NEW.avionics_model_id;
  END IF;
  IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_manufacturer()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT id FROM public.avionics_models
      WHERE avionics_manufacturer_id = OLD.id
    );
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_same_case_for_origin_revocation()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT model.id
      FROM public.avionics_models model
      JOIN public.avionics_approved_product_graph_identities product_identity
        ON product_identity.avionics_model_id = model.id
      JOIN public.avionics_authoritative_source_origins source_origin
        ON source_origin.id = NEW.avionics_authoritative_source_origin_id
      LEFT JOIN public.avionics_manufacturer_effective_identities origin_identity
        ON origin_identity.identity_id =
           source_origin.avionics_manufacturer_identity_id
      WHERE (
          lower(BTRIM(model.identity_source_url)) = source_origin.https_origin
          OR substring(lower(BTRIM(model.identity_source_url))
               FROM 1 FOR length(source_origin.https_origin) + 1) IN (
            source_origin.https_origin || '/',
            source_origin.https_origin || '?',
            source_origin.https_origin || '#'
          )
        )
        AND (
          source_origin.authority_kind = 'regulator_primary'
          OR (
            source_origin.authority_kind = 'manufacturer_primary'
            AND origin_identity.avionics_manufacturer_identity_id =
                product_identity.avionics_manufacturer_identity_id
          )
        )
    );
  RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION
  public.invalidate_listing_avionics_authorization_for_capture()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog
AS $function$
BEGIN
  DELETE FROM public.aircraft_sale_listing_avionics_authorizations authorization_row
  USING public.aircraft_sale_listing_avionics link
  WHERE link.id = authorization_row.listing_link_id
    AND authorization_row.evidence_capture_sha256 = OLD.rendered_html_sha256
    AND link.aircraft_sale_listing_id = OLD.canonical_listing_id
    AND length(BTRIM(COALESCE(link.source_notes, ''))) > 0
    AND position(link.source_notes IN OLD.rendered_html) > 0
    AND NOT EXISTS (
      SELECT 1 FROM public.plugin_submissions retained_capture
      WHERE retained_capture.canonical_listing_id =
              link.aircraft_sale_listing_id
        AND retained_capture.rendered_html_sha256 =
              authorization_row.evidence_capture_sha256
        AND position(link.source_notes IN retained_capture.rendered_html) > 0
    );
  DELETE FROM public.aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND plugin_submission_id = OLD.id;
  IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_capture_update
ON public.plugin_submissions;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_capture_update
AFTER UPDATE OF canonical_listing_id, rendered_html, rendered_html_sha256,
  extracted_listing_json, extraction_error
ON public.plugin_submissions
FOR EACH ROW
EXECUTE FUNCTION public.invalidate_listing_avionics_authorization_for_capture();

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260825_listing_avionics_grounded_capabilities',
  2,
  'fa4cbe0caefd049a712d3b7bdbc593a1984171d2a663b70a49591ccfb1d7ca30',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
