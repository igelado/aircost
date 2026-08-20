-- Move legacy factory-default claims whose avionics product is not approved
-- into a pending-only table. Pending claims are never valuation inputs.
-- Rejection deletes the pending row. Explicit admission inserts the exact claim
-- into the canonical table after product approval; the admission trigger then
-- removes the pending row in the same transaction.

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

DROP TABLE IF EXISTS temp.default_avionics_candidate_migration_guard;
CREATE TEMP TABLE default_avionics_candidate_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO default_avionics_candidate_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260802_default_avionics_candidate_quarantine'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260802_default_avionics_candidate_quarantine'
      AND (
        (
          contract_version = 1
          AND contract_fingerprint =
            'b50683c27b244cadf3cf88b226665f79051f678df9b30e0d01d0ca261464581f'
        )
        OR (
          contract_version = 2
          AND contract_fingerprint =
            'b8a6ecd15acc0ce14f67bf37ff4387c0ded4d1c6669d2fc4698b6c0a6c209ba4'
        )
      )
  ) THEN 1
  ELSE 0
END;
DROP TABLE default_avionics_candidate_migration_guard;

CREATE TABLE IF NOT EXISTS aircraft_model_variant_default_avionics_candidates (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  quarantined_default_avionics_id INTEGER UNIQUE,
  aircraft_model_variant_id INTEGER NOT NULL
    REFERENCES aircraft_model_variants(id) ON DELETE RESTRICT,
  model_year INTEGER NOT NULL,
  avionics_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  quantity INTEGER NOT NULL,
  source_url TEXT NOT NULL,
  source_title TEXT NOT NULL,
  source_notes TEXT NOT NULL,
  source_confidence TEXT NOT NULL,
  pending_reason TEXT NOT NULL DEFAULT 'factory_default_claim_unverified',
  quarantined_created_at TEXT,
  quarantined_updated_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_variant_id, model_year, avionics_model_id),
  CHECK (pending_reason IN (
    'catalog_product_unverified', 'factory_default_claim_unverified'
  )),
  CHECK (
    (
      quarantined_default_avionics_id IS NULL
      AND quarantined_created_at IS NULL
      AND quarantined_updated_at IS NULL
    )
    OR (
      quarantined_default_avionics_id > 0
      AND quarantined_created_at IS NOT NULL
      AND quarantined_updated_at IS NOT NULL
    )
  )
);

CREATE INDEX IF NOT EXISTS idx_aircraft_default_avionics_candidates_product
  ON aircraft_model_variant_default_avionics_candidates (
    avionics_model_id, aircraft_model_variant_id, model_year, id
  );

INSERT INTO aircraft_model_variant_default_avionics_candidates (
  quarantined_default_avionics_id,
  aircraft_model_variant_id,
  model_year,
  avionics_model_id,
  quantity,
  source_url,
  source_title,
  source_notes,
  source_confidence,
  pending_reason,
  quarantined_created_at,
  quarantined_updated_at,
  created_at
)
SELECT
  default_avionics.id,
  default_avionics.aircraft_model_variant_id,
  default_avionics.model_year,
  default_avionics.avionics_model_id,
  default_avionics.quantity,
  default_avionics.source_url,
  default_avionics.source_title,
  default_avionics.source_notes,
  default_avionics.source_confidence,
  'catalog_product_unverified',
  default_avionics.created_at,
  default_avionics.updated_at,
  CURRENT_TIMESTAMP
FROM aircraft_model_variant_default_avionics default_avionics
JOIN avionics_models model
  ON model.id = default_avionics.avionics_model_id
WHERE model.catalog_status <> 'approved'
ON CONFLICT (quarantined_default_avionics_id) DO NOTHING;

DELETE FROM aircraft_model_variant_default_avionics
WHERE EXISTS (
  SELECT 1
  FROM avionics_models model
  JOIN aircraft_model_variant_default_avionics_candidates candidate
    ON candidate.quarantined_default_avionics_id =
       aircraft_model_variant_default_avionics.id
   AND candidate.aircraft_model_variant_id =
       aircraft_model_variant_default_avionics.aircraft_model_variant_id
   AND candidate.model_year =
       aircraft_model_variant_default_avionics.model_year
   AND candidate.avionics_model_id =
       aircraft_model_variant_default_avionics.avionics_model_id
   AND candidate.quantity =
       aircraft_model_variant_default_avionics.quantity
   AND candidate.source_url =
       aircraft_model_variant_default_avionics.source_url
   AND candidate.source_title =
       aircraft_model_variant_default_avionics.source_title
   AND candidate.source_notes =
       aircraft_model_variant_default_avionics.source_notes
   AND candidate.source_confidence =
       aircraft_model_variant_default_avionics.source_confidence
   AND candidate.quarantined_created_at =
       aircraft_model_variant_default_avionics.created_at
   AND candidate.quarantined_updated_at =
       aircraft_model_variant_default_avionics.updated_at
  WHERE model.id = aircraft_model_variant_default_avionics.avionics_model_id
    AND model.catalog_status <> 'approved'
);

CREATE TRIGGER IF NOT EXISTS aircraft_default_avionics_candidate_active_conflict_insert
BEFORE INSERT ON aircraft_model_variant_default_avionics_candidates
WHEN EXISTS (
  SELECT 1
  FROM aircraft_model_variant_default_avionics active
  WHERE active.aircraft_model_variant_id = NEW.aircraft_model_variant_id
    AND active.model_year = NEW.model_year
    AND active.avionics_model_id = NEW.avionics_model_id
)
BEGIN
  SELECT RAISE(ABORT, 'default avionics claim already exists in the canonical table');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_default_avionics_candidate_claim_immutable
BEFORE UPDATE ON aircraft_model_variant_default_avionics_candidates
BEGIN
  SELECT RAISE(ABORT, 'pending default avionics claims must be replaced, admitted, or rejected explicitly');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_default_avionics_candidate_admission_guard
BEFORE INSERT ON aircraft_model_variant_default_avionics
WHEN EXISTS (
  SELECT 1
  FROM aircraft_model_variant_default_avionics_candidates candidate
  WHERE candidate.aircraft_model_variant_id = NEW.aircraft_model_variant_id
    AND candidate.model_year = NEW.model_year
    AND candidate.avionics_model_id = NEW.avionics_model_id
)
AND NOT EXISTS (
  SELECT 1
  FROM aircraft_model_variant_default_avionics_candidates candidate
  WHERE candidate.aircraft_model_variant_id = NEW.aircraft_model_variant_id
    AND candidate.model_year = NEW.model_year
    AND candidate.avionics_model_id = NEW.avionics_model_id
    AND candidate.quantity = NEW.quantity
    AND candidate.source_url = NEW.source_url
    AND candidate.source_title = NEW.source_title
    AND candidate.source_notes = NEW.source_notes
    AND candidate.source_confidence = NEW.source_confidence
)
BEGIN
  SELECT RAISE(ABORT, 'canonical default admission must exactly match its pending claim');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_default_avionics_candidate_admission_move
AFTER INSERT ON aircraft_model_variant_default_avionics
BEGIN
  DELETE FROM aircraft_model_variant_default_avionics_candidates
  WHERE aircraft_model_variant_id = NEW.aircraft_model_variant_id
    AND model_year = NEW.model_year
    AND avionics_model_id = NEW.avionics_model_id
    AND quantity = NEW.quantity
    AND source_url = NEW.source_url
    AND source_title = NEW.source_title
    AND source_notes = NEW.source_notes
    AND source_confidence = NEW.source_confidence;
END;

DROP TABLE IF EXISTS temp.default_avionics_candidate_postcondition_guard;
CREATE TEMP TABLE default_avionics_candidate_postcondition_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO default_avionics_candidate_postcondition_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM aircraft_model_variant_default_avionics default_avionics
    JOIN avionics_models model ON model.id = default_avionics.avionics_model_id
    WHERE model.catalog_status <> 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_model_variant_default_avionics active
    JOIN aircraft_model_variant_default_avionics_candidates candidate
      ON candidate.aircraft_model_variant_id =
         active.aircraft_model_variant_id
     AND candidate.model_year = active.model_year
     AND candidate.avionics_model_id = active.avionics_model_id
  )
  THEN 1
  ELSE 0
END;
DROP TABLE default_avionics_candidate_postcondition_guard;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260802_default_avionics_candidate_quarantine',
  2,
  'b8a6ecd15acc0ce14f67bf37ff4387c0ded4d1c6669d2fc4698b6c0a6c209ba4',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = excluded.contract_version,
  contract_fingerprint = excluded.contract_fingerprint,
  installed_at = excluded.installed_at
WHERE schema_migration_contracts.contract_version = 1
  AND schema_migration_contracts.contract_fingerprint =
      'b50683c27b244cadf3cf88b226665f79051f678df9b30e0d01d0ca261464581f';

COMMIT;
PRAGMA foreign_key_check;
