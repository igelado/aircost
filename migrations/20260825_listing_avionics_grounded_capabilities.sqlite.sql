PRAGMA foreign_keys = OFF;
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
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
  )
  OR EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260825_listing_avionics_grounded_capabilities'
      AND contract_version = 1
      AND contract_fingerprint =
        'a7a249e910f4c16530760d18786f106f11f3b36a25c6a3e80fa8adacd1b79b31'
  ) THEN 1
  ELSE 0
END;
DROP TABLE listing_avionics_grounded_capabilities_migration_guard;

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
BEGIN
  SELECT RAISE(ABORT, 'grounded avionics capability requires its exact current capture-bound listing and approved product');
END;

DROP TRIGGER IF EXISTS listing_avionics_grounded_capabilities_immutable_update;
CREATE TRIGGER listing_avionics_grounded_capabilities_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_grounded_capabilities
BEGIN
  SELECT RAISE(ABORT, 'grounded avionics capabilities are immutable');
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260825_listing_avionics_grounded_capabilities',
  1,
  'a7a249e910f4c16530760d18786f106f11f3b36a25c6a3e80fa8adacd1b79b31',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
PRAGMA foreign_keys = ON;
