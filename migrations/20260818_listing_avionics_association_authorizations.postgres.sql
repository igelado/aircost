-- Replace reuse-only corroborations and their split collision scopes with one
-- exact listing-association authorization. Only hash-bound receipts persist.

BEGIN;

DO $migration_guard$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260805_listing_avionics_association_corroborations'
      AND contract_version = 1
      AND contract_fingerprint =
        '2c4661b8bf76e1a28d5ab5c636ed100f5d73f845c44b9515e5f46c5827e66fc9'
  ) OR NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260806_listing_avionics_collision_closure'
      AND contract_version = 1
      AND contract_fingerprint =
        '363fd039068667cca351c0009c0621e55942186a5d63804cf0e7da8212fa26b3'
  ) OR NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260807_avionics_product_reuse_v2'
      AND contract_version = 1
      AND contract_fingerprint =
        'efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc'
  ) OR EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260818_listing_avionics_association_authorizations'
      AND (
        contract_version <> 1
        OR contract_fingerprint <>
          'bbb76c8535647f2ecaab3179d5ef483bdef9ca23a0e14e3fd0888912fc3d90f9'
      )
  ) THEN
    RAISE EXCEPTION
      'listing avionics authorization migration is already installed or is missing a required predecessor';
  END IF;
END
$migration_guard$;

-- Empty migration-local predecessors make a verified re-application a no-op.
-- They are always dropped before commit and never enter the canonical schema.
CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics_corroborations (
  listing_link_id BIGINT NOT NULL,
  association_role TEXT NOT NULL,
  avionics_model_id BIGINT NOT NULL,
  observation_sha256 TEXT NOT NULL,
  product_fingerprint TEXT NOT NULL,
  policy_version TEXT NOT NULL,
  corroborated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics_corroboration_scopes (
  listing_link_id BIGINT NOT NULL,
  association_role TEXT NOT NULL,
  collision_closure_sha256 TEXT NOT NULL,
  policy_version TEXT NOT NULL
);

LOCK TABLE
  aircraft_sale_listing_avionics,
  aircraft_sale_listing_avionics_corroborations,
  aircraft_sale_listing_avionics_corroboration_scopes,
  avionics_product_reuse_attestations,
  plugin_submissions
IN SHARE ROW EXCLUSIVE MODE;

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics_authorizations (
  listing_link_id BIGINT NOT NULL
    REFERENCES aircraft_sale_listing_avionics(id) ON DELETE CASCADE,
  association_role TEXT NOT NULL
    CHECK (association_role IN ('installed', 'replacement')),
  avionics_model_id BIGINT NOT NULL
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  authorization_kind TEXT NOT NULL
    CHECK (authorization_kind IN ('manufacturer_reuse', 'same_case_grounded')),
  observation_sha256 TEXT NOT NULL CHECK (observation_sha256 ~ '^[0-9a-f]{64}$'),
  product_fingerprint TEXT NOT NULL CHECK (product_fingerprint ~ '^[0-9a-f]{64}$'),
  grounded_resolution_sha256 TEXT,
  evidence_capture_sha256 TEXT NOT NULL
    CHECK (evidence_capture_sha256 ~ '^[0-9a-f]{64}$'),
  collision_closure_sha256 TEXT NOT NULL
    CHECK (collision_closure_sha256 ~ '^[0-9a-f]{64}$'),
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'listing_avionics_authorization_v1'),
  authorized_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (listing_link_id, association_role),
  CHECK (
    (authorization_kind = 'manufacturer_reuse'
      AND grounded_resolution_sha256 IS NULL)
    OR
    (authorization_kind = 'same_case_grounded'
      AND grounded_resolution_sha256 ~ '^[0-9a-f]{64}$')
  )
);

INSERT INTO aircraft_sale_listing_avionics_authorizations (
  listing_link_id, association_role, avionics_model_id, authorization_kind,
  observation_sha256, product_fingerprint, grounded_resolution_sha256,
  evidence_capture_sha256, collision_closure_sha256, policy_version,
  authorized_at
)
SELECT
  corroboration.listing_link_id,
  corroboration.association_role,
  corroboration.avionics_model_id,
  'manufacturer_reuse',
  corroboration.observation_sha256,
  corroboration.product_fingerprint,
  NULL,
  capture.rendered_html_sha256,
  scope.collision_closure_sha256,
  'listing_avionics_authorization_v1',
  corroboration.corroborated_at
FROM aircraft_sale_listing_avionics_corroborations corroboration
JOIN aircraft_sale_listing_avionics_corroboration_scopes scope
  ON scope.listing_link_id = corroboration.listing_link_id
 AND scope.association_role = corroboration.association_role
JOIN aircraft_sale_listing_avionics link
  ON link.id = corroboration.listing_link_id
JOIN avionics_product_reuse_attestations attestation
  ON attestation.avionics_model_id = corroboration.avionics_model_id
 AND attestation.product_fingerprint = corroboration.product_fingerprint
 AND attestation.policy_version = 'avionics_reuse_v2'
JOIN LATERAL (
  SELECT MIN(source.rendered_html_sha256) AS rendered_html_sha256
  FROM plugin_submissions source
  WHERE source.canonical_listing_id = link.aircraft_sale_listing_id
    AND length(BTRIM(COALESCE(link.source_notes, ''))) > 0
    AND position(link.source_notes IN source.rendered_html) > 0
  HAVING COUNT(DISTINCT source.rendered_html_sha256) = 1
) capture ON TRUE
WHERE corroboration.policy_version = 'listing_avionics_association_v1'
  AND scope.policy_version = 'listing_avionics_collision_closure_v1'
  AND link.source_confidence = 'high';

DROP TRIGGER IF EXISTS listing_avionics_corroborations_invalidate_link_update
  ON aircraft_sale_listing_avionics;
DROP FUNCTION IF EXISTS invalidate_listing_avionics_corroboration();
DROP TABLE aircraft_sale_listing_avionics_corroboration_scopes;
DROP TABLE aircraft_sale_listing_avionics_corroborations;
DROP FUNCTION IF EXISTS preserve_listing_avionics_corroboration_scope();
DROP FUNCTION IF EXISTS validate_listing_avionics_corroboration();
DROP FUNCTION IF EXISTS preserve_listing_avionics_corroboration();

CREATE INDEX IF NOT EXISTS idx_listing_avionics_authorizations_model
  ON aircraft_sale_listing_avionics_authorizations (avionics_model_id);

CREATE OR REPLACE FUNCTION validate_listing_avionics_authorization()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_avionics link
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
      AND EXISTS (
        SELECT 1 FROM plugin_submissions capture
        WHERE capture.canonical_listing_id = link.aircraft_sale_listing_id
          AND capture.rendered_html_sha256 = NEW.evidence_capture_sha256
          AND position(link.source_notes IN capture.rendered_html) > 0
      )
      AND (
        (NEW.authorization_kind = 'manufacturer_reuse' AND EXISTS (
          SELECT 1 FROM avionics_product_reuse_attestations attestation
          WHERE attestation.avionics_model_id = NEW.avionics_model_id
            AND attestation.product_fingerprint = NEW.product_fingerprint
        ))
        OR
        (NEW.authorization_kind = 'same_case_grounded' AND EXISTS (
          SELECT 1 FROM avionics_approved_product_graph_identities identity
          WHERE identity.avionics_model_id = NEW.avionics_model_id
        ))
      )
  ) THEN
    RAISE EXCEPTION
      'listing avionics authorization requires the exact current link role, retained capture, and product proof';
  END IF;
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS listing_avionics_authorizations_validate_insert
  ON aircraft_sale_listing_avionics_authorizations;
CREATE TRIGGER listing_avionics_authorizations_validate_insert
BEFORE INSERT ON aircraft_sale_listing_avionics_authorizations
FOR EACH ROW EXECUTE FUNCTION validate_listing_avionics_authorization();

CREATE OR REPLACE FUNCTION preserve_listing_avionics_authorization()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  RAISE EXCEPTION 'listing avionics authorizations are replaced, never updated';
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS listing_avionics_authorizations_immutable_update
  ON aircraft_sale_listing_avionics_authorizations;
CREATE TRIGGER listing_avionics_authorizations_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_authorizations
FOR EACH ROW EXECUTE FUNCTION preserve_listing_avionics_authorization();

CREATE OR REPLACE FUNCTION invalidate_listing_avionics_authorization_for_link()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE listing_link_id = NEW.id;
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_link_update
  ON aircraft_sale_listing_avionics;
CREATE TRIGGER listing_avionics_authorizations_invalidate_link_update
AFTER UPDATE OF
  aircraft_sale_listing_id, avionics_model_id, quantity, source_notes,
  source_confidence, configuration_action, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
FOR EACH ROW EXECUTE FUNCTION invalidate_listing_avionics_authorization_for_link();

CREATE OR REPLACE FUNCTION invalidate_listing_avionics_authorization_for_reuse()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'manufacturer_reuse'
    AND avionics_model_id = OLD.avionics_model_id;
  RETURN OLD;
END
$function$;

DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_reuse_delete
  ON avionics_product_reuse_attestations;
CREATE TRIGGER listing_avionics_authorizations_invalidate_reuse_delete
AFTER DELETE ON avionics_product_reuse_attestations
FOR EACH ROW EXECUTE FUNCTION invalidate_listing_avionics_authorization_for_reuse();


CREATE OR REPLACE FUNCTION
  invalidate_listing_avionics_same_case_for_model_proof()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = OLD.id;
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_model_proof_update
ON avionics_models;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_model_proof_update
AFTER UPDATE OF
  avionics_manufacturer_id, name, normalized_name, catalog_status,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text
ON avionics_models
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_same_case_for_model_proof();

CREATE OR REPLACE FUNCTION
  invalidate_listing_avionics_same_case_for_model_type()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF TG_OP IN ('DELETE', 'UPDATE') THEN
    DELETE FROM aircraft_sale_listing_avionics_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = OLD.avionics_model_id;
  END IF;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    DELETE FROM aircraft_sale_listing_avionics_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = NEW.avionics_model_id;
  END IF;
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_model_type_insert
ON avionics_model_types;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_model_type_insert
AFTER INSERT ON avionics_model_types
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_same_case_for_model_type();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_model_type_delete
ON avionics_model_types;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_model_type_delete
AFTER DELETE ON avionics_model_types
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_same_case_for_model_type();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_model_type_update
ON avionics_model_types;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_model_type_update
AFTER UPDATE OF avionics_model_id, avionics_type_id ON avionics_model_types
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_same_case_for_model_type();

CREATE OR REPLACE FUNCTION
  invalidate_listing_avionics_same_case_for_type()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT avionics_model_id FROM avionics_model_types
      WHERE avionics_type_id = OLD.id
    );
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_type_update
ON avionics_types;
CREATE TRIGGER listing_avionics_authorizations_invalidate_type_update
AFTER UPDATE OF name, normalized_name ON avionics_types
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_same_case_for_type();

CREATE OR REPLACE FUNCTION
  invalidate_listing_avionics_same_case_for_graph()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  IF TG_OP IN ('DELETE', 'UPDATE') THEN
    DELETE FROM aircraft_sale_listing_avionics_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = OLD.avionics_model_id;
  END IF;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    DELETE FROM aircraft_sale_listing_avionics_authorizations
    WHERE authorization_kind = 'same_case_grounded'
      AND avionics_model_id = NEW.avionics_model_id;
  END IF;
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_graph_insert
ON avionics_approved_product_identities;
CREATE TRIGGER listing_avionics_authorizations_invalidate_graph_insert
AFTER INSERT ON avionics_approved_product_identities
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_same_case_for_graph();

DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_graph_delete
ON avionics_approved_product_identities;
CREATE TRIGGER listing_avionics_authorizations_invalidate_graph_delete
AFTER DELETE ON avionics_approved_product_identities
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_same_case_for_graph();

DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_graph_update
ON avionics_approved_product_identities;
CREATE TRIGGER listing_avionics_authorizations_invalidate_graph_update
AFTER UPDATE OF
  avionics_model_id, avionics_manufacturer_identity_id,
  canonical_product_key, manufacturer_identifier_kind,
  canonical_identifier_key
ON avionics_approved_product_identities
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_same_case_for_graph();

CREATE OR REPLACE FUNCTION
  invalidate_listing_avionics_same_case_for_manufacturer()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT id FROM avionics_models
      WHERE avionics_manufacturer_id = OLD.id
    );
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_manufacturer_update
ON avionics_manufacturers;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_manufacturer_update
AFTER UPDATE OF name, normalized_name ON avionics_manufacturers
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_same_case_for_manufacturer();

CREATE OR REPLACE FUNCTION
  invalidate_listing_avionics_same_case_for_origin_revocation()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT model.id
      FROM avionics_models model
      JOIN avionics_approved_product_graph_identities product_identity
        ON product_identity.avionics_model_id = model.id
      JOIN avionics_authoritative_source_origins source_origin
        ON source_origin.id =
             NEW.avionics_authoritative_source_origin_id
      LEFT JOIN avionics_manufacturer_effective_identities origin_identity
        ON origin_identity.identity_id =
             source_origin.avionics_manufacturer_identity_id
      WHERE (
          lower(BTRIM(model.identity_source_url)) = source_origin.https_origin
          OR substring(
              lower(BTRIM(model.identity_source_url))
              FROM 1 FOR length(source_origin.https_origin) + 1
            ) IN (
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

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_origin_revocation
ON avionics_authoritative_source_origin_revocations;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_origin_revocation
AFTER INSERT ON avionics_authoritative_source_origin_revocations
FOR EACH ROW
EXECUTE FUNCTION
  invalidate_listing_avionics_same_case_for_origin_revocation();


CREATE OR REPLACE FUNCTION
  invalidate_listing_avionics_authorization_for_capture()
RETURNS TRIGGER LANGUAGE plpgsql AS $function$
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations authorization_row
  USING aircraft_sale_listing_avionics link
  WHERE link.id = authorization_row.listing_link_id
    AND authorization_row.evidence_capture_sha256 = OLD.rendered_html_sha256
    AND link.aircraft_sale_listing_id = OLD.canonical_listing_id
    AND length(BTRIM(COALESCE(link.source_notes, ''))) > 0
    AND position(link.source_notes IN OLD.rendered_html) > 0
    AND NOT EXISTS (
      SELECT 1 FROM plugin_submissions retained_capture
      WHERE retained_capture.canonical_listing_id =
              link.aircraft_sale_listing_id
        AND retained_capture.rendered_html_sha256 =
              authorization_row.evidence_capture_sha256
        AND position(link.source_notes IN retained_capture.rendered_html) > 0
    );
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END
$function$;

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_capture_delete
ON plugin_submissions;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_capture_delete
AFTER DELETE ON plugin_submissions
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_authorization_for_capture();

DROP TRIGGER IF EXISTS
  listing_avionics_authorizations_invalidate_capture_update
ON plugin_submissions;
CREATE TRIGGER
  listing_avionics_authorizations_invalidate_capture_update
AFTER UPDATE OF canonical_listing_id, rendered_html, rendered_html_sha256
ON plugin_submissions
FOR EACH ROW
EXECUTE FUNCTION invalidate_listing_avionics_authorization_for_capture();


INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260818_listing_avionics_association_authorizations',
  1,
  'bbb76c8535647f2ecaab3179d5ef483bdef9ca23a0e14e3fd0888912fc3d90f9',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = EXCLUDED.contract_version,
  contract_fingerprint = EXCLUDED.contract_fingerprint,
  installed_at = EXCLUDED.installed_at;

COMMIT;
