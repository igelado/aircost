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

DROP TABLE IF EXISTS temp.listing_avionics_grounded_capabilities_migration_guard;
CREATE TEMP TABLE listing_avionics_grounded_capabilities_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO listing_avionics_grounded_capabilities_migration_guard (accepted)
SELECT CASE
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
      AND contract_version = 1
      AND contract_fingerprint =
        'e29dd6062dca13f4b97cdc9a78666bf407ec791d78e7a858c4ae09333fcf677e'
  )
  AND (
    SELECT group_concat(name, ',')
    FROM (
      SELECT name
      FROM pragma_table_info(
        'aircraft_sale_listing_avionics_grounded_capabilities'
      )
      ORDER BY cid
    )
  ) = 'listing_id,plugin_submission_id,occurrence_index,occurrence_role,avionics_model_id,requested_quantity,configuration_action,request_sha256,capability_sha256,grounded_resolution_sha256,evidence_capture_sha256,extracted_listing_sha256,product_fingerprint,collision_closure_sha256,source_revocation_count,policy_version,created_at'
  AND (
    SELECT replace(replace(replace(replace(
      sql, char(9), ''), char(10), ''), char(13), ''), ' ', '')
    FROM sqlite_schema
    WHERE type = 'table'
      AND name = 'aircraft_sale_listing_avionics_grounded_capabilities'
  ) = 'CREATETABLEaircraft_sale_listing_avionics_grounded_capabilities(listing_idINTEGERNOTNULLREFERENCESaircraft_sale_listings(id)ONDELETECASCADE,plugin_submission_idINTEGERNOTNULLREFERENCESplugin_submissions(id)ONDELETECASCADE,occurrence_indexINTEGERNOTNULLCHECK(occurrence_index>=0),occurrence_roleTEXTNOTNULLCHECK(occurrence_roleIN(''primary'',''replacement'')),avionics_model_idINTEGERNOTNULLREFERENCESavionics_models(id)ONDELETECASCADE,requested_quantityINTEGERNOTNULLCHECK(requested_quantity>0),configuration_actionTEXTNOTNULLCHECK(configuration_actionIN(''installed'',''replaces'',''removes'')),request_sha256TEXTNOTNULL,capability_sha256TEXTNOTNULL,grounded_resolution_sha256TEXTNOTNULL,evidence_capture_sha256TEXTNOTNULL,extracted_listing_sha256TEXTNOTNULL,product_fingerprintTEXTNOTNULL,collision_closure_sha256TEXTNOTNULL,source_revocation_countINTEGERNOTNULLCHECK(source_revocation_count>=0),policy_versionTEXTNOTNULLCHECK(policy_version=''listing_avionics_grounded_capability''),created_atTEXTNOTNULLDEFAULTCURRENT_TIMESTAMP,PRIMARYKEY(listing_id,plugin_submission_id,occurrence_index,occurrence_role),CHECK(occurrence_role=''primary''ORrequested_quantity=1),CHECK(occurrence_role=''primary''ORconfiguration_actionIN(''replaces'',''removes'')),CHECK(length(request_sha256)=64),CHECK(request_sha256=lower(request_sha256)),CHECK(request_sha256NOTGLOB''*[^0-9a-f]*''),CHECK(length(capability_sha256)=64),CHECK(capability_sha256=lower(capability_sha256)),CHECK(capability_sha256NOTGLOB''*[^0-9a-f]*''),CHECK(length(grounded_resolution_sha256)=64),CHECK(grounded_resolution_sha256=lower(grounded_resolution_sha256)),CHECK(grounded_resolution_sha256NOTGLOB''*[^0-9a-f]*''),CHECK(length(evidence_capture_sha256)=64),CHECK(evidence_capture_sha256=lower(evidence_capture_sha256)),CHECK(evidence_capture_sha256NOTGLOB''*[^0-9a-f]*''),CHECK(length(extracted_listing_sha256)=64),CHECK(extracted_listing_sha256=lower(extracted_listing_sha256)),CHECK(extracted_listing_sha256NOTGLOB''*[^0-9a-f]*''),CHECK(length(product_fingerprint)=64),CHECK(product_fingerprint=lower(product_fingerprint)),CHECK(product_fingerprintNOTGLOB''*[^0-9a-f]*''),CHECK(length(collision_closure_sha256)=64),CHECK(collision_closure_sha256=lower(collision_closure_sha256)),CHECK(collision_closure_sha256NOTGLOB''*[^0-9a-f]*''))'
  AND (
    SELECT group_concat(name, ',')
    FROM (
      SELECT name
      FROM pragma_table_info(
        'aircraft_sale_listing_avionics_link_authorizations'
      )
      ORDER BY cid
    )
  ) = 'listing_link_id,association_role,avionics_model_id,authorization_kind,observation_sha256,product_fingerprint,grounded_resolution_sha256,evidence_capture_sha256,plugin_submission_id,extracted_listing_sha256,collision_closure_sha256,source_revocation_count,policy_version,authorized_at'
  AND (
    SELECT replace(replace(replace(replace(
      sql, char(9), ''), char(10), ''), char(13), ''), ' ', '')
    FROM sqlite_schema
    WHERE type = 'table'
      AND name = 'aircraft_sale_listing_avionics_link_authorizations'
  ) = 'CREATETABLEaircraft_sale_listing_avionics_link_authorizations(listing_link_idINTEGERNOTNULLREFERENCESaircraft_sale_listing_avionics(id)ONDELETECASCADE,association_roleTEXTNOTNULLCHECK(association_roleIN(''installed'',''replacement'')),avionics_model_idINTEGERNOTNULLREFERENCESavionics_models(id)ONDELETECASCADE,authorization_kindTEXTNOTNULLCHECK(authorization_kindIN(''manufacturer_reuse'',''same_case_grounded'')),observation_sha256TEXTNOTNULL,product_fingerprintTEXTNOTNULL,grounded_resolution_sha256TEXT,evidence_capture_sha256TEXTNOTNULL,plugin_submission_idINTEGERREFERENCESplugin_submissions(id)ONDELETECASCADE,extracted_listing_sha256TEXT,collision_closure_sha256TEXTNOTNULL,source_revocation_countINTEGER,policy_versionTEXTNOTNULLCHECK(policy_version=''listing_avionics_authorization''),authorized_atTEXTNOTNULLDEFAULTCURRENT_TIMESTAMP,PRIMARYKEY(listing_link_id,association_role),CHECK(length(observation_sha256)=64),CHECK(observation_sha256=lower(observation_sha256)),CHECK(observation_sha256NOTGLOB''*[^0-9a-f]*''),CHECK(length(product_fingerprint)=64),CHECK(product_fingerprint=lower(product_fingerprint)),CHECK(product_fingerprintNOTGLOB''*[^0-9a-f]*''),CHECK(length(evidence_capture_sha256)=64),CHECK(evidence_capture_sha256=lower(evidence_capture_sha256)),CHECK(evidence_capture_sha256NOTGLOB''*[^0-9a-f]*''),CHECK(extracted_listing_sha256ISNULLOR(length(extracted_listing_sha256)=64ANDextracted_listing_sha256=lower(extracted_listing_sha256)ANDextracted_listing_sha256NOTGLOB''*[^0-9a-f]*'')),CHECK(length(collision_closure_sha256)=64),CHECK(collision_closure_sha256=lower(collision_closure_sha256)),CHECK(collision_closure_sha256NOTGLOB''*[^0-9a-f]*''),CHECK((authorization_kind=''manufacturer_reuse''ANDgrounded_resolution_sha256ISNULLANDplugin_submission_idISNULLANDextracted_listing_sha256ISNULLANDsource_revocation_countISNULL)OR(authorization_kind=''same_case_grounded''ANDlength(grounded_resolution_sha256)=64ANDgrounded_resolution_sha256=lower(grounded_resolution_sha256)ANDgrounded_resolution_sha256NOTGLOB''*[^0-9a-f]*''ANDplugin_submission_idISNOTNULLANDextracted_listing_sha256ISNOTNULLANDsource_revocation_countISNOTNULLANDsource_revocation_count>=0)))'
  THEN 1
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
  )
  AND NOT EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'table'
      AND name = 'aircraft_sale_listing_avionics_grounded_capabilities'
  )
  AND NOT EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'table'
      AND name = 'aircraft_sale_listing_avionics_link_authorizations'
  )
  AND NOT EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE name IN (
      'idx_listing_avionics_grounded_capabilities_model',
      'idx_listing_avionics_grounded_capabilities_submission',
      'listing_avionics_grounded_capabilities_validate_insert',
      'listing_avionics_grounded_capabilities_immutable_update'
    )
  )
  AND NOT EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE name IN (
      'idx_listing_avionics_authorizations_model',
      'listing_avionics_authorizations_validate_insert',
      'listing_avionics_authorizations_immutable_update',
      'listing_avionics_authorizations_invalidate_link_update',
      'listing_avionics_authorizations_invalidate_reuse_delete',
      'listing_avionics_authorizations_invalidate_model_proof_update',
      'listing_avionics_authorizations_invalidate_model_type_insert',
      'listing_avionics_authorizations_invalidate_model_type_delete',
      'listing_avionics_authorizations_invalidate_model_type_update',
      'listing_avionics_authorizations_invalidate_type_update',
      'listing_avionics_authorizations_invalidate_graph_insert',
      'listing_avionics_authorizations_invalidate_graph_delete',
      'listing_avionics_authorizations_invalidate_graph_update',
      'listing_avionics_authorizations_invalidate_manufacturer_update',
      'listing_avionics_authorizations_invalidate_origin_revocation',
      'listing_avionics_authorizations_invalidate_capture_delete',
      'listing_avionics_authorizations_invalidate_capture_update'
    )
  )
  THEN 1
  ELSE 0
END;
DROP TABLE listing_avionics_grounded_capabilities_migration_guard;

DROP TABLE IF EXISTS temp.listing_avionics_grounded_capabilities_rerun_state;
CREATE TEMP TABLE listing_avionics_grounded_capabilities_rerun_state AS
SELECT EXISTS (
  SELECT 1 FROM schema_migration_contracts
  WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
    AND contract_version = 1
    AND contract_fingerprint =
      'e29dd6062dca13f4b97cdc9a78666bf407ec791d78e7a858c4ae09333fcf677e'
) AS exact_rerun;

DROP TABLE IF EXISTS temp.listing_avionics_grounded_capabilities_installed_objects;
CREATE TEMP TABLE listing_avionics_grounded_capabilities_installed_objects AS
SELECT type, name,
       replace(replace(replace(replace(
         sql, char(9), ''), char(10), ''), char(13), ''), ' ', '')
         AS definition
FROM sqlite_schema
WHERE sql IS NOT NULL
  AND (
    (type = 'index' AND tbl_name IN (
      'aircraft_sale_listing_avionics_grounded_capabilities',
      'aircraft_sale_listing_avionics_link_authorizations'
    ))
    OR
    (type = 'trigger' AND (
      tbl_name IN (
        'aircraft_sale_listing_avionics_grounded_capabilities',
        'aircraft_sale_listing_avionics_link_authorizations'
      )
      OR instr(lower(sql),
        'aircraft_sale_listing_avionics_grounded_capabilities') > 0
      OR instr(lower(sql),
        'aircraft_sale_listing_avionics_link_authorizations') > 0
    ))
  );

DROP TABLE IF EXISTS temp.listing_avionics_grounded_capabilities_object_guard;
CREATE TEMP TABLE listing_avionics_grounded_capabilities_object_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 2)
);
INSERT INTO listing_avionics_grounded_capabilities_object_guard (accepted)
SELECT CASE
  WHEN exact_rerun = 0
    OR (SELECT COUNT(*)
        FROM listing_avionics_grounded_capabilities_installed_objects) = 21
  THEN 2 ELSE 0
END
FROM listing_avionics_grounded_capabilities_rerun_state;
DROP TABLE listing_avionics_grounded_capabilities_object_guard;

DROP INDEX IF EXISTS idx_listing_avionics_grounded_capabilities_model;
DROP INDEX IF EXISTS idx_listing_avionics_grounded_capabilities_submission;

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics_grounded_capabilities (
  listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE CASCADE,
  occurrence_index INTEGER NOT NULL CHECK (occurrence_index >= 0),
  occurrence_role TEXT NOT NULL
    CHECK (occurrence_role IN ('primary', 'replacement')),
  avionics_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  requested_quantity INTEGER NOT NULL CHECK (requested_quantity > 0),
  configuration_action TEXT NOT NULL
    CHECK (configuration_action IN ('installed', 'replaces', 'removes')),
  request_sha256 TEXT NOT NULL,
  capability_sha256 TEXT NOT NULL,
  grounded_resolution_sha256 TEXT NOT NULL,
  evidence_capture_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT NOT NULL,
  product_fingerprint TEXT NOT NULL,
  collision_closure_sha256 TEXT NOT NULL,
  source_revocation_count INTEGER NOT NULL
    CHECK (source_revocation_count >= 0),
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'listing_avionics_grounded_capability'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (
    listing_id, plugin_submission_id, occurrence_index, occurrence_role
  ),
  CHECK (occurrence_role = 'primary' OR requested_quantity = 1),
  CHECK (
    occurrence_role = 'primary'
    OR configuration_action IN ('replaces', 'removes')
  ),
  CHECK (length(request_sha256) = 64),
  CHECK (request_sha256 = lower(request_sha256)),
  CHECK (request_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(capability_sha256) = 64),
  CHECK (capability_sha256 = lower(capability_sha256)),
  CHECK (capability_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(grounded_resolution_sha256) = 64),
  CHECK (grounded_resolution_sha256 = lower(grounded_resolution_sha256)),
  CHECK (grounded_resolution_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(evidence_capture_sha256) = 64),
  CHECK (evidence_capture_sha256 = lower(evidence_capture_sha256)),
  CHECK (evidence_capture_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(extracted_listing_sha256) = 64),
  CHECK (extracted_listing_sha256 = lower(extracted_listing_sha256)),
  CHECK (extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(product_fingerprint) = 64),
  CHECK (product_fingerprint = lower(product_fingerprint)),
  CHECK (product_fingerprint NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(collision_closure_sha256) = 64),
  CHECK (collision_closure_sha256 = lower(collision_closure_sha256)),
  CHECK (collision_closure_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_grounded_capabilities_model
ON aircraft_sale_listing_avionics_grounded_capabilities (avionics_model_id);

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_grounded_capabilities_submission
ON aircraft_sale_listing_avionics_grounded_capabilities (plugin_submission_id);

DROP TRIGGER IF EXISTS listing_avionics_grounded_capabilities_validate_insert;
CREATE TRIGGER listing_avionics_grounded_capabilities_validate_insert
BEFORE INSERT ON aircraft_sale_listing_avionics_grounded_capabilities
WHEN NOT EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.canonical_listing_id = NEW.listing_id
    AND submission.rendered_html_sha256 = NEW.evidence_capture_sha256
    AND submission.extracted_listing_json IS NOT NULL
    AND submission.extraction_error IS NULL
)
OR NOT EXISTS (
  SELECT 1
  FROM avionics_approved_product_graph_identities approved
  WHERE approved.avionics_model_id = NEW.avionics_model_id
)
OR NEW.source_revocation_count <> (
  SELECT COUNT(*) FROM avionics_authoritative_source_origin_revocations
)
BEGIN
  SELECT RAISE(ABORT, 'grounded avionics capability requires its exact current capture-bound listing, approved product, and source-revocation epoch');
END;

DROP TRIGGER IF EXISTS listing_avionics_grounded_capabilities_immutable_update;
CREATE TRIGGER listing_avionics_grounded_capabilities_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_grounded_capabilities
BEGIN
  SELECT RAISE(ABORT, 'grounded avionics capabilities are immutable');
END;

DROP TRIGGER IF EXISTS listing_avionics_authorizations_validate_insert;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_immutable_update;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_link_update;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_reuse_delete;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_model_proof_update;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_model_type_insert;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_model_type_delete;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_model_type_update;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_type_update;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_graph_insert;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_graph_delete;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_graph_update;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_manufacturer_update;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_origin_revocation;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_capture_delete;
DROP TRIGGER IF EXISTS listing_avionics_authorizations_invalidate_capture_update;
DROP INDEX IF EXISTS idx_listing_avionics_authorizations_model;
CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics_link_authorizations (
  listing_link_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listing_avionics(id) ON DELETE CASCADE,
  association_role TEXT NOT NULL
    CHECK (association_role IN ('installed', 'replacement')),
  avionics_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  authorization_kind TEXT NOT NULL
    CHECK (authorization_kind IN ('manufacturer_reuse', 'same_case_grounded')),
  observation_sha256 TEXT NOT NULL,
  product_fingerprint TEXT NOT NULL,
  grounded_resolution_sha256 TEXT,
  evidence_capture_sha256 TEXT NOT NULL,
  plugin_submission_id INTEGER
    REFERENCES plugin_submissions(id) ON DELETE CASCADE,
  extracted_listing_sha256 TEXT,
  collision_closure_sha256 TEXT NOT NULL,
  source_revocation_count INTEGER,
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'listing_avionics_authorization'),
  authorized_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (listing_link_id, association_role),
  CHECK (length(observation_sha256) = 64),
  CHECK (observation_sha256 = lower(observation_sha256)),
  CHECK (observation_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(product_fingerprint) = 64),
  CHECK (product_fingerprint = lower(product_fingerprint)),
  CHECK (product_fingerprint NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(evidence_capture_sha256) = 64),
  CHECK (evidence_capture_sha256 = lower(evidence_capture_sha256)),
  CHECK (evidence_capture_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (extracted_listing_sha256 IS NULL OR (
    length(extracted_listing_sha256) = 64
    AND extracted_listing_sha256 = lower(extracted_listing_sha256)
    AND extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*'
  )),
  CHECK (length(collision_closure_sha256) = 64),
  CHECK (collision_closure_sha256 = lower(collision_closure_sha256)),
  CHECK (collision_closure_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (
    (authorization_kind = 'manufacturer_reuse'
      AND grounded_resolution_sha256 IS NULL
      AND plugin_submission_id IS NULL
      AND extracted_listing_sha256 IS NULL
      AND source_revocation_count IS NULL)
    OR
    (authorization_kind = 'same_case_grounded'
      AND length(grounded_resolution_sha256) = 64
      AND grounded_resolution_sha256 = lower(grounded_resolution_sha256)
      AND grounded_resolution_sha256 NOT GLOB '*[^0-9a-f]*'
      AND plugin_submission_id IS NOT NULL
      AND extracted_listing_sha256 IS NOT NULL
      AND source_revocation_count IS NOT NULL
      AND source_revocation_count >= 0)
  )
);

CREATE INDEX IF NOT EXISTS idx_listing_avionics_authorizations_model
ON aircraft_sale_listing_avionics_link_authorizations (avionics_model_id);

CREATE TRIGGER listing_avionics_authorizations_validate_insert
BEFORE INSERT ON aircraft_sale_listing_avionics_link_authorizations
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_avionics link
  WHERE link.id = NEW.listing_link_id
    AND link.source_confidence = 'high'
    AND length(trim(COALESCE(link.source_notes, ''))) > 0
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
          SELECT 1 FROM plugin_submissions capture
          WHERE capture.canonical_listing_id = link.aircraft_sale_listing_id
            AND capture.rendered_html_sha256 = NEW.evidence_capture_sha256
            AND instr(capture.rendered_html, link.source_notes) > 0
        )
        AND EXISTS (
          SELECT 1 FROM avionics_product_reuse_attestations attestation
          WHERE attestation.avionics_model_id = NEW.avionics_model_id
            AND attestation.product_fingerprint = NEW.product_fingerprint
        ))
      OR
      (NEW.authorization_kind = 'same_case_grounded'
        AND EXISTS (
          SELECT 1 FROM plugin_submissions submission
          WHERE submission.id = NEW.plugin_submission_id
            AND submission.canonical_listing_id = link.aircraft_sale_listing_id
            AND submission.rendered_html_sha256 = NEW.evidence_capture_sha256
            AND submission.extracted_listing_json IS NOT NULL
            AND submission.extraction_error IS NULL
            AND instr(submission.rendered_html, link.source_notes) > 0
        )
        AND EXISTS (
          SELECT 1 FROM avionics_approved_product_graph_identities identity
          WHERE identity.avionics_model_id = NEW.avionics_model_id
        )
        AND NEW.source_revocation_count = (
          SELECT COUNT(*)
          FROM avionics_authoritative_source_origin_revocations
        ))
    )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics authorization requires the exact current link role, retained capture, and product proof');
END;

CREATE TRIGGER listing_avionics_authorizations_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_link_authorizations
BEGIN
  SELECT RAISE(ABORT, 'listing avionics authorizations are replaced, never updated');
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_link_update
AFTER UPDATE OF aircraft_sale_listing_id, avionics_model_id, quantity,
  source_notes, source_confidence, configuration_action,
  replaces_avionics_model_id
ON aircraft_sale_listing_avionics
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE listing_link_id = NEW.id;
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_reuse_delete
AFTER DELETE ON avionics_product_reuse_attestations
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'manufacturer_reuse'
    AND avionics_model_id = OLD.avionics_model_id;
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_model_proof_update
AFTER UPDATE OF avionics_manufacturer_id, name, normalized_name, catalog_status,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text
ON avionics_models
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = OLD.id;
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_model_type_insert
AFTER INSERT ON avionics_model_types
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = NEW.avionics_model_id;
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_model_type_delete
AFTER DELETE ON avionics_model_types
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = OLD.avionics_model_id;
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_model_type_update
AFTER UPDATE OF avionics_model_id, avionics_type_id ON avionics_model_types
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (OLD.avionics_model_id, NEW.avionics_model_id);
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_type_update
AFTER UPDATE OF name, normalized_name ON avionics_types
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT avionics_model_id FROM avionics_model_types
      WHERE avionics_type_id = OLD.id
    );
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_graph_insert
AFTER INSERT ON avionics_approved_product_identities
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = NEW.avionics_model_id;
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_graph_delete
AFTER DELETE ON avionics_approved_product_identities
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = OLD.avionics_model_id;
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_graph_update
AFTER UPDATE OF avionics_model_id, avionics_manufacturer_identity_id,
  canonical_product_key, manufacturer_identifier_kind, canonical_identifier_key
ON avionics_approved_product_identities
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (OLD.avionics_model_id, NEW.avionics_model_id);
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_manufacturer_update
AFTER UPDATE OF name, normalized_name ON avionics_manufacturers
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT id FROM avionics_models
      WHERE avionics_manufacturer_id = OLD.id
    );
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_origin_revocation
AFTER INSERT ON avionics_authoritative_source_origin_revocations
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_grounded_capabilities;
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded';
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_capture_delete
AFTER DELETE ON plugin_submissions
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE evidence_capture_sha256 = OLD.rendered_html_sha256
    AND EXISTS (
      SELECT 1 FROM aircraft_sale_listing_avionics link
      WHERE link.id =
              aircraft_sale_listing_avionics_link_authorizations.listing_link_id
        AND link.aircraft_sale_listing_id = OLD.canonical_listing_id
        AND length(trim(COALESCE(link.source_notes, ''))) > 0
        AND instr(OLD.rendered_html, link.source_notes) > 0
        AND NOT EXISTS (
          SELECT 1 FROM plugin_submissions retained_capture
          WHERE retained_capture.canonical_listing_id =
                  link.aircraft_sale_listing_id
            AND retained_capture.rendered_html_sha256 =
                  aircraft_sale_listing_avionics_link_authorizations.evidence_capture_sha256
            AND instr(retained_capture.rendered_html, link.source_notes) > 0
        )
    );
END;

CREATE TRIGGER listing_avionics_authorizations_invalidate_capture_update
AFTER UPDATE OF canonical_listing_id, rendered_html, rendered_html_sha256,
  extracted_listing_json, extraction_error
ON plugin_submissions
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE evidence_capture_sha256 = OLD.rendered_html_sha256
    AND EXISTS (
      SELECT 1 FROM aircraft_sale_listing_avionics link
      WHERE link.id =
              aircraft_sale_listing_avionics_link_authorizations.listing_link_id
        AND link.aircraft_sale_listing_id = OLD.canonical_listing_id
        AND length(trim(COALESCE(link.source_notes, ''))) > 0
        AND instr(OLD.rendered_html, link.source_notes) > 0
        AND NOT EXISTS (
          SELECT 1 FROM plugin_submissions retained_capture
          WHERE retained_capture.canonical_listing_id =
                  link.aircraft_sale_listing_id
            AND retained_capture.rendered_html_sha256 =
                  aircraft_sale_listing_avionics_link_authorizations.evidence_capture_sha256
            AND instr(retained_capture.rendered_html, link.source_notes) > 0
        )
    );
  DELETE FROM aircraft_sale_listing_avionics_link_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND plugin_submission_id = OLD.id;
END;

CREATE TEMP TABLE listing_avionics_grounded_capabilities_object_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 3)
);
INSERT INTO listing_avionics_grounded_capabilities_object_guard (accepted)
SELECT CASE
  WHEN exact_rerun = 0 OR (
    (SELECT COUNT(*)
     FROM listing_avionics_grounded_capabilities_installed_objects) = 21
    AND (
      SELECT COUNT(*)
      FROM (
        SELECT type, name, definition
        FROM listing_avionics_grounded_capabilities_installed_objects
        EXCEPT
        SELECT type, name,
               replace(replace(replace(replace(
                 sql, char(9), ''), char(10), ''), char(13), ''), ' ', '')
        FROM sqlite_schema
        WHERE sql IS NOT NULL
          AND (
            (type = 'index' AND tbl_name IN (
              'aircraft_sale_listing_avionics_grounded_capabilities',
              'aircraft_sale_listing_avionics_link_authorizations'
            ))
            OR
            (type = 'trigger' AND (
              tbl_name IN (
                'aircraft_sale_listing_avionics_grounded_capabilities',
                'aircraft_sale_listing_avionics_link_authorizations'
              )
              OR instr(lower(sql),
                'aircraft_sale_listing_avionics_grounded_capabilities') > 0
              OR instr(lower(sql),
                'aircraft_sale_listing_avionics_link_authorizations') > 0
            ))
          )
      ) changed_object
    ) = 0
    AND (
      SELECT COUNT(*)
      FROM (
        SELECT type, name,
               replace(replace(replace(replace(
                 sql, char(9), ''), char(10), ''), char(13), ''), ' ', '')
                 AS definition
        FROM sqlite_schema
        WHERE sql IS NOT NULL
          AND (
            (type = 'index' AND tbl_name IN (
              'aircraft_sale_listing_avionics_grounded_capabilities',
              'aircraft_sale_listing_avionics_link_authorizations'
            ))
            OR
            (type = 'trigger' AND (
              tbl_name IN (
                'aircraft_sale_listing_avionics_grounded_capabilities',
                'aircraft_sale_listing_avionics_link_authorizations'
              )
              OR instr(lower(sql),
                'aircraft_sale_listing_avionics_grounded_capabilities') > 0
              OR instr(lower(sql),
                'aircraft_sale_listing_avionics_link_authorizations') > 0
            ))
          )
        EXCEPT
        SELECT type, name, definition
        FROM listing_avionics_grounded_capabilities_installed_objects
      ) changed_object
    ) = 0
  ) THEN 3 ELSE 0
END
FROM listing_avionics_grounded_capabilities_rerun_state;
DROP TABLE listing_avionics_grounded_capabilities_object_guard;
DROP TABLE listing_avionics_grounded_capabilities_installed_objects;
DROP TABLE listing_avionics_grounded_capabilities_rerun_state;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260825_listing_avionics_grounded_capabilities',
  1,
  'e29dd6062dca13f4b97cdc9a78666bf407ec791d78e7a858c4ae09333fcf677e',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
