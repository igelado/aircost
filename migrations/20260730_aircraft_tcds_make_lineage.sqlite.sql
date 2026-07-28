-- FAA/TCDS-scoped legal-make lineage.
--
-- A registry manufacturer label is not a global catalog alias.  This binding
-- can relate it to an existing canonical make only for one exact FAA release,
-- aircraft reference code, certified model, TCDS, and manufacturer-serial
-- interval.  The first binding is an approved curation decision backed by the
-- exact imported FAA identity plus distinct TCDS identity/applicability claims;
-- subsequent listings may reuse only an exact matching immutable binding.

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

CREATE TEMP TABLE aircraft_tcds_make_lineage_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO aircraft_tcds_make_lineage_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260730_aircraft_tcds_make_lineage'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260730_aircraft_tcds_make_lineage'
      AND contract_version = 1
      AND contract_fingerprint =
        '566485027d3df81bb5a90abcc0ce2b707e565bcbdc92ae3f007f527832fae735'
  ) THEN 1
  ELSE 0
END;
DROP TABLE aircraft_tcds_make_lineage_migration_guard;

CREATE UNIQUE INDEX IF NOT EXISTS idx_faa_registry_aircraft_lineage_record
  ON faa_registry_aircraft (
    snapshot_id,
    n_number,
    source_record_sha256,
    manufacturer_serial_key,
    aircraft_code
  );

CREATE TABLE IF NOT EXISTS aircraft_tcds_make_lineage_bindings (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  faa_snapshot_date TEXT NOT NULL,
  faa_archive_sha256 TEXT NOT NULL,
  faa_aircraft_code TEXT NOT NULL,
  representative_faa_registry_snapshot_id INTEGER NOT NULL,
  representative_faa_n_number TEXT NOT NULL,
  representative_faa_source_record_sha256 TEXT NOT NULL,
  representative_faa_manufacturer_serial_key TEXT NOT NULL,
  faa_manufacturer_name TEXT NOT NULL,
  faa_model TEXT NOT NULL,
  aircraft_make_id INTEGER NOT NULL
    REFERENCES aircraft_makes(id) ON DELETE RESTRICT,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE RESTRICT,
  tcds_number TEXT NOT NULL,
  tcds_document_guid TEXT NOT NULL,
  tcds_pdf_sha256 TEXT NOT NULL,
  tcds_former_holder_name TEXT NOT NULL,
  tcds_current_holder_name TEXT NOT NULL,
  tcds_manufacturer_name TEXT,
  tcds_selection_basis TEXT NOT NULL CHECK (
    tcds_selection_basis IN (
      'registry_reference',
      'drs_unique_current_exact_model',
      'operator_validated_exact_model_serial'
    )
  ),
  serial_scope_kind TEXT NOT NULL
    CHECK (serial_scope_kind IN ('tcds_model', 'manufacturer')),
  serial_prefix TEXT NOT NULL,
  serial_digits_width INTEGER NOT NULL,
  first_serial_number INTEGER NOT NULL,
  last_serial_number INTEGER,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  faa_make_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  tcds_model_identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  tcds_serial_applicability_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  tcds_holder_transfer_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  tcds_manufacturer_range_evidence_claim_id INTEGER
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (
    representative_faa_registry_snapshot_id, faa_aircraft_code
  ) REFERENCES faa_registry_aircraft_references(snapshot_id, aircraft_code)
    ON DELETE RESTRICT,
  FOREIGN KEY (
    representative_faa_registry_snapshot_id,
    representative_faa_n_number,
    representative_faa_source_record_sha256,
    representative_faa_manufacturer_serial_key,
    faa_aircraft_code
  ) REFERENCES faa_registry_aircraft (
    snapshot_id,
    n_number,
    source_record_sha256,
    manufacturer_serial_key,
    aircraft_code
  ) ON DELETE RESTRICT,
  CHECK (
    faa_snapshot_date
      GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
  ),
  CHECK (
    length(faa_archive_sha256) = 64
    AND faa_archive_sha256 NOT GLOB '*[^0-9a-f]*'
  ),
  CHECK (length(trim(faa_aircraft_code)) > 0),
  CHECK (
    substr(representative_faa_n_number, 1, 1) = 'N'
    AND length(representative_faa_n_number) BETWEEN 2 AND 6
  ),
  CHECK (
    length(representative_faa_source_record_sha256) = 64
    AND representative_faa_source_record_sha256 NOT GLOB '*[^0-9a-f]*'
  ),
  CHECK (
    length(representative_faa_manufacturer_serial_key) > 0
    AND representative_faa_manufacturer_serial_key =
      upper(representative_faa_manufacturer_serial_key)
  ),
  CHECK (
    length(trim(faa_manufacturer_name)) > 0
    AND faa_manufacturer_name = trim(faa_manufacturer_name)
  ),
  CHECK (length(trim(faa_model)) > 0 AND faa_model = trim(faa_model)),
  CHECK (length(trim(tcds_number)) > 0 AND tcds_number = trim(tcds_number)),
  CHECK (
    length(tcds_document_guid) = 36
    AND length(replace(tcds_document_guid, '-', '')) = 32
    AND substr(tcds_document_guid, 9, 1) = '-'
    AND substr(tcds_document_guid, 14, 1) = '-'
    AND substr(tcds_document_guid, 19, 1) = '-'
    AND substr(tcds_document_guid, 24, 1) = '-'
    AND replace(tcds_document_guid, '-', '') NOT GLOB '*[^0-9A-Fa-f]*'
  ),
  CHECK (
    length(tcds_pdf_sha256) = 64
    AND tcds_pdf_sha256 NOT GLOB '*[^0-9a-f]*'
  ),
  CHECK (
    length(trim(tcds_former_holder_name)) > 0
    AND tcds_former_holder_name = trim(tcds_former_holder_name)
    AND length(trim(tcds_current_holder_name)) > 0
    AND tcds_current_holder_name = trim(tcds_current_holder_name)
    AND tcds_former_holder_name <> tcds_current_holder_name
  ),
  CHECK (
    tcds_manufacturer_name IS NULL
    OR (
      length(trim(tcds_manufacturer_name)) > 0
      AND tcds_manufacturer_name = trim(tcds_manufacturer_name)
    )
  ),
  CHECK (
    serial_prefix = upper(serial_prefix)
    AND serial_prefix NOT GLOB '*[^A-Z]*'
    AND length(serial_prefix) <= 16
  ),
  CHECK (
    typeof(serial_digits_width) = 'integer'
    AND serial_digits_width BETWEEN 1 AND 18
  ),
  CHECK (
    typeof(first_serial_number) = 'integer'
    AND first_serial_number >= 0
  ),
  CHECK (
    last_serial_number IS NULL
    OR (
      typeof(last_serial_number) = 'integer'
      AND last_serial_number >= first_serial_number
    )
  ),
  CHECK (
    faa_make_evidence_claim_id <> tcds_model_identity_evidence_claim_id
    AND faa_make_evidence_claim_id <>
      tcds_serial_applicability_evidence_claim_id
    AND faa_make_evidence_claim_id <>
      tcds_holder_transfer_evidence_claim_id
    AND tcds_model_identity_evidence_claim_id <>
      tcds_serial_applicability_evidence_claim_id
    AND tcds_model_identity_evidence_claim_id <>
      tcds_holder_transfer_evidence_claim_id
    AND tcds_serial_applicability_evidence_claim_id <>
      tcds_holder_transfer_evidence_claim_id
    AND (
      tcds_manufacturer_range_evidence_claim_id IS NULL
      OR (
        tcds_manufacturer_range_evidence_claim_id <>
          faa_make_evidence_claim_id
        AND tcds_manufacturer_range_evidence_claim_id <>
          tcds_model_identity_evidence_claim_id
        AND tcds_manufacturer_range_evidence_claim_id <>
          tcds_serial_applicability_evidence_claim_id
        AND tcds_manufacturer_range_evidence_claim_id <>
          tcds_holder_transfer_evidence_claim_id
      )
    )
    AND (
      (
        serial_scope_kind = 'tcds_model'
        AND tcds_manufacturer_name IS NULL
        AND tcds_manufacturer_range_evidence_claim_id IS NULL
      )
      OR (
        serial_scope_kind = 'manufacturer'
        AND tcds_manufacturer_name IS NOT NULL
        AND tcds_manufacturer_range_evidence_claim_id IS NOT NULL
      )
    )
  )
);

DROP INDEX IF EXISTS idx_aircraft_tcds_make_lineage_scope;
CREATE UNIQUE INDEX idx_aircraft_tcds_make_lineage_scope
  ON aircraft_tcds_make_lineage_bindings (
    faa_snapshot_date,
    faa_archive_sha256,
    faa_aircraft_code,
    faa_manufacturer_name,
    faa_model,
    serial_prefix,
    serial_digits_width,
    first_serial_number,
    coalesce(last_serial_number, -1)
  );

CREATE INDEX IF NOT EXISTS idx_aircraft_tcds_make_lineage_lookup
  ON aircraft_tcds_make_lineage_bindings (
    faa_snapshot_date,
    faa_archive_sha256,
    faa_aircraft_code,
    aircraft_designation_id,
    faa_manufacturer_name,
    faa_model,
    serial_prefix,
    serial_digits_width,
    first_serial_number,
    last_serial_number
  );

-- One approved range must be backed by the exact imported FAA record and four
-- distinct claims: FAA make, TCDS model identity, TCDS model/serial
-- applicability, and TCDS holder transfer. A manufacturer-specific serial
-- range is optional strengthening evidence. Matching names are copied
-- literally; this trigger never strips a legal suffix or manufactures a
-- semantic alias.
DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_requires_provenance;
CREATE TRIGGER aircraft_tcds_make_lineage_requires_provenance
BEFORE INSERT ON aircraft_tcds_make_lineage_bindings
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims faa_link
    ON faa_link.decision_id = decision.id
   AND faa_link.evidence_claim_id = NEW.faa_make_evidence_claim_id
   AND faa_link.evidence_role = 'identity'
  JOIN aircraft_identity_decision_claims tcds_model_link
    ON tcds_model_link.decision_id = decision.id
   AND tcds_model_link.evidence_claim_id =
       NEW.tcds_model_identity_evidence_claim_id
   AND tcds_model_link.evidence_role = 'identity'
  JOIN aircraft_identity_decision_claims tcds_serial_link
    ON tcds_serial_link.decision_id = decision.id
   AND tcds_serial_link.evidence_claim_id =
       NEW.tcds_serial_applicability_evidence_claim_id
   AND tcds_serial_link.evidence_role = 'applicability'
  JOIN aircraft_identity_decision_claims tcds_holder_link
    ON tcds_holder_link.decision_id = decision.id
   AND tcds_holder_link.evidence_claim_id =
       NEW.tcds_holder_transfer_evidence_claim_id
   AND tcds_holder_link.evidence_role = 'identity'
  JOIN curation_evidence_claims faa_claim
    ON faa_claim.id = NEW.faa_make_evidence_claim_id
  JOIN curation_evidence_sources faa_source
    ON faa_source.id = faa_claim.evidence_source_id
  JOIN curation_evidence_claims tcds_model_claim
    ON tcds_model_claim.id = NEW.tcds_model_identity_evidence_claim_id
  JOIN curation_evidence_sources tcds_model_source
    ON tcds_model_source.id = tcds_model_claim.evidence_source_id
  JOIN curation_evidence_claims tcds_serial_claim
    ON tcds_serial_claim.id =
       NEW.tcds_serial_applicability_evidence_claim_id
  JOIN curation_evidence_sources tcds_serial_source
    ON tcds_serial_source.id = tcds_serial_claim.evidence_source_id
  JOIN curation_evidence_claims tcds_holder_claim
    ON tcds_holder_claim.id = NEW.tcds_holder_transfer_evidence_claim_id
  JOIN curation_evidence_sources tcds_holder_source
    ON tcds_holder_source.id = tcds_holder_claim.evidence_source_id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = NEW.representative_faa_registry_snapshot_id
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = snapshot.id
   AND reference.aircraft_code = NEW.faa_aircraft_code
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  JOIN aircraft_model_families family
    ON family.id = designation.aircraft_model_family_id
   AND family.aircraft_make_id = NEW.aircraft_make_id
  JOIN aircraft_makes canonical_make
    ON canonical_make.id = family.aircraft_make_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  WHERE decision.id = NEW.approval_decision_id
    AND decision.entity_kind = 'make'
    AND decision.decision_action = 'match_existing'
    AND decision.decision_status = 'approved'
    AND decision.deterministic_validation_passed = 1
    AND decision.selected_entity_id = NEW.aircraft_make_id
    AND faa_claim.claim_kind = 'identity'
    AND faa_claim.validation_status = 'validated'
    AND faa_source.id = snapshot.evidence_source_id
    AND faa_source.source_tier = 'regulator_primary'
    AND tcds_model_claim.claim_kind = 'identity'
    AND tcds_model_claim.validation_status = 'validated'
    AND tcds_model_source.source_tier = 'regulator_primary'
    AND tcds_model_source.content_sha256 = NEW.tcds_pdf_sha256
    AND tcds_serial_claim.claim_kind = 'applicability'
    AND tcds_serial_claim.validation_status = 'validated'
    AND tcds_serial_source.id = tcds_model_source.id
    AND tcds_holder_claim.claim_kind = 'identity'
    AND tcds_holder_claim.validation_status = 'validated'
    AND tcds_holder_source.id = tcds_model_source.id
    AND tcds_model_source.source_url =
      'https://drs.faa.gov/api/drs/data-pull/download/'
      || NEW.tcds_document_guid
    AND (
      NEW.tcds_manufacturer_range_evidence_claim_id IS NULL
      OR (
        EXISTS (
          SELECT 1
          FROM aircraft_identity_decision_claims manufacturer_link
          JOIN curation_evidence_claims manufacturer_claim
            ON manufacturer_claim.id = manufacturer_link.evidence_claim_id
          WHERE manufacturer_link.decision_id = decision.id
            AND manufacturer_link.evidence_claim_id =
              NEW.tcds_manufacturer_range_evidence_claim_id
            AND manufacturer_link.evidence_role = 'applicability'
            AND manufacturer_claim.claim_kind = 'applicability'
            AND manufacturer_claim.validation_status = 'validated'
            AND manufacturer_claim.evidence_source_id = tcds_model_source.id
        )
      )
    )
    AND NEW.faa_snapshot_date = snapshot.snapshot_date
    AND NEW.faa_archive_sha256 = snapshot.archive_sha256
    AND NEW.faa_manufacturer_name = reference.manufacturer_name
    AND NEW.faa_model = reference.model_name
    AND (
      (
        NEW.tcds_selection_basis = 'registry_reference'
        AND length(trim(coalesce(reference.type_certificate_data_sheet, ''))) > 0
        AND NEW.tcds_number = trim(reference.type_certificate_data_sheet)
      )
      OR (
        NEW.tcds_selection_basis IN (
          'drs_unique_current_exact_model',
          'operator_validated_exact_model_serial'
        )
        AND length(trim(coalesce(reference.type_certificate_data_sheet, ''))) = 0
      )
    )
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(rtrim(trim(canonical_make.name), '.')) =
        lower(rtrim(trim(NEW.tcds_former_holder_name), '.'))
      OR lower(rtrim(trim(canonical_make.name), '.')) =
        lower(rtrim(trim(NEW.tcds_current_holder_name), '.'))
    )
    AND (
      NEW.tcds_manufacturer_name IS NULL
      OR lower(rtrim(trim(NEW.tcds_manufacturer_name), '.')) =
          lower(rtrim(trim(NEW.tcds_former_holder_name), '.'))
      OR lower(rtrim(trim(NEW.tcds_manufacturer_name), '.')) =
          lower(rtrim(trim(NEW.tcds_current_holder_name), '.'))
    )
    AND EXISTS (
      SELECT 1
      FROM faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = snapshot.id
        AND aircraft.n_number = NEW.representative_faa_n_number
        AND aircraft.source_record_sha256 =
          NEW.representative_faa_source_record_sha256
        AND aircraft.manufacturer_serial_key =
          NEW.representative_faa_manufacturer_serial_key
        AND aircraft.aircraft_code = NEW.faa_aircraft_code
        AND aircraft.manufacturer_serial_key IS NOT NULL
        AND length(aircraft.manufacturer_serial_key) =
          length(NEW.serial_prefix) + NEW.serial_digits_width
        AND substr(
          aircraft.manufacturer_serial_key, 1, length(NEW.serial_prefix)
        ) = NEW.serial_prefix
        AND substr(
          aircraft.manufacturer_serial_key, length(NEW.serial_prefix) + 1
        ) NOT GLOB '*[^0-9]*'
        AND CAST(substr(
          aircraft.manufacturer_serial_key, length(NEW.serial_prefix) + 1
        ) AS INTEGER) >= NEW.first_serial_number
        AND (
          NEW.last_serial_number IS NULL
          OR CAST(substr(
            aircraft.manufacturer_serial_key, length(NEW.serial_prefix) + 1
          ) AS INTEGER) <= NEW.last_serial_number
        )
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'FAA/TCDS make lineage requires distinct FAA make, TCDS model, serial-applicability, and holder-transfer evidence'
  );
END;

DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_no_overlap;
CREATE TRIGGER aircraft_tcds_make_lineage_no_overlap
BEFORE INSERT ON aircraft_tcds_make_lineage_bindings
WHEN EXISTS (
  SELECT 1
  FROM aircraft_tcds_make_lineage_bindings existing
  WHERE existing.faa_snapshot_date = NEW.faa_snapshot_date
    AND existing.faa_archive_sha256 = NEW.faa_archive_sha256
    AND existing.faa_aircraft_code = NEW.faa_aircraft_code
    AND existing.faa_manufacturer_name = NEW.faa_manufacturer_name
    AND existing.faa_model = NEW.faa_model
    AND existing.serial_prefix = NEW.serial_prefix
    AND existing.serial_digits_width = NEW.serial_digits_width
    AND (
      existing.last_serial_number IS NULL
      OR existing.last_serial_number >= NEW.first_serial_number
    )
    AND (
      NEW.last_serial_number IS NULL
      OR NEW.last_serial_number >= existing.first_serial_number
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'FAA/TCDS make-lineage serial ranges cannot overlap'
  );
END;

DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_no_catalog_collision;
CREATE TRIGGER aircraft_tcds_make_lineage_no_catalog_collision
BEFORE INSERT ON aircraft_tcds_make_lineage_bindings
WHEN EXISTS (
  SELECT 1
  FROM aircraft_makes other_make
  WHERE other_make.id <> NEW.aircraft_make_id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(other_make.name), ' ', ''), '-', ''), '.', ''),
      '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')',
      '')) =
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(NEW.faa_manufacturer_name), ' ', ''), '-', ''),
        '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
        '(', ''), ')', ''))
)
OR EXISTS (
  SELECT 1
  FROM aircraft_make_aliases alias
  WHERE alias.aircraft_make_id <> NEW.aircraft_make_id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/',
      ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      = lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(NEW.faa_manufacturer_name), ' ', ''), '-', ''),
        '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
        '(', ''), ')', ''))
)
BEGIN
  SELECT RAISE(
    ABORT,
    'FAA/TCDS make lineage collides with another canonical make or alias'
  );
END;

DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_immutable_update;
CREATE TRIGGER aircraft_tcds_make_lineage_immutable_update
BEFORE UPDATE ON aircraft_tcds_make_lineage_bindings
BEGIN
  SELECT RAISE(ABORT, 'approved FAA/TCDS make-lineage bindings are immutable');
END;

DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_immutable_delete;
CREATE TRIGGER aircraft_tcds_make_lineage_immutable_delete
BEFORE DELETE ON aircraft_tcds_make_lineage_bindings
BEGIN
  SELECT RAISE(ABORT, 'approved FAA/TCDS make-lineage bindings are immutable');
END;

DROP TRIGGER IF EXISTS aircraft_make_tcds_lineage_collision_insert;
CREATE TRIGGER aircraft_make_tcds_lineage_collision_insert
BEFORE INSERT ON aircraft_makes
WHEN EXISTS (
  SELECT 1
  FROM aircraft_tcds_make_lineage_bindings binding
  WHERE lower(replace(replace(replace(replace(replace(replace(replace(replace(
    replace(replace(trim(NEW.name), ' ', ''), '-', ''), '.', ''), '/', ''),
    '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', '')) =
    lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(binding.faa_manufacturer_name), ' ', ''), '-', ''),
      '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(',
      ''), ')', ''))
)
BEGIN
  SELECT RAISE(
    ABORT,
    'canonical aircraft make collides with an approved FAA/TCDS lineage label'
  );
END;

DROP TRIGGER IF EXISTS aircraft_make_tcds_lineage_collision_update;
CREATE TRIGGER aircraft_make_tcds_lineage_collision_update
BEFORE UPDATE OF name, normalized_name ON aircraft_makes
WHEN EXISTS (
  SELECT 1
  FROM aircraft_tcds_make_lineage_bindings binding
  WHERE binding.aircraft_make_id <> OLD.id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(NEW.name), ' ', ''), '-', ''), '.', ''), '/', ''),
      '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', '')) =
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(binding.faa_manufacturer_name), ' ', ''), '-',
        ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
        '(', ''), ')', ''))
)
BEGIN
  SELECT RAISE(
    ABORT,
    'canonical aircraft make collides with an approved FAA/TCDS lineage label'
  );
END;

DROP TRIGGER IF EXISTS aircraft_make_alias_tcds_lineage_collision;
CREATE TRIGGER aircraft_make_alias_tcds_lineage_collision
BEFORE INSERT ON aircraft_make_aliases
WHEN EXISTS (
  SELECT 1
  FROM aircraft_tcds_make_lineage_bindings binding
  WHERE binding.aircraft_make_id <> NEW.aircraft_make_id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(NEW.alias), ' ', ''), '-', ''), '.', ''), '/', ''),
      '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', '')) =
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(binding.faa_manufacturer_name), ' ', ''), '-',
        ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
        '(', ''), ')', ''))
)
BEGIN
  SELECT RAISE(
    ABORT,
    'aircraft make alias collides with an approved FAA/TCDS lineage label'
  );
END;

-- The three admission barriers below replace their year-alias-only versions.
-- Every TCDS branch repeats the exact FAA release/code/model/manufacturer and
-- serial-range match; possession of an unrelated binding is never sufficient.

DROP TRIGGER IF EXISTS aircraft_designation_faa_binding_requires_provenance;
CREATE TRIGGER aircraft_designation_faa_binding_requires_provenance
BEFORE INSERT ON aircraft_designation_faa_bindings
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_designations designation
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN aircraft_model_families family
    ON family.id = designation.aircraft_model_family_id
  JOIN aircraft_makes make
    ON make.id = family.aircraft_make_id
  JOIN aircraft_identity_decisions decision
    ON decision.id = designation.approval_decision_id
  JOIN curation_evidence_claims claim
    ON claim.id = NEW.identity_evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = NEW.representative_faa_registry_snapshot_id
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = snapshot.id
   AND reference.aircraft_code = NEW.faa_aircraft_code
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  WHERE designation.id = NEW.aircraft_designation_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'designation'
    AND claim.claim_kind = 'identity'
    AND claim.validation_status = 'validated'
    AND source.id = snapshot.evidence_source_id
    AND source.source_tier = 'regulator_primary'
    AND NEW.faa_snapshot_date = snapshot.snapshot_date
    AND NEW.faa_archive_sha256 = snapshot.archive_sha256
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(make.name), ' ', ''), '-', ''), '.', ''), '/',
        ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(
          replace(replace(trim(reference.manufacturer_name), ' ', ''), '-',
          ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39),
          ''), '(', ''), ')', ''))
      OR (
        EXISTS (
          SELECT 1
          FROM faa_registry_aircraft registered_aircraft
          WHERE registered_aircraft.snapshot_id = snapshot.id
            AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
        )
        AND NOT EXISTS (
          SELECT 1
          FROM faa_registry_aircraft registered_aircraft
          WHERE registered_aircraft.snapshot_id = snapshot.id
            AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
            AND NOT EXISTS (
              SELECT 1
              FROM aircraft_make_aliases alias
              LEFT JOIN aircraft_markets market
                ON market.id = alias.aircraft_market_id
              WHERE alias.aircraft_make_id = make.id
                AND lower(replace(replace(replace(replace(replace(replace(
                  replace(replace(replace(replace(trim(alias.alias), ' ', ''),
                  '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''),
                  char(39), ''), '(', ''), ')', '')) =
                  lower(replace(replace(replace(replace(replace(replace(
                    replace(replace(replace(replace(
                    trim(reference.manufacturer_name), ' ', ''), '-', ''), '.',
                    ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
                    '(', ''), ')', ''))
                AND (
                  alias.aircraft_market_id IS NULL
                  OR market.code IN ('GLOBAL', 'US')
                )
                AND (
                  (
                    registered_aircraft.year_manufactured IS NULL
                    AND alias.valid_from_model_year IS NULL
                    AND alias.valid_to_model_year IS NULL
                  )
                  OR (
                    registered_aircraft.year_manufactured IS NOT NULL
                    AND (
                      alias.valid_from_model_year IS NULL
                      OR alias.valid_from_model_year <=
                         registered_aircraft.year_manufactured
                    )
                    AND (
                      alias.valid_to_model_year IS NULL
                      OR alias.valid_to_model_year >=
                         registered_aircraft.year_manufactured
                    )
                  )
                )
            )
        )
      )
      OR EXISTS (
        SELECT 1
        FROM aircraft_tcds_make_lineage_bindings binding
        JOIN faa_registry_aircraft registered_aircraft
          ON registered_aircraft.snapshot_id = snapshot.id
         AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
        WHERE binding.faa_snapshot_date = NEW.faa_snapshot_date
          AND binding.faa_archive_sha256 = NEW.faa_archive_sha256
          AND binding.faa_aircraft_code = NEW.faa_aircraft_code
          AND binding.faa_manufacturer_name = reference.manufacturer_name
          AND binding.faa_model = reference.model_name
          AND binding.aircraft_make_id = make.id
          AND binding.aircraft_designation_id = designation.id
          AND registered_aircraft.manufacturer_serial_key IS NOT NULL
          AND length(registered_aircraft.manufacturer_serial_key) =
            length(binding.serial_prefix) + binding.serial_digits_width
          AND substr(
            registered_aircraft.manufacturer_serial_key,
            1,
            length(binding.serial_prefix)
          ) = binding.serial_prefix
          AND substr(
            registered_aircraft.manufacturer_serial_key,
            length(binding.serial_prefix) + 1
          ) NOT GLOB '*[^0-9]*'
          AND CAST(substr(
            registered_aircraft.manufacturer_serial_key,
            length(binding.serial_prefix) + 1
          ) AS INTEGER) >= binding.first_serial_number
          AND (
            binding.last_serial_number IS NULL
            OR CAST(substr(
              registered_aircraft.manufacturer_serial_key,
              length(binding.serial_prefix) + 1
            ) AS INTEGER) <= binding.last_serial_number
          )
      )
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'FAA aircraft code binding requires an exact approved designation, applicable manufacturer identity, and regulator evidence'
  );
END;

DROP TRIGGER IF EXISTS listing_identity_assignment_requires_faa_identity;
CREATE TRIGGER listing_identity_assignment_requires_faa_identity
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN faa_registry_aircraft aircraft
    ON aircraft.snapshot_id = NEW.faa_registry_snapshot_id
   AND aircraft.n_number = NEW.faa_n_number
   AND aircraft.source_record_sha256 = NEW.faa_source_record_sha256
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = aircraft.snapshot_id
   AND reference.aircraft_code = aircraft.aircraft_code
  JOIN faa_registry_snapshots registry_snapshot
    ON registry_snapshot.id = aircraft.snapshot_id
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN aircraft_designation_faa_bindings faa_binding
    ON faa_binding.faa_snapshot_date = registry_snapshot.snapshot_date
   AND faa_binding.faa_archive_sha256 = registry_snapshot.archive_sha256
   AND faa_binding.faa_aircraft_code = aircraft.aircraft_code
   AND faa_binding.aircraft_designation_id = designation.id
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  JOIN aircraft_makes make
    ON make.id = NEW.aircraft_make_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
    AND upper(replace(replace(trim(listing.registration_number), '-', ''), ' ', ''))
      = NEW.faa_n_number
    AND length(trim(reference.manufacturer_name)) > 0
    AND length(trim(reference.model_name)) > 0
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(make.name), ' ', ''), '-', ''), '.', ''), '/',
        ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(
          replace(replace(trim(reference.manufacturer_name), ' ', ''), '-',
          ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39),
          ''), '(', ''), ')', ''))
      OR EXISTS (
        SELECT 1
        FROM aircraft_make_aliases alias
        LEFT JOIN aircraft_markets market
          ON market.id = alias.aircraft_market_id
        WHERE alias.aircraft_make_id = make.id
          AND lower(replace(replace(replace(replace(replace(replace(replace(
            replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.',
            ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(',
            ''), ')', '')) =
            lower(replace(replace(replace(replace(replace(replace(replace(
              replace(replace(replace(trim(reference.manufacturer_name), ' ',
              ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''),
              char(39), ''), '(', ''), ')', ''))
          AND (
            alias.aircraft_market_id IS NULL
            OR market.code IN ('GLOBAL', 'US')
          )
          AND (
            alias.valid_from_model_year IS NULL
            OR alias.valid_from_model_year <= listing.model_year
          )
          AND (
            alias.valid_to_model_year IS NULL
            OR alias.valid_to_model_year >= listing.model_year
          )
      )
      OR EXISTS (
        SELECT 1
        FROM aircraft_tcds_make_lineage_bindings binding
        WHERE binding.faa_snapshot_date = registry_snapshot.snapshot_date
          AND binding.faa_archive_sha256 = registry_snapshot.archive_sha256
          AND binding.faa_aircraft_code = aircraft.aircraft_code
          AND binding.faa_manufacturer_name = reference.manufacturer_name
          AND binding.faa_model = reference.model_name
          AND binding.aircraft_make_id = make.id
          AND binding.aircraft_designation_id = designation.id
          AND aircraft.manufacturer_serial_key IS NOT NULL
          AND length(aircraft.manufacturer_serial_key) =
            length(binding.serial_prefix) + binding.serial_digits_width
          AND substr(
            aircraft.manufacturer_serial_key, 1, length(binding.serial_prefix)
          ) = binding.serial_prefix
          AND substr(
            aircraft.manufacturer_serial_key, length(binding.serial_prefix) + 1
          ) NOT GLOB '*[^0-9]*'
          AND CAST(substr(
            aircraft.manufacturer_serial_key, length(binding.serial_prefix) + 1
          ) AS INTEGER) >= binding.first_serial_number
          AND (
            binding.last_serial_number IS NULL
            OR CAST(substr(
              aircraft.manufacturer_serial_key,
              length(binding.serial_prefix) + 1
            ) AS INTEGER) <= binding.last_serial_number
          )
      )
    )
    AND designation_key.identity_key = reference_key.identity_key
)
BEGIN
  SELECT RAISE(
    ABORT,
    'listing aircraft assignment designation does not match the exact FAA aircraft identity'
  );
END;

DROP TRIGGER IF EXISTS listing_ready_requires_canonical_aircraft_update;
CREATE TRIGGER listing_ready_requires_canonical_aircraft_update
BEFORE UPDATE OF
  ingestion_state,
  aircraft_model_variant_id,
  model_year,
  registration_number,
  serial_number
ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready' AND NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_current_identity_assignments current_assignment
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = NEW.id
  JOIN aircraft_makes canonical_make
    ON canonical_make.id = assignment.aircraft_make_id
  JOIN aircraft_designations canonical_designation
    ON canonical_designation.id = assignment.aircraft_designation_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = canonical_designation.id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = assignment.faa_registry_snapshot_id
  JOIN faa_registry_aircraft aircraft
    ON aircraft.snapshot_id = snapshot.id
   AND aircraft.n_number = assignment.faa_n_number
   AND aircraft.source_record_sha256 = assignment.faa_source_record_sha256
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = aircraft.snapshot_id
   AND reference.aircraft_code = aircraft.aircraft_code
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  JOIN aircraft_designation_faa_bindings faa_binding
    ON faa_binding.faa_snapshot_date = snapshot.snapshot_date
   AND faa_binding.faa_archive_sha256 = snapshot.archive_sha256
   AND faa_binding.faa_aircraft_code = aircraft.aircraft_code
   AND faa_binding.aircraft_designation_id =
       assignment.aircraft_designation_id
  WHERE current_assignment.aircraft_sale_listing_id = NEW.id
    AND EXISTS (
      SELECT 1
      FROM faa_registry_snapshots latest_release
      WHERE latest_release.id = (
        SELECT id
        FROM faa_registry_snapshots
        ORDER BY snapshot_date DESC, id DESC
        LIMIT 1
      )
        AND latest_release.snapshot_date = snapshot.snapshot_date
        AND latest_release.archive_sha256 = snapshot.archive_sha256
    )
    AND upper(replace(replace(trim(NEW.registration_number), '-', ''), ' ', ''))
      = assignment.faa_n_number
    AND (
      NEW.serial_number IS NULL
      OR trim(NEW.serial_number) = ''
      OR aircraft.manufacturer_serial_raw IS NULL
      OR upper(replace(replace(trim(NEW.serial_number), '-', ''), ' ', '')) =
         upper(replace(replace(
           trim(aircraft.manufacturer_serial_raw), '-', ''
         ), ' ', ''))
    )
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(canonical_make.name), ' ', ''), '-', ''), '.',
        ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''),
        ')', '')) =
        lower(replace(replace(replace(replace(replace(replace(replace(replace(
          replace(replace(trim(reference.manufacturer_name), ' ', ''), '-',
          ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39),
          ''), '(', ''), ')', ''))
      OR EXISTS (
        SELECT 1
        FROM aircraft_make_aliases alias
        LEFT JOIN aircraft_markets market
          ON market.id = alias.aircraft_market_id
        WHERE alias.aircraft_make_id = canonical_make.id
          AND lower(replace(replace(replace(replace(replace(replace(replace(
            replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.',
            ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(',
            ''), ')', '')) =
            lower(replace(replace(replace(replace(replace(replace(replace(
              replace(replace(replace(trim(reference.manufacturer_name), ' ',
              ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''),
              char(39), ''), '(', ''), ')', ''))
          AND (
            alias.aircraft_market_id IS NULL
            OR market.code IN ('GLOBAL', 'US')
          )
          AND (
            alias.valid_from_model_year IS NULL
            OR alias.valid_from_model_year <= NEW.model_year
          )
          AND (
            alias.valid_to_model_year IS NULL
            OR alias.valid_to_model_year >= NEW.model_year
          )
      )
      OR EXISTS (
        SELECT 1
        FROM aircraft_tcds_make_lineage_bindings binding
        WHERE binding.faa_snapshot_date = snapshot.snapshot_date
          AND binding.faa_archive_sha256 = snapshot.archive_sha256
          AND binding.faa_aircraft_code = aircraft.aircraft_code
          AND binding.faa_manufacturer_name = reference.manufacturer_name
          AND binding.faa_model = reference.model_name
          AND binding.aircraft_make_id = canonical_make.id
          AND binding.aircraft_designation_id =
              canonical_designation.id
          AND aircraft.manufacturer_serial_key IS NOT NULL
          AND length(aircraft.manufacturer_serial_key) =
            length(binding.serial_prefix) + binding.serial_digits_width
          AND substr(
            aircraft.manufacturer_serial_key, 1, length(binding.serial_prefix)
          ) = binding.serial_prefix
          AND substr(
            aircraft.manufacturer_serial_key, length(binding.serial_prefix) + 1
          ) NOT GLOB '*[^0-9]*'
          AND CAST(substr(
            aircraft.manufacturer_serial_key, length(binding.serial_prefix) + 1
          ) AS INTEGER) >= binding.first_serial_number
          AND (
            binding.last_serial_number IS NULL
            OR CAST(substr(
              aircraft.manufacturer_serial_key,
              length(binding.serial_prefix) + 1
            ) AS INTEGER) <= binding.last_serial_number
          )
      )
    )
    AND (
      (
        assignment.aircraft_generation_id IS NULL
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_generation_designations generation_link
          WHERE generation_link.aircraft_designation_id =
                assignment.aircraft_designation_id
        )
      )
      OR (
        assignment.aircraft_generation_id IS NOT NULL
        AND EXISTS (
          SELECT 1
          FROM aircraft_generation_designations generation_link
          WHERE generation_link.aircraft_generation_id =
                assignment.aircraft_generation_id
            AND generation_link.aircraft_designation_id =
                assignment.aircraft_designation_id
        )
      )
    )
    AND (
      (
        assignment.aircraft_factory_package_id IS NULL
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_package_applicability applicability
          JOIN aircraft_factory_packages package
            ON package.id = applicability.aircraft_factory_package_id
          WHERE applicability.aircraft_designation_id =
                assignment.aircraft_designation_id
            AND package.package_kind = 'trim_tier'
            AND (
              applicability.aircraft_generation_id IS NULL
              OR applicability.aircraft_generation_id =
                 assignment.aircraft_generation_id
            )
            AND (
              applicability.valid_from_model_year IS NULL
              OR applicability.valid_from_model_year <= NEW.model_year
            )
            AND (
              applicability.valid_to_model_year IS NULL
              OR applicability.valid_to_model_year >= NEW.model_year
            )
        )
      )
      OR (
        assignment.aircraft_factory_package_id IS NOT NULL
        AND EXISTS (
          SELECT 1
          FROM aircraft_package_applicability applicability
          WHERE applicability.aircraft_factory_package_id =
                assignment.aircraft_factory_package_id
            AND applicability.aircraft_designation_id =
                assignment.aircraft_designation_id
            AND (
              applicability.aircraft_generation_id IS NULL
              OR applicability.aircraft_generation_id =
                 assignment.aircraft_generation_id
            )
            AND (
              applicability.valid_from_model_year IS NULL
              OR applicability.valid_from_model_year <= NEW.model_year
            )
            AND (
              applicability.valid_to_model_year IS NULL
              OR applicability.valid_to_model_year >= NEW.model_year
            )
        )
      )
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'ready listing requires a current canonical aircraft assignment matching current FAA identity'
  );
END;

INSERT INTO schema_migration_contracts (
  migration_name,
  contract_version,
  contract_fingerprint,
  installed_at
) VALUES (
  '20260730_aircraft_tcds_make_lineage',
  1,
  '566485027d3df81bb5a90abcc0ce2b707e565bcbdc92ae3f007f527832fae735',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = excluded.contract_version,
  contract_fingerprint = excluded.contract_fingerprint,
  installed_at = excluded.installed_at;

COMMIT;
PRAGMA foreign_key_check;
