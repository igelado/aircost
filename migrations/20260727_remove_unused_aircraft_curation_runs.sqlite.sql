-- Remove the unused Gemini request/response dossier table from databases that
-- already received the original aircraft-reference migration. Runtime usage
-- accounting remains in gemini_api_usage, and validated evidence remains in
-- curation_evidence_sources/curation_evidence_claims.
--
-- SQLite has no `DROP COLUMN IF EXISTS`, so the two referencing tables are
-- rebuilt to their canonical shape. The rebuild is deliberately safe to run
-- again after the obsolete columns and table are already gone.

PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;
BEGIN IMMEDIATE;

DROP TABLE IF EXISTS temp.aircraft_curation_cleanup_sequences;
CREATE TEMP TABLE aircraft_curation_cleanup_sequences (
  table_name TEXT PRIMARY KEY,
  seq INTEGER NOT NULL
);
INSERT INTO temp.aircraft_curation_cleanup_sequences (table_name, seq)
SELECT name, seq
FROM sqlite_sequence
WHERE name IN (
  'aircraft_identity_decisions',
  'aircraft_reference_profile_proposals'
);

DROP TABLE IF EXISTS aircraft_identity_decisions_without_interaction_run;
CREATE TABLE aircraft_identity_decisions_without_interaction_run (
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
    'match_existing', 'approve_new', 'ambiguous', 'not_an_entity', 'reject'
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
      AND decision_action IN ('match_existing', 'approve_new')
      AND deterministic_validation_passed = 1)
    OR (decision_status = 'rejected'
      AND decision_action IN ('not_an_entity', 'reject'))
    OR (decision_status = 'ambiguous' AND decision_action = 'ambiguous')
  ),
  CHECK (
    (decision_action = 'match_existing' AND selected_entity_id IS NOT NULL)
    OR (decision_action <> 'match_existing' AND selected_entity_id IS NULL)
  )
);

INSERT INTO aircraft_identity_decisions_without_interaction_run (
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
FROM aircraft_identity_decisions;

DROP INDEX IF EXISTS idx_aircraft_identity_decisions_case;
DROP TABLE aircraft_identity_decisions;
ALTER TABLE aircraft_identity_decisions_without_interaction_run
  RENAME TO aircraft_identity_decisions;
CREATE INDEX idx_aircraft_identity_decisions_case
  ON aircraft_identity_decisions (resolution_case_id, decision_status);

DROP TABLE IF EXISTS aircraft_reference_profile_proposals_without_interaction_run;
CREATE TABLE aircraft_reference_profile_proposals_without_interaction_run (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  resolution_case_id INTEGER NOT NULL
    REFERENCES aircraft_identity_resolution_cases(id) ON DELETE CASCADE,
  proposed_identity_json TEXT NOT NULL,
  proposed_profile_json TEXT NOT NULL,
  deterministic_validation_json TEXT NOT NULL,
  validation_status TEXT NOT NULL CHECK (validation_status IN (
    'pending', 'valid', 'invalid', 'needs_review'
  )),
  catalog_revision TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO aircraft_reference_profile_proposals_without_interaction_run (
  id,
  resolution_case_id,
  proposed_identity_json,
  proposed_profile_json,
  deterministic_validation_json,
  validation_status,
  catalog_revision,
  created_at
)
SELECT
  id,
  resolution_case_id,
  proposed_identity_json,
  proposed_profile_json,
  deterministic_validation_json,
  validation_status,
  catalog_revision,
  created_at
FROM aircraft_reference_profile_proposals;

DROP TABLE aircraft_reference_profile_proposals;
ALTER TABLE aircraft_reference_profile_proposals_without_interaction_run
  RENAME TO aircraft_reference_profile_proposals;

DROP TABLE IF EXISTS aircraft_curation_interaction_runs;

DELETE FROM sqlite_sequence
WHERE name IN (
  'aircraft_identity_decisions',
  'aircraft_identity_decisions_without_interaction_run',
  'aircraft_reference_profile_proposals',
  'aircraft_reference_profile_proposals_without_interaction_run'
);
INSERT INTO sqlite_sequence (name, seq)
SELECT
  'aircraft_identity_decisions',
  max(
    COALESCE((
      SELECT seq
      FROM temp.aircraft_curation_cleanup_sequences
      WHERE table_name = 'aircraft_identity_decisions'
    ), 0),
    COALESCE((SELECT max(id) FROM aircraft_identity_decisions), 0)
  )
UNION ALL
SELECT
  'aircraft_reference_profile_proposals',
  max(
    COALESCE((
      SELECT seq
      FROM temp.aircraft_curation_cleanup_sequences
      WHERE table_name = 'aircraft_reference_profile_proposals'
    ), 0),
    COALESCE((SELECT max(id) FROM aircraft_reference_profile_proposals), 0)
  );

DROP TABLE temp.aircraft_curation_cleanup_sequences;

COMMIT;
PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
PRAGMA foreign_key_check;
