-- Remove the unused Gemini request/response dossier table from databases that
-- already received the original aircraft-reference migration. Runtime usage
-- accounting remains in gemini_api_usage, and validated evidence remains in
-- curation_evidence_sources/curation_evidence_claims. This migration is
-- idempotent.

BEGIN;

ALTER TABLE IF EXISTS aircraft_identity_decisions
  DROP COLUMN IF EXISTS interaction_run_id;
ALTER TABLE IF EXISTS aircraft_reference_profile_proposals
  DROP COLUMN IF EXISTS interaction_run_id;

DROP TABLE IF EXISTS aircraft_curation_interaction_runs;

COMMIT;
