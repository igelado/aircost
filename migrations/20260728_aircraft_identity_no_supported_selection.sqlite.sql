-- Replace the overloaded `not_an_entity` action with a clean operational
-- `no_supported_selection` outcome for newly validated optional dimensions.
-- Historical negative decisions are conservatively retained as generic
-- rejections: they predate the server-owned token, catalog-relation, and
-- applicability predicates and therefore cannot be upgraded to approval.
--
-- The table rebuild is safe to rerun. Canonical legacy rows are translated,
-- while any legacy use outside the two optional dimensions aborts the
-- migration before the original table is changed.

PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;
BEGIN IMMEDIATE;

CREATE TEMP TABLE aircraft_identity_no_selection_migration_contract_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO aircraft_identity_no_selection_migration_contract_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260728_aircraft_identity_no_supported_selection'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260728_aircraft_identity_no_supported_selection'
      AND contract_version = 2
      AND contract_fingerprint =
        '2c61547aae5158dd0a5393ca49218f0f3aada7d9b87caf950fa27fe2953d7dee'
  ) THEN 1
  ELSE 0
END;
DROP TABLE aircraft_identity_no_selection_migration_contract_guard;

DROP TABLE IF EXISTS temp.aircraft_identity_decision_action_guard;
CREATE TEMP TABLE aircraft_identity_decision_action_guard (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO temp.aircraft_identity_decision_action_guard (valid)
SELECT 0
WHERE EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  WHERE decision.decision_action = 'not_an_entity'
    AND NOT (
      decision.entity_kind IN ('generation', 'package')
      AND decision.decision_status = 'rejected'
      AND decision.selected_entity_id IS NULL
    )
)
OR EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  WHERE decision.decision_action = 'no_supported_selection'
    AND (
      NOT (
        decision.entity_kind IN ('generation', 'package')
        AND decision.decision_status = 'approved'
        AND decision.selected_entity_id IS NULL
        AND decision.deterministic_validation_passed = 1
      )
      OR EXISTS (
        SELECT 1
        FROM aircraft_identity_decision_claims claim
        WHERE claim.decision_id = decision.id
      )
    )
);
DROP TABLE temp.aircraft_identity_decision_action_guard;

DROP TABLE IF EXISTS temp.aircraft_identity_decision_action_sequences;
CREATE TEMP TABLE aircraft_identity_decision_action_sequences (
  seq INTEGER NOT NULL
);
INSERT INTO temp.aircraft_identity_decision_action_sequences (seq)
SELECT seq
FROM sqlite_sequence
WHERE name = 'aircraft_identity_decisions';

DROP TABLE IF EXISTS aircraft_identity_decisions_no_supported_selection;
CREATE TABLE aircraft_identity_decisions_no_supported_selection (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  resolution_case_id INTEGER NOT NULL
    REFERENCES aircraft_identity_resolution_cases(id) ON DELETE RESTRICT,
  entity_kind TEXT NOT NULL CHECK (entity_kind IN (
    'make', 'family', 'designation', 'alias', 'identifier', 'generation',
    'generation_designation', 'package', 'package_applicability',
    'engine_model', 'propeller_model',
    'reference_configuration', 'serial_scheme', 'feature_definition',
    'reference_profile'
  )),
  decision_action TEXT NOT NULL CHECK (decision_action IN (
    'match_existing', 'approve_new', 'no_supported_selection', 'ambiguous',
    'reject'
  )),
  decision_status TEXT NOT NULL CHECK (decision_status IN (
    'approved', 'rejected', 'ambiguous'
  )),
  selected_entity_id INTEGER,
  decision_payload_json TEXT NOT NULL,
  deterministic_validation_json TEXT NOT NULL,
  deterministic_validation_passed INTEGER NOT NULL
    CHECK (deterministic_validation_passed IN (0, 1)),
  rationale TEXT NOT NULL,
  decided_by_user_id INTEGER REFERENCES users(id) ON DELETE RESTRICT,
  decided_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (decision_status = 'approved'
      AND decision_action IN (
        'match_existing', 'approve_new', 'no_supported_selection'
      )
      AND deterministic_validation_passed = 1)
    OR (decision_status = 'rejected' AND decision_action = 'reject')
    OR (decision_status = 'ambiguous' AND decision_action = 'ambiguous')
  ),
  CHECK (
    (decision_action = 'match_existing' AND selected_entity_id IS NOT NULL)
    OR (decision_action <> 'match_existing' AND selected_entity_id IS NULL)
  ),
  CHECK (
    decision_action <> 'no_supported_selection'
    OR entity_kind IN ('generation', 'package')
  )
);

INSERT INTO aircraft_identity_decisions_no_supported_selection (
  id,
  resolution_case_id,
  entity_kind,
  decision_action,
  decision_status,
  selected_entity_id,
  decision_payload_json,
  deterministic_validation_json,
  deterministic_validation_passed,
  rationale,
  decided_by_user_id,
  decided_at,
  created_at
)
SELECT
  id,
  resolution_case_id,
  entity_kind,
  CASE decision_action
    WHEN 'not_an_entity' THEN 'reject'
    ELSE decision_action
  END,
  decision_status,
  selected_entity_id,
  decision_payload_json,
  deterministic_validation_json,
  deterministic_validation_passed,
  rationale,
  decided_by_user_id,
  decided_at,
  created_at
FROM aircraft_identity_decisions;

DROP INDEX IF EXISTS idx_aircraft_identity_decisions_case;
DROP TABLE aircraft_identity_decisions;
ALTER TABLE aircraft_identity_decisions_no_supported_selection
  RENAME TO aircraft_identity_decisions;
CREATE INDEX idx_aircraft_identity_decisions_case
  ON aircraft_identity_decisions (resolution_case_id, decision_status);

DROP TRIGGER IF EXISTS aircraft_identity_no_supported_selection_claim_insert;
CREATE TRIGGER aircraft_identity_no_supported_selection_claim_insert
BEFORE INSERT ON aircraft_identity_decision_claims
WHEN EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  WHERE decision.id = NEW.decision_id
    AND decision.decision_action = 'no_supported_selection'
)
BEGIN
  SELECT RAISE(ABORT, 'no-supported-selection decision cannot have evidence claims');
END;

DROP TRIGGER IF EXISTS aircraft_identity_no_supported_selection_claim_update;
CREATE TRIGGER aircraft_identity_no_supported_selection_claim_update
BEFORE UPDATE OF decision_id ON aircraft_identity_decision_claims
WHEN EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  WHERE decision.id = NEW.decision_id
    AND decision.decision_action = 'no_supported_selection'
)
BEGIN
  SELECT RAISE(ABORT, 'no-supported-selection decision cannot have evidence claims');
END;

DROP TRIGGER IF EXISTS aircraft_identity_no_supported_selection_decision_update;
CREATE TRIGGER aircraft_identity_no_supported_selection_decision_update
BEFORE UPDATE OF decision_action ON aircraft_identity_decisions
WHEN NEW.decision_action = 'no_supported_selection'
  AND EXISTS (
    SELECT 1
    FROM aircraft_identity_decision_claims claim
    WHERE claim.decision_id = OLD.id
  )
BEGIN
  SELECT RAISE(ABORT, 'decision with evidence claims cannot become no-supported-selection');
END;

DELETE FROM sqlite_sequence
WHERE name IN (
  'aircraft_identity_decisions',
  'aircraft_identity_decisions_no_supported_selection'
);
INSERT INTO sqlite_sequence (name, seq)
SELECT
  'aircraft_identity_decisions',
  max(
    COALESCE((
      SELECT seq
      FROM temp.aircraft_identity_decision_action_sequences
    ), 0),
    COALESCE((SELECT max(id) FROM aircraft_identity_decisions), 0)
  );
DROP TABLE temp.aircraft_identity_decision_action_sequences;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260728_aircraft_identity_no_supported_selection',
  2,
  '2c61547aae5158dd0a5393ca49218f0f3aada7d9b87caf950fa27fe2953d7dee',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
PRAGMA foreign_key_check;
