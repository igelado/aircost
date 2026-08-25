use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use sha3::Sha3_256;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Connection, Executor, PgConnection, PgPool, SqliteConnection, SqlitePool};

use crate::models::User;

pub const DEFAULT_DATABASE_PATH: &str = "data/aircost.sqlite3";
pub const DEFAULT_DATABASE_URL: &str = "sqlite://data/aircost.sqlite3";
pub const DEVELOPER_EMAIL: &str = "developer@localhost";
const DEVELOPER_AUTH_SUBJECT: &str = "developer";
const SQLITE_SCHEMA_SQL: &str = include_str!("../schema/sqlite.sql");
const POSTGRES_SCHEMA_SQL: &str = include_str!("../schema/postgres.sql");
const POSTGRES_SEARCH_PATH: &str = "public,pg_catalog,pg_temp";
const POSTGRES_STARTUP_ADVISORY_LOCK_KEY: i64 = 0x0041_4952_434f_5354;
const VALUATION_DATA_HARDENING_MIGRATION: &str = "20260720_valuation_data_hardening";
const AVIONICS_CATALOG_CURATION_MIGRATION: &str = "20260721_avionics_catalog_curation";
const AVIONICS_MULTI_TYPE_MIGRATION: &str = "20260721_avionics_multi_type";
const AIRCRAFT_REFERENCE_CATALOG_MIGRATION: &str = "20260722_aircraft_reference_catalog";
const LISTING_PENDING_REVIEWS_MIGRATION: &str = "20260724_listing_pending_reviews";
const IDENTITY_DEDUPLICATION_POSTCONDITIONS_MIGRATION: &str =
    "20260725_identity_deduplication_postconditions";
const IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_VERSION: i64 = 6;
const IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_FINGERPRINT: &str =
    "cd001240b48a1480fd8bbee39b9ddedbba01d00fad45cbac315cec7a243cf133";
const LISTING_AIRCRAFT_IDENTITY_MIGRATION: &str = "20260725_listing_aircraft_identity";
const LISTING_AIRCRAFT_IDENTITY_CONTRACT_VERSION: i64 = 2;
const LISTING_AIRCRAFT_IDENTITY_CONTRACT_FINGERPRINT: &str =
    "63fb5b5213fc9eb2b7b4dcb2b0be3a9f22a80d4acae49f64e68ec1302c1437be";
const LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_MIGRATION: &str =
    "20260726_listing_aircraft_compatibility_projection";
const LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_CONTRACT_VERSION: i64 = 2;
const LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_CONTRACT_FINGERPRINT: &str =
    "0a182d5972d62be3d906395df8d08b741bc3e23d713badf7596b360048aa45ba";
const AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_MIGRATION: &str =
    "20260728_aircraft_identity_no_supported_selection";
const AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_CONTRACT_VERSION: i64 = 2;
const AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_CONTRACT_FINGERPRINT: &str =
    "2c61547aae5158dd0a5393ca49218f0f3aada7d9b87caf950fa27fe2953d7dee";
const AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION: &str = "20260729_aircraft_catalog_retrieval_keys";
const AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION: i64 = 1;
const AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_FINGERPRINT: &str =
    "b40b266fc450810cf89acc78c9405f4cd7d816ea38d389114e93a20cfea6901d";
const AIRCRAFT_TCDS_MAKE_LINEAGE_MIGRATION: &str = "20260730_aircraft_tcds_make_lineage";
const AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_VERSION: i64 = 1;
const AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_FINGERPRINT: &str =
    "566485027d3df81bb5a90abcc0ce2b707e565bcbdc92ae3f007f527832fae735";
const AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_MIGRATION: &str =
    "20260731_avionics_human_reviewed_consolidation";
const AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_VERSION: i64 = 1;
const AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_FINGERPRINT: &str =
    "93a641a0f653eacf0c8413bdb697a35c588fe34efc1419d30bf65146c8b2d55a";
const AVIONICS_DESCRIPTIVE_CONSOLIDATION_MIGRATION: &str =
    "20260808_avionics_descriptive_consolidation";
const AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_VERSION: i64 = 1;
const AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_FINGERPRINT: &str =
    "3aacf958efa7fb5e24c5897cf0369d40cb506b2a22444d629ea0a76462ce1a70";
const AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_MIGRATION: &str =
    "20260810_avionics_grounded_exact_model_consolidation";
const AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_VERSION: i64 = 1;
const AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_FINGERPRINT: &str =
    "36f9ff06bf42fc769508ecfe578f4b4a11f2e0072b81efebed1dee8958654f2a";
const AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_MIGRATION: &str =
    "20260801_avionics_authoritative_source_origins";
const AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_VERSION: i64 = 2;
const AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_FINGERPRINT: &str =
    "f78087f6354d93d78dc8cebc895f285e38a91ca6f72dc2351acaaa88b49f9620";
const AVIONICS_PRODUCT_REUSE_ATTESTATIONS_MIGRATION: &str =
    "20260803_avionics_product_reuse_attestations";
const AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_VERSION: i64 = 2;
const AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_FINGERPRINT: &str =
    "8ad6e935e1222a03e2da4848a9e3c6f4b7f50ee027a6e50ede3b692d034cae55";
const AVIONICS_PRODUCT_REUSE_V2_MIGRATION: &str = "20260807_avionics_product_reuse_v2";
const AVIONICS_PRODUCT_REUSE_V2_CONTRACT_VERSION: i64 = 1;
const AVIONICS_PRODUCT_REUSE_V2_CONTRACT_FINGERPRINT: &str =
    "efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc";
const AVIONICS_GROUNDED_EVIDENCE_REFRESH_MIGRATION: &str =
    "20260804_avionics_grounded_evidence_refresh";
const AVIONICS_GROUNDED_EVIDENCE_REFRESH_CONTRACT_VERSION: i64 = 1;
const AVIONICS_GROUNDED_EVIDENCE_REFRESH_CONTRACT_FINGERPRINT: &str =
    "0c44e30c662d8f51c11f7db883251c1356cfda4d53957df038988c32d3b91399";
const LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION: &str =
    "20260825_listing_avionics_grounded_capabilities";
const LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_VERSION: i64 = 1;
// SHA-256 of
// `20260825_listing_avionics_grounded_capabilities:v1:capability-global-source-revocation-epoch:link-authorization-exact-submission-checkpoint-global-source-revocation-epoch:public-qualified-trigger-functions:pg_catalog-search-path:exact-startup-object-contract:complete-external-trigger-repair:data-preserving-rerun`.
const LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_FINGERPRINT: &str =
    "682ca4e44ced30b0d14da879c31e0fa4b24cc1b6fceb9f213ecc39d9abca0338";
const LISTING_AVIONICS_DISPOSITIONS_MIGRATION: &str = "20260819_listing_avionics_dispositions";
const FAA_REFERENCE_REACHABILITY_MIGRATION: &str = "20260819_faa_reference_reachability";
const FAA_REFERENCE_REACHABILITY_CONTRACT_VERSION: i64 = 1;
const FAA_REFERENCE_REACHABILITY_CONTRACT_FINGERPRINT: &str =
    "fc6451ffe8e1ee2034e76480767d16d6c37463461d9e684687448b4d43f96bef";
const FAA_RECORD_HASH_DOMAIN_MIGRATION: &str = "20260820_faa_record_hash_domain";
const FAA_RECORD_HASH_DOMAIN_CONTRACT_VERSION: i64 = 1;
const FAA_RECORD_HASH_DOMAIN_CONTRACT_FINGERPRINT: &str =
    "f124f573bf705da6c1e4b0a5c7a8df45ea5a4a5dc009a28eee012be42c691502";
const AVIONICS_APPROVED_CONCRETE_MODEL_MIGRATION: &str =
    "20260821_avionics_approved_concrete_model";
const AVIONICS_APPROVED_CONCRETE_MODEL_CONTRACT_VERSION: i64 = 1;
const AVIONICS_APPROVED_CONCRETE_MODEL_CONTRACT_FINGERPRINT: &str =
    "1305564519a99b0ecdfb85a045b9924bf90a33b2914bb6822a219170d541a5f6";
const AVIONICS_GENERIC_FEATURE_LABELS_MIGRATION: &str = "20260824_avionics_generic_feature_labels";
const AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_VERSION: i64 = 1;
const AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_FINGERPRINT: &str =
    "366cf90682d11e71293461aca169445a04f8b906d8c15dab6fde76e1dc2384c8";
const AVIONICS_APPROVED_CONCRETE_MODEL_OBJECT_CONTRACT_VERSION: i64 = 2;
const SQLITE_AVIONICS_APPROVED_CONCRETE_MODEL_OBJECT_CONTRACT_FINGERPRINT: &str =
    "c544640fc9fd748d8601ac21f4510e0659f2039b162bd6ef54485e598ef95355";
const POSTGRES_AVIONICS_APPROVED_CONCRETE_MODEL_OBJECT_CONTRACT_FINGERPRINT: &str =
    "0d3f244fc0ac9324e2c16bb74c489f6aca97ae6ac6a900e98bc8640752b7e850";
const POSTGRES_AVIONICS_APPROVED_CONCRETE_MODEL_FUNCTION_SOURCE: &str = r#"
BEGIN
  IF NEW.catalog_status = 'approved' AND (
    NEW.normalized_name <> LOWER(BTRIM(NEW.normalized_name))
    OR NEW.normalized_name !~ '^[a-z0-9]+( [a-z0-9]+)*$'
  ) THEN
    RAISE EXCEPTION 'approved avionics normalized_name is not canonical; canonicalize, correct, or demote it before retrying migration';
  END IF;
  IF NEW.catalog_status = 'approved' AND NEW.normalized_name IN (
    '', 'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
    'avionics', 'avionics suite', 'integrated avionics', 'integrated avionics suite',
    'glass panel', 'flight instruments', 'standard flight instruments',
    'standard vfr avionics', 'standard ifr avionics', 'radio', 'radios', 'nav',
    'com', 'nav com', 'gps nav com', 'navigation system', 'gps', 'autopilot',
    'flight director', 'transponder', 'ads b', 'ads b in', 'ads b out',
    'ads b in out', 'ads b in and out', 'weather radar', 'audio panel',
    'standard audio panel', 'audio controller', 'audio control panel',
    'display', 'flight display', 'pfd', 'mfd', 'pfd mfd', 'navigation indicator',
    'traffic', 'active traffic', 'traffic advisory system', 'datalink',
    'datalink weather', 'xm',
    'xm weather', 'xm radio', 'xm weather radio', 'lightning detection',
    'terrain awareness', 'terrain awareness system', 'terrain avoidance system',
    'taws', 'synthetic vision', 'synthetic vision system', 'svt',
    'safetaxi', 'safe taxi',
    'flitecharts', 'flite charts', 'charts', 'electronic charts',
    'electronic stability and protection', 'electronic stability protection',
    'stability and protection', 'wireless data loading',
    'wireless database loading', 'engine monitor', 'engine fuel monitoring',
    'standby instrument', 'backup instruments', 'elt', 'adf', 'dme', 'ahrs',
    'air data computer', 'radar altimeter', 'magnetometer', 'clock timer', 'waas',
    'waas gps', 'dual waas', 'remote transponder', 'transponder ads b',
    'stormscope', 'standard radio navigation', 'equipment'
  ) THEN
    RAISE EXCEPTION 'approved avionics model is a generic category; canonicalize, correct, or demote it before retrying migration';
  END IF;
  RETURN NEW;
END;
"#;
// SHA-256 fingerprints of newline-terminated, ordered PostgreSQL catalog
// signatures. Keeping each object class separate lets startup identify the
// broken class without exposing row data. Trigger and function definitions are
// attested separately because their bodies need reviewable source constants.
const POSTGRES_FAA_RELATION_SHAPE_FINGERPRINT: &str =
    "89fec02f98c47c310eff50a08c2ad3e36e1c384f815ca0cf68a60f26cc3d15a5";
const POSTGRES_FAA_COLUMN_SHAPE_FINGERPRINT: &str =
    "b6b60fe7998931e1b22d4d7d074fb45f8df3781c18335f5a06ccfc3e7102f798";
const POSTGRES_FAA_CONSTRAINT_SHAPE_FINGERPRINT: &str =
    "980de61c2318834ecfeb1b6cb0dfc97d85edb3b08b1ed2048a1ff867d88c259c";
const POSTGRES_FAA_FOREIGN_KEY_SHAPE_FINGERPRINT: &str =
    "2a9c555c9393f8ea9e55adceb51ae53fd9d074d5eb4071b883e28797aa346a6f";
const POSTGRES_FAA_INDEX_SHAPE_FINGERPRINT: &str =
    "1cf04e4f89a155745c8dcf13aaca19b8ff92e80437a95f176282d1acb1977158";
const POSTGRES_FAA_SNAPSHOT_EVIDENCE_FUNCTION_SOURCE: &str = r#"
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM public.curation_evidence_sources source
    WHERE source.id = NEW.evidence_source_id
      AND source.source_domain = 'faa.gov'
      AND source.source_tier = 'regulator_primary'
      AND source.source_url = NEW.source_url
      AND source.content_sha256 = NEW.archive_sha256
  ) THEN
    RAISE EXCEPTION 'FAA snapshot requires exact regulator evidence provenance';
  END IF;
  RETURN NEW;
END;
"#;
const POSTGRES_FAA_AIRCRAFT_REFERENCE_REACHABILITY_FUNCTION_SOURCE: &str = r#"
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM public.faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.aircraft_code = NEW.aircraft_code
  ) THEN
    RAISE EXCEPTION 'FAA aircraft reference must be reachable from a target match';
  END IF;
  RETURN NEW;
END;
"#;
const POSTGRES_FAA_ENGINE_REFERENCE_REACHABILITY_FUNCTION_SOURCE: &str = r#"
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM public.faa_registry_aircraft aircraft
    WHERE aircraft.snapshot_id = NEW.snapshot_id
      AND aircraft.engine_code = NEW.engine_code
  ) THEN
    RAISE EXCEPTION 'FAA engine reference must be reachable from a target match';
  END IF;
  RETURN NEW;
END;
"#;
const POSTGRES_FAA_COVERAGE_FUNCTION_SOURCE: &str = r#"
BEGIN
  IF (NEW.lookup_status = 'matched' AND NOT EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) OR (NEW.lookup_status = 'absent' AND EXISTS (
        SELECT 1 FROM public.faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id
          AND aircraft.n_number = NEW.n_number
      )) THEN
    RAISE EXCEPTION 'FAA coverage must agree with its target match';
  END IF;
  RETURN NEW;
END;
"#;
const POSTGRES_FAA_IMMUTABILITY_FUNCTION_SOURCE: &str = r#"
BEGIN
  RAISE EXCEPTION 'FAA registry snapshots and projections are immutable';
END;
"#;
const AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_MIGRATION: &str =
    "20260819_aircraft_listing_identity_corrections";
const AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_VERSION: i64 = 1;
const AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_FINGERPRINT: &str =
    "589a0716726d2ffd34bf84c08583198383c003228b769c88f094ac6bd9f677b8";
const AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_MIGRATION: &str =
    "20260821_aircraft_visual_source_corrections";
const AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_VERSION: i64 = 1;
const AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_FINGERPRINT: &str =
    "ccc63aa23f2579ec5cec682bf1493a13eb73829718936b5890bd84de51bb828a";
const LISTING_REPLAY_RUNS_MIGRATION: &str = "20260819_listing_replay_runs";
const LISTING_REPLAY_RUNS_CONTRACT_VERSION: i64 = 1;
const LISTING_REPLAY_RUNS_CONTRACT_FINGERPRINT: &str =
    "ef344cdb9cf9a7ffcd0ae66e1c9cb3979afa07c1155377cee5dc1031dd0d47c1";
const POSTGRES_LISTING_REPLAY_CHECKS_FINGERPRINT: &str =
    "36cb3b5e9642cedfe6e0b2d92c03864fc9ff9cc2d54ee64348f8fca67d567f40";
const POSTGRES_LISTING_REPLAY_FUNCTIONS_FINGERPRINT: &str = "7e885abd1d361c7c831c84e5e3a58e1d";
const SQLITE_CORRECTION_DECISION_UPDATE_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_listing_identity_corrections_immutable_update
BEFORE UPDATE ON aircraft_listing_identity_correction_decisions
BEGIN SELECT RAISE(ABORT, 'aircraft listing identity correction decisions are immutable'); END
"#;
const SQLITE_CORRECTION_DECISION_DELETE_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_listing_identity_corrections_immutable_delete
BEFORE DELETE ON aircraft_listing_identity_correction_decisions
BEGIN SELECT RAISE(ABORT, 'aircraft listing identity correction decisions are immutable'); END
"#;
const SQLITE_CORRECTION_OBSERVATION_UPDATE_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_identity_correction_observation_immutable_update
BEFORE UPDATE ON aircraft_identity_observations
WHEN EXISTS (
  SELECT 1 FROM aircraft_listing_identity_correction_decisions decision
  WHERE decision.observation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'aircraft identity observations referenced by correction decisions are immutable'); END
"#;
const SQLITE_CORRECTION_OBSERVATION_DELETE_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_identity_correction_observation_immutable_delete
BEFORE DELETE ON aircraft_identity_observations
WHEN EXISTS (
  SELECT 1 FROM aircraft_listing_identity_correction_decisions decision
  WHERE decision.observation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'aircraft identity observations referenced by correction decisions are immutable'); END
"#;
const SQLITE_SOURCE_IDENTITY_RECEIPT_GATE_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_source_identity_receipt_gate
BEFORE UPDATE OF ingestion_state, ingestion_error, is_verified
ON aircraft_sale_listings
WHEN OLD.ingestion_error = 'source_identity_correction_receipt_pending'
 AND (
   NEW.ingestion_error IS NOT OLD.ingestion_error
   OR NEW.ingestion_state IS NOT OLD.ingestion_state
   OR NEW.is_verified IS NOT OLD.is_verified
 )
 AND NOT EXISTS (
   SELECT 1
   FROM aircraft_listing_identity_correction_decisions decision
   JOIN plugin_submissions submission
     ON submission.id = decision.plugin_submission_id
   WHERE decision.aircraft_sale_listing_id = OLD.id
     AND decision.correction_kind IN ('faa_serial', 'visual_identifier')
     AND decision.rendered_html_sha256 = submission.rendered_html_sha256
     AND submission.user_id = OLD.created_by_user_id
     AND submission.canonical_listing_id = OLD.id
     AND submission.extraction_error IS NULL
     AND NEW.registration_number IS decision.corrected_registration_number
     AND NEW.serial_number IS decision.corrected_serial_number
 )
BEGIN SELECT RAISE(ABORT, 'source identity correction receipt is required before leaving the receipt gate'); END
"#;
const SQLITE_SOURCE_VISUAL_ARTIFACT_UPDATE_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_source_visual_artifacts_immutable_update
BEFORE UPDATE ON aircraft_source_visual_correction_artifacts
BEGIN SELECT RAISE(ABORT, 'aircraft source visual correction artifacts are immutable'); END
"#;
const SQLITE_SOURCE_VISUAL_ARTIFACT_DELETE_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_source_visual_artifacts_immutable_delete
BEFORE DELETE ON aircraft_source_visual_correction_artifacts
BEGIN SELECT RAISE(ABORT, 'aircraft source visual correction artifacts are immutable'); END
"#;
const SQLITE_SOURCE_VISUAL_ARTIFACT_INSERT_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_source_visual_artifacts_validate_insert
BEFORE INSERT ON aircraft_source_visual_correction_artifacts
WHEN NOT EXISTS (
  SELECT 1 FROM plugin_submissions submission
  JOIN faa_registry_snapshots snapshot ON snapshot.id = NEW.faa_registry_snapshot_id
  JOIN faa_registry_coverage observed ON observed.snapshot_id = snapshot.id AND observed.n_number = NEW.observed_registration_number AND observed.lookup_status = 'absent'
  JOIN faa_registry_coverage corrected ON corrected.snapshot_id = snapshot.id AND corrected.n_number = NEW.corrected_registration_number AND corrected.lookup_status = 'matched'
  JOIN faa_registry_aircraft aircraft ON aircraft.snapshot_id = snapshot.id AND aircraft.n_number = corrected.n_number
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.rendered_html_sha256 = NEW.rendered_html_sha256
    AND snapshot.id = (SELECT id FROM faa_registry_snapshots ORDER BY snapshot_date DESC, id DESC LIMIT 1)
    AND snapshot.archive_sha256 = NEW.faa_snapshot_archive_sha256
    AND aircraft.source_record_sha256 = NEW.faa_source_record_sha256
    AND aircraft.manufacturer_serial_raw IS NEW.corrected_serial_number
)
BEGIN SELECT RAISE(ABORT, 'source visual correction artifact requires one exact current FAA absence/match pair'); END
"#;
const POSTGRES_CORRECTION_DECISION_FUNCTION_SOURCE: &str = r#"
BEGIN
  RAISE EXCEPTION 'aircraft listing identity correction decisions are immutable';
END;
"#;
const POSTGRES_CORRECTION_OBSERVATION_FUNCTION_SOURCE: &str = r#"
BEGIN
  IF EXISTS (
    SELECT 1 FROM public.aircraft_listing_identity_correction_decisions decision
    WHERE decision.observation_id = OLD.id
  ) THEN
    RAISE EXCEPTION 'aircraft identity observations referenced by correction decisions are immutable';
  END IF;
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END;
"#;
const POSTGRES_SOURCE_IDENTITY_RECEIPT_GATE_FUNCTION_SOURCE: &str = r#"
BEGIN
  IF OLD.ingestion_error = 'source_identity_correction_receipt_pending'
     AND (
       NEW.ingestion_error IS DISTINCT FROM OLD.ingestion_error
       OR NEW.ingestion_state IS DISTINCT FROM OLD.ingestion_state
       OR NEW.is_verified IS DISTINCT FROM OLD.is_verified
     )
     AND NOT EXISTS (
       SELECT 1
       FROM public.aircraft_listing_identity_correction_decisions decision
       JOIN public.plugin_submissions submission
         ON submission.id = decision.plugin_submission_id
       WHERE decision.aircraft_sale_listing_id = OLD.id
         AND decision.correction_kind IN ('faa_serial', 'visual_identifier')
         AND decision.rendered_html_sha256 = submission.rendered_html_sha256
         AND submission.user_id = OLD.created_by_user_id
         AND submission.canonical_listing_id = OLD.id
         AND submission.extraction_error IS NULL
         AND NEW.registration_number IS NOT DISTINCT FROM decision.corrected_registration_number
         AND NEW.serial_number IS NOT DISTINCT FROM decision.corrected_serial_number
     ) THEN
    RAISE EXCEPTION 'source identity correction receipt is required before leaving the receipt gate';
  END IF;
  RETURN NEW;
END;
"#;
const POSTGRES_SOURCE_VISUAL_ARTIFACT_FUNCTION_SOURCE: &str = r#"
BEGIN
  RAISE EXCEPTION 'aircraft source visual correction artifacts are immutable';
END;
"#;
const POSTGRES_SOURCE_VISUAL_ARTIFACT_VALIDATION_FUNCTION_SOURCE: &str = r#"
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM public.plugin_submissions submission
    JOIN public.faa_registry_snapshots snapshot ON snapshot.id = NEW.faa_registry_snapshot_id
    JOIN public.faa_registry_coverage observed ON observed.snapshot_id = snapshot.id AND observed.n_number = NEW.observed_registration_number AND observed.lookup_status = 'absent'
    JOIN public.faa_registry_coverage corrected ON corrected.snapshot_id = snapshot.id AND corrected.n_number = NEW.corrected_registration_number AND corrected.lookup_status = 'matched'
    JOIN public.faa_registry_aircraft aircraft ON aircraft.snapshot_id = snapshot.id AND aircraft.n_number = corrected.n_number
    WHERE submission.id = NEW.plugin_submission_id
      AND submission.rendered_html_sha256 = NEW.rendered_html_sha256
      AND snapshot.id = (SELECT id FROM public.faa_registry_snapshots ORDER BY snapshot_date DESC, id DESC LIMIT 1)
      AND snapshot.archive_sha256 = NEW.faa_snapshot_archive_sha256
      AND aircraft.source_record_sha256 = NEW.faa_source_record_sha256
      AND aircraft.manufacturer_serial_raw IS NOT DISTINCT FROM NEW.corrected_serial_number
  ) THEN
    RAISE EXCEPTION 'source visual correction artifact requires one exact current FAA absence/match pair';
  END IF;
  RETURN NEW;
END;
"#;
const REFERENCE_CATALOG_CUTOVER_MIGRATION: &str = "20260819_reference_catalog_cutover";
const REFERENCE_CATALOG_CUTOVER_CONTRACT_VERSION: i64 = 1;
const REFERENCE_CATALOG_CUTOVER_CONTRACT_FINGERPRINT: &str =
    "85b97a46a697a3b835e5c8817821722fd558120700b1725615161b357bc63522";
const REFERENCE_CATALOG_CUTOVER_SQLITE_MIGRATION_SQL: &str =
    include_str!("../migrations/20260819_reference_catalog_cutover.sqlite.sql");
const REFERENCE_CATALOG_CUTOVER_POSTGRES_MIGRATION_SQL: &str =
    include_str!("../migrations/20260819_reference_catalog_cutover.postgres.sql");

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct MigrationContractReceipt {
    pub migration_name: &'static str,
    pub contract_version: i64,
    pub contract_fingerprint: &'static str,
}

const COMMON_STARTUP_MIGRATION_CONTRACT_RECEIPTS: &[MigrationContractReceipt] = &[
    MigrationContractReceipt {
        migration_name: IDENTITY_DEDUPLICATION_POSTCONDITIONS_MIGRATION,
        contract_version: IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_VERSION,
        contract_fingerprint: IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: LISTING_AIRCRAFT_IDENTITY_MIGRATION,
        contract_version: LISTING_AIRCRAFT_IDENTITY_CONTRACT_VERSION,
        contract_fingerprint: LISTING_AIRCRAFT_IDENTITY_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_MIGRATION,
        contract_version: LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_MIGRATION,
        contract_version: AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_CONTRACT_VERSION,
        contract_fingerprint: AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION,
        contract_version: AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION,
        contract_fingerprint: AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AIRCRAFT_TCDS_MAKE_LINEAGE_MIGRATION,
        contract_version: AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_VERSION,
        contract_fingerprint: AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_MIGRATION,
        contract_version: AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_VERSION,
        contract_fingerprint: AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_MIGRATION,
        contract_version: AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_VERSION,
        contract_fingerprint: AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AVIONICS_PRODUCT_REUSE_ATTESTATIONS_MIGRATION,
        contract_version: AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_VERSION,
        contract_fingerprint: AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AVIONICS_GROUNDED_EVIDENCE_REFRESH_MIGRATION,
        contract_version: AVIONICS_GROUNDED_EVIDENCE_REFRESH_CONTRACT_VERSION,
        contract_fingerprint: AVIONICS_GROUNDED_EVIDENCE_REFRESH_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AVIONICS_PRODUCT_REUSE_V2_MIGRATION,
        contract_version: AVIONICS_PRODUCT_REUSE_V2_CONTRACT_VERSION,
        contract_fingerprint: AVIONICS_PRODUCT_REUSE_V2_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AVIONICS_DESCRIPTIVE_CONSOLIDATION_MIGRATION,
        contract_version: AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_VERSION,
        contract_fingerprint: AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_MIGRATION,
        contract_version: AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_VERSION,
        contract_fingerprint: AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION,
        contract_version: LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_VERSION,
        contract_fingerprint: LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_MIGRATION,
        contract_version: AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_VERSION,
        contract_fingerprint: AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_MIGRATION,
        contract_version: AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_VERSION,
        contract_fingerprint: AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: LISTING_REPLAY_RUNS_MIGRATION,
        contract_version: LISTING_REPLAY_RUNS_CONTRACT_VERSION,
        contract_fingerprint: LISTING_REPLAY_RUNS_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: REFERENCE_CATALOG_CUTOVER_MIGRATION,
        contract_version: REFERENCE_CATALOG_CUTOVER_CONTRACT_VERSION,
        contract_fingerprint: REFERENCE_CATALOG_CUTOVER_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: FAA_RECORD_HASH_DOMAIN_MIGRATION,
        contract_version: FAA_RECORD_HASH_DOMAIN_CONTRACT_VERSION,
        contract_fingerprint: FAA_RECORD_HASH_DOMAIN_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AVIONICS_APPROVED_CONCRETE_MODEL_MIGRATION,
        contract_version: AVIONICS_APPROVED_CONCRETE_MODEL_CONTRACT_VERSION,
        contract_fingerprint: AVIONICS_APPROVED_CONCRETE_MODEL_CONTRACT_FINGERPRINT,
    },
    MigrationContractReceipt {
        migration_name: AVIONICS_GENERIC_FEATURE_LABELS_MIGRATION,
        contract_version: AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_VERSION,
        contract_fingerprint: AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_FINGERPRINT,
    },
];

const POSTGRES_ONLY_STARTUP_MIGRATION_CONTRACT_RECEIPTS: &[MigrationContractReceipt] =
    &[MigrationContractReceipt {
        migration_name: FAA_REFERENCE_REACHABILITY_MIGRATION,
        contract_version: FAA_REFERENCE_REACHABILITY_CONTRACT_VERSION,
        contract_fingerprint: FAA_REFERENCE_REACHABILITY_CONTRACT_FINGERPRINT,
    }];

pub(crate) fn canonical_startup_migration_contract_receipts(
    kind: DatabaseKind,
) -> Vec<MigrationContractReceipt> {
    let mut receipts = COMMON_STARTUP_MIGRATION_CONTRACT_RECEIPTS.to_vec();
    if kind == DatabaseKind::Postgres {
        receipts.extend_from_slice(POSTGRES_ONLY_STARTUP_MIGRATION_CONTRACT_RECEIPTS);
    }
    receipts.sort();
    receipts
}

const REFERENCE_CATALOG_CUTOVER_ROUTINES: &[&str] = &[
    "aircraft_serial_natural_sort_key",
    "validate_aircraft_serial_scheme_ordering",
    "prevent_referenced_avionics_catalog_downgrade",
    "validate_aircraft_valuation_compatibility_projection",
    "require_aircraft_catalog_approval",
    "validate_aircraft_reference_version_insert",
    "preserve_assigned_aircraft_applicability",
    "prevent_new_unresolved_aircraft_dimension",
    "validate_official_dollar_normalization_fact",
    "prevent_official_dollar_normalization_mutation",
    "validate_aircraft_reference_child_insert",
    "prevent_aircraft_reference_fact_mutation",
    "validate_aircraft_reference_version_update",
];

const REFERENCE_CATALOG_CUTOVER_SQLITE_TRIGGERS: &[&str] = &[
    "avionics_models_referenced_status_update",
    "aircraft_valuation_projection_validate_insert",
    "aircraft_reference_scope_canonical_insert",
    "aircraft_reference_scope_key_recompute_insert",
    "aircraft_reference_versions_require_approval",
    "official_dollar_normalization_require_evidence",
    "official_dollar_normalization_immutable_update",
    "official_dollar_normalization_immutable_delete",
    "aircraft_reference_price_building_insert",
    "aircraft_reference_price_immutable_update",
    "aircraft_reference_price_immutable_delete",
    "aircraft_reference_fact_set_building_insert",
    "aircraft_reference_fact_set_immutable_update",
    "aircraft_reference_fact_set_immutable_delete",
    "aircraft_reference_versions_publish",
];
const REFERENCE_CATALOG_CUTOVER_PROTECTED_RELATIONS: &[&str] = &[
    "plugin_submissions",
    "avionics_models",
    "aircraft_engine_catalog_models",
    "aircraft_propeller_catalog_models",
    "aircraft_makes",
    "aircraft_model_families",
    "aircraft_designations",
    "aircraft_make_aliases",
    "aircraft_family_aliases",
    "aircraft_designation_aliases",
    "aircraft_designation_identifiers",
    "aircraft_generations",
    "aircraft_generation_designations",
    "aircraft_factory_packages",
    "aircraft_package_applicability",
    "aircraft_reference_configurations",
    "aircraft_serial_number_schemes",
    "aircraft_feature_definitions",
    "aircraft_reference_configuration_versions",
    "aircraft_reference_applicability_scopes",
    "aircraft_reference_prices",
    "aircraft_reference_avionics",
    "aircraft_reference_engines",
    "aircraft_reference_propellers",
    "aircraft_reference_features",
    "aircraft_reference_fact_set_attestations",
    "official_dollar_normalization_facts",
    "aircraft_valuation_compatibility_projections",
    "listing_verification_run_items",
];
const REFERENCE_CATALOG_CUTOVER_RETIRED_RELATIONS: &[&str] = &[
    "aircraft_model_spec_versions",
    "aircraft_model_variant_price_points",
    "aircraft_model_variant_default_avionics",
    "aircraft_model_variant_default_avionics_candidates",
    "depreciation_profiles",
    "depreciation_profile_fit_metadata",
    "component_depreciation_profiles",
];
const REFERENCE_CATALOG_CUTOVER_RETIRED_ROUTINES: &[&str] = &[
    "require_approved_default_avionics_model",
    "reject_active_default_avionics_candidate",
    "preserve_pending_default_avionics_claim",
    "require_exact_pending_default_avionics_admission",
    "move_admitted_default_avionics_candidate",
    "prevent_projected_aircraft_evidence_variant_move",
];
const REFERENCE_CATALOG_CUTOVER_SQLITE_OBJECT_COUNT: i64 = 213;
const REFERENCE_CATALOG_CUTOVER_SQLITE_DEFINITION_DIGEST: &str =
    "581bc9491e66de7fcb0c81d6d0fd0c26abbed74dac4c56de6d133643dd4b4b54";
const REFERENCE_CATALOG_CUTOVER_SQLITE_INDEX_SIGNATURES: &[&str] = &[
    "aircraft_reference_fact_set_attestations:sqlite_autoindex_aircraft_reference_fact_set_attestations_1:1:u:0:0:1:aircraft_reference_configuration_version_id:0:BINARY:1,1:2:fact_set_kind:0:BINARY:1,2:-1::0:BINARY:0",
    "aircraft_reference_prices:sqlite_autoindex_aircraft_reference_prices_1:1:u:0:0:1:aircraft_reference_configuration_version_id:0:BINARY:1,1:2:price_kind:0:BINARY:1,2:4:currency:0:BINARY:1,3:-1::0:BINARY:0",
    "listing_verification_run_items:idx_listing_verification_run_items_claim:0:c:0:0:1:run_id:0:BINARY:1,1:4:status:0:BINARY:1,2:3:position:0:BINARY:1,3:0:id:0:BINARY:1,4:-1::0:BINARY:0",
    "listing_verification_run_items:idx_listing_verification_run_items_one_active_listing:1:c:1:0:2:listing_id:0:BINARY:1,1:-1::0:BINARY:0",
    "listing_verification_run_items:idx_listing_verification_run_items_one_running_per_run:1:c:1:0:1:run_id:0:BINARY:1,1:-1::0:BINARY:0",
    "listing_verification_run_items:sqlite_autoindex_listing_verification_run_items_1:1:u:0:0:1:run_id:0:BINARY:1,1:3:position:0:BINARY:1,2:-1::0:BINARY:0",
    "listing_verification_run_items:sqlite_autoindex_listing_verification_run_items_2:1:u:0:0:1:run_id:0:BINARY:1,1:2:listing_id:0:BINARY:1,2:-1::0:BINARY:0",
    "official_dollar_normalization_facts:sqlite_autoindex_official_dollar_normalization_facts_1:1:u:0:0:7:evidence_claim_id:0:BINARY:1,1:-1::0:BINARY:0",
    "official_dollar_normalization_facts:sqlite_autoindex_official_dollar_normalization_facts_2:1:u:0:0:1:source_year:0:BINARY:1,1:2:target_year:0:BINARY:1,2:-1::0:BINARY:0",
];
const REFERENCE_CATALOG_CUTOVER_POSTGRES_OBJECT_COUNT: i64 = 792;
const REFERENCE_CATALOG_CUTOVER_POSTGRES_DEFINITION_DIGEST: &str =
    "a12dfb4a0ff4f026bee8b16c1c26ac0a";
const SQLITE_SERIAL_SCHEME_INSERT_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_serial_schemes_require_approval
BEFORE INSERT ON aircraft_serial_number_schemes
WHEN NEW.normalization_version <> 'natural_alphanumeric_segments_v1'
OR NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'serial_scheme'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'serial scheme requires the universal ordering and an approved evidence-backed decision'); END
"#;
const SQLITE_SERIAL_SCHEME_UPDATE_TRIGGER: &str = r#"
CREATE TRIGGER aircraft_serial_schemes_preserve_ordering
BEFORE UPDATE OF normalization_version ON aircraft_serial_number_schemes
WHEN NEW.normalization_version <> 'natural_alphanumeric_segments_v1'
BEGIN SELECT RAISE(ABORT, 'serial scheme ordering version is immutable'); END
"#;
const SQLITE_REFERENCE_PRICES_FRESH_TABLE: &str = r#"
CREATE TABLE aircraft_reference_prices (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  price_kind TEXT NOT NULL CHECK (price_kind IN (
    'base_msrp', 'equipped_msrp', 'tier_increment', 'other_factory_price'
  )),
  amount REAL NOT NULL CHECK (amount > 0),
  currency TEXT NOT NULL CHECK (length(currency) = 3 AND currency = upper(currency)),
  price_reference_year INTEGER NOT NULL CHECK (price_reference_year BETWEEN 1900 AND 2200),
  configuration_basis TEXT NOT NULL DEFAULT 'unknown' CHECK (configuration_basis IN (
    'full_standard_configuration', 'base_aircraft_only', 'unknown'
  )),
  evidence_kind TEXT NOT NULL CHECK (evidence_kind IN (
    'direct_model_year', 'direct_other_year', 'interpolated', 'inferred'
  )),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, price_kind, currency)
)
"#;
#[derive(Clone)]
pub struct AppDb {
    backend: DatabaseBackend,
}

#[derive(Clone)]
pub(crate) enum DatabaseBackend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

enum GateConnection<'connection> {
    Sqlite(&'connection mut SqliteConnection),
    Postgres(&'connection mut PgConnection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DatabaseKind {
    Sqlite,
    Postgres,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MigrationContractState {
    Fresh,
    Installed,
    Invalid,
}

#[derive(sqlx::FromRow)]
struct PostgresCorrectionTriggerDefinition {
    trigger_name: String,
    trigger_type: i16,
    has_no_when_clause: bool,
    update_columns: String,
    trigger_enabled: String,
    relation_schema: String,
    relation_name: String,
    relation_oid_matches: bool,
    function_name: String,
    function_schema: String,
    function_oid_matches: bool,
    function_source: String,
    function_configuration: String,
    function_language: String,
    returns_trigger: bool,
    argument_count: i16,
    security_definer: bool,
    strict: bool,
    volatility: String,
}

#[derive(sqlx::FromRow)]
struct PostgresFaaReferenceTriggerDefinition {
    trigger_name: String,
    trigger_type: i16,
    has_no_when_clause: bool,
    has_no_update_columns: bool,
    trigger_enabled: String,
    trigger_argument_count: i16,
    relation_schema: String,
    relation_name: String,
    relation_oid_matches: bool,
    function_name: String,
    function_schema: String,
    function_oid_matches: bool,
    function_source: String,
    function_configuration: String,
    function_language: String,
    returns_trigger: bool,
    function_argument_count: i16,
    ordinary_function: bool,
    security_definer: bool,
    strict: bool,
    volatility: String,
    parallel_safety: String,
}

#[derive(sqlx::FromRow)]
struct PostgresApprovedConcreteModelTriggerDefinition {
    trigger_name: String,
    trigger_type: i16,
    has_no_when_clause: bool,
    update_columns: String,
    trigger_enabled: String,
    trigger_argument_count: i16,
    relation_schema: String,
    relation_name: String,
    relation_oid_matches: bool,
    function_name: String,
    function_schema: String,
    function_oid_matches: bool,
    function_source: String,
    function_configuration: String,
    function_language: String,
    returns_trigger: bool,
    function_argument_count: i16,
    ordinary_function: bool,
    security_definer: bool,
    strict: bool,
    volatility: String,
    parallel_safety: String,
}

#[derive(sqlx::FromRow)]
struct PostgresReferenceRoutineDefinition {
    function_name: String,
    function_oid_matches: bool,
    function_source: String,
    function_configuration: String,
    function_language: String,
    result_type: String,
    identity_arguments: String,
    security_definer: bool,
    strict: bool,
    volatility: String,
    parallel_safety: String,
}

#[derive(sqlx::FromRow)]
struct PostgresFaaRegistryShape {
    relation_signature: Option<String>,
    column_signature: Option<String>,
    constraint_signature: Option<String>,
    foreign_key_signature: Option<String>,
    index_signature: Option<String>,
}

#[derive(sqlx::FromRow)]
struct SqliteSchemaDefinition {
    object_type: String,
    name: String,
    sql: Option<String>,
}

const SQLITE_FAA_REGISTRY_OBJECTS: [(&str, &str); 24] = [
    ("table", "faa_registry_aircraft"),
    ("table", "faa_registry_aircraft_references"),
    ("table", "faa_registry_coverage"),
    ("table", "faa_registry_engine_references"),
    ("table", "faa_registry_snapshots"),
    ("index", "idx_faa_registry_aircraft_code"),
    ("index", "idx_faa_registry_aircraft_lineage_record"),
    ("index", "idx_faa_registry_coverage_lookup"),
    ("index", "idx_faa_registry_engine_code"),
    ("index", "idx_faa_registry_snapshots_current"),
    ("trigger", "faa_registry_aircraft_immutable_delete"),
    ("trigger", "faa_registry_aircraft_immutable_update"),
    (
        "trigger",
        "faa_registry_aircraft_references_immutable_delete",
    ),
    (
        "trigger",
        "faa_registry_aircraft_references_immutable_update",
    ),
    ("trigger", "faa_registry_aircraft_references_reachable"),
    ("trigger", "faa_registry_coverage_consistent"),
    ("trigger", "faa_registry_coverage_immutable_delete"),
    ("trigger", "faa_registry_coverage_immutable_update"),
    ("trigger", "faa_registry_engine_references_immutable_delete"),
    ("trigger", "faa_registry_engine_references_immutable_update"),
    ("trigger", "faa_registry_engine_references_reachable"),
    ("trigger", "faa_registry_snapshots_immutable_delete"),
    ("trigger", "faa_registry_snapshots_immutable_update"),
    ("trigger", "faa_registry_snapshots_require_exact_evidence"),
];

#[derive(sqlx::FromRow)]
struct PostgresReferenceColumnDefinition {
    relation_name: String,
    ordinal: i16,
    column_name: String,
    data_type: String,
    not_null: bool,
    identity_kind: String,
    default_expression: String,
}

#[derive(sqlx::FromRow)]
struct PostgresReferenceConstraintDefinition {
    relation_name: String,
    constraint_type: String,
    definition: String,
}

#[derive(sqlx::FromRow)]
struct PostgresGroundedCapabilityTriggerDefinition {
    trigger_name: String,
    trigger_type: i16,
    has_no_when_clause: bool,
    is_enabled: bool,
    has_zero_trigger_args: bool,
    is_uninherited: bool,
    is_non_constraint: bool,
    has_no_transition_tables: bool,
    has_no_update_columns: bool,
    definition: String,
    function_namespace: String,
    function_name: String,
    function_source: String,
    function_config: String,
    is_security_invoker: bool,
    is_ordinary_function: bool,
    returns_trigger: bool,
    has_zero_function_args: bool,
    language_name: String,
    is_not_strict: bool,
    is_volatile: bool,
    is_parallel_unsafe: bool,
    is_not_leakproof: bool,
}

fn canonical_sql_definition(value: &str) -> String {
    let mut canonical = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    let mut quote = None;
    while let Some(character) = characters.next() {
        if let Some(active_quote) = quote {
            canonical.push(character);
            if character == active_quote {
                if characters.peek() == Some(&active_quote) {
                    canonical.push(characters.next().expect("peeked quote must exist"));
                } else {
                    quote = None;
                }
            }
            continue;
        }
        if character == '\'' || character == '"' {
            quote = Some(character);
            canonical.push(character);
        } else if !character.is_whitespace() {
            canonical.extend(character.to_lowercase());
        }
    }
    canonical
}

fn canonical_sqlite_schema_definition(value: &str) -> String {
    let canonical = canonical_sql_definition(value);
    for prefix in [
        "createtableifnotexists",
        "createindexifnotexists",
        "createuniqueindexifnotexists",
        "createtriggerifnotexists",
    ] {
        if let Some(remainder) = canonical.strip_prefix(prefix) {
            let replacement = prefix.replace("ifnotexists", "");
            return format!("{replacement}{remainder}");
        }
    }
    canonical
}

fn postgres_approved_concrete_model_object_payload(
    actual: &PostgresApprovedConcreteModelTriggerDefinition,
) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        actual.trigger_name,
        actual.trigger_type,
        actual.has_no_when_clause,
        actual.update_columns,
        actual.trigger_enabled,
        actual.trigger_argument_count,
        actual.relation_schema,
        actual.relation_name,
        actual.relation_oid_matches,
        actual.function_name,
        actual.function_schema,
        actual.function_oid_matches,
        canonical_sql_definition(&actual.function_source),
        actual.function_configuration,
        actual.function_language,
        actual.returns_trigger,
        actual.function_argument_count,
        actual.ordinary_function,
        actual.security_definer,
        actual.strict,
        actual.volatility,
        actual.parallel_safety,
    )
}

fn versioned_avionics_approved_concrete_model_object_fingerprint(payload: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!(
            "v{}\n{}",
            AVIONICS_APPROVED_CONCRETE_MODEL_OBJECT_CONTRACT_VERSION, payload
        ))
    )
}

fn expected_sqlite_faa_registry_definitions() -> Vec<SqliteSchemaDefinition> {
    SQLITE_FAA_REGISTRY_OBJECTS
        .iter()
        .map(|(object_type, name)| {
            let expected_prefixes = match *object_type {
                "table" => vec![format!("createtable{name}(")],
                "index" => vec![
                    format!("createindex{name}on"),
                    format!("createuniqueindex{name}on"),
                ],
                "trigger" => vec![format!("createtrigger{name}")],
                _ => unreachable!("FAA SQLite object type is fixed"),
            };
            let definition = split_sql_statements(SQLITE_SCHEMA_SQL)
                .into_iter()
                .map(strip_leading_sql_comments)
                .map(canonical_sqlite_schema_definition)
                .find(|statement| {
                    expected_prefixes
                        .iter()
                        .any(|prefix| statement.starts_with(prefix))
                })
                .unwrap_or_else(|| panic!("canonical SQLite schema is missing {name}"));
            SqliteSchemaDefinition {
                object_type: (*object_type).to_owned(),
                name: (*name).to_owned(),
                sql: Some(definition),
            }
        })
        .collect()
}

fn sqlite_table_definition<'a>(schema: &'a str, table: &str) -> Option<&'a str> {
    let marker = format!("CREATE TABLE IF NOT EXISTS {table} (");
    let qualified_marker = format!("CREATE TABLE IF NOT EXISTS public.{table} (");
    let start = schema
        .find(&marker)
        .or_else(|| schema.find(&qualified_marker))?;
    let tail = &schema[start..];
    let mut depth = 0_i64;
    let mut quote = None;
    let mut characters = tail.char_indices().peekable();
    while let Some((offset, character)) = characters.next() {
        if let Some(active_quote) = quote {
            if character == active_quote {
                if characters
                    .peek()
                    .is_some_and(|(_, next)| *next == active_quote)
                {
                    characters.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&tail[..=offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn canonical_table_definition(schema: &str, table: &str) -> Option<String> {
    Some(
        canonical_sql_definition(sqlite_table_definition(schema, table)?)
            .replacen("createtableifnotexists", "createtable", 1)
            .replace("public.", ""),
    )
}

fn canonical_sqlite_table_definition(schema: &str, table: &str) -> Option<String> {
    canonical_table_definition(schema, table)
}

fn canonical_sqlite_named_definition(schema: &str, name: &str) -> Option<String> {
    let canonical_name = name.to_ascii_lowercase();
    split_sql_statements(schema)
        .into_iter()
        .map(strip_leading_sql_comments)
        .map(canonical_sqlite_schema_definition)
        .find(|statement| {
            statement.starts_with(&format!("createindex{canonical_name}on"))
                || statement.starts_with(&format!("createuniqueindex{canonical_name}on"))
                || statement.starts_with(&format!("createtrigger{canonical_name}"))
        })
}

fn postgres_function_source<'a>(schema: &'a str, function_name: &str) -> Option<&'a str> {
    let marker = format!("CREATE OR REPLACE FUNCTION public.{function_name}()");
    let wrapped_marker = format!("CREATE OR REPLACE FUNCTION\n  public.{function_name}()");
    let declaration = schema
        .split_once(&marker)
        .or_else(|| schema.split_once(&wrapped_marker))?
        .1;
    let body = declaration.split_once("AS $function$")?.1;
    body.split_once("$function$;")
        .map(|(source, _)| source.trim())
}

fn canonical_postgres_named_trigger_definition(schema: &str, name: &str) -> Option<String> {
    let marker = format!("createtrigger{}", name.to_ascii_lowercase());
    split_sql_statements(schema)
        .into_iter()
        .map(strip_leading_sql_comments)
        .map(canonical_postgres_trigger_definition)
        .find(|statement| statement.starts_with(&marker))
}

fn postgres_migration_function_source(function_name: &str) -> Option<&'static str> {
    let inline_marker = format!("CREATE OR REPLACE FUNCTION public.{function_name}");
    let wrapped_marker = format!("CREATE OR REPLACE FUNCTION\n  public.{function_name}");
    let declaration = REFERENCE_CATALOG_CUTOVER_POSTGRES_MIGRATION_SQL
        .split_once(&inline_marker)
        .or_else(|| REFERENCE_CATALOG_CUTOVER_POSTGRES_MIGRATION_SQL.split_once(&wrapped_marker))?
        .1;
    let (body, delimiter) = if let Some((_, body)) = declaration.split_once("AS $function$") {
        (body, "$function$")
    } else if let Some((_, body)) = declaration.split_once("AS $$") {
        (body, "$$")
    } else {
        return None;
    };
    body.split_once(delimiter).map(|(source, _)| source.trim())
}

fn postgres_reference_owned_objects_query() -> Option<&'static str> {
    REFERENCE_CATALOG_CUTOVER_POSTGRES_MIGRATION_SQL
        .split_once("AS $owned_objects$")?
        .1
        .split_once("$owned_objects$;")
        .map(|(query, _)| query.trim())
}

fn sqlite_migration_definition(object_kind: &str, object_name: &str) -> Option<&'static str> {
    let marker = format!("CREATE {object_kind} {object_name}");
    let idempotent_marker = format!("CREATE {object_kind} IF NOT EXISTS {object_name}");
    let start = REFERENCE_CATALOG_CUTOVER_SQLITE_MIGRATION_SQL
        .find(&marker)
        .or_else(|| REFERENCE_CATALOG_CUTOVER_SQLITE_MIGRATION_SQL.find(&idempotent_marker))?;
    let definition = &REFERENCE_CATALOG_CUTOVER_SQLITE_MIGRATION_SQL[start..];
    let terminator = match object_kind {
        "TABLE" => "\n);",
        "TRIGGER" => "END;",
        _ => return None,
    };
    let end = definition.find(terminator)? + terminator.len() - 1;
    Some(&definition[..end])
}

fn postgres_schema_reference_trigger_definitions() -> Vec<String> {
    let routine_markers = REFERENCE_CATALOG_CUTOVER_ROUTINES
        .iter()
        .filter(|name| **name != "aircraft_serial_natural_sort_key")
        .map(|name| format!("executefunctionpublic.{name}()"))
        .collect::<Vec<_>>();
    let relation_markers = REFERENCE_CATALOG_CUTOVER_PROTECTED_RELATIONS
        .iter()
        .flat_map(|name| [format!("onpublic.{name}"), format!("on{name}")])
        .collect::<Vec<_>>();
    let mut definitions = split_sql_statements(POSTGRES_SCHEMA_SQL)
        .into_iter()
        .map(|statement| canonical_sql_definition(statement))
        .filter(|canonical_statement| {
            routine_markers
                .iter()
                .any(|marker| canonical_statement.contains(marker))
                || relation_markers
                    .iter()
                    .any(|marker| canonical_statement.contains(marker))
        })
        .filter_map(|canonical_statement| {
            canonical_statement
                .find("createtrigger")
                .map(|start| canonical_statement[start..].to_string())
        })
        .map(|definition| canonical_postgres_trigger_definition(&definition))
        .collect::<Vec<_>>();
    definitions.sort();
    definitions
}

fn canonical_postgres_trigger_definition(value: &str) -> String {
    canonical_sql_definition(value)
        .replace("public.", "")
        // pg_get_triggerdef emits multi-event trigger operations in canonical
        // PostgreSQL order, which can differ from the order used in DDL.
        .replace("beforeupdateordeleteon", "beforedeleteorupdateon")
        .replace("afterupdateordeleteon", "afterdeleteorupdateon")
}

fn postgres_pool_options(max_connections: u32) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .after_connect(|connection, _metadata| {
            Box::pin(async move {
                sqlx::query(
                    "SELECT pg_catalog.set_config( \
                       'search_path', 'public,pg_catalog,pg_temp', false \
                     )",
                )
                .execute(connection)
                .await?;
                Ok(())
            })
        })
}

impl AppDb {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let database_url = normalize_database_url(database_url);
        if is_postgres_url(&database_url) {
            let options = PgConnectOptions::from_str(&database_url)
                .with_context(|| format!("invalid Postgres database URL {database_url}"))?
                .options([("search_path", POSTGRES_SEARCH_PATH)]);
            let pool = postgres_pool_options(5)
                .connect_with(options)
                .await
                .with_context(|| {
                    format!("could not connect to Postgres database {database_url}")
                })?;
            let db = Self {
                backend: DatabaseBackend::Postgres(pool),
            };
            db.initialize_transactionally().await?;
            Ok(db)
        } else {
            ensure_sqlite_parent_directory(&database_url)?;
            let options = SqliteConnectOptions::from_str(&database_url)
                .with_context(|| format!("invalid SQLite database URL {database_url}"))?
                .create_if_missing(true)
                .foreign_keys(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(options)
                .await
                .with_context(|| format!("could not connect to SQLite database {database_url}"))?;
            let db = Self {
                backend: DatabaseBackend::Sqlite(pool),
            };
            db.initialize_transactionally().await?;
            Ok(db)
        }
    }

    /// Opens an existing database for diagnostic and export commands without
    /// schema initialization, migrations, seed writes, or writable
    /// transactions. SQLite opens the file read-only and PostgreSQL makes
    /// read-only the default for every connection in the pool.
    pub async fn connect_diagnostic(database_url: &str) -> Result<Self> {
        let database_url = normalize_database_url(database_url);
        if is_postgres_url(&database_url) {
            let options = PgConnectOptions::from_str(&database_url)
                .with_context(|| format!("invalid Postgres database URL {database_url}"))?
                .options([("search_path", POSTGRES_SEARCH_PATH)]);
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .after_connect(|connection, _metadata| {
                    Box::pin(async move {
                        sqlx::query(
                            "SELECT pg_catalog.set_config( \
                               'search_path', 'public,pg_catalog,pg_temp', false \
                             )",
                        )
                        .execute(&mut *connection)
                        .await?;
                        sqlx::query("SET default_transaction_read_only = on")
                            .execute(connection)
                            .await?;
                        Ok(())
                    })
                })
                .connect_with(options)
                .await
                .with_context(|| {
                    format!("could not open diagnostic Postgres database {database_url}")
                })?;
            let db = Self {
                backend: DatabaseBackend::Postgres(pool),
            };
            db.ensure_required_migrations().await?;
            return Ok(db);
        }
        let options = SqliteConnectOptions::from_str(&database_url)
            .with_context(|| format!("invalid SQLite database URL {database_url}"))?
            .create_if_missing(false)
            .read_only(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .with_context(|| format!("could not open diagnostic SQLite database {database_url}"))?;
        let db = Self {
            backend: DatabaseBackend::Sqlite(pool),
        };
        db.ensure_required_migrations().await?;
        Ok(db)
    }

    pub(crate) fn backend(&self) -> &DatabaseBackend {
        &self.backend
    }

    pub async fn close(self) {
        match self.backend {
            DatabaseBackend::Sqlite(pool) => pool.close().await,
            DatabaseBackend::Postgres(pool) => pool.close().await,
        }
    }

    pub(crate) fn kind(&self) -> DatabaseKind {
        match self.backend {
            DatabaseBackend::Sqlite(_) => DatabaseKind::Sqlite,
            DatabaseBackend::Postgres(_) => DatabaseKind::Postgres,
        }
    }

    pub(crate) fn sql<'a>(&self, sqlite_sql: &'a str) -> Cow<'a, str> {
        match self.kind() {
            DatabaseKind::Sqlite => Cow::Borrowed(sqlite_sql),
            DatabaseKind::Postgres => Cow::Owned(postgres_placeholders(sqlite_sql)),
        }
    }

    pub async fn current_user(&self, identity: Option<&str>) -> Result<User> {
        let identity = identity.unwrap_or(DEVELOPER_EMAIL);
        let sql = self.sql(
            r#"
            SELECT id, email, display_name, auth_provider, auth_subject
            FROM users
            WHERE email = ? OR auth_subject = ?
            "#,
        );
        let user = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, User>(&sql)
                    .bind(identity)
                    .bind(identity)
                    .fetch_optional(pool)
                    .await?
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, User>(&sql)
                    .bind(identity)
                    .bind(identity)
                    .fetch_optional(pool)
                    .await?
            }
        };
        user.with_context(|| format!("unknown user: {identity}"))
    }

    async fn ensure_required_migrations(&self) -> Result<()> {
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Sqlite(&mut connection);
                self.ensure_required_migrations_on(&mut connection).await
            }
            DatabaseBackend::Postgres(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Postgres(&mut connection);
                self.ensure_required_migrations_on(&mut connection).await
            }
        }
    }

    async fn avionics_approved_concrete_model_object_contract_valid_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<bool> {
        let (valid_shape, payload, expected_fingerprint) = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                let definitions = sqlx::query_as::<_, (String, Option<String>)>(
                    r#"
                    SELECT name, sql
                    FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND tbl_name = 'avionics_models'
                      AND name IN (
                        'avionics_models_approved_concrete_model_insert',
                        'avionics_models_approved_concrete_model_update'
                      )
                    ORDER BY name
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                let valid_shape = definitions.len() == 2
                    && definitions
                        .iter()
                        .all(|(_, definition)| definition.is_some());
                let payload = definitions
                    .iter()
                    .map(|(name, definition)| {
                        format!(
                            "{name}={}",
                            definition
                                .as_deref()
                                .map(canonical_sqlite_schema_definition)
                                .unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (
                    valid_shape,
                    payload,
                    SQLITE_AVIONICS_APPROVED_CONCRETE_MODEL_OBJECT_CONTRACT_FINGERPRINT,
                )
            }
            GateConnection::Postgres(pool) => {
                let definitions =
                    sqlx::query_as::<_, PostgresApprovedConcreteModelTriggerDefinition>(
                        r#"
                    SELECT
                      actual_trigger.tgname AS trigger_name,
                      actual_trigger.tgtype::smallint AS trigger_type,
                      actual_trigger.tgqual IS NULL AS has_no_when_clause,
                      COALESCE((
                        SELECT pg_catalog.string_agg(
                          attribute.attname, ',' ORDER BY attribute.attname
                        )
                        FROM pg_catalog.unnest(actual_trigger.tgattr)
                          AS update_column(attnum)
                        JOIN pg_catalog.pg_attribute attribute
                          ON attribute.attrelid = actual_trigger.tgrelid
                         AND attribute.attnum = update_column.attnum
                      ), '') AS update_columns,
                      actual_trigger.tgenabled::text AS trigger_enabled,
                      actual_trigger.tgnargs::smallint AS trigger_argument_count,
                      relation_namespace.nspname AS relation_schema,
                      relation.relname AS relation_name,
                      actual_trigger.tgrelid = pg_catalog.to_regclass(
                        'public.avionics_models'
                      ) AS relation_oid_matches,
                      routine.proname AS function_name,
                      routine_namespace.nspname AS function_schema,
                      routine.oid = pg_catalog.to_regprocedure(
                        'public.enforce_avionics_approved_concrete_model()'
                      ) AS function_oid_matches,
                      routine.prosrc AS function_source,
                      COALESCE(
                        pg_catalog.array_to_string(routine.proconfig, E'\n'), ''
                      ) AS function_configuration,
                      language.lanname AS function_language,
                      routine.prorettype = 'trigger'::regtype AS returns_trigger,
                      routine.pronargs::smallint AS function_argument_count,
                      routine.prokind = 'f' AS ordinary_function,
                      routine.prosecdef AS security_definer,
                      routine.proisstrict AS strict,
                      routine.provolatile::text AS volatility,
                      routine.proparallel::text AS parallel_safety
                    FROM pg_catalog.pg_trigger actual_trigger
                    JOIN pg_catalog.pg_class relation
                      ON relation.oid = actual_trigger.tgrelid
                    JOIN pg_catalog.pg_namespace relation_namespace
                      ON relation_namespace.oid = relation.relnamespace
                    JOIN pg_catalog.pg_proc routine
                      ON routine.oid = actual_trigger.tgfoid
                    JOIN pg_catalog.pg_namespace routine_namespace
                      ON routine_namespace.oid = routine.pronamespace
                    JOIN pg_catalog.pg_language language
                      ON language.oid = routine.prolang
                    WHERE NOT actual_trigger.tgisinternal
                      AND actual_trigger.tgname =
                        'avionics_models_approved_concrete_model'
                      AND relation_namespace.nspname = 'public'
                      AND relation.relname = 'avionics_models'
                    "#,
                    )
                    .fetch_all(&mut **pool)
                    .await?;
                let valid_shape = definitions.as_slice().first().is_some_and(|actual| {
                    definitions.len() == 1
                        && actual.trigger_name == "avionics_models_approved_concrete_model"
                        && actual.trigger_type == 23
                        && actual.has_no_when_clause
                        && actual.update_columns == "catalog_status,normalized_name"
                        && actual.trigger_enabled == "O"
                        && actual.trigger_argument_count == 0
                        && actual.relation_schema == "public"
                        && actual.relation_name == "avionics_models"
                        && actual.relation_oid_matches
                        && actual.function_name == "enforce_avionics_approved_concrete_model"
                        && actual.function_schema == "public"
                        && actual.function_oid_matches
                        && canonical_sql_definition(&actual.function_source)
                            == canonical_sql_definition(
                                POSTGRES_AVIONICS_APPROVED_CONCRETE_MODEL_FUNCTION_SOURCE,
                            )
                        && actual.function_configuration.is_empty()
                        && actual.function_language == "plpgsql"
                        && actual.returns_trigger
                        && actual.function_argument_count == 0
                        && actual.ordinary_function
                        && !actual.security_definer
                        && !actual.strict
                        && actual.volatility == "v"
                        && actual.parallel_safety == "u"
                });
                let payload = definitions
                    .first()
                    .map(postgres_approved_concrete_model_object_payload)
                    .unwrap_or_default();
                (
                    valid_shape,
                    payload,
                    POSTGRES_AVIONICS_APPROVED_CONCRETE_MODEL_OBJECT_CONTRACT_FINGERPRINT,
                )
            }
        };
        let fingerprint = versioned_avionics_approved_concrete_model_object_fingerprint(&payload);
        #[cfg(test)]
        if !valid_shape || fingerprint != expected_fingerprint {
            eprintln!(
                "approved concrete-model object contract mismatch shape={valid_shape} fingerprint={fingerprint} expected={expected_fingerprint}"
            );
        }
        Ok(valid_shape && fingerprint == expected_fingerprint)
    }

    async fn ensure_required_migrations_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<()> {
        let missing_valuation_hardening = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                SELECT
                  EXISTS (
                    SELECT 1
                    FROM sqlite_schema
                    WHERE type = 'table' AND name = 'aircraft_sale_listings'
                  )
                  AND NOT EXISTS (
                    SELECT 1
                    FROM pragma_table_info('aircraft_sale_listings')
                    WHERE name = 'ingestion_state'
                  )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                SELECT
                  to_regclass('aircraft_sale_listings') IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = to_regclass('aircraft_sale_listings')
                      AND attname = 'ingestion_state'
                      AND NOT attisdropped
                  )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        if missing_valuation_hardening {
            bail!(migration_required_message(
                self.kind(),
                "aircraft_sale_listings",
                "ingestion_state",
                VALUATION_DATA_HARDENING_MIGRATION,
            ));
        }

        let missing_avionics_curation = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                SELECT
                  EXISTS (
                    SELECT 1
                    FROM sqlite_schema
                    WHERE type = 'table' AND name = 'avionics_models'
                  )
                  AND NOT EXISTS (
                    SELECT 1
                    FROM pragma_table_info('avionics_models')
                    WHERE name = 'catalog_status'
                  )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                SELECT
                  to_regclass('avionics_models') IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = to_regclass('avionics_models')
                      AND attname = 'catalog_status'
                      AND NOT attisdropped
                  )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        if missing_avionics_curation {
            bail!(migration_required_message(
                self.kind(),
                "avionics_models",
                "catalog_status",
                AVIONICS_CATALOG_CURATION_MIGRATION,
            ));
        }

        let missing_avionics_multi_type = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                SELECT
                  EXISTS (
                    SELECT 1
                    FROM sqlite_schema
                    WHERE type = 'table' AND name = 'avionics_models'
                  )
                  AND (
                    NOT EXISTS (
                      SELECT 1
                      FROM sqlite_schema
                      WHERE type = 'table' AND name = 'avionics_model_types'
                    )
                    OR EXISTS (
                      SELECT 1
                      FROM pragma_table_info('avionics_models')
                      WHERE name = 'avionics_type_id'
                    )
                  )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                SELECT
                  to_regclass('avionics_models') IS NOT NULL
                  AND (
                    to_regclass('avionics_model_types') IS NULL
                    OR EXISTS (
                      SELECT 1
                      FROM pg_attribute
                      WHERE attrelid = to_regclass('avionics_models')
                        AND attname = 'avionics_type_id'
                        AND NOT attisdropped
                    )
                  )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        if missing_avionics_multi_type {
            bail!(avionics_multi_type_migration_required_message(self.kind()));
        }

        let missing_aircraft_reference_catalog = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table' AND name = 'aircraft_sale_listings'
                      )
                      AND (
                        NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'aircraft_identity_observations'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'aircraft_engine_catalog_models'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'aircraft_propeller_catalog_models'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_snapshots'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_aircraft'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_aircraft_references'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_engine_references'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'faa_registry_coverage'
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT
                      to_regclass('aircraft_sale_listings') IS NOT NULL
                      AND (
                        to_regclass('aircraft_identity_observations') IS NULL
                        OR to_regclass('aircraft_engine_catalog_models') IS NULL
                        OR to_regclass('aircraft_propeller_catalog_models') IS NULL
                        OR to_regclass('faa_registry_snapshots') IS NULL
                        OR to_regclass('faa_registry_aircraft') IS NULL
                        OR to_regclass('faa_registry_aircraft_references') IS NULL
                        OR to_regclass('faa_registry_engine_references') IS NULL
                        OR to_regclass('faa_registry_coverage') IS NULL
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        if missing_aircraft_reference_catalog {
            bail!(aircraft_reference_catalog_migration_required_message(
                self.kind()
            ));
        }

        let missing_listing_pending_reviews = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table' AND name = 'aircraft_sale_listings'
                      )
                      AND (
                        NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table'
                            AND name = 'aircraft_sale_listing_pending_reviews'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table' AND name = 'aircraft_sale_listings'
                            AND lower(sql) LIKE '%pending_review%'
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT
                      to_regclass('aircraft_sale_listings') IS NOT NULL
                      AND (
                        to_regclass('aircraft_sale_listing_pending_reviews') IS NULL
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pg_constraint constraint_row
                          WHERE constraint_row.conrelid = to_regclass('aircraft_sale_listings')
                            AND constraint_row.contype = 'c'
                            AND lower(pg_get_constraintdef(constraint_row.oid))
                              LIKE '%pending_review%'
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        if missing_listing_pending_reviews {
            bail!(listing_pending_reviews_migration_required_message(
                self.kind()
            ));
        }

        let missing_identity_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_objects(object_type, object_name, parent_name) AS (
                      VALUES
                        ('table', 'avionics_manufacturer_canonical_keys', NULL),
                        ('table', 'avionics_manufacturer_identities', NULL),
                        ('table', 'avionics_manufacturer_identity_memberships', NULL),
                        ('table', 'avionics_manufacturer_alias_candidates', NULL),
                        ('table', 'avionics_manufacturer_identity_merges', NULL),
                        ('table', 'avionics_approved_product_identities', NULL),
                        ('table', 'avionics_catalog_consolidation_guard', NULL),
                        ('table', 'avionics_manufacturer_canonical_key_delete_context', NULL),
                        ('view', 'avionics_catalog_authorized_consolidations', NULL),
                        ('view', 'avionics_approved_product_graph_identities', NULL),
                        ('view', 'avionics_manufacturer_effective_identities', NULL),
                        ('view', 'avionics_manufacturer_effective_memberships', NULL),
                        ('view', 'avionics_manufacturer_normalization_contract', NULL),
                        ('view', 'avionics_semantic_duplicate_listing_links', NULL),
                        ('view', 'avionics_semantic_invalid_replacement_links', NULL),
                        ('view', 'avionics_semantic_duplicate_displacement_targets', NULL),
                        ('view', 'avionics_semantic_installed_displacement_conflicts', NULL),
                        ('view', 'avionics_semantic_invalid_listing_action_graphs', NULL),
                        ('index', 'idx_avionics_models_manufacturer_identifier', 'avionics_models'),
                        ('index', 'idx_aircraft_sale_listing_avionics_unique_displacement',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'avionics_manufacturer_membership_validate_insert',
                          'avionics_manufacturer_identity_memberships'),
                        ('trigger', 'avionics_manufacturer_membership_immutable_update',
                          'avionics_manufacturer_identity_memberships'),
                        ('trigger', 'avionics_manufacturer_membership_immutable_delete',
                          'avionics_manufacturer_identity_memberships'),
                        ('trigger', 'avionics_manufacturer_alias_membership_requires_decision',
                          'avionics_manufacturer_identity_memberships'),
                        ('trigger', 'avionics_manufacturer_identity_immutable_update',
                          'avionics_manufacturer_identities'),
                        ('trigger', 'avionics_manufacturer_identity_immutable_delete',
                          'avionics_manufacturer_identities'),
                        ('trigger', 'avionics_manufacturer_identity_name_immutable',
                          'avionics_manufacturers'),
                        ('trigger', 'avionics_manufacturer_alias_candidate_pending_insert',
                          'avionics_manufacturer_alias_candidates'),
                        ('trigger', 'avionics_manufacturer_alias_candidate_update',
                          'avionics_manufacturer_alias_candidates'),
                        ('trigger', 'avionics_manufacturer_alias_candidate_delete',
                          'avionics_manufacturer_alias_candidates'),
                        ('trigger', 'avionics_manufacturer_identity_merge_validate',
                          'avionics_manufacturer_identity_merges'),
                        ('trigger', 'avionics_manufacturer_identity_merge_apply',
                          'avionics_manufacturer_identity_merges'),
                        ('trigger', 'avionics_manufacturer_identity_merge_immutable_update',
                          'avionics_manufacturer_identity_merges'),
                        ('trigger', 'avionics_manufacturer_identity_merge_immutable_delete',
                          'avionics_manufacturer_identity_merges'),
                        ('trigger', 'avionics_catalog_consolidation_guard_validate_insert',
                          'avionics_catalog_consolidation_guard'),
                        ('trigger', 'avionics_catalog_consolidation_guard_immutable',
                          'avionics_catalog_consolidation_guard'),
                        ('trigger', 'avionics_manufacturer_canonical_key_delete',
                          'avionics_manufacturer_canonical_keys'),
                        ('trigger', 'avionics_manufacturer_canonical_key_immutable',
                          'avionics_manufacturer_canonical_keys'),
                        ('trigger', 'avionics_manufacturer_canonical_key_insert',
                          'avionics_manufacturers'),
                        ('trigger', 'avionics_manufacturer_normalized_name_preserve_key',
                          'avionics_manufacturers'),
                        ('trigger', 'avionics_manufacturer_canonical_key_delete_begin',
                          'avionics_manufacturers'),
                        ('trigger', 'avionics_manufacturer_canonical_key_delete_end',
                          'avionics_manufacturers'),
                        ('trigger', 'avionics_models_consolidation_identity_immutable',
                          'avionics_models'),
                        ('trigger', 'avionics_models_approved_identity_immutable',
                          'avionics_models'),
                        ('trigger', 'avionics_models_approved_delete_guard',
                          'avionics_models'),
                        ('trigger', 'avionics_models_approved_types_insert',
                          'avionics_models'),
                        ('trigger', 'avionics_models_approved_types_update',
                          'avionics_models'),
                        ('trigger', 'avionics_models_referenced_status_update',
                          'avionics_models'),
                        ('trigger', 'avionics_model_types_preserve_approved_delete',
                          'avionics_model_types'),
                        ('trigger', 'avionics_model_types_preserve_approved_update',
                          'avionics_model_types'),
                        ('trigger', 'avionics_suite_components_approved_insert',
                          'avionics_suite_components'),
                        ('trigger', 'avionics_suite_components_approved_update',
                          'avionics_suite_components'),
                        ('trigger', 'avionics_models_canonical_identity_validate_update',
                          'avionics_models'),
                        ('trigger', 'avionics_models_canonical_identity_sync_update',
                          'avionics_models'),
                        ('trigger', 'avionics_approved_identity_validate_insert',
                          'avionics_approved_product_identities'),
                        ('trigger', 'avionics_approved_identity_validate_update',
                          'avionics_approved_product_identities'),
                        ('trigger', 'avionics_approved_identity_preserve_delete',
                          'avionics_approved_product_identities'),
                        ('trigger', 'aircraft_sale_listing_avionics_approved_update',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_approved_insert',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_mutable_insert',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_mutable_update',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_mutable_delete',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_distinct_replacement_insert',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_distinct_replacement_update',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_semantic_unique_insert',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_semantic_unique_update',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_action_graph_insert',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_sale_listing_avionics_action_graph_update',
                          'aircraft_sale_listing_avionics'),
                        ('trigger', 'aircraft_reference_avionics_building_insert',
                          'aircraft_reference_avionics'),
                        ('trigger', 'aircraft_reference_avionics_immutable_update',
                          'aircraft_reference_avionics'),
                        ('trigger', 'aircraft_reference_avionics_immutable_delete',
                          'aircraft_reference_avionics'),
                        ('trigger', 'aircraft_sale_listings_ready_semantic_avionics',
                          'aircraft_sale_listings'),
                        ('trigger', 'aircraft_sale_listings_ready_semantic_avionics_insert',
                          'aircraft_sale_listings'),
                        ('trigger', 'listing_verified_requires_ready_insert',
                          'aircraft_sale_listings'),
                        ('trigger', 'listing_verified_requires_ready_update',
                          'aircraft_sale_listings')
                    )
                    SELECT
                      EXISTS (SELECT 1 FROM sqlite_schema WHERE name = 'avionics_models')
                      AND EXISTS (
                        SELECT 1
                        FROM required_objects required
                        WHERE NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema actual
                          WHERE actual.type = required.object_type
                            AND actual.name = required.object_name
                            AND (
                              required.parent_name IS NULL
                              OR actual.tbl_name = required.parent_name
                            )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_relations(object_name, relation_kind) AS (
                      VALUES
                        ('avionics_manufacturer_canonical_keys', 'r'),
                        ('avionics_manufacturer_identities', 'r'),
                        ('avionics_manufacturer_identity_memberships', 'r'),
                        ('avionics_manufacturer_alias_candidates', 'r'),
                        ('avionics_manufacturer_identity_merges', 'r'),
                        ('avionics_approved_product_identities', 'r'),
                        ('avionics_catalog_consolidation_guard', 'r'),
                        ('avionics_catalog_authorized_consolidations', 'v'),
                        ('avionics_approved_product_graph_identities', 'v'),
                        ('avionics_manufacturer_effective_identities', 'v'),
                        ('avionics_manufacturer_effective_memberships', 'v'),
                        ('avionics_manufacturer_normalization_contract', 'v'),
                        ('avionics_semantic_duplicate_listing_links', 'v'),
                        ('avionics_semantic_invalid_replacement_links', 'v'),
                        ('avionics_semantic_duplicate_displacement_targets', 'v'),
                        ('avionics_semantic_installed_displacement_conflicts', 'v'),
                        ('avionics_semantic_invalid_listing_action_graphs', 'v'),
                        ('idx_avionics_models_manufacturer_identifier', 'i'),
                        ('idx_aircraft_sale_listing_avionics_unique_displacement', 'i')
                    ),
                    required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('avionics_catalog_consolidation_guard',
                          'avionics_catalog_consolidation_guard_validate_insert'),
                        ('avionics_catalog_consolidation_guard',
                          'avionics_catalog_consolidation_guard_immutable'),
                        ('avionics_manufacturer_identity_memberships',
                          'avionics_manufacturer_membership_validate_insert'),
                        ('avionics_manufacturer_identity_memberships',
                          'avionics_manufacturer_membership_immutable'),
                        ('avionics_manufacturer_identity_memberships',
                          'avionics_manufacturer_alias_membership_requires_decision'),
                        ('avionics_manufacturer_identities',
                          'avionics_manufacturer_identity_immutable'),
                        ('avionics_manufacturers',
                          'avionics_manufacturer_identity_name_immutable'),
                        ('avionics_manufacturer_alias_candidates',
                          'avionics_manufacturer_alias_candidate_pending_insert'),
                        ('avionics_manufacturer_alias_candidates',
                          'avionics_manufacturer_alias_candidate_immutable'),
                        ('avionics_manufacturer_identity_merges',
                          'avionics_manufacturer_identity_merge_validate'),
                        ('avionics_manufacturer_canonical_keys',
                          'avionics_manufacturer_canonical_key_delete'),
                        ('avionics_manufacturer_canonical_keys',
                          'avionics_manufacturer_canonical_key_immutable'),
                        ('avionics_manufacturers',
                          'avionics_manufacturer_canonical_key_insert'),
                        ('avionics_manufacturers',
                          'avionics_manufacturer_normalized_name_preserve_key'),
                        ('avionics_manufacturer_identity_merges',
                          'avionics_manufacturer_identity_merge_apply'),
                        ('avionics_manufacturer_identity_merges',
                          'avionics_manufacturer_identity_merge_immutable'),
                        ('avionics_models',
                          'avionics_models_consolidation_identity_immutable'),
                        ('avionics_models',
                          'avionics_models_approved_identity_immutable'),
                        ('avionics_models',
                          'avionics_models_approved_delete_guard'),
                        ('avionics_models',
                          'avionics_models_approved_types_insert'),
                        ('avionics_models',
                          'avionics_models_approved_types_update'),
                        ('avionics_models',
                          'avionics_models_referenced_status_update'),
                        ('avionics_model_types',
                          'avionics_model_types_preserve_approved_delete'),
                        ('avionics_model_types',
                          'avionics_model_types_preserve_approved_update'),
                        ('avionics_suite_components',
                          'avionics_suite_components_approved_insert'),
                        ('avionics_suite_components',
                          'avionics_suite_components_approved_update'),
                        ('avionics_models',
                          'avionics_models_canonical_identity_validate_update'),
                        ('avionics_models',
                          'avionics_models_canonical_identity_sync_update'),
                        ('avionics_approved_product_identities',
                          'avionics_approved_identity_validate'),
                        ('avionics_approved_product_identities',
                          'avionics_approved_identity_preserve_delete'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_approved_update'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_approved_insert'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_mutable_insert'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_mutable_update'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_mutable_delete'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_distinct_replacement_insert'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_distinct_replacement_update'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_semantic_unique_insert'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_semantic_unique_update'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_action_graph_insert'),
                        ('aircraft_sale_listing_avionics',
                          'aircraft_sale_listing_avionics_action_graph_update'),
                        ('aircraft_reference_avionics',
                          'aircraft_reference_avionics_building_insert'),
                        ('aircraft_reference_avionics',
                          'aircraft_reference_avionics_immutable'),
                        ('aircraft_sale_listings',
                          'aircraft_sale_listings_ready_semantic_avionics'),
                        ('aircraft_sale_listings',
                          'aircraft_sale_listings_ready_semantic_avionics_insert'),
                        ('aircraft_sale_listings',
                          'listing_verified_requires_ready_insert'),
                        ('aircraft_sale_listings',
                          'listing_verified_requires_ready_update')
                    )
                    SELECT
                      to_regclass('avionics_models') IS NOT NULL
                      AND (
                        EXISTS (
                          SELECT 1
                          FROM required_relations required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_class actual
                            WHERE actual.oid = to_regclass(required.object_name)
                              AND actual.relkind::text = required.relation_kind
                          )
                        )
                        OR EXISTS (
                          SELECT 1
                          FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_trigger actual
                            WHERE actual.tgrelid = to_regclass(required.parent_name)
                              AND actual.tgname = required.trigger_name
                              AND NOT actual.tgisinternal
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_identity_deduplication_postconditions = missing_identity_objects
            || self
                .migration_contract_invalid_on(
                    connection,
                    "avionics_models",
                    IDENTITY_DEDUPLICATION_POSTCONDITIONS_MIGRATION,
                    IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_VERSION,
                    IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_identity_deduplication_postconditions {
            bail!(identity_deduplication_postconditions_migration_required_message(self.kind()));
        }

        let missing_listing_aircraft_identity_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_objects(object_type, object_name, parent_name) AS (
                      VALUES
                        ('table', 'aircraft_designation_faa_bindings', NULL),
                        ('table', 'aircraft_sale_listing_identity_assignments', NULL),
                        ('table', 'aircraft_sale_listing_current_identity_assignments', NULL),
                        ('trigger', 'aircraft_designation_faa_binding_requires_provenance',
                          'aircraft_designation_faa_bindings'),
                        ('trigger', 'aircraft_designation_faa_binding_immutable_update',
                          'aircraft_designation_faa_bindings'),
                        ('trigger', 'listing_identity_assignment_requires_provenance',
                          'aircraft_sale_listing_identity_assignments'),
                        ('trigger', 'listing_identity_assignment_requires_faa_identity',
                          'aircraft_sale_listing_identity_assignments'),
                        ('trigger', 'listing_identity_assignment_requires_linear_history',
                          'aircraft_sale_listing_identity_assignments'),
                        ('trigger', 'listing_identity_assignment_immutable_update',
                          'aircraft_sale_listing_identity_assignments'),
                        ('trigger', 'listing_current_identity_validate_insert',
                          'aircraft_sale_listing_current_identity_assignments'),
                        ('trigger', 'listing_current_identity_validate_update',
                          'aircraft_sale_listing_current_identity_assignments'),
                        ('trigger', 'listing_ready_requires_canonical_aircraft_insert',
                          'aircraft_sale_listings'),
                        ('trigger', 'listing_ready_requires_canonical_aircraft_update',
                          'aircraft_sale_listings')
                    )
                    SELECT
                      EXISTS (SELECT 1 FROM sqlite_schema WHERE name = 'aircraft_sale_listings')
                      AND (
                        EXISTS (
                          SELECT 1
                          FROM required_objects required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM sqlite_schema actual
                            WHERE actual.type = required.object_type
                              AND actual.name = required.object_name
                              AND (
                                required.parent_name IS NULL
                                OR actual.tbl_name = required.parent_name
                              )
                          )
                        )
                        OR NOT EXISTS (
                          SELECT foreign_key.id
                          FROM pragma_foreign_key_list(
                            'aircraft_sale_listing_identity_assignments'
                          ) foreign_key
                          WHERE foreign_key."table" =
                            'aircraft_sale_listing_identity_assignments'
                          GROUP BY foreign_key.id
                          HAVING count(*) = 2
                            AND sum(
                              foreign_key."from" = 'supersedes_assignment_id'
                              AND foreign_key."to" = 'id'
                            ) = 1
                            AND sum(
                              foreign_key."from" = 'aircraft_sale_listing_id'
                              AND foreign_key."to" = 'aircraft_sale_listing_id'
                            ) = 1
                            AND min(upper(foreign_key.on_delete)) = 'CASCADE'
                            AND max(upper(foreign_key.on_delete)) = 'CASCADE'
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_relations(object_name, relation_kind) AS (
                      VALUES
                        ('aircraft_designation_faa_bindings', 'r'),
                        ('aircraft_sale_listing_identity_assignments', 'r'),
                        ('aircraft_sale_listing_current_identity_assignments', 'r')
                    ),
                    required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('aircraft_designation_faa_bindings',
                          'aircraft_designation_faa_binding_validate'),
                        ('aircraft_designation_faa_bindings',
                          'aircraft_designation_faa_binding_immutable'),
                        ('aircraft_sale_listing_identity_assignments',
                          'listing_identity_assignment_validate'),
                        ('aircraft_sale_listing_identity_assignments',
                          'listing_identity_assignment_immutable'),
                        ('aircraft_sale_listing_current_identity_assignments',
                          'listing_current_identity_validate_insert'),
                        ('aircraft_sale_listing_current_identity_assignments',
                          'listing_current_identity_validate_update'),
                        ('aircraft_sale_listings',
                          'listing_ready_requires_canonical_aircraft_insert'),
                        ('aircraft_sale_listings',
                          'listing_ready_requires_canonical_aircraft_update')
                    )
                    SELECT
                      to_regclass('aircraft_sale_listings') IS NOT NULL
                      AND (
                        EXISTS (
                          SELECT 1
                          FROM required_relations required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_class actual
                            WHERE actual.oid = to_regclass(required.object_name)
                              AND actual.relkind::text = required.relation_kind
                          )
                        )
                        OR EXISTS (
                          SELECT 1
                          FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_trigger actual
                            WHERE actual.tgrelid = to_regclass(required.parent_name)
                              AND actual.tgname = required.trigger_name
                              AND NOT actual.tgisinternal
                          )
                        )
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pg_constraint actual
                          WHERE actual.conrelid =
                            to_regclass('aircraft_sale_listing_identity_assignments')
                            AND actual.confrelid =
                              to_regclass('aircraft_sale_listing_identity_assignments')
                            AND actual.contype = 'f'
                            AND actual.confdeltype = 'c'
                            AND actual.conname =
                              'aircraft_listing_identity_assignment_supersedes_fk'
                            AND pg_get_constraintdef(actual.oid, true) =
                              'FOREIGN KEY (supersedes_assignment_id, aircraft_sale_listing_id) REFERENCES aircraft_sale_listing_identity_assignments(id, aircraft_sale_listing_id) ON DELETE CASCADE'
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await
                .context("could not inspect PostgreSQL listing aircraft identity objects")?
            }
        };
        let missing_listing_aircraft_identity = missing_listing_aircraft_identity_objects
            || self
                .migration_contract_invalid_on(
                    connection,
                    "aircraft_sale_listings",
                    LISTING_AIRCRAFT_IDENTITY_MIGRATION,
                    LISTING_AIRCRAFT_IDENTITY_CONTRACT_VERSION,
                    LISTING_AIRCRAFT_IDENTITY_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_listing_aircraft_identity {
            bail!(listing_aircraft_identity_migration_required_message(
                self.kind()
            ));
        }

        let missing_listing_aircraft_compatibility_projection_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_objects(object_type, object_name, parent_name) AS (
                      VALUES
                        ('table',
                          'aircraft_sale_listing_pending_compatibility_placeholder', NULL),
                        ('table', 'aircraft_listing_identity_input_observations', NULL),
                        ('table', 'aircraft_valuation_compatibility_projections', NULL),
                        ('table', 'aircraft_valuation_projection_transitions', NULL),
                        ('view',
                          'aircraft_sale_listing_exact_compatibility_projections', NULL),
                        ('trigger', 'aircraft_listing_identity_input_append_only_update',
                          'aircraft_listing_identity_input_observations'),
                        ('trigger', 'aircraft_listing_identity_input_append_only_delete',
                          'aircraft_listing_identity_input_observations'),
                        ('trigger', 'listing_insert_requires_aircraft_projection_or_placeholder',
                          'aircraft_sale_listings'),
                        ('trigger', 'aircraft_valuation_transition_validate_insert',
                          'aircraft_valuation_projection_transitions'),
                        ('trigger', 'aircraft_valuation_transition_immutable_update',
                          'aircraft_valuation_projection_transitions'),
                        ('trigger', 'aircraft_valuation_transition_execute',
                          'aircraft_valuation_projection_transitions'),
                        ('trigger', 'aircraft_valuation_transition_validate_delete',
                          'aircraft_valuation_projection_transitions'),
                        ('trigger', 'aircraft_valuation_projection_validate_insert',
                          'aircraft_valuation_compatibility_projections'),
                        ('trigger', 'aircraft_valuation_projection_immutable_update',
                          'aircraft_valuation_compatibility_projections'),
                        ('trigger', 'aircraft_valuation_projection_immutable_delete',
                          'aircraft_valuation_compatibility_projections'),
                        ('trigger', 'listing_aircraft_projection_transition_update',
                          'aircraft_sale_listings'),
                        ('trigger', 'listing_current_identity_projection_insert',
                          'aircraft_sale_listing_current_identity_assignments'),
                        ('trigger', 'listing_current_identity_projection_update',
                          'aircraft_sale_listing_current_identity_assignments'),
                        ('trigger', 'listing_ready_requires_aircraft_projection',
                          'aircraft_sale_listings'),
                        ('trigger', 'listing_ready_insert_requires_aircraft_projection',
                          'aircraft_sale_listings'),
                        ('trigger', 'listing_ready_rejects_pending_aircraft_placeholder',
                          'aircraft_sale_listings')
                    )
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table' AND name = 'aircraft_sale_listings'
                      )
                      AND EXISTS (
                        SELECT 1
                        FROM required_objects required
                        WHERE NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema actual
                          WHERE actual.type = required.object_type
                            AND actual.name = required.object_name
                            AND (
                              required.parent_name IS NULL
                              OR actual.tbl_name = required.parent_name
                            )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_relations(object_name, relation_kind) AS (
                      VALUES
                        ('aircraft_sale_listing_pending_compatibility_placeholder', 'r'),
                        ('aircraft_listing_identity_input_observations', 'r'),
                        ('aircraft_valuation_compatibility_projections', 'r'),
                        ('aircraft_valuation_projection_transitions', 'r'),
                        ('aircraft_sale_listing_exact_compatibility_projections', 'v')
                    ),
                    required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('aircraft_listing_identity_input_observations',
                          'aircraft_listing_identity_input_append_only_update'),
                        ('aircraft_listing_identity_input_observations',
                          'aircraft_listing_identity_input_append_only_delete'),
                        ('aircraft_sale_listings',
                          'listing_insert_requires_aircraft_projection_or_placeholder'),
                        ('aircraft_valuation_projection_transitions',
                          'aircraft_valuation_transition_validate_insert'),
                        ('aircraft_valuation_projection_transitions',
                          'aircraft_valuation_transition_immutable_update'),
                        ('aircraft_valuation_projection_transitions',
                          'aircraft_valuation_transition_execute'),
                        ('aircraft_valuation_projection_transitions',
                          'aircraft_valuation_transition_validate_delete'),
                        ('aircraft_valuation_compatibility_projections',
                          'aircraft_valuation_projection_validate_insert'),
                        ('aircraft_valuation_compatibility_projections',
                          'aircraft_valuation_projection_immutable_update'),
                        ('aircraft_valuation_compatibility_projections',
                          'aircraft_valuation_projection_immutable_delete'),
                        ('aircraft_sale_listings',
                          'listing_aircraft_projection_transition_update'),
                        ('aircraft_sale_listing_current_identity_assignments',
                          'listing_current_identity_projection_insert'),
                        ('aircraft_sale_listing_current_identity_assignments',
                          'listing_current_identity_projection_update'),
                        ('aircraft_sale_listings',
                          'listing_ready_requires_aircraft_projection'),
                        ('aircraft_sale_listings',
                          'listing_ready_insert_requires_aircraft_projection'),
                        ('aircraft_sale_listings',
                          'listing_ready_rejects_pending_aircraft_placeholder')
                    )
                    SELECT
                      to_regclass('aircraft_sale_listings') IS NOT NULL
                      AND (
                        EXISTS (
                          SELECT 1
                          FROM required_relations required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_class actual
                            WHERE actual.oid = to_regclass(required.object_name)
                              AND actual.relkind::text = required.relation_kind
                          )
                        )
                        OR EXISTS (
                          SELECT 1
                          FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_trigger actual
                            WHERE actual.tgrelid = to_regclass(required.parent_name)
                              AND actual.tgname = required.trigger_name
                              AND NOT actual.tgisinternal
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_listing_aircraft_compatibility_projection =
            missing_listing_aircraft_compatibility_projection_objects
                || self
                    .migration_contract_invalid_on(
                        connection,
                        "aircraft_sale_listings",
                        LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_MIGRATION,
                        LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_CONTRACT_VERSION,
                        LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_CONTRACT_FINGERPRINT,
                    )
                    .await?;
        if missing_listing_aircraft_compatibility_projection {
            bail!(
                listing_aircraft_compatibility_projection_migration_required_message(self.kind())
            );
        }

        let missing_no_supported_selection_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_triggers(trigger_name, parent_name) AS (
                      VALUES
                        ('aircraft_identity_no_supported_selection_claim_insert',
                          'aircraft_identity_decision_claims'),
                        ('aircraft_identity_no_supported_selection_claim_update',
                          'aircraft_identity_decision_claims'),
                        ('aircraft_identity_no_supported_selection_decision_update',
                          'aircraft_identity_decisions')
                    )
                    SELECT
                      EXISTS (
                        SELECT 1
                        FROM sqlite_schema
                        WHERE type = 'table'
                          AND name = 'aircraft_identity_decisions'
                      )
                      AND (
                        NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema
                          WHERE type = 'table'
                            AND name = 'aircraft_identity_decisions'
                            AND lower(sql) LIKE '%no_supported_selection%'
                            AND lower(sql) NOT LIKE '%not_an_entity%'
                        )
                        OR EXISTS (
                          SELECT 1
                          FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM sqlite_schema actual
                            WHERE actual.type = 'trigger'
                              AND actual.name = required.trigger_name
                              AND actual.tbl_name = required.parent_name
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('aircraft_identity_decision_claims',
                          'aircraft_identity_no_supported_selection_claim_insert'),
                        ('aircraft_identity_decision_claims',
                          'aircraft_identity_no_supported_selection_claim_update'),
                        ('aircraft_identity_decisions',
                          'aircraft_identity_no_supported_selection_decision_update')
                    )
                    SELECT
                      to_regclass('aircraft_identity_decisions') IS NOT NULL
                      AND (
                        NOT EXISTS (
                          SELECT 1
                          FROM pg_constraint actual
                          WHERE actual.conrelid =
                              to_regclass('aircraft_identity_decisions')
                            AND actual.contype = 'c'
                            AND lower(pg_get_constraintdef(actual.oid))
                                LIKE '%no_supported_selection%'
                        )
                        OR EXISTS (
                          SELECT 1
                          FROM pg_constraint actual
                          WHERE actual.conrelid =
                              to_regclass('aircraft_identity_decisions')
                            AND actual.contype = 'c'
                            AND lower(pg_get_constraintdef(actual.oid))
                                LIKE '%not_an_entity%'
                        )
                        OR EXISTS (
                          SELECT 1
                          FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_trigger actual
                            WHERE actual.tgrelid =
                                to_regclass(required.parent_name)
                              AND actual.tgname = required.trigger_name
                              AND NOT actual.tgisinternal
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_no_supported_selection = missing_no_supported_selection_objects
            || self
                .migration_contract_invalid_on(
                    connection,
                    "aircraft_identity_decisions",
                    AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_MIGRATION,
                    AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_CONTRACT_VERSION,
                    AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_no_supported_selection {
            bail!(aircraft_identity_no_supported_selection_migration_required_message(self.kind()));
        }

        let missing_aircraft_catalog_retrieval_key_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('aircraft_makes',
                          'aircraft_make_retrieval_key_validate_insert'),
                        ('aircraft_makes',
                          'aircraft_make_retrieval_key_validate_update'),
                        ('aircraft_model_families',
                          'aircraft_family_retrieval_key_validate_insert'),
                        ('aircraft_model_families',
                          'aircraft_family_retrieval_key_validate_update'),
                        ('aircraft_generations',
                          'aircraft_generation_retrieval_key_validate_insert'),
                        ('aircraft_generations',
                          'aircraft_generation_retrieval_key_validate_update'),
                        ('aircraft_factory_packages',
                          'aircraft_package_retrieval_key_validate_insert'),
                        ('aircraft_factory_packages',
                          'aircraft_package_retrieval_key_validate_update')
                    )
                    SELECT
                      EXISTS (
                        SELECT 1
                        FROM sqlite_schema
                        WHERE type = 'table' AND name = 'aircraft_makes'
                      )
                      AND EXISTS (
                        SELECT 1
                        FROM required_triggers required
                        WHERE NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema actual
                          WHERE actual.type = 'trigger'
                            AND actual.name = required.trigger_name
                            AND actual.tbl_name = required.parent_name
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('aircraft_makes',
                          'aircraft_make_retrieval_key_validate'),
                        ('aircraft_model_families',
                          'aircraft_family_retrieval_key_validate'),
                        ('aircraft_generations',
                          'aircraft_generation_retrieval_key_validate'),
                        ('aircraft_factory_packages',
                          'aircraft_package_retrieval_key_validate')
                    )
                    SELECT
                      to_regclass('aircraft_makes') IS NOT NULL
                      AND (
                        to_regprocedure('aircraft_retrieval_key(text)') IS NULL
                        OR to_regprocedure(
                          'require_aircraft_catalog_retrieval_key()'
                        ) IS NULL
                        OR EXISTS (
                          SELECT 1
                          FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_trigger actual
                            WHERE actual.tgrelid =
                                to_regclass(required.parent_name)
                              AND actual.tgname = required.trigger_name
                              AND NOT actual.tgisinternal
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_aircraft_catalog_retrieval_keys = missing_aircraft_catalog_retrieval_key_objects
            || self
                .migration_contract_invalid_on(
                    connection,
                    "aircraft_makes",
                    AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION,
                    AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION,
                    AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_aircraft_catalog_retrieval_keys {
            bail!(aircraft_catalog_retrieval_keys_migration_required_message(
                self.kind()
            ));
        }

        let missing_aircraft_tcds_make_lineage_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('aircraft_tcds_make_lineage_bindings',
                          'aircraft_tcds_make_lineage_requires_provenance'),
                        ('aircraft_tcds_make_lineage_bindings',
                          'aircraft_tcds_make_lineage_no_overlap'),
                        ('aircraft_tcds_make_lineage_bindings',
                          'aircraft_tcds_make_lineage_no_catalog_collision'),
                        ('aircraft_tcds_make_lineage_bindings',
                          'aircraft_tcds_make_lineage_immutable_update'),
                        ('aircraft_tcds_make_lineage_bindings',
                          'aircraft_tcds_make_lineage_immutable_delete'),
                        ('aircraft_makes',
                          'aircraft_make_tcds_lineage_collision_insert'),
                        ('aircraft_makes',
                          'aircraft_make_tcds_lineage_collision_update'),
                        ('aircraft_make_aliases',
                          'aircraft_make_alias_tcds_lineage_collision'),
                        ('aircraft_designation_faa_bindings',
                          'aircraft_designation_faa_binding_requires_provenance'),
                        ('aircraft_sale_listing_identity_assignments',
                          'listing_identity_assignment_requires_faa_identity'),
                        ('aircraft_sale_listings',
                          'listing_ready_requires_canonical_aircraft_update')
                    )
                    SELECT
                      EXISTS (
                        SELECT 1
                        FROM sqlite_schema
                        WHERE type = 'table'
                          AND name = 'aircraft_makes'
                      )
                      AND (
                      NOT EXISTS (
                        SELECT 1
                        FROM sqlite_schema
                        WHERE type = 'table'
                          AND name = 'aircraft_tcds_make_lineage_bindings'
                      )
                      OR NOT EXISTS (
                        SELECT 1
                        FROM sqlite_schema
                        WHERE type = 'index'
                          AND name =
                            'idx_faa_registry_aircraft_lineage_record'
                      )
                      OR EXISTS (
                        SELECT 1
                        FROM required_triggers required
                        WHERE NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema actual
                          WHERE actual.type = 'trigger'
                            AND actual.name = required.trigger_name
                            AND actual.tbl_name = required.parent_name
                        )
                      ))
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('aircraft_tcds_make_lineage_bindings',
                          'aircraft_tcds_make_lineage_validate'),
                        ('aircraft_tcds_make_lineage_bindings',
                          'aircraft_tcds_make_lineage_immutable'),
                        ('aircraft_makes',
                          'aircraft_make_tcds_lineage_collision'),
                        ('aircraft_make_aliases',
                          'aircraft_make_alias_tcds_lineage_collision'),
                        ('aircraft_designation_faa_bindings',
                          'aircraft_designation_faa_binding_validate'),
                        ('aircraft_sale_listing_identity_assignments',
                          'listing_identity_assignment_validate'),
                        ('aircraft_sale_listings',
                          'listing_ready_requires_canonical_aircraft_insert'),
                        ('aircraft_sale_listings',
                          'listing_ready_requires_canonical_aircraft_update')
                    )
                    SELECT
                      to_regclass('aircraft_makes') IS NOT NULL
                      AND (
                      to_regclass(
                        'aircraft_tcds_make_lineage_bindings'
                      ) IS NULL
                      OR to_regclass(
                        'idx_faa_registry_aircraft_lineage_record'
                      ) IS NULL
                      OR to_regprocedure(
                        'validate_aircraft_tcds_make_lineage()'
                      ) IS NULL
                      OR to_regprocedure(
                        'aircraft_tcds_make_lineage_matches(text,text,text,bigint,bigint,text,text,text)'
                      ) IS NULL
                      OR EXISTS (
                        SELECT 1
                        FROM required_triggers required
                        WHERE NOT EXISTS (
                          SELECT 1
                          FROM pg_trigger actual
                          WHERE actual.tgrelid =
                                to_regclass(required.parent_name)
                            AND actual.tgname = required.trigger_name
                            AND NOT actual.tgisinternal
                        )
                      ))
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_aircraft_tcds_make_lineage = missing_aircraft_tcds_make_lineage_objects
            || self
                .migration_contract_invalid_on(
                    connection,
                    "aircraft_makes",
                    AIRCRAFT_TCDS_MAKE_LINEAGE_MIGRATION,
                    AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_VERSION,
                    AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_aircraft_tcds_make_lineage {
            bail!(aircraft_tcds_make_lineage_migration_required_message(
                self.kind()
            ));
        }

        let missing_human_consolidation_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_objects(object_type, object_name) AS (
                      VALUES
                        ('table', 'avionics_catalog_human_consolidation_authorizations'),
                        ('table', 'avionics_catalog_human_consolidation_members'),
                        ('table', 'avionics_catalog_human_consolidation_guard'),
                        ('table', 'avionics_catalog_human_consolidation_claim'),
                        ('view', 'avionics_catalog_valid_human_consolidation_pairs'),
                        ('trigger', 'avionics_catalog_human_consolidation_authorizations_immutable'),
                        ('trigger', 'avionics_catalog_human_consolidation_members_immutable'),
                        ('trigger', 'avionics_catalog_human_consolidation_guard_validate_insert'),
                        ('trigger', 'avionics_catalog_human_consolidation_claim_validate_insert')
                    )
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table' AND name = 'avionics_models'
                      )
                      AND EXISTS (
                        SELECT 1
                        FROM required_objects required
                        WHERE NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema actual
                          WHERE actual.type = required.object_type
                            AND actual.name = required.object_name
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_relations(object_name) AS (
                      VALUES
                        ('avionics_catalog_human_consolidation_authorizations'),
                        ('avionics_catalog_human_consolidation_members'),
                        ('avionics_catalog_human_consolidation_guard'),
                        ('avionics_catalog_human_consolidation_claim'),
                        ('avionics_catalog_valid_human_consolidation_pairs')
                    ),
                    required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('avionics_catalog_human_consolidation_authorizations',
                          'avionics_catalog_human_consolidation_authorizations_immutable'),
                        ('avionics_catalog_human_consolidation_members',
                          'avionics_catalog_human_consolidation_members_immutable'),
                        ('avionics_catalog_human_consolidation_guard',
                          'avionics_catalog_human_consolidation_guard_validate_insert'),
                        ('avionics_catalog_human_consolidation_claim',
                          'avionics_catalog_human_consolidation_claim_validate_insert')
                    )
                    SELECT
                      to_regclass('avionics_models') IS NOT NULL
                      AND (
                        EXISTS (
                          SELECT 1 FROM required_relations required
                          WHERE to_regclass(required.object_name) IS NULL
                        )
                        OR EXISTS (
                          SELECT 1 FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_trigger actual
                            WHERE actual.tgrelid = to_regclass(required.parent_name)
                              AND actual.tgname = required.trigger_name
                              AND NOT actual.tgisinternal
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_human_consolidation = missing_human_consolidation_objects
            || self
                .migration_contract_invalid_on(
                    connection,
                    "avionics_models",
                    AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_MIGRATION,
                    AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_VERSION,
                    AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_human_consolidation {
            bail!(avionics_human_reviewed_consolidation_migration_required_message(self.kind()));
        }
        let missing_descriptive_consolidation = self
            .migration_contract_invalid_on(
                connection,
                "avionics_catalog_valid_human_consolidation_pairs",
                AVIONICS_DESCRIPTIVE_CONSOLIDATION_MIGRATION,
                AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_VERSION,
                AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_FINGERPRINT,
            )
            .await?;
        if missing_descriptive_consolidation {
            bail!(avionics_descriptive_consolidation_migration_required_message(self.kind()));
        }
        let missing_grounded_exact_model_consolidation_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_objects(object_type, object_name) AS (
                      VALUES
                        ('table', 'avionics_catalog_grounded_consolidation_authorizations'),
                        ('table', 'avionics_catalog_grounded_consolidation_guard'),
                        ('table', 'avionics_catalog_grounded_consolidation_claim'),
                        ('view', 'avionics_catalog_valid_grounded_consolidation_pairs'),
                        ('trigger', 'avionics_catalog_grounded_consolidation_authorization_validate_insert'),
                        ('trigger', 'avionics_catalog_grounded_consolidation_authorization_immutable'),
                        ('trigger', 'avionics_catalog_grounded_consolidation_guard_validate_insert'),
                        ('trigger', 'avionics_catalog_grounded_consolidation_guard_immutable'),
                        ('trigger', 'avionics_catalog_grounded_consolidation_claim_validate_insert'),
                        ('trigger', 'avionics_catalog_grounded_consolidation_claim_immutable')
                    )
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table' AND name = 'avionics_models'
                      )
                      AND EXISTS (
                        SELECT 1 FROM required_objects required
                        WHERE NOT EXISTS (
                          SELECT 1 FROM sqlite_schema actual
                          WHERE actual.type = required.object_type
                            AND actual.name = required.object_name
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_relations(object_name) AS (
                      VALUES
                        ('avionics_catalog_grounded_consolidation_authorizations'),
                        ('avionics_catalog_grounded_consolidation_guard'),
                        ('avionics_catalog_grounded_consolidation_claim'),
                        ('avionics_catalog_valid_grounded_consolidation_pairs')
                    ),
                    required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('avionics_catalog_grounded_consolidation_authorizations',
                          'avionics_catalog_grounded_consolidation_authorization_validate_'),
                        ('avionics_catalog_grounded_consolidation_authorizations',
                          'avionics_catalog_grounded_consolidation_authorization_immutable'),
                        ('avionics_catalog_grounded_consolidation_guard',
                          'avionics_catalog_grounded_consolidation_guard_validate_insert'),
                        ('avionics_catalog_grounded_consolidation_guard',
                          'avionics_catalog_grounded_consolidation_guard_immutable'),
                        ('avionics_catalog_grounded_consolidation_claim',
                          'avionics_catalog_grounded_consolidation_claim_validate_insert'),
                        ('avionics_catalog_grounded_consolidation_claim',
                          'avionics_catalog_grounded_consolidation_claim_immutable')
                    )
                    SELECT
                      to_regclass('avionics_models') IS NOT NULL
                      AND (
                        EXISTS (
                          SELECT 1 FROM required_relations required
                          WHERE to_regclass(required.object_name) IS NULL
                        )
                        OR EXISTS (
                          SELECT 1 FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM pg_trigger actual
                            WHERE actual.tgrelid = to_regclass(required.parent_name)
                              AND actual.tgname = required.trigger_name
                              AND NOT actual.tgisinternal
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_grounded_exact_model_consolidation =
            missing_grounded_exact_model_consolidation_objects
                || self
                    .migration_contract_invalid_on(
                        connection,
                        "avionics_models",
                        AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_MIGRATION,
                        AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_VERSION,
                        AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_FINGERPRINT,
                    )
                    .await?;
        if missing_grounded_exact_model_consolidation {
            bail!(
                avionics_grounded_exact_model_consolidation_migration_required_message(self.kind())
            );
        }

        let missing_avionics_source_origin_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_objects(object_type, object_name) AS (
                      VALUES
                        ('table', 'avionics_authoritative_source_origins'),
                        ('table', 'avionics_authoritative_source_origin_revocations'),
                        ('view', 'avionics_active_authoritative_source_origins'),
                        ('trigger', 'avionics_authoritative_source_origins_immutable_update'),
                        ('trigger', 'avionics_authoritative_source_origins_immutable_delete'),
                        ('trigger', 'avionics_authoritative_source_origin_revocations_immutable_update'),
                        ('trigger', 'avionics_authoritative_source_origin_revocations_immutable_delete'),
                        ('trigger', 'avionics_garmin_authoritative_source_origins_bootstrap')
                    )
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table' AND name = 'avionics_manufacturers'
                      )
                      AND EXISTS (
                        SELECT 1
                        FROM required_objects required
                        WHERE NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema actual
                          WHERE actual.type = required.object_type
                            AND actual.name = required.object_name
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_relations(object_name) AS (
                      VALUES
                        ('avionics_authoritative_source_origins'),
                        ('avionics_authoritative_source_origin_revocations'),
                        ('avionics_active_authoritative_source_origins')
                    ),
                    required_triggers(parent_name, trigger_name) AS (
                      VALUES
                        ('avionics_authoritative_source_origins',
                          'avionics_authoritative_source_origins_immutable'),
                        ('avionics_authoritative_source_origin_revocations',
                          'avionics_authoritative_source_origin_revocations_immutable'),
                        ('avionics_manufacturer_identities',
                          'avionics_garmin_authoritative_source_origins_bootstrap')
                    )
                    SELECT
                      to_regclass('avionics_manufacturers') IS NOT NULL
                      AND (
                        EXISTS (
                          SELECT 1 FROM required_relations required
                          WHERE to_regclass(required.object_name) IS NULL
                        )
                        OR EXISTS (
                          SELECT 1 FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_trigger actual
                            WHERE actual.tgrelid = to_regclass(required.parent_name)
                              AND actual.tgname = required.trigger_name
                              AND NOT actual.tgisinternal
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_avionics_source_origins = missing_avionics_source_origin_objects
            || self
                .migration_contract_invalid_on(
                    connection,
                    "avionics_manufacturers",
                    AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_MIGRATION,
                    AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_VERSION,
                    AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_avionics_source_origins {
            bail!(avionics_authoritative_source_origins_migration_required_message(self.kind()));
        }

        let missing_avionics_reuse_attestation_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_columns(
                      column_name, column_type, required_not_null, primary_key
                    ) AS (
                      VALUES
                        ('avionics_model_id', 'INTEGER', -1, 1),
                        ('avionics_authoritative_source_origin_id', 'INTEGER', 1, 0),
                        ('policy_version', 'TEXT', 1, 0),
                        ('product_fingerprint', 'TEXT', 1, 0),
                        ('attested_at', 'TEXT', 1, 0)
                    ),
                    required_foreign_keys(
                      parent_table, child_column, parent_column, delete_action
                    ) AS (
                      VALUES
                        ('avionics_models', 'avionics_model_id', 'id', 'CASCADE'),
                        ('avionics_authoritative_source_origins',
                          'avionics_authoritative_source_origin_id', 'id', 'RESTRICT')
                    ),
                    required_triggers(
                      trigger_name, parent_name, event_fragment, body_fragment
                    ) AS (
                      VALUES
                        ('avionics_product_reuse_attestations_validate_insert',
                          'avionics_product_reuse_attestations',
                          'before insert on avionics_product_reuse_attestations',
                          'avionics_active_authoritative_source_origins'),
                        ('avionics_product_reuse_attestations_immutable_update',
                          'avionics_product_reuse_attestations',
                          'before update on avionics_product_reuse_attestations',
                          'reuse attestations are replaced, never updated'),
                        ('avionics_product_reuse_invalidate_type_insert',
                          'avionics_model_types',
                          'after insert on avionics_model_types',
                          'new.avionics_model_id'),
                        ('avionics_product_reuse_invalidate_type_delete',
                          'avionics_model_types',
                          'after delete on avionics_model_types',
                          'old.avionics_model_id'),
                        ('avionics_product_reuse_invalidate_type_update',
                          'avionics_model_types',
                          'after update of avionics_model_id, avionics_type_id on avionics_model_types',
                          'where avionics_model_id in'),
                        ('avionics_product_reuse_invalidate_capability_update',
                          'avionics_types',
                          'after update of name, normalized_name on avionics_types',
                          'membership.avionics_type_id = new.id'),
                        ('avionics_product_reuse_invalidate_identity_update',
                          'avionics_approved_product_identities',
                          'after update on avionics_approved_product_identities',
                          'new.avionics_model_id'),
                        ('avionics_product_reuse_invalidate_origin_revocation',
                          'avionics_authoritative_source_origin_revocations',
                          'after insert on avionics_authoritative_source_origin_revocations',
                          'new.avionics_authoritative_source_origin_id')
                    )
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table' AND name = 'avionics_models'
                      )
                      AND (
                        NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema
                          WHERE type = 'table'
                            AND name = 'avionics_product_reuse_attestations'
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM pragma_table_info(
                            'avionics_product_reuse_attestations'
                          )
                        ) <> 5
                        OR EXISTS (
                          SELECT 1
                          FROM required_columns required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pragma_table_info(
                              'avionics_product_reuse_attestations'
                            ) actual
                            WHERE actual.name = required.column_name
                              AND upper(actual.type) = required.column_type
                              AND (
                                required.required_not_null < 0
                                OR actual."notnull" =
                                   required.required_not_null
                              )
                              AND actual.pk = required.primary_key
                          )
                        )
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pragma_table_info(
                            'avionics_product_reuse_attestations'
                          )
                          WHERE name = 'attested_at'
                            AND upper(dflt_value) = 'CURRENT_TIMESTAMP'
                        )
                        OR NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema actual
                          WHERE actual.type = 'table'
                            AND actual.name =
                              'avionics_product_reuse_attestations'
                            AND instr(
                              lower(actual.sql),
                              'check (policy_version = ''avionics_reuse_v2'')'
                            ) > 0
                            AND instr(
                              lower(actual.sql),
                              'length(product_fingerprint) = 64'
                            ) > 0
                            AND instr(
                              lower(actual.sql),
                              'product_fingerprint = lower(product_fingerprint)'
                            ) > 0
                            AND instr(
                              lower(actual.sql),
                              'product_fingerprint not glob ''*[^0-9a-f]*'''
                            ) > 0
                        )
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pragma_index_list(
                            'avionics_product_reuse_attestations'
                          )
                          WHERE name = 'idx_avionics_product_reuse_origin'
                            AND "unique" = 0
                            AND origin = 'c'
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM pragma_foreign_key_list(
                            'avionics_product_reuse_attestations'
                          )
                        ) <> 2
                        OR EXISTS (
                          SELECT 1
                          FROM required_foreign_keys required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pragma_foreign_key_list(
                              'avionics_product_reuse_attestations'
                            ) actual
                            WHERE actual."table" = required.parent_table
                              AND actual."from" = required.child_column
                              AND actual."to" = required.parent_column
                              AND upper(actual.on_delete) =
                                  required.delete_action
                          )
                        )
                        OR NOT EXISTS (
                          SELECT 1
                          FROM sqlite_schema actual
                          WHERE actual.type = 'index'
                            AND actual.name =
                              'idx_avionics_product_reuse_origin'
                            AND actual.tbl_name =
                              'avionics_product_reuse_attestations'
                            AND instr(
                              lower(actual.sql),
                              'avionics_authoritative_source_origin_id'
                            ) > 0
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM pragma_index_info(
                            'idx_avionics_product_reuse_origin'
                          )
                        ) <> 1
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pragma_index_info(
                            'idx_avionics_product_reuse_origin'
                          )
                          WHERE name =
                            'avionics_authoritative_source_origin_id'
                        )
                        OR EXISTS (
                          SELECT 1
                          FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM sqlite_schema actual
                            WHERE actual.type = 'trigger'
                              AND actual.name = required.trigger_name
                              AND actual.tbl_name = required.parent_name
                              AND instr(
                                lower(actual.sql),
                                required.event_fragment
                              ) > 0
                              AND instr(
                                lower(actual.sql),
                                required.body_fragment
                              ) > 0
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_columns(
                      column_name, column_type, required_not_null
                    ) AS (
                      VALUES
                        ('avionics_model_id', 'bigint', TRUE),
                        ('avionics_authoritative_source_origin_id',
                          'bigint', TRUE),
                        ('policy_version', 'text', TRUE),
                        ('product_fingerprint', 'text', TRUE),
                        ('attested_at', 'text', TRUE)
                    ),
                    required_foreign_keys(definition_fragment) AS (
                      VALUES
                        ('FOREIGN KEY (avionics_model_id) REFERENCES avionics_models(id) ON DELETE CASCADE'),
                        ('FOREIGN KEY (avionics_authoritative_source_origin_id) REFERENCES avionics_authoritative_source_origins(id) ON DELETE RESTRICT')
                    ),
                    required_triggers(
                      parent_name, trigger_name, function_signature,
                      trigger_type, definition_fragment
                    ) AS (
                      VALUES
                        ('avionics_product_reuse_attestations',
                          'avionics_product_reuse_attestations_validate_insert',
                          'validate_avionics_product_reuse_attestation()', 7,
                          'active exact manufacturer origin'),
                        ('avionics_product_reuse_attestations',
                          'avionics_product_reuse_attestations_immutable_update',
                          'preserve_avionics_product_reuse_attestation()', 19,
                          'replaced, never updated'),
                        ('avionics_model_types',
                          'avionics_product_reuse_invalidate_type_insert',
                          'invalidate_avionics_product_reuse_for_type()', 5,
                          'DELETE FROM avionics_product_reuse_attestations'),
                        ('avionics_model_types',
                          'avionics_product_reuse_invalidate_type_delete',
                          'invalidate_avionics_product_reuse_for_type()', 9,
                          'DELETE FROM avionics_product_reuse_attestations'),
                        ('avionics_model_types',
                          'avionics_product_reuse_invalidate_type_update',
                          'invalidate_avionics_product_reuse_for_type()', 17,
                          'DELETE FROM avionics_product_reuse_attestations'),
                        ('avionics_types',
                          'avionics_product_reuse_invalidate_capability_update',
                          'invalidate_avionics_product_reuse_for_capability()', 17,
                          'DELETE FROM avionics_product_reuse_attestations'),
                        ('avionics_approved_product_identities',
                          'avionics_product_reuse_invalidate_identity_update',
                          'invalidate_avionics_product_reuse_for_identity()', 17,
                          'DELETE FROM avionics_product_reuse_attestations'),
                        ('avionics_authoritative_source_origin_revocations',
                          'avionics_product_reuse_invalidate_origin_revocation',
                          'invalidate_avionics_product_reuse_for_revocation()', 5,
                          'DELETE FROM avionics_product_reuse_attestations')
                    )
                    SELECT
                      to_regclass('avionics_models') IS NOT NULL
                      AND (
                        to_regclass(
                          'avionics_product_reuse_attestations'
                        ) IS NULL
                        OR (
                          SELECT COUNT(*)
                          FROM pg_attribute actual
                          WHERE actual.attrelid = to_regclass(
                            'avionics_product_reuse_attestations'
                          )
                            AND actual.attnum > 0
                            AND NOT actual.attisdropped
                        ) <> 5
                        OR EXISTS (
                          SELECT 1
                          FROM required_columns required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_attribute actual
                            WHERE actual.attrelid = to_regclass(
                              'avionics_product_reuse_attestations'
                            )
                              AND actual.attname = required.column_name
                              AND format_type(
                                actual.atttypid, actual.atttypmod
                              ) = required.column_type
                              AND actual.attnotnull =
                                  required.required_not_null
                          )
                        )
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pg_constraint actual
                          WHERE actual.conrelid = to_regclass(
                            'avionics_product_reuse_attestations'
                          )
                            AND actual.contype = 'p'
                            AND pg_get_constraintdef(actual.oid) =
                              'PRIMARY KEY (avionics_model_id)'
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM pg_constraint actual
                          WHERE actual.conrelid = to_regclass(
                            'avionics_product_reuse_attestations'
                          )
                            AND actual.contype = 'f'
                        ) <> 2
                        OR EXISTS (
                          SELECT 1
                          FROM required_foreign_keys required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_constraint actual
                            WHERE actual.conrelid = to_regclass(
                              'avionics_product_reuse_attestations'
                            )
                              AND actual.contype = 'f'
                              AND pg_get_constraintdef(actual.oid) =
                                  required.definition_fragment
                          )
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM pg_constraint actual
                          WHERE actual.conrelid = to_regclass(
                            'avionics_product_reuse_attestations'
                          )
                            AND actual.contype = 'c'
                        ) <> 2
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pg_constraint actual
                          WHERE actual.conrelid = to_regclass(
                            'avionics_product_reuse_attestations'
                          )
                            AND actual.contype = 'c'
                            AND position(
                              'avionics_reuse_v2'
                              IN pg_get_constraintdef(actual.oid)
                            ) > 0
                        )
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pg_constraint actual
                          WHERE actual.conrelid = to_regclass(
                            'avionics_product_reuse_attestations'
                          )
                            AND actual.contype = 'c'
                            AND position(
                              '^[0-9a-f]{64}$'
                              IN pg_get_constraintdef(actual.oid)
                            ) > 0
                        )
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pg_attribute attribute
                          JOIN pg_attrdef default_value
                            ON default_value.adrelid = attribute.attrelid
                           AND default_value.adnum = attribute.attnum
                          WHERE attribute.attrelid = to_regclass(
                            'avionics_product_reuse_attestations'
                          )
                            AND attribute.attname = 'attested_at'
                            AND position(
                              'CURRENT_TIMESTAMP'
                              IN pg_get_expr(
                                default_value.adbin,
                                default_value.adrelid
                              )
                            ) > 0
                        )
                        OR NOT EXISTS (
                          SELECT 1
                          FROM pg_index actual
                          WHERE actual.indexrelid = to_regclass(
                            'idx_avionics_product_reuse_origin'
                          )
                            AND actual.indrelid = to_regclass(
                              'avionics_product_reuse_attestations'
                            )
                            AND NOT actual.indisunique
                            AND lower(
                              pg_get_indexdef(actual.indexrelid)
                            ) LIKE
                              '%(avionics_authoritative_source_origin_id)%'
                        )
                        OR EXISTS (
                          SELECT 1 FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_trigger actual
                            WHERE actual.tgrelid =
                                  to_regclass(required.parent_name)
                              AND actual.tgname = required.trigger_name
                              AND actual.tgfoid =
                                  to_regprocedure(required.function_signature)
                              AND actual.tgtype = required.trigger_type
                              AND actual.tgenabled IN ('O', 'A')
                              AND NOT actual.tgisinternal
                              AND position(
                                required.definition_fragment
                                IN pg_get_functiondef(actual.tgfoid)
                              ) > 0
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_avionics_reuse_attestations = missing_avionics_reuse_attestation_objects
            || self
                .migration_contract_invalid_on(
                    connection,
                    "avionics_models",
                    AVIONICS_PRODUCT_REUSE_ATTESTATIONS_MIGRATION,
                    AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_VERSION,
                    AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_FINGERPRINT,
                )
                .await?
            || self
                .migration_contract_invalid_on(
                    connection,
                    "avionics_models",
                    AVIONICS_PRODUCT_REUSE_V2_MIGRATION,
                    AVIONICS_PRODUCT_REUSE_V2_CONTRACT_VERSION,
                    AVIONICS_PRODUCT_REUSE_V2_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_avionics_reuse_attestations {
            bail!(avionics_product_reuse_attestations_migration_required_message(self.kind()));
        }
        if self
            .migration_contract_invalid_on(
                connection,
                "avionics_models",
                AVIONICS_GROUNDED_EVIDENCE_REFRESH_MIGRATION,
                AVIONICS_GROUNDED_EVIDENCE_REFRESH_CONTRACT_VERSION,
                AVIONICS_GROUNDED_EVIDENCE_REFRESH_CONTRACT_FINGERPRINT,
            )
            .await?
        {
            bail!(avionics_grounded_evidence_refresh_migration_required_message(self.kind()));
        }
        let missing_listing_avionics_grounded_capability_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    WITH required_columns(
                      column_name, column_type, required_not_null,
                      primary_key_position, default_value
                    ) AS (
                      VALUES
                        ('listing_id', 'INTEGER', 1, 1, NULL),
                        ('plugin_submission_id', 'INTEGER', 1, 2, NULL),
                        ('occurrence_index', 'INTEGER', 1, 3, NULL),
                        ('occurrence_role', 'TEXT', 1, 4, NULL),
                        ('avionics_model_id', 'INTEGER', 1, 0, NULL),
                        ('requested_quantity', 'INTEGER', 1, 0, NULL),
                        ('configuration_action', 'TEXT', 1, 0, NULL),
                        ('request_sha256', 'TEXT', 1, 0, NULL),
                        ('capability_sha256', 'TEXT', 1, 0, NULL),
                        ('grounded_resolution_sha256', 'TEXT', 1, 0, NULL),
                        ('evidence_capture_sha256', 'TEXT', 1, 0, NULL),
                        ('extracted_listing_sha256', 'TEXT', 1, 0, NULL),
                        ('product_fingerprint', 'TEXT', 1, 0, NULL),
                        ('collision_closure_sha256', 'TEXT', 1, 0, NULL),
                        ('source_revocation_count', 'INTEGER', 1, 0, NULL),
                        ('policy_version', 'TEXT', 1, 0, NULL),
                        ('created_at', 'TEXT', 1, 0, 'CURRENT_TIMESTAMP')
                    ),
                    required_foreign_keys(
                      parent_table, child_column, parent_column,
                      update_action, delete_action, match_kind
                    ) AS (
                      VALUES
                        ('aircraft_sale_listings', 'listing_id', 'id',
                          'NO ACTION', 'CASCADE', 'NONE'),
                        ('plugin_submissions', 'plugin_submission_id', 'id',
                          'NO ACTION', 'CASCADE', 'NONE'),
                        ('avionics_models', 'avionics_model_id', 'id',
                          'NO ACTION', 'CASCADE', 'NONE')
                    ),
                    required_indexes(index_name, column_name) AS (
                      VALUES
                        ('idx_listing_avionics_grounded_capabilities_model',
                          'avionics_model_id'),
                        ('idx_listing_avionics_grounded_capabilities_submission',
                          'plugin_submission_id')
                    ),
                    required_triggers(
                      trigger_name, event_fragment, body_fragment
                    ) AS (
                      VALUES
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'beforeinsertonaircraft_sale_listing_avionics_grounded_capabilities',
                          'groundedavionicscapabilityrequiresitsexactcurrentcapture-boundlisting,approvedproduct,andsource-revocationepoch'),
                        ('listing_avionics_grounded_capabilities_immutable_update',
                          'beforeupdateonaircraft_sale_listing_avionics_grounded_capabilities',
                          'groundedavionicscapabilitiesareimmutable')
                    ),
                    required_trigger_fragments(
                      trigger_name, definition_fragment
                    ) AS (
                      VALUES
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'fromplugin_submissionssubmission'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.id=new.plugin_submission_id'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.canonical_listing_id=new.listing_id'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.rendered_html_sha256=new.evidence_capture_sha256'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.extracted_listing_jsonisnotnull'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.extraction_errorisnull'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'fromavionics_approved_product_graph_identitiesapproved'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'approved.avionics_model_id=new.avionics_model_id'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'new.source_revocation_count<>(selectcount(*)fromavionics_authoritative_source_origin_revocations)')
                    ),
                    required_checks(definition_fragment) AS (
                      VALUES
                        ('check(occurrence_index>=0)'),
                        ('check(occurrence_rolein(''primary'',''replacement''))'),
                        ('check(requested_quantity>0)'),
                        ('check(configuration_actionin(''installed'',''replaces'',''removes''))'),
                        ('check(policy_version=''listing_avionics_grounded_capability'')'),
                        ('check(occurrence_role=''primary''orrequested_quantity=1)'),
                        ('check(occurrence_role=''primary''orconfiguration_actionin(''replaces'',''removes''))'),
                        ('check(length(request_sha256)=64)'),
                        ('check(request_sha256=lower(request_sha256))'),
                        ('check(request_sha256notglob''*[^0-9a-f]*'')'),
                        ('check(length(capability_sha256)=64)'),
                        ('check(capability_sha256=lower(capability_sha256))'),
                        ('check(capability_sha256notglob''*[^0-9a-f]*'')'),
                        ('check(length(grounded_resolution_sha256)=64)'),
                        ('check(grounded_resolution_sha256=lower(grounded_resolution_sha256))'),
                        ('check(grounded_resolution_sha256notglob''*[^0-9a-f]*'')'),
                        ('check(length(evidence_capture_sha256)=64)'),
                        ('check(evidence_capture_sha256=lower(evidence_capture_sha256))'),
                        ('check(evidence_capture_sha256notglob''*[^0-9a-f]*'')'),
                        ('check(length(extracted_listing_sha256)=64)'),
                        ('check(extracted_listing_sha256=lower(extracted_listing_sha256))'),
                        ('check(extracted_listing_sha256notglob''*[^0-9a-f]*'')'),
                        ('check(length(product_fingerprint)=64)'),
                        ('check(product_fingerprint=lower(product_fingerprint))'),
                        ('check(product_fingerprintnotglob''*[^0-9a-f]*'')'),
                        ('check(length(collision_closure_sha256)=64)'),
                        ('check(collision_closure_sha256=lower(collision_closure_sha256))'),
                        ('check(collision_closure_sha256notglob''*[^0-9a-f]*'')'),
                        ('check(source_revocation_count>=0)')
                    ),
                    table_definition(normalized_sql) AS (
                      SELECT replace(replace(replace(replace(
                        lower(sql), ' ', ''), char(10), ''), char(13), ''),
                        char(9), '')
                      FROM sqlite_schema
                      WHERE type = 'table'
                        AND name =
                          'aircraft_sale_listing_avionics_grounded_capabilities'
                    ),
                    trigger_definitions(trigger_name, normalized_sql) AS (
                      SELECT name, replace(replace(replace(replace(
                        lower(sql), ' ', ''), char(10), ''), char(13), ''),
                        char(9), '')
                      FROM sqlite_schema
                      WHERE type = 'trigger'
                        AND tbl_name =
                          'aircraft_sale_listing_avionics_grounded_capabilities'
                    )
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table'
                          AND name = 'aircraft_sale_listing_avionics'
                      )
                      AND (
                        NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table'
                            AND name =
                              'aircraft_sale_listing_avionics_grounded_capabilities'
                        )
                        OR (
                          SELECT COUNT(*) FROM pragma_table_info(
                            'aircraft_sale_listing_avionics_grounded_capabilities'
                          )
                        ) <> 17
                        OR EXISTS (
                          SELECT 1 FROM required_columns required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM pragma_table_info(
                              'aircraft_sale_listing_avionics_grounded_capabilities'
                            ) actual
                            WHERE actual.name = required.column_name
                              AND upper(actual.type) = required.column_type
                              AND actual."notnull" = required.required_not_null
                              AND actual.pk = required.primary_key_position
                              AND actual.dflt_value IS required.default_value
                          )
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM pragma_foreign_key_list(
                            'aircraft_sale_listing_avionics_grounded_capabilities'
                          )
                        ) <> 3
                        OR EXISTS (
                          SELECT 1 FROM required_foreign_keys required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM pragma_foreign_key_list(
                              'aircraft_sale_listing_avionics_grounded_capabilities'
                            ) actual
                            WHERE actual."table" = required.parent_table
                              AND actual."from" = required.child_column
                              AND actual."to" = required.parent_column
                              AND upper(actual.on_update) = required.update_action
                              AND upper(actual.on_delete) = required.delete_action
                              AND upper(actual."match") = required.match_kind
                          )
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM table_definition
                          WHERE (
                            length(normalized_sql) - length(replace(
                              normalized_sql, 'check(', ''
                            ))
                          ) / length('check(') = 29
                        )
                        OR EXISTS (
                          SELECT 1 FROM required_checks required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM table_definition actual
                            WHERE instr(
                              actual.normalized_sql,
                              required.definition_fragment
                            ) > 0
                          )
                        )
                        OR EXISTS (
                          SELECT 1 FROM required_indexes required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM sqlite_schema actual
                            WHERE actual.type = 'index'
                              AND actual.name = required.index_name
                              AND actual.tbl_name =
                                'aircraft_sale_listing_avionics_grounded_capabilities'
                          )
                          OR NOT EXISTS (
                            SELECT 1 FROM pragma_index_list(
                              'aircraft_sale_listing_avionics_grounded_capabilities'
                            ) actual
                            WHERE actual.name = required.index_name
                              AND actual."unique" = 0
                              AND actual.origin = 'c'
                              AND actual.partial = 0
                          )
                          OR (
                            SELECT COUNT(*) FROM pragma_index_info(
                              required.index_name
                            )
                          ) <> 1
                          OR NOT EXISTS (
                            SELECT 1 FROM pragma_index_info(required.index_name)
                            WHERE seqno = 0 AND name = required.column_name
                          )
                        )
                        OR (
                          SELECT COUNT(*) FROM trigger_definitions
                          WHERE trigger_name IN (
                            'listing_avionics_grounded_capabilities_validate_insert',
                            'listing_avionics_grounded_capabilities_immutable_update'
                          )
                        ) <> 2
                        OR EXISTS (
                          SELECT 1 FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM trigger_definitions actual
                            WHERE actual.trigger_name = required.trigger_name
                              AND instr(
                                actual.normalized_sql,
                                required.event_fragment
                              ) > 0
                              AND instr(
                                actual.normalized_sql,
                                required.body_fragment
                              ) > 0
                          )
                        )
                        OR EXISTS (
                          SELECT 1 FROM required_trigger_fragments required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM trigger_definitions actual
                            WHERE actual.trigger_name = required.trigger_name
                              AND instr(
                                actual.normalized_sql,
                                required.definition_fragment
                              ) > 0
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH required_columns(
                      column_name, column_type, required_not_null,
                      default_expression
                    ) AS (
                      VALUES
                        ('listing_id', 'bigint', TRUE, NULL),
                        ('plugin_submission_id', 'bigint', TRUE, NULL),
                        ('occurrence_index', 'bigint', TRUE, NULL),
                        ('occurrence_role', 'text', TRUE, NULL),
                        ('avionics_model_id', 'bigint', TRUE, NULL),
                        ('requested_quantity', 'bigint', TRUE, NULL),
                        ('configuration_action', 'text', TRUE, NULL),
                        ('request_sha256', 'text', TRUE, NULL),
                        ('capability_sha256', 'text', TRUE, NULL),
                        ('grounded_resolution_sha256', 'text', TRUE, NULL),
                        ('evidence_capture_sha256', 'text', TRUE, NULL),
                        ('extracted_listing_sha256', 'text', TRUE, NULL),
                        ('product_fingerprint', 'text', TRUE, NULL),
                        ('collision_closure_sha256', 'text', TRUE, NULL),
                        ('source_revocation_count', 'bigint', TRUE, NULL),
                        ('policy_version', 'text', TRUE, NULL),
                        ('created_at', 'text', TRUE, 'CURRENT_TIMESTAMP')
                    ),
                    required_foreign_keys(
                      parent_name, child_column, parent_column
                    ) AS (
                      VALUES
                        ('public.aircraft_sale_listings', 'listing_id', 'id'),
                        ('public.plugin_submissions', 'plugin_submission_id', 'id'),
                        ('public.avionics_models', 'avionics_model_id', 'id')
                    ),
                    required_indexes(index_name, column_name) AS (
                      VALUES
                        ('public.idx_listing_avionics_grounded_capabilities_model',
                          'avionics_model_id'),
                        ('public.idx_listing_avionics_grounded_capabilities_submission',
                          'plugin_submission_id')
                    ),
                    required_checks(
                      first_fragment, second_fragment,
                      third_fragment, fourth_fragment
                    ) AS (
                      VALUES
                        ('occurrence_index', '>= 0', NULL, NULL),
                        ('occurrence_role', '= ANY', 'primary', 'replacement'),
                        ('requested_quantity', '> 0', NULL, NULL),
                        ('configuration_action', 'installed', 'replaces', 'removes'),
                        ('request_sha256', '^[0-9a-f]{64}$', NULL, NULL),
                        ('capability_sha256', '^[0-9a-f]{64}$', NULL, NULL),
                        ('grounded_resolution_sha256', '^[0-9a-f]{64}$', NULL, NULL),
                        ('evidence_capture_sha256', '^[0-9a-f]{64}$', NULL, NULL),
                        ('extracted_listing_sha256', '^[0-9a-f]{64}$', NULL, NULL),
                        ('product_fingerprint', '^[0-9a-f]{64}$', NULL, NULL),
                        ('collision_closure_sha256', '^[0-9a-f]{64}$', NULL, NULL),
                        ('source_revocation_count', '>= 0', NULL, NULL),
                        ('policy_version',
                          'listing_avionics_grounded_capability', NULL, NULL),
                        ('occurrence_role', 'requested_quantity = 1',
                          'primary', NULL),
                        ('occurrence_role', 'configuration_action',
                          'primary', 'replaces')
                    ),
                    required_triggers(
                      trigger_name, function_signature, trigger_type,
                      definition_fragment
                    ) AS (
                      VALUES
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'public.validate_listing_avionics_grounded_capability()',
                          7, 'public.plugin_submissions'),
                        ('listing_avionics_grounded_capabilities_immutable_update',
                          'public.reject_listing_avionics_grounded_capability_update()',
                          19, 'grounded avionics capabilities are immutable')
                    ),
                    required_trigger_fragments(
                      trigger_name, definition_fragment
                    ) AS (
                      VALUES
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'public.avionics_approved_product_graph_identities'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.id = NEW.plugin_submission_id'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.canonical_listing_id = NEW.listing_id'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.rendered_html_sha256 = NEW.evidence_capture_sha256'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.extracted_listing_json IS NOT NULL'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'submission.extraction_error IS NULL'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'approved.avionics_model_id = NEW.avionics_model_id'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'grounded avionics capability requires its exact current capture-bound listing, approved product, and source-revocation epoch'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'NEW.source_revocation_count <>'),
                        ('listing_avionics_grounded_capabilities_validate_insert',
                          'public.avionics_authoritative_source_origin_revocations')
                    )
                    SELECT
                      pg_catalog.to_regclass(
                        'public.aircraft_sale_listing_avionics'
                      ) IS NOT NULL
                      AND (
                        NOT EXISTS (
                          SELECT 1
                          FROM pg_catalog.pg_class relation
                          WHERE relation.oid = pg_catalog.to_regclass(
                            'public.aircraft_sale_listing_avionics_grounded_capabilities'
                          )
                            AND relation.relkind = 'r'
                            AND relation.relpersistence = 'p'
                            AND NOT relation.relispartition
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM pg_catalog.pg_attribute actual
                          WHERE actual.attrelid = pg_catalog.to_regclass(
                            'public.aircraft_sale_listing_avionics_grounded_capabilities'
                          )
                            AND actual.attnum > 0
                            AND NOT actual.attisdropped
                        ) <> 17
                        OR EXISTS (
                          SELECT 1 FROM required_columns required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_catalog.pg_attribute actual
                            LEFT JOIN pg_catalog.pg_attrdef default_value
                              ON default_value.adrelid = actual.attrelid
                             AND default_value.adnum = actual.attnum
                            WHERE actual.attrelid = pg_catalog.to_regclass(
                              'public.aircraft_sale_listing_avionics_grounded_capabilities'
                            )
                              AND actual.attname = required.column_name
                              AND pg_catalog.format_type(
                                actual.atttypid, actual.atttypmod
                              ) = required.column_type
                              AND actual.attnotnull = required.required_not_null
                              AND (
                                (required.default_expression IS NULL
                                  AND default_value.oid IS NULL)
                                OR pg_catalog.pg_get_expr(
                                  default_value.adbin, default_value.adrelid
                                ) = required.default_expression
                              )
                          )
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM pg_catalog.pg_constraint actual
                          WHERE actual.conrelid = pg_catalog.to_regclass(
                            'public.aircraft_sale_listing_avionics_grounded_capabilities'
                          )
                            AND actual.contype = 'p'
                            AND pg_catalog.pg_get_constraintdef(actual.oid) =
                              'PRIMARY KEY (listing_id, plugin_submission_id, occurrence_index, occurrence_role)'
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM pg_catalog.pg_constraint actual
                          WHERE actual.conrelid = pg_catalog.to_regclass(
                            'public.aircraft_sale_listing_avionics_grounded_capabilities'
                          )
                            AND actual.contype = 'f'
                        ) <> 3
                        OR EXISTS (
                          SELECT 1 FROM required_foreign_keys required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_catalog.pg_constraint actual
                            JOIN pg_catalog.pg_attribute child_attribute
                              ON child_attribute.attrelid = actual.conrelid
                             AND child_attribute.attnum = actual.conkey[1]
                            JOIN pg_catalog.pg_attribute parent_attribute
                              ON parent_attribute.attrelid = actual.confrelid
                             AND parent_attribute.attnum = actual.confkey[1]
                            WHERE actual.conrelid = pg_catalog.to_regclass(
                              'public.aircraft_sale_listing_avionics_grounded_capabilities'
                            )
                              AND actual.contype = 'f'
                              AND actual.confrelid =
                                pg_catalog.to_regclass(required.parent_name)
                              AND pg_catalog.array_length(actual.conkey, 1) = 1
                              AND pg_catalog.array_length(actual.confkey, 1) = 1
                              AND child_attribute.attname = required.child_column
                              AND parent_attribute.attname = required.parent_column
                              AND actual.confupdtype = 'a'
                              AND actual.confdeltype = 'c'
                              AND actual.confmatchtype = 's'
                          )
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM pg_catalog.pg_constraint actual
                          WHERE actual.conrelid = pg_catalog.to_regclass(
                            'public.aircraft_sale_listing_avionics_grounded_capabilities'
                          )
                            AND actual.contype = 'c'
                        ) <> 15
                        OR EXISTS (
                          SELECT 1 FROM required_checks required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM pg_catalog.pg_constraint actual
                            WHERE actual.conrelid = pg_catalog.to_regclass(
                              'public.aircraft_sale_listing_avionics_grounded_capabilities'
                            )
                              AND actual.contype = 'c'
                              AND pg_catalog.strpos(
                                pg_catalog.pg_get_constraintdef(actual.oid),
                                required.first_fragment
                              ) > 0
                              AND pg_catalog.strpos(
                                pg_catalog.pg_get_constraintdef(actual.oid),
                                required.second_fragment
                              ) > 0
                              AND (
                                required.third_fragment IS NULL
                                OR pg_catalog.strpos(
                                  pg_catalog.pg_get_constraintdef(actual.oid),
                                  required.third_fragment
                                ) > 0
                              )
                              AND (
                                required.fourth_fragment IS NULL
                                OR pg_catalog.strpos(
                                  pg_catalog.pg_get_constraintdef(actual.oid),
                                  required.fourth_fragment
                                ) > 0
                              )
                          )
                        )
                        OR EXISTS (
                          SELECT 1 FROM required_indexes required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM pg_catalog.pg_index actual
                            WHERE actual.indexrelid =
                                  pg_catalog.to_regclass(required.index_name)
                              AND actual.indrelid = pg_catalog.to_regclass(
                                'public.aircraft_sale_listing_avionics_grounded_capabilities'
                              )
                              AND NOT actual.indisunique
                              AND NOT actual.indisprimary
                              AND NOT actual.indisexclusion
                              AND actual.indisvalid
                              AND actual.indisready
                              AND actual.indislive
                              AND actual.indnatts = 1
                              AND actual.indnkeyatts = 1
                              AND actual.indexprs IS NULL
                              AND actual.indpred IS NULL
                              AND pg_catalog.pg_get_indexdef(
                                actual.indexrelid, 1, TRUE
                              ) = required.column_name
                          )
                        )
                        OR (
                          SELECT COUNT(*) FROM pg_catalog.pg_trigger actual
                          WHERE actual.tgrelid = pg_catalog.to_regclass(
                            'public.aircraft_sale_listing_avionics_grounded_capabilities'
                          )
                            AND actual.tgname IN (
                              'listing_avionics_grounded_capabilities_validate_insert',
                              'listing_avionics_grounded_capabilities_immutable_update'
                            )
                            AND NOT actual.tgisinternal
                        ) <> 2
                        OR EXISTS (
                          SELECT 1 FROM required_triggers required
                          WHERE NOT EXISTS (
                            SELECT 1
                            FROM pg_catalog.pg_trigger actual
                            JOIN pg_catalog.pg_proc routine
                              ON routine.oid = actual.tgfoid
                            WHERE actual.tgrelid = pg_catalog.to_regclass(
                              'public.aircraft_sale_listing_avionics_grounded_capabilities'
                            )
                              AND actual.tgname = required.trigger_name
                              AND actual.tgfoid = pg_catalog.to_regprocedure(
                                required.function_signature
                              )
                              AND actual.tgtype = required.trigger_type
                              AND actual.tgenabled = 'O'
                              AND NOT actual.tgisinternal
                              AND routine.proconfig =
                                ARRAY['search_path=pg_catalog']::text[]
                              AND pg_catalog.strpos(
                                pg_catalog.pg_get_functiondef(actual.tgfoid),
                                required.definition_fragment
                              ) > 0
                          )
                        )
                        OR EXISTS (
                          SELECT 1 FROM required_trigger_fragments required
                          WHERE NOT EXISTS (
                            SELECT 1 FROM pg_catalog.pg_trigger actual
                            WHERE actual.tgrelid = pg_catalog.to_regclass(
                              'public.aircraft_sale_listing_avionics_grounded_capabilities'
                            )
                              AND actual.tgname = required.trigger_name
                              AND pg_catalog.strpos(
                                pg_catalog.pg_get_functiondef(actual.tgfoid),
                                required.definition_fragment
                              ) > 0
                          )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let grounded_capability_definitions_are_valid = self
            .listing_avionics_grounded_capability_definitions_valid_on(connection)
            .await?;
        let authorization_definitions_are_valid = self
            .listing_avionics_authorization_definitions_valid_on(connection)
            .await?;
        let grounded_capability_contract_is_invalid = self
            .migration_contract_invalid_on(
                connection,
                "aircraft_sale_listing_avionics",
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION,
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_VERSION,
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_FINGERPRINT,
            )
            .await?;
        let missing_listing_avionics_grounded_capabilities =
            missing_listing_avionics_grounded_capability_objects
                || !grounded_capability_definitions_are_valid
                || !authorization_definitions_are_valid
                || grounded_capability_contract_is_invalid;
        if missing_listing_avionics_grounded_capabilities {
            bail!(listing_avionics_grounded_capabilities_migration_required_message(self.kind()));
        }
        let missing_occurrence_dispositions = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1 FROM sqlite_schema
                      WHERE type = 'table' AND name = 'plugin_submissions'
                    ) AND NOT EXISTS (
                      SELECT 1 FROM sqlite_schema
                      WHERE type = 'table'
                        AND name = 'aircraft_sale_listing_avionics_dispositions'
                    )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT to_regclass('plugin_submissions') IS NOT NULL
                       AND to_regclass('aircraft_sale_listing_avionics_dispositions') IS NULL
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        if missing_occurrence_dispositions {
            bail!(migration_required_message(
                self.kind(),
                "aircraft_sale_listing_avionics_dispositions",
                "occurrence_fingerprint",
                LISTING_AVIONICS_DISPOSITIONS_MIGRATION,
            ));
        }
        let missing_approved_concrete_model_contract = self
            .migration_contract_invalid_on(
                connection,
                match self.kind() {
                    DatabaseKind::Sqlite => "avionics_models",
                    DatabaseKind::Postgres => "public.avionics_models",
                },
                AVIONICS_GENERIC_FEATURE_LABELS_MIGRATION,
                AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_VERSION,
                AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_FINGERPRINT,
            )
            .await?;
        let approved_concrete_model_anchor_exists = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'avionics_models')",
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    "SELECT pg_catalog.to_regclass('public.avionics_models') IS NOT NULL",
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let missing_approved_concrete_model_enforcement = approved_concrete_model_anchor_exists
            && !self
                .avionics_approved_concrete_model_object_contract_valid_on(connection)
                .await?;
        if missing_approved_concrete_model_contract || missing_approved_concrete_model_enforcement {
            bail!(migration_required_message(
                self.kind(),
                "avionics_models",
                "approved concrete-model invariant",
                AVIONICS_GENERIC_FEATURE_LABELS_MIGRATION,
            ));
        }
        let faa_registry_schema_started = self.faa_registry_schema_started_on(connection).await?;
        let missing_faa_reference_contract = match &mut *connection {
            GateConnection::Sqlite(_) => false,
            GateConnection::Postgres(_) => {
                self.migration_contract_invalid_on(
                    connection,
                    "public.faa_registry_aircraft_references",
                    FAA_REFERENCE_REACHABILITY_MIGRATION,
                    FAA_REFERENCE_REACHABILITY_CONTRACT_VERSION,
                    FAA_REFERENCE_REACHABILITY_CONTRACT_FINGERPRINT,
                )
                .await?
            }
        };
        let missing_faa_record_hash_domain_contract = self
            .migration_contract_invalid_on(
                connection,
                match self.kind() {
                    DatabaseKind::Sqlite => "faa_registry_snapshots",
                    DatabaseKind::Postgres => "public.faa_registry_snapshots",
                },
                FAA_RECORD_HASH_DOMAIN_MIGRATION,
                FAA_RECORD_HASH_DOMAIN_CONTRACT_VERSION,
                FAA_RECORD_HASH_DOMAIN_CONTRACT_FINGERPRINT,
            )
            .await?;
        if faa_registry_schema_started {
            if missing_faa_record_hash_domain_contract {
                bail!(faa_record_hash_domain_migration_required_message(
                    self.kind()
                ));
            }
            let contract_problem = if missing_faa_reference_contract {
                Some(String::from("migration contract marker"))
            } else {
                self.faa_registry_contract_problem_on(connection).await?
            };
            if let Some(problem) = contract_problem {
                bail!(faa_registry_contract_required_message(
                    self.kind(),
                    &problem
                ));
            }
        }
        let missing_aircraft_listing_identity_correction_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1 FROM sqlite_schema
                      WHERE type = 'table' AND name = 'aircraft_sale_listings'
                    ) AND (
                      NOT EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table'
                          AND name = 'aircraft_listing_identity_correction_decisions'
                      ) OR NOT EXISTS (
                        SELECT 1 FROM pragma_index_list('plugin_submissions') actual
                        WHERE actual.name = 'uq_plugin_submissions_signed_capture'
                          AND actual.[unique] = 1
                          AND (
                            SELECT group_concat(name, ',') FROM (
                              SELECT name FROM pragma_index_info(actual.name)
                              ORDER BY seqno
                            )
                          ) = 'user_id,plugin_install_id,source_url,rendered_html_sha256'
                      ) OR NOT EXISTS (
                        SELECT 1 FROM pragma_index_list(
                          'aircraft_listing_identity_correction_decisions'
                        ) actual
                        WHERE actual.name = 'uq_aircraft_listing_identity_correction_receipt'
                          AND actual.[unique] = 1
                          AND (
                            SELECT group_concat(name, ',') FROM (
                              SELECT name FROM pragma_index_info(actual.name)
                              ORDER BY seqno
                            )
                          ) = 'plugin_submission_id,correction_kind'
                      )
                    )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT pg_catalog.to_regclass('public.aircraft_sale_listings') IS NOT NULL AND (
                      pg_catalog.to_regclass(
                        'public.aircraft_listing_identity_correction_decisions'
                      ) IS NULL
                      OR NOT EXISTS (
                        SELECT 1
                        FROM pg_catalog.pg_class index_class
                        JOIN pg_catalog.pg_index actual
                          ON actual.indexrelid = index_class.oid
                        WHERE index_class.oid = pg_catalog.to_regclass(
                          'public.uq_plugin_submissions_signed_capture'
                        )
                          AND actual.indrelid = pg_catalog.to_regclass(
                            'public.plugin_submissions'
                          )
                          AND actual.indisunique
                          AND (
                            SELECT array_agg(
                              attribute.attname::text ORDER BY index_key.ordinality
                            )
                            FROM unnest(actual.indkey) WITH ORDINALITY
                              AS index_key(attnum, ordinality)
                            JOIN pg_catalog.pg_attribute attribute
                              ON attribute.attrelid = actual.indrelid
                             AND attribute.attnum = index_key.attnum
                          ) = ARRAY[
                            'user_id', 'plugin_install_id', 'source_url',
                            'rendered_html_sha256'
                          ]::text[]
                      )
                      OR NOT EXISTS (
                        SELECT 1
                        FROM pg_catalog.pg_class index_class
                        JOIN pg_catalog.pg_index actual
                          ON actual.indexrelid = index_class.oid
                        WHERE index_class.oid = pg_catalog.to_regclass(
                          'public.uq_aircraft_listing_identity_correction_receipt'
                        )
                          AND actual.indrelid = pg_catalog.to_regclass(
                            'public.aircraft_listing_identity_correction_decisions'
                          )
                          AND actual.indisunique
                          AND (
                            SELECT array_agg(
                              attribute.attname::text ORDER BY index_key.ordinality
                            )
                            FROM unnest(actual.indkey) WITH ORDINALITY
                              AS index_key(attnum, ordinality)
                            JOIN pg_catalog.pg_attribute attribute
                              ON attribute.attrelid = actual.indrelid
                             AND attribute.attnum = index_key.attnum
                          ) = ARRAY['plugin_submission_id', 'correction_kind']::text[]
                      )
                    )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let aircraft_listing_identity_correction_schema_started = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1 FROM sqlite_schema
                      WHERE (type = 'table' AND name =
                        'aircraft_listing_identity_correction_decisions')
                         OR (type = 'index' AND name IN (
                           'uq_plugin_submissions_signed_capture',
                           'uq_aircraft_listing_identity_correction_receipt'
                         ))
                         OR (type = 'trigger' AND name IN (
                           'aircraft_listing_identity_corrections_immutable_update',
                           'aircraft_listing_identity_corrections_immutable_delete',
                           'aircraft_identity_correction_observation_immutable_update',
                           'aircraft_identity_correction_observation_immutable_delete',
                           'aircraft_source_identity_receipt_gate'
                         ))
                    )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT
                      pg_catalog.to_regclass(
                        'public.aircraft_listing_identity_correction_decisions'
                      ) IS NOT NULL
                      OR pg_catalog.to_regclass(
                        'public.uq_plugin_submissions_signed_capture'
                      ) IS NOT NULL
                      OR pg_catalog.to_regclass(
                        'public.uq_aircraft_listing_identity_correction_receipt'
                      ) IS NOT NULL
                      OR EXISTS (
                        SELECT 1 FROM pg_catalog.pg_trigger
                        WHERE NOT tgisinternal
                          AND tgname IN (
                            'aircraft_listing_identity_corrections_immutable',
                            'aircraft_identity_correction_observation_immutable',
                            'aircraft_source_identity_receipt_gate'
                          )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let invalid_aircraft_listing_identity_correction_definitions =
            if missing_aircraft_listing_identity_correction_objects
                || !aircraft_listing_identity_correction_schema_started
            {
                false
            } else {
                !self
                    .aircraft_listing_identity_correction_definitions_valid_on(connection)
                    .await?
            };
        let missing_aircraft_listing_identity_corrections =
            missing_aircraft_listing_identity_correction_objects
                || invalid_aircraft_listing_identity_correction_definitions
                || self
                    .migration_contract_invalid_on(
                        connection,
                        match self.kind() {
                            DatabaseKind::Sqlite => {
                                "aircraft_listing_identity_correction_decisions"
                            }
                            DatabaseKind::Postgres => {
                                "public.aircraft_listing_identity_correction_decisions"
                            }
                        },
                        AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_MIGRATION,
                        AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_VERSION,
                        AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_FINGERPRINT,
                    )
                    .await?;
        if missing_aircraft_listing_identity_corrections {
            bail!(aircraft_listing_identity_corrections_migration_required_message(self.kind()));
        }
        let aircraft_visual_source_correction_state = self
            .migration_contract_state_on(
                connection,
                match self.kind() {
                    DatabaseKind::Sqlite => "aircraft_source_visual_correction_artifacts",
                    DatabaseKind::Postgres => "public.aircraft_source_visual_correction_artifacts",
                },
                AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_MIGRATION,
                AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_VERSION,
                AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_FINGERPRINT,
            )
            .await?;
        let missing_aircraft_visual_source_corrections =
            match aircraft_visual_source_correction_state {
                MigrationContractState::Fresh => false,
                MigrationContractState::Installed => {
                    !self
                        .aircraft_visual_source_correction_definitions_valid_on(connection)
                        .await?
                }
                MigrationContractState::Invalid => true,
            };
        if missing_aircraft_visual_source_corrections {
            bail!(aircraft_visual_source_corrections_migration_required_message(self.kind()));
        }
        let missing_listing_replay_objects = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                SELECT EXISTS (
                  SELECT 1 FROM sqlite_schema
                  WHERE type = 'table' AND name = 'plugin_submissions'
                ) AND (
                  NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'table' AND name = 'listing_replay_runs'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'table' AND name = 'listing_replay_run_items'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'table'
                      AND name = 'plugin_submission_materialization_receipts'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'index' AND name = 'idx_listing_replay_runs_one_running'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'index' AND name = 'idx_listing_replay_run_items_phase'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'index'
                      AND name = 'uq_aircraft_sale_listings_owner_source'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND name = 'listing_replay_run_items_checkpoint_exact_insert'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND name = 'listing_replay_run_items_checkpoint_exact_update'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND name = 'listing_replay_run_items_completed_immutable_update'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND name = 'listing_replay_run_items_completed_immutable_delete'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND name = 'plugin_submission_materialization_receipts_immutable_update'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND name = 'plugin_submission_materialization_receipts_immutable_delete'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND name = 'plugin_submissions_replay_checkpoint_immutable'
                  ) OR NOT EXISTS (
                    SELECT 1 FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND name = 'plugin_installs_replay_identity_immutable'
                  ) OR (
                    SELECT COUNT(*) FROM pragma_table_info('listing_replay_runs')
                    WHERE name IN ('manifest_sha256', 'status', 'active_phase',
                      'owner_token', 'heartbeat_at_epoch_seconds')
                  ) <> 5 OR (
                    SELECT COUNT(*) FROM pragma_table_info('listing_replay_run_items')
                    WHERE name IN ('plugin_submission_id', 'expected_rendered_html_sha256',
                      'extraction_state', 'materialization_state', 'resulting_listing_id',
                      'terminal_rejection_stage', 'terminal_rejection_reason_code')
                  ) <> 7 OR NOT EXISTS (
                    SELECT 1 FROM pragma_foreign_key_list('listing_replay_run_items')
                    WHERE [from] = 'resulting_listing_id' AND [table] = 'aircraft_sale_listings'
                      AND [on_delete] = 'RESTRICT'
                  )
                )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                SELECT pg_catalog.to_regclass('public.plugin_submissions') IS NOT NULL
                  AND (
                    pg_catalog.to_regclass('public.listing_replay_runs') IS NULL
                    OR pg_catalog.to_regclass('public.listing_replay_run_items') IS NULL
                    OR pg_catalog.to_regclass(
                      'public.plugin_submission_materialization_receipts'
                    ) IS NULL
                    OR pg_catalog.to_regclass('public.idx_listing_replay_runs_one_running') IS NULL
                    OR pg_catalog.to_regclass('public.idx_listing_replay_run_items_phase') IS NULL
                    OR pg_catalog.to_regclass(
                      'public.uq_aircraft_sale_listings_owner_source'
                    ) IS NULL
                    OR NOT EXISTS (
                      SELECT 1 FROM pg_catalog.pg_trigger
                      WHERE NOT tgisinternal
                        AND tgname IN (
                          'listing_replay_run_items_checkpoint_exact',
                          'listing_replay_run_items_completed_immutable',
                          'plugin_submission_materialization_receipts_immutable',
                          'plugin_submissions_replay_checkpoint_immutable',
                          'plugin_installs_replay_identity_immutable'
                        )
                      HAVING COUNT(*) = 5
                    )
                    OR (
                      SELECT COUNT(*) FROM pg_catalog.pg_attribute
                      WHERE attrelid = pg_catalog.to_regclass('public.listing_replay_runs')
                        AND attname IN ('manifest_sha256', 'status', 'active_phase',
                          'owner_token', 'heartbeat_at_epoch_seconds')
                        AND NOT attisdropped
                    ) <> 5
                    OR (
                      SELECT COUNT(*) FROM pg_catalog.pg_attribute
                      WHERE attrelid = pg_catalog.to_regclass('public.listing_replay_run_items')
                        AND attname IN ('plugin_submission_id', 'expected_rendered_html_sha256',
                          'extraction_state', 'materialization_state', 'resulting_listing_id',
                          'terminal_rejection_stage', 'terminal_rejection_reason_code')
                        AND NOT attisdropped
                    ) <> 7
                    OR NOT EXISTS (
                      SELECT 1 FROM pg_catalog.pg_constraint
                      WHERE conrelid = pg_catalog.to_regclass('public.listing_replay_run_items')
                        AND contype = 'f'
                        AND pg_catalog.pg_get_constraintdef(oid) LIKE
                          'FOREIGN KEY (resulting_listing_id)%ON DELETE RESTRICT%'
                    )
                  )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let listing_replay_schema_started = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                SELECT EXISTS (
                  SELECT 1 FROM sqlite_schema
                  WHERE name IN (
                    'listing_replay_runs', 'listing_replay_run_items',
                    'plugin_submission_materialization_receipts',
                    'idx_listing_replay_runs_one_running',
                    'idx_listing_replay_run_items_phase',
                    'uq_aircraft_sale_listings_owner_source',
                    'listing_replay_run_items_checkpoint_exact_insert',
                    'listing_replay_run_items_checkpoint_exact_update',
                    'listing_replay_run_items_completed_immutable_update',
                    'listing_replay_run_items_completed_immutable_delete',
                    'plugin_submission_materialization_receipts_immutable_update',
                    'plugin_submission_materialization_receipts_immutable_delete',
                    'plugin_submissions_replay_checkpoint_immutable',
                    'plugin_installs_replay_identity_immutable'
                  )
                )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                SELECT pg_catalog.to_regclass('public.listing_replay_runs') IS NOT NULL
                  OR pg_catalog.to_regclass('public.listing_replay_run_items') IS NOT NULL
                  OR pg_catalog.to_regclass(
                    'public.plugin_submission_materialization_receipts'
                  ) IS NOT NULL
                  OR pg_catalog.to_regclass(
                    'public.idx_listing_replay_runs_one_running'
                  ) IS NOT NULL
                  OR pg_catalog.to_regclass(
                    'public.idx_listing_replay_run_items_phase'
                  ) IS NOT NULL
                  OR pg_catalog.to_regclass(
                    'public.uq_aircraft_sale_listings_owner_source'
                  ) IS NOT NULL
                  OR EXISTS (
                    SELECT 1 FROM pg_catalog.pg_trigger
                    WHERE NOT tgisinternal AND tgname IN (
                      'listing_replay_run_items_checkpoint_exact',
                      'listing_replay_run_items_completed_immutable',
                      'plugin_submission_materialization_receipts_immutable',
                      'plugin_submissions_replay_checkpoint_immutable',
                      'plugin_installs_replay_identity_immutable'
                    )
                  )
                "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let invalid_listing_replay_definitions =
            if missing_listing_replay_objects || !listing_replay_schema_started {
                false
            } else {
                !self.listing_replay_definitions_valid_on(connection).await?
            };
        let missing_listing_replay_runs = missing_listing_replay_objects
            || invalid_listing_replay_definitions
            || self
                .migration_contract_invalid_on(
                    connection,
                    match self.kind() {
                        DatabaseKind::Sqlite => "listing_replay_runs",
                        DatabaseKind::Postgres => "public.listing_replay_runs",
                    },
                    LISTING_REPLAY_RUNS_MIGRATION,
                    LISTING_REPLAY_RUNS_CONTRACT_VERSION,
                    LISTING_REPLAY_RUNS_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_listing_replay_runs {
            bail!(listing_replay_runs_migration_required_message(self.kind()));
        }
        let reference_catalog_cutover_started = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                let has_object = sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1 FROM sqlite_schema
                      WHERE name IN (
                        'aircraft_reference_configuration_versions',
                        'aircraft_reference_fact_set_attestations',
                        'official_dollar_normalization_facts',
                        'aircraft_reference_fact_set_building_insert',
                        'aircraft_reference_fact_set_immutable_update',
                        'aircraft_reference_fact_set_immutable_delete'
                      )
                    ) OR EXISTS (
                      SELECT 1 FROM pragma_table_info('aircraft_reference_prices')
                      WHERE name = 'configuration_basis'
                    )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0;
                let has_ledger = sqlx::query_scalar::<_, i64>(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_migration_contracts')",
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0;
                let has_marker = if has_ledger {
                    sqlx::query_scalar::<_, i64>(
                        "SELECT EXISTS (SELECT 1 FROM schema_migration_contracts WHERE migration_name = ?)",
                    )
                    .bind(REFERENCE_CATALOG_CUTOVER_MIGRATION)
                    .fetch_one(&mut **pool)
                    .await?
                        != 0
                } else {
                    false
                };
                has_object || has_marker
            }
            GateConnection::Postgres(pool) => {
                let has_object = sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT
                      pg_catalog.to_regclass(
                        'public.aircraft_reference_configuration_versions'
                      ) IS NOT NULL
                      OR pg_catalog.to_regclass(
                        'public.aircraft_reference_fact_set_attestations'
                      ) IS NOT NULL
                      OR pg_catalog.to_regclass(
                        'public.official_dollar_normalization_facts'
                      ) IS NOT NULL
                      OR EXISTS (
                        SELECT 1
                        FROM pg_catalog.pg_proc routine
                        JOIN pg_catalog.pg_namespace namespace
                          ON namespace.oid = routine.pronamespace
                        WHERE namespace.nspname = 'public'
                          AND routine.proname = ANY($1)
                      )
                    "#,
                )
                .bind(
                    &REFERENCE_CATALOG_CUTOVER_ROUTINES
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect::<Vec<_>>(),
                )
                .fetch_one(&mut **pool)
                .await?;
                let has_ledger = sqlx::query_scalar::<_, bool>(
                    "SELECT pg_catalog.to_regclass('public.schema_migration_contracts') IS NOT NULL",
                )
                .fetch_one(&mut **pool)
                .await?;
                let has_marker = if has_ledger {
                    sqlx::query_scalar::<_, bool>(
                        "SELECT EXISTS (SELECT 1 FROM ONLY public.schema_migration_contracts WHERE migration_name = $1)",
                    )
                    .bind(REFERENCE_CATALOG_CUTOVER_MIGRATION)
                    .fetch_one(&mut **pool)
                    .await?
                } else {
                    false
                };
                has_object || has_marker
            }
        };
        let invalid_reference_catalog_cutover_shape = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table'
                          AND name = 'aircraft_reference_configuration_versions'
                      )
                      AND (
                        NOT EXISTS (
                          SELECT 1 FROM pragma_table_info('aircraft_reference_prices')
                          WHERE name = 'configuration_basis'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table'
                            AND name = 'aircraft_reference_fact_set_attestations'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table'
                            AND name = 'official_dollar_normalization_facts'
                        )
                        OR EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'table'
                            AND name IN (
                              'aircraft_model_spec_versions',
                              'aircraft_model_variant_price_points',
                              'aircraft_model_variant_default_avionics',
                              'aircraft_model_variant_default_avionics_candidates',
                              'depreciation_profiles',
                              'depreciation_profile_fit_metadata',
                              'component_depreciation_profiles'
                            )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT
                      pg_catalog.to_regclass(
                        'public.aircraft_reference_configuration_versions'
                      ) IS NOT NULL
                      AND (
                        NOT EXISTS (
                          SELECT 1
                          FROM pg_catalog.pg_attribute attribute
                          WHERE attribute.attrelid = pg_catalog.to_regclass(
                            'public.aircraft_reference_prices'
                          )
                            AND attribute.attname = 'configuration_basis'
                            AND NOT attribute.attisdropped
                        )
                        OR pg_catalog.to_regclass(
                          'public.aircraft_reference_fact_set_attestations'
                        ) IS NULL
                        OR pg_catalog.to_regclass(
                          'public.official_dollar_normalization_facts'
                        ) IS NULL
                        OR EXISTS (
                          SELECT 1
                          FROM unnest(ARRAY[
                            'aircraft_model_spec_versions',
                            'aircraft_model_variant_price_points',
                            'aircraft_model_variant_default_avionics',
                            'aircraft_model_variant_default_avionics_candidates',
                            'depreciation_profiles',
                            'depreciation_profile_fit_metadata',
                            'component_depreciation_profiles'
                          ]) AS legacy(table_name)
                          WHERE pg_catalog.to_regclass(
                            'public.' || legacy.table_name
                          ) IS NOT NULL
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        let invalid_reference_catalog_cutover_definitions = reference_catalog_cutover_started
            && !self
                .reference_catalog_cutover_definitions_valid_on(connection)
                .await?;
        if invalid_reference_catalog_cutover_shape
            || invalid_reference_catalog_cutover_definitions
            || self
                .migration_contract_invalid_on(
                    connection,
                    match self.kind() {
                        DatabaseKind::Sqlite => "aircraft_reference_configuration_versions",
                        DatabaseKind::Postgres => {
                            "public.aircraft_reference_configuration_versions"
                        }
                    },
                    REFERENCE_CATALOG_CUTOVER_MIGRATION,
                    REFERENCE_CATALOG_CUTOVER_CONTRACT_VERSION,
                    REFERENCE_CATALOG_CUTOVER_CONTRACT_FINGERPRINT,
                )
                .await?
        {
            bail!(reference_catalog_cutover_migration_required_message(
                self.kind()
            ));
        }
        Ok(())
    }

    async fn listing_avionics_grounded_capability_definitions_valid_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<bool> {
        const TABLE: &str = "aircraft_sale_listing_avionics_grounded_capabilities";
        const SQLITE_OBJECTS: &[(&str, &str)] = &[
            ("index", "idx_listing_avionics_grounded_capabilities_model"),
            (
                "index",
                "idx_listing_avionics_grounded_capabilities_submission",
            ),
            (
                "trigger",
                "listing_avionics_grounded_capabilities_immutable_update",
            ),
            (
                "trigger",
                "listing_avionics_grounded_capabilities_validate_insert",
            ),
        ];
        const POSTGRES_CHECKS: &[&str] = &[
            "CHECK (((occurrence_role = 'primary'::text) OR (configuration_action = ANY (ARRAY['replaces'::text, 'removes'::text]))))",
            "CHECK (((occurrence_role = 'primary'::text) OR (requested_quantity = 1)))",
            "CHECK ((capability_sha256 ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((collision_closure_sha256 ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((configuration_action = ANY (ARRAY['installed'::text, 'replaces'::text, 'removes'::text])))",
            "CHECK ((evidence_capture_sha256 ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((extracted_listing_sha256 ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((grounded_resolution_sha256 ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((occurrence_index >= 0))",
            "CHECK ((occurrence_role = ANY (ARRAY['primary'::text, 'replacement'::text])))",
            "CHECK ((policy_version = 'listing_avionics_grounded_capability'::text))",
            "CHECK ((product_fingerprint ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((request_sha256 ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((requested_quantity > 0))",
            "CHECK ((source_revocation_count >= 0))",
        ];
        const POSTGRES_TRIGGERS: &[(&str, i16, &str)] = &[
            (
                "listing_avionics_grounded_capabilities_immutable_update",
                19,
                "reject_listing_avionics_grounded_capability_update",
            ),
            (
                "listing_avionics_grounded_capabilities_validate_insert",
                7,
                "validate_listing_avionics_grounded_capability",
            ),
        ];

        match &mut *connection {
            GateConnection::Sqlite(pool) => {
                let table_definition = sqlx::query_scalar::<_, String>(
                    "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?",
                )
                .bind(TABLE)
                .fetch_optional(&mut **pool)
                .await?;
                let Some(table_definition) = table_definition else {
                    return Ok(true);
                };
                let table_is_exact =
                    canonical_sql_definition(&table_definition).replacen(
                        "createtableifnotexists",
                        "createtable",
                        1,
                    ) == canonical_sqlite_table_definition(SQLITE_SCHEMA_SQL, TABLE)
                        .expect("canonical grounded-capability table must exist");
                let actual_objects = sqlx::query_as::<_, (String, String, Option<String>)>(
                    "SELECT type, name, sql FROM sqlite_schema \
                     WHERE tbl_name = ? AND type IN ('index', 'trigger') \
                       AND sql IS NOT NULL ORDER BY type, name",
                )
                .bind(TABLE)
                .fetch_all(&mut **pool)
                .await?;
                let expected_objects = SQLITE_OBJECTS
                    .iter()
                    .map(|(object_type, name)| {
                        (
                            (*object_type).to_owned(),
                            (*name).to_owned(),
                            canonical_sqlite_named_definition(SQLITE_SCHEMA_SQL, name)
                                .unwrap_or_else(|| {
                                    panic!("canonical grounded-capability object {name} must exist")
                                }),
                        )
                    })
                    .collect::<Vec<_>>();
                let objects_are_exact = actual_objects.len() == expected_objects.len()
                    && actual_objects.iter().zip(&expected_objects).all(
                        |(
                            (actual_type, actual_name, actual_definition),
                            (expected_type, expected_name, expected_definition),
                        )| {
                            actual_type == expected_type
                                && actual_name == expected_name
                                && actual_definition.as_deref().is_some_and(|actual| {
                                    canonical_sqlite_schema_definition(actual)
                                        == *expected_definition
                                })
                        },
                    );
                Ok(table_is_exact && objects_are_exact)
            }
            GateConnection::Postgres(pool) => {
                let table_exists = sqlx::query_scalar::<_, bool>(
                    "SELECT pg_catalog.to_regclass( \
                       'public.aircraft_sale_listing_avionics_grounded_capabilities' \
                     ) IS NOT NULL",
                )
                .fetch_one(&mut **pool)
                .await?;
                if !table_exists {
                    return Ok(true);
                }
                let actual_checks = sqlx::query_scalar::<_, String>(
                    r#"
                    SELECT pg_catalog.pg_get_constraintdef(constraint_row.oid)
                    FROM pg_catalog.pg_constraint constraint_row
                    WHERE constraint_row.conrelid = pg_catalog.to_regclass(
                      'public.aircraft_sale_listing_avionics_grounded_capabilities'
                    )
                      AND constraint_row.contype = 'c'
                    ORDER BY pg_catalog.pg_get_constraintdef(constraint_row.oid)
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                if actual_checks.len() != POSTGRES_CHECKS.len()
                    || !actual_checks
                        .iter()
                        .zip(POSTGRES_CHECKS)
                        .all(|(actual, expected)| actual == expected)
                {
                    return Ok(false);
                }

                let actual_triggers =
                    sqlx::query_as::<_, PostgresGroundedCapabilityTriggerDefinition>(
                        r#"
                    SELECT trigger_row.tgname AS trigger_name,
                           trigger_row.tgtype AS trigger_type,
                           trigger_row.tgqual IS NULL AS has_no_when_clause,
                           trigger_row.tgenabled = 'O' AS is_enabled,
                           trigger_row.tgnargs = 0 AS has_zero_trigger_args,
                           trigger_row.tgparentid = 0 AS is_uninherited,
                           trigger_row.tgconstraint = 0 AS is_non_constraint,
                           trigger_row.tgoldtable IS NULL
                             AND trigger_row.tgnewtable IS NULL
                             AS has_no_transition_tables,
                           trigger_row.tgattr::text = '' AS has_no_update_columns,
                           pg_catalog.pg_get_triggerdef(trigger_row.oid)
                             AS definition,
                           routine_namespace.nspname AS function_namespace,
                           routine.proname AS function_name,
                           routine.prosrc AS function_source,
                           COALESCE(
                             pg_catalog.array_to_string(routine.proconfig, E'\n'),
                             ''
                           ) AS function_config,
                           NOT routine.prosecdef AS is_security_invoker,
                           routine.prokind = 'f' AS is_ordinary_function,
                           routine.prorettype = pg_catalog.to_regtype(
                             'pg_catalog.trigger'
                           ) AS returns_trigger,
                           routine.pronargs = 0 AS has_zero_function_args,
                           language_row.lanname AS language_name,
                           NOT routine.proisstrict AS is_not_strict,
                           routine.provolatile = 'v' AS is_volatile,
                           routine.proparallel = 'u' AS is_parallel_unsafe,
                           NOT routine.proleakproof AS is_not_leakproof
                    FROM pg_catalog.pg_trigger trigger_row
                    JOIN pg_catalog.pg_proc routine
                      ON routine.oid = trigger_row.tgfoid
                    JOIN pg_catalog.pg_namespace routine_namespace
                      ON routine_namespace.oid = routine.pronamespace
                    JOIN pg_catalog.pg_language language_row
                      ON language_row.oid = routine.prolang
                    WHERE trigger_row.tgrelid = pg_catalog.to_regclass(
                      'public.aircraft_sale_listing_avionics_grounded_capabilities'
                    )
                      AND NOT trigger_row.tgisinternal
                    ORDER BY trigger_row.tgname
                    "#,
                    )
                    .fetch_all(&mut **pool)
                    .await?;
                Ok(actual_triggers.len() == POSTGRES_TRIGGERS.len()
                    && actual_triggers.iter().zip(POSTGRES_TRIGGERS).all(
                        |(actual, (expected_name, expected_type, expected_function))| {
                            actual.trigger_name == *expected_name
                                && actual.trigger_type == *expected_type
                                && actual.has_no_when_clause
                                && actual.is_enabled
                                && actual.has_zero_trigger_args
                                && actual.is_uninherited
                                && actual.is_non_constraint
                                && actual.has_no_transition_tables
                                && actual.has_no_update_columns
                                && actual.function_namespace == "public"
                                && actual.function_name == *expected_function
                                && actual.function_config == "search_path=pg_catalog"
                                && actual.is_security_invoker
                                && actual.is_ordinary_function
                                && actual.returns_trigger
                                && actual.has_zero_function_args
                                && actual.language_name == "plpgsql"
                                && actual.is_not_strict
                                && actual.is_volatile
                                && actual.is_parallel_unsafe
                                && actual.is_not_leakproof
                                && canonical_postgres_trigger_definition(&actual.definition)
                                    == canonical_postgres_named_trigger_definition(
                                        POSTGRES_SCHEMA_SQL,
                                        expected_name,
                                    )
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "canonical grounded-capability trigger {expected_name} must exist"
                                        )
                                    })
                                && canonical_sql_definition(&actual.function_source)
                                    == canonical_sql_definition(
                                        postgres_function_source(
                                            POSTGRES_SCHEMA_SQL,
                                            expected_function,
                                        )
                                        .unwrap_or_else(|| {
                                            panic!(
                                                "canonical grounded-capability function {expected_function} must exist"
                                            )
                                        }),
                                    )
                        },
                    ))
            }
        }
    }

    async fn listing_avionics_authorization_definitions_valid_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<bool> {
        const TABLE: &str = "aircraft_sale_listing_avionics_link_authorizations";
        const SQLITE_INDEXES: &[&str] = &["idx_listing_avionics_authorizations_model"];
        const TRIGGERS: &[(&str, i16, bool, &str)] = &[
            (
                "listing_avionics_authorizations_immutable_update",
                19,
                true,
                "preserve_listing_avionics_authorization",
            ),
            (
                "listing_avionics_authorizations_invalidate_capture_delete",
                9,
                true,
                "invalidate_listing_avionics_authorization_for_capture",
            ),
            (
                "listing_avionics_authorizations_invalidate_capture_update",
                17,
                false,
                "invalidate_listing_avionics_authorization_for_capture",
            ),
            (
                "listing_avionics_authorizations_invalidate_graph_delete",
                9,
                true,
                "invalidate_listing_avionics_same_case_for_graph",
            ),
            (
                "listing_avionics_authorizations_invalidate_graph_insert",
                5,
                true,
                "invalidate_listing_avionics_same_case_for_graph",
            ),
            (
                "listing_avionics_authorizations_invalidate_graph_update",
                17,
                false,
                "invalidate_listing_avionics_same_case_for_graph",
            ),
            (
                "listing_avionics_authorizations_invalidate_link_update",
                17,
                false,
                "invalidate_listing_avionics_authorization_for_link",
            ),
            (
                "listing_avionics_authorizations_invalidate_manufacturer_update",
                17,
                false,
                "invalidate_listing_avionics_same_case_for_manufacturer",
            ),
            (
                "listing_avionics_authorizations_invalidate_model_proof_update",
                17,
                false,
                "invalidate_listing_avionics_same_case_for_model_proof",
            ),
            (
                "listing_avionics_authorizations_invalidate_model_type_delete",
                9,
                true,
                "invalidate_listing_avionics_same_case_for_model_type",
            ),
            (
                "listing_avionics_authorizations_invalidate_model_type_insert",
                5,
                true,
                "invalidate_listing_avionics_same_case_for_model_type",
            ),
            (
                "listing_avionics_authorizations_invalidate_model_type_update",
                17,
                false,
                "invalidate_listing_avionics_same_case_for_model_type",
            ),
            (
                "listing_avionics_authorizations_invalidate_origin_revocation",
                5,
                true,
                "invalidate_listing_avionics_same_case_for_origin_revocation",
            ),
            (
                "listing_avionics_authorizations_invalidate_reuse_delete",
                9,
                true,
                "invalidate_listing_avionics_authorization_for_reuse",
            ),
            (
                "listing_avionics_authorizations_invalidate_type_update",
                17,
                false,
                "invalidate_listing_avionics_same_case_for_type",
            ),
            (
                "listing_avionics_authorizations_validate_insert",
                7,
                true,
                "validate_listing_avionics_authorization",
            ),
        ];
        const POSTGRES_CHECKS: &[&str] = &[
            "CHECK ((((authorization_kind = 'manufacturer_reuse'::text) AND (grounded_resolution_sha256 IS NULL) AND (plugin_submission_id IS NULL) AND (extracted_listing_sha256 IS NULL) AND (source_revocation_count IS NULL)) OR ((authorization_kind = 'same_case_grounded'::text) AND (grounded_resolution_sha256 ~ '^[0-9a-f]{64}$'::text) AND (plugin_submission_id IS NOT NULL) AND (extracted_listing_sha256 IS NOT NULL) AND (source_revocation_count IS NOT NULL) AND (source_revocation_count >= 0))))",
            "CHECK (((extracted_listing_sha256 IS NULL) OR (extracted_listing_sha256 ~ '^[0-9a-f]{64}$'::text)))",
            "CHECK ((association_role = ANY (ARRAY['installed'::text, 'replacement'::text])))",
            "CHECK ((authorization_kind = ANY (ARRAY['manufacturer_reuse'::text, 'same_case_grounded'::text])))",
            "CHECK ((collision_closure_sha256 ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((evidence_capture_sha256 ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((observation_sha256 ~ '^[0-9a-f]{64}$'::text))",
            "CHECK ((policy_version = 'listing_avionics_authorization'::text))",
            "CHECK ((product_fingerprint ~ '^[0-9a-f]{64}$'::text))",
        ];

        match &mut *connection {
            GateConnection::Sqlite(pool) => {
                let table_definition = sqlx::query_scalar::<_, String>(
                    "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?",
                )
                .bind(TABLE)
                .fetch_optional(&mut **pool)
                .await?;
                let Some(table_definition) = table_definition else {
                    return Ok(true);
                };
                let table_is_exact =
                    canonical_sql_definition(&table_definition).replacen(
                        "createtableifnotexists",
                        "createtable",
                        1,
                    ) == canonical_sqlite_table_definition(SQLITE_SCHEMA_SQL, TABLE)
                        .expect("canonical listing authorization table must exist");
                let actual_indexes = sqlx::query_as::<_, (String, Option<String>)>(
                    "SELECT name, sql FROM sqlite_schema \
                     WHERE type = 'index' AND tbl_name = ? AND sql IS NOT NULL \
                     ORDER BY name",
                )
                .bind(TABLE)
                .fetch_all(&mut **pool)
                .await?;
                let indexes_are_exact = actual_indexes.len() == SQLITE_INDEXES.len()
                    && actual_indexes.iter().zip(SQLITE_INDEXES).all(
                        |((actual_name, actual_definition), expected_name)| {
                            actual_name == expected_name
                                && actual_definition.as_deref().is_some_and(|actual| {
                                    canonical_sqlite_schema_definition(actual)
                                        == canonical_sqlite_named_definition(
                                            SQLITE_SCHEMA_SQL,
                                            expected_name,
                                        )
                                        .expect("canonical listing authorization index must exist")
                                })
                        },
                    );
                let actual_triggers = sqlx::query_as::<_, (String, Option<String>)>(
                    "SELECT name, sql FROM sqlite_schema \
                     WHERE type = 'trigger' AND sql IS NOT NULL \
                       AND instr(lower(sql), ?) > 0 \
                     ORDER BY name",
                )
                .bind(TABLE)
                .fetch_all(&mut **pool)
                .await?;
                let triggers_are_exact = actual_triggers.len() == TRIGGERS.len()
                    && actual_triggers.iter().zip(TRIGGERS).all(
                        |((actual_name, actual_definition), (expected_name, _, _, _))| {
                            actual_name == expected_name
                                && actual_definition.as_deref().is_some_and(|actual| {
                                    canonical_sqlite_schema_definition(actual)
                                        == canonical_sqlite_named_definition(
                                            SQLITE_SCHEMA_SQL,
                                            expected_name,
                                        )
                                        .unwrap_or_else(|| {
                                            panic!(
                                                "canonical listing authorization trigger {expected_name} must exist"
                                            )
                                        })
                                })
                        },
                    );
                Ok(table_is_exact && indexes_are_exact && triggers_are_exact)
            }
            GateConnection::Postgres(pool) => {
                let table_exists = sqlx::query_scalar::<_, bool>(
                    "SELECT pg_catalog.to_regclass( \
                       'public.aircraft_sale_listing_avionics_link_authorizations' \
                     ) IS NOT NULL",
                )
                .fetch_one(&mut **pool)
                .await?;
                if !table_exists {
                    return Ok(true);
                }
                let columns_are_exact = sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH expected(name, data_type, not_null, default_expression) AS (
                      VALUES
                        ('listing_link_id', 'bigint', TRUE, NULL),
                        ('association_role', 'text', TRUE, NULL),
                        ('avionics_model_id', 'bigint', TRUE, NULL),
                        ('authorization_kind', 'text', TRUE, NULL),
                        ('observation_sha256', 'text', TRUE, NULL),
                        ('product_fingerprint', 'text', TRUE, NULL),
                        ('grounded_resolution_sha256', 'text', FALSE, NULL),
                        ('evidence_capture_sha256', 'text', TRUE, NULL),
                        ('plugin_submission_id', 'bigint', FALSE, NULL),
                        ('extracted_listing_sha256', 'text', FALSE, NULL),
                        ('collision_closure_sha256', 'text', TRUE, NULL),
                        ('source_revocation_count', 'bigint', FALSE, NULL),
                        ('policy_version', 'text', TRUE, NULL),
                        ('authorized_at', 'text', TRUE, 'CURRENT_TIMESTAMP')
                    )
                    SELECT
                      (SELECT COUNT(*) FROM pg_catalog.pg_attribute actual
                       WHERE actual.attrelid = pg_catalog.to_regclass(
                         'public.aircraft_sale_listing_avionics_link_authorizations'
                       ) AND actual.attnum > 0 AND NOT actual.attisdropped) = 14
                      AND NOT EXISTS (
                        SELECT 1 FROM expected
                        WHERE NOT EXISTS (
                          SELECT 1 FROM pg_catalog.pg_attribute actual
                          LEFT JOIN pg_catalog.pg_attrdef default_value
                            ON default_value.adrelid = actual.attrelid
                           AND default_value.adnum = actual.attnum
                          WHERE actual.attrelid = pg_catalog.to_regclass(
                            'public.aircraft_sale_listing_avionics_link_authorizations'
                          )
                            AND actual.attname = expected.name
                            AND pg_catalog.format_type(
                              actual.atttypid, actual.atttypmod
                            ) = expected.data_type
                            AND actual.attnotnull = expected.not_null
                            AND (
                              (expected.default_expression IS NULL
                                AND default_value.oid IS NULL)
                              OR pg_catalog.pg_get_expr(
                                default_value.adbin, default_value.adrelid
                              ) = expected.default_expression
                            )
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?;
                if !columns_are_exact {
                    #[cfg(test)]
                    eprintln!("listing avionics authorization PostgreSQL columns drifted");
                    return Ok(false);
                }
                let actual_checks = sqlx::query_scalar::<_, String>(
                    r#"
                    SELECT pg_catalog.pg_get_constraintdef(constraint_row.oid)
                    FROM pg_catalog.pg_constraint constraint_row
                    WHERE constraint_row.conrelid = pg_catalog.to_regclass(
                      'public.aircraft_sale_listing_avionics_link_authorizations'
                    ) AND constraint_row.contype = 'c'
                    ORDER BY pg_catalog.pg_get_constraintdef(constraint_row.oid)
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                if actual_checks.len() != POSTGRES_CHECKS.len()
                    || !actual_checks
                        .iter()
                        .zip(POSTGRES_CHECKS)
                        .all(|(actual, expected)| actual == expected)
                {
                    #[cfg(test)]
                    eprintln!(
                        "listing avionics authorization PostgreSQL checks drifted: {actual_checks:?}"
                    );
                    return Ok(false);
                }
                let relations_are_exact = sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH expected(parent_name, child_column) AS (
                      VALUES
                        ('public.aircraft_sale_listing_avionics', 'listing_link_id'),
                        ('public.avionics_models', 'avionics_model_id'),
                        ('public.plugin_submissions', 'plugin_submission_id')
                    )
                    SELECT
                      (SELECT COUNT(*) FROM pg_catalog.pg_constraint actual
                       WHERE actual.conrelid = pg_catalog.to_regclass(
                         'public.aircraft_sale_listing_avionics_link_authorizations'
                       ) AND actual.contype = 'f') = 3
                      AND NOT EXISTS (
                        SELECT 1 FROM expected
                        WHERE NOT EXISTS (
                          SELECT 1
                          FROM pg_catalog.pg_constraint actual
                          JOIN pg_catalog.pg_attribute child_attribute
                            ON child_attribute.attrelid = actual.conrelid
                           AND child_attribute.attnum = actual.conkey[1]
                          WHERE actual.conrelid = pg_catalog.to_regclass(
                            'public.aircraft_sale_listing_avionics_link_authorizations'
                          )
                            AND actual.contype = 'f'
                            AND actual.confrelid =
                                pg_catalog.to_regclass(expected.parent_name)
                            AND child_attribute.attname = expected.child_column
                            AND pg_catalog.array_length(actual.conkey, 1) = 1
                            AND actual.confupdtype = 'a'
                            AND actual.confdeltype = 'c'
                            AND actual.confmatchtype = 's'
                        )
                      )
                      AND EXISTS (
                        SELECT 1 FROM pg_catalog.pg_constraint actual
                        WHERE actual.conrelid = pg_catalog.to_regclass(
                          'public.aircraft_sale_listing_avionics_link_authorizations'
                        ) AND actual.contype = 'p'
                          AND pg_catalog.pg_get_constraintdef(actual.oid) =
                            'PRIMARY KEY (listing_link_id, association_role)'
                      )
                      AND EXISTS (
                        SELECT 1 FROM pg_catalog.pg_index actual
                        WHERE actual.indexrelid = pg_catalog.to_regclass(
                          'public.idx_listing_avionics_authorizations_model'
                        )
                          AND actual.indrelid = pg_catalog.to_regclass(
                            'public.aircraft_sale_listing_avionics_link_authorizations'
                          )
                          AND NOT actual.indisunique
                          AND actual.indnatts = 1 AND actual.indnkeyatts = 1
                          AND actual.indexprs IS NULL AND actual.indpred IS NULL
                          AND pg_catalog.pg_get_indexdef(
                            actual.indexrelid, 1, TRUE
                          ) = 'avionics_model_id'
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?;
                if !relations_are_exact {
                    #[cfg(test)]
                    eprintln!("listing avionics authorization PostgreSQL relations drifted");
                    return Ok(false);
                }
                let actual_triggers =
                    sqlx::query_as::<_, PostgresGroundedCapabilityTriggerDefinition>(
                        r#"
                    SELECT trigger_row.tgname AS trigger_name,
                           trigger_row.tgtype AS trigger_type,
                           trigger_row.tgqual IS NULL AS has_no_when_clause,
                           trigger_row.tgenabled = 'O' AS is_enabled,
                           trigger_row.tgnargs = 0 AS has_zero_trigger_args,
                           trigger_row.tgparentid = 0 AS is_uninherited,
                           trigger_row.tgconstraint = 0 AS is_non_constraint,
                           trigger_row.tgoldtable IS NULL
                             AND trigger_row.tgnewtable IS NULL
                             AS has_no_transition_tables,
                           trigger_row.tgattr::text = '' AS has_no_update_columns,
                           pg_catalog.pg_get_triggerdef(trigger_row.oid) AS definition,
                           routine_namespace.nspname AS function_namespace,
                           routine.proname AS function_name,
                           routine.prosrc AS function_source,
                           COALESCE(
                             pg_catalog.array_to_string(routine.proconfig, E'\n'), ''
                           ) AS function_config,
                           NOT routine.prosecdef AS is_security_invoker,
                           routine.prokind = 'f' AS is_ordinary_function,
                           routine.prorettype = pg_catalog.to_regtype(
                             'pg_catalog.trigger'
                           ) AS returns_trigger,
                           routine.pronargs = 0 AS has_zero_function_args,
                           language_row.lanname AS language_name,
                           NOT routine.proisstrict AS is_not_strict,
                           routine.provolatile = 'v' AS is_volatile,
                           routine.proparallel = 'u' AS is_parallel_unsafe,
                           NOT routine.proleakproof AS is_not_leakproof
                    FROM pg_catalog.pg_trigger trigger_row
                    JOIN pg_catalog.pg_proc routine ON routine.oid = trigger_row.tgfoid
                    JOIN pg_catalog.pg_namespace routine_namespace
                      ON routine_namespace.oid = routine.pronamespace
                    JOIN pg_catalog.pg_language language_row
                      ON language_row.oid = routine.prolang
                    WHERE NOT trigger_row.tgisinternal
                      AND (
                        trigger_row.tgrelid = pg_catalog.to_regclass(
                          'public.aircraft_sale_listing_avionics_link_authorizations'
                        )
                        OR pg_catalog.strpos(
                          routine.prosrc,
                          'aircraft_sale_listing_avionics_link_authorizations'
                        ) > 0
                      )
                    ORDER BY trigger_row.tgname
                    "#,
                    )
                    .fetch_all(&mut **pool)
                    .await?;
                let triggers_are_exact = actual_triggers.len() == TRIGGERS.len()
                    && actual_triggers.iter().zip(TRIGGERS).all(
                        |(actual, (name, trigger_type, no_update_columns, function_name))| {
                            actual.trigger_name == *name
                                && actual.trigger_type == *trigger_type
                                && actual.has_no_when_clause
                                && actual.is_enabled
                                && actual.has_zero_trigger_args
                                && actual.is_uninherited
                                && actual.is_non_constraint
                                && actual.has_no_transition_tables
                                && actual.has_no_update_columns == *no_update_columns
                                && actual.function_namespace == "public"
                                && actual.function_name == *function_name
                                && actual.function_config == "search_path=pg_catalog"
                                && actual.is_security_invoker
                                && actual.is_ordinary_function
                                && actual.returns_trigger
                                && actual.has_zero_function_args
                                && actual.language_name == "plpgsql"
                                && actual.is_not_strict
                                && actual.is_volatile
                                && actual.is_parallel_unsafe
                                && actual.is_not_leakproof
                                && canonical_postgres_trigger_definition(&actual.definition)
                                    == canonical_postgres_named_trigger_definition(
                                        POSTGRES_SCHEMA_SQL,
                                        name,
                                    )
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "canonical listing authorization trigger {name} must exist"
                                        )
                                    })
                                && canonical_sql_definition(&actual.function_source)
                                    == canonical_sql_definition(
                                        postgres_function_source(
                                            POSTGRES_SCHEMA_SQL,
                                            function_name,
                                        )
                                        .unwrap_or_else(|| {
                                            panic!(
                                                "canonical listing authorization function {function_name} must exist"
                                            )
                                        }),
                                    )
                        },
                    );
                #[cfg(test)]
                if !triggers_are_exact {
                    eprintln!(
                        "listing avionics authorization PostgreSQL triggers drifted: actual={:?} expected={:?}",
                        actual_triggers
                            .iter()
                            .map(|trigger| (&trigger.trigger_name, trigger.trigger_type, trigger.has_no_update_columns, &trigger.function_name))
                            .collect::<Vec<_>>(),
                        TRIGGERS
                    );
                }
                Ok(triggers_are_exact)
            }
        }
    }

    async fn aircraft_visual_source_correction_definitions_valid_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<bool> {
        match &mut *connection {
            GateConnection::Sqlite(pool) => {
                let table_definition = sqlx::query_scalar::<_, Option<String>>(
                    r#"
                    SELECT sql FROM sqlite_schema
                    WHERE type = 'table'
                      AND name = 'aircraft_source_visual_correction_artifacts'
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?;
                let table_is_exact = table_definition.as_deref().is_some_and(|actual| {
                    canonical_sql_definition(actual).replacen(
                        "createtableifnotexists",
                        "createtable",
                        1,
                    ) == canonical_sqlite_table_definition(
                        SQLITE_SCHEMA_SQL,
                        "aircraft_source_visual_correction_artifacts",
                    )
                    .expect("canonical visual source correction artifact table must exist")
                });
                let definitions = sqlx::query_as::<_, (String, Option<String>)>(
                    r#"
                    SELECT name, sql FROM sqlite_schema
                    WHERE type = 'trigger' AND (
                      tbl_name = 'aircraft_source_visual_correction_artifacts'
                      OR name = 'aircraft_source_identity_receipt_gate'
                    )
                    ORDER BY name
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                let expected = [
                    (
                        "aircraft_source_identity_receipt_gate",
                        SQLITE_SOURCE_IDENTITY_RECEIPT_GATE_TRIGGER,
                    ),
                    (
                        "aircraft_source_visual_artifacts_immutable_delete",
                        SQLITE_SOURCE_VISUAL_ARTIFACT_DELETE_TRIGGER,
                    ),
                    (
                        "aircraft_source_visual_artifacts_immutable_update",
                        SQLITE_SOURCE_VISUAL_ARTIFACT_UPDATE_TRIGGER,
                    ),
                    (
                        "aircraft_source_visual_artifacts_validate_insert",
                        SQLITE_SOURCE_VISUAL_ARTIFACT_INSERT_TRIGGER,
                    ),
                ];
                let valid = table_is_exact
                    && definitions.len() == expected.len()
                    && definitions.iter().zip(expected).all(
                        |((actual_name, actual_sql), (expected_name, expected_sql))| {
                            actual_name == expected_name
                                && actual_sql.as_deref().is_some_and(|actual| {
                                    canonical_sql_definition(actual)
                                        == canonical_sql_definition(expected_sql)
                                })
                        },
                    );
                Ok(valid)
            }
            GateConnection::Postgres(pool) => {
                let table_is_exact = sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH expected_columns (
                      ordinal_position, column_name, column_type, is_not_null,
                      identity_kind, default_expression
                    ) AS (
                      VALUES
                        (1, 'plugin_submission_id', 'bigint', TRUE, '', ''),
                        (2, 'rendered_html_sha256', 'text', TRUE, '', ''),
                        (3, 'observed_registration_number', 'text', TRUE, '', ''),
                        (4, 'corrected_registration_number', 'text', TRUE, '', ''),
                        (5, 'corrected_serial_number', 'text', FALSE, '', ''),
                        (6, 'faa_registry_snapshot_id', 'bigint', TRUE, '', ''),
                        (7, 'faa_snapshot_archive_sha256', 'text', TRUE, '', ''),
                        (8, 'faa_source_record_sha256', 'text', TRUE, '', ''),
                        (9, 'primary_photo_asset_id', 'text', TRUE, '', ''),
                        (10, 'primary_photo_url', 'text', TRUE, '', ''),
                        (11, 'primary_photo_sha256', 'text', TRUE, '', ''),
                        (12, 'visual_resolution_sha256', 'text', TRUE, '', ''),
                        (13, 'visual_resolution_json', 'text', TRUE, '', ''),
                        (14, 'created_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP')
                    ), actual_columns AS (
                      SELECT attribute.attnum::integer,
                             attribute.attname::text,
                             pg_catalog.format_type(
                               attribute.atttypid, attribute.atttypmod
                             ),
                             attribute.attnotnull,
                             attribute.attidentity::text,
                             COALESCE(pg_catalog.pg_get_expr(
                               attribute_default.adbin,
                               attribute_default.adrelid
                             ), '')
                      FROM pg_catalog.pg_class relation
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      JOIN pg_catalog.pg_attribute attribute
                        ON attribute.attrelid = relation.oid
                       AND attribute.attnum > 0
                       AND NOT attribute.attisdropped
                      LEFT JOIN pg_catalog.pg_attrdef attribute_default
                        ON attribute_default.adrelid = relation.oid
                       AND attribute_default.adnum = attribute.attnum
                      WHERE namespace.nspname = 'public'
                        AND relation.relname =
                          'aircraft_source_visual_correction_artifacts'
                        AND relation.relkind = 'r'
                        AND relation.relpersistence = 'p'
                        AND NOT relation.relrowsecurity
                        AND NOT relation.relforcerowsecurity
                        AND NOT relation.relispartition
                    ), expected_constraints (constraint_name, constraint_type) AS (
                      VALUES
                        ('aircraft_source_visual_correction_artifacts_pkey', 'p'),
                        ('visual_source_artifact_submission_fk', 'f'),
                        ('visual_source_artifact_snapshot_fk', 'f'),
                        ('visual_source_artifact_observed_coverage_fk', 'f'),
                        ('visual_source_artifact_corrected_aircraft_fk', 'f'),
                        ('visual_source_artifact_record_aircraft_fk', 'f'),
                        ('visual_source_artifact_rendered_sha256_check', 'c'),
                        ('visual_source_artifact_archive_sha256_check', 'c'),
                        ('visual_source_artifact_record_sha256_check', 'c'),
                        ('visual_source_artifact_photo_sha256_check', 'c'),
                        ('visual_source_artifact_resolution_sha256_check', 'c'),
                        ('visual_source_artifact_registration_distinct_check', 'c'),
                        ('visual_source_artifact_observed_registration_length_check', 'c'),
                        ('visual_source_artifact_corrected_registration_length_check', 'c'),
                        ('visual_source_artifact_corrected_serial_length_check', 'c'),
                        ('visual_source_artifact_asset_id_length_check', 'c'),
                        ('visual_source_artifact_url_length_check', 'c'),
                        ('visual_source_artifact_resolution_json_check', 'c')
                    ), actual_constraints AS (
                      SELECT constraint_row.conname::text,
                             constraint_row.contype::text
                      FROM pg_catalog.pg_constraint constraint_row
                      WHERE constraint_row.conrelid = pg_catalog.to_regclass(
                        'public.aircraft_source_visual_correction_artifacts'
                      )
                    ), expected_foreign_keys (
                      constraint_name, child_columns, parent_relation, parent_columns
                    ) AS (
                      VALUES
                        ('visual_source_artifact_submission_fk',
                         'plugin_submission_id', 'plugin_submissions', 'id'),
                        ('visual_source_artifact_snapshot_fk',
                         'faa_registry_snapshot_id', 'faa_registry_snapshots', 'id'),
                        ('visual_source_artifact_observed_coverage_fk',
                         'faa_registry_snapshot_id,observed_registration_number',
                         'faa_registry_coverage', 'snapshot_id,n_number'),
                        ('visual_source_artifact_corrected_aircraft_fk',
                         'faa_registry_snapshot_id,corrected_registration_number',
                         'faa_registry_aircraft', 'snapshot_id,n_number'),
                        ('visual_source_artifact_record_aircraft_fk',
                         'faa_registry_snapshot_id,faa_source_record_sha256',
                         'faa_registry_aircraft', 'snapshot_id,source_record_sha256')
                    ), actual_foreign_keys AS (
                      SELECT constraint_row.conname::text,
                             (
                               SELECT pg_catalog.string_agg(
                                 child_attribute.attname, ',' ORDER BY key.ordinality
                               )
                               FROM pg_catalog.unnest(constraint_row.conkey)
                                 WITH ORDINALITY AS key(attnum, ordinality)
                               JOIN pg_catalog.pg_attribute child_attribute
                                 ON child_attribute.attrelid = constraint_row.conrelid
                                AND child_attribute.attnum = key.attnum
                             ),
                             parent_relation.relname::text,
                             (
                               SELECT pg_catalog.string_agg(
                                 parent_attribute.attname, ',' ORDER BY key.ordinality
                               )
                               FROM pg_catalog.unnest(constraint_row.confkey)
                                 WITH ORDINALITY AS key(attnum, ordinality)
                               JOIN pg_catalog.pg_attribute parent_attribute
                                 ON parent_attribute.attrelid = constraint_row.confrelid
                                AND parent_attribute.attnum = key.attnum
                             )
                      FROM pg_catalog.pg_constraint constraint_row
                      JOIN pg_catalog.pg_class parent_relation
                        ON parent_relation.oid = constraint_row.confrelid
                      JOIN pg_catalog.pg_namespace parent_namespace
                        ON parent_namespace.oid = parent_relation.relnamespace
                      WHERE constraint_row.conrelid = pg_catalog.to_regclass(
                              'public.aircraft_source_visual_correction_artifacts'
                            )
                        AND constraint_row.contype = 'f'
                        AND constraint_row.convalidated
                        AND NOT constraint_row.condeferrable
                        AND NOT constraint_row.condeferred
                        AND constraint_row.confmatchtype = 's'
                        AND constraint_row.confupdtype = 'a'
                        AND constraint_row.confdeltype = 'r'
                        AND parent_namespace.nspname = 'public'
                    ), actual_checks AS (
                      SELECT constraint_row.conname::text AS constraint_name,
                             pg_catalog.regexp_replace(
                               pg_catalog.lower(pg_catalog.pg_get_constraintdef(
                                 constraint_row.oid, FALSE
                               )), '[[:space:]()]', '', 'g'
                             ) AS definition
                      FROM pg_catalog.pg_constraint constraint_row
                      WHERE constraint_row.conrelid = pg_catalog.to_regclass(
                              'public.aircraft_source_visual_correction_artifacts'
                            )
                        AND constraint_row.contype = 'c'
                        AND constraint_row.convalidated
                        AND NOT constraint_row.connoinherit
                    ), expected_checks (constraint_name, definition) AS (
                      VALUES
                        ('visual_source_artifact_rendered_sha256_check',
                         'checkrendered_html_sha256~''^[0-9a-f]{64}$''::text'),
                        ('visual_source_artifact_archive_sha256_check',
                         'checkfaa_snapshot_archive_sha256~''^[0-9a-f]{64}$''::text'),
                        ('visual_source_artifact_record_sha256_check',
                         'checkfaa_source_record_sha256~''^[0-9a-f]{64}$''::text'),
                        ('visual_source_artifact_photo_sha256_check',
                         'checkprimary_photo_sha256~''^[0-9a-f]{64}$''::text'),
                        ('visual_source_artifact_resolution_sha256_check',
                         'checkvisual_resolution_sha256~''^[0-9a-f]{64}$''::text'),
                        ('visual_source_artifact_registration_distinct_check',
                         'checkobserved_registration_number<>corrected_registration_number'),
                        ('visual_source_artifact_observed_registration_length_check',
                         'checklengthobserved_registration_number>=2andlengthobserved_registration_number<=6'),
                        ('visual_source_artifact_corrected_registration_length_check',
                         'checklengthcorrected_registration_number>=2andlengthcorrected_registration_number<=6'),
                        ('visual_source_artifact_corrected_serial_length_check',
                         'checkcorrected_serial_numberisnullorlengthcorrected_serial_number>=1andlengthcorrected_serial_number<=128'),
                        ('visual_source_artifact_asset_id_length_check',
                         'checklengthprimary_photo_asset_id>=1andlengthprimary_photo_asset_id<=256'),
                        ('visual_source_artifact_url_length_check',
                         'checklengthprimary_photo_url>=1andlengthprimary_photo_url<=4096'),
                        ('visual_source_artifact_resolution_json_check',
                         'checklengthvisual_resolution_json>=2andlengthvisual_resolution_json<=65536andjsonb_typeofvisual_resolution_json::jsonb=''object''::text')
                    )
                    SELECT
                      NOT EXISTS (
                        (SELECT * FROM expected_columns
                         EXCEPT SELECT * FROM actual_columns)
                        UNION ALL
                        (SELECT * FROM actual_columns
                         EXCEPT SELECT * FROM expected_columns)
                      )
                      AND NOT EXISTS (
                        (SELECT * FROM expected_constraints
                         EXCEPT SELECT * FROM actual_constraints)
                        UNION ALL
                        (SELECT * FROM actual_constraints
                         EXCEPT SELECT * FROM expected_constraints)
                      )
                      AND NOT EXISTS (
                        (SELECT * FROM expected_foreign_keys
                         EXCEPT SELECT * FROM actual_foreign_keys)
                        UNION ALL
                        (SELECT * FROM actual_foreign_keys
                         EXCEPT SELECT * FROM expected_foreign_keys)
                      )
                      AND NOT EXISTS (
                        (SELECT * FROM expected_checks
                         EXCEPT SELECT * FROM actual_checks)
                        UNION ALL
                        (SELECT * FROM actual_checks
                         EXCEPT SELECT * FROM expected_checks)
                      )
                      AND EXISTS (
                        SELECT 1
                        FROM pg_catalog.pg_constraint primary_key
                        JOIN pg_catalog.pg_index backing_index
                          ON backing_index.indexrelid = primary_key.conindid
                        WHERE primary_key.conrelid = pg_catalog.to_regclass(
                                'public.aircraft_source_visual_correction_artifacts'
                              )
                          AND primary_key.conname =
                            'aircraft_source_visual_correction_artifacts_pkey'
                          AND primary_key.contype = 'p'
                          AND primary_key.convalidated
                          AND NOT primary_key.condeferrable
                          AND NOT primary_key.condeferred
                          AND primary_key.conkey = ARRAY[(
                            SELECT attribute.attnum
                            FROM pg_catalog.pg_attribute attribute
                            WHERE attribute.attrelid = primary_key.conrelid
                              AND attribute.attname = 'plugin_submission_id'
                          )]::smallint[]
                          AND backing_index.indisprimary
                          AND backing_index.indisunique
                          AND backing_index.indisvalid
                          AND backing_index.indisready
                          AND backing_index.indnkeyatts = 1
                          AND backing_index.indnatts = 1
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_inherits inheritance
                        WHERE inheritance.inhrelid = pg_catalog.to_regclass(
                          'public.aircraft_source_visual_correction_artifacts'
                        )
                           OR inheritance.inhparent = pg_catalog.to_regclass(
                          'public.aircraft_source_visual_correction_artifacts'
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?;
                if !table_is_exact {
                    return Ok(false);
                }
                let rows =
                    sqlx::query_as::<_, (String, i16, String, String, String, String, String)>(
                        r#"
                    SELECT trigger_row.tgname,
                           trigger_row.tgtype::smallint,
                           COALESCE((
                             SELECT string_agg(attribute.attname, ',' ORDER BY attribute.attname)
                             FROM unnest(trigger_row.tgattr) AS update_column(attnum)
                             JOIN pg_catalog.pg_attribute attribute
                               ON attribute.attrelid = trigger_row.tgrelid
                              AND attribute.attnum = update_column.attnum
                           ), ''),
                           relation.relname,
                           routine.proname,
                           routine.prosrc,
                           COALESCE(pg_catalog.array_to_string(routine.proconfig, E'\n'), '')
                    FROM pg_catalog.pg_trigger trigger_row
                    JOIN pg_catalog.pg_class relation ON relation.oid = trigger_row.tgrelid
                    JOIN pg_catalog.pg_namespace relation_namespace
                      ON relation_namespace.oid = relation.relnamespace
                    JOIN pg_catalog.pg_proc routine ON routine.oid = trigger_row.tgfoid
                    JOIN pg_catalog.pg_namespace routine_namespace
                      ON routine_namespace.oid = routine.pronamespace
                    WHERE NOT trigger_row.tgisinternal
                      AND relation_namespace.nspname = 'public'
                      AND routine_namespace.nspname = 'public'
                      AND (
                        relation.relname = 'aircraft_source_visual_correction_artifacts'
                        OR trigger_row.tgname = 'aircraft_source_identity_receipt_gate'
                      )
                    ORDER BY trigger_row.tgname
                    "#,
                    )
                    .fetch_all(&mut **pool)
                    .await?;
                let expected = [
                    (
                        "aircraft_source_identity_receipt_gate",
                        19_i16,
                        "ingestion_error,ingestion_state,is_verified",
                        "aircraft_sale_listings",
                        "require_source_identity_correction_receipt",
                        POSTGRES_SOURCE_IDENTITY_RECEIPT_GATE_FUNCTION_SOURCE,
                    ),
                    (
                        "aircraft_source_visual_artifacts_immutable",
                        27_i16,
                        "",
                        "aircraft_source_visual_correction_artifacts",
                        "preserve_aircraft_source_visual_correction_artifact",
                        POSTGRES_SOURCE_VISUAL_ARTIFACT_FUNCTION_SOURCE,
                    ),
                    (
                        "aircraft_source_visual_artifacts_validate_insert",
                        7_i16,
                        "",
                        "aircraft_source_visual_correction_artifacts",
                        "validate_aircraft_source_visual_correction_artifact",
                        POSTGRES_SOURCE_VISUAL_ARTIFACT_VALIDATION_FUNCTION_SOURCE,
                    ),
                ];
                Ok(rows.len() == expected.len()
                    && rows.iter().zip(expected).all(
                        |(
                            (name, kind, columns, relation, function, source, config),
                            (
                                expected_name,
                                expected_kind,
                                expected_columns,
                                expected_relation,
                                expected_function,
                                expected_source,
                            ),
                        )| {
                            name == expected_name
                                && *kind == expected_kind
                                && columns == expected_columns
                                && relation == expected_relation
                                && function == expected_function
                                && canonical_sql_definition(source)
                                    == canonical_sql_definition(expected_source)
                                && config == "search_path=pg_catalog"
                        },
                    ))
            }
        }
    }

    #[cfg(test)]
    async fn listing_replay_definitions_valid(&self) -> Result<bool> {
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Sqlite(&mut connection);
                self.listing_replay_definitions_valid_on(&mut connection)
                    .await
            }
            DatabaseBackend::Postgres(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Postgres(&mut connection);
                self.listing_replay_definitions_valid_on(&mut connection)
                    .await
            }
        }
    }

    async fn listing_replay_definitions_valid_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<bool> {
        match &mut *connection {
            GateConnection::Sqlite(pool) => {
                let table_definitions = sqlx::query_as::<_, (String, Option<String>)>(
                    r#"
                    SELECT name, sql FROM sqlite_schema
                    WHERE type = 'table'
                      AND name IN (
                        'listing_replay_runs', 'listing_replay_run_items',
                        'plugin_submission_materialization_receipts'
                      )
                    ORDER BY name
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                let expected_table_definitions = [
                    (
                        "listing_replay_run_items",
                        canonical_sqlite_table_definition(
                            SQLITE_SCHEMA_SQL,
                            "listing_replay_run_items",
                        )
                        .expect("canonical replay item table must exist"),
                    ),
                    (
                        "listing_replay_runs",
                        canonical_sqlite_table_definition(SQLITE_SCHEMA_SQL, "listing_replay_runs")
                            .expect("canonical replay run table must exist"),
                    ),
                    (
                        "plugin_submission_materialization_receipts",
                        canonical_sqlite_table_definition(
                            SQLITE_SCHEMA_SQL,
                            "plugin_submission_materialization_receipts",
                        )
                        .expect("canonical replay materialization receipt table must exist"),
                    ),
                ];
                let tables_are_exact = table_definitions.len() == expected_table_definitions.len()
                    && table_definitions
                        .iter()
                        .zip(expected_table_definitions)
                        .all(
                            |(
                                (actual_name, actual_definition),
                                (expected_name, expected_definition),
                            )| {
                                actual_name == expected_name
                                    && actual_definition.as_deref().is_some_and(|actual| {
                                        canonical_sql_definition(actual).replacen(
                                            "createtableifnotexists",
                                            "createtable",
                                            1,
                                        ) == expected_definition
                                    })
                            },
                        );
                let running_index = sqlx::query_as::<_, (i64, i64, Option<String>)>(
                    r#"
                    SELECT "unique", partial, (
                      SELECT sql FROM sqlite_schema
                      WHERE type = 'index'
                        AND name = 'idx_listing_replay_runs_one_running'
                        AND tbl_name = 'listing_replay_runs'
                    )
                    FROM pragma_index_list('listing_replay_runs')
                    WHERE name = 'idx_listing_replay_runs_one_running'
                    "#,
                )
                .fetch_optional(&mut **pool)
                .await?;
                let running_columns = sqlx::query_scalar::<_, String>(
                    r#"
                    SELECT group_concat(name, ',') FROM (
                      SELECT name
                      FROM pragma_index_info('idx_listing_replay_runs_one_running')
                      ORDER BY seqno
                    )
                    "#,
                )
                .fetch_optional(&mut **pool)
                .await?;
                let phase_index = sqlx::query_as::<_, (i64, i64)>(
                    r#"
                    SELECT "unique", partial
                    FROM pragma_index_list('listing_replay_run_items')
                    WHERE name = 'idx_listing_replay_run_items_phase'
                    "#,
                )
                .fetch_optional(&mut **pool)
                .await?;
                let phase_columns = sqlx::query_scalar::<_, String>(
                    r#"
                    SELECT group_concat(name, ',') FROM (
                      SELECT name
                      FROM pragma_index_info('idx_listing_replay_run_items_phase')
                      ORDER BY seqno
                    )
                    "#,
                )
                .fetch_optional(&mut **pool)
                .await?;
                let attached_objects_are_closed = sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT NOT EXISTS (
                      SELECT 1 FROM sqlite_schema
                      WHERE tbl_name IN (
                        'listing_replay_runs', 'listing_replay_run_items',
                        'plugin_submission_materialization_receipts'
                      )
                        AND (
                          (
                            type = 'trigger'
                            AND name NOT IN (
                              'listing_replay_run_items_checkpoint_exact_insert',
                              'listing_replay_run_items_checkpoint_exact_update',
                              'listing_replay_run_items_completed_immutable_update',
                              'listing_replay_run_items_completed_immutable_delete',
                              'plugin_submission_materialization_receipts_immutable_update',
                              'plugin_submission_materialization_receipts_immutable_delete'
                            )
                          )
                          OR (
                            type = 'index'
                            AND name NOT IN (
                              'idx_listing_replay_runs_one_running',
                              'idx_listing_replay_run_items_phase',
                              'sqlite_autoindex_listing_replay_runs_1',
                              'sqlite_autoindex_listing_replay_run_items_1',
                              'sqlite_autoindex_listing_replay_run_items_2',
                              'sqlite_autoindex_plugin_submission_materialization_receipts_1'
                            )
                          )
                        )
                    ) AND (
                      SELECT COUNT(*) FROM sqlite_schema
                      WHERE type = 'index' AND tbl_name IN (
                        'listing_replay_runs', 'listing_replay_run_items',
                        'plugin_submission_materialization_receipts'
                      )
                    ) = 6
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0;
                let plugin_attached_triggers_are_closed = sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT NOT EXISTS (
                      SELECT 1 FROM sqlite_schema
                      WHERE type = 'trigger'
                        AND tbl_name IN ('plugin_submissions', 'plugin_installs')
                        AND NOT (
                          (
                            tbl_name = 'plugin_submissions'
                            AND name IN (
                              'plugin_submissions_replay_checkpoint_immutable',
                              'listing_avionics_authorizations_invalidate_capture_delete',
                              'listing_avionics_authorizations_invalidate_capture_update'
                            )
                          )
                          OR (
                            tbl_name = 'plugin_installs'
                            AND name = 'plugin_installs_replay_identity_immutable'
                          )
                        )
                    )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0;
                let unique_indexes = sqlx::query_as::<_, (String, i64, String, i64)>(
                    r#"
                    SELECT name, "unique", origin, partial
                    FROM pragma_index_list('listing_replay_run_items')
                    WHERE "unique" = 1 AND origin = 'u'
                    ORDER BY name
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                let mut unique_column_sets = Vec::with_capacity(unique_indexes.len());
                for (name, unique, origin, partial) in unique_indexes {
                    if unique != 1 || origin != "u" || partial != 0 {
                        return Ok(false);
                    }
                    let sql = format!(
                        "SELECT group_concat(name, ',') FROM (SELECT name FROM pragma_index_info('{}') ORDER BY seqno)",
                        name.replace('\'', "''")
                    );
                    let columns = sqlx::query_scalar::<_, String>(&sql)
                        .fetch_optional(&mut **pool)
                        .await?;
                    if let Some(columns) = columns {
                        unique_column_sets.push(columns);
                    }
                }
                unique_column_sets.sort();

                let exact_guard_names = [
                    "listing_replay_run_items_checkpoint_exact_insert",
                    "listing_replay_run_items_checkpoint_exact_update",
                    "listing_replay_run_items_completed_immutable_delete",
                    "listing_replay_run_items_completed_immutable_update",
                    "plugin_installs_replay_identity_immutable",
                    "plugin_submission_materialization_receipts_immutable_delete",
                    "plugin_submission_materialization_receipts_immutable_update",
                    "plugin_submissions_replay_checkpoint_immutable",
                    "uq_aircraft_sale_listings_owner_source",
                ];
                let exact_guard_definitions = sqlx::query_as::<_, (String, Option<String>)>(
                    r#"
                    SELECT name, sql FROM sqlite_schema
                    WHERE name IN (
                      'listing_replay_run_items_checkpoint_exact_insert',
                      'listing_replay_run_items_checkpoint_exact_update',
                      'listing_replay_run_items_completed_immutable_delete',
                      'listing_replay_run_items_completed_immutable_update',
                      'plugin_installs_replay_identity_immutable',
                      'plugin_submission_materialization_receipts_immutable_delete',
                      'plugin_submission_materialization_receipts_immutable_update',
                      'plugin_submissions_replay_checkpoint_immutable',
                      'uq_aircraft_sale_listings_owner_source'
                    )
                    ORDER BY name
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                let exact_guards_are_canonical = exact_guard_definitions.len()
                    == exact_guard_names.len()
                    && exact_guard_definitions.iter().zip(exact_guard_names).all(
                        |((actual_name, actual_definition), expected_name)| {
                            actual_name == expected_name
                                && actual_definition.as_deref().is_some_and(|actual| {
                                    canonical_sqlite_schema_definition(actual)
                                        == canonical_sqlite_named_definition(
                                            SQLITE_SCHEMA_SQL,
                                            expected_name,
                                        )
                                        .expect("canonical replay guard must exist")
                                })
                        },
                    );

                let running_predicate_is_exact = running_index
                    .as_ref()
                    .and_then(|(_, _, definition)| definition.as_deref())
                    .map(canonical_sql_definition)
                    .and_then(|definition| {
                        definition
                            .split_once("where")
                            .map(|(_, predicate)| predicate.to_string())
                    })
                    .is_some_and(|predicate| predicate == "status='running'");
                Ok(tables_are_exact
                    && running_index
                        .as_ref()
                        .is_some_and(|(unique, partial, _)| *unique == 1 && *partial == 1)
                    && running_columns.as_deref() == Some("status")
                    && running_predicate_is_exact
                    && phase_index == Some((0, 0))
                    && phase_columns.as_deref()
                        == Some("run_id,extraction_state,materialization_state,position")
                    && attached_objects_are_closed
                    && plugin_attached_triggers_are_closed
                    && exact_guards_are_canonical
                    && unique_column_sets == ["run_id,plugin_submission_id", "run_id,position"])
            }
            GateConnection::Postgres(pool) => {
                let structural_contract = sqlx::query_scalar::<_, bool>(
                    r#"
                    WITH expected_columns(
                      relation_name, ordinal_position, column_name, column_type,
                      is_not_null, identity_kind, default_expression
                    ) AS (
                      VALUES
                        ('listing_replay_runs', 1, 'id', 'bigint', TRUE, 'd', ''),
                        ('listing_replay_runs', 2, 'manifest_version', 'bigint', TRUE, '', ''),
                        ('listing_replay_runs', 3, 'manifest_sha256', 'text', TRUE, '', ''),
                        ('listing_replay_runs', 4, 'manifest_capture_count', 'bigint', TRUE, '', ''),
                        ('listing_replay_runs', 5, 'status', 'text', TRUE, '', '''queued''::text'),
                        ('listing_replay_runs', 6, 'active_phase', 'text', FALSE, '', ''),
                        ('listing_replay_runs', 7, 'owner_token', 'text', FALSE, '', ''),
                        ('listing_replay_runs', 8, 'heartbeat_at_epoch_seconds', 'bigint', FALSE, '', ''),
                        ('listing_replay_runs', 9, 'started_at', 'text', FALSE, '', ''),
                        ('listing_replay_runs', 10, 'created_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP'),
                        ('listing_replay_runs', 11, 'updated_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP'),
                        ('listing_replay_runs', 12, 'completed_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 1, 'id', 'bigint', TRUE, 'd', ''),
                        ('listing_replay_run_items', 2, 'run_id', 'bigint', TRUE, '', ''),
                        ('listing_replay_run_items', 3, 'plugin_submission_id', 'bigint', TRUE, '', ''),
                        ('listing_replay_run_items', 4, 'position', 'bigint', TRUE, '', ''),
                        ('listing_replay_run_items', 5, 'expected_rendered_html_sha256', 'text', TRUE, '', ''),
                        ('listing_replay_run_items', 6, 'extracted_listing_sha256', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 7, 'extracted_listing_json', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 8, 'extraction_state', 'text', TRUE, '', '''queued''::text'),
                        ('listing_replay_run_items', 9, 'materialization_state', 'text', TRUE, '', '''blocked''::text'),
                        ('listing_replay_run_items', 10, 'resulting_listing_id', 'bigint', FALSE, '', ''),
                        ('listing_replay_run_items', 11, 'terminal_rejection_phase', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 12, 'terminal_rejection_stage', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 13, 'terminal_rejection_reason_code', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 14, 'last_failure_phase', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 15, 'last_failure_reason_code', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 16, 'extraction_attempt_count', 'bigint', TRUE, '', '0'),
                        ('listing_replay_run_items', 17, 'materialization_attempt_count', 'bigint', TRUE, '', '0'),
                        ('listing_replay_run_items', 18, 'extraction_started_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 19, 'extraction_completed_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 20, 'materialization_started_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 21, 'materialization_completed_at', 'text', FALSE, '', ''),
                        ('listing_replay_run_items', 22, 'created_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP'),
                        ('listing_replay_run_items', 23, 'updated_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP'),
                        ('plugin_submission_materialization_receipts', 1, 'plugin_submission_id', 'bigint', TRUE, '', ''),
                        ('plugin_submission_materialization_receipts', 2, 'aircraft_sale_listing_id', 'bigint', TRUE, '', ''),
                        ('plugin_submission_materialization_receipts', 3, 'rendered_html_sha256', 'text', TRUE, '', ''),
                        ('plugin_submission_materialization_receipts', 4, 'extracted_listing_sha256', 'text', TRUE, '', ''),
                        ('plugin_submission_materialization_receipts', 5, 'completed_at', 'text', TRUE, '', 'CURRENT_TIMESTAMP')
                    ), actual_columns AS (
                      SELECT relation.relname::text AS relation_name,
                        attribute.attnum::integer AS ordinal_position,
                        attribute.attname::text AS column_name,
                        pg_catalog.format_type(attribute.atttypid, attribute.atttypmod)
                          AS column_type,
                        attribute.attnotnull AS is_not_null,
                        attribute.attidentity::text AS identity_kind,
                        COALESCE(pg_catalog.pg_get_expr(
                          default_value.adbin, default_value.adrelid
                        ), '') AS default_expression
                      FROM pg_catalog.pg_attribute attribute
                      JOIN pg_catalog.pg_class relation
                        ON relation.oid = attribute.attrelid
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      LEFT JOIN pg_catalog.pg_attrdef default_value
                        ON default_value.adrelid = attribute.attrelid
                       AND default_value.adnum = attribute.attnum
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND attribute.attnum > 0 AND NOT attribute.attisdropped
                    ), replay_relations AS (
                      SELECT relation.relname::text AS relation_name,
                        relation.oid AS relation_oid,
                        relation.relkind::text AS relation_kind,
                        relation.relpersistence::text AS persistence,
                        relation.relrowsecurity AS row_security,
                        relation.relforcerowsecurity AS force_row_security,
                        relation.relispartition AS is_partition,
                        relation.relhasrules AS has_rules,
                        relation.relhastriggers AS has_triggers,
                        relation.relpartbound IS NOT NULL AS has_partition_bound
                      FROM pg_catalog.pg_class relation
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                    ), replay_indexes AS (
                      SELECT
                        index_relation.relname AS index_name,
                        indexed_relation.oid AS relation_oid,
                        index_definition.indisunique AS is_unique,
                        index_definition.indisprimary AS is_primary,
                        index_definition.indisexclusion AS is_exclusion,
                        index_definition.indimmediate AS is_immediate,
                        index_definition.indisclustered AS is_clustered,
                        index_definition.indisvalid AS is_valid,
                        index_definition.indisready AS is_ready,
                        index_definition.indislive AS is_live,
                        index_definition.indisreplident AS is_replica_identity,
                        index_definition.indnullsnotdistinct AS nulls_not_distinct,
                        index_definition.indpred IS NOT NULL AS is_partial,
                        index_definition.indexprs IS NOT NULL AS has_expressions,
                        index_definition.indnkeyatts AS key_attribute_count,
                        index_definition.indnatts AS total_attribute_count,
                        index_definition.indkey::text AS index_keys,
                        index_definition.indcollation::text AS index_collations,
                        index_definition.indclass::text AS index_operator_classes,
                        index_definition.indoption::text AS index_options,
                        access_method.amname::text AS access_method,
                        lower(pg_catalog.pg_get_expr(
                          index_definition.indpred,
                          index_definition.indrelid
                        )) AS predicate,
                        (
                          SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                          FROM unnest(index_definition.indkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = index_definition.indrelid
                           AND attribute.attnum = key.attnum
                        ) AS columns
                        ,(
                          SELECT array_agg(
                            CASE WHEN collation_key.collation_oid = 0 THEN '0'
                              ELSE collation_namespace.nspname::text || '.' ||
                                collation_definition.collname::text END
                            ORDER BY collation_key.ordinality
                          )
                          FROM unnest(index_definition.indcollation) WITH ORDINALITY
                            AS collation_key(collation_oid, ordinality)
                          LEFT JOIN pg_catalog.pg_collation collation_definition
                            ON collation_definition.oid = collation_key.collation_oid
                          LEFT JOIN pg_catalog.pg_namespace collation_namespace
                            ON collation_namespace.oid = collation_definition.collnamespace
                        ) AS collations,
                        (
                          SELECT array_agg(
                            operator_namespace.nspname::text || '.' ||
                              operator_class.opcname::text
                            ORDER BY operator_key.ordinality
                          )
                          FROM unnest(index_definition.indclass) WITH ORDINALITY
                            AS operator_key(operator_class_oid, ordinality)
                          JOIN pg_catalog.pg_opclass operator_class
                            ON operator_class.oid = operator_key.operator_class_oid
                          JOIN pg_catalog.pg_namespace operator_namespace
                            ON operator_namespace.oid = operator_class.opcnamespace
                        ) AS operator_classes
                      FROM pg_catalog.pg_index index_definition
                      JOIN pg_catalog.pg_class index_relation
                        ON index_relation.oid = index_definition.indexrelid
                      JOIN pg_catalog.pg_am access_method
                        ON access_method.oid = index_relation.relam
                      JOIN pg_catalog.pg_namespace index_namespace
                        ON index_namespace.oid = index_relation.relnamespace
                      JOIN pg_catalog.pg_class indexed_relation
                        ON indexed_relation.oid = index_definition.indrelid
                      WHERE index_namespace.nspname = 'public'
                        AND index_relation.relname IN (
                          'idx_listing_replay_runs_one_running',
                          'idx_listing_replay_run_items_phase',
                          'uq_aircraft_sale_listings_owner_source'
                        )
                    ), replay_attached_indexes AS (
                      SELECT index_definition.indexrelid
                      FROM pg_catalog.pg_index index_definition
                      WHERE index_definition.indrelid IN (
                        pg_catalog.to_regclass('public.listing_replay_runs'),
                        pg_catalog.to_regclass('public.listing_replay_run_items'),
                        pg_catalog.to_regclass(
                          'public.plugin_submission_materialization_receipts'
                        )
                      )
                    ), replay_unique_constraints AS (
                      SELECT relation.relname::text AS relation_name, (
                        SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                        FROM unnest(constraint_definition.conkey) WITH ORDINALITY
                          AS key(attnum, ordinality)
                        JOIN pg_catalog.pg_attribute attribute
                          ON attribute.attrelid = constraint_definition.conrelid
                         AND attribute.attnum = key.attnum
                      ) AS columns,
                        constraint_definition.convalidated AS is_validated,
                        constraint_definition.condeferrable AS is_deferrable,
                        constraint_definition.condeferred AS is_initially_deferred,
                        backing_index.indisunique AS backing_is_unique,
                        backing_index.indisprimary AS backing_is_primary,
                        backing_index.indisexclusion AS backing_is_exclusion,
                        backing_index.indimmediate AS backing_is_immediate,
                        backing_index.indisclustered AS backing_is_clustered,
                        backing_index.indisvalid AS backing_is_valid,
                        backing_index.indisready AS backing_is_ready,
                        backing_index.indislive AS backing_is_live,
                        backing_index.indisreplident AS backing_is_replica_identity,
                        backing_index.indnullsnotdistinct AS backing_nulls_not_distinct,
                        backing_index.indpred IS NOT NULL AS backing_is_partial,
                        backing_index.indexprs IS NOT NULL AS backing_has_expressions,
                        backing_index.indnkeyatts AS backing_key_attribute_count,
                        backing_index.indnatts AS backing_total_attribute_count,
                        backing_index.indkey::text AS backing_index_keys,
                        backing_index.indcollation::text AS backing_index_collations,
                        backing_index.indclass::text AS backing_index_operator_classes,
                        backing_index.indoption::text AS backing_index_options,
                        backing_access_method.amname::text AS backing_access_method,
                        (
                          SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                          FROM unnest(backing_index.indkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = constraint_definition.conrelid
                           AND attribute.attnum = key.attnum
                        ) AS backing_columns,
                        (
                          SELECT array_agg(
                            CASE WHEN collation_key.collation_oid = 0 THEN '0'
                              ELSE collation_namespace.nspname::text || '.' ||
                                collation_definition.collname::text END
                            ORDER BY collation_key.ordinality
                          )
                          FROM unnest(backing_index.indcollation) WITH ORDINALITY
                            AS collation_key(collation_oid, ordinality)
                          LEFT JOIN pg_catalog.pg_collation collation_definition
                            ON collation_definition.oid = collation_key.collation_oid
                          LEFT JOIN pg_catalog.pg_namespace collation_namespace
                            ON collation_namespace.oid = collation_definition.collnamespace
                        ) AS backing_collations,
                        (
                          SELECT array_agg(
                            operator_namespace.nspname::text || '.' ||
                              operator_class.opcname::text
                            ORDER BY operator_key.ordinality
                          )
                          FROM unnest(backing_index.indclass) WITH ORDINALITY
                            AS operator_key(operator_class_oid, ordinality)
                          JOIN pg_catalog.pg_opclass operator_class
                            ON operator_class.oid = operator_key.operator_class_oid
                          JOIN pg_catalog.pg_namespace operator_namespace
                            ON operator_namespace.oid = operator_class.opcnamespace
                        ) AS backing_operator_classes
                      FROM pg_catalog.pg_constraint constraint_definition
                      JOIN pg_catalog.pg_class relation
                        ON relation.oid = constraint_definition.conrelid
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      JOIN pg_catalog.pg_index backing_index
                        ON backing_index.indexrelid = constraint_definition.conindid
                      JOIN pg_catalog.pg_class backing_index_relation
                        ON backing_index_relation.oid = backing_index.indexrelid
                      JOIN pg_catalog.pg_am backing_access_method
                        ON backing_access_method.oid = backing_index_relation.relam
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND constraint_definition.contype = 'u'
                    ), replay_primary_keys AS (
                      SELECT relation.relname::text AS relation_name, (
                        SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                        FROM unnest(constraint_definition.conkey) WITH ORDINALITY
                          AS key(attnum, ordinality)
                        JOIN pg_catalog.pg_attribute attribute
                          ON attribute.attrelid = constraint_definition.conrelid
                         AND attribute.attnum = key.attnum
                      ) AS columns,
                        constraint_definition.convalidated AS is_validated,
                        constraint_definition.condeferrable AS is_deferrable,
                        constraint_definition.condeferred AS is_initially_deferred,
                        backing_index.indisunique AS backing_is_unique,
                        backing_index.indisprimary AS backing_is_primary,
                        backing_index.indisexclusion AS backing_is_exclusion,
                        backing_index.indimmediate AS backing_is_immediate,
                        backing_index.indisclustered AS backing_is_clustered,
                        backing_index.indisvalid AS backing_is_valid,
                        backing_index.indisready AS backing_is_ready,
                        backing_index.indislive AS backing_is_live,
                        backing_index.indisreplident AS backing_is_replica_identity,
                        backing_index.indnullsnotdistinct AS backing_nulls_not_distinct,
                        backing_index.indpred IS NOT NULL AS backing_is_partial,
                        backing_index.indexprs IS NOT NULL AS backing_has_expressions,
                        backing_index.indnkeyatts AS backing_key_attribute_count,
                        backing_index.indnatts AS backing_total_attribute_count,
                        backing_index.indkey::text AS backing_index_keys,
                        backing_index.indcollation::text AS backing_index_collations,
                        backing_index.indclass::text AS backing_index_operator_classes,
                        backing_index.indoption::text AS backing_index_options,
                        backing_access_method.amname::text AS backing_access_method,
                        (
                          SELECT array_agg(attribute.attname::text ORDER BY key.ordinality)
                          FROM unnest(backing_index.indkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = constraint_definition.conrelid
                           AND attribute.attnum = key.attnum
                        ) AS backing_columns,
                        (
                          SELECT array_agg(
                            CASE WHEN collation_key.collation_oid = 0 THEN '0'
                              ELSE collation_namespace.nspname::text || '.' ||
                                collation_definition.collname::text END
                            ORDER BY collation_key.ordinality
                          )
                          FROM unnest(backing_index.indcollation) WITH ORDINALITY
                            AS collation_key(collation_oid, ordinality)
                          LEFT JOIN pg_catalog.pg_collation collation_definition
                            ON collation_definition.oid = collation_key.collation_oid
                          LEFT JOIN pg_catalog.pg_namespace collation_namespace
                            ON collation_namespace.oid = collation_definition.collnamespace
                        ) AS backing_collations,
                        (
                          SELECT array_agg(
                            operator_namespace.nspname::text || '.' ||
                              operator_class.opcname::text
                            ORDER BY operator_key.ordinality
                          )
                          FROM unnest(backing_index.indclass) WITH ORDINALITY
                            AS operator_key(operator_class_oid, ordinality)
                          JOIN pg_catalog.pg_opclass operator_class
                            ON operator_class.oid = operator_key.operator_class_oid
                          JOIN pg_catalog.pg_namespace operator_namespace
                            ON operator_namespace.oid = operator_class.opcnamespace
                        ) AS backing_operator_classes
                      FROM pg_catalog.pg_constraint constraint_definition
                      JOIN pg_catalog.pg_class relation
                        ON relation.oid = constraint_definition.conrelid
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      JOIN pg_catalog.pg_index backing_index
                        ON backing_index.indexrelid = constraint_definition.conindid
                      JOIN pg_catalog.pg_class backing_index_relation
                        ON backing_index_relation.oid = backing_index.indexrelid
                      JOIN pg_catalog.pg_am backing_access_method
                        ON backing_access_method.oid = backing_index_relation.relam
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND constraint_definition.contype = 'p'
                    ), replay_foreign_keys AS (
                      SELECT child_namespace.nspname::text AS child_namespace,
                        child.relname::text AS child_relation,
                        child.oid AS child_oid,
                        parent_namespace.nspname::text AS parent_namespace,
                        parent.relname::text AS parent_relation,
                        parent.oid AS parent_oid,
                        (
                          SELECT string_agg(attribute.attname::text, ',' ORDER BY key.ordinality)
                          FROM unnest(constraint_definition.conkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = constraint_definition.conrelid
                           AND attribute.attnum = key.attnum
                        ) AS child_columns,
                        (
                          SELECT string_agg(attribute.attname::text, ',' ORDER BY key.ordinality)
                          FROM unnest(constraint_definition.confkey) WITH ORDINALITY
                            AS key(attnum, ordinality)
                          JOIN pg_catalog.pg_attribute attribute
                            ON attribute.attrelid = constraint_definition.confrelid
                           AND attribute.attnum = key.attnum
                        ) AS parent_columns,
                        constraint_definition.convalidated AS is_validated,
                        constraint_definition.condeferrable AS is_deferrable,
                        constraint_definition.condeferred AS is_initially_deferred,
                        constraint_definition.confmatchtype::text AS match_type,
                        constraint_definition.confupdtype::text AS update_action,
                        constraint_definition.confdeltype::text AS delete_action
                      FROM pg_catalog.pg_constraint constraint_definition
                      JOIN pg_catalog.pg_class child
                        ON child.oid = constraint_definition.conrelid
                      JOIN pg_catalog.pg_namespace child_namespace
                        ON child_namespace.oid = child.relnamespace
                      JOIN pg_catalog.pg_class parent
                        ON parent.oid = constraint_definition.confrelid
                      JOIN pg_catalog.pg_namespace parent_namespace
                        ON parent_namespace.oid = parent.relnamespace
                      WHERE child_namespace.nspname = 'public'
                        AND child.relname IN (
                          'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND constraint_definition.contype = 'f'
                    ), required_check_fragments(relation_name, fragment) AS (
                      VALUES
                        ('listing_replay_runs', 'manifest_version > 0'),
                        ('listing_replay_runs', '^[0-9a-f]{64}$'),
                        ('listing_replay_runs', 'manifest_capture_count > 0'),
                        ('listing_replay_runs', 'status = ANY'),
                        ('listing_replay_runs', 'active_phase = ANY'),
                        ('listing_replay_runs', 'length(btrim(owner_token))'),
                        ('listing_replay_runs', 'heartbeat_at_epoch_seconds IS NOT NULL'),
                        ('listing_replay_run_items', '"position" >= 0'),
                        ('listing_replay_run_items', 'expected_rendered_html_sha256'),
                        ('listing_replay_run_items', 'extracted_listing_sha256'),
                        ('listing_replay_run_items', 'extracted_listing_json IS NOT NULL'),
                        ('listing_replay_run_items', 'extraction_state = ANY'),
                        ('listing_replay_run_items', 'materialization_state = ANY'),
                        ('listing_replay_run_items', 'terminal_rejection_phase = ANY'),
                        ('listing_replay_run_items', 'faa_aircraft_admission'),
                        ('listing_replay_run_items', 'capture_authentication_failed'),
                        ('listing_replay_run_items', 'last_failure_phase = ANY'),
                        ('listing_replay_run_items', 'faa_lookup_failed'),
                        ('listing_replay_run_items', 'extraction_attempt_count >= 0'),
                        ('listing_replay_run_items', 'materialization_attempt_count >= 0'),
                        ('listing_replay_run_items', 'terminal_rejection_phase IS NULL'),
                        ('listing_replay_run_items', 'last_failure_phase IS NULL'),
                        ('listing_replay_run_items', 'resulting_listing_id IS NOT NULL'),
                        ('listing_replay_run_items', 'extracted_listing_sha256 IS NOT NULL'),
                        ('listing_replay_run_items', 'extraction_state = ''succeeded'''),
                        ('listing_replay_run_items', 'extraction_started_at IS NOT NULL'),
                        ('listing_replay_run_items', 'materialization_started_at IS NOT NULL')
                        ,('plugin_submission_materialization_receipts', 'rendered_html_sha256')
                        ,('plugin_submission_materialization_receipts', 'extracted_listing_sha256')
                    ), replay_checks AS (
                      SELECT relation.relname::text AS relation_name,
                        pg_catalog.pg_get_constraintdef(constraint_definition.oid)
                          AS definition
                      FROM pg_catalog.pg_constraint constraint_definition
                      JOIN pg_catalog.pg_class relation
                        ON relation.oid = constraint_definition.conrelid
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      WHERE namespace.nspname = 'public'
                        AND relation.relname IN (
                          'listing_replay_runs', 'listing_replay_run_items',
                          'plugin_submission_materialization_receipts'
                        )
                        AND constraint_definition.contype = 'c'
                    )
                    SELECT
                      (SELECT COUNT(*) = 40 FROM actual_columns)
                      AND NOT EXISTS (
                        SELECT 1 FROM expected_columns expected
                        WHERE NOT EXISTS (
                          SELECT 1 FROM actual_columns actual
                          WHERE actual.relation_name = expected.relation_name
                            AND actual.ordinal_position = expected.ordinal_position
                            AND actual.column_name = expected.column_name
                            AND actual.column_type = expected.column_type
                            AND actual.is_not_null = expected.is_not_null
                            AND actual.identity_kind = expected.identity_kind
                            AND actual.default_expression = expected.default_expression
                        )
                      )
                      AND (SELECT COUNT(*) = 3 FROM replay_relations)
                      AND NOT EXISTS (
                        SELECT 1 FROM replay_relations
                        WHERE relation_oid IS DISTINCT FROM pg_catalog.to_regclass(
                          'public.' || relation_name
                        )
                          OR relation_kind <> 'r'
                          OR persistence <> 'p'
                          OR row_security OR force_row_security OR is_partition
                          OR has_rules OR NOT has_triggers OR has_partition_bound
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_trigger trigger_definition
                        WHERE trigger_definition.tgrelid IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        ) AND NOT trigger_definition.tgisinternal
                          AND trigger_definition.tgname NOT IN (
                            'listing_replay_run_items_checkpoint_exact',
                            'listing_replay_run_items_completed_immutable',
                            'plugin_submission_materialization_receipts_immutable'
                          )
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_trigger trigger_definition
                        WHERE NOT trigger_definition.tgisinternal
                          AND trigger_definition.tgrelid IN (
                            pg_catalog.to_regclass('public.plugin_submissions'),
                            pg_catalog.to_regclass('public.plugin_installs')
                          )
                          AND NOT (
                            (
                              trigger_definition.tgrelid = pg_catalog.to_regclass(
                                'public.plugin_submissions'
                              )
                              AND trigger_definition.tgname IN (
                                'plugin_submissions_replay_checkpoint_immutable',
                                'listing_avionics_authorizations_invalidate_capture_delete',
                                'listing_avionics_authorizations_invalidate_capture_update'
                              )
                            )
                            OR (
                              trigger_definition.tgrelid = pg_catalog.to_regclass(
                                'public.plugin_installs'
                              )
                              AND trigger_definition.tgname =
                                'plugin_installs_replay_identity_immutable'
                            )
                          )
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_policy policy_definition
                        WHERE policy_definition.polrelid IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        )
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_rewrite rule_definition
                        WHERE rule_definition.ev_class IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        )
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM pg_catalog.pg_inherits inheritance
                        WHERE inheritance.inhrelid IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        ) OR inheritance.inhparent IN (
                          pg_catalog.to_regclass('public.listing_replay_runs'),
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        )
                      )
                      AND (SELECT COUNT(*) = 9 FROM replay_attached_indexes)
                      AND (SELECT COUNT(*) = 3 FROM pg_catalog.pg_trigger trigger_definition
                        WHERE trigger_definition.tgrelid IN (
                          pg_catalog.to_regclass('public.listing_replay_run_items'),
                          pg_catalog.to_regclass(
                            'public.plugin_submission_materialization_receipts'
                          )
                        ) AND NOT trigger_definition.tgisinternal)
                      AND (SELECT COUNT(*) = 1 FROM replay_indexes
                       WHERE index_name = 'idx_listing_replay_runs_one_running'
                         AND relation_oid = pg_catalog.to_regclass(
                           'public.listing_replay_runs'
                         )
                         AND is_unique AND NOT is_primary AND NOT is_exclusion
                         AND is_immediate AND NOT is_clustered
                         AND is_valid AND is_ready AND is_live
                         AND NOT is_replica_identity AND NOT nulls_not_distinct
                         AND is_partial
                         AND NOT has_expressions
                         AND key_attribute_count = 1 AND total_attribute_count = 1
                         AND access_method = 'btree'
                         AND index_options = '0'
                         AND columns = ARRAY['status']::text[]
                         AND collations = ARRAY['pg_catalog.default']::text[]
                         AND operator_classes = ARRAY['pg_catalog.text_ops']::text[]
                         AND pg_catalog.translate(
                           predicate, E' \n\r\t()', ''
                         ) IN ('status=''running''', 'status=''running''::text'))
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_indexes
                       WHERE index_name = 'idx_listing_replay_run_items_phase'
                         AND relation_oid = pg_catalog.to_regclass(
                           'public.listing_replay_run_items'
                         )
                         AND NOT is_unique AND NOT is_primary AND NOT is_exclusion
                         AND is_immediate AND NOT is_clustered
                         AND is_valid AND is_ready AND is_live
                         AND NOT is_replica_identity AND NOT nulls_not_distinct
                         AND NOT is_partial
                         AND NOT has_expressions
                         AND key_attribute_count = 4 AND total_attribute_count = 4
                         AND access_method = 'btree'
                         AND index_options = '0 0 0 0'
                         AND columns = ARRAY[
                           'run_id', 'extraction_state',
                           'materialization_state', 'position'
                         ]::text[]
                         AND collations = ARRAY[
                           '0', 'pg_catalog.default', 'pg_catalog.default', '0'
                         ]::text[]
                         AND operator_classes = ARRAY[
                           'pg_catalog.int8_ops', 'pg_catalog.text_ops',
                           'pg_catalog.text_ops', 'pg_catalog.int8_ops'
                         ]::text[])
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_indexes
                       WHERE index_name = 'uq_aircraft_sale_listings_owner_source'
                         AND relation_oid = pg_catalog.to_regclass(
                           'public.aircraft_sale_listings'
                         )
                         AND is_unique AND NOT is_primary AND NOT is_exclusion
                         AND is_immediate AND NOT is_clustered
                         AND is_valid AND is_ready AND is_live
                         AND NOT is_replica_identity AND NOT nulls_not_distinct
                         AND is_partial AND NOT has_expressions
                         AND key_attribute_count = 2 AND total_attribute_count = 2
                         AND access_method = 'btree' AND index_options = '0 0'
                         AND columns = ARRAY['created_by_user_id', 'source_url']::text[]
                         AND collations = ARRAY['0', 'pg_catalog.default']::text[]
                         AND operator_classes = ARRAY[
                           'pg_catalog.int8_ops', 'pg_catalog.text_ops'
                         ]::text[]
                         AND pg_catalog.translate(predicate, E' \n\r\t()', '') IN (
                           'source_urlisnotnullandlengthbtrimsource_url>0',
                           'source_urlisnotnullandlengthbtrimsource_url>0::integer'
                         ))
                      AND (SELECT COUNT(*) = 5
                        FROM pg_catalog.pg_trigger replay_trigger
                        JOIN pg_catalog.pg_proc routine
                          ON routine.oid = replay_trigger.tgfoid
                        JOIN pg_catalog.pg_namespace routine_namespace
                          ON routine_namespace.oid = routine.pronamespace
                        WHERE NOT replay_trigger.tgisinternal
                          AND replay_trigger.tgenabled = 'O'
                          AND replay_trigger.tgqual IS NULL
                          AND replay_trigger.tgnargs = 0
                          AND routine_namespace.nspname = 'public'
                          AND routine.proconfig = ARRAY['search_path=pg_catalog']::text[]
                          AND NOT routine.prosecdef AND NOT routine.proisstrict
                          AND NOT routine.proleakproof
                          AND routine.provolatile = 'v'
                          AND routine.proparallel = 'u'
                          AND routine.prokind = 'f'
                          AND routine.pronargs = 0
                          AND routine.prorettype = pg_catalog.to_regtype(
                            'pg_catalog.trigger'
                          )
                          AND routine.prolang = (
                            SELECT language.oid FROM pg_catalog.pg_language language
                            WHERE language.lanname = 'plpgsql'
                          )
                          AND (
                            (replay_trigger.tgname =
                               'listing_replay_run_items_checkpoint_exact'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.listing_replay_run_items'
                             ) AND replay_trigger.tgtype = 23
                             AND routine.proname =
                               'enforce_replay_extraction_checkpoint_exactness')
                            OR
                            (replay_trigger.tgname =
                               'listing_replay_run_items_completed_immutable'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.listing_replay_run_items'
                             ) AND replay_trigger.tgtype = 27
                             AND routine.proname = 'preserve_completed_replay_item')
                            OR
                            (replay_trigger.tgname =
                               'plugin_submission_materialization_receipts_immutable'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.plugin_submission_materialization_receipts'
                             ) AND replay_trigger.tgtype = 27
                             AND routine.proname =
                               'preserve_replay_materialization_receipt')
                            OR
                            (replay_trigger.tgname =
                               'plugin_submissions_replay_checkpoint_immutable'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.plugin_submissions'
                             ) AND replay_trigger.tgtype = 19
                             AND routine.proname =
                               'enforce_replay_checkpoint_capture_immutability')
                            OR
                            (replay_trigger.tgname =
                               'plugin_installs_replay_identity_immutable'
                             AND replay_trigger.tgrelid = pg_catalog.to_regclass(
                               'public.plugin_installs'
                             ) AND replay_trigger.tgtype = 19
                             AND routine.proname =
                               'enforce_replay_plugin_identity_immutability')
                          ))
                      AND
                      (SELECT COUNT(*) = 4 FROM replay_unique_constraints)
                      AND NOT EXISTS (
                        SELECT 1 FROM replay_unique_constraints
                        WHERE NOT is_validated OR is_deferrable OR is_initially_deferred
                          OR NOT backing_is_unique OR backing_is_primary
                          OR backing_is_exclusion OR NOT backing_is_immediate
                          OR backing_is_clustered
                          OR NOT backing_is_valid OR NOT backing_is_ready
                          OR NOT backing_is_live OR backing_is_replica_identity
                          OR backing_nulls_not_distinct
                          OR backing_is_partial OR backing_has_expressions
                          OR backing_access_method <> 'btree'
                          OR backing_key_attribute_count <> cardinality(columns)
                          OR backing_total_attribute_count <> cardinality(columns)
                          OR backing_columns <> columns
                          OR backing_index_options <>
                            array_to_string(array_fill(
                              '0'::text, ARRAY[cardinality(columns)]
                            ), ' ')
                          OR backing_collations <> CASE
                            WHEN columns = ARRAY['manifest_sha256']::text[]
                              THEN ARRAY['pg_catalog.default']::text[]
                            ELSE array_fill(
                              '0'::text, ARRAY[cardinality(columns)]
                            ) END
                          OR backing_operator_classes <> CASE
                            WHEN columns = ARRAY['manifest_sha256']::text[]
                              THEN ARRAY['pg_catalog.text_ops']::text[]
                            ELSE array_fill(
                              'pg_catalog.int8_ops'::text,
                              ARRAY[cardinality(columns)]
                            ) END
                      )
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_unique_constraints
                       WHERE relation_name = 'listing_replay_runs'
                         AND columns = ARRAY['manifest_sha256']::text[])
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_unique_constraints
                       WHERE relation_name = 'listing_replay_run_items'
                         AND columns = ARRAY['run_id', 'position']::text[])
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_unique_constraints
                       WHERE relation_name = 'listing_replay_run_items'
                         AND columns = ARRAY[
                         'run_id', 'plugin_submission_id'
                       ]::text[])
                      AND
                      (SELECT COUNT(*) = 1 FROM replay_unique_constraints
                       WHERE relation_name = 'plugin_submission_materialization_receipts'
                         AND columns = ARRAY['aircraft_sale_listing_id']::text[])
                      AND (SELECT COUNT(*) = 3 FROM replay_primary_keys)
                      AND NOT EXISTS (
                        SELECT 1 FROM replay_primary_keys
                        WHERE NOT is_validated OR is_deferrable OR is_initially_deferred
                          OR NOT backing_is_unique OR NOT backing_is_primary
                          OR backing_is_exclusion OR NOT backing_is_immediate
                          OR backing_is_clustered
                          OR NOT backing_is_valid OR NOT backing_is_ready
                          OR NOT backing_is_live OR backing_is_replica_identity
                          OR backing_nulls_not_distinct
                          OR backing_is_partial OR backing_has_expressions
                          OR backing_access_method <> 'btree'
                          OR backing_key_attribute_count <> cardinality(columns)
                          OR backing_total_attribute_count <> cardinality(columns)
                          OR backing_columns <> columns
                          OR backing_index_options <>
                            array_to_string(array_fill(
                              '0'::text, ARRAY[cardinality(columns)]
                            ), ' ')
                          OR backing_collations <>
                            array_fill('0'::text, ARRAY[cardinality(columns)])
                          OR backing_operator_classes <>
                            array_fill(
                              'pg_catalog.int8_ops'::text,
                              ARRAY[cardinality(columns)]
                            )
                      )
                      AND (SELECT COUNT(*) = 1 FROM replay_primary_keys
                           WHERE relation_name = 'listing_replay_runs'
                             AND columns = ARRAY['id']::text[])
                      AND (SELECT COUNT(*) = 1 FROM replay_primary_keys
                           WHERE relation_name = 'listing_replay_run_items'
                             AND columns = ARRAY['id']::text[])
                      AND (SELECT COUNT(*) = 1 FROM replay_primary_keys
                           WHERE relation_name = 'plugin_submission_materialization_receipts'
                             AND columns = ARRAY['plugin_submission_id']::text[])
                      AND (SELECT COUNT(*) = 5 FROM replay_foreign_keys)
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'listing_replay_run_items'
                          AND child_oid = pg_catalog.to_regclass('public.listing_replay_run_items')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'listing_replay_runs'
                          AND parent_oid = pg_catalog.to_regclass('public.listing_replay_runs')
                          AND child_columns = 'run_id' AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'c')
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'listing_replay_run_items'
                          AND child_oid = pg_catalog.to_regclass('public.listing_replay_run_items')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'plugin_submissions'
                          AND parent_oid = pg_catalog.to_regclass('public.plugin_submissions')
                          AND child_columns = 'plugin_submission_id'
                          AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'r')
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'listing_replay_run_items'
                          AND child_oid = pg_catalog.to_regclass('public.listing_replay_run_items')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'aircraft_sale_listings'
                          AND parent_oid = pg_catalog.to_regclass('public.aircraft_sale_listings')
                          AND child_columns = 'resulting_listing_id'
                          AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'r')
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'plugin_submission_materialization_receipts'
                          AND child_oid = pg_catalog.to_regclass('public.plugin_submission_materialization_receipts')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'plugin_submissions'
                          AND parent_oid = pg_catalog.to_regclass('public.plugin_submissions')
                          AND child_columns = 'plugin_submission_id' AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'c')
                      AND EXISTS (SELECT 1 FROM replay_foreign_keys
                        WHERE child_namespace = 'public'
                          AND child_relation = 'plugin_submission_materialization_receipts'
                          AND child_oid = pg_catalog.to_regclass('public.plugin_submission_materialization_receipts')
                          AND parent_namespace = 'public'
                          AND parent_relation = 'aircraft_sale_listings'
                          AND parent_oid = pg_catalog.to_regclass('public.aircraft_sale_listings')
                          AND child_columns = 'aircraft_sale_listing_id' AND parent_columns = 'id'
                          AND is_validated AND NOT is_deferrable AND NOT is_initially_deferred
                          AND match_type = 's' AND update_action = 'a' AND delete_action = 'r')
                      AND (SELECT COUNT(*) = 7 FROM replay_checks
                           WHERE relation_name = 'listing_replay_runs')
                      AND (SELECT COUNT(*) = 20 FROM replay_checks
                           WHERE relation_name = 'listing_replay_run_items')
                      AND (SELECT COUNT(*) = 2 FROM replay_checks
                           WHERE relation_name = 'plugin_submission_materialization_receipts')
                      AND NOT EXISTS (
                        SELECT 1 FROM required_check_fragments required
                        WHERE NOT EXISTS (
                          SELECT 1 FROM replay_checks actual
                          WHERE actual.relation_name = required.relation_name
                            AND position(required.fragment IN actual.definition) > 0
                        )
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?;
                if !structural_contract {
                    return Ok(false);
                }
                let function_fingerprint = sqlx::query_scalar::<_, Option<String>>(
                    r#"
                    SELECT pg_catalog.md5(pg_catalog.string_agg(
                      routine.proname::text || '|' || routine.prosrc || E'\n',
                      '' ORDER BY routine.proname
                    ))
                    FROM pg_catalog.pg_proc routine
                    JOIN pg_catalog.pg_namespace namespace
                      ON namespace.oid = routine.pronamespace
                    WHERE namespace.nspname = 'public'
                      AND routine.pronargs = 0
                      AND routine.proname IN (
                        'enforce_replay_extraction_checkpoint_exactness',
                        'preserve_completed_replay_item',
                        'preserve_replay_materialization_receipt',
                        'enforce_replay_checkpoint_capture_immutability',
                        'enforce_replay_plugin_identity_immutability'
                      )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?;
                if function_fingerprint.as_deref()
                    != Some(POSTGRES_LISTING_REPLAY_FUNCTIONS_FINGERPRINT)
                {
                    return Ok(false);
                }
                let checks = sqlx::query_as::<_, (String, String, String)>(
                    r#"
                    SELECT relation.relname::text, constraint_definition.conname::text,
                      pg_catalog.pg_get_constraintdef(constraint_definition.oid)
                    FROM pg_catalog.pg_constraint constraint_definition
                    JOIN pg_catalog.pg_class relation
                      ON relation.oid = constraint_definition.conrelid
                    JOIN pg_catalog.pg_namespace namespace
                      ON namespace.oid = relation.relnamespace
                    WHERE namespace.nspname = 'public'
                      AND relation.relname IN (
                        'listing_replay_runs', 'listing_replay_run_items',
                        'plugin_submission_materialization_receipts'
                      )
                      AND constraint_definition.contype = 'c'
                    ORDER BY relation.relname, constraint_definition.conname
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                let mut fingerprint = Sha256::new();
                for (relation, name, definition) in checks {
                    fingerprint.update(relation.as_bytes());
                    fingerprint.update(b"|");
                    fingerprint.update(name.as_bytes());
                    fingerprint.update(b"|");
                    fingerprint.update(definition.as_bytes());
                    fingerprint.update(b"\n");
                }
                Ok(format!("{:x}", fingerprint.finalize())
                    == POSTGRES_LISTING_REPLAY_CHECKS_FINGERPRINT)
            }
        }
    }

    #[cfg(test)]
    async fn reference_catalog_cutover_definitions_valid(&self) -> Result<bool> {
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Sqlite(&mut connection);
                self.reference_catalog_cutover_definitions_valid_on(&mut connection)
                    .await
            }
            DatabaseBackend::Postgres(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Postgres(&mut connection);
                self.reference_catalog_cutover_definitions_valid_on(&mut connection)
                    .await
            }
        }
    }

    async fn reference_catalog_cutover_definitions_valid_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<bool> {
        match &mut *connection {
            GateConnection::Sqlite(pool) => {
                if !self
                    .sqlite_reference_catalog_object_contract_valid(&mut **pool)
                    .await?
                {
                    return Ok(false);
                }
                let mut expected = Vec::new();
                for table_name in [
                    "aircraft_reference_fact_set_attestations",
                    "official_dollar_normalization_facts",
                    "listing_verification_run_items",
                ] {
                    let Some(definition) = sqlite_migration_definition("TABLE", table_name) else {
                        #[cfg(test)]
                        eprintln!("missing cutover table definition for {table_name}");
                        return Ok(false);
                    };
                    expected.push(("table", table_name, definition));
                }
                for trigger_name in REFERENCE_CATALOG_CUTOVER_SQLITE_TRIGGERS {
                    let Some(definition) = sqlite_migration_definition("TRIGGER", trigger_name)
                    else {
                        #[cfg(test)]
                        eprintln!("missing cutover trigger definition for {trigger_name}");
                        return Ok(false);
                    };
                    expected.push(("trigger", *trigger_name, definition));
                }
                expected.push((
                    "trigger",
                    "aircraft_serial_schemes_require_approval",
                    SQLITE_SERIAL_SCHEME_INSERT_TRIGGER,
                ));
                expected.push((
                    "trigger",
                    "aircraft_serial_schemes_preserve_ordering",
                    SQLITE_SERIAL_SCHEME_UPDATE_TRIGGER,
                ));
                let definitions = sqlx::query_as::<_, (String, String, Option<String>)>(
                    r#"
                    SELECT type, name, sql
                    FROM sqlite_schema
                    WHERE (type = 'table' AND name IN (
                      'aircraft_reference_fact_set_attestations',
                      'official_dollar_normalization_facts',
                      'listing_verification_run_items'
                    )) OR (type = 'trigger' AND (
                      name IN (
                        'avionics_models_referenced_status_update',
                        'aircraft_valuation_projection_validate_insert',
                        'aircraft_reference_scope_canonical_insert',
                        'aircraft_reference_scope_key_recompute_insert',
                        'aircraft_reference_versions_require_approval',
                        'official_dollar_normalization_require_evidence',
                        'official_dollar_normalization_immutable_update',
                        'official_dollar_normalization_immutable_delete',
                        'aircraft_reference_price_building_insert',
                        'aircraft_reference_price_immutable_update',
                        'aircraft_reference_price_immutable_delete',
                        'aircraft_reference_fact_set_building_insert',
                        'aircraft_reference_fact_set_immutable_update',
                        'aircraft_reference_fact_set_immutable_delete',
                        'aircraft_reference_versions_publish',
                        'aircraft_serial_schemes_require_approval',
                        'aircraft_serial_schemes_preserve_ordering'
                      )
                      OR tbl_name IN (
                        'aircraft_reference_prices',
                        'aircraft_reference_fact_set_attestations',
                        'official_dollar_normalization_facts',
                        'listing_verification_run_items'
                      )
                    ))
                    ORDER BY type, name
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                expected.sort_by_key(|(kind, name, _)| (*kind, *name));
                #[cfg(test)]
                if definitions.len() != expected.len() {
                    eprintln!(
                        "reference cutover definition count actual={} expected={} actual_names={:?} expected_names={:?}",
                        definitions.len(),
                        expected.len(),
                        definitions.iter().map(|row| (&row.0, &row.1)).collect::<Vec<_>>(),
                        expected.iter().map(|row| (row.0, row.1)).collect::<Vec<_>>()
                    );
                }
                let valid = definitions.len() == expected.len()
                    && definitions.iter().zip(expected).all(
                        |(
                            (actual_kind, actual_name, actual_definition),
                            (expected_kind, expected_name, expected_definition),
                        )| {
                            let matches = actual_kind == expected_kind
                                && actual_name == expected_name
                                && actual_definition.as_deref().is_some_and(|actual| {
                                    let expected = canonical_sql_definition(expected_definition)
                                        .replace(
                                            "createtableifnotexists",
                                            "createtable",
                                        )
                                        .replace(
                                            "createtriggerifnotexists",
                                            "createtrigger",
                                        );
                                    canonical_sql_definition(actual)
                                        == expected
                                });
                            #[cfg(test)]
                            if !matches {
                                eprintln!(
                                    "reference cutover mismatch {actual_kind}:{actual_name}\nactual={actual_definition:?}\nexpected={expected_definition}"
                                );
                            }
                            matches
                        },
                    );
                if !valid {
                    return Ok(false);
                }
                let price_table_definition = sqlx::query_scalar::<_, Option<String>>(
                    "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'aircraft_reference_prices'",
                )
                .fetch_one(&mut **pool)
                .await?;
                let Some(price_table_definition) = price_table_definition else {
                    return Ok(false);
                };
                let canonical_price_table = canonical_sql_definition(&price_table_definition);
                let valid_price_table = canonical_price_table
                    == canonical_sql_definition(SQLITE_REFERENCE_PRICES_FRESH_TABLE);
                let valid_price_column = sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT COUNT(*) = 1
                    FROM pragma_table_info('aircraft_reference_prices')
                    WHERE name = 'configuration_basis'
                      AND upper(type) = 'TEXT'
                      AND "notnull" = 1
                      AND dflt_value = '''unknown'''
                      AND pk = 0
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0;
                let protected_index_signatures = sqlx::query_scalar::<_, String>(
                    r#"
                    WITH protected_relation(relation_name) AS (
                      VALUES
                        ('aircraft_reference_prices'),
                        ('aircraft_reference_fact_set_attestations'),
                        ('official_dollar_normalization_facts'),
                        ('listing_verification_run_items')
                    )
                    SELECT
                      protected_relation.relation_name || ':' ||
                      index_row.name || ':' || index_row.[unique] || ':' ||
                      index_row.origin || ':' || index_row.partial || ':' ||
                      COALESCE((
                        SELECT group_concat(index_column.signature, ',')
                        FROM (
                          SELECT
                            xinfo.seqno || ':' || xinfo.cid || ':' ||
                            COALESCE(xinfo.name, '') || ':' || xinfo.desc || ':' ||
                            xinfo.coll || ':' || xinfo.key AS signature
                          FROM pragma_index_xinfo(index_row.name) xinfo
                          ORDER BY xinfo.seqno
                        ) index_column
                      ), '')
                    FROM protected_relation
                    JOIN pragma_index_list(protected_relation.relation_name) index_row
                    ORDER BY protected_relation.relation_name, index_row.name
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                let valid_indexes = protected_index_signatures.iter().map(String::as_str).eq(
                    REFERENCE_CATALOG_CUTOVER_SQLITE_INDEX_SIGNATURES
                        .iter()
                        .copied(),
                );
                #[cfg(test)]
                if !valid_indexes {
                    eprintln!(
                        "SQLite protected reference index mismatch\nactual={protected_index_signatures:#?}\nexpected={REFERENCE_CATALOG_CUTOVER_SQLITE_INDEX_SIGNATURES:#?}"
                    );
                }
                Ok(valid_price_table && valid_price_column && valid_indexes)
            }
            GateConnection::Postgres(pool) => {
                let routine_names = REFERENCE_CATALOG_CUTOVER_ROUTINES
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect::<Vec<_>>();
                let routines = sqlx::query_as::<_, PostgresReferenceRoutineDefinition>(
                    r#"
                    SELECT
                      routine.proname AS function_name,
                      routine.oid IS NOT DISTINCT FROM CASE routine.proname
                        WHEN 'aircraft_serial_natural_sort_key' THEN
                          pg_catalog.to_regprocedure(
                            'public.aircraft_serial_natural_sort_key(pg_catalog.text)'
                          )
                        ELSE pg_catalog.to_regprocedure(
                          'public.' || routine.proname || '()'
                        )
                      END AS function_oid_matches,
                      routine.prosrc AS function_source,
                      COALESCE(
                        pg_catalog.array_to_string(routine.proconfig, E'\n'), ''
                      ) AS function_configuration,
                      language.lanname AS function_language,
                      pg_catalog.format_type(routine.prorettype, NULL) AS result_type,
                      pg_catalog.pg_get_function_identity_arguments(routine.oid)
                        AS identity_arguments,
                      routine.prosecdef AS security_definer,
                      routine.proisstrict AS strict,
                      routine.provolatile::text AS volatility,
                      routine.proparallel::text AS parallel_safety
                    FROM pg_catalog.pg_proc routine
                    JOIN pg_catalog.pg_namespace namespace
                      ON namespace.oid = routine.pronamespace
                    JOIN pg_catalog.pg_language language
                      ON language.oid = routine.prolang
                    WHERE namespace.nspname = 'public'
                      AND routine.proname = ANY($1)
                    ORDER BY routine.proname, routine.oid
                    "#,
                )
                .bind(&routine_names)
                .fetch_all(&mut **pool)
                .await
                .context("could not inspect PostgreSQL reference-cutover routines")?;
                if routines.len() != REFERENCE_CATALOG_CUTOVER_ROUTINES.len() {
                    #[cfg(test)]
                    eprintln!(
                        "PostgreSQL reference routine count actual={} expected={} names={:?}",
                        routines.len(),
                        REFERENCE_CATALOG_CUTOVER_ROUTINES.len(),
                        routines
                            .iter()
                            .map(|routine| routine.function_name.as_str())
                            .collect::<Vec<_>>()
                    );
                    return Ok(false);
                }
                for routine in &routines {
                    let expected_source = if routine.function_name
                        == "invalidate_listing_avionics_authorization_for_capture"
                    {
                        postgres_function_source(POSTGRES_SCHEMA_SQL, &routine.function_name)
                    } else {
                        postgres_migration_function_source(&routine.function_name)
                    };
                    let Some(expected_source) = expected_source else {
                        #[cfg(test)]
                        eprintln!(
                            "missing PostgreSQL migration source for {}",
                            routine.function_name
                        );
                        return Ok(false);
                    };
                    let natural_sort = routine.function_name == "aircraft_serial_natural_sort_key";
                    if !routine.function_oid_matches
                        || canonical_sql_definition(&routine.function_source)
                            != canonical_sql_definition(expected_source)
                        || routine.function_configuration != "search_path=pg_catalog"
                        || routine.function_language != "plpgsql"
                        || routine.result_type != if natural_sort { "text" } else { "trigger" }
                        || routine.identity_arguments
                            != if natural_sort {
                                "serial_value text"
                            } else {
                                ""
                            }
                        || routine.security_definer
                        || routine.strict != natural_sort
                        || routine.volatility != if natural_sort { "i" } else { "v" }
                        || routine.parallel_safety != if natural_sort { "s" } else { "u" }
                    {
                        #[cfg(test)]
                        eprintln!(
                            "PostgreSQL reference routine mismatch {}: oid={} source={} config={:?} language={:?} result={:?} args={:?} security_definer={} strict={} volatility={:?} parallel={:?}",
                            routine.function_name,
                            routine.function_oid_matches,
                            canonical_sql_definition(&routine.function_source)
                                == canonical_sql_definition(expected_source),
                            routine.function_configuration,
                            routine.function_language,
                            routine.result_type,
                            routine.identity_arguments,
                            routine.security_definer,
                            routine.strict,
                            routine.volatility,
                            routine.parallel_safety,
                        );
                        return Ok(false);
                    }
                }

                let triggers = sqlx::query_as::<_, (String, String)>(
                    r#"
                    SELECT
                      pg_catalog.pg_get_triggerdef(trigger_row.oid, true),
                      trigger_row.tgenabled::text
                    FROM pg_catalog.pg_trigger trigger_row
                    JOIN pg_catalog.pg_class relation
                      ON relation.oid = trigger_row.tgrelid
                    JOIN pg_catalog.pg_namespace relation_namespace
                      ON relation_namespace.oid = relation.relnamespace
                    JOIN pg_catalog.pg_proc routine ON routine.oid = trigger_row.tgfoid
                    WHERE NOT trigger_row.tgisinternal
                      AND relation_namespace.nspname = 'public'
                      AND (
                        routine.proname = ANY($1)
                        OR relation.relname = ANY($2)
                      )
                    "#,
                )
                .bind(&routine_names)
                .bind(
                    &REFERENCE_CATALOG_CUTOVER_PROTECTED_RELATIONS
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect::<Vec<_>>(),
                )
                .fetch_all(&mut **pool)
                .await
                .context("could not inspect PostgreSQL reference-cutover trigger bindings")?;
                let mut actual_triggers = triggers
                    .iter()
                    .map(|(definition, _)| canonical_postgres_trigger_definition(definition))
                    .collect::<Vec<_>>();
                actual_triggers.sort();
                if triggers.iter().any(|(_, enabled)| enabled != "O")
                    || actual_triggers != postgres_schema_reference_trigger_definitions()
                {
                    #[cfg(test)]
                    {
                        let expected_triggers = postgres_schema_reference_trigger_definitions();
                        eprintln!(
                            "PostgreSQL reference trigger mismatch: disabled={:?}\nactual-only={:#?}\nexpected-only={:#?}",
                            triggers
                                .iter()
                                .filter(|(_, enabled)| enabled != "O")
                                .collect::<Vec<_>>(),
                            actual_triggers
                                .iter()
                                .filter(|definition| !expected_triggers.contains(definition))
                                .collect::<Vec<_>>(),
                            expected_triggers
                                .iter()
                                .filter(|definition| !actual_triggers.contains(definition))
                                .collect::<Vec<_>>()
                        );
                    }
                    return Ok(false);
                }

                if !self
                    .postgres_reference_catalog_relations_valid(&mut **pool)
                    .await?
                {
                    return Ok(false);
                }
                self.postgres_reference_catalog_object_contract_valid(&mut **pool)
                    .await
            }
        }
    }

    async fn sqlite_reference_catalog_object_contract_valid(
        &self,
        pool: &mut SqliteConnection,
    ) -> Result<bool> {
        let retired_relations = serde_json::to_string(REFERENCE_CATALOG_CUTOVER_RETIRED_RELATIONS)?;
        let retired_object_count = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT count(*) FROM sqlite_schema
            WHERE name IN (SELECT value FROM json_each(?))
               OR tbl_name IN (SELECT value FROM json_each(?))
            "#,
        )
        .bind(&retired_relations)
        .bind(&retired_relations)
        .fetch_one(&mut *pool)
        .await?;
        if retired_object_count != 0 {
            return Ok(false);
        }
        let objects = sqlx::query_as::<_, (String, String)>(
            r#"
                WITH
                protected_relations(name) AS (
                  VALUES
                    ('plugin_submissions'),
                    ('avionics_models'),
                    ('aircraft_engine_catalog_models'),
                    ('aircraft_propeller_catalog_models'),
                    ('aircraft_makes'),
                    ('aircraft_model_families'),
                    ('aircraft_designations'),
                    ('aircraft_make_aliases'),
                    ('aircraft_family_aliases'),
                    ('aircraft_designation_aliases'),
                    ('aircraft_designation_identifiers'),
                    ('aircraft_generations'),
                    ('aircraft_generation_designations'),
                    ('aircraft_factory_packages'),
                    ('aircraft_package_applicability'),
                    ('aircraft_reference_configurations'),
                    ('aircraft_serial_number_schemes'),
                    ('aircraft_feature_definitions'),
                    ('aircraft_reference_configuration_versions'),
                    ('aircraft_reference_applicability_scopes'),
                    ('aircraft_reference_prices'),
                    ('aircraft_reference_avionics'),
                    ('aircraft_reference_engines'),
                    ('aircraft_reference_propellers'),
                    ('aircraft_reference_features'),
                    ('aircraft_reference_fact_set_attestations'),
                    ('official_dollar_normalization_facts'),
                    ('aircraft_valuation_compatibility_projections'),
                    ('listing_verification_run_items')
                ),
                retired_relations(name) AS (
                  VALUES
                    ('aircraft_model_spec_versions'),
                    ('aircraft_model_variant_price_points'),
                    ('aircraft_model_variant_default_avionics'),
                    ('aircraft_model_variant_default_avionics_candidates'),
                    ('depreciation_profiles'),
                    ('depreciation_profile_fit_metadata'),
                    ('component_depreciation_profiles')
                ),
                owned_relations(name) AS (
                  SELECT name FROM protected_relations
                  UNION ALL SELECT name FROM retired_relations
                ),
                objects(object_key, definition) AS (
                  SELECT
                    schema_row.type || ':' || schema_row.name,
                    COALESCE(lower(replace(replace(replace(replace(
                      schema_row.sql, char(9), ''
                    ), char(10), ''), char(13), ''), ' ', '')), '')
                  FROM sqlite_schema schema_row
                  WHERE (
                    schema_row.type = 'table'
                    AND schema_row.name IN (SELECT name FROM owned_relations)
                  ) OR (
                    schema_row.name IN (SELECT name FROM retired_relations)
                    OR schema_row.tbl_name IN (SELECT name FROM retired_relations)
                  ) OR (
                    schema_row.type = 'view'
                    AND schema_row.name = 'aircraft_reference_serial_key_errors'
                  ) OR (
                    schema_row.type = 'trigger'
                    AND schema_row.tbl_name IN (SELECT name FROM owned_relations)
                    AND schema_row.name NOT IN (
                      'avionics_models_approved_concrete_model_insert',
                      'avionics_models_approved_concrete_model_update'
                    )
                  )
                  UNION ALL
                  SELECT
                    'index:' || relation.name || ':' || index_row.name,
                    index_row.[unique] || ':' || index_row.origin || ':' ||
                      index_row.partial || ':' || COALESCE(lower(replace(replace(
                        replace(replace((SELECT sql FROM sqlite_schema
                          WHERE type = 'index' AND name = index_row.name),
                          char(9), ''), char(10), ''), char(13), ''), ' ', ''
                      )), '') || ':' || COALESCE((
                        SELECT group_concat(index_column.signature, ',')
                        FROM (
                          SELECT
                            xinfo.seqno || ':' || xinfo.cid || ':' ||
                            COALESCE(xinfo.name, '') || ':' || xinfo.desc || ':' ||
                            xinfo.coll || ':' || xinfo.key AS signature
                          FROM pragma_index_xinfo(index_row.name) xinfo
                          ORDER BY xinfo.seqno
                        ) index_column
                      ), '')
                  FROM owned_relations relation
                  JOIN pragma_index_list(relation.name) index_row
                )
                SELECT object_key, definition FROM objects ORDER BY object_key
                "#,
        )
        .fetch_all(&mut *pool)
        .await
        .context("could not attest exact SQLite reference-cutover objects")?;
        let object_count = i64::try_from(objects.len())?;
        let mut definition_hasher = Sha3_256::new();
        for (index, (object_key, definition)) in objects.iter().enumerate() {
            if index > 0 {
                definition_hasher.update(b"|");
            }
            definition_hasher.update(object_key.as_bytes());
            definition_hasher.update(b"=");
            definition_hasher.update(definition.as_bytes());
        }
        let definition_digest = format!("{:x}", definition_hasher.finalize());
        let valid = object_count == REFERENCE_CATALOG_CUTOVER_SQLITE_OBJECT_COUNT
            && definition_digest == REFERENCE_CATALOG_CUTOVER_SQLITE_DEFINITION_DIGEST;
        #[cfg(test)]
        if !valid {
            eprintln!(
                "SQLite reference object contract mismatch count={object_count} digest={definition_digest} expected_count={REFERENCE_CATALOG_CUTOVER_SQLITE_OBJECT_COUNT} expected_digest={REFERENCE_CATALOG_CUTOVER_SQLITE_DEFINITION_DIGEST}"
            );
        }
        Ok(valid)
    }

    async fn postgres_reference_catalog_object_contract_valid(
        &self,
        pool: &mut PgConnection,
    ) -> Result<bool> {
        let retired_relations = REFERENCE_CATALOG_CUTOVER_RETIRED_RELATIONS
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        let retired_routines = REFERENCE_CATALOG_CUTOVER_RETIRED_ROUTINES
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        let retired_object_exists = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
              SELECT 1
              FROM pg_catalog.pg_class relation
              JOIN pg_catalog.pg_namespace namespace
                ON namespace.oid = relation.relnamespace
              WHERE namespace.nspname = 'public'
                AND relation.relname = ANY($1)
            ) OR EXISTS (
              SELECT 1
              FROM pg_catalog.pg_proc routine
              JOIN pg_catalog.pg_namespace namespace
                ON namespace.oid = routine.pronamespace
              WHERE namespace.nspname = 'public'
                AND routine.proname = ANY($2)
            )
            "#,
        )
        .bind(&retired_relations)
        .bind(&retired_routines)
        .fetch_one(&mut *pool)
        .await?;
        if retired_object_exists {
            return Ok(false);
        }
        let owned_objects_query = postgres_reference_owned_objects_query()
            .context("cutover migration is missing its owned-object query")?;
        let contract_query = format!(
            "SELECT count(*), pg_catalog.md5(pg_catalog.string_agg(\
             object_key || '=' || definition, E'\\n' ORDER BY object_key)) \
             FROM ({owned_objects_query}) owned_object(object_key, definition) \
             WHERE object_key <> \
               'trigger:avionics_models:avionics_models_approved_concrete_model'"
        );
        let (object_count, definition_digest) =
            sqlx::query_as::<_, (i64, Option<String>)>(&contract_query)
                .fetch_one(&mut *pool)
                .await
                .context("could not attest exact PostgreSQL reference-cutover objects")?;
        let valid = object_count == REFERENCE_CATALOG_CUTOVER_POSTGRES_OBJECT_COUNT
            && definition_digest.as_deref()
                == Some(REFERENCE_CATALOG_CUTOVER_POSTGRES_DEFINITION_DIGEST);
        #[cfg(test)]
        if !valid {
            eprintln!(
                "PostgreSQL reference object contract mismatch count={object_count} digest={definition_digest:?} expected_count={REFERENCE_CATALOG_CUTOVER_POSTGRES_OBJECT_COUNT} expected_digest={REFERENCE_CATALOG_CUTOVER_POSTGRES_DEFINITION_DIGEST}"
            );
        }
        Ok(valid)
    }

    async fn postgres_reference_catalog_relations_valid(
        &self,
        pool: &mut PgConnection,
    ) -> Result<bool> {
        let relation_count = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM pg_catalog.pg_class relation
            JOIN pg_catalog.pg_namespace namespace
              ON namespace.oid = relation.relnamespace
            WHERE namespace.nspname = 'public'
              AND relation.relkind = 'r'
              AND relation.oid IN (
                pg_catalog.to_regclass(
                  'public.aircraft_reference_fact_set_attestations'
                ),
                pg_catalog.to_regclass(
                  'public.official_dollar_normalization_facts'
                )
              )
            "#,
        )
        .fetch_one(&mut *pool)
        .await
        .context("could not inspect PostgreSQL reference-cutover relations")?;
        if relation_count != 2 {
            #[cfg(test)]
            eprintln!("PostgreSQL reference relation count={relation_count}, expected=2");
            return Ok(false);
        }

        let columns = sqlx::query_as::<_, PostgresReferenceColumnDefinition>(
            r#"
            SELECT
              relation.relname AS relation_name,
              attribute.attnum::smallint AS ordinal,
              attribute.attname AS column_name,
              pg_catalog.format_type(attribute.atttypid, attribute.atttypmod)
                AS data_type,
              attribute.attnotnull AS not_null,
              attribute.attidentity::text AS identity_kind,
              COALESCE(
                pg_catalog.pg_get_expr(default_row.adbin, default_row.adrelid),
                ''
              ) AS default_expression
            FROM pg_catalog.pg_class relation
            JOIN pg_catalog.pg_namespace namespace
              ON namespace.oid = relation.relnamespace
            JOIN pg_catalog.pg_attribute attribute
              ON attribute.attrelid = relation.oid
             AND attribute.attnum > 0
             AND NOT attribute.attisdropped
            LEFT JOIN pg_catalog.pg_attrdef default_row
              ON default_row.adrelid = relation.oid
             AND default_row.adnum = attribute.attnum
            WHERE namespace.nspname = 'public'
              AND relation.oid IN (
                pg_catalog.to_regclass(
                  'public.aircraft_reference_fact_set_attestations'
                ),
                pg_catalog.to_regclass(
                  'public.official_dollar_normalization_facts'
                )
              )
            ORDER BY relation.relname, attribute.attnum
            "#,
        )
        .fetch_all(&mut *pool)
        .await
        .context("could not inspect PostgreSQL reference-cutover columns")?;
        let expected_columns = [
            (
                "aircraft_reference_fact_set_attestations",
                1_i16,
                "id",
                "bigint",
                true,
                "d",
                "",
            ),
            (
                "aircraft_reference_fact_set_attestations",
                2,
                "aircraft_reference_configuration_version_id",
                "bigint",
                true,
                "",
                "",
            ),
            (
                "aircraft_reference_fact_set_attestations",
                3,
                "fact_set_kind",
                "text",
                true,
                "",
                "",
            ),
            (
                "aircraft_reference_fact_set_attestations",
                4,
                "evidence_claim_id",
                "bigint",
                true,
                "",
                "",
            ),
            (
                "aircraft_reference_fact_set_attestations",
                5,
                "created_at",
                "timestamp with time zone",
                true,
                "",
                "CURRENT_TIMESTAMP",
            ),
            (
                "official_dollar_normalization_facts",
                1,
                "id",
                "bigint",
                true,
                "d",
                "",
            ),
            (
                "official_dollar_normalization_facts",
                2,
                "source_year",
                "bigint",
                true,
                "",
                "",
            ),
            (
                "official_dollar_normalization_facts",
                3,
                "target_year",
                "bigint",
                true,
                "",
                "",
            ),
            (
                "official_dollar_normalization_facts",
                4,
                "index_series",
                "text",
                true,
                "",
                "",
            ),
            (
                "official_dollar_normalization_facts",
                5,
                "source_index_value",
                "double precision",
                true,
                "",
                "",
            ),
            (
                "official_dollar_normalization_facts",
                6,
                "target_index_value",
                "double precision",
                true,
                "",
                "",
            ),
            (
                "official_dollar_normalization_facts",
                7,
                "normalization_factor",
                "double precision",
                true,
                "",
                "",
            ),
            (
                "official_dollar_normalization_facts",
                8,
                "evidence_claim_id",
                "bigint",
                true,
                "",
                "",
            ),
            (
                "official_dollar_normalization_facts",
                9,
                "created_at",
                "timestamp with time zone",
                true,
                "",
                "CURRENT_TIMESTAMP",
            ),
        ];
        if columns.len() != expected_columns.len()
            || !columns
                .iter()
                .zip(expected_columns)
                .all(|(actual, expected)| {
                    actual.relation_name == expected.0
                        && actual.ordinal == expected.1
                        && actual.column_name == expected.2
                        && actual.data_type == expected.3
                        && actual.not_null == expected.4
                        && actual.identity_kind == expected.5
                        && actual.default_expression == expected.6
                })
        {
            #[cfg(test)]
            eprintln!(
                "PostgreSQL reference column mismatch\nactual={:#?}",
                columns
                    .iter()
                    .map(|column| (
                        &column.relation_name,
                        column.ordinal,
                        &column.column_name,
                        &column.data_type,
                        column.not_null,
                        &column.identity_kind,
                        &column.default_expression,
                    ))
                    .collect::<Vec<_>>()
            );
            return Ok(false);
        }

        let constraints = sqlx::query_as::<_, PostgresReferenceConstraintDefinition>(
            r#"
            SELECT
              relation.relname AS relation_name,
              constraint_row.contype::text AS constraint_type,
              pg_catalog.pg_get_constraintdef(constraint_row.oid, true) AS definition
            FROM pg_catalog.pg_constraint constraint_row
            JOIN pg_catalog.pg_class relation
              ON relation.oid = constraint_row.conrelid
            JOIN pg_catalog.pg_namespace namespace
              ON namespace.oid = relation.relnamespace
            WHERE namespace.nspname = 'public'
              AND relation.oid IN (
                pg_catalog.to_regclass(
                  'public.aircraft_reference_fact_set_attestations'
                ),
                pg_catalog.to_regclass(
                  'public.official_dollar_normalization_facts'
                )
              )
            "#,
        )
        .fetch_all(&mut *pool)
        .await
        .context("could not inspect PostgreSQL reference-cutover constraints")?;
        let expected_constraints = [
            ("aircraft_reference_fact_set_attestations", "c", "CHECK (fact_set_kind = ANY (ARRAY['avionics'::text, 'engines'::text, 'propellers'::text, 'features'::text]))"),
            ("aircraft_reference_fact_set_attestations", "f", "FOREIGN KEY (aircraft_reference_configuration_version_id) REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE"),
            ("aircraft_reference_fact_set_attestations", "f", "FOREIGN KEY (evidence_claim_id) REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT"),
            ("aircraft_reference_fact_set_attestations", "p", "PRIMARY KEY (id)"),
            ("aircraft_reference_fact_set_attestations", "u", "UNIQUE (aircraft_reference_configuration_version_id, fact_set_kind)"),
            ("official_dollar_normalization_facts", "c", "CHECK (source_year <> target_year)"),
            ("official_dollar_normalization_facts", "c", "CHECK (abs(normalization_factor - target_index_value / source_index_value) <= 0.000000001::double precision)"),
            ("official_dollar_normalization_facts", "c", "CHECK (length(btrim(index_series)) > 0)"),
            ("official_dollar_normalization_facts", "c", "CHECK (normalization_factor > 0::double precision)"),
            ("official_dollar_normalization_facts", "c", "CHECK (source_index_value > 0::double precision)"),
            ("official_dollar_normalization_facts", "c", "CHECK (source_year >= 1900 AND source_year <= 2200)"),
            ("official_dollar_normalization_facts", "c", "CHECK (target_index_value > 0::double precision)"),
            ("official_dollar_normalization_facts", "c", "CHECK (target_year >= 1900 AND target_year <= 2200)"),
            ("official_dollar_normalization_facts", "f", "FOREIGN KEY (evidence_claim_id) REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT"),
            ("official_dollar_normalization_facts", "p", "PRIMARY KEY (id)"),
            ("official_dollar_normalization_facts", "u", "UNIQUE (evidence_claim_id)"),
            ("official_dollar_normalization_facts", "u", "UNIQUE (source_year, target_year)"),
        ];
        let mut actual_constraints = constraints
            .iter()
            .map(|constraint| {
                (
                    constraint.relation_name.as_str(),
                    constraint.constraint_type.as_str(),
                    canonical_sql_definition(&constraint.definition.replace("public.", "")),
                )
            })
            .collect::<Vec<_>>();
        actual_constraints.sort();
        let mut expected_constraints = expected_constraints
            .iter()
            .map(|constraint| {
                (
                    constraint.0,
                    constraint.1,
                    canonical_sql_definition(constraint.2),
                )
            })
            .collect::<Vec<_>>();
        expected_constraints.sort();
        if actual_constraints != expected_constraints {
            #[cfg(test)]
            eprintln!(
                "PostgreSQL reference constraint mismatch\nactual-only={:#?}\nexpected-only={:#?}",
                actual_constraints
                    .iter()
                    .filter(|constraint| !expected_constraints.contains(constraint))
                    .collect::<Vec<_>>(),
                expected_constraints
                    .iter()
                    .filter(|constraint| !actual_constraints.contains(constraint))
                    .collect::<Vec<_>>()
            );
            return Ok(false);
        }

        let valid_price_basis = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT
              EXISTS (
                SELECT 1
                FROM pg_catalog.pg_attribute attribute
                LEFT JOIN pg_catalog.pg_attrdef default_row
                  ON default_row.adrelid = attribute.attrelid
                 AND default_row.adnum = attribute.attnum
                WHERE attribute.attrelid = pg_catalog.to_regclass(
                  'public.aircraft_reference_prices'
                )
                  AND attribute.attname = 'configuration_basis'
                  AND attribute.atttypid = pg_catalog.to_regtype('pg_catalog.text')
                  AND attribute.attnotnull
                  AND NOT attribute.attisdropped
                  AND pg_catalog.pg_get_expr(
                    default_row.adbin, default_row.adrelid
                  ) = '''unknown''::text'
              )
              AND 1 = (
                SELECT COUNT(*)
                FROM pg_catalog.pg_constraint constraint_row
                WHERE constraint_row.conrelid = pg_catalog.to_regclass(
                  'public.aircraft_reference_prices'
                )
                  AND constraint_row.contype = 'c'
                  AND pg_catalog.pg_get_constraintdef(
                    constraint_row.oid, true
                  ) = 'CHECK (configuration_basis = ANY (ARRAY[''full_standard_configuration''::text, ''base_aircraft_only''::text, ''unknown''::text]))'
              )
            "#,
        )
        .fetch_one(&mut *pool)
        .await
        .context("could not inspect PostgreSQL reference-price basis contract")?;
        #[cfg(test)]
        if !valid_price_basis {
            eprintln!("PostgreSQL reference-price basis contract mismatch");
        }
        Ok(valid_price_basis)
    }
    #[cfg(test)]
    async fn aircraft_listing_identity_correction_definitions_valid(&self) -> Result<bool> {
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Sqlite(&mut connection);
                self.aircraft_listing_identity_correction_definitions_valid_on(&mut connection)
                    .await
            }
            DatabaseBackend::Postgres(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Postgres(&mut connection);
                self.aircraft_listing_identity_correction_definitions_valid_on(&mut connection)
                    .await
            }
        }
    }

    async fn aircraft_listing_identity_correction_definitions_valid_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<bool> {
        match &mut *connection {
            GateConnection::Sqlite(pool) => {
                let definitions = sqlx::query_as::<_, (String, Option<String>)>(
                    r#"
                    SELECT name, sql
                    FROM sqlite_schema
                    WHERE type = 'trigger'
                      AND name IN (
                        'aircraft_listing_identity_corrections_immutable_update',
                        'aircraft_listing_identity_corrections_immutable_delete',
                        'aircraft_identity_correction_observation_immutable_update',
                        'aircraft_identity_correction_observation_immutable_delete'
                      )
                    ORDER BY name
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                let expected = [
                    (
                        "aircraft_identity_correction_observation_immutable_delete",
                        SQLITE_CORRECTION_OBSERVATION_DELETE_TRIGGER,
                    ),
                    (
                        "aircraft_identity_correction_observation_immutable_update",
                        SQLITE_CORRECTION_OBSERVATION_UPDATE_TRIGGER,
                    ),
                    (
                        "aircraft_listing_identity_corrections_immutable_delete",
                        SQLITE_CORRECTION_DECISION_DELETE_TRIGGER,
                    ),
                    (
                        "aircraft_listing_identity_corrections_immutable_update",
                        SQLITE_CORRECTION_DECISION_UPDATE_TRIGGER,
                    ),
                ];
                Ok(definitions.len() == expected.len()
                    && definitions.iter().zip(expected).all(
                        |(
                            (actual_name, actual_definition),
                            (expected_name, expected_definition),
                        )| {
                            actual_name == expected_name
                                && actual_definition.as_deref().is_some_and(|actual| {
                                    canonical_sql_definition(actual)
                                        == canonical_sql_definition(expected_definition)
                                })
                        },
                    ))
            }
            GateConnection::Postgres(pool) => {
                let definitions = sqlx::query_as::<_, PostgresCorrectionTriggerDefinition>(
                    r#"
                    SELECT
                      actual_trigger.tgname AS trigger_name,
                      actual_trigger.tgtype::smallint AS trigger_type,
                      actual_trigger.tgqual IS NULL AS has_no_when_clause,
                      COALESCE((
                        SELECT string_agg(attribute.attname, ',' ORDER BY attribute.attname)
                        FROM unnest(actual_trigger.tgattr) AS update_column(attnum)
                        JOIN pg_attribute attribute
                          ON attribute.attrelid = actual_trigger.tgrelid
                         AND attribute.attnum = update_column.attnum
                      ), '') AS update_columns,
                      actual_trigger.tgenabled::text AS trigger_enabled,
                      relation_namespace.nspname AS relation_schema,
                      relation.relname AS relation_name,
                      actual_trigger.tgrelid IS NOT DISTINCT FROM CASE actual_trigger.tgname
                        WHEN 'aircraft_listing_identity_corrections_immutable'
                          THEN pg_catalog.to_regclass(
                            'public.aircraft_listing_identity_correction_decisions'
                          )
                        WHEN 'aircraft_identity_correction_observation_immutable'
                          THEN pg_catalog.to_regclass(
                            'public.aircraft_identity_observations'
                          )
                        WHEN 'aircraft_source_identity_receipt_gate'
                          THEN pg_catalog.to_regclass('public.aircraft_sale_listings')
                      END AS relation_oid_matches,
                      routine.proname AS function_name,
                      routine_namespace.nspname AS function_schema,
                      routine.oid IS NOT DISTINCT FROM CASE actual_trigger.tgname
                        WHEN 'aircraft_listing_identity_corrections_immutable'
                          THEN pg_catalog.to_regprocedure(
                            'public.preserve_aircraft_listing_identity_correction()'
                          )
                        WHEN 'aircraft_identity_correction_observation_immutable'
                          THEN pg_catalog.to_regprocedure(
                            'public.preserve_correction_identity_observation()'
                          )
                        WHEN 'aircraft_source_identity_receipt_gate'
                          THEN pg_catalog.to_regprocedure(
                            'public.require_source_identity_correction_receipt()'
                          )
                      END AS function_oid_matches,
                      routine.prosrc AS function_source,
                      COALESCE(
                        pg_catalog.array_to_string(routine.proconfig, E'\n'), ''
                      ) AS function_configuration,
                      language.lanname AS function_language,
                      routine.prorettype = 'trigger'::regtype AS returns_trigger,
                      routine.pronargs::smallint AS argument_count,
                      routine.prosecdef AS security_definer,
                      routine.proisstrict AS strict,
                      routine.provolatile::text AS volatility
                    FROM pg_catalog.pg_trigger actual_trigger
                    JOIN pg_catalog.pg_class relation
                      ON relation.oid = actual_trigger.tgrelid
                    JOIN pg_catalog.pg_namespace relation_namespace
                      ON relation_namespace.oid = relation.relnamespace
                    JOIN pg_catalog.pg_proc routine ON routine.oid = actual_trigger.tgfoid
                    JOIN pg_catalog.pg_namespace routine_namespace
                      ON routine_namespace.oid = routine.pronamespace
                    JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
                    WHERE NOT actual_trigger.tgisinternal
                      AND (
                        (
                          actual_trigger.tgname =
                            'aircraft_listing_identity_corrections_immutable'
                          AND relation_namespace.nspname = 'public'
                          AND relation.relname =
                            'aircraft_listing_identity_correction_decisions'
                        ) OR (
                          actual_trigger.tgname =
                            'aircraft_identity_correction_observation_immutable'
                          AND relation_namespace.nspname = 'public'
                          AND relation.relname = 'aircraft_identity_observations'
                        )
                      )
                    ORDER BY actual_trigger.tgname
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?;
                let expected = [
                    (
                        "aircraft_identity_correction_observation_immutable",
                        27_i16,
                        "",
                        "aircraft_identity_observations",
                        "preserve_correction_identity_observation",
                        POSTGRES_CORRECTION_OBSERVATION_FUNCTION_SOURCE,
                    ),
                    (
                        "aircraft_listing_identity_corrections_immutable",
                        27_i16,
                        "",
                        "aircraft_listing_identity_correction_decisions",
                        "preserve_aircraft_listing_identity_correction",
                        POSTGRES_CORRECTION_DECISION_FUNCTION_SOURCE,
                    ),
                ];
                Ok(definitions.len() == expected.len()
                    && definitions.iter().zip(expected).all(
                        |(
                            actual,
                            (
                                expected_name,
                                expected_type,
                                expected_columns,
                                expected_relation,
                                expected_function,
                                expected_source,
                            ),
                        )| {
                            actual.trigger_name == expected_name
                                && actual.trigger_type == expected_type
                                && actual.has_no_when_clause
                                && actual.update_columns == expected_columns
                                && actual.trigger_enabled == "O"
                                && actual.relation_schema == "public"
                                && actual.relation_name == expected_relation
                                && actual.relation_oid_matches
                                && actual.function_name == expected_function
                                && actual.function_schema == "public"
                                && actual.function_oid_matches
                                && canonical_sql_definition(&actual.function_source)
                                    == canonical_sql_definition(expected_source)
                                && actual.function_configuration == "search_path=pg_catalog"
                                && actual.function_language == "plpgsql"
                                && actual.returns_trigger
                                && actual.argument_count == 0
                                && !actual.security_definer
                                && !actual.strict
                                && actual.volatility == "v"
                        },
                    ))
            }
        }
    }

    async fn postgres_faa_registry_trigger_definitions_valid(
        &self,
        pool: &mut PgConnection,
    ) -> Result<bool> {
        let definitions = sqlx::query_as::<_, PostgresFaaReferenceTriggerDefinition>(
            r#"
            SELECT
              actual_trigger.tgname AS trigger_name,
              actual_trigger.tgtype::smallint AS trigger_type,
              actual_trigger.tgqual IS NULL AS has_no_when_clause,
              pg_catalog.cardinality(actual_trigger.tgattr) = 0
                AS has_no_update_columns,
              actual_trigger.tgenabled::text AS trigger_enabled,
              actual_trigger.tgnargs::smallint AS trigger_argument_count,
              relation_namespace.nspname AS relation_schema,
              relation.relname AS relation_name,
              TRUE AS relation_oid_matches,
              routine.proname AS function_name,
              routine_namespace.nspname AS function_schema,
              TRUE AS function_oid_matches,
              routine.prosrc AS function_source,
              COALESCE(
                pg_catalog.array_to_string(routine.proconfig, E'\n'), ''
              ) AS function_configuration,
              language.lanname AS function_language,
              routine.prorettype = 'trigger'::pg_catalog.regtype AS returns_trigger,
              routine.pronargs::smallint AS function_argument_count,
              routine.prokind = 'f' AS ordinary_function,
              routine.prosecdef AS security_definer,
              routine.proisstrict AS strict,
              routine.provolatile::text AS volatility,
              routine.proparallel::text AS parallel_safety
            FROM pg_catalog.pg_trigger actual_trigger
            JOIN pg_catalog.pg_class relation
              ON relation.oid = actual_trigger.tgrelid
            JOIN pg_catalog.pg_namespace relation_namespace
              ON relation_namespace.oid = relation.relnamespace
            JOIN pg_catalog.pg_proc routine ON routine.oid = actual_trigger.tgfoid
            JOIN pg_catalog.pg_namespace routine_namespace
              ON routine_namespace.oid = routine.pronamespace
            JOIN pg_catalog.pg_language language ON language.oid = routine.prolang
            WHERE NOT actual_trigger.tgisinternal
              AND relation_namespace.nspname = 'public'
              AND relation.relname IN (
                'faa_registry_aircraft',
                'faa_registry_aircraft_references',
                'faa_registry_coverage',
                'faa_registry_engine_references',
                'faa_registry_snapshots'
              )
            ORDER BY actual_trigger.tgname
            "#,
        )
        .fetch_all(&mut *pool)
        .await?;
        let expected = [
            (
                "faa_registry_aircraft_immutable",
                27_i16,
                "faa_registry_aircraft",
                "preserve_faa_registry_data",
                POSTGRES_FAA_IMMUTABILITY_FUNCTION_SOURCE,
            ),
            (
                "faa_registry_aircraft_references_immutable",
                27_i16,
                "faa_registry_aircraft_references",
                "preserve_faa_registry_data",
                POSTGRES_FAA_IMMUTABILITY_FUNCTION_SOURCE,
            ),
            (
                "faa_registry_aircraft_references_reachable",
                7_i16,
                "faa_registry_aircraft_references",
                "validate_faa_aircraft_reference_reachability",
                POSTGRES_FAA_AIRCRAFT_REFERENCE_REACHABILITY_FUNCTION_SOURCE,
            ),
            (
                "faa_registry_coverage_consistent",
                7_i16,
                "faa_registry_coverage",
                "validate_faa_coverage",
                POSTGRES_FAA_COVERAGE_FUNCTION_SOURCE,
            ),
            (
                "faa_registry_coverage_immutable",
                27_i16,
                "faa_registry_coverage",
                "preserve_faa_registry_data",
                POSTGRES_FAA_IMMUTABILITY_FUNCTION_SOURCE,
            ),
            (
                "faa_registry_engine_references_immutable",
                27_i16,
                "faa_registry_engine_references",
                "preserve_faa_registry_data",
                POSTGRES_FAA_IMMUTABILITY_FUNCTION_SOURCE,
            ),
            (
                "faa_registry_engine_references_reachable",
                7_i16,
                "faa_registry_engine_references",
                "validate_faa_engine_reference_reachability",
                POSTGRES_FAA_ENGINE_REFERENCE_REACHABILITY_FUNCTION_SOURCE,
            ),
            (
                "faa_registry_snapshots_immutable",
                27_i16,
                "faa_registry_snapshots",
                "preserve_faa_registry_data",
                POSTGRES_FAA_IMMUTABILITY_FUNCTION_SOURCE,
            ),
            (
                "faa_registry_snapshots_require_exact_evidence",
                7_i16,
                "faa_registry_snapshots",
                "validate_faa_snapshot_evidence",
                POSTGRES_FAA_SNAPSHOT_EVIDENCE_FUNCTION_SOURCE,
            ),
        ];
        Ok(definitions.len() == expected.len()
            && definitions.iter().zip(expected).all(
                |(
                    actual,
                    (trigger_name, trigger_type, relation_name, function_name, function_source),
                )| {
                    actual.trigger_name == trigger_name
                        && actual.trigger_type == trigger_type
                        && actual.has_no_when_clause
                        && actual.has_no_update_columns
                        && actual.trigger_enabled == "O"
                        && actual.trigger_argument_count == 0
                        && actual.relation_schema == "public"
                        && actual.relation_name == relation_name
                        && actual.relation_oid_matches
                        && actual.function_name == function_name
                        && actual.function_schema == "public"
                        && actual.function_oid_matches
                        && canonical_sql_definition(&actual.function_source)
                            == canonical_sql_definition(function_source)
                        && actual.function_configuration == "search_path=pg_catalog"
                        && actual.function_language == "plpgsql"
                        && actual.returns_trigger
                        && actual.function_argument_count == 0
                        && actual.ordinary_function
                        && !actual.security_definer
                        && !actual.strict
                        && actual.volatility == "v"
                        && actual.parallel_safety == "u"
                },
            ))
    }

    async fn postgres_faa_registry_shape_problem(
        &self,
        pool: &mut PgConnection,
    ) -> Result<Option<String>> {
        let shape = sqlx::query_as::<_, PostgresFaaRegistryShape>(
            r#"
            SELECT
              (
                SELECT pg_catalog.string_agg(
                  pg_catalog.format(
                    '%s|%s|%s|%s|%s|%s|%s', relation.relname,
                    relation.relkind, relation.relpersistence,
                    relation.relrowsecurity, relation.relforcerowsecurity,
                    relation.relispartition, relation.relhasrules
                  ), E'\n' ORDER BY relation.relname
                )
                FROM pg_catalog.pg_class relation
                JOIN pg_catalog.pg_namespace relation_namespace
                  ON relation_namespace.oid = relation.relnamespace
                WHERE relation_namespace.nspname = 'public'
                  AND relation.relname IN (
                    'faa_registry_aircraft',
                    'faa_registry_aircraft_references',
                    'faa_registry_coverage',
                    'faa_registry_engine_references',
                    'faa_registry_snapshots'
                  )
              ) AS relation_signature,
              (
                SELECT pg_catalog.string_agg(
                  pg_catalog.format(
                    '%s|%s|%s|%s|%s|%s|%s', relation.relname,
                    attribute.attnum, attribute.attname,
                    pg_catalog.format_type(attribute.atttypid, attribute.atttypmod),
                    attribute.attnotnull, attribute.attidentity,
                    COALESCE(
                      pg_catalog.pg_get_expr(
                        attribute_default.adbin, attribute_default.adrelid
                      ), ''
                    )
                  ), E'\n' ORDER BY relation.relname, attribute.attnum
                )
                FROM pg_catalog.pg_class relation
                JOIN pg_catalog.pg_namespace relation_namespace
                  ON relation_namespace.oid = relation.relnamespace
                JOIN pg_catalog.pg_attribute attribute
                  ON attribute.attrelid = relation.oid
                 AND attribute.attnum > 0
                 AND NOT attribute.attisdropped
                LEFT JOIN pg_catalog.pg_attrdef attribute_default
                  ON attribute_default.adrelid = relation.oid
                 AND attribute_default.adnum = attribute.attnum
                WHERE relation_namespace.nspname = 'public'
                  AND relation.relname IN (
                    'faa_registry_aircraft',
                    'faa_registry_aircraft_references',
                    'faa_registry_coverage',
                    'faa_registry_engine_references',
                    'faa_registry_snapshots'
                  )
              ) AS column_signature,
              (
                SELECT pg_catalog.string_agg(
                  pg_catalog.format(
                    '%s|%s|%s|%s|%s', relation.relname,
                    constraint_row.contype, constraint_row.conname,
                    constraint_row.convalidated,
                    pg_catalog.pg_get_constraintdef(constraint_row.oid, FALSE)
                  ), E'\n' ORDER BY relation.relname,
                    constraint_row.contype, constraint_row.conname
                )
                FROM pg_catalog.pg_constraint constraint_row
                JOIN pg_catalog.pg_class relation
                  ON relation.oid = constraint_row.conrelid
                JOIN pg_catalog.pg_namespace relation_namespace
                  ON relation_namespace.oid = relation.relnamespace
                WHERE relation_namespace.nspname = 'public'
                  AND relation.relname IN (
                    'faa_registry_aircraft',
                    'faa_registry_aircraft_references',
                    'faa_registry_coverage',
                    'faa_registry_engine_references',
                    'faa_registry_snapshots'
                  )
                  AND constraint_row.contype <> 'f'
              ) AS constraint_signature,
              (
                SELECT pg_catalog.string_agg(
                  pg_catalog.format(
                    '%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s',
                    relation.relname,
                    constraint_namespace.nspname,
                    constraint_row.contype,
                    constraint_row.conname,
                    constraint_row.convalidated,
                    constraint_row.condeferrable,
                    constraint_row.condeferred,
                    constraint_row.connoinherit,
                    constraint_row.conislocal,
                    constraint_row.coninhcount,
                    referenced_namespace.nspname,
                    referenced_relation.relname,
                    referenced_index_namespace.nspname,
                    referenced_index.relname,
                    constraint_row.contypid = 0,
                    constraint_row.conparentid = 0,
                    constraint_row.confupdtype::text,
                    constraint_row.confdeltype::text,
                    constraint_row.confmatchtype::text,
                    COALESCE(constraint_row.conkey::text, ''),
                    COALESCE(constraint_row.confkey::text, ''),
                    COALESCE((
                      SELECT pg_catalog.string_agg(
                        operator_namespace.nspname || '.' || operator_row.oprname || '(' ||
                        left_namespace.nspname || '.' || left_type.typname || ',' ||
                        right_namespace.nspname || '.' || right_type.typname || ')',
                        ',' ORDER BY operator_key.ordinality
                      )
                      FROM pg_catalog.unnest(constraint_row.conpfeqop) WITH ORDINALITY
                        AS operator_key(operator_oid, ordinality)
                      JOIN pg_catalog.pg_operator operator_row
                        ON operator_row.oid = operator_key.operator_oid
                      JOIN pg_catalog.pg_namespace operator_namespace
                        ON operator_namespace.oid = operator_row.oprnamespace
                      JOIN pg_catalog.pg_type left_type
                        ON left_type.oid = operator_row.oprleft
                      JOIN pg_catalog.pg_namespace left_namespace
                        ON left_namespace.oid = left_type.typnamespace
                      JOIN pg_catalog.pg_type right_type
                        ON right_type.oid = operator_row.oprright
                      JOIN pg_catalog.pg_namespace right_namespace
                        ON right_namespace.oid = right_type.typnamespace
                    ), ''),
                    COALESCE((
                      SELECT pg_catalog.string_agg(
                        operator_namespace.nspname || '.' || operator_row.oprname || '(' ||
                        left_namespace.nspname || '.' || left_type.typname || ',' ||
                        right_namespace.nspname || '.' || right_type.typname || ')',
                        ',' ORDER BY operator_key.ordinality
                      )
                      FROM pg_catalog.unnest(constraint_row.conppeqop) WITH ORDINALITY
                        AS operator_key(operator_oid, ordinality)
                      JOIN pg_catalog.pg_operator operator_row
                        ON operator_row.oid = operator_key.operator_oid
                      JOIN pg_catalog.pg_namespace operator_namespace
                        ON operator_namespace.oid = operator_row.oprnamespace
                      JOIN pg_catalog.pg_type left_type
                        ON left_type.oid = operator_row.oprleft
                      JOIN pg_catalog.pg_namespace left_namespace
                        ON left_namespace.oid = left_type.typnamespace
                      JOIN pg_catalog.pg_type right_type
                        ON right_type.oid = operator_row.oprright
                      JOIN pg_catalog.pg_namespace right_namespace
                        ON right_namespace.oid = right_type.typnamespace
                    ), ''),
                    COALESCE((
                      SELECT pg_catalog.string_agg(
                        operator_namespace.nspname || '.' || operator_row.oprname || '(' ||
                        left_namespace.nspname || '.' || left_type.typname || ',' ||
                        right_namespace.nspname || '.' || right_type.typname || ')',
                        ',' ORDER BY operator_key.ordinality
                      )
                      FROM pg_catalog.unnest(constraint_row.conffeqop) WITH ORDINALITY
                        AS operator_key(operator_oid, ordinality)
                      JOIN pg_catalog.pg_operator operator_row
                        ON operator_row.oid = operator_key.operator_oid
                      JOIN pg_catalog.pg_namespace operator_namespace
                        ON operator_namespace.oid = operator_row.oprnamespace
                      JOIN pg_catalog.pg_type left_type
                        ON left_type.oid = operator_row.oprleft
                      JOIN pg_catalog.pg_namespace left_namespace
                        ON left_namespace.oid = left_type.typnamespace
                      JOIN pg_catalog.pg_type right_type
                        ON right_type.oid = operator_row.oprright
                      JOIN pg_catalog.pg_namespace right_namespace
                        ON right_namespace.oid = right_type.typnamespace
                    ), ''),
                    COALESCE(constraint_row.confdelsetcols::text, ''),
                    constraint_row.conexclop IS NULL,
                    constraint_row.conbin IS NULL,
                    ''
                  ), E'\n' ORDER BY relation.relname, constraint_row.conname
                )
                FROM pg_catalog.pg_constraint constraint_row
                JOIN pg_catalog.pg_namespace constraint_namespace
                  ON constraint_namespace.oid = constraint_row.connamespace
                JOIN pg_catalog.pg_class relation
                  ON relation.oid = constraint_row.conrelid
                JOIN pg_catalog.pg_namespace relation_namespace
                  ON relation_namespace.oid = relation.relnamespace
                JOIN pg_catalog.pg_class referenced_relation
                  ON referenced_relation.oid = constraint_row.confrelid
                JOIN pg_catalog.pg_namespace referenced_namespace
                  ON referenced_namespace.oid = referenced_relation.relnamespace
                JOIN pg_catalog.pg_class referenced_index
                  ON referenced_index.oid = constraint_row.conindid
                JOIN pg_catalog.pg_namespace referenced_index_namespace
                  ON referenced_index_namespace.oid = referenced_index.relnamespace
                WHERE relation_namespace.nspname = 'public'
                  AND relation.relname IN (
                    'faa_registry_aircraft',
                    'faa_registry_aircraft_references',
                    'faa_registry_coverage',
                    'faa_registry_engine_references',
                    'faa_registry_snapshots'
                  )
                  AND constraint_row.contype = 'f'
              ) AS foreign_key_signature,
              (
                SELECT pg_catalog.string_agg(
                  pg_catalog.format(
                    '%s|%s|%s|%s|%s|%s|%s', relation.relname,
                    index_relation.relname, index_row.indisunique,
                    index_row.indisprimary, index_row.indisvalid,
                    index_row.indisready,
                    pg_catalog.pg_get_indexdef(index_relation.oid)
                  ), E'\n' ORDER BY relation.relname, index_relation.relname
                )
                FROM pg_catalog.pg_index index_row
                JOIN pg_catalog.pg_class relation
                  ON relation.oid = index_row.indrelid
                JOIN pg_catalog.pg_namespace relation_namespace
                  ON relation_namespace.oid = relation.relnamespace
                JOIN pg_catalog.pg_class index_relation
                  ON index_relation.oid = index_row.indexrelid
                WHERE relation_namespace.nspname = 'public'
                  AND relation.relname IN (
                    'faa_registry_aircraft',
                    'faa_registry_aircraft_references',
                    'faa_registry_coverage',
                    'faa_registry_engine_references',
                    'faa_registry_snapshots'
                  )
              ) AS index_signature
            "#,
        )
        .fetch_one(&mut *pool)
        .await?;
        let Some(relation_signature) = shape.relation_signature else {
            return Ok(Some(String::from("PostgreSQL FAA registry relations")));
        };
        let Some(column_signature) = shape.column_signature else {
            return Ok(Some(String::from("PostgreSQL FAA registry columns")));
        };
        let Some(constraint_signature) = shape.constraint_signature else {
            return Ok(Some(String::from("PostgreSQL FAA registry constraints")));
        };
        let Some(foreign_key_signature) = shape.foreign_key_signature else {
            return Ok(Some(String::from("PostgreSQL FAA registry foreign keys")));
        };
        let Some(index_signature) = shape.index_signature else {
            return Ok(Some(String::from("PostgreSQL FAA registry indexes")));
        };
        for (object_class, signature, expected_fingerprint) in [
            (
                "PostgreSQL FAA registry relations",
                relation_signature,
                POSTGRES_FAA_RELATION_SHAPE_FINGERPRINT,
            ),
            (
                "PostgreSQL FAA registry columns",
                column_signature,
                POSTGRES_FAA_COLUMN_SHAPE_FINGERPRINT,
            ),
            (
                "PostgreSQL FAA registry constraints",
                constraint_signature,
                POSTGRES_FAA_CONSTRAINT_SHAPE_FINGERPRINT,
            ),
            (
                "PostgreSQL FAA registry foreign keys",
                foreign_key_signature,
                POSTGRES_FAA_FOREIGN_KEY_SHAPE_FINGERPRINT,
            ),
            (
                "PostgreSQL FAA registry indexes",
                index_signature,
                POSTGRES_FAA_INDEX_SHAPE_FINGERPRINT,
            ),
        ] {
            let fingerprint = format!("{:x}", Sha256::digest(format!("{signature}\n")));
            if fingerprint != expected_fingerprint {
                return Ok(Some(object_class.to_owned()));
            }
        }
        Ok(None)
    }

    async fn sqlite_faa_registry_definition_problem(
        &self,
        pool: &mut SqliteConnection,
    ) -> Result<Option<String>> {
        let foreign_keys = sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
            .fetch_one(&mut *pool)
            .await?;
        if foreign_keys != 1 {
            return Ok(Some(String::from("SQLite PRAGMA foreign_keys")));
        }
        let actual = sqlx::query_as::<_, SqliteSchemaDefinition>(
            r#"
            SELECT type AS object_type, name, sql
            FROM sqlite_schema
            WHERE name IN (
              'faa_registry_aircraft',
              'faa_registry_aircraft_references',
              'faa_registry_coverage',
              'faa_registry_engine_references',
              'faa_registry_snapshots',
              'idx_faa_registry_aircraft_code',
              'idx_faa_registry_aircraft_lineage_record',
              'idx_faa_registry_coverage_lookup',
              'idx_faa_registry_engine_code',
              'idx_faa_registry_snapshots_current',
              'faa_registry_aircraft_immutable_delete',
              'faa_registry_aircraft_immutable_update',
              'faa_registry_aircraft_references_immutable_delete',
              'faa_registry_aircraft_references_immutable_update',
              'faa_registry_aircraft_references_reachable',
              'faa_registry_coverage_consistent',
              'faa_registry_coverage_immutable_delete',
              'faa_registry_coverage_immutable_update',
              'faa_registry_engine_references_immutable_delete',
              'faa_registry_engine_references_immutable_update',
              'faa_registry_engine_references_reachable',
              'faa_registry_snapshots_immutable_delete',
              'faa_registry_snapshots_immutable_update',
              'faa_registry_snapshots_require_exact_evidence'
            )
            OR (
              type IN ('index', 'trigger')
              AND tbl_name IN (
                'faa_registry_aircraft',
                'faa_registry_aircraft_references',
                'faa_registry_coverage',
                'faa_registry_engine_references',
                'faa_registry_snapshots'
              )
              AND sql IS NOT NULL
            )
            ORDER BY type, name
            "#,
        )
        .fetch_all(&mut *pool)
        .await?;
        let mut expected = expected_sqlite_faa_registry_definitions();
        expected.sort_by(|left, right| {
            (&left.object_type, &left.name).cmp(&(&right.object_type, &right.name))
        });
        if actual.len() != expected.len() {
            return Ok(Some(String::from("SQLite FAA registry object set")));
        }
        for (actual, expected) in actual.iter().zip(expected) {
            if actual.object_type != expected.object_type
                || actual.name != expected.name
                || actual
                    .sql
                    .as_deref()
                    .map(canonical_sqlite_schema_definition)
                    != expected.sql
            {
                return Ok(Some(format!(
                    "SQLite FAA registry {} `{}`",
                    expected.object_type, expected.name
                )));
            }
        }
        Ok(None)
    }

    async fn faa_registry_contract_problem_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<Option<String>> {
        let structure_problem = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                self.sqlite_faa_registry_definition_problem(&mut **pool)
                    .await?
            }
            GateConnection::Postgres(pool) => {
                if let Some(problem) = self
                    .postgres_faa_registry_shape_problem(&mut **pool)
                    .await?
                {
                    return Ok(Some(problem));
                }
                if !self
                    .postgres_faa_registry_trigger_definitions_valid(&mut **pool)
                    .await?
                {
                    return Ok(Some(String::from(
                        "PostgreSQL FAA registry triggers or functions",
                    )));
                }
                None
            }
        };
        if structure_problem.is_some() {
            return Ok(structure_problem);
        }
        let mismatched_domain = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT EXISTS (SELECT 1 FROM faa_registry_snapshots \
                     WHERE record_hash_domain IS NOT ?)",
                )
                .bind(crate::aircraft::faa::AIRCRAFT_RECORD_HASH_DOMAIN)
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS (SELECT 1 FROM public.faa_registry_snapshots \
                     WHERE record_hash_domain IS DISTINCT FROM $1)",
                )
                .bind(crate::aircraft::faa::AIRCRAFT_RECORD_HASH_DOMAIN)
                .fetch_one(&mut **pool)
                .await?
            }
        };
        if mismatched_domain {
            return Ok(Some(String::from("FAA record hash domain values")));
        }
        Ok(None)
    }

    #[cfg(test)]
    async fn faa_registry_contract_valid(&self) -> Result<bool> {
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Sqlite(&mut connection);
                Ok(self
                    .faa_registry_contract_problem_on(&mut connection)
                    .await?
                    .is_none())
            }
            DatabaseBackend::Postgres(pool) => {
                let mut connection = pool.acquire().await?;
                let mut connection = GateConnection::Postgres(&mut connection);
                Ok(self
                    .faa_registry_contract_problem_on(&mut connection)
                    .await?
                    .is_none())
            }
        }
    }

    async fn faa_registry_schema_started_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<bool> {
        let objects_started = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1 FROM sqlite_schema
                      WHERE name IN (
                        'faa_registry_aircraft',
                        'faa_registry_aircraft_references',
                        'faa_registry_coverage',
                        'faa_registry_engine_references',
                        'faa_registry_snapshots',
                        'faa_registry_aircraft_references_reachable',
                        'faa_registry_engine_references_reachable'
                      )
                    )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
            SELECT
              pg_catalog.to_regclass('public.faa_registry_snapshots') IS NOT NULL
              OR pg_catalog.to_regclass('public.faa_registry_coverage') IS NOT NULL
              OR pg_catalog.to_regclass('public.faa_registry_aircraft') IS NOT NULL
              OR pg_catalog.to_regclass(
                'public.faa_registry_aircraft_references'
              ) IS NOT NULL
              OR pg_catalog.to_regclass(
                'public.faa_registry_engine_references'
              ) IS NOT NULL
              OR pg_catalog.to_regprocedure(
                'public.validate_faa_aircraft_reference_reachability()'
              ) IS NOT NULL
              OR pg_catalog.to_regprocedure(
                'public.validate_faa_engine_reference_reachability()'
              ) IS NOT NULL
              OR pg_catalog.to_regprocedure(
                'public.validate_faa_snapshot_evidence()'
              ) IS NOT NULL
              OR pg_catalog.to_regprocedure(
                'public.validate_faa_coverage()'
              ) IS NOT NULL
              OR pg_catalog.to_regprocedure(
                'public.preserve_faa_registry_data()'
              ) IS NOT NULL
              OR EXISTS (
                SELECT 1 FROM pg_catalog.pg_trigger
                WHERE NOT tgisinternal
                  AND tgname IN (
                    'faa_registry_snapshots_require_exact_evidence',
                    'faa_registry_aircraft_references_reachable',
                    'faa_registry_engine_references_reachable',
                    'faa_registry_coverage_consistent',
                    'faa_registry_snapshots_immutable',
                    'faa_registry_aircraft_immutable',
                    'faa_registry_aircraft_references_immutable',
                    'faa_registry_engine_references_immutable',
                    'faa_registry_coverage_immutable'
                  )
              )
            "#,
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        if objects_started {
            return Ok(true);
        }
        let GateConnection::Postgres(pool) = connection else {
            return Ok(false);
        };
        let ledger_exists = sqlx::query_scalar::<_, bool>(
            "SELECT pg_catalog.to_regclass('public.schema_migration_contracts') IS NOT NULL",
        )
        .fetch_one(&mut **pool)
        .await?;
        if !ledger_exists {
            return Ok(false);
        }
        Ok(sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
              SELECT 1 FROM ONLY public.schema_migration_contracts
              WHERE migration_name = $1
            )
            "#,
        )
        .bind(FAA_REFERENCE_REACHABILITY_MIGRATION)
        .fetch_one(&mut **pool)
        .await?)
    }

    async fn migration_contract_invalid_on(
        &self,
        connection: &mut GateConnection<'_>,
        anchor_object: &str,
        migration_name: &str,
        contract_version: i64,
        contract_fingerprint: &str,
    ) -> Result<bool> {
        Ok(self
            .migration_contract_state_on(
                connection,
                anchor_object,
                migration_name,
                contract_version,
                contract_fingerprint,
            )
            .await?
            == MigrationContractState::Invalid)
    }

    async fn migration_ledger_has_expected_shape_on(
        &self,
        connection: &mut GateConnection<'_>,
    ) -> Result<bool> {
        match &mut *connection {
            GateConnection::Sqlite(pool) => {
                let actual_definition = sqlx::query_scalar::<_, String>(
                    "SELECT sql FROM sqlite_schema \
                     WHERE type = 'table' AND name = 'schema_migration_contracts'",
                )
                .fetch_optional(&mut **pool)
                .await?;
                let expected_definition = canonical_sqlite_table_definition(
                    SQLITE_SCHEMA_SQL,
                    "schema_migration_contracts",
                )
                .expect("canonical SQLite schema must define the migration ledger");
                if !actual_definition.is_some_and(|definition| {
                    canonical_sql_definition(&definition) == expected_definition
                }) {
                    return Ok(false);
                }
                let attached_behavior = sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1 FROM sqlite_schema
                      WHERE tbl_name = 'schema_migration_contracts'
                        AND (
                          type = 'trigger'
                          OR (type = 'index' AND sql IS NOT NULL)
                        )
                      UNION ALL
                      SELECT 1 FROM sqlite_temp_schema
                      WHERE tbl_name = 'schema_migration_contracts'
                        AND type = 'trigger'
                    )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0;
                Ok(!attached_behavior)
            }
            GateConnection::Postgres(pool) => {
                let ordinary_table = sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1
                      FROM pg_catalog.pg_class relation
                      JOIN pg_catalog.pg_namespace namespace
                        ON namespace.oid = relation.relnamespace
                      WHERE namespace.nspname = 'public'
                        AND relation.relname = 'schema_migration_contracts'
                        AND relation.relkind = 'r'
                        AND relation.relpersistence = 'p'
                        AND NOT relation.relispartition
                        AND NOT relation.relrowsecurity
                        AND NOT relation.relforcerowsecurity
                        AND NOT EXISTS (
                          SELECT 1 FROM pg_catalog.pg_inherits inheritance
                          WHERE inheritance.inhrelid = relation.oid
                             OR inheritance.inhparent = relation.oid
                        )
                        AND NOT EXISTS (
                          SELECT 1 FROM pg_catalog.pg_trigger attached_trigger
                          WHERE attached_trigger.tgrelid = relation.oid
                            AND NOT attached_trigger.tgisinternal
                        )
                        AND NOT EXISTS (
                          SELECT 1 FROM pg_catalog.pg_rewrite attached_rule
                          WHERE attached_rule.ev_class = relation.oid
                        )
                        AND NOT EXISTS (
                          SELECT 1 FROM pg_catalog.pg_policy attached_policy
                          WHERE attached_policy.polrelid = relation.oid
                        )
                    )
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?;
                if !ordinary_table {
                    #[cfg(test)]
                    eprintln!("PostgreSQL migration ledger relation shape mismatch");
                    return Ok(false);
                }

                let actual_columns =
                    sqlx::query_as::<_, (String, String, bool, String, String, String, bool)>(
                        r#"
                    SELECT attribute.attname,
                           pg_catalog.format_type(attribute.atttypid, attribute.atttypmod),
                           attribute.attnotnull,
                           COALESCE(
                             pg_catalog.pg_get_expr(
                               default_value.adbin, default_value.adrelid
                             ),
                             ''
                           ),
                           attribute.attidentity::text,
                           attribute.attgenerated::text,
                           attribute.attcollation = CASE
                             WHEN attribute.atttypid = 'pg_catalog.text'::pg_catalog.regtype
                             THEN (
                               SELECT catalog_collation.oid
                               FROM pg_catalog.pg_collation catalog_collation
                               JOIN pg_catalog.pg_namespace collation_namespace
                                 ON collation_namespace.oid =
                                    catalog_collation.collnamespace
                               WHERE collation_namespace.nspname = 'pg_catalog'
                                 AND catalog_collation.collname = 'default'
                             )
                             ELSE 0::pg_catalog.oid
                           END
                    FROM pg_catalog.pg_attribute attribute
                    LEFT JOIN pg_catalog.pg_attrdef default_value
                      ON default_value.adrelid = attribute.attrelid
                     AND default_value.adnum = attribute.attnum
                    WHERE attribute.attrelid = pg_catalog.to_regclass(
                            'public.schema_migration_contracts'
                          )
                      AND attribute.attnum > 0
                      AND NOT attribute.attisdropped
                    ORDER BY attribute.attnum
                    "#,
                    )
                    .fetch_all(&mut **pool)
                    .await?
                    .into_iter()
                    .map(
                        |(
                            name,
                            data_type,
                            not_null,
                            default_expression,
                            identity_kind,
                            generated_kind,
                            canonical_collation,
                        )| {
                            (
                                name,
                                data_type,
                                not_null,
                                canonical_sql_definition(&default_expression),
                                identity_kind,
                                generated_kind,
                                canonical_collation,
                            )
                        },
                    )
                    .collect::<Vec<_>>();
                let expected_columns = vec![
                    (
                        "migration_name".to_owned(),
                        "text".to_owned(),
                        true,
                        String::new(),
                        String::new(),
                        String::new(),
                        true,
                    ),
                    (
                        "contract_version".to_owned(),
                        "integer".to_owned(),
                        true,
                        String::new(),
                        String::new(),
                        String::new(),
                        true,
                    ),
                    (
                        "contract_fingerprint".to_owned(),
                        "text".to_owned(),
                        true,
                        String::new(),
                        String::new(),
                        String::new(),
                        true,
                    ),
                    (
                        "installed_at".to_owned(),
                        "text".to_owned(),
                        true,
                        "current_timestamp".to_owned(),
                        String::new(),
                        String::new(),
                        true,
                    ),
                ];
                if actual_columns != expected_columns {
                    #[cfg(test)]
                    eprintln!("PostgreSQL migration ledger column mismatch: {actual_columns:?}");
                    return Ok(false);
                }

                let mut actual_constraints = sqlx::query_as::<_, (String, String, String, bool)>(
                    r#"
                    SELECT ledger_constraint.conname,
                           ledger_constraint.contype::text,
                           pg_catalog.pg_get_constraintdef(ledger_constraint.oid),
                           ledger_constraint.convalidated
                             AND NOT ledger_constraint.condeferrable
                             AND NOT ledger_constraint.condeferred
                             AND ledger_constraint.conparentid = 0
                             AND ledger_constraint.conislocal
                             AND ledger_constraint.coninhcount = 0
                             AND ledger_constraint.connoinherit =
                                   (ledger_constraint.contype = 'p')
                             AND ledger_constraint.connamespace = (
                               SELECT namespace.oid
                               FROM pg_catalog.pg_namespace namespace
                               WHERE namespace.nspname = 'public'
                             )
                             AND ledger_constraint.contypid = 0
                             AND ledger_constraint.confrelid = 0
                             AND CASE ledger_constraint.contype
                               WHEN 'p' THEN ledger_constraint.conindid =
                                 pg_catalog.to_regclass(
                                   'public.schema_migration_contracts_pkey'
                                 )
                               ELSE ledger_constraint.conindid = 0
                             END
                    FROM pg_catalog.pg_constraint ledger_constraint
                    WHERE ledger_constraint.conrelid = pg_catalog.to_regclass(
                            'public.schema_migration_contracts'
                          )
                    "#,
                )
                .fetch_all(&mut **pool)
                .await?
                .into_iter()
                .map(|(name, constraint_type, definition, flags_are_exact)| {
                    let definition = canonical_sql_definition(&definition)
                        .replace("trim(bothfrommigration_name)", "btrim(migration_name)");
                    (name, constraint_type, definition, flags_are_exact)
                })
                .collect::<Vec<_>>();
                actual_constraints.sort();
                let mut expected_constraints = vec![
                    (
                        "schema_migration_contracts_contract_version_check".to_owned(),
                        "c".to_owned(),
                        canonical_sql_definition("CHECK ((contract_version > 0))"),
                        true,
                    ),
                    (
                        "schema_migration_contracts_contract_fingerprint_check".to_owned(),
                        "c".to_owned(),
                        canonical_sql_definition(
                            "CHECK ((contract_fingerprint ~ '^[0-9a-f]{64}$'::text))",
                        ),
                        true,
                    ),
                    (
                        "schema_migration_contracts_migration_name_check".to_owned(),
                        "c".to_owned(),
                        canonical_sql_definition("CHECK ((length(btrim(migration_name)) > 0))"),
                        true,
                    ),
                    (
                        "schema_migration_contracts_pkey".to_owned(),
                        "p".to_owned(),
                        canonical_sql_definition("PRIMARY KEY (migration_name)"),
                        true,
                    ),
                ];
                expected_constraints.sort();
                if actual_constraints != expected_constraints {
                    #[cfg(test)]
                    eprintln!(
                        "PostgreSQL migration ledger constraint mismatch: {actual_constraints:?}"
                    );
                    return Ok(false);
                }

                let index_is_exact = sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT count(*) = 1 AND COALESCE(bool_and(
                      index_relation.relnamespace = ledger_relation.relnamespace
                      AND index_relation.relname = 'schema_migration_contracts_pkey'
                      AND index_relation.relkind = 'i'
                      AND index_relation.relpersistence = 'p'
                      AND index_relation.reltablespace = 0
                      AND index_relation.reloptions IS NULL
                      AND access_method.amname = 'btree'
                      AND index_row.indisunique
                      AND NOT index_row.indnullsnotdistinct
                      AND index_row.indisprimary
                      AND NOT index_row.indisexclusion
                      AND index_row.indimmediate
                      AND NOT index_row.indisclustered
                      AND index_row.indisvalid
                      AND NOT index_row.indcheckxmin
                      AND index_row.indisready
                      AND index_row.indislive
                      AND NOT index_row.indisreplident
                      AND index_row.indnatts = 1
                      AND index_row.indnkeyatts = 1
                      AND index_row.indexprs IS NULL
                      AND index_row.indpred IS NULL
                      AND index_row.indkey[0] = (
                        SELECT attribute.attnum
                        FROM pg_catalog.pg_attribute attribute
                        WHERE attribute.attrelid = ledger_relation.oid
                          AND attribute.attname = 'migration_name'
                          AND NOT attribute.attisdropped
                      )
                      AND index_row.indcollation[0] = (
                        SELECT catalog_collation.oid
                        FROM pg_catalog.pg_collation catalog_collation
                        JOIN pg_catalog.pg_namespace collation_namespace
                          ON collation_namespace.oid =
                             catalog_collation.collnamespace
                        WHERE collation_namespace.nspname = 'pg_catalog'
                          AND catalog_collation.collname = 'default'
                      )
                      AND index_row.indclass[0] = (
                        SELECT operator_class.oid
                        FROM pg_catalog.pg_opclass operator_class
                        JOIN pg_catalog.pg_namespace operator_namespace
                          ON operator_namespace.oid = operator_class.opcnamespace
                        WHERE operator_namespace.nspname = 'pg_catalog'
                          AND operator_class.opcname = 'text_ops'
                          AND operator_class.opcmethod = access_method.oid
                      )
                      AND index_row.indoption[0] = 0
                    ), FALSE)
                    FROM pg_catalog.pg_class ledger_relation
                    JOIN pg_catalog.pg_namespace ledger_namespace
                      ON ledger_namespace.oid = ledger_relation.relnamespace
                    LEFT JOIN pg_catalog.pg_index index_row
                      ON index_row.indrelid = ledger_relation.oid
                    LEFT JOIN pg_catalog.pg_class index_relation
                      ON index_relation.oid = index_row.indexrelid
                    LEFT JOIN pg_catalog.pg_am access_method
                      ON access_method.oid = index_relation.relam
                    WHERE ledger_namespace.nspname = 'public'
                      AND ledger_relation.relname = 'schema_migration_contracts'
                    "#,
                )
                .fetch_one(&mut **pool)
                .await?;
                #[cfg(test)]
                if !index_is_exact {
                    eprintln!("PostgreSQL migration ledger index mismatch");
                }
                Ok(index_is_exact)
            }
        }
    }

    async fn migration_contract_state_on(
        &self,
        connection: &mut GateConnection<'_>,
        anchor_object: &str,
        migration_name: &str,
        contract_version: i64,
        contract_fingerprint: &str,
    ) -> Result<MigrationContractState> {
        // An anchor and its exact receipt are one installation attestation.
        // Only their joint absence is fresh; every partial pairing is corrupt.
        let anchor_exists = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE name = ?)",
                )
                .bind(anchor_object)
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                let anchor_object = if anchor_object.contains('.') {
                    Cow::Borrowed(anchor_object)
                } else {
                    Cow::Owned(format!("public.{anchor_object}"))
                };
                sqlx::query_scalar::<_, bool>("SELECT pg_catalog.to_regclass($1::text) IS NOT NULL")
                    .bind(anchor_object.as_ref())
                    .fetch_one(&mut **pool)
                    .await?
            }
        };

        let ledger_exists = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_schema \
                     WHERE name = 'schema_migration_contracts')",
                )
                .fetch_one(&mut **pool)
                .await?
                    != 0
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    "SELECT pg_catalog.to_regclass( \
                     'public.schema_migration_contracts') IS NOT NULL",
                )
                .fetch_one(&mut **pool)
                .await?
            }
        };
        if !ledger_exists {
            return Ok(if anchor_exists {
                MigrationContractState::Invalid
            } else {
                MigrationContractState::Fresh
            });
        }

        let ledger_has_expected_shape = self
            .migration_ledger_has_expected_shape_on(connection)
            .await?;
        if !ledger_has_expected_shape {
            return Ok(MigrationContractState::Invalid);
        }

        let (receipt_exists, exact_contract_exists) = match &mut *connection {
            GateConnection::Sqlite(pool) => {
                let (receipt_exists, exact_contract_exists) = sqlx::query_as::<_, (i64, i64)>(
                    r#"
                    SELECT
                      EXISTS (
                        SELECT 1 FROM schema_migration_contracts
                        WHERE migration_name = ?
                      ),
                      EXISTS (
                        SELECT 1 FROM schema_migration_contracts
                        WHERE migration_name = ?
                          AND contract_version = ?
                          AND contract_fingerprint = ?
                      )
                    "#,
                )
                .bind(migration_name)
                .bind(migration_name)
                .bind(contract_version)
                .bind(contract_fingerprint)
                .fetch_one(&mut **pool)
                .await?;
                (receipt_exists != 0, exact_contract_exists != 0)
            }
            GateConnection::Postgres(pool) => {
                sqlx::query_as::<_, (bool, bool)>(
                    r#"
                    SELECT
                      EXISTS (
                        SELECT 1 FROM ONLY public.schema_migration_contracts
                        WHERE migration_name = $1
                      ),
                      EXISTS (
                        SELECT 1 FROM ONLY public.schema_migration_contracts
                        WHERE migration_name = $1
                          AND contract_version = $2
                          AND contract_fingerprint = $3
                      )
                    "#,
                )
                .bind(migration_name)
                .bind(contract_version)
                .bind(contract_fingerprint)
                .fetch_one(&mut **pool)
                .await?
            }
        };
        Ok(
            match (anchor_exists, receipt_exists, exact_contract_exists) {
                (false, false, false) => MigrationContractState::Fresh,
                (true, true, true) => MigrationContractState::Installed,
                _ => MigrationContractState::Invalid,
            },
        )
    }

    async fn initialize_transactionally(&self) -> Result<()> {
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                let mut transaction = pool
                    .begin_with("BEGIN IMMEDIATE")
                    .await
                    .context("could not begin serialized SQLite schema initialization")?;
                let initialization = async {
                    let mut gate_connection = GateConnection::Sqlite(&mut transaction);
                    self.ensure_required_migrations_on(&mut gate_connection)
                        .await?;
                    for statement in split_sql_statements(SQLITE_SCHEMA_SQL) {
                        (&mut *transaction).execute(statement).await?;
                    }
                    sqlx::query(
                        r#"
                        INSERT INTO users (
                          email, display_name, auth_provider, auth_subject
                        ) VALUES (?, ?, ?, ?)
                        ON CONFLICT (auth_subject) DO NOTHING
                        "#,
                    )
                    .bind(DEVELOPER_EMAIL)
                    .bind("Developer")
                    .bind("local")
                    .bind(DEVELOPER_AUTH_SUBJECT)
                    .execute(&mut *transaction)
                    .await?;
                    let mut gate_connection = GateConnection::Sqlite(&mut transaction);
                    self.ensure_required_migrations_on(&mut gate_connection)
                        .await?;
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                match initialization {
                    Ok(()) => transaction
                        .commit()
                        .await
                        .context("could not commit SQLite schema initialization"),
                    Err(error) => {
                        if let Err(rollback_error) = transaction.rollback().await {
                            return Err(error.context(format!(
                                "SQLite schema initialization rollback failed: {rollback_error}"
                            )));
                        }
                        Err(error)
                    }
                }
            }
            DatabaseBackend::Postgres(pool) => {
                let mut connection = pool.acquire().await?;
                // A failed unlock or broken initialization session must never
                // return a connection that may still own the session lock.
                connection.close_on_drop();
                sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
                    .bind(POSTGRES_STARTUP_ADVISORY_LOCK_KEY)
                    .execute(&mut *connection)
                    .await
                    .context("could not serialize PostgreSQL schema initialization")?;
                let ledger_exists = sqlx::query_scalar::<_, bool>(
                    "SELECT pg_catalog.to_regclass( \
                           'public.schema_migration_contracts' \
                         ) IS NOT NULL",
                )
                .fetch_one(&mut *connection)
                .await
                .context("could not inspect PostgreSQL migration receipt ledger")?;
                let mut transaction = connection
                    .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
                    .await
                    .context("could not begin PostgreSQL schema initialization transaction")?;
                let initialization = async {
                    transaction
                        .execute("SET LOCAL search_path = public, pg_catalog, pg_temp")
                        .await?;
                    if ledger_exists {
                        transaction
                            .execute(
                                "LOCK TABLE public.schema_migration_contracts \
                             IN SHARE ROW EXCLUSIVE MODE",
                            )
                            .await
                            .context("could not lock PostgreSQL migration receipt ledger")?;
                    }
                    let mut gate_connection = GateConnection::Postgres(&mut transaction);
                    self.ensure_required_migrations_on(&mut gate_connection)
                        .await?;
                    for statement in split_sql_statements(POSTGRES_SCHEMA_SQL) {
                        (&mut *transaction).execute(statement).await?;
                    }
                    sqlx::query(
                        r#"
                        INSERT INTO public.users (
                          email, display_name, auth_provider, auth_subject
                        ) VALUES ($1, $2, $3, $4)
                        ON CONFLICT (auth_subject) DO NOTHING
                        "#,
                    )
                    .bind(DEVELOPER_EMAIL)
                    .bind("Developer")
                    .bind("local")
                    .bind(DEVELOPER_AUTH_SUBJECT)
                    .execute(&mut *transaction)
                    .await?;
                    let mut gate_connection = GateConnection::Postgres(&mut transaction);
                    self.ensure_required_migrations_on(&mut gate_connection)
                        .await?;
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                let transaction_result = match initialization {
                    Ok(()) => transaction
                        .commit()
                        .await
                        .context("could not commit PostgreSQL schema initialization"),
                    Err(error) => match transaction.rollback().await {
                        Ok(()) => Err(error),
                        Err(rollback_error) => Err(error.context(format!(
                            "PostgreSQL schema initialization rollback failed: \
                                 {rollback_error}"
                        ))),
                    },
                };
                let unlock_result =
                    sqlx::query_scalar::<_, bool>("SELECT pg_catalog.pg_advisory_unlock($1)")
                        .bind(POSTGRES_STARTUP_ADVISORY_LOCK_KEY)
                        .fetch_one(&mut *connection)
                        .await
                        .context("could not release PostgreSQL schema initialization lock")
                        .and_then(|unlocked| {
                            if unlocked {
                                Ok(())
                            } else {
                                bail!("PostgreSQL schema initialization lock was not owned")
                            }
                        });
                match (transaction_result, unlock_result) {
                    (Ok(()), Ok(())) => Ok(()),
                    (Err(error), Ok(())) => Err(error),
                    (Ok(()), Err(error)) => Err(error),
                    (Err(error), Err(unlock_error)) => Err(error.context(format!(
                        "PostgreSQL schema initialization unlock failed: {unlock_error}"
                    ))),
                }
            }
        }
    }

    #[cfg(test)]
    async fn initialize(&self) -> Result<()> {
        self.initialize_transactionally().await
    }
}

pub fn database_url_from_arg(value: Option<String>) -> String {
    value
        .map(|value| {
            if is_database_url(&value) {
                value
            } else {
                sqlite_url_for_path(PathBuf::from(value))
            }
        })
        .unwrap_or_else(|| {
            std::env::var("AIRCOST_DATABASE_URL")
                .unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
        })
}

pub fn sqlite_database_urls_equal(left: &str, right: &str) -> Result<bool> {
    fn identity(value: &str) -> Result<PathBuf> {
        let value = normalize_database_url(value);
        if value == "sqlite::memory:" || is_postgres_url(&value) {
            bail!("clean replay requires two distinct file-backed SQLite databases");
        }
        let path = value
            .strip_prefix("sqlite://")
            .context("clean replay database URL must be a file-backed SQLite URL")?;
        let path = PathBuf::from(path);
        if path.exists() {
            return path.canonicalize().with_context(|| {
                format!("could not canonicalize database path {}", path.display())
            });
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let file_name = path
            .file_name()
            .context("SQLite target path has no file name")?;
        Ok(parent
            .canonicalize()
            .with_context(|| {
                format!(
                    "could not canonicalize database parent {}",
                    parent.display()
                )
            })?
            .join(file_name))
    }
    Ok(identity(left)? == identity(right)?)
}

fn normalize_database_url(value: &str) -> String {
    if is_database_url(value) {
        value.to_string()
    } else {
        sqlite_url_for_path(PathBuf::from(value))
    }
}

fn sqlite_url_for_path(path: PathBuf) -> String {
    if path == Path::new(":memory:") {
        "sqlite::memory:".to_string()
    } else {
        format!("sqlite://{}", path.to_string_lossy())
    }
}

fn is_database_url(value: &str) -> bool {
    value.starts_with("sqlite:")
        || value.starts_with("postgres:")
        || value.starts_with("postgresql:")
}

fn is_postgres_url(value: &str) -> bool {
    value.starts_with("postgres:") || value.starts_with("postgresql:")
}

fn ensure_sqlite_parent_directory(database_url: &str) -> Result<()> {
    if database_url == "sqlite::memory:" {
        return Ok(());
    }
    let Some(path) = database_url.strip_prefix("sqlite://") else {
        return Ok(());
    };
    let path = Path::new(path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create database directory {parent:?}"))?;
    }
    Ok(())
}

/// Split the checked-in schema files without breaking trigger bodies, quoted
/// strings, or PostgreSQL dollar-quoted function definitions.
fn split_sql_statements(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut index = 0;
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut dollar_quote: Option<String> = None;

    while index < bytes.len() {
        if line_comment {
            if bytes[index] == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(delimiter) = dollar_quote.as_deref() {
            if bytes[index..].starts_with(delimiter.as_bytes()) {
                index += delimiter.len();
                dollar_quote = None;
            } else {
                index += 1;
            }
            continue;
        }
        if single_quoted {
            if bytes[index] == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                } else {
                    single_quoted = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }
        if double_quoted {
            if bytes[index] == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    index += 2;
                } else {
                    double_quoted = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }

        if bytes[index] == b'-' && bytes.get(index + 1) == Some(&b'-') {
            line_comment = true;
            index += 2;
            continue;
        }
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            block_comment = true;
            index += 2;
            continue;
        }
        if bytes[index] == b'\'' {
            single_quoted = true;
            index += 1;
            continue;
        }
        if bytes[index] == b'"' {
            double_quoted = true;
            index += 1;
            continue;
        }
        if bytes[index] == b'$' {
            if let Some(delimiter) = dollar_quote_delimiter(&sql[index..]) {
                index += delimiter.len();
                dollar_quote = Some(delimiter.to_string());
                continue;
            }
        }
        if bytes[index] == b';' {
            let candidate = sql[start..index].trim();
            if !candidate.is_empty() && !sqlite_trigger_body_is_open(candidate) {
                statements.push(candidate);
                start = index + 1;
            }
        }
        index += 1;
    }

    let trailing = sql[start..].trim();
    if !trailing.is_empty() {
        statements.push(trailing);
    }
    statements
}

fn dollar_quote_delimiter(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'$') {
        return None;
    }
    let end = bytes[1..].iter().position(|byte| *byte == b'$')? + 1;
    if bytes[1..end]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        Some(&value[..=end])
    } else {
        None
    }
}

fn sqlite_trigger_body_is_open(statement: &str) -> bool {
    let statement = strip_leading_sql_comments(statement);
    let uppercase = statement.to_ascii_uppercase();
    let mut words = uppercase
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|word| !word.is_empty());
    if words.next() != Some("CREATE") {
        return false;
    }
    let second = words.next();
    let trigger = if second == Some("TEMP") || second == Some("TEMPORARY") {
        words.next()
    } else {
        second
    };
    trigger == Some("TRIGGER")
        && words.any(|word| word == "BEGIN")
        && !uppercase.trim_end().ends_with("END")
}

fn strip_leading_sql_comments(mut value: &str) -> &str {
    loop {
        value = value.trim_start();
        if let Some(line_comment) = value.strip_prefix("--") {
            value = line_comment
                .find('\n')
                .map(|newline| &line_comment[newline + 1..])
                .unwrap_or("");
            continue;
        }
        if let Some(block_comment) = value.strip_prefix("/*") {
            value = block_comment
                .find("*/")
                .map(|end| &block_comment[end + 2..])
                .unwrap_or("");
            continue;
        }
        return value;
    }
}

fn postgres_placeholders(sql: &str) -> String {
    let mut next_placeholder = 1_usize;
    let mut converted = String::with_capacity(sql.len());
    for character in sql.chars() {
        if character == '?' {
            converted.push('$');
            converted.push_str(&next_placeholder.to_string());
            next_placeholder += 1;
        } else {
            converted.push(character);
        }
    }
    converted
}

fn migration_required_message(
    kind: DatabaseKind,
    table: &str,
    column: &str,
    migration: &str,
) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing `{table}` is missing `{column}`; \
         back up the database, apply `migrations/{migration}.{backend}.sql`, then restart aircost"
    )
}

fn avionics_multi_type_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing avionics catalog must use the \
         `avionics_model_types` capability table without scalar `avionics_models.avionics_type_id`; \
         back up the database, apply `migrations/{AVIONICS_MULTI_TYPE_MIGRATION}.{backend}.sql`, \
         then restart aircost"
    )
}

fn aircraft_reference_catalog_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing aircraft data is missing the clean \
         aircraft identity/reference catalogs or FAA registry projection; back up the \
         database, apply `migrations/{AIRCRAFT_REFERENCE_CATALOG_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn reference_catalog_cutover_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: the immutable aircraft reference catalog is \
         missing the canonical price-basis and complete-fact-set contract; back up the database, \
         apply `migrations/{REFERENCE_CATALOG_CUTOVER_MIGRATION}.{backend}.sql`, then restart aircost"
    )
}

fn listing_pending_reviews_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing listing data is missing the \
         pending-review handoff or `pending_review` ingestion state; back up the database, apply \
         `migrations/{LISTING_PENDING_REVIEWS_MIGRATION}.{backend}.sql`, then restart aircost"
    )
}

fn identity_deduplication_postconditions_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing avionics data is missing the \
         canonical approved-identity registry or guarded consolidation postconditions; back up \
         the database, apply \
         `migrations/{IDENTITY_DEDUPLICATION_POSTCONDITIONS_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn listing_aircraft_identity_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing listing data is missing immutable \
         FAA-backed aircraft identity assignments; back up the database, apply \
         `migrations/{LISTING_AIRCRAFT_IDENTITY_MIGRATION}.{backend}.sql`, then restart aircost"
    )
}

fn listing_aircraft_compatibility_projection_migration_required_message(
    kind: DatabaseKind,
) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing listing data is missing the \
         immutable FAA-backed aircraft compatibility projection contract; back up the database, \
         apply `migrations/{LISTING_AIRCRAFT_COMPATIBILITY_PROJECTION_MIGRATION}.{backend}.sql`, \
         then restart aircost"
    )
}

fn aircraft_identity_no_supported_selection_migration_required_message(
    kind: DatabaseKind,
) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing aircraft identity decisions still \
         use the legacy optional-dimension rejection contract; back up the database, apply \
         `migrations/{AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn aircraft_catalog_retrieval_keys_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing canonical aircraft catalog has not \
         completed the deterministic retrieval-key data repair and validation contract; back up \
         the database, apply \
         `migrations/{AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION}.{backend}.sql`, then restart \
         aircost"
    )
}

fn aircraft_tcds_make_lineage_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: the canonical aircraft catalog is missing \
         the immutable FAA/TCDS make-lineage contract; back up the database, apply \
         `migrations/{AIRCRAFT_TCDS_MAKE_LINEAGE_MIGRATION}.{backend}.sql`, then restart aircost"
    )
}

fn avionics_human_reviewed_consolidation_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: the avionics catalog is missing the \
         evidence-backed human-review consolidation contract; back up the database, apply \
         `migrations/{AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn avionics_descriptive_consolidation_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: the avionics catalog is missing the \
         complete descriptive-equivalent human-consolidation contract; back up the database, \
         apply `migrations/{AVIONICS_DESCRIPTIVE_CONSOLIDATION_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn avionics_grounded_exact_model_consolidation_migration_required_message(
    kind: DatabaseKind,
) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: the avionics catalog is missing the \
         grounded exact-model duplicate consolidation contract; back up the database, apply \
         `migrations/{AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_MIGRATION}.{backend}.sql`, \
         then restart aircost"
    )
}

fn avionics_authoritative_source_origins_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: the avionics catalog is missing immutable \
         exact-origin authority approvals or auditable revocations; back up the database, apply \
         `migrations/{AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn avionics_product_reuse_attestations_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: approved avionics products must use the \
         target-aware current-policy reuse-attestation gate; back up the database, apply \
         `migrations/{AVIONICS_PRODUCT_REUSE_V2_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn avionics_grounded_evidence_refresh_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: freshly grounded approved avionics evidence \
         must be refreshed atomically before reuse attestation; back up the database, apply \
         `migrations/{AVIONICS_GROUNDED_EVIDENCE_REFRESH_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn listing_avionics_grounded_capabilities_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: capture-bound grounded avionics resolutions \
         must survive listing retries as one-use capabilities; back up the database, apply \
         `migrations/{LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn aircraft_listing_identity_corrections_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: existing listing data is missing immutable \
         aircraft identity correction decisions; back up the database, apply \
         `migrations/{AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn aircraft_visual_source_corrections_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: signed visual aircraft corrections need an \
         immutable pinned artifact and receipt gate; back up the database, apply \
         `migrations/{AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_MIGRATION}.{backend}.sql`, then restart \
         aircost"
    )
}

fn faa_registry_contract_required_message(kind: DatabaseKind, problem: &str) -> String {
    match kind {
        DatabaseKind::Sqlite => format!(
            "database migration required before startup: {problem} does not match the canonical \
             FAA projection contract; restore the database from a verified backup before \
             restarting aircost"
        ),
        DatabaseKind::Postgres => format!(
            "database migration required before startup: {problem} does not match the exact \
             namespace-locked FAA projection contract; back up the database, apply \
             `migrations/{FAA_REFERENCE_REACHABILITY_MIGRATION}.postgres.sql`, then restart \
             aircost"
        ),
    }
}

fn faa_record_hash_domain_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: FAA source-record hashes need an explicit \
         immutable domain; a nonempty legacy FAA projection must be discarded and regenerated \
         from its exact release archive, then apply \
         `migrations/{FAA_RECORD_HASH_DOMAIN_MIGRATION}.{backend}.sql` and restart aircost"
    )
}

fn listing_replay_runs_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: apply \
         `migrations/{LISTING_REPLAY_RUNS_MIGRATION}.{backend}.sql`, then restart aircost"
    )
}

pub fn ensure_supported_database_url(database_url: &str) -> Result<()> {
    if is_database_url(database_url) || !database_url.trim().is_empty() {
        Ok(())
    } else {
        bail!("database URL cannot be empty")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use sha2::{Digest, Sha256};
    use sqlx::postgres::PgPoolOptions;
    use sqlx::sqlite::{SqliteConnection, SqlitePoolOptions};
    use sqlx::{Connection, Executor};

    use super::{
        aircraft_catalog_retrieval_keys_migration_required_message,
        aircraft_identity_no_supported_selection_migration_required_message,
        aircraft_reference_catalog_migration_required_message,
        aircraft_tcds_make_lineage_migration_required_message,
        avionics_authoritative_source_origins_migration_required_message,
        avionics_descriptive_consolidation_migration_required_message,
        avionics_multi_type_migration_required_message,
        avionics_product_reuse_attestations_migration_required_message,
        canonical_postgres_named_trigger_definition, canonical_sql_definition,
        canonical_sqlite_named_definition, canonical_startup_migration_contract_receipts,
        canonical_table_definition, faa_record_hash_domain_migration_required_message,
        faa_registry_contract_required_message,
        identity_deduplication_postconditions_migration_required_message,
        listing_aircraft_compatibility_projection_migration_required_message,
        listing_aircraft_identity_migration_required_message,
        listing_pending_reviews_migration_required_message, migration_required_message,
        postgres_approved_concrete_model_object_payload, postgres_function_source,
        postgres_reference_owned_objects_query, split_sql_statements, sqlite_migration_definition,
        sqlite_table_definition, versioned_avionics_approved_concrete_model_object_fingerprint,
        AppDb, DatabaseBackend, DatabaseKind, PostgresApprovedConcreteModelTriggerDefinition,
        AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_FINGERPRINT,
        AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION,
        AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION,
        AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_FINGERPRINT,
        AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_VERSION,
        AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_MIGRATION,
        AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_FINGERPRINT,
        AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_VERSION, AIRCRAFT_TCDS_MAKE_LINEAGE_MIGRATION,
        AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_FINGERPRINT,
        AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_VERSION,
        AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_MIGRATION,
        AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_FINGERPRINT,
        AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_VERSION,
        AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_MIGRATION, AVIONICS_CATALOG_CURATION_MIGRATION,
        AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_FINGERPRINT,
        AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_VERSION,
        AVIONICS_DESCRIPTIVE_CONSOLIDATION_MIGRATION,
        AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_FINGERPRINT,
        AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_VERSION,
        AVIONICS_GENERIC_FEATURE_LABELS_MIGRATION,
        AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_FINGERPRINT,
        AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_VERSION,
        AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_MIGRATION,
        AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_FINGERPRINT,
        AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_VERSION,
        AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_MIGRATION,
        AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_FINGERPRINT,
        AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_VERSION,
        AVIONICS_PRODUCT_REUSE_ATTESTATIONS_MIGRATION,
        AVIONICS_PRODUCT_REUSE_V2_CONTRACT_FINGERPRINT, AVIONICS_PRODUCT_REUSE_V2_CONTRACT_VERSION,
        AVIONICS_PRODUCT_REUSE_V2_MIGRATION, FAA_RECORD_HASH_DOMAIN_CONTRACT_FINGERPRINT,
        FAA_RECORD_HASH_DOMAIN_CONTRACT_VERSION, FAA_RECORD_HASH_DOMAIN_MIGRATION,
        FAA_REFERENCE_REACHABILITY_CONTRACT_FINGERPRINT,
        FAA_REFERENCE_REACHABILITY_CONTRACT_VERSION, FAA_REFERENCE_REACHABILITY_MIGRATION,
        IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_FINGERPRINT,
        LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_FINGERPRINT,
        LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_VERSION,
        LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION, LISTING_REPLAY_RUNS_CONTRACT_FINGERPRINT,
        LISTING_REPLAY_RUNS_CONTRACT_VERSION, LISTING_REPLAY_RUNS_MIGRATION,
        POSTGRES_AVIONICS_APPROVED_CONCRETE_MODEL_FUNCTION_SOURCE,
        POSTGRES_AVIONICS_APPROVED_CONCRETE_MODEL_OBJECT_CONTRACT_FINGERPRINT,
        POSTGRES_CORRECTION_DECISION_FUNCTION_SOURCE,
        POSTGRES_FAA_AIRCRAFT_REFERENCE_REACHABILITY_FUNCTION_SOURCE,
        POSTGRES_FAA_COVERAGE_FUNCTION_SOURCE,
        POSTGRES_FAA_ENGINE_REFERENCE_REACHABILITY_FUNCTION_SOURCE,
        POSTGRES_FAA_IMMUTABILITY_FUNCTION_SOURCE, POSTGRES_FAA_SNAPSHOT_EVIDENCE_FUNCTION_SOURCE,
        POSTGRES_SCHEMA_SQL, POSTGRES_SEARCH_PATH, REFERENCE_CATALOG_CUTOVER_MIGRATION,
        REFERENCE_CATALOG_CUTOVER_POSTGRES_MIGRATION_SQL, SQLITE_SCHEMA_SQL,
        VALUATION_DATA_HARDENING_MIGRATION,
    };

    const LISTING_PENDING_REVIEWS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260724_listing_pending_reviews.sqlite.sql");
    const LISTING_PENDING_REVIEWS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260724_listing_pending_reviews.postgres.sql");
    const IDENTITY_POSTCONDITIONS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260725_identity_deduplication_postconditions.sqlite.sql");
    const IDENTITY_POSTCONDITIONS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260725_identity_deduplication_postconditions.postgres.sql");
    const AIRCRAFT_CATALOG_RETRIEVAL_KEYS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260729_aircraft_catalog_retrieval_keys.sqlite.sql");
    const AIRCRAFT_CATALOG_RETRIEVAL_KEYS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260729_aircraft_catalog_retrieval_keys.postgres.sql");
    const AIRCRAFT_TCDS_MAKE_LINEAGE_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260730_aircraft_tcds_make_lineage.sqlite.sql");
    const AIRCRAFT_TCDS_MAKE_LINEAGE_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260730_aircraft_tcds_make_lineage.postgres.sql");
    const FAA_REFERENCE_REACHABILITY_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260819_faa_reference_reachability.postgres.sql");
    const FAA_RECORD_HASH_DOMAIN_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260820_faa_record_hash_domain.sqlite.sql");
    const FAA_RECORD_HASH_DOMAIN_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260820_faa_record_hash_domain.postgres.sql");
    const AVIONICS_APPROVED_CONCRETE_MODEL_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260821_avionics_approved_concrete_model.sqlite.sql");
    const AVIONICS_APPROVED_CONCRETE_MODEL_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260821_avionics_approved_concrete_model.postgres.sql");
    const AVIONICS_GENERIC_FEATURE_LABELS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260824_avionics_generic_feature_labels.sqlite.sql");
    const AVIONICS_GENERIC_FEATURE_LABELS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260824_avionics_generic_feature_labels.postgres.sql");
    const AVIONICS_HUMAN_CONSOLIDATION_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260731_avionics_human_reviewed_consolidation.sqlite.sql");
    const AVIONICS_HUMAN_CONSOLIDATION_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260731_avionics_human_reviewed_consolidation.postgres.sql");
    const AVIONICS_DESCRIPTIVE_CONSOLIDATION_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260808_avionics_descriptive_consolidation.sqlite.sql");
    const AVIONICS_DESCRIPTIVE_CONSOLIDATION_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260808_avionics_descriptive_consolidation.postgres.sql");
    const AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_SQLITE_MIGRATION_SQL: &str = include_str!(
        "../migrations/20260810_avionics_grounded_exact_model_consolidation.sqlite.sql"
    );
    const AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_POSTGRES_MIGRATION_SQL: &str = include_str!(
        "../migrations/20260810_avionics_grounded_exact_model_consolidation.postgres.sql"
    );
    const AVIONICS_SOURCE_ORIGINS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260801_avionics_authoritative_source_origins.sqlite.sql");
    const AVIONICS_SOURCE_ORIGINS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260801_avionics_authoritative_source_origins.postgres.sql");
    const AVIONICS_REUSE_ATTESTATIONS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260803_avionics_product_reuse_attestations.sqlite.sql");
    const AVIONICS_REUSE_ATTESTATIONS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260803_avionics_product_reuse_attestations.postgres.sql");
    const AVIONICS_REUSE_V2_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260807_avionics_product_reuse_v2.sqlite.sql");
    const AVIONICS_REUSE_V2_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260807_avionics_product_reuse_v2.postgres.sql");
    const LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260825_listing_avionics_grounded_capabilities.sqlite.sql");
    const LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260825_listing_avionics_grounded_capabilities.postgres.sql");
    const AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260819_aircraft_listing_identity_corrections.sqlite.sql");
    const AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260819_aircraft_listing_identity_corrections.postgres.sql");
    const AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260821_aircraft_visual_source_corrections.sqlite.sql");
    const AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260821_aircraft_visual_source_corrections.postgres.sql");
    const LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260819_listing_replay_runs.sqlite.sql");
    const LISTING_REPLAY_RUNS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260819_listing_replay_runs.postgres.sql");
    async fn sqlite_db_with_statements(statements: &[&str]) -> AppDb {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("SQLite test database should connect");
        for statement in statements {
            pool.execute(*statement)
                .await
                .expect("legacy test schema should be created");
        }
        AppDb {
            backend: DatabaseBackend::Sqlite(pool),
        }
    }

    fn unique_sqlite_test_database(label: &str) -> (PathBuf, String) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should be after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aircost-{label}-{}-{nonce}.sqlite3",
            std::process::id()
        ));
        let url = format!("sqlite://{}", path.display());
        (path, url)
    }

    fn connect_error(result: anyhow::Result<AppDb>) -> String {
        match result {
            Ok(_) => panic!("database startup unexpectedly succeeded"),
            Err(error) => format!("{error:#}"),
        }
    }

    fn canonical_receipt_statements(schema: &str) -> Vec<&str> {
        split_sql_statements(schema)
            .into_iter()
            .filter(|statement| {
                let canonical = canonical_sql_definition(statement);
                canonical.contains("insertintoschema_migration_contracts")
                    || canonical.contains("insertintopublic.schema_migration_contracts")
            })
            .collect()
    }

    fn canonical_receipt_names(schema: &str) -> BTreeSet<String> {
        canonical_receipt_statements(schema)
            .into_iter()
            .flat_map(|statement| {
                statement
                    .split('\'')
                    .skip(1)
                    .step_by(2)
                    .filter(|value| {
                        value.len() > 9
                            && value.as_bytes()[..8].iter().all(u8::is_ascii_digit)
                            && value.as_bytes()[8] == b'_'
                    })
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn canonical_receipts_are_insert_only_and_have_backend_parity() {
        for (backend, schema) in [
            ("SQLite", SQLITE_SCHEMA_SQL),
            ("PostgreSQL", POSTGRES_SCHEMA_SQL),
        ] {
            for statement in canonical_receipt_statements(schema) {
                let canonical = canonical_sql_definition(statement);
                assert!(
                    canonical.contains("onconflict(migration_name)donothing"),
                    "{backend} canonical receipt is not insert-only: {statement}"
                );
                assert!(
                    !canonical.contains("doupdate") && !canonical.contains("installed_at="),
                    "{backend} canonical receipt can rewrite provenance: {statement}"
                );
            }
        }

        let sqlite_receipts = canonical_receipt_names(SQLITE_SCHEMA_SQL);
        let postgres_receipts = canonical_receipt_names(POSTGRES_SCHEMA_SQL);
        let postgres_only = postgres_receipts
            .difference(&sqlite_receipts)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            postgres_only,
            BTreeSet::from([FAA_REFERENCE_REACHABILITY_MIGRATION.to_owned()])
        );
        assert!(sqlite_receipts
            .difference(&postgres_receipts)
            .next()
            .is_none());
        let expected_sqlite = canonical_startup_migration_contract_receipts(DatabaseKind::Sqlite)
            .into_iter()
            .map(|receipt| receipt.migration_name.to_string())
            .collect::<BTreeSet<_>>();
        let expected_postgres =
            canonical_startup_migration_contract_receipts(DatabaseKind::Postgres)
                .into_iter()
                .map(|receipt| receipt.migration_name.to_string())
                .collect::<BTreeSet<_>>();
        assert_eq!(sqlite_receipts, expected_sqlite);
        assert_eq!(postgres_receipts, expected_postgres);
        assert!(!sqlite_receipts.contains("20260802_default_avionics_candidate_quarantine"));
    }

    async fn sqlite_catalog_snapshot(pool: &sqlx::SqlitePool) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT type, name, COALESCE(sql, '') FROM sqlite_schema \
             ORDER BY type, name",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn sqlite_receipt_snapshot(
        pool: &sqlx::SqlitePool,
    ) -> Vec<(String, Option<i64>, Option<String>, Option<String>)> {
        sqlx::query_as(
            "SELECT migration_name, contract_version, contract_fingerprint, installed_at \
             FROM schema_migration_contracts ORDER BY migration_name",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn sqlite_startup_rejects_anchorless_hostile_receipts_without_mutation() {
        let expected_fingerprint = AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_FINGERPRINT;
        let cases = [
            (
                "exact",
                Some(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION),
                Some(expected_fingerprint),
            ),
            ("wrong-version", Some(99), Some(expected_fingerprint)),
            (
                "wrong-fingerprint",
                Some(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION),
                Some("0000000000000000000000000000000000000000000000000000000000000000"),
            ),
            ("null-version", None, Some(expected_fingerprint)),
            (
                "null-fingerprint",
                Some(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION),
                None,
            ),
            ("both-null", None, None),
        ];

        for (label, version, fingerprint) in cases {
            let (database_path, database_url) =
                unique_sqlite_test_database(&format!("anchorless-receipt-{label}"));
            std::fs::File::create(&database_path).unwrap();
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            pool.execute(
                r#"
                CREATE TABLE schema_migration_contracts (
                  migration_name TEXT PRIMARY KEY,
                  contract_version INTEGER,
                  contract_fingerprint TEXT,
                  installed_at TEXT
                )
                "#,
            )
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO schema_migration_contracts ( \
                   migration_name, contract_version, contract_fingerprint, installed_at \
                 ) VALUES (?, ?, ?, ?)",
            )
            .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION)
            .bind(version)
            .bind(fingerprint)
            .bind("1999-12-31T23:59:59Z")
            .execute(&pool)
            .await
            .unwrap();
            let schema_before = sqlite_catalog_snapshot(&pool).await;
            let receipts_before = sqlite_receipt_snapshot(&pool).await;
            pool.close().await;

            let error = match AppDb::connect(&database_url).await {
                Ok(_) => panic!("{label}: anchorless receipt must reject startup"),
                Err(error) => format!("{error:#}"),
            };
            assert!(
                error.contains("database migration required before startup"),
                "{label}: {error}"
            );

            let inspection = SqlitePoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            assert_eq!(
                sqlite_catalog_snapshot(&inspection).await,
                schema_before,
                "{label}: rejected startup must not create canonical objects"
            );
            assert_eq!(
                sqlite_receipt_snapshot(&inspection).await,
                receipts_before,
                "{label}: rejected startup must not heal the receipt"
            );
            inspection.close().await;
            std::fs::remove_file(database_path).unwrap();
        }
    }

    #[tokio::test]
    async fn sqlite_startup_rejects_exact_receipts_in_impostor_ledgers_without_mutation() {
        let cases = [
            (
                "missing-primary-key",
                r#"
                CREATE TABLE schema_migration_contracts (
                  migration_name TEXT NOT NULL,
                  contract_version INTEGER NOT NULL,
                  contract_fingerprint TEXT NOT NULL,
                  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  CHECK (length(trim(migration_name)) > 0),
                  CHECK (typeof(contract_version) = 'integer' AND contract_version > 0),
                  CHECK (length(contract_fingerprint) = 64),
                  CHECK (contract_fingerprint = lower(contract_fingerprint)),
                  CHECK (contract_fingerprint NOT GLOB '*[^0-9a-f]*')
                )
                "#,
            ),
            (
                "nullable-version",
                r#"
                CREATE TABLE schema_migration_contracts (
                  migration_name TEXT PRIMARY KEY,
                  contract_version INTEGER,
                  contract_fingerprint TEXT NOT NULL,
                  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  CHECK (length(trim(migration_name)) > 0),
                  CHECK (typeof(contract_version) = 'integer' AND contract_version > 0),
                  CHECK (length(contract_fingerprint) = 64),
                  CHECK (contract_fingerprint = lower(contract_fingerprint)),
                  CHECK (contract_fingerprint NOT GLOB '*[^0-9a-f]*')
                )
                "#,
            ),
            (
                "missing-default",
                r#"
                CREATE TABLE schema_migration_contracts (
                  migration_name TEXT PRIMARY KEY,
                  contract_version INTEGER NOT NULL,
                  contract_fingerprint TEXT NOT NULL,
                  installed_at TEXT NOT NULL,
                  CHECK (length(trim(migration_name)) > 0),
                  CHECK (typeof(contract_version) = 'integer' AND contract_version > 0),
                  CHECK (length(contract_fingerprint) = 64),
                  CHECK (contract_fingerprint = lower(contract_fingerprint)),
                  CHECK (contract_fingerprint NOT GLOB '*[^0-9a-f]*')
                )
                "#,
            ),
            (
                "missing-checks",
                r#"
                CREATE TABLE schema_migration_contracts (
                  migration_name TEXT PRIMARY KEY,
                  contract_version INTEGER NOT NULL,
                  contract_fingerprint TEXT NOT NULL,
                  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                )
                "#,
            ),
        ];

        for (label, impostor_definition) in cases {
            let (database_path, database_url) =
                unique_sqlite_test_database(&format!("impostor-ledger-{label}"));
            let initialized = AppDb::connect(&database_url).await.unwrap();
            let DatabaseBackend::Sqlite(pool) = initialized.backend() else {
                unreachable!()
            };
            let receipts = sqlite_receipt_snapshot(pool).await;
            assert!(!receipts.is_empty());
            pool.execute("DROP TABLE schema_migration_contracts")
                .await
                .unwrap();
            pool.execute(impostor_definition).await.unwrap();
            for (migration_name, contract_version, contract_fingerprint, installed_at) in receipts {
                sqlx::query(
                    "INSERT INTO schema_migration_contracts ( \
                       migration_name, contract_version, contract_fingerprint, installed_at \
                     ) VALUES (?, ?, ?, ?)",
                )
                .bind(migration_name)
                .bind(contract_version)
                .bind(contract_fingerprint)
                .bind(installed_at)
                .execute(pool)
                .await
                .unwrap();
            }
            let schema_before = sqlite_catalog_snapshot(pool).await;
            let receipts_before = sqlite_receipt_snapshot(pool).await;
            initialized.close().await;

            let error = match AppDb::connect(&database_url).await {
                Ok(_) => {
                    panic!("{label}: exact receipts in an impostor ledger must reject startup")
                }
                Err(error) => format!("{error:#}"),
            };
            assert!(
                error.contains("database migration required before startup"),
                "{label}: {error}"
            );

            let inspection = SqlitePoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            assert_eq!(
                sqlite_catalog_snapshot(&inspection).await,
                schema_before,
                "{label}: rejected startup must not repair the ledger or other schema"
            );
            assert_eq!(
                sqlite_receipt_snapshot(&inspection).await,
                receipts_before,
                "{label}: rejected startup must not replace exact receipts"
            );
            inspection.close().await;
            std::fs::remove_file(database_path).unwrap();
        }
    }

    #[tokio::test]
    async fn sqlite_startup_rejects_attached_ledger_trigger_without_mutation() {
        let (database_path, database_url) = unique_sqlite_test_database("ledger-trigger");
        let initialized = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = initialized.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE schema_migration_contracts \
             SET installed_at = 'sentinel:' || migration_name",
        )
        .execute(pool)
        .await
        .unwrap();
        pool.execute(
            r#"
            CREATE TRIGGER schema_migration_contracts_mutate_before_insert
            BEFORE INSERT ON schema_migration_contracts
            BEGIN
              UPDATE schema_migration_contracts
              SET installed_at = 'trigger-mutated';
            END
            "#,
        )
        .await
        .unwrap();
        let schema_before = sqlite_catalog_snapshot(pool).await;
        let receipts_before = sqlite_receipt_snapshot(pool).await;
        assert!(receipts_before.iter().all(|receipt| receipt
            .3
            .as_deref()
            .is_some_and(|installed_at| installed_at.starts_with("sentinel:"))));
        initialized.close().await;

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("an attached ledger trigger must reject startup"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("database migration required before startup"),
            "{error}"
        );

        let inspection = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(sqlite_catalog_snapshot(&inspection).await, schema_before);
        assert_eq!(
            sqlite_receipt_snapshot(&inspection).await,
            receipts_before,
            "rejected startup must not fire the hostile BEFORE INSERT trigger"
        );
        inspection.close().await;
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn sqlite_startup_rejects_anchor_without_receipt_before_initialization() {
        let (database_path, database_url) = unique_sqlite_test_database("anchor-without-receipt");
        std::fs::File::create(&database_path).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        pool.execute("CREATE TABLE aircraft_makes (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        let schema_before = sqlite_catalog_snapshot(&pool).await;
        pool.close().await;

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("an anchor without its receipt must reject startup"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("deterministic retrieval-key data repair"),
            "{error}"
        );

        let inspection = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(sqlite_catalog_snapshot(&inspection).await, schema_before);
        inspection.close().await;
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn sqlite_all_receipt_timestamps_survive_two_normal_startups() {
        let (database_path, database_url) = unique_sqlite_test_database("all-receipt-times");
        let initialized = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = initialized.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            INSERT INTO schema_migration_contracts (
              migration_name, contract_version, contract_fingerprint, installed_at
            ) VALUES (
              '20260809_listing_verification_runs', 1,
              'a8beda24d71517ba07e4a81b2802b2fef97296ae6b2256a7ff493d6af5235232',
              'historical-sentinel'
            )
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE schema_migration_contracts \
             SET installed_at = 'sentinel:' || migration_name",
        )
        .execute(pool)
        .await
        .unwrap();
        let expected = sqlite_receipt_snapshot(pool).await;
        assert_eq!(
            expected.len(),
            canonical_startup_migration_contract_receipts(DatabaseKind::Sqlite).len() + 1
        );
        assert!(expected
            .iter()
            .any(|receipt| receipt.0 == "20260809_listing_verification_runs"));
        assert!(expected.iter().all(|receipt| receipt
            .3
            .as_deref()
            .is_some_and(|installed_at| installed_at.starts_with("sentinel:"))));
        initialized.close().await;

        for startup in 1..=2 {
            let reopened = AppDb::connect(&database_url).await.unwrap();
            let DatabaseBackend::Sqlite(pool) = reopened.backend() else {
                unreachable!()
            };
            assert_eq!(
                sqlite_receipt_snapshot(pool).await,
                expected,
                "startup {startup} must preserve every original install receipt"
            );
            reopened.close().await;
        }
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn sqlite_startup_waits_for_receipt_writer_commit_or_rollback() {
        for commit_writer in [true, false] {
            let label = if commit_writer { "commit" } else { "rollback" };
            let (database_path, database_url) =
                unique_sqlite_test_database(&format!("writer-first-{label}"));
            AppDb::connect(&database_url).await.unwrap().close().await;

            let mut writer = SqliteConnection::connect(&database_url).await.unwrap();
            writer.execute("BEGIN IMMEDIATE").await.unwrap();
            sqlx::query("DELETE FROM schema_migration_contracts WHERE migration_name = ?")
                .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION)
                .execute(&mut writer)
                .await
                .unwrap();

            let startup_url = database_url.clone();
            let mut startup = tokio::spawn(async move { AppDb::connect(&startup_url).await });
            assert!(
                tokio::time::timeout(Duration::from_millis(150), &mut startup)
                    .await
                    .is_err(),
                "startup must wait behind the writer's BEGIN IMMEDIATE"
            );
            writer
                .execute(if commit_writer { "COMMIT" } else { "ROLLBACK" })
                .await
                .unwrap();

            let startup_result = tokio::time::timeout(Duration::from_secs(20), startup)
                .await
                .expect("serialized SQLite startup timed out")
                .unwrap();
            if commit_writer {
                let error = connect_error(startup_result);
                assert!(
                    error.contains("deterministic retrieval-key data repair"),
                    "{error}"
                );
                let mut inspection = SqliteConnection::connect(&database_url).await.unwrap();
                let receipt_exists: i64 = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM schema_migration_contracts \
                     WHERE migration_name = ?)",
                )
                .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION)
                .fetch_one(&mut inspection)
                .await
                .unwrap();
                assert_eq!(
                    receipt_exists, 0,
                    "rejected startup must not heal the receipt"
                );
            } else {
                startup_result.unwrap().close().await;
            }
            std::fs::remove_file(database_path).unwrap();
        }
    }

    #[tokio::test]
    async fn sqlite_fresh_startups_serialize_and_late_failure_rolls_back() {
        let (database_path, database_url) = unique_sqlite_test_database("fresh-concurrency");
        let first_url = database_url.clone();
        let second_url = database_url.clone();
        let (first, second) = tokio::join!(AppDb::connect(&first_url), AppDb::connect(&second_url));
        first.unwrap().close().await;
        second.unwrap().close().await;
        let inspection = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(
            sqlite_receipt_snapshot(&inspection).await.len(),
            canonical_startup_migration_contract_receipts(DatabaseKind::Sqlite).len()
        );
        inspection.close().await;
        std::fs::remove_file(database_path).unwrap();

        let (database_path, database_url) = unique_sqlite_test_database("late-rollback");
        std::fs::File::create(&database_path).unwrap();
        let setup = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        setup
            .execute("CREATE TABLE users (sentinel TEXT NOT NULL)")
            .await
            .unwrap();
        let before = sqlite_catalog_snapshot(&setup).await;
        setup.close().await;

        let error = connect_error(AppDb::connect(&database_url).await);
        assert!(
            error.contains("users") || error.contains("email"),
            "{error}"
        );
        let inspection = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(
            sqlite_catalog_snapshot(&inspection).await,
            before,
            "late seed failure must roll back every canonical SQLite DDL statement"
        );
        inspection.close().await;
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn diagnostic_sqlite_connection_never_creates_a_missing_database() {
        let (database_path, database_url) = unique_sqlite_test_database("diagnostic-missing");
        assert!(!database_path.exists());
        let error = match AppDb::connect_diagnostic(&database_url).await {
            Ok(_) => panic!("diagnostic connection must not create a missing SQLite database"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("could not open diagnostic SQLite database"));
        assert!(!database_path.exists());
    }

    #[tokio::test]
    async fn diagnostic_sqlite_connection_leaves_database_bytes_unchanged() {
        let (database_path, database_url) = unique_sqlite_test_database("diagnostic-unchanged");
        let writable = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(writable_pool) = writable.backend() else {
            unreachable!()
        };
        writable_pool.close().await;
        drop(writable);
        let before = std::fs::read(&database_path).unwrap();

        let diagnostic = AppDb::connect_diagnostic(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(diagnostic_pool) = diagnostic.backend() else {
            unreachable!()
        };
        let table_count =
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sqlite_schema WHERE type = 'table'")
                .fetch_one(diagnostic_pool)
                .await
                .unwrap();
        assert!(table_count > 0);
        diagnostic_pool.close().await;
        drop(diagnostic);

        assert_eq!(std::fs::read(&database_path).unwrap(), before);
        assert!(!PathBuf::from(format!("{}-wal", database_path.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", database_path.display())).exists());
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL in AIRCOST_TEST_POSTGRES_URL"]
    async fn diagnostic_postgres_connections_default_to_read_only() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let diagnostic = AppDb::connect_diagnostic(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = diagnostic.backend() else {
            unreachable!()
        };
        let setting = sqlx::query_scalar::<_, String>("SHOW default_transaction_read_only")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(setting, "on");
        let error = pool
            .execute("CREATE TABLE public.aircost_diagnostic_write_must_fail (id BIGINT)")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("read-only"), "{error}");
        pool.close().await;
    }

    fn file_sha256(path: &std::path::Path) -> String {
        format!(
            "{:x}",
            Sha256::digest(std::fs::read(path).expect("test database must remain readable"))
        )
    }

    async fn diagnostic_table_snapshot(
        db: &AppDb,
    ) -> (Vec<(String, String)>, Vec<(String, String)>) {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let schema = sqlx::query_as::<_, (String, String)>(
            "SELECT name, COALESCE(sql, '') FROM sqlite_schema ORDER BY type, name",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        let contracts = sqlx::query_as::<_, (String, String)>(
            r#"SELECT migration_name,
                      contract_version || ':' || contract_fingerprint || ':' || installed_at
               FROM schema_migration_contracts ORDER BY migration_name"#,
        )
        .fetch_all(pool)
        .await
        .unwrap();
        (schema, contracts)
    }

    #[tokio::test]
    async fn diagnostic_connection_attests_without_mutating_and_accepts_read_only_uri() {
        let (database_path, database_url) = unique_sqlite_test_database("diagnostic-read-only");
        let initialized = AppDb::connect(&database_url).await.unwrap();
        let before_tables = diagnostic_table_snapshot(&initialized).await;
        let DatabaseBackend::Sqlite(initialized_pool) = initialized.backend() else {
            unreachable!()
        };
        initialized_pool.close().await;
        let before_sha256 = file_sha256(&database_path);

        let read_only_url = format!("{database_url}?mode=ro");
        let diagnostic = AppDb::connect_diagnostic(&read_only_url)
            .await
            .expect("the installed schema must attest through a read-only URI");
        assert_eq!(diagnostic_table_snapshot(&diagnostic).await, before_tables);
        let DatabaseBackend::Sqlite(diagnostic_pool) = diagnostic.backend() else {
            unreachable!()
        };
        assert!(
            sqlx::query("UPDATE schema_migration_contracts SET installed_at = 'mutated'")
                .execute(diagnostic_pool)
                .await
                .is_err()
        );
        diagnostic_pool.close().await;

        assert_eq!(file_sha256(&database_path), before_sha256);
        let reopened = AppDb::connect_diagnostic(&read_only_url).await.unwrap();
        assert_eq!(diagnostic_table_snapshot(&reopened).await, before_tables);
        let DatabaseBackend::Sqlite(reopened_pool) = reopened.backend() else {
            unreachable!()
        };
        reopened_pool.close().await;
        std::fs::remove_file(database_path).unwrap();
    }

    async fn legacy_replay_migration_connection() -> SqliteConnection {
        let mut connection = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE schema_migration_contracts (
              migration_name TEXT PRIMARY KEY,
              contract_version INTEGER NOT NULL,
              contract_fingerprint TEXT NOT NULL,
              installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE users (id INTEGER PRIMARY KEY);
            CREATE TABLE plugin_installs (
              id INTEGER PRIMARY KEY, user_id INTEGER, public_key_base64 TEXT,
              created_at TEXT, revoked_at TEXT
            );
            CREATE TABLE plugin_submissions (
              id INTEGER PRIMARY KEY, user_id INTEGER, plugin_install_id INTEGER,
              source_url TEXT, submitted_at TEXT, rendered_html TEXT,
              rendered_html_sha256 TEXT, signature_base64 TEXT,
              extracted_listing_json TEXT, extraction_error TEXT,
              canonical_listing_id INTEGER
            );
            CREATE TABLE aircraft_sale_listings (
              id INTEGER PRIMARY KEY, created_by_user_id INTEGER, source_url TEXT
            );
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection
    }

    async fn assert_sqlite_replay_migration_rerun_rejected_without_changes(
        db: &AppDb,
        label: &str,
    ) {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query(
            "UPDATE schema_migration_contracts SET installed_at = ? WHERE migration_name = ?",
        )
        .bind("1999-12-31T23:59:59Z")
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .execute(&mut *connection)
        .await
        .unwrap();
        let snapshot_sql = r#"
            SELECT type, name, COALESCE(sql, '')
            FROM sqlite_schema
            WHERE name IN (
                'listing_replay_runs', 'listing_replay_run_items',
                'plugin_submission_materialization_receipts',
                'idx_listing_replay_runs_one_running',
                'idx_listing_replay_run_items_phase'
              )
              OR tbl_name IN (
                'listing_replay_runs', 'listing_replay_run_items',
                'plugin_submission_materialization_receipts',
                'plugin_submissions', 'plugin_installs'
              )
            ORDER BY type, name
        "#;
        let before = sqlx::query_as::<_, (String, String, String)>(snapshot_sql)
            .fetch_all(&mut *connection)
            .await
            .unwrap();
        let rerun = sqlx::raw_sql(LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL)
            .execute(&mut *connection)
            .await;
        assert!(rerun.is_err(), "{label}: hostile rerun must fail");
        let _ = connection.execute("ROLLBACK").await;
        let after = sqlx::query_as::<_, (String, String, String)>(snapshot_sql)
            .fetch_all(&mut *connection)
            .await
            .unwrap();
        assert_eq!(after, before, "{label}: rejected rerun must not mutate DDL");
        let installed_at: String = sqlx::query_scalar(
            "SELECT installed_at FROM schema_migration_contracts WHERE migration_name = ?",
        )
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .fetch_one(&mut *connection)
        .await
        .unwrap();
        assert_eq!(installed_at, "1999-12-31T23:59:59Z", "{label}");
    }

    async fn assert_weakened_replay_item_rejected(
        label: &str,
        expected_fragment: &str,
        weakened_fragment: &str,
    ) {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let canonical = sqlite_table_definition(SQLITE_SCHEMA_SQL, "listing_replay_run_items")
            .unwrap()
            .replace("CREATE TABLE IF NOT EXISTS", "CREATE TABLE");
        let weakened = canonical.replace(expected_fragment, weakened_fragment);
        assert_ne!(canonical, weakened, "{label} must alter the table contract");
        let mut connection = pool.acquire().await.unwrap();
        connection
            .execute("PRAGMA foreign_keys = OFF")
            .await
            .unwrap();
        connection
            .execute("ALTER TABLE listing_replay_run_items RENAME TO listing_replay_run_items_old")
            .await
            .unwrap();
        connection
            .execute("DROP INDEX idx_listing_replay_run_items_phase")
            .await
            .unwrap();
        sqlx::raw_sql(&weakened)
            .execute(&mut *connection)
            .await
            .unwrap();
        connection
            .execute(
                "CREATE INDEX idx_listing_replay_run_items_phase ON listing_replay_run_items (run_id, extraction_state, materialization_state, position)",
            )
            .await
            .unwrap();
        connection
            .execute("DROP TABLE listing_replay_run_items_old")
            .await
            .unwrap();
        connection
            .execute("PRAGMA foreign_keys = ON")
            .await
            .unwrap();
        drop(connection);

        assert_sqlite_replay_migration_rerun_rejected_without_changes(&db, label).await;

        assert!(
            !db.listing_replay_definitions_valid().await.unwrap(),
            "{label}"
        );
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("20260819_listing_replay_runs.sqlite.sql"));
    }

    async fn assert_weakened_replay_run_rejected(
        label: &str,
        expected_fragment: &str,
        weakened_fragment: &str,
    ) {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let canonical_run = sqlite_table_definition(SQLITE_SCHEMA_SQL, "listing_replay_runs")
            .unwrap()
            .replace("CREATE TABLE IF NOT EXISTS", "CREATE TABLE");
        let weakened_run = canonical_run.replace(expected_fragment, weakened_fragment);
        assert_ne!(
            canonical_run, weakened_run,
            "{label} must alter the table contract"
        );
        let canonical_item = sqlite_table_definition(SQLITE_SCHEMA_SQL, "listing_replay_run_items")
            .unwrap()
            .replace("CREATE TABLE IF NOT EXISTS", "CREATE TABLE");
        let mut connection = pool.acquire().await.unwrap();
        connection
            .execute("PRAGMA foreign_keys = OFF")
            .await
            .unwrap();
        connection
            .execute("DROP INDEX idx_listing_replay_run_items_phase")
            .await
            .unwrap();
        connection
            .execute("DROP INDEX idx_listing_replay_runs_one_running")
            .await
            .unwrap();
        connection
            .execute("ALTER TABLE listing_replay_run_items RENAME TO listing_replay_run_items_old")
            .await
            .unwrap();
        connection
            .execute("ALTER TABLE listing_replay_runs RENAME TO listing_replay_runs_old")
            .await
            .unwrap();
        sqlx::raw_sql(&weakened_run)
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::raw_sql(&canonical_item)
            .execute(&mut *connection)
            .await
            .unwrap();
        connection
            .execute("DROP TABLE listing_replay_run_items_old")
            .await
            .unwrap();
        connection
            .execute("DROP TABLE listing_replay_runs_old")
            .await
            .unwrap();
        connection
            .execute(
                "CREATE UNIQUE INDEX idx_listing_replay_runs_one_running ON listing_replay_runs (status) WHERE status = 'running'",
            )
            .await
            .unwrap();
        connection
            .execute(
                "CREATE INDEX idx_listing_replay_run_items_phase ON listing_replay_run_items (run_id, extraction_state, materialization_state, position)",
            )
            .await
            .unwrap();
        connection
            .execute("PRAGMA foreign_keys = ON")
            .await
            .unwrap();
        drop(connection);

        assert_sqlite_replay_migration_rerun_rejected_without_changes(&db, label).await;

        assert!(
            !db.listing_replay_definitions_valid().await.unwrap(),
            "{label}"
        );
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("20260819_listing_replay_runs.sqlite.sql"));
    }

    #[tokio::test]
    async fn sqlite_listing_replay_migration_upgrades_once_and_is_idempotent() {
        let mut connection = legacy_replay_migration_connection().await;
        sqlx::raw_sql(LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE schema_migration_contracts SET installed_at = ? WHERE migration_name = ?",
        )
        .bind("1999-12-31T23:59:59Z")
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::raw_sql(LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL)
            .execute(&mut connection)
            .await
            .unwrap();
        let contract: (i64, String, String) = sqlx::query_as(
            "SELECT contract_version, contract_fingerprint, installed_at FROM schema_migration_contracts WHERE migration_name = ?",
        )
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(contract.0, LISTING_REPLAY_RUNS_CONTRACT_VERSION);
        assert_eq!(contract.1, LISTING_REPLAY_RUNS_CONTRACT_FINGERPRINT);
        assert_eq!(contract.2, "1999-12-31T23:59:59Z");
        let foreign_key_failures: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(foreign_key_failures, 0);
    }

    #[tokio::test]
    async fn sqlite_listing_replay_installed_at_survives_two_normal_startups() {
        let (database_path, database_url) =
            unique_sqlite_test_database("listing-replay-installed-at");
        let initialized = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = initialized.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE schema_migration_contracts SET installed_at = ? WHERE migration_name = ?",
        )
        .bind("1999-12-31T23:59:59Z")
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .execute(pool)
        .await
        .unwrap();
        pool.close().await;
        drop(initialized);

        for startup in 1..=2 {
            let reopened = AppDb::connect(&database_url).await.unwrap();
            let DatabaseBackend::Sqlite(pool) = reopened.backend() else {
                unreachable!()
            };
            let installed_at: String = sqlx::query_scalar(
                "SELECT installed_at FROM schema_migration_contracts WHERE migration_name = ?",
            )
            .bind(LISTING_REPLAY_RUNS_MIGRATION)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(
                installed_at, "1999-12-31T23:59:59Z",
                "startup {startup} must preserve the original install receipt"
            );
            pool.close().await;
            drop(reopened);
        }
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn sqlite_replay_ledger_rejects_invalid_state_outcome_pairings() {
        let mut connection = legacy_replay_migration_connection().await;
        sqlx::raw_sql(LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO users (id) VALUES (1)")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO plugin_installs \
             (id, user_id, public_key_base64, created_at) \
             VALUES (1, 1, 'key', '2026-08-19T00:00:00Z')",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_sale_listings (id, created_by_user_id, source_url) \
             VALUES (7, 1, 'https://example.test/replay')",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO plugin_submissions (id, user_id, plugin_install_id, source_url, \
             submitted_at, rendered_html, rendered_html_sha256, signature_base64, \
             extracted_listing_json, canonical_listing_id) VALUES \
             (1, 1, 1, 'https://example.test/replay', '2026-08-19T01:00:00Z', \
              '<html/>', ?, 'signature', '{}', 7)",
        )
        .bind("b".repeat(64))
        .execute(&mut connection)
        .await
        .unwrap();
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO listing_replay_runs (manifest_version, manifest_sha256, manifest_capture_count) VALUES (1, ?, 3) RETURNING id",
        )
        .bind("a".repeat(64))
        .fetch_one(&mut connection)
        .await
        .unwrap();

        for sql in [
            r#"INSERT INTO listing_replay_run_items (
                 run_id, plugin_submission_id, position, expected_rendered_html_sha256,
                 extraction_state, materialization_state, terminal_rejection_phase,
                 terminal_rejection_stage, terminal_rejection_reason_code
               ) VALUES (?, 1, 0, ?, 'rejected', 'blocked', 'extraction',
                         'faa_aircraft_admission', 'missing_registration')"#,
            r#"INSERT INTO listing_replay_run_items (
                 run_id, plugin_submission_id, position, expected_rendered_html_sha256,
                 extracted_listing_sha256, extraction_state, materialization_state,
                 last_failure_phase, last_failure_reason_code
               ) VALUES (?, 1, 1, ?, ?, 'succeeded', 'failed',
                         'extraction', 'operation_failed')"#,
            r#"INSERT INTO listing_replay_run_items (
                 run_id, plugin_submission_id, position, expected_rendered_html_sha256,
                 extracted_listing_sha256, extraction_state, materialization_state,
                 resulting_listing_id, last_failure_phase, last_failure_reason_code
               ) VALUES (?, 1, 2, ?, ?, 'succeeded', 'failed', 7,
                         'materialization', 'operation_failed')"#,
            r#"INSERT INTO listing_replay_run_items (
                 run_id, plugin_submission_id, position, expected_rendered_html_sha256,
                 extracted_listing_sha256, extraction_state, materialization_state
               ) VALUES (?, 1, 3, ?, ?, 'succeeded', 'succeeded')"#,
        ] {
            let mut query = sqlx::query(sql).bind(run_id).bind("b".repeat(64));
            if sql.contains("extracted_listing_sha256") {
                query = query.bind("c".repeat(64));
            }
            assert!(query.execute(&mut connection).await.is_err(), "{sql}");
        }
    }

    #[tokio::test]
    async fn fresh_schema_accepts_the_exact_listing_replay_migration_again() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::raw_sql(LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn startup_rejects_tampered_listing_replay_concurrency_index() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("DROP INDEX idx_listing_replay_runs_one_running")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE INDEX idx_listing_replay_runs_one_running ON listing_replay_runs (status)",
        )
        .execute(pool)
        .await
        .unwrap();

        assert_sqlite_replay_migration_rerun_rejected_without_changes(
            &db,
            "weakened-running-index",
        )
        .await;

        assert!(!db.listing_replay_definitions_valid().await.unwrap());
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("20260819_listing_replay_runs.sqlite.sql"));
    }

    #[tokio::test]
    async fn startup_rejects_missing_listing_replay_item_uniqueness() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let table_start = LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL
            .find("CREATE TABLE IF NOT EXISTS listing_replay_run_items (")
            .unwrap();
        let table_tail = &LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL[table_start..];
        let table_end = table_tail
            .find("\n);\n\nCREATE INDEX IF NOT EXISTS idx_listing_replay_run_items_phase")
            .unwrap()
            + 3;
        let replacement_definition = table_tail[..table_end]
            .replace("CREATE TABLE IF NOT EXISTS", "CREATE TABLE")
            .replace(
                "  UNIQUE (run_id, position),\n  UNIQUE (run_id, plugin_submission_id),\n",
                "",
            );
        let mut connection = pool.acquire().await.unwrap();
        connection
            .execute("PRAGMA foreign_keys = OFF")
            .await
            .unwrap();
        connection
            .execute("ALTER TABLE listing_replay_run_items RENAME TO listing_replay_run_items_old")
            .await
            .unwrap();
        connection
            .execute("DROP INDEX idx_listing_replay_run_items_phase")
            .await
            .unwrap();
        sqlx::raw_sql(&replacement_definition)
            .execute(&mut *connection)
            .await
            .unwrap();
        connection
            .execute(
                "CREATE INDEX idx_listing_replay_run_items_phase ON listing_replay_run_items (run_id, extraction_state, materialization_state, position)",
            )
            .await
            .unwrap();
        connection
            .execute("DROP TABLE listing_replay_run_items_old")
            .await
            .unwrap();
        connection
            .execute("PRAGMA foreign_keys = ON")
            .await
            .unwrap();
        drop(connection);

        assert_sqlite_replay_migration_rerun_rejected_without_changes(
            &db,
            "missing-item-uniqueness",
        )
        .await;

        assert!(!db.listing_replay_definitions_valid().await.unwrap());
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("20260819_listing_replay_runs.sqlite.sql"));
    }

    #[tokio::test]
    async fn listing_replay_rerun_rejects_unexpected_sqlite_indexes_and_triggers() {
        for (label, statement) in [
            (
                "unexpected-attached-index",
                "CREATE INDEX unexpected_replay_owner_index ON listing_replay_runs(owner_token)",
            ),
            (
                "unexpected-attached-trigger",
                "CREATE TRIGGER unexpected_replay_update BEFORE UPDATE ON listing_replay_runs BEGIN SELECT 1; END",
            ),
            (
                "unexpected-submission-trigger",
                "CREATE TRIGGER unexpected_submission_delete BEFORE DELETE ON plugin_submissions BEGIN SELECT 1; END",
            ),
            (
                "unexpected-install-trigger",
                "CREATE TRIGGER unexpected_install_delete BEFORE DELETE ON plugin_installs BEGIN SELECT 1; END",
            ),
        ] {
            let db = AppDb::connect("sqlite::memory:").await.unwrap();
            let DatabaseBackend::Sqlite(pool) = db.backend() else {
                unreachable!()
            };
            pool.execute(statement).await.unwrap();
            assert_sqlite_replay_migration_rerun_rejected_without_changes(&db, label).await;
            assert!(!db.listing_replay_definitions_valid().await.unwrap(), "{label}");
        }
    }

    #[tokio::test]
    async fn listing_replay_rerun_rejects_weakened_same_name_sqlite_trigger_without_healing() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TRIGGER listing_replay_run_items_checkpoint_exact_insert")
            .await
            .unwrap();
        pool.execute(
            "CREATE TRIGGER listing_replay_run_items_checkpoint_exact_insert \
             BEFORE INSERT ON listing_replay_run_items BEGIN SELECT 1; END",
        )
        .await
        .unwrap();

        assert_sqlite_replay_migration_rerun_rejected_without_changes(
            &db,
            "weakened-same-name-trigger",
        )
        .await;

        let retained_definition: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema \
             WHERE type = 'trigger' \
               AND name = 'listing_replay_run_items_checkpoint_exact_insert'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(retained_definition.ends_with("BEGIN SELECT 1; END"));
        assert!(!db.listing_replay_definitions_valid().await.unwrap());
    }

    #[tokio::test]
    async fn startup_rejects_weakened_replay_columns_defaults_checks_and_foreign_keys() {
        assert_weakened_replay_item_rejected(
            "nullable-plugin-submission",
            "plugin_submission_id INTEGER NOT NULL",
            "plugin_submission_id INTEGER",
        )
        .await;
        assert_weakened_replay_item_rejected(
            "changed-extraction-default",
            "extraction_state TEXT NOT NULL DEFAULT 'queued'",
            "extraction_state TEXT NOT NULL DEFAULT 'failed'",
        )
        .await;
        assert_weakened_replay_item_rejected(
            "weakened-plugin-foreign-key",
            "REFERENCES plugin_submissions(id) ON DELETE RESTRICT",
            "REFERENCES plugin_submissions(id) ON DELETE CASCADE",
        )
        .await;
        assert_weakened_replay_item_rejected(
            "missing-checkpoint-state-check",
            "  CHECK ((extraction_state = 'succeeded') = (extracted_listing_sha256 IS NOT NULL)),\n",
            "",
        )
        .await;
    }

    #[tokio::test]
    async fn startup_rejects_weakened_manifest_uniqueness_and_owner_state_checks() {
        assert_weakened_replay_run_rejected(
            "manifest-not-unique",
            "manifest_sha256 TEXT NOT NULL UNIQUE",
            "manifest_sha256 TEXT NOT NULL",
        )
        .await;
        assert_weakened_replay_run_rejected(
            "owner-state-check-removed",
            "  CHECK (\n    (status = 'running' AND active_phase IS NOT NULL AND owner_token IS NOT NULL\n      AND heartbeat_at_epoch_seconds IS NOT NULL AND started_at IS NOT NULL\n      AND completed_at IS NULL)\n    OR\n    (status = 'queued' AND active_phase IS NULL AND owner_token IS NULL\n      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NULL)\n    OR\n    (status = 'completed' AND active_phase IS NULL AND owner_token IS NULL\n      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NOT NULL)\n  )\n",
            "  CHECK (status IN ('queued', 'running', 'completed'))\n",
        )
        .await;
    }

    #[tokio::test]
    async fn listing_replay_migration_rejects_mismatched_contract_and_partial_objects() {
        let mut mismatched = legacy_replay_migration_connection().await;
        sqlx::query(
            "INSERT INTO schema_migration_contracts (migration_name, contract_version, contract_fingerprint) VALUES (?, 99, ?)",
        )
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .bind("f".repeat(64))
        .execute(&mut mismatched)
        .await
        .unwrap();
        assert!(sqlx::raw_sql(LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL)
            .execute(&mut mismatched)
            .await
            .is_err());
        let _ = sqlx::query("ROLLBACK").execute(&mut mismatched).await;
        let created: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'listing_replay_runs'",
        )
        .fetch_one(&mut mismatched)
        .await
        .unwrap();
        assert_eq!(created, 0);

        let mut partial = legacy_replay_migration_connection().await;
        sqlx::query("CREATE TABLE listing_replay_runs (id INTEGER PRIMARY KEY)")
            .execute(&mut partial)
            .await
            .unwrap();
        assert!(sqlx::raw_sql(LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL)
            .execute(&mut partial)
            .await
            .is_err());
        let _ = sqlx::query("ROLLBACK").execute(&mut partial).await;
        let item_created: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'listing_replay_run_items'",
        )
        .fetch_one(&mut partial)
        .await
        .unwrap();
        let contract_created: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM schema_migration_contracts WHERE migration_name = ?",
        )
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .fetch_one(&mut partial)
        .await
        .unwrap();
        assert_eq!((item_created, contract_created), (0, 0));
    }

    #[tokio::test]
    async fn listing_replay_result_preserves_materialized_listing() {
        let mut connection = legacy_replay_migration_connection().await;
        sqlx::raw_sql(LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO users (id) VALUES (1)")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO plugin_installs \
             (id, user_id, public_key_base64, created_at) \
             VALUES (1, 1, 'key', '2026-08-19T00:00:00Z')",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_sale_listings (id, created_by_user_id, source_url) \
             VALUES (7, 1, 'https://example.test/replay-result')",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO plugin_submissions (id, user_id, plugin_install_id, source_url, \
             submitted_at, rendered_html, rendered_html_sha256, signature_base64, \
             extracted_listing_json, canonical_listing_id) VALUES \
             (1, 1, 1, 'https://example.test/replay-result', '2026-08-19T01:00:00Z', \
              '<html/>', ?, 'signature', '{}', 7)",
        )
        .bind("b".repeat(64))
        .execute(&mut connection)
        .await
        .unwrap();
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO listing_replay_runs (manifest_version, manifest_sha256, manifest_capture_count) VALUES (1, ?, 1) RETURNING id",
        )
        .bind("a".repeat(64))
        .fetch_one(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO listing_replay_run_items (run_id, plugin_submission_id, position, expected_rendered_html_sha256, extracted_listing_sha256, extracted_listing_json, extraction_state, materialization_state, resulting_listing_id) VALUES (?, 1, 0, ?, ?, '{}', 'succeeded', 'succeeded', 7)",
        )
        .bind(run_id)
        .bind("b".repeat(64))
        .bind("c".repeat(64))
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO plugin_submission_materialization_receipts (plugin_submission_id, aircraft_sale_listing_id, rendered_html_sha256, extracted_listing_sha256) VALUES (1, 7, ?, ?)",
        )
        .bind("b".repeat(64))
        .bind("c".repeat(64))
        .execute(&mut connection)
        .await
        .unwrap();
        for (label, statement) in [
            (
                "signed capture bytes",
                "UPDATE plugin_submissions SET rendered_html = '<changed/>' WHERE id = 1",
            ),
            (
                "plugin signing identity",
                "UPDATE plugin_installs SET public_key_base64 = 'changed' WHERE id = 1",
            ),
            (
                "completion receipt update",
                "UPDATE plugin_submission_materialization_receipts SET completed_at = 'changed' WHERE plugin_submission_id = 1",
            ),
            (
                "completion receipt deletion",
                "DELETE FROM plugin_submission_materialization_receipts WHERE plugin_submission_id = 1",
            ),
            (
                "completed replay item update",
                "UPDATE listing_replay_run_items SET updated_at = 'changed' WHERE run_id = 1",
            ),
            (
                "completed replay item deletion",
                "DELETE FROM listing_replay_run_items WHERE run_id = 1",
            ),
        ] {
            assert!(
                connection.execute(statement).await.is_err(),
                "{label} must be immutable after completion"
            );
        }
        let deletion = sqlx::query("DELETE FROM aircraft_sale_listings WHERE id = 7")
            .execute(&mut connection)
            .await;
        assert!(deletion.is_err());
        let resulting_listing_id: Option<i64> = sqlx::query_scalar(
            "SELECT resulting_listing_id FROM listing_replay_run_items WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_one(&mut connection)
        .await
        .unwrap();
        let listing_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aircraft_sale_listings WHERE id = 7")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        let receipt_listing_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_sale_listing_id FROM plugin_submission_materialization_receipts WHERE plugin_submission_id = 1",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(resulting_listing_id, Some(7));
        assert_eq!(listing_count, 1);
        assert_eq!(receipt_listing_id, 7);
    }

    #[tokio::test]
    async fn listing_source_claim_is_atomic_per_owner_and_reusable_across_owners() {
        let mut connection = legacy_replay_migration_connection().await;
        sqlx::raw_sql(LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO users (id) VALUES (1), (2)")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_sale_listings (id, created_by_user_id, source_url) \
             VALUES (1, 1, 'https://example.test/shared-source')",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let duplicate = sqlx::query(
            "INSERT INTO aircraft_sale_listings (id, created_by_user_id, source_url) \
             VALUES (2, 1, 'https://example.test/shared-source')",
        )
        .execute(&mut connection)
        .await;
        assert!(duplicate.is_err(), "one owner cannot claim a source twice");
        sqlx::query(
            "INSERT INTO aircraft_sale_listings (id, created_by_user_id, source_url) \
             VALUES (3, 2, 'https://example.test/shared-source')",
        )
        .execute(&mut connection)
        .await
        .expect("the same publisher URL remains independent across owners");
    }

    #[test]
    fn postgres_listing_replay_migration_is_public_qualified() {
        for required in [
            "public.schema_migration_contracts",
            "public.listing_replay_runs",
            "public.listing_replay_run_items",
            "public.plugin_submission_materialization_receipts",
            "public.plugin_submissions",
            "public.aircraft_sale_listings",
        ] {
            assert!(
                LISTING_REPLAY_RUNS_POSTGRES_MIGRATION_SQL.contains(required),
                "Postgres replay migration must qualify {required}"
            );
        }
        assert!(LISTING_REPLAY_RUNS_POSTGRES_MIGRATION_SQL
            .contains("listing replay objects exist without the exact migration contract"));
        for definition in [
            SQLITE_SCHEMA_SQL,
            POSTGRES_SCHEMA_SQL,
            LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL,
            LISTING_REPLAY_RUNS_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(definition.contains(LISTING_REPLAY_RUNS_MIGRATION));
            assert!(definition.contains(LISTING_REPLAY_RUNS_CONTRACT_FINGERPRINT));
            assert!(definition.contains("listing_replay_runs"));
            assert!(definition.contains("listing_replay_run_items"));
            let marker = definition
                .find("'20260819_listing_replay_runs', 1")
                .expect("the replay install marker must be present");
            let receipt = &definition[marker..(marker + 600).min(definition.len())];
            assert!(receipt.contains("ON CONFLICT (migration_name) DO NOTHING"));
            assert!(!receipt.contains("installed_at ="));
        }
        for (migration, first_replay_ddl) in [
            (
                LISTING_REPLAY_RUNS_SQLITE_MIGRATION_SQL,
                "CREATE TABLE IF NOT EXISTS listing_replay_runs",
            ),
            (
                LISTING_REPLAY_RUNS_POSTGRES_MIGRATION_SQL,
                "CREATE TABLE IF NOT EXISTS public.listing_replay_runs",
            ),
        ] {
            let ddl = migration.find(first_replay_ddl).unwrap();
            let prefix = &migration[..ddl];
            assert!(prefix.contains("contract_guard") || prefix.contains("$migration_guard$"));
            assert!(!prefix.lines().any(|line| line.starts_with("CREATE TABLE")));
        }
    }

    async fn assert_corrupt_identity_correction_schema_rejected(label: &str, statements: &[&str]) {
        let (database_path, database_url) = unique_sqlite_test_database(label);
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let mut connection = pool.acquire().await.unwrap();
        for statement in statements {
            connection.execute(*statement).await.unwrap();
        }
        drop(connection);
        drop(db);

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("startup must reject corrupt aircraft correction schema"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("immutable aircraft identity correction decisions"),
            "{error}"
        );
        assert!(error.contains("20260819_aircraft_listing_identity_corrections.sqlite.sql"));
        std::fs::remove_file(database_path).unwrap();
    }

    async fn assert_corrupt_visual_source_correction_schema_rejected(
        label: &str,
        statements: &[&str],
    ) {
        let (database_path, database_url) = unique_sqlite_test_database(label);
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let mut connection = pool.acquire().await.unwrap();
        for statement in statements {
            connection.execute(*statement).await.unwrap();
        }
        drop(connection);
        drop(db);

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("startup must reject corrupt visual source correction schema"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("immutable pinned artifact and receipt gate"),
            "{error}"
        );
        assert!(error.contains("20260821_aircraft_visual_source_corrections.sqlite.sql"));
        std::fs::remove_file(database_path).unwrap();
    }

    async fn assert_corrupt_reference_cutover_schema_rejected(label: &str, statements: &[&str]) {
        let (database_path, database_url) = unique_sqlite_test_database(label);
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let mut connection = pool.acquire().await.unwrap();
        for statement in statements {
            connection.execute(*statement).await.unwrap();
        }
        drop(connection);
        drop(db);

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("startup must reject corrupt reference-cutover schema"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("canonical price-basis and complete-fact-set contract"),
            "{error}"
        );
        assert!(error.contains("20260819_reference_catalog_cutover.sqlite.sql"));
        std::fs::remove_file(database_path).unwrap();
    }

    async fn assert_unexpected_reference_cutover_object_rejected_without_healing(
        label: &str,
        create_statement: &str,
        object_type: &str,
        object_name: &str,
    ) {
        let (database_path, database_url) = unique_sqlite_test_database(label);
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute(create_statement).await.unwrap();
        let definition_before: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE type = ? AND name = ?")
                .bind(object_type)
                .bind(object_name)
                .fetch_one(pool)
                .await
                .unwrap();
        drop(db);

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("startup must reject unexpected protected reference object"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("canonical price-basis and complete-fact-set contract"),
            "{error}"
        );

        let inspection_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let definition_after: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE type = ? AND name = ?")
                .bind(object_type)
                .bind(object_name)
                .fetch_one(&inspection_pool)
                .await
                .unwrap();
        assert_eq!(
            definition_after, definition_before,
            "startup must not heal or replace the unexpected object"
        );
        inspection_pool.close().await;
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn startup_rejects_missing_or_mutated_reference_cutover_objects() {
        for (label, statements) in [
            (
                "reference-cutover-missing-table",
                vec!["DROP TABLE official_dollar_normalization_facts"],
            ),
            (
                "reference-cutover-renamed-anchor",
                vec!["ALTER TABLE official_dollar_normalization_facts RENAME TO official_dollar_normalization_facts_renamed"],
            ),
            (
                "reference-cutover-missing-trigger",
                vec!["DROP TRIGGER aircraft_reference_versions_publish"],
            ),
            (
                "reference-cutover-mutated-trigger",
                vec![
                    "DROP TRIGGER aircraft_reference_versions_publish",
                    "CREATE TRIGGER aircraft_reference_versions_publish BEFORE UPDATE OF publication_state ON aircraft_reference_configuration_versions BEGIN SELECT 1; END",
                ],
            ),
            (
                "reference-cutover-invalid-price-column",
                vec![
                    "ALTER TABLE aircraft_reference_prices RENAME TO aircraft_reference_prices_valid",
                    "CREATE TABLE aircraft_reference_prices (id INTEGER PRIMARY KEY, configuration_basis INTEGER NOT NULL DEFAULT 0 CHECK (configuration_basis >= 0))",
                ],
            ),
        ] {
            assert_corrupt_reference_cutover_schema_rejected(label, &statements).await;
        }
    }

    #[tokio::test]
    async fn startup_rejects_every_unexpected_protected_reference_trigger_and_index_without_healing(
    ) {
        for (label, create_statement, object_type, object_name) in [
            (
                "reference-price-unexpected-trigger",
                "CREATE TRIGGER unexpected_reference_price_trigger BEFORE INSERT ON aircraft_reference_prices BEGIN SELECT 1; END",
                "trigger",
                "unexpected_reference_price_trigger",
            ),
            (
                "reference-version-unexpected-trigger",
                "CREATE TRIGGER unexpected_reference_version_trigger BEFORE INSERT ON aircraft_reference_configuration_versions BEGIN SELECT 1; END",
                "trigger",
                "unexpected_reference_version_trigger",
            ),
            (
                "reference-fact-set-unexpected-trigger",
                "CREATE TRIGGER unexpected_reference_fact_set_trigger BEFORE INSERT ON aircraft_reference_fact_set_attestations BEGIN SELECT 1; END",
                "trigger",
                "unexpected_reference_fact_set_trigger",
            ),
            (
                "reference-normalization-unexpected-trigger",
                "CREATE TRIGGER unexpected_reference_normalization_trigger BEFORE INSERT ON official_dollar_normalization_facts BEGIN SELECT 1; END",
                "trigger",
                "unexpected_reference_normalization_trigger",
            ),
            (
                "verification-run-unexpected-trigger",
                "CREATE TRIGGER unexpected_verification_run_trigger BEFORE INSERT ON listing_verification_run_items BEGIN SELECT 1; END",
                "trigger",
                "unexpected_verification_run_trigger",
            ),
            (
                "reference-price-unexpected-index",
                "CREATE INDEX unexpected_reference_price_index ON aircraft_reference_prices(amount)",
                "index",
                "unexpected_reference_price_index",
            ),
            (
                "reference-version-unexpected-index",
                "CREATE INDEX unexpected_reference_version_index ON aircraft_reference_configuration_versions(model_year)",
                "index",
                "unexpected_reference_version_index",
            ),
            (
                "reference-scope-unexpected-partial-index",
                "CREATE INDEX unexpected_reference_scope_partial_index ON aircraft_reference_applicability_scopes(aircraft_market_id) WHERE applies_to_all_serials = 1",
                "index",
                "unexpected_reference_scope_partial_index",
            ),
            (
                "reference-fact-set-unexpected-index",
                "CREATE INDEX unexpected_reference_fact_set_index ON aircraft_reference_fact_set_attestations(evidence_claim_id)",
                "index",
                "unexpected_reference_fact_set_index",
            ),
            (
                "reference-normalization-unexpected-index",
                "CREATE INDEX unexpected_reference_normalization_index ON official_dollar_normalization_facts(index_series)",
                "index",
                "unexpected_reference_normalization_index",
            ),
            (
                "verification-run-unexpected-index",
                "CREATE INDEX unexpected_verification_run_index ON listing_verification_run_items(reason_code)",
                "index",
                "unexpected_verification_run_index",
            ),
        ] {
            assert_unexpected_reference_cutover_object_rejected_without_healing(
                label,
                create_statement,
                object_type,
                object_name,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn startup_rejects_weakened_verification_run_status_check_without_healing() {
        let (database_path, database_url) =
            unique_sqlite_test_database("verification-run-weakened-check");
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let canonical =
            sqlite_migration_definition("TABLE", "listing_verification_run_items").unwrap();
        let weakened = canonical.replace(
            "'verified', 'pending_review',\n      'blocked', 'failed'",
            "'verified', 'pending_review', 'pending_reference',\n      'blocked', 'failed'",
        );
        assert_ne!(weakened, canonical);
        sqlx::raw_sql(
            "DROP INDEX idx_listing_verification_run_items_one_active_listing; \
             DROP INDEX idx_listing_verification_run_items_one_running_per_run; \
             DROP INDEX idx_listing_verification_run_items_claim; \
             ALTER TABLE listing_verification_run_items \
               RENAME TO weakened_listing_verification_run_items;",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::raw_sql(&weakened).execute(pool).await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO listing_verification_run_items \
             SELECT * FROM weakened_listing_verification_run_items; \
             DROP TABLE weakened_listing_verification_run_items; \
             CREATE UNIQUE INDEX idx_listing_verification_run_items_one_active_listing \
               ON listing_verification_run_items(listing_id) \
               WHERE status IN ('queued','running'); \
             CREATE UNIQUE INDEX idx_listing_verification_run_items_one_running_per_run \
               ON listing_verification_run_items(run_id) WHERE status='running'; \
             CREATE INDEX idx_listing_verification_run_items_claim \
               ON listing_verification_run_items(run_id,status,position,id);",
        )
        .execute(pool)
        .await
        .unwrap();
        drop(db);

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("startup must reject the weakened run-item CHECK"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("canonical price-basis and complete-fact-set contract"));
        let inspection_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let preserved: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema \
             WHERE type='table' AND name='listing_verification_run_items'",
        )
        .fetch_one(&inspection_pool)
        .await
        .unwrap();
        assert!(preserved.contains("pending_reference"));
        inspection_pool.close().await;
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn startup_rejects_same_name_nonunique_or_reordered_correction_indexes() {
        assert_corrupt_identity_correction_schema_rejected(
            "aircraft-correction-nonunique-capture-index",
            &[
                "DROP INDEX uq_plugin_submissions_signed_capture",
                "CREATE INDEX uq_plugin_submissions_signed_capture ON plugin_submissions (source_url)",
            ],
        )
        .await;
        assert_corrupt_identity_correction_schema_rejected(
            "aircraft-correction-reordered-receipt-index",
            &[
                "DROP INDEX uq_aircraft_listing_identity_correction_receipt",
                "CREATE UNIQUE INDEX uq_aircraft_listing_identity_correction_receipt ON aircraft_listing_identity_correction_decisions (correction_kind, plugin_submission_id)",
            ],
        )
        .await;
    }

    #[test]
    fn visual_source_correction_migrations_and_fresh_schemas_share_the_exact_contract() {
        for definition in [
            SQLITE_SCHEMA_SQL,
            POSTGRES_SCHEMA_SQL,
            AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_SQLITE_MIGRATION_SQL,
            AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(definition.contains(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_MIGRATION));
            assert!(definition.contains(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_FINGERPRINT));
            assert!(definition.contains("aircraft_source_visual_correction_artifacts"));
            assert!(definition.contains("aircraft_source_visual_artifacts_validate_insert"));
            assert!(definition.contains("correction_kind IN ('faa_serial', 'visual_identifier')"));
        }
        for definition in [
            POSTGRES_SCHEMA_SQL,
            AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_POSTGRES_MIGRATION_SQL,
        ] {
            for constraint in [
                "aircraft_source_visual_correction_artifacts_pkey",
                "visual_source_artifact_submission_fk",
                "visual_source_artifact_snapshot_fk",
                "visual_source_artifact_observed_coverage_fk",
                "visual_source_artifact_corrected_aircraft_fk",
                "visual_source_artifact_record_aircraft_fk",
                "visual_source_artifact_rendered_sha256_check",
                "visual_source_artifact_archive_sha256_check",
                "visual_source_artifact_record_sha256_check",
                "visual_source_artifact_photo_sha256_check",
                "visual_source_artifact_resolution_sha256_check",
                "visual_source_artifact_resolution_json_check",
            ] {
                assert!(definition.contains(constraint), "missing {constraint}");
            }
            assert!(definition.contains("length(visual_resolution_json) BETWEEN 2 AND 65536"));
            assert!(definition.contains("'^[0-9a-f]{64}$'"));
        }
        assert_eq!(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_VERSION, 1);
        assert!(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_SQLITE_MIGRATION_SQL
            .starts_with("PRAGMA foreign_keys = ON;\nBEGIN IMMEDIATE;"));
        assert!(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_SQLITE_MIGRATION_SQL.ends_with("COMMIT;\n"));
        assert!(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_POSTGRES_MIGRATION_SQL.starts_with("BEGIN;\n"));
        assert!(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_POSTGRES_MIGRATION_SQL.ends_with("COMMIT;\n"));
    }

    #[tokio::test]
    async fn sqlite_visual_source_correction_migration_upgrades_and_reruns_atomically() {
        let (database_path, database_url) =
            unique_sqlite_test_database("visual-source-correction-migration");
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::raw_sql(
            "DROP TRIGGER aircraft_source_visual_artifacts_validate_insert; \
             DROP TRIGGER aircraft_source_visual_artifacts_immutable_update; \
             DROP TRIGGER aircraft_source_visual_artifacts_immutable_delete; \
             DROP TABLE aircraft_source_visual_correction_artifacts; \
             DELETE FROM schema_migration_contracts \
             WHERE migration_name = '20260821_aircraft_visual_source_corrections';",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::raw_sql(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_SQLITE_MIGRATION_SQL)
            .execute(pool)
            .await
            .unwrap();
        let installed_at: String = sqlx::query_scalar(
            "SELECT installed_at FROM schema_migration_contracts WHERE migration_name = ?",
        )
        .bind(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::raw_sql(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_SQLITE_MIGRATION_SQL)
            .execute(pool)
            .await
            .unwrap();
        let installed_at_after: String = sqlx::query_scalar(
            "SELECT installed_at FROM schema_migration_contracts WHERE migration_name = ?",
        )
        .bind(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(installed_at_after, installed_at);
        drop(db);
        AppDb::connect(&database_url)
            .await
            .expect("the upgraded exact contract must pass startup")
            .close()
            .await;
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn startup_rejects_visual_source_correction_contract_tampering() {
        assert_corrupt_visual_source_correction_schema_rejected(
            "visual-source-correction-noop-validation",
            &[
                "DROP TRIGGER aircraft_source_visual_artifacts_validate_insert",
                "CREATE TRIGGER aircraft_source_visual_artifacts_validate_insert BEFORE INSERT ON aircraft_source_visual_correction_artifacts BEGIN SELECT 1; END",
            ],
        )
        .await;
        assert_corrupt_visual_source_correction_schema_rejected(
            "visual-source-correction-noop-receipt-gate",
            &[
                "DROP TRIGGER aircraft_source_identity_receipt_gate",
                "CREATE TRIGGER aircraft_source_identity_receipt_gate BEFORE UPDATE OF ingestion_state, ingestion_error, is_verified ON aircraft_sale_listings BEGIN SELECT 1; END",
            ],
        )
        .await;
        assert_corrupt_visual_source_correction_schema_rejected(
            "visual-source-correction-weakened-receipt-gate",
            &[
                "DROP TRIGGER aircraft_source_identity_receipt_gate",
                "CREATE TRIGGER aircraft_source_identity_receipt_gate BEFORE UPDATE OF ingestion_state, ingestion_error, is_verified ON aircraft_sale_listings WHEN OLD.ingestion_error = 'source_identity_correction_receipt_pending' AND (NEW.ingestion_error IS NOT OLD.ingestion_error OR NEW.ingestion_state IS NOT OLD.ingestion_state OR NEW.is_verified IS NOT OLD.is_verified) AND 1 = 0 AND NOT EXISTS (SELECT 1 FROM aircraft_listing_identity_correction_decisions decision JOIN plugin_submissions submission ON submission.id = decision.plugin_submission_id WHERE decision.aircraft_sale_listing_id = OLD.id AND decision.correction_kind IN ('faa_serial', 'visual_identifier') AND decision.rendered_html_sha256 = submission.rendered_html_sha256 AND submission.user_id = OLD.created_by_user_id AND submission.canonical_listing_id = OLD.id AND submission.extraction_error IS NULL AND NEW.registration_number IS decision.corrected_registration_number AND NEW.serial_number IS decision.corrected_serial_number) BEGIN SELECT RAISE(ABORT, 'source identity correction receipt is required before leaving the receipt gate'); END",
            ],
        )
        .await;
        assert_corrupt_visual_source_correction_schema_rejected(
            "visual-source-correction-weakened-table",
            &[
                "DROP TRIGGER aircraft_source_visual_artifacts_validate_insert",
                "DROP TRIGGER aircraft_source_visual_artifacts_immutable_update",
                "DROP TRIGGER aircraft_source_visual_artifacts_immutable_delete",
                "DROP TABLE aircraft_source_visual_correction_artifacts",
                "CREATE TABLE aircraft_source_visual_correction_artifacts (plugin_submission_id INTEGER PRIMARY KEY)",
            ],
        )
        .await;
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_startup_rejects_visual_artifact_constraint_tampering() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let initialized = reset_isolated_postgres(&database_url).await;
        initialized.close().await;

        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        let receipt_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM public.schema_migration_contracts \
             WHERE migration_name = $1 AND contract_fingerprint = $2",
        )
        .bind(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_MIGRATION)
        .bind(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_CONTRACT_FINGERPRINT)
        .fetch_one(pool)
        .await
        .unwrap();
        let trigger_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_catalog.pg_trigger trigger_row \
             WHERE trigger_row.tgrelid = pg_catalog.to_regclass(\
               'public.aircraft_source_visual_correction_artifacts'\
             ) AND NOT trigger_row.tgisinternal",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(receipt_count, 1);
        assert_eq!(trigger_count, 2);
        pool.execute(
            "ALTER TABLE public.aircraft_source_visual_correction_artifacts \
             DROP CONSTRAINT visual_source_artifact_resolution_json_check",
        )
        .await
        .unwrap();
        drop(db);

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("tampered PostgreSQL artifact table must fail startup"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("20260821_aircraft_visual_source_corrections.postgres.sql"));
        assert!(error.contains("signed visual aircraft corrections"));
    }

    #[tokio::test]
    async fn correction_preflight_does_not_treat_a_partial_anchor_set_as_fresh() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_listing_identity_correction_decisions (id INTEGER PRIMARY KEY)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("immutable aircraft identity correction decisions"));
        assert!(error.contains("20260819_aircraft_listing_identity_corrections.sqlite.sql"));
    }

    #[tokio::test]
    async fn startup_rejects_noop_correction_immutability_and_receipt_triggers() {
        for (label, statements) in [
            (
                "aircraft-correction-noop-decision-update",
                [
                    "DROP TRIGGER aircraft_listing_identity_corrections_immutable_update",
                    "CREATE TRIGGER aircraft_listing_identity_corrections_immutable_update BEFORE UPDATE ON aircraft_listing_identity_correction_decisions BEGIN SELECT 1; END",
                ],
            ),
            (
                "aircraft-correction-noop-decision-delete",
                [
                    "DROP TRIGGER aircraft_listing_identity_corrections_immutable_delete",
                    "CREATE TRIGGER aircraft_listing_identity_corrections_immutable_delete BEFORE DELETE ON aircraft_listing_identity_correction_decisions BEGIN SELECT 1; END",
                ],
            ),
            (
                "aircraft-correction-noop-observation-update",
                [
                    "DROP TRIGGER aircraft_identity_correction_observation_immutable_update",
                    "CREATE TRIGGER aircraft_identity_correction_observation_immutable_update BEFORE UPDATE ON aircraft_identity_observations BEGIN SELECT 1; END",
                ],
            ),
            (
                "aircraft-correction-noop-observation-delete",
                [
                    "DROP TRIGGER aircraft_identity_correction_observation_immutable_delete",
                    "CREATE TRIGGER aircraft_identity_correction_observation_immutable_delete BEFORE DELETE ON aircraft_identity_observations BEGIN SELECT 1; END",
                ],
            ),
        ] {
            assert_corrupt_identity_correction_schema_rejected(label, &statements).await;
        }
    }

    #[tokio::test]
    async fn startup_rejects_semantically_weakened_correction_triggers() {
        for (label, statements) in [
            (
                "aircraft-correction-decision-when-false",
                [
                    "DROP TRIGGER aircraft_listing_identity_corrections_immutable_update",
                    "CREATE TRIGGER aircraft_listing_identity_corrections_immutable_update BEFORE UPDATE ON aircraft_listing_identity_correction_decisions WHEN 0 BEGIN SELECT RAISE(ABORT, 'aircraft listing identity correction decisions are immutable'); END",
                ],
            ),
            (
                "aircraft-correction-observation-inverted",
                [
                    "DROP TRIGGER aircraft_identity_correction_observation_immutable_update",
                    "CREATE TRIGGER aircraft_identity_correction_observation_immutable_update BEFORE UPDATE ON aircraft_identity_observations WHEN NOT EXISTS (SELECT 1 FROM aircraft_listing_identity_correction_decisions decision WHERE decision.observation_id = OLD.id) BEGIN SELECT RAISE(ABORT, 'aircraft identity observations referenced by correction decisions are immutable'); END",
                ],
            ),
            (
                "aircraft-correction-decision-altered-body",
                [
                    "DROP TRIGGER aircraft_listing_identity_corrections_immutable_delete",
                    "CREATE TRIGGER aircraft_listing_identity_corrections_immutable_delete BEFORE DELETE ON aircraft_listing_identity_correction_decisions BEGIN SELECT RAISE(ABORT, 'a different error'); END",
                ],
            ),
        ] {
            assert_corrupt_identity_correction_schema_rejected(label, &statements).await;
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_canonical_schema_passes_end_to_end_startup() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let installed_at_before: String = sqlx::query_scalar(
            "SELECT installed_at FROM public.schema_migration_contracts WHERE migration_name = $1",
        )
        .bind(REFERENCE_CATALOG_CUTOVER_MIGRATION)
        .fetch_one(&pool)
        .await
        .unwrap();
        let prices_before: String = sqlx::query_scalar(
            "SELECT COALESCE(jsonb_agg(to_jsonb(price_row) ORDER BY id), '[]'::jsonb)::text FROM public.aircraft_reference_prices price_row",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let price_sequence_before: String = sqlx::query_scalar(
            "SELECT last_value::text || ':' || is_called::text FROM public.aircraft_reference_prices_id_seq",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut migration_connection = pool.acquire().await.unwrap();
        for statement in split_sql_statements(REFERENCE_CATALOG_CUTOVER_POSTGRES_MIGRATION_SQL) {
            migration_connection.execute(statement).await.unwrap();
        }
        drop(migration_connection);
        let prices_after: String = sqlx::query_scalar(
            "SELECT COALESCE(jsonb_agg(to_jsonb(price_row) ORDER BY id), '[]'::jsonb)::text FROM public.aircraft_reference_prices price_row",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let price_sequence_after: String = sqlx::query_scalar(
            "SELECT last_value::text || ':' || is_called::text FROM public.aircraft_reference_prices_id_seq",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(prices_after, prices_before);
        assert_eq!(price_sequence_after, price_sequence_before);
        let preflight = AppDb {
            backend: DatabaseBackend::Postgres(pool),
        };
        assert!(preflight
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        let db = AppDb::connect(&database_url).await.unwrap();
        db.ensure_required_migrations().await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        let installed_at_after: String = sqlx::query_scalar(
            "SELECT installed_at FROM public.schema_migration_contracts WHERE migration_name = $1",
        )
        .bind(REFERENCE_CATALOG_CUTOVER_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(installed_at_after, installed_at_before);
    }

    async fn assert_postgres_reference_migration_rerun_rejected(pool: &sqlx::PgPool) -> String {
        let mut migration_connection = pool.acquire().await.unwrap();
        let mut migration_error = None;
        for statement in split_sql_statements(REFERENCE_CATALOG_CUTOVER_POSTGRES_MIGRATION_SQL) {
            if let Err(error) = migration_connection.execute(statement).await {
                migration_error = Some(error.to_string());
                break;
            }
        }
        migration_connection.execute("ROLLBACK").await.unwrap();
        let migration_error =
            migration_error.expect("marker-present cutover corruption must reject migration rerun");
        assert!(
            migration_error.contains("marker-present owned-object mismatch"),
            "{migration_error}"
        );
        migration_error
    }

    async fn postgres_reference_owned_object_snapshot(
        pool: &sqlx::PgPool,
    ) -> (i64, Option<String>) {
        let owned_objects_query = postgres_reference_owned_objects_query().unwrap();
        let snapshot_query = format!(
            "SELECT count(*), pg_catalog.md5(pg_catalog.string_agg(\
             object_key || '=' || definition, E'\\n' ORDER BY object_key)) \
             FROM ({owned_objects_query}) owned_object(object_key, definition)"
        );
        sqlx::query_as(&snapshot_query)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn assert_postgres_reference_null_marker_rejected(
        database_url: &str,
        null_assignment: &str,
        runner_name: &str,
        runner_sql: &str,
    ) {
        let reset = reset_isolated_postgres(database_url).await;
        reset.close().await;
        let db = AppDb::connect(database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute(
            "ALTER TABLE public.schema_migration_contracts \
             ALTER COLUMN contract_version DROP NOT NULL, \
             ALTER COLUMN contract_fingerprint DROP NOT NULL",
        )
        .await
        .unwrap();
        let update_marker = format!(
            "UPDATE public.schema_migration_contracts SET {null_assignment} \
             WHERE migration_name = '{REFERENCE_CATALOG_CUTOVER_MIGRATION}'"
        );
        pool.execute(update_marker.as_str()).await.unwrap();
        let marker_before = sqlx::query_as::<_, (Option<i32>, Option<String>, String)>(
            "SELECT contract_version, contract_fingerprint, installed_at \
             FROM public.schema_migration_contracts WHERE migration_name = $1",
        )
        .bind(REFERENCE_CATALOG_CUTOVER_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();
        let objects_before = postgres_reference_owned_object_snapshot(pool).await;

        let mut connection = pool.acquire().await.unwrap();
        let mut rejection = None;
        for statement in split_sql_statements(runner_sql) {
            if let Err(error) = connection.execute(statement).await {
                rejection = Some(error.to_string());
                break;
            }
        }
        let rejection = rejection.unwrap_or_else(|| {
            panic!("{runner_name}: NULL cutover marker must reject before transition DDL")
        });
        let _ = connection.execute("ROLLBACK").await;
        drop(connection);
        assert!(
            rejection.contains("reference catalog cutover contract marker mismatch"),
            "{runner_name}: {rejection}"
        );
        let marker_after = sqlx::query_as::<_, (Option<i32>, Option<String>, String)>(
            "SELECT contract_version, contract_fingerprint, installed_at \
             FROM public.schema_migration_contracts WHERE migration_name = $1",
        )
        .bind(REFERENCE_CATALOG_CUTOVER_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(marker_after, marker_before, "{runner_name}: marker changed");
        assert_eq!(
            postgres_reference_owned_object_snapshot(pool).await,
            objects_before,
            "{runner_name}: rejected rerun changed the protected object closure"
        );
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_reference_cutover_rejects_null_marker_fields_without_healing() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        for (case_name, null_assignment) in [
            ("null-version", "contract_version = NULL"),
            ("null-fingerprint", "contract_fingerprint = NULL"),
            (
                "null-version-and-fingerprint",
                "contract_version = NULL, contract_fingerprint = NULL",
            ),
        ] {
            for (runner_name, runner_sql) in [
                ("canonical-schema", POSTGRES_SCHEMA_SQL),
                (
                    "explicit-migration",
                    REFERENCE_CATALOG_CUTOVER_POSTGRES_MIGRATION_SQL,
                ),
            ] {
                assert_postgres_reference_null_marker_rejected(
                    &database_url,
                    null_assignment,
                    &format!("{case_name}-{runner_name}"),
                    runner_sql,
                )
                .await;
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_reference_cutover_validation_rejects_adversarial_mutations() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let db = AppDb {
            backend: DatabaseBackend::Postgres(pool.clone()),
        };
        assert!(db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());

        pool.execute(
            "CREATE FUNCTION public.unexpected_reference_cutover_trigger() RETURNS TRIGGER LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END' SET search_path = pg_catalog",
        )
        .await
        .unwrap();
        for (object_name, create_statement, drop_statement, object_query) in [
            (
                "unexpected_reference_price_trigger",
                "CREATE TRIGGER unexpected_reference_price_trigger BEFORE INSERT ON public.aircraft_reference_prices FOR EACH ROW EXECUTE FUNCTION public.unexpected_reference_cutover_trigger()",
                "DROP TRIGGER unexpected_reference_price_trigger ON public.aircraft_reference_prices",
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger WHERE tgrelid = 'public.aircraft_reference_prices'::regclass AND tgname = 'unexpected_reference_price_trigger' AND NOT tgisinternal)",
            ),
            (
                "unexpected_reference_version_trigger",
                "CREATE TRIGGER unexpected_reference_version_trigger BEFORE INSERT ON public.aircraft_reference_configuration_versions FOR EACH ROW EXECUTE FUNCTION public.unexpected_reference_cutover_trigger()",
                "DROP TRIGGER unexpected_reference_version_trigger ON public.aircraft_reference_configuration_versions",
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger WHERE tgrelid = 'public.aircraft_reference_configuration_versions'::regclass AND tgname = 'unexpected_reference_version_trigger' AND NOT tgisinternal)",
            ),
            (
                "unexpected_reference_fact_set_trigger",
                "CREATE TRIGGER unexpected_reference_fact_set_trigger BEFORE INSERT ON public.aircraft_reference_fact_set_attestations FOR EACH ROW EXECUTE FUNCTION public.unexpected_reference_cutover_trigger()",
                "DROP TRIGGER unexpected_reference_fact_set_trigger ON public.aircraft_reference_fact_set_attestations",
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger WHERE tgrelid = 'public.aircraft_reference_fact_set_attestations'::regclass AND tgname = 'unexpected_reference_fact_set_trigger' AND NOT tgisinternal)",
            ),
            (
                "unexpected_reference_normalization_trigger",
                "CREATE TRIGGER unexpected_reference_normalization_trigger BEFORE INSERT ON public.official_dollar_normalization_facts FOR EACH ROW EXECUTE FUNCTION public.unexpected_reference_cutover_trigger()",
                "DROP TRIGGER unexpected_reference_normalization_trigger ON public.official_dollar_normalization_facts",
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger WHERE tgrelid = 'public.official_dollar_normalization_facts'::regclass AND tgname = 'unexpected_reference_normalization_trigger' AND NOT tgisinternal)",
            ),
            (
                "unexpected_verification_run_trigger",
                "CREATE TRIGGER unexpected_verification_run_trigger BEFORE INSERT ON public.listing_verification_run_items FOR EACH ROW EXECUTE FUNCTION public.unexpected_reference_cutover_trigger()",
                "DROP TRIGGER unexpected_verification_run_trigger ON public.listing_verification_run_items",
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger WHERE tgrelid = 'public.listing_verification_run_items'::regclass AND tgname = 'unexpected_verification_run_trigger' AND NOT tgisinternal)",
            ),
            (
                "unexpected_reference_price_index",
                "CREATE INDEX unexpected_reference_price_index ON public.aircraft_reference_prices(amount)",
                "DROP INDEX public.unexpected_reference_price_index",
                "SELECT pg_catalog.to_regclass('public.unexpected_reference_price_index') IS NOT NULL",
            ),
            (
                "unexpected_reference_version_index",
                "CREATE INDEX unexpected_reference_version_index ON public.aircraft_reference_configuration_versions(model_year)",
                "DROP INDEX public.unexpected_reference_version_index",
                "SELECT pg_catalog.to_regclass('public.unexpected_reference_version_index') IS NOT NULL",
            ),
            (
                "unexpected_reference_scope_partial_index",
                "CREATE INDEX unexpected_reference_scope_partial_index ON public.aircraft_reference_applicability_scopes(aircraft_market_id) WHERE applies_to_all_serials",
                "DROP INDEX public.unexpected_reference_scope_partial_index",
                "SELECT pg_catalog.to_regclass('public.unexpected_reference_scope_partial_index') IS NOT NULL",
            ),
            (
                "unexpected_reference_fact_set_index",
                "CREATE INDEX unexpected_reference_fact_set_index ON public.aircraft_reference_fact_set_attestations(evidence_claim_id)",
                "DROP INDEX public.unexpected_reference_fact_set_index",
                "SELECT pg_catalog.to_regclass('public.unexpected_reference_fact_set_index') IS NOT NULL",
            ),
            (
                "unexpected_reference_normalization_index",
                "CREATE INDEX unexpected_reference_normalization_index ON public.official_dollar_normalization_facts(index_series)",
                "DROP INDEX public.unexpected_reference_normalization_index",
                "SELECT pg_catalog.to_regclass('public.unexpected_reference_normalization_index') IS NOT NULL",
            ),
            (
                "unexpected_verification_run_index",
                "CREATE INDEX unexpected_verification_run_index ON public.listing_verification_run_items(reason_code)",
                "DROP INDEX public.unexpected_verification_run_index",
                "SELECT pg_catalog.to_regclass('public.unexpected_verification_run_index') IS NOT NULL",
            ),
        ] {
            pool.execute(create_statement).await.unwrap();
            assert!(
                !db.reference_catalog_cutover_definitions_valid()
                    .await
                    .unwrap(),
                "runtime must reject {object_name}"
            );
            assert_postgres_reference_migration_rerun_rejected(&pool).await;
            let startup_error = match AppDb::connect(&database_url).await {
                Ok(_) => panic!("startup must reject {object_name}"),
                Err(error) => format!("{error:#}"),
            };
            assert!(
                startup_error.contains("canonical price-basis and complete-fact-set contract"),
                "{startup_error}"
            );
            assert!(
                sqlx::query_scalar::<_, bool>(object_query)
                    .fetch_one(&pool)
                    .await
                    .unwrap(),
                "startup and migration rerun must not heal {object_name}"
            );
            pool.execute(drop_statement).await.unwrap();
            assert!(db
                .reference_catalog_cutover_definitions_valid()
                .await
                .unwrap());
        }

        pool.execute(
            "CREATE FUNCTION public.require_approved_default_avionics_model(TEXT) \
             RETURNS TEXT LANGUAGE sql IMMUTABLE AS 'SELECT $1' \
             SET search_path = pg_catalog",
        )
        .await
        .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        assert_postgres_reference_migration_rerun_rejected(&pool).await;
        let startup_error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("startup must reject a retired routine overload"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            startup_error.contains("canonical price-basis and complete-fact-set contract"),
            "{startup_error}"
        );
        let retired_overload_preserved: bool = sqlx::query_scalar(
            "SELECT pg_catalog.to_regprocedure(\
               'public.require_approved_default_avionics_model(pg_catalog.text)'\
             ) IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(retired_overload_preserved);
        pool.execute("DROP FUNCTION public.require_approved_default_avionics_model(TEXT)")
            .await
            .unwrap();

        pool.execute(
            "ALTER TABLE public.listing_verification_run_items \
             RENAME CONSTRAINT listing_verification_run_items_status_check \
             TO unexpected_listing_verification_run_items_status_check",
        )
        .await
        .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        assert_postgres_reference_migration_rerun_rejected(&pool).await;
        let renamed_constraint_preserved: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint \
             WHERE conrelid = 'public.listing_verification_run_items'::regclass \
               AND conname = 'unexpected_listing_verification_run_items_status_check')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(renamed_constraint_preserved);
        pool.execute(
            "ALTER TABLE public.listing_verification_run_items \
             RENAME CONSTRAINT unexpected_listing_verification_run_items_status_check \
             TO listing_verification_run_items_status_check",
        )
        .await
        .unwrap();

        pool.execute(
            "ALTER TABLE public.listing_verification_run_items \
             DROP CONSTRAINT listing_verification_run_items_completion_check, \
             ADD CONSTRAINT listing_verification_run_items_completion_check CHECK (\
               (status IN ('queued', 'running') AND completed_at IS NULL) OR \
               (status IN ('verified', 'pending_review', 'blocked', 'failed', 'cancelled') \
                 AND completed_at IS NOT NULL)\
             ) NOT VALID",
        )
        .await
        .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        assert_postgres_reference_migration_rerun_rejected(&pool).await;
        let unvalidated_constraint_preserved: bool = sqlx::query_scalar(
            "SELECT NOT convalidated FROM pg_catalog.pg_constraint \
             WHERE conrelid = 'public.listing_verification_run_items'::regclass \
               AND conname = 'listing_verification_run_items_completion_check'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(unvalidated_constraint_preserved);
        pool.execute(
            "ALTER TABLE public.listing_verification_run_items \
             VALIDATE CONSTRAINT listing_verification_run_items_completion_check",
        )
        .await
        .unwrap();

        pool.execute("ALTER SEQUENCE public.aircraft_reference_prices_id_seq CACHE 17")
            .await
            .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        assert_postgres_reference_migration_rerun_rejected(&pool).await;
        let changed_sequence_cache: i64 = sqlx::query_scalar(
            "SELECT seqcache FROM pg_catalog.pg_sequence \
             WHERE seqrelid = 'public.aircraft_reference_prices_id_seq'::regclass",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(changed_sequence_cache, 17);
        pool.execute("ALTER SEQUENCE public.aircraft_reference_prices_id_seq CACHE 1")
            .await
            .unwrap();

        pool.execute(
            "CREATE SEQUENCE public.unexpected_reference_price_owned_sequence \
             OWNED BY public.aircraft_reference_prices.id",
        )
        .await
        .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        assert_postgres_reference_migration_rerun_rejected(&pool).await;
        let unexpected_sequence_ownership_preserved: bool = sqlx::query_scalar(
            "SELECT EXISTS (\
               SELECT 1 FROM pg_catalog.pg_depend \
               WHERE classid = 'pg_catalog.pg_class'::regclass \
                 AND objid = \
                   'public.unexpected_reference_price_owned_sequence'::regclass \
                 AND refclassid = 'pg_catalog.pg_class'::regclass \
                 AND refobjid = 'public.aircraft_reference_prices'::regclass \
                 AND deptype IN ('a', 'i')\
             )",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(unexpected_sequence_ownership_preserved);
        pool.execute("DROP SEQUENCE public.unexpected_reference_price_owned_sequence")
            .await
            .unwrap();
        assert!(db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());

        pool.execute("ALTER TABLE public.aircraft_reference_prices ENABLE ROW LEVEL SECURITY")
            .await
            .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        assert_postgres_reference_migration_rerun_rejected(&pool).await;
        let row_security_preserved: bool = sqlx::query_scalar(
            "SELECT relrowsecurity FROM pg_catalog.pg_class \
             WHERE oid = 'public.aircraft_reference_prices'::regclass",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(row_security_preserved);
        pool.execute("ALTER TABLE public.aircraft_reference_prices DISABLE ROW LEVEL SECURITY")
            .await
            .unwrap();
        assert!(db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());

        pool.execute(
            "ALTER TABLE public.listing_verification_run_items \
             DROP CONSTRAINT listing_verification_run_items_status_check, \
             ADD CONSTRAINT listing_verification_run_items_status_check CHECK (status IN (\
               'queued', 'running', 'verified', 'pending_review', \
               'pending_reference', 'blocked', 'failed', 'cancelled'))",
        )
        .await
        .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        assert_postgres_reference_migration_rerun_rejected(&pool).await;
        let weakened_constraint_preserved: bool = sqlx::query_scalar(
            "SELECT pg_catalog.pg_get_constraintdef(oid) LIKE '%pending_reference%' \
             FROM pg_catalog.pg_constraint \
             WHERE conrelid='public.listing_verification_run_items'::regclass \
               AND conname='listing_verification_run_items_status_check'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(weakened_constraint_preserved);
        pool.execute(
            "ALTER TABLE public.listing_verification_run_items \
             DROP CONSTRAINT listing_verification_run_items_status_check, \
             ADD CONSTRAINT listing_verification_run_items_status_check CHECK (status IN (\
               'queued', 'running', 'verified', 'pending_review', \
               'blocked', 'failed', 'cancelled'))",
        )
        .await
        .unwrap();
        assert!(db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        pool.execute("DROP FUNCTION public.unexpected_reference_cutover_trigger()")
            .await
            .unwrap();

        pool.execute("DROP SCHEMA IF EXISTS reference_cutover_attacker CASCADE")
            .await
            .unwrap();
        pool.execute("CREATE SCHEMA reference_cutover_attacker")
            .await
            .unwrap();
        pool.execute(
            "CREATE FUNCTION reference_cutover_attacker.validate_aircraft_reference_version_update() RETURNS TRIGGER LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'",
        )
        .await
        .unwrap();
        pool.execute("SET search_path = reference_cutover_attacker, public, pg_catalog")
            .await
            .unwrap();
        assert!(db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        pool.execute("RESET search_path").await.unwrap();

        pool.execute(
            "ALTER FUNCTION public.validate_aircraft_reference_version_update() SET search_path = public",
        )
        .await
        .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        pool.execute(
            "ALTER FUNCTION public.validate_aircraft_reference_version_update() SET search_path = pg_catalog",
        )
        .await
        .unwrap();

        pool.execute(
            "ALTER TABLE public.aircraft_reference_configuration_versions DISABLE TRIGGER aircraft_reference_versions_validate_update",
        )
        .await
        .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        pool.execute(
            "ALTER TABLE public.aircraft_reference_configuration_versions ENABLE TRIGGER aircraft_reference_versions_validate_update",
        )
        .await
        .unwrap();

        pool.execute(
            "DROP TRIGGER aircraft_reference_price_building_insert ON public.aircraft_reference_prices",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TRIGGER aircraft_reference_price_building_insert BEFORE INSERT ON public.aircraft_reference_prices FOR EACH ROW EXECUTE FUNCTION public.validate_aircraft_reference_version_update()",
        )
        .await
        .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        assert_postgres_reference_migration_rerun_rejected(&pool).await;
        let startup_error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("startup must reject the wrong protected-trigger function binding"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            startup_error.contains("canonical price-basis and complete-fact-set contract"),
            "{startup_error}"
        );
        let wrong_binding_preserved: bool = sqlx::query_scalar(
            r#"
            SELECT trigger_row.tgfoid =
              'public.validate_aircraft_reference_version_update()'::regprocedure
            FROM pg_catalog.pg_trigger trigger_row
            WHERE trigger_row.tgrelid =
                    'public.aircraft_reference_prices'::regclass
              AND trigger_row.tgname = 'aircraft_reference_price_building_insert'
              AND NOT trigger_row.tgisinternal
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            wrong_binding_preserved,
            "startup and migration rerun must not replace the wrong binding"
        );
        pool.execute(
            "DROP TRIGGER aircraft_reference_price_building_insert ON public.aircraft_reference_prices",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TRIGGER aircraft_reference_price_building_insert BEFORE INSERT ON public.aircraft_reference_prices FOR EACH ROW EXECUTE FUNCTION public.validate_aircraft_reference_child_insert()",
        )
        .await
        .unwrap();

        pool.execute("DROP TABLE public.official_dollar_normalization_facts CASCADE")
            .await
            .unwrap();
        assert!(!db
            .reference_catalog_cutover_definitions_valid()
            .await
            .unwrap());
        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("startup must not heal marker-present PostgreSQL cutover damage"),
            Err(error) => format!("{error:#}"),
        };
        assert!(error.contains("canonical price-basis and complete-fact-set contract"));
        let missing_table: Option<String> = sqlx::query_scalar(
            "SELECT pg_catalog.to_regclass('public.official_dollar_normalization_facts')::text",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(missing_table.is_none());
    }

    async fn install_faa_registry_functions(pool: &sqlx::PgPool) {
        for (function_name, body) in [
            (
                "validate_faa_snapshot_evidence",
                POSTGRES_FAA_SNAPSHOT_EVIDENCE_FUNCTION_SOURCE,
            ),
            (
                "validate_faa_aircraft_reference_reachability",
                POSTGRES_FAA_AIRCRAFT_REFERENCE_REACHABILITY_FUNCTION_SOURCE,
            ),
            (
                "validate_faa_engine_reference_reachability",
                POSTGRES_FAA_ENGINE_REFERENCE_REACHABILITY_FUNCTION_SOURCE,
            ),
            (
                "validate_faa_coverage",
                POSTGRES_FAA_COVERAGE_FUNCTION_SOURCE,
            ),
            (
                "preserve_faa_registry_data",
                POSTGRES_FAA_IMMUTABILITY_FUNCTION_SOURCE,
            ),
        ] {
            let statement = format!(
                "CREATE OR REPLACE FUNCTION public.{function_name}() RETURNS TRIGGER \
                 LANGUAGE plpgsql AS $function${body}$function$ \
                 SET search_path = pg_catalog"
            );
            pool.execute(statement.as_str()).await.unwrap();
        }
    }

    async fn install_faa_registry_triggers(pool: &sqlx::PgPool) {
        for (trigger_name, relation_name, event, function_name) in [
            (
                "faa_registry_snapshots_require_exact_evidence",
                "faa_registry_snapshots",
                "INSERT",
                "validate_faa_snapshot_evidence",
            ),
            (
                "faa_registry_aircraft_references_reachable",
                "faa_registry_aircraft_references",
                "INSERT",
                "validate_faa_aircraft_reference_reachability",
            ),
            (
                "faa_registry_engine_references_reachable",
                "faa_registry_engine_references",
                "INSERT",
                "validate_faa_engine_reference_reachability",
            ),
            (
                "faa_registry_coverage_consistent",
                "faa_registry_coverage",
                "INSERT",
                "validate_faa_coverage",
            ),
            (
                "faa_registry_snapshots_immutable",
                "faa_registry_snapshots",
                "UPDATE OR DELETE",
                "preserve_faa_registry_data",
            ),
            (
                "faa_registry_aircraft_immutable",
                "faa_registry_aircraft",
                "UPDATE OR DELETE",
                "preserve_faa_registry_data",
            ),
            (
                "faa_registry_aircraft_references_immutable",
                "faa_registry_aircraft_references",
                "UPDATE OR DELETE",
                "preserve_faa_registry_data",
            ),
            (
                "faa_registry_engine_references_immutable",
                "faa_registry_engine_references",
                "UPDATE OR DELETE",
                "preserve_faa_registry_data",
            ),
            (
                "faa_registry_coverage_immutable",
                "faa_registry_coverage",
                "UPDATE OR DELETE",
                "preserve_faa_registry_data",
            ),
        ] {
            let drop_statement =
                format!("DROP TRIGGER IF EXISTS {trigger_name} ON public.{relation_name}");
            pool.execute(drop_statement.as_str()).await.unwrap();
            let create_statement = format!(
                "CREATE TRIGGER {trigger_name} BEFORE {event} ON public.{relation_name} \
                 FOR EACH ROW EXECUTE FUNCTION public.{function_name}()"
            );
            pool.execute(create_statement.as_str()).await.unwrap();
        }
    }

    async fn assert_faa_reference_startup_rejected(db: &AppDb) -> String {
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("database migration required before startup"),
            "{error}"
        );
        assert!(
            error.contains("20260819_faa_reference_reachability.postgres.sql")
                || error.contains("20260722_aircraft_reference_catalog.postgres.sql"),
            "{error}"
        );
        assert!(
            error.contains("PostgreSQL FAA")
                || error.contains("migration contract marker")
                || error.contains("clean aircraft identity/reference catalogs"),
            "{error}"
        );
        error
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_faa_reference_startup_attests_exact_objects() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        pool.execute("DROP SCHEMA public CASCADE").await.unwrap();
        pool.execute("CREATE SCHEMA public").await.unwrap();
        let mut connection = pool.acquire().await.unwrap();
        for statement in split_sql_statements(POSTGRES_SCHEMA_SQL) {
            connection.execute(statement).await.unwrap();
        }
        drop(connection);
        let db = AppDb {
            backend: DatabaseBackend::Postgres(pool.clone()),
        };
        assert!(db.faa_registry_contract_valid().await.unwrap());
        db.ensure_required_migrations().await.unwrap();

        pool.execute(
            "ALTER TABLE public.faa_registry_aircraft_references DISABLE TRIGGER \
             faa_registry_aircraft_references_reachable",
        )
        .await
        .unwrap();
        assert_faa_reference_startup_rejected(&db).await;
        pool.execute(
            "ALTER TABLE public.faa_registry_aircraft_references ENABLE TRIGGER \
             faa_registry_aircraft_references_reachable",
        )
        .await
        .unwrap();

        pool.execute(
            "DROP TRIGGER faa_registry_aircraft_references_reachable \
             ON public.faa_registry_aircraft_references",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TRIGGER faa_registry_aircraft_references_reachable BEFORE INSERT \
             ON public.faa_registry_aircraft_references FOR EACH ROW EXECUTE FUNCTION \
             public.validate_faa_engine_reference_reachability()",
        )
        .await
        .unwrap();
        assert_faa_reference_startup_rejected(&db).await;
        install_faa_registry_triggers(&pool).await;

        pool.execute(
            "CREATE OR REPLACE FUNCTION \
             public.validate_faa_aircraft_reference_reachability() RETURNS TRIGGER \
             LANGUAGE plpgsql AS $function$BEGIN RETURN NEW; END;$function$ \
             SET search_path = pg_catalog",
        )
        .await
        .unwrap();
        assert_faa_reference_startup_rejected(&db).await;
        install_faa_registry_functions(&pool).await;

        pool.execute(
            "ALTER FUNCTION public.validate_faa_aircraft_reference_reachability() PARALLEL SAFE",
        )
        .await
        .unwrap();
        assert_faa_reference_startup_rejected(&db).await;
        pool.execute(
            "ALTER FUNCTION public.validate_faa_aircraft_reference_reachability() PARALLEL UNSAFE",
        )
        .await
        .unwrap();

        pool.execute("CREATE SCHEMA attacker_schema").await.unwrap();
        pool.execute(
            "CREATE FUNCTION attacker_schema.validate_faa_aircraft_reference_reachability() \
             RETURNS TRIGGER LANGUAGE plpgsql AS $function$BEGIN RETURN NEW; END;$function$",
        )
        .await
        .unwrap();
        pool.execute(
            "DROP TRIGGER faa_registry_aircraft_references_reachable \
             ON public.faa_registry_aircraft_references",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TRIGGER faa_registry_aircraft_references_reachable BEFORE INSERT \
             ON public.faa_registry_aircraft_references FOR EACH ROW EXECUTE FUNCTION \
             attacker_schema.validate_faa_aircraft_reference_reachability()",
        )
        .await
        .unwrap();
        assert_faa_reference_startup_rejected(&db).await;
        install_faa_registry_triggers(&pool).await;
        pool.execute(
            "ALTER FUNCTION public.validate_faa_snapshot_evidence() \
             SET search_path = attacker_schema, public",
        )
        .await
        .unwrap();
        let error = assert_faa_reference_startup_rejected(&db).await;
        assert!(error.contains("triggers or functions"), "{error}");
        install_faa_registry_functions(&pool).await;
        pool.execute("DROP SCHEMA attacker_schema CASCADE")
            .await
            .unwrap();

        pool.execute(
            "ALTER TABLE public.faa_registry_aircraft_references \
             RENAME TO faa_registry_aircraft_references_missing",
        )
        .await
        .unwrap();
        assert_faa_reference_startup_rejected(&db).await;
        pool.execute(
            "ALTER TABLE public.faa_registry_aircraft_references_missing \
             RENAME TO faa_registry_aircraft_references",
        )
        .await
        .unwrap();

        pool.execute("DROP FUNCTION public.validate_faa_engine_reference_reachability() CASCADE")
            .await
            .unwrap();
        assert_faa_reference_startup_rejected(&db).await;
        install_faa_registry_functions(&pool).await;
        install_faa_registry_triggers(&pool).await;

        pool.execute(
            "ALTER TABLE public.faa_registry_snapshots DISABLE TRIGGER \
             faa_registry_snapshots_require_exact_evidence",
        )
        .await
        .unwrap();
        let error = assert_faa_reference_startup_rejected(&db).await;
        assert!(error.contains("triggers or functions"), "{error}");
        pool.execute(
            "ALTER TABLE public.faa_registry_snapshots ENABLE TRIGGER \
             faa_registry_snapshots_require_exact_evidence",
        )
        .await
        .unwrap();

        pool.execute(
            "CREATE OR REPLACE FUNCTION public.validate_faa_coverage() RETURNS TRIGGER \
             LANGUAGE plpgsql AS $function$BEGIN RETURN NEW; END;$function$ \
             SET search_path = pg_catalog",
        )
        .await
        .unwrap();
        let error = assert_faa_reference_startup_rejected(&db).await;
        assert!(error.contains("triggers or functions"), "{error}");
        install_faa_registry_functions(&pool).await;

        pool.execute(
            "CREATE FUNCTION public.unexpected_faa_trigger_function() RETURNS TRIGGER \
             LANGUAGE plpgsql AS $function$BEGIN RETURN NEW; END;$function$ \
             SET search_path = pg_catalog",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TRIGGER unexpected_faa_trigger BEFORE INSERT \
             ON public.faa_registry_aircraft FOR EACH ROW EXECUTE FUNCTION \
             public.unexpected_faa_trigger_function()",
        )
        .await
        .unwrap();
        let error = assert_faa_reference_startup_rejected(&db).await;
        assert!(error.contains("triggers or functions"), "{error}");
        pool.execute("DROP TRIGGER unexpected_faa_trigger ON public.faa_registry_aircraft")
            .await
            .unwrap();
        pool.execute("DROP FUNCTION public.unexpected_faa_trigger_function()")
            .await
            .unwrap();

        pool.execute(
            "CREATE INDEX unexpected_faa_index \
             ON public.faa_registry_engine_references (model_name)",
        )
        .await
        .unwrap();
        let error = assert_faa_reference_startup_rejected(&db).await;
        assert!(error.contains("registry indexes"), "{error}");
        pool.execute("DROP INDEX public.unexpected_faa_index")
            .await
            .unwrap();

        pool.execute(
            "ALTER TABLE public.faa_registry_coverage ADD CONSTRAINT \
             unexpected_faa_constraint CHECK (length(lookup_status) > 0)",
        )
        .await
        .unwrap();
        let error = assert_faa_reference_startup_rejected(&db).await;
        assert!(error.contains("registry constraints"), "{error}");
        pool.execute(
            "ALTER TABLE public.faa_registry_coverage \
             DROP CONSTRAINT unexpected_faa_constraint",
        )
        .await
        .unwrap();

        pool.execute("DROP INDEX public.idx_faa_registry_coverage_lookup")
            .await
            .unwrap();
        let error = assert_faa_reference_startup_rejected(&db).await;
        assert!(error.contains("registry indexes"), "{error}");
        pool.execute(
            "CREATE INDEX idx_faa_registry_coverage_lookup \
             ON public.faa_registry_coverage (n_number, snapshot_id)",
        )
        .await
        .unwrap();

        pool.execute(
            "ALTER TABLE public.faa_registry_coverage \
             DROP CONSTRAINT faa_registry_coverage_lookup_status_check",
        )
        .await
        .unwrap();
        let error = assert_faa_reference_startup_rejected(&db).await;
        assert!(error.contains("registry constraints"), "{error}");
        pool.execute(
            "ALTER TABLE public.faa_registry_coverage ADD CONSTRAINT \
             faa_registry_coverage_lookup_status_check \
             CHECK (lookup_status IN ('matched', 'absent'))",
        )
        .await
        .unwrap();

        pool.execute("CREATE SCHEMA attacker_schema").await.unwrap();
        pool.execute(
            "CREATE TABLE attacker_schema.faa_registry_snapshots \
             (id BIGINT PRIMARY KEY)",
        )
        .await
        .unwrap();
        pool.execute(
            "ALTER TABLE public.faa_registry_coverage DROP CONSTRAINT \
             faa_registry_coverage_snapshot_id_fkey",
        )
        .await
        .unwrap();
        pool.execute(
            "SET search_path = attacker_schema, public; \
             ALTER TABLE public.faa_registry_coverage ADD CONSTRAINT \
             faa_registry_coverage_snapshot_id_fkey FOREIGN KEY (snapshot_id) \
             REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT; \
             RESET search_path",
        )
        .await
        .unwrap();
        let error = assert_faa_reference_startup_rejected(&db).await;
        assert!(error.contains("registry foreign keys"), "{error}");
        pool.execute(
            "ALTER TABLE public.faa_registry_coverage DROP CONSTRAINT \
             faa_registry_coverage_snapshot_id_fkey; \
             ALTER TABLE public.faa_registry_coverage ADD CONSTRAINT \
             faa_registry_coverage_snapshot_id_fkey FOREIGN KEY (snapshot_id) \
             REFERENCES public.faa_registry_snapshots(id) ON DELETE RESTRICT",
        )
        .await
        .unwrap();
        pool.execute("DROP SCHEMA attacker_schema CASCADE")
            .await
            .unwrap();

        sqlx::query("DELETE FROM public.schema_migration_contracts WHERE migration_name = $1")
            .bind(FAA_REFERENCE_REACHABILITY_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();
        assert_faa_reference_startup_rejected(&db).await;
        sqlx::query(
            "INSERT INTO public.schema_migration_contracts \
             (migration_name, contract_version, contract_fingerprint) VALUES ($1, $2, $3)",
        )
        .bind(FAA_REFERENCE_REACHABILITY_MIGRATION)
        .bind(FAA_REFERENCE_REACHABILITY_CONTRACT_VERSION)
        .bind(FAA_REFERENCE_REACHABILITY_CONTRACT_FINGERPRINT)
        .execute(&pool)
        .await
        .unwrap();

        assert!(db.faa_registry_contract_valid().await.unwrap());
        db.ensure_required_migrations().await.unwrap();
    }

    #[tokio::test]
    async fn sqlite_faa_registry_startup_attests_exact_objects() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        assert!(db.faa_registry_contract_valid().await.unwrap());

        sqlx::query("DELETE FROM schema_migration_contracts WHERE migration_name = ?")
            .bind(FAA_RECORD_HASH_DOMAIN_MIGRATION)
            .execute(pool)
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("explicit immutable domain"), "{error}");
        sqlx::query(
            "INSERT INTO schema_migration_contracts \
             (migration_name, contract_version, contract_fingerprint) VALUES (?, ?, ?)",
        )
        .bind(FAA_RECORD_HASH_DOMAIN_MIGRATION)
        .bind(FAA_RECORD_HASH_DOMAIN_CONTRACT_VERSION)
        .bind(FAA_RECORD_HASH_DOMAIN_CONTRACT_FINGERPRINT)
        .execute(pool)
        .await
        .unwrap();

        pool.execute(
            "CREATE TRIGGER unexpected_faa_trigger BEFORE INSERT \
             ON faa_registry_aircraft BEGIN SELECT 1; END",
        )
        .await
        .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("SQLite FAA registry object set"), "{error}");
        pool.execute("DROP TRIGGER unexpected_faa_trigger")
            .await
            .unwrap();

        pool.execute(
            "CREATE INDEX unexpected_faa_index \
             ON faa_registry_engine_references(model_name)",
        )
        .await
        .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("SQLite FAA registry object set"), "{error}");
        pool.execute("DROP INDEX unexpected_faa_index")
            .await
            .unwrap();

        let evidence_trigger: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' \
             AND name = 'faa_registry_snapshots_require_exact_evidence'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        pool.execute("DROP TRIGGER faa_registry_snapshots_require_exact_evidence")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("SQLite FAA registry object set"), "{error}");
        pool.execute(evidence_trigger.as_str()).await.unwrap();

        let coverage_trigger: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' \
             AND name = 'faa_registry_coverage_consistent'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        pool.execute("DROP TRIGGER faa_registry_coverage_consistent")
            .await
            .unwrap();
        pool.execute(
            "CREATE TRIGGER faa_registry_coverage_consistent BEFORE INSERT \
             ON faa_registry_coverage BEGIN SELECT 1; END",
        )
        .await
        .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("SQLite FAA registry trigger `faa_registry_coverage_consistent`"),
            "{error}"
        );
        pool.execute("DROP TRIGGER faa_registry_coverage_consistent")
            .await
            .unwrap();
        pool.execute(coverage_trigger.as_str()).await.unwrap();

        let coverage_index: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type = 'index' \
             AND name = 'idx_faa_registry_coverage_lookup'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        pool.execute("DROP INDEX idx_faa_registry_coverage_lookup")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("SQLite FAA registry object set"), "{error}");
        pool.execute(coverage_index.as_str()).await.unwrap();

        let coverage_object_names = [
            "faa_registry_coverage_consistent",
            "faa_registry_coverage_immutable_delete",
            "faa_registry_coverage_immutable_update",
            "idx_faa_registry_coverage_lookup",
        ];
        let mut coverage_objects = Vec::new();
        for name in coverage_object_names {
            let sql: String = sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE name = ?")
                .bind(name)
                .fetch_one(pool)
                .await
                .unwrap();
            coverage_objects.push(sql);
        }
        for name in &coverage_object_names[..3] {
            pool.execute(format!("DROP TRIGGER {name}").as_str())
                .await
                .unwrap();
        }
        pool.execute("DROP INDEX idx_faa_registry_coverage_lookup")
            .await
            .unwrap();
        pool.execute("DROP TABLE faa_registry_coverage")
            .await
            .unwrap();
        pool.execute(
            "CREATE TABLE faa_registry_coverage ( \
               snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) \
                 ON DELETE RESTRICT, \
               n_number TEXT NOT NULL, \
               lookup_status TEXT NOT NULL, \
               PRIMARY KEY (snapshot_id, n_number), \
               CHECK (substr(n_number, 1, 1) = 'N' AND length(n_number) BETWEEN 2 AND 6), \
               CHECK (lookup_status IN ('matched', 'absent')), \
               CONSTRAINT unexpected_faa_constraint CHECK (length(lookup_status) > 0) \
             )",
        )
        .await
        .unwrap();
        for sql in coverage_objects {
            pool.execute(sql.as_str()).await.unwrap();
        }
        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("SQLite FAA registry table `faa_registry_coverage`"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn sqlite_faa_registry_startup_rejects_mismatched_record_hash_domain_rows() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute(
            "INSERT INTO curation_evidence_sources (\
               source_url, source_title, source_domain, source_tier, \
               content_sha256, retrieved_at\
             ) VALUES (\
               'https://www.faa.gov/registry/corrupt.zip', 'corrupt fixture', \
               'faa.gov', 'regulator_primary', printf('%064d', 1), '2026-08-20'\
             )",
        )
        .await
        .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        connection
            .execute("PRAGMA ignore_check_constraints = ON")
            .await
            .unwrap();
        connection
            .execute(
                "INSERT INTO faa_registry_snapshots (\
                   evidence_source_id, snapshot_date, source_url, archive_sha256, \
                   source_manifest_sha256, target_set_sha256, \
                   master_member_name, master_member_sha256, \
                   aircraft_member_name, aircraft_member_sha256, \
                   engine_member_name, engine_member_sha256, record_hash_domain\
                 ) SELECT id, '2026-08-20', source_url, content_sha256, \
                   printf('%064d', 2), printf('%064d', 3), \
                   'MASTER.txt', printf('%064d', 4), \
                   'ACFTREF.txt', printf('%064d', 5), \
                   'ENGINE.txt', printf('%064d', 6), 'wrong-domain' \
                 FROM curation_evidence_sources WHERE source_title = 'corrupt fixture'",
            )
            .await
            .unwrap();
        connection
            .execute("PRAGMA ignore_check_constraints = OFF")
            .await
            .unwrap();
        drop(connection);

        let error = db
            .ensure_required_migrations()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("FAA record hash domain values"), "{error}");
    }

    async fn reset_isolated_postgres(database_url: &str) -> sqlx::PgPool {
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(database_url)
            .await
            .unwrap();
        pool.execute("DROP SCHEMA IF EXISTS attacker_schema CASCADE")
            .await
            .unwrap();
        pool.execute("DROP SCHEMA public CASCADE").await.unwrap();
        pool.execute("CREATE SCHEMA public").await.unwrap();
        pool
    }

    async fn postgres_catalog_snapshot(pool: &sqlx::PgPool) -> Vec<(String, String)> {
        sqlx::query_as(
            r#"
            SELECT relation.relkind::text, relation.relname
            FROM pg_catalog.pg_class relation
            JOIN pg_catalog.pg_namespace namespace
              ON namespace.oid = relation.relnamespace
            WHERE namespace.nspname = 'public'
            ORDER BY relation.relkind, relation.relname
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn postgres_receipt_snapshot(
        pool: &sqlx::PgPool,
    ) -> Vec<(String, Option<i64>, Option<String>, Option<String>)> {
        sqlx::query_as(
            r#"
            SELECT migration_name, contract_version::bigint,
                   contract_fingerprint, installed_at
            FROM ONLY public.schema_migration_contracts
            ORDER BY migration_name
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn postgres_ledger_behavior_snapshot(pool: &sqlx::PgPool) -> (i64, i64, bool, bool, i64) {
        sqlx::query_as(
            r#"
            SELECT
              (
                SELECT count(*) FROM pg_catalog.pg_trigger attached_trigger
                WHERE attached_trigger.tgrelid = relation.oid
                  AND NOT attached_trigger.tgisinternal
              ),
              (
                SELECT count(*) FROM pg_catalog.pg_rewrite attached_rule
                WHERE attached_rule.ev_class = relation.oid
              ),
              relation.relrowsecurity,
              relation.relforcerowsecurity,
              (
                SELECT count(*) FROM pg_catalog.pg_policy attached_policy
                WHERE attached_policy.polrelid = relation.oid
              )
            FROM pg_catalog.pg_class relation
            WHERE relation.oid = pg_catalog.to_regclass(
              'public.schema_migration_contracts'
            )
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn postgres_attacker_schema_snapshot(pool: &sqlx::PgPool) -> (i64, i64, Option<String>) {
        sqlx::query_as(
            r#"
            SELECT
              (
                SELECT count(*)
                FROM pg_catalog.pg_class relation
                JOIN pg_catalog.pg_namespace namespace
                  ON namespace.oid = relation.relnamespace
                WHERE namespace.nspname = 'attacker_schema'
              ),
              (
                SELECT count(*)
                FROM attacker_schema.schema_migration_contracts
              ),
              (
                SELECT max(installed_at)
                FROM attacker_schema.schema_migration_contracts
              )
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_startup_rejects_anchor_receipt_xor_and_hostile_markers_without_mutation() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let expected_fingerprint = AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_FINGERPRINT;
        let cases = [
            (
                "exact",
                Some(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION),
                Some(expected_fingerprint),
            ),
            ("wrong-version", Some(99), Some(expected_fingerprint)),
            (
                "wrong-fingerprint",
                Some(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION),
                Some("0000000000000000000000000000000000000000000000000000000000000000"),
            ),
            ("null-version", None, Some(expected_fingerprint)),
            (
                "null-fingerprint",
                Some(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION),
                None,
            ),
            ("both-null", None, None),
        ];

        for (label, version, fingerprint) in cases {
            let pool = reset_isolated_postgres(&database_url).await;
            pool.execute(
                r#"
                CREATE TABLE public.schema_migration_contracts (
                  migration_name TEXT PRIMARY KEY,
                  contract_version INTEGER,
                  contract_fingerprint TEXT,
                  installed_at TEXT
                )
                "#,
            )
            .await
            .unwrap();
            sqlx::query(
                r#"
                INSERT INTO public.schema_migration_contracts (
                  migration_name, contract_version,
                  contract_fingerprint, installed_at
                ) VALUES ($1, $2::integer, $3, $4)
                "#,
            )
            .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION)
            .bind(version)
            .bind(fingerprint)
            .bind("1999-12-31T23:59:59Z")
            .execute(&pool)
            .await
            .unwrap();
            let catalog_before = postgres_catalog_snapshot(&pool).await;
            let receipts_before = postgres_receipt_snapshot(&pool).await;
            pool.close().await;

            let error = match AppDb::connect(&database_url).await {
                Ok(_) => panic!("{label}: anchorless receipt must reject PostgreSQL startup"),
                Err(error) => format!("{error:#}"),
            };
            assert!(
                error.contains("database migration required before startup"),
                "{label}: {error}"
            );

            let inspection = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            assert_eq!(
                postgres_catalog_snapshot(&inspection).await,
                catalog_before,
                "{label}: rejected startup must not create canonical objects"
            );
            assert_eq!(
                postgres_receipt_snapshot(&inspection).await,
                receipts_before,
                "{label}: rejected startup must not heal the receipt"
            );
            inspection.close().await;
        }

        let pool = reset_isolated_postgres(&database_url).await;
        pool.execute("CREATE TABLE public.aircraft_makes (id BIGINT PRIMARY KEY)")
            .await
            .unwrap();
        let catalog_before = postgres_catalog_snapshot(&pool).await;
        pool.close().await;
        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("an anchor without its receipt must reject PostgreSQL startup"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("deterministic retrieval-key data repair"),
            "{error}"
        );
        let inspection = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(postgres_catalog_snapshot(&inspection).await, catalog_before);
        inspection.close().await;

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let initialized = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = initialized.backend() else {
            unreachable!()
        };
        let receipts = postgres_receipt_snapshot(pool).await;
        assert!(!receipts.is_empty());
        pool.execute("DROP TABLE public.schema_migration_contracts")
            .await
            .unwrap();
        pool.execute(
            r#"
            CREATE TABLE public.schema_migration_contracts (
              migration_name TEXT,
              contract_version INTEGER,
              contract_fingerprint TEXT,
              installed_at TEXT
            )
            "#,
        )
        .await
        .unwrap();
        for (migration_name, contract_version, contract_fingerprint, installed_at) in receipts {
            sqlx::query(
                r#"
                INSERT INTO public.schema_migration_contracts (
                  migration_name, contract_version,
                  contract_fingerprint, installed_at
                ) VALUES ($1, $2::integer, $3, $4)
                "#,
            )
            .bind(migration_name)
            .bind(contract_version)
            .bind(contract_fingerprint)
            .bind(installed_at)
            .execute(pool)
            .await
            .unwrap();
        }
        let catalog_before = postgres_catalog_snapshot(pool).await;
        let receipts_before = postgres_receipt_snapshot(pool).await;
        initialized.close().await;

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("exact receipts in an impostor ledger must reject PostgreSQL startup"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("database migration required before startup"),
            "{error}"
        );
        let inspection = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(postgres_catalog_snapshot(&inspection).await, catalog_before);
        assert_eq!(
            postgres_receipt_snapshot(&inspection).await,
            receipts_before
        );
        inspection.close().await;

        for label in ["statement-trigger", "rewrite-rule", "row-level-security"] {
            let reset = reset_isolated_postgres(&database_url).await;
            reset.close().await;
            let initialized = AppDb::connect(&database_url).await.unwrap();
            let DatabaseBackend::Postgres(pool) = initialized.backend() else {
                unreachable!()
            };
            sqlx::query(
                "UPDATE public.schema_migration_contracts \
                 SET installed_at = 'sentinel:' || migration_name",
            )
            .execute(pool)
            .await
            .unwrap();
            match label {
                "statement-trigger" => {
                    pool.execute(
                        r#"
                        CREATE FUNCTION public.mutate_migration_receipts()
                        RETURNS trigger
                        LANGUAGE plpgsql
                        AS $function$
                        BEGIN
                          UPDATE public.schema_migration_contracts
                          SET installed_at = 'trigger-mutated';
                          RETURN NULL;
                        END
                        $function$
                        "#,
                    )
                    .await
                    .unwrap();
                    pool.execute(
                        r#"
                        CREATE TRIGGER mutate_migration_receipts_before_insert
                        BEFORE INSERT ON public.schema_migration_contracts
                        FOR EACH STATEMENT
                        EXECUTE FUNCTION public.mutate_migration_receipts()
                        "#,
                    )
                    .await
                    .unwrap();
                }
                "rewrite-rule" => {
                    pool.execute(
                        r#"
                        CREATE RULE mutate_migration_receipts_on_insert AS
                        ON INSERT TO public.schema_migration_contracts
                        DO ALSO
                          UPDATE public.schema_migration_contracts
                          SET installed_at = 'rule-mutated'
                        "#,
                    )
                    .await
                    .unwrap();
                }
                "row-level-security" => {
                    pool.execute(
                        "ALTER TABLE public.schema_migration_contracts \
                         ENABLE ROW LEVEL SECURITY",
                    )
                    .await
                    .unwrap();
                    pool.execute(
                        "CREATE POLICY migration_receipt_policy \
                         ON public.schema_migration_contracts \
                         USING (true) WITH CHECK (true)",
                    )
                    .await
                    .unwrap();
                }
                _ => unreachable!(),
            }
            let catalog_before = postgres_catalog_snapshot(pool).await;
            let behavior_before = postgres_ledger_behavior_snapshot(pool).await;
            let receipts_before = postgres_receipt_snapshot(pool).await;
            assert!(receipts_before.iter().all(|receipt| receipt
                .3
                .as_deref()
                .is_some_and(|installed_at| installed_at.starts_with("sentinel:"))));
            initialized.close().await;

            let error = match AppDb::connect(&database_url).await {
                Ok(_) => panic!("{label}: attached ledger behavior must reject startup"),
                Err(error) => format!("{error:#}"),
            };
            assert!(
                error.contains("database migration required before startup"),
                "{label}: {error}"
            );
            let inspection = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            assert_eq!(postgres_catalog_snapshot(&inspection).await, catalog_before);
            assert_eq!(
                postgres_ledger_behavior_snapshot(&inspection).await,
                behavior_before,
                "{label}: rejected startup must not remove attached behavior"
            );
            assert_eq!(
                postgres_receipt_snapshot(&inspection).await,
                receipts_before,
                "{label}: rejected startup must not run attached behavior"
            );
            inspection.close().await;
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_all_receipt_timestamps_survive_two_normal_startups() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;

        let initialized = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = initialized.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            INSERT INTO public.schema_migration_contracts (
              migration_name, contract_version, contract_fingerprint, installed_at
            ) VALUES (
              '20260809_listing_verification_runs', 1,
              'a8beda24d71517ba07e4a81b2802b2fef97296ae6b2256a7ff493d6af5235232',
              'historical-sentinel'
            )
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE public.schema_migration_contracts \
             SET installed_at = 'sentinel:' || migration_name",
        )
        .execute(pool)
        .await
        .unwrap();
        let expected = postgres_receipt_snapshot(pool).await;
        assert_eq!(expected.len(), 24);
        assert!(expected
            .iter()
            .any(|receipt| receipt.0 == "20260809_listing_verification_runs"));
        assert!(expected.iter().all(|receipt| receipt
            .3
            .as_deref()
            .is_some_and(|installed_at| installed_at.starts_with("sentinel:"))));
        initialized.close().await;

        for startup in 1..=2 {
            let reopened = AppDb::connect(&database_url).await.unwrap();
            let DatabaseBackend::Postgres(pool) = reopened.backend() else {
                unreachable!()
            };
            assert_eq!(
                postgres_receipt_snapshot(pool).await,
                expected,
                "startup {startup} must preserve every original PostgreSQL install receipt"
            );
            reopened.close().await;
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_startup_rejects_noncanonical_ledger_storage_without_mutation() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        for label in ["unlogged", "collation", "extra-index"] {
            let reset = reset_isolated_postgres(&database_url).await;
            reset.close().await;
            let initialized = AppDb::connect(&database_url).await.unwrap();
            let DatabaseBackend::Postgres(pool) = initialized.backend() else {
                unreachable!()
            };
            sqlx::query(
                "UPDATE public.schema_migration_contracts \
                 SET installed_at = 'sentinel:' || migration_name",
            )
            .execute(pool)
            .await
            .unwrap();
            match label {
                "unlogged" => pool
                    .execute("ALTER TABLE public.schema_migration_contracts SET UNLOGGED")
                    .await
                    .unwrap(),
                "collation" => pool
                    .execute(
                        "ALTER TABLE public.schema_migration_contracts \
                         ALTER COLUMN migration_name TYPE text COLLATE \"C\"",
                    )
                    .await
                    .unwrap(),
                "extra-index" => pool
                    .execute(
                        "CREATE INDEX hostile_migration_receipt_installed_at \
                         ON public.schema_migration_contracts (installed_at)",
                    )
                    .await
                    .unwrap(),
                _ => unreachable!(),
            };
            let catalog_before = postgres_catalog_snapshot(pool).await;
            let receipts_before = postgres_receipt_snapshot(pool).await;
            initialized.close().await;

            let error = connect_error(AppDb::connect(&database_url).await);
            assert!(
                error.contains("database migration required before startup"),
                "{label}: {error}"
            );
            let inspection = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            assert_eq!(
                postgres_catalog_snapshot(&inspection).await,
                catalog_before,
                "{label}: rejected startup must preserve catalog shape"
            );
            assert_eq!(
                postgres_receipt_snapshot(&inspection).await,
                receipts_before,
                "{label}: rejected startup must preserve receipts"
            );
            match label {
                "unlogged" => {
                    let persistence: String = sqlx::query_scalar(
                        "SELECT relpersistence::text FROM pg_catalog.pg_class \
                         WHERE oid = 'public.schema_migration_contracts'::pg_catalog.regclass",
                    )
                    .fetch_one(&inspection)
                    .await
                    .unwrap();
                    assert_eq!(persistence, "u");
                }
                "collation" => {
                    let collation: String = sqlx::query_scalar(
                        "SELECT catalog_collation.collname \
                         FROM pg_catalog.pg_attribute attribute \
                         JOIN pg_catalog.pg_collation catalog_collation \
                           ON catalog_collation.oid = attribute.attcollation \
                         WHERE attribute.attrelid = \
                           'public.schema_migration_contracts'::pg_catalog.regclass \
                           AND attribute.attname = 'migration_name'",
                    )
                    .fetch_one(&inspection)
                    .await
                    .unwrap();
                    assert_eq!(collation, "C");
                }
                "extra-index" => {
                    let exists: bool = sqlx::query_scalar(
                        "SELECT pg_catalog.to_regclass( \
                           'public.hostile_migration_receipt_installed_at' \
                         ) IS NOT NULL",
                    )
                    .fetch_one(&inspection)
                    .await
                    .unwrap();
                    assert!(exists);
                }
                _ => unreachable!(),
            }
            inspection.close().await;
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_inherited_child_receipt_cannot_satisfy_parent_ledger() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let initialized = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = initialized.backend() else {
            unreachable!()
        };
        sqlx::query(
            "DELETE FROM ONLY public.schema_migration_contracts \
             WHERE migration_name = $1",
        )
        .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION)
        .execute(pool)
        .await
        .unwrap();
        pool.execute(
            "CREATE TABLE public.hostile_migration_receipt_child () \
             INHERITS (public.schema_migration_contracts)",
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO public.hostile_migration_receipt_child ( \
               migration_name, contract_version, contract_fingerprint, installed_at \
             ) VALUES ($1, $2, $3, 'child-only-sentinel')",
        )
        .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION)
        .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION)
        .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_FINGERPRINT)
        .execute(pool)
        .await
        .unwrap();
        let catalog_before = postgres_catalog_snapshot(pool).await;
        let parent_receipts_before = postgres_receipt_snapshot(pool).await;
        initialized.close().await;

        let error = connect_error(AppDb::connect(&database_url).await);
        assert!(
            error.contains("database migration required before startup"),
            "{error}"
        );
        let inspection = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(postgres_catalog_snapshot(&inspection).await, catalog_before);
        assert_eq!(
            postgres_receipt_snapshot(&inspection).await,
            parent_receipts_before
        );
        let (parent_count, child_count): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM ONLY public.schema_migration_contracts \
                     WHERE migration_name = $1), \
                    (SELECT count(*) FROM public.hostile_migration_receipt_child \
                     WHERE migration_name = $1)",
        )
        .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION)
        .fetch_one(&inspection)
        .await
        .unwrap();
        assert_eq!((parent_count, child_count), (0, 1));
        inspection.close().await;
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_startup_waits_for_writer_and_fresh_startups_serialize() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        for commit_writer in [true, false] {
            let reset = reset_isolated_postgres(&database_url).await;
            reset.close().await;
            AppDb::connect(&database_url).await.unwrap().close().await;
            let writer_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            let mut writer = writer_pool.acquire().await.unwrap();
            writer.execute("BEGIN").await.unwrap();
            sqlx::query(
                "DELETE FROM ONLY public.schema_migration_contracts \
                 WHERE migration_name = $1",
            )
            .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION)
            .execute(&mut *writer)
            .await
            .unwrap();

            let startup_url = database_url.clone();
            let mut startup = tokio::spawn(async move { AppDb::connect(&startup_url).await });
            assert!(
                tokio::time::timeout(Duration::from_millis(150), &mut startup)
                    .await
                    .is_err(),
                "startup must wait for the receipt writer before taking its snapshot"
            );
            writer
                .execute(if commit_writer { "COMMIT" } else { "ROLLBACK" })
                .await
                .unwrap();
            drop(writer);
            writer_pool.close().await;

            let startup_result = tokio::time::timeout(Duration::from_secs(30), startup)
                .await
                .expect("serialized PostgreSQL startup timed out")
                .unwrap();
            if commit_writer {
                let error = connect_error(startup_result);
                assert!(
                    error.contains("deterministic retrieval-key data repair"),
                    "{error}"
                );
                let inspection = PgPoolOptions::new()
                    .max_connections(1)
                    .connect(&database_url)
                    .await
                    .unwrap();
                let receipt_exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM ONLY public.schema_migration_contracts \
                     WHERE migration_name = $1)",
                )
                .bind(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION)
                .fetch_one(&inspection)
                .await
                .unwrap();
                assert!(
                    !receipt_exists,
                    "rejected startup must not heal the receipt"
                );
                inspection.close().await;
            } else {
                startup_result.unwrap().close().await;
            }
        }

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let first_url = database_url.clone();
        let second_url = database_url.clone();
        let (first, second) = tokio::join!(AppDb::connect(&first_url), AppDb::connect(&second_url));
        first.unwrap().close().await;
        second.unwrap().close().await;
        let inspection = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(postgres_receipt_snapshot(&inspection).await.len(), 23);
        inspection.close().await;
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_late_initialization_failure_rolls_back_all_ddl() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let setup = reset_isolated_postgres(&database_url).await;
        setup
            .execute("CREATE TABLE public.users (sentinel TEXT NOT NULL)")
            .await
            .unwrap();
        let before = postgres_catalog_snapshot(&setup).await;
        setup.close().await;

        let error = connect_error(AppDb::connect(&database_url).await);
        assert!(
            error.contains("email") || error.contains("users") || error.contains("column \"id\""),
            "{error}"
        );
        let inspection = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(
            postgres_catalog_snapshot(&inspection).await,
            before,
            "late seed failure must roll back every canonical PostgreSQL DDL statement"
        );
        inspection.close().await;
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_startup_pins_search_path_and_ignores_attacker_shadows() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let separator = if database_url.contains('?') { '&' } else { '?' };
        let hostile_url =
            format!("{database_url}{separator}options%5Bsearch_path%5D=attacker_schema%2Cpublic");
        let setup = reset_isolated_postgres(&database_url).await;
        setup
            .execute("CREATE SCHEMA attacker_schema")
            .await
            .unwrap();
        setup
            .execute(
                r#"
                CREATE TABLE attacker_schema.schema_migration_contracts (
                  migration_name TEXT PRIMARY KEY,
                  contract_version INTEGER NOT NULL CHECK (contract_version > 0),
                  contract_fingerprint TEXT NOT NULL
                    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
                  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  CHECK (length(trim(migration_name)) > 0)
                )
                "#,
            )
            .await
            .unwrap();
        setup
            .execute(
                r#"
                INSERT INTO attacker_schema.schema_migration_contracts (
                  migration_name, contract_version,
                  contract_fingerprint, installed_at
                ) VALUES (
                  '20991231_attacker_sentinel', 1,
                  'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                  'attacker-sentinel'
                )
                "#,
            )
            .await
            .unwrap();
        let attacker_before = postgres_attacker_schema_snapshot(&setup).await;
        setup.close().await;

        let initialized = AppDb::connect(&hostile_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = initialized.backend() else {
            unreachable!()
        };
        let search_path =
            sqlx::query_scalar::<_, String>("SELECT pg_catalog.current_setting('search_path')")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(search_path.replace(' ', ""), POSTGRES_SEARCH_PATH);
        assert!(sqlx::query_scalar::<_, bool>(
            "SELECT pg_catalog.to_regclass( \
               'public.schema_migration_contracts' \
             ) IS NOT NULL",
        )
        .fetch_one(pool)
        .await
        .unwrap());
        assert_eq!(
            postgres_attacker_schema_snapshot(pool).await,
            attacker_before,
            "hostile URL search_path must not redirect canonical startup writes"
        );
        initialized.close().await;

        let diagnostic = AppDb::connect_diagnostic(&hostile_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = diagnostic.backend() else {
            unreachable!()
        };
        let diagnostic_search_path =
            sqlx::query_scalar::<_, String>("SELECT pg_catalog.current_setting('search_path')")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(
            diagnostic_search_path.replace(' ', ""),
            POSTGRES_SEARCH_PATH
        );
        assert_eq!(
            postgres_attacker_schema_snapshot(pool).await,
            attacker_before
        );
        diagnostic.close().await;

        let normal = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = normal.backend() else {
            unreachable!()
        };
        assert_eq!(
            postgres_attacker_schema_snapshot(pool).await,
            attacker_before
        );
        normal.close().await;
    }

    async fn apply_postgres_listing_replay_migration(pool: &sqlx::PgPool, hostile: bool) {
        let mut connection = pool.acquire().await.unwrap();
        if hostile {
            connection
                .execute("CREATE SCHEMA IF NOT EXISTS attacker_schema")
                .await
                .unwrap();
            connection
                .execute("SET search_path = attacker_schema, pg_catalog")
                .await
                .unwrap();
        }
        for statement in split_sql_statements(LISTING_REPLAY_RUNS_POSTGRES_MIGRATION_SQL) {
            connection.execute(statement).await.unwrap();
        }
        if hostile {
            connection.execute("RESET search_path").await.unwrap();
        }
    }

    async fn postgres_replay_contract_snapshot(pool: &sqlx::PgPool) -> String {
        sqlx::query_scalar(
            r#"
            SELECT pg_catalog.md5(
              COALESCE((
                SELECT pg_catalog.string_agg(
                  relation.oid::text || '|' || namespace.nspname || '|' ||
                  relation.relname || '|' || attribute.attnum::text || '|' ||
                  attribute.attname || '|' || pg_catalog.format_type(
                    attribute.atttypid, attribute.atttypmod
                  ) || '|' || relation.relkind::text || '|' ||
                  relation.relpersistence::text || '|' ||
                  relation.relrowsecurity::text || '|' ||
                  relation.relforcerowsecurity::text || '|' ||
                  relation.relispartition::text || '|' ||
                  relation.relhasrules::text || '|' ||
                  (relation.relpartbound IS NOT NULL)::text || '|' ||
                  attribute.attnotnull::text || '|' ||
                  attribute.attidentity::text || '|' || COALESCE(
                    pg_catalog.pg_get_expr(default_value.adbin, default_value.adrelid), ''
                  ), E'\n' ORDER BY relation.relname, attribute.attnum
                )
                FROM pg_catalog.pg_attribute attribute
                JOIN pg_catalog.pg_class relation ON relation.oid = attribute.attrelid
                JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace
                LEFT JOIN pg_catalog.pg_attrdef default_value
                  ON default_value.adrelid = attribute.attrelid
                 AND default_value.adnum = attribute.attnum
                WHERE namespace.nspname = 'public'
                  AND relation.relname IN (
                    'listing_replay_runs', 'listing_replay_run_items',
                    'plugin_submission_materialization_receipts'
                  )
                  AND attribute.attnum > 0 AND NOT attribute.attisdropped
              ), '') || E'\n--constraints--\n' || COALESCE((
                SELECT pg_catalog.string_agg(
                  child.oid::text || '|' || constraint_definition.conname || '|' ||
                  constraint_definition.contype::text || '|' ||
                  constraint_definition.conkey::text || '|' ||
                  constraint_definition.confrelid::text || '|' ||
                  constraint_definition.confkey::text || '|' ||
                  constraint_definition.convalidated::text || '|' ||
                  constraint_definition.condeferrable::text || '|' ||
                  constraint_definition.condeferred::text || '|' ||
                  constraint_definition.confmatchtype::text || '|' ||
                  constraint_definition.confupdtype::text || '|' ||
                  constraint_definition.confdeltype::text || '|' ||
                  pg_catalog.pg_get_constraintdef(constraint_definition.oid),
                  E'\n' ORDER BY child.relname, constraint_definition.conname
                )
                FROM pg_catalog.pg_constraint constraint_definition
                JOIN pg_catalog.pg_class child
                  ON child.oid = constraint_definition.conrelid
                JOIN pg_catalog.pg_namespace namespace
                  ON namespace.oid = child.relnamespace
                WHERE namespace.nspname = 'public'
                  AND child.relname IN (
                    'listing_replay_runs', 'listing_replay_run_items',
                    'plugin_submission_materialization_receipts'
                  )
              ), '') || E'\n--indexes--\n' || COALESCE((
                SELECT pg_catalog.string_agg(
                  index_definition.indexrelid::text || '|' ||
                  index_definition.indrelid::text || '|' ||
                  index_relation.relname || '|' ||
                  index_definition.indisunique::text || '|' ||
                  index_definition.indisprimary::text || '|' ||
                  index_definition.indisexclusion::text || '|' ||
                  index_definition.indimmediate::text || '|' ||
                  index_definition.indisclustered::text || '|' ||
                  index_definition.indisvalid::text || '|' ||
                  index_definition.indisready::text || '|' ||
                  index_definition.indislive::text || '|' ||
                  index_definition.indisreplident::text || '|' ||
                  index_definition.indnullsnotdistinct::text || '|' ||
                  index_definition.indnkeyatts::text || '|' ||
                  index_definition.indnatts::text || '|' ||
                  index_definition.indkey::text || '|' ||
                  index_definition.indcollation::text || '|' ||
                  index_definition.indclass::text || '|' ||
                  index_definition.indoption::text || '|' ||
                  access_method.amname::text || '|' || COALESCE(
                    pg_catalog.pg_get_expr(
                      index_definition.indpred, index_definition.indrelid
                    ), ''
                  ) || '|' || COALESCE(
                    pg_catalog.pg_get_expr(
                      index_definition.indexprs, index_definition.indrelid
                    ), ''
                  ), E'\n' ORDER BY index_relation.relname
                )
                FROM pg_catalog.pg_index index_definition
                JOIN pg_catalog.pg_class index_relation
                  ON index_relation.oid = index_definition.indexrelid
                JOIN pg_catalog.pg_am access_method
                  ON access_method.oid = index_relation.relam
                WHERE index_definition.indrelid IN (
                  pg_catalog.to_regclass('public.listing_replay_runs'),
                  pg_catalog.to_regclass('public.listing_replay_run_items'),
                  pg_catalog.to_regclass(
                    'public.plugin_submission_materialization_receipts'
                  ),
                  pg_catalog.to_regclass('public.aircraft_sale_listings')
                )
              ), '') || E'\n--triggers--\n' || COALESCE((
                SELECT pg_catalog.string_agg(
                  trigger_definition.tgrelid::text || '|' ||
                  trigger_definition.tgname || '|' ||
                  trigger_definition.tgtype::text || '|' ||
                  trigger_definition.tgenabled::text || '|' ||
                  trigger_definition.tgisinternal::text || '|' ||
                  pg_catalog.pg_get_triggerdef(trigger_definition.oid),
                  E'\n' ORDER BY trigger_definition.tgrelid, trigger_definition.tgname
                )
                FROM pg_catalog.pg_trigger trigger_definition
                WHERE trigger_definition.tgrelid IN (
                  pg_catalog.to_regclass('public.listing_replay_runs'),
                  pg_catalog.to_regclass('public.listing_replay_run_items'),
                  pg_catalog.to_regclass(
                    'public.plugin_submission_materialization_receipts'
                  ),
                  pg_catalog.to_regclass('public.plugin_submissions'),
                  pg_catalog.to_regclass('public.plugin_installs')
                )
              ), '') || E'\n--functions--\n' || COALESCE((
                SELECT pg_catalog.string_agg(
                  routine.oid::text || '|' || routine.proname || '|' ||
                  routine.prosrc || '|' || COALESCE(
                    pg_catalog.array_to_string(routine.proconfig, E'\n'), ''
                  ), E'\n' ORDER BY routine.proname
                )
                FROM pg_catalog.pg_proc routine
                JOIN pg_catalog.pg_namespace namespace
                  ON namespace.oid = routine.pronamespace
                WHERE namespace.nspname = 'public'
                  AND routine.pronargs = 0
                  AND routine.proname IN (
                    'enforce_replay_extraction_checkpoint_exactness',
                    'preserve_completed_replay_item',
                    'preserve_replay_materialization_receipt',
                    'enforce_replay_checkpoint_capture_immutability',
                    'enforce_replay_plugin_identity_immutability'
                  )
              ), '') || E'\n--policies--\n' || COALESCE((
                SELECT pg_catalog.string_agg(
                  policy_definition.polrelid::text || '|' ||
                  policy_definition.polname || '|' ||
                  policy_definition.polcmd::text || '|' ||
                  policy_definition.polpermissive::text,
                  E'\n' ORDER BY policy_definition.polrelid, policy_definition.polname
                )
                FROM pg_catalog.pg_policy policy_definition
                WHERE policy_definition.polrelid IN (
                  pg_catalog.to_regclass('public.listing_replay_runs'),
                  pg_catalog.to_regclass('public.listing_replay_run_items'),
                  pg_catalog.to_regclass(
                    'public.plugin_submission_materialization_receipts'
                  )
                )
              ), '') || E'\n--inheritance--\n' || COALESCE((
                SELECT pg_catalog.string_agg(
                  inheritance.inhrelid::text || '|' ||
                  inheritance.inhparent::text || '|' ||
                  inheritance.inhseqno::text,
                  E'\n' ORDER BY inheritance.inhrelid, inheritance.inhseqno
                )
                FROM pg_catalog.pg_inherits inheritance
                WHERE inheritance.inhrelid IN (
                  pg_catalog.to_regclass('public.listing_replay_runs'),
                  pg_catalog.to_regclass('public.listing_replay_run_items'),
                  pg_catalog.to_regclass(
                    'public.plugin_submission_materialization_receipts'
                  )
                ) OR inheritance.inhparent IN (
                  pg_catalog.to_regclass('public.listing_replay_runs'),
                  pg_catalog.to_regclass('public.listing_replay_run_items'),
                  pg_catalog.to_regclass(
                    'public.plugin_submission_materialization_receipts'
                  )
                )
              ), '')
            )
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn assert_postgres_replay_migration_rerun_rejected_without_changes(
        pool: &sqlx::PgPool,
        label: &str,
    ) {
        sqlx::query(
            "UPDATE public.schema_migration_contracts SET installed_at = $1 WHERE migration_name = $2",
        )
        .bind("1999-12-31T23:59:59Z")
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .execute(pool)
        .await
        .unwrap();
        let before = postgres_replay_contract_snapshot(pool).await;
        let mut connection = pool.acquire().await.unwrap();
        connection
            .execute("SET search_path = pg_catalog")
            .await
            .unwrap();
        let mut rejected = false;
        for statement in split_sql_statements(LISTING_REPLAY_RUNS_POSTGRES_MIGRATION_SQL) {
            if connection.execute(statement).await.is_err() {
                rejected = true;
                break;
            }
        }
        assert!(rejected, "{label}: hostile PostgreSQL rerun must fail");
        let _ = connection.execute("ROLLBACK").await;
        connection.execute("RESET search_path").await.unwrap();
        drop(connection);
        assert_eq!(
            postgres_replay_contract_snapshot(pool).await,
            before,
            "{label}: rejected rerun must not mutate the replay catalog"
        );
        let installed_at: String = sqlx::query_scalar(
            "SELECT installed_at FROM public.schema_migration_contracts WHERE migration_name = $1",
        )
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(installed_at, "1999-12-31T23:59:59Z", "{label}");
    }

    async fn assert_postgres_replay_startup_rejected(database_url: &str) {
        let error = match AppDb::connect(database_url).await {
            Ok(_) => panic!("startup must reject a weakened PostgreSQL replay contract"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("20260819_listing_replay_runs.postgres.sql"),
            "{error}"
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_listing_replay_fresh_migrated_idempotent_and_hostile_search_path() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;

        let fresh = AppDb::connect(&database_url).await.unwrap();
        assert!(fresh.listing_replay_definitions_valid().await.unwrap());
        let DatabaseBackend::Postgres(fresh_pool) = fresh.backend() else {
            unreachable!()
        };
        apply_postgres_listing_replay_migration(fresh_pool, true).await;
        apply_postgres_listing_replay_migration(fresh_pool, true).await;
        fresh.ensure_required_migrations().await.unwrap();
        drop(fresh);

        let pool = reset_isolated_postgres(&database_url).await;
        pool.close().await;
        let initialized = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = initialized.backend() else {
            unreachable!()
        };
        pool.execute(
            "DROP TRIGGER plugin_submissions_replay_checkpoint_immutable \
             ON public.plugin_submissions",
        )
        .await
        .unwrap();
        pool.execute(
            "DROP TRIGGER plugin_installs_replay_identity_immutable \
             ON public.plugin_installs",
        )
        .await
        .unwrap();
        pool.execute("DROP INDEX public.uq_aircraft_sale_listings_owner_source")
            .await
            .unwrap();
        pool.execute("DROP TABLE public.plugin_submission_materialization_receipts")
            .await
            .unwrap();
        pool.execute("DROP TABLE public.listing_replay_run_items")
            .await
            .unwrap();
        pool.execute("DROP TABLE public.listing_replay_runs")
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            DROP FUNCTION public.enforce_replay_extraction_checkpoint_exactness();
            DROP FUNCTION public.preserve_completed_replay_item();
            DROP FUNCTION public.preserve_replay_materialization_receipt();
            DROP FUNCTION public.enforce_replay_checkpoint_capture_immutability();
            DROP FUNCTION public.enforce_replay_plugin_identity_immutability();
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM public.schema_migration_contracts WHERE migration_name = $1")
            .bind(LISTING_REPLAY_RUNS_MIGRATION)
            .execute(pool)
            .await
            .unwrap();
        apply_postgres_listing_replay_migration(pool, true).await;
        apply_postgres_listing_replay_migration(pool, true).await;
        assert!(initialized
            .listing_replay_definitions_valid()
            .await
            .unwrap());
        initialized.ensure_required_migrations().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_listing_replay_installed_at_survives_two_normal_startups() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;

        let initialized = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = initialized.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE public.schema_migration_contracts SET installed_at = $1 WHERE migration_name = $2",
        )
        .bind("1999-12-31T23:59:59Z")
        .bind(LISTING_REPLAY_RUNS_MIGRATION)
        .execute(pool)
        .await
        .unwrap();
        pool.close().await;
        drop(initialized);

        for startup in 1..=2 {
            let reopened = AppDb::connect(&database_url).await.unwrap();
            let DatabaseBackend::Postgres(pool) = reopened.backend() else {
                unreachable!()
            };
            let installed_at: String = sqlx::query_scalar(
                "SELECT installed_at FROM public.schema_migration_contracts WHERE migration_name = $1",
            )
            .bind(LISTING_REPLAY_RUNS_MIGRATION)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(
                installed_at, "1999-12-31T23:59:59Z",
                "startup {startup} must preserve the original install receipt"
            );
            pool.close().await;
            drop(reopened);
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_listing_replay_startup_rejects_weakened_column_constraint_and_index() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute(
            "ALTER TABLE public.listing_replay_run_items ALTER COLUMN plugin_submission_id DROP NOT NULL",
        )
        .await
        .unwrap();
        assert_postgres_replay_migration_rerun_rejected_without_changes(
            pool,
            "nullable-plugin-submission",
        )
        .await;
        drop(db);
        assert_postgres_replay_startup_rejected(&database_url).await;

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        let constraint_name: String = sqlx::query_scalar(
            r#"SELECT conname FROM pg_catalog.pg_constraint
               WHERE conrelid = pg_catalog.to_regclass('public.listing_replay_runs')
                 AND contype = 'u'
                 AND conkey = ARRAY[
                   (SELECT attnum FROM pg_catalog.pg_attribute
                    WHERE attrelid = pg_catalog.to_regclass('public.listing_replay_runs')
                      AND attname = 'manifest_sha256')
                 ]::smallint[]"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let replace_with_included_column = format!(
            "ALTER TABLE public.listing_replay_runs DROP CONSTRAINT \"{}\"; \
             ALTER TABLE public.listing_replay_runs ADD CONSTRAINT \"{}\" \
             UNIQUE (manifest_sha256) INCLUDE (status)",
            constraint_name.replace('"', "\"\""),
            constraint_name.replace('"', "\"\"")
        );
        sqlx::raw_sql(&replace_with_included_column)
            .execute(pool)
            .await
            .unwrap();
        assert_postgres_replay_migration_rerun_rejected_without_changes(
            pool,
            "unique-backing-index-with-included-column",
        )
        .await;
        drop(db);
        assert_postgres_replay_startup_rejected(&database_url).await;

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        let constraint_name: String = sqlx::query_scalar(
            r#"SELECT constraint_definition.conname
               FROM pg_catalog.pg_constraint constraint_definition
               WHERE constraint_definition.conrelid =
                 pg_catalog.to_regclass('public.listing_replay_run_items')
                 AND constraint_definition.contype = 'c'
                 AND position(
                   'extracted_listing_sha256 IS NOT NULL'
                   IN pg_catalog.pg_get_constraintdef(constraint_definition.oid)
                 ) > 0"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let drop_constraint = format!(
            "ALTER TABLE public.listing_replay_run_items DROP CONSTRAINT \"{}\"",
            constraint_name.replace('"', "\"\"")
        );
        pool.execute(drop_constraint.as_str()).await.unwrap();
        assert_postgres_replay_migration_rerun_rejected_without_changes(
            pool,
            "missing-check-constraint",
        )
        .await;
        drop(db);
        assert_postgres_replay_startup_rejected(&database_url).await;

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP INDEX public.idx_listing_replay_runs_one_running")
            .await
            .unwrap();
        pool.execute(
            "CREATE INDEX idx_listing_replay_runs_one_running ON public.listing_replay_runs (status)",
        )
        .await
        .unwrap();
        assert_postgres_replay_migration_rerun_rejected_without_changes(
            pool,
            "weakened-running-index",
        )
        .await;
        drop(db);
        assert_postgres_replay_startup_rejected(&database_url).await;

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("CREATE SCHEMA attacker_schema").await.unwrap();
        pool.execute("CREATE TABLE attacker_schema.plugin_submissions (id BIGINT PRIMARY KEY)")
            .await
            .unwrap();
        let constraint_name: String = sqlx::query_scalar(
            r#"SELECT conname FROM pg_catalog.pg_constraint
               WHERE conrelid = pg_catalog.to_regclass('public.listing_replay_run_items')
                 AND confrelid = pg_catalog.to_regclass('public.plugin_submissions')
                 AND contype = 'f'"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let replace_foreign_key = format!(
            "ALTER TABLE public.listing_replay_run_items DROP CONSTRAINT \"{}\"; \
             ALTER TABLE public.listing_replay_run_items ADD CONSTRAINT \"{}\" \
             FOREIGN KEY (plugin_submission_id) REFERENCES attacker_schema.plugin_submissions(id) \
             MATCH FULL ON UPDATE CASCADE ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED NOT VALID",
            constraint_name.replace('"', "\"\""),
            constraint_name.replace('"', "\"\"")
        );
        sqlx::raw_sql(&replace_foreign_key)
            .execute(pool)
            .await
            .unwrap();
        assert_postgres_replay_migration_rerun_rejected_without_changes(
            pool,
            "hostile-same-name-parent-foreign-key",
        )
        .await;
        drop(db);
        assert_postgres_replay_startup_rejected(&database_url).await;
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_listing_replay_rerun_rejects_unexpected_indexes_and_triggers() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute(
            "CREATE INDEX unexpected_replay_owner_index ON public.listing_replay_runs(owner_token)",
        )
        .await
        .unwrap();
        assert_postgres_replay_migration_rerun_rejected_without_changes(
            pool,
            "unexpected-attached-index",
        )
        .await;
        drop(db);
        assert_postgres_replay_startup_rejected(&database_url).await;

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::raw_sql(
            r#"
            CREATE FUNCTION public.unexpected_replay_trigger()
            RETURNS TRIGGER LANGUAGE plpgsql AS $function$
            BEGIN
              RETURN NEW;
            END
            $function$;
            CREATE TRIGGER unexpected_replay_update
            BEFORE UPDATE ON public.listing_replay_runs
            FOR EACH ROW EXECUTE FUNCTION public.unexpected_replay_trigger();
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        assert_postgres_replay_migration_rerun_rejected_without_changes(
            pool,
            "unexpected-attached-trigger",
        )
        .await;
        drop(db);
        assert_postgres_replay_startup_rejected(&database_url).await;

        for (label, relation) in [
            ("unexpected-submission-trigger", "plugin_submissions"),
            ("unexpected-install-trigger", "plugin_installs"),
        ] {
            let reset = reset_isolated_postgres(&database_url).await;
            reset.close().await;
            let db = AppDb::connect(&database_url).await.unwrap();
            let DatabaseBackend::Postgres(pool) = db.backend() else {
                unreachable!()
            };
            let trigger_sql = format!(
                "CREATE FUNCTION public.unexpected_plugin_trigger() RETURNS TRIGGER \
                 LANGUAGE plpgsql AS 'BEGIN RETURN OLD; END'; \
                 CREATE TRIGGER unexpected_plugin_delete BEFORE DELETE ON public.{relation} \
                 FOR EACH ROW EXECUTE FUNCTION public.unexpected_plugin_trigger();"
            );
            sqlx::raw_sql(&trigger_sql).execute(pool).await.unwrap();
            assert_postgres_replay_migration_rerun_rejected_without_changes(pool, label).await;
            drop(db);
            assert_postgres_replay_startup_rejected(&database_url).await;
        }

        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::raw_sql(
            r#"
            CREATE OR REPLACE FUNCTION public.enforce_replay_checkpoint_capture_immutability()
            RETURNS TRIGGER
            LANGUAGE plpgsql
            SET search_path = pg_catalog
            AS $function$
            BEGIN
              RETURN NEW;
            END
            $function$;
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        assert_postgres_replay_migration_rerun_rejected_without_changes(
            pool,
            "weakened-replay-trigger-function",
        )
        .await;
        drop(db);
        assert_postgres_replay_startup_rejected(&database_url).await;
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_replay_ledger_rejects_invalid_state_outcome_pairings() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        let user_id: i64 = sqlx::query_scalar("SELECT id FROM public.users ORDER BY id LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap();
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO public.plugin_installs (user_id, public_key_base64) VALUES ($1, 'key') RETURNING id",
        )
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO public.plugin_submissions (
                 user_id, plugin_install_id, source_url, rendered_html,
                 rendered_html_sha256, signature_base64
               ) VALUES ($1, $2, 'https://example.test/pg-ledger', '<html/>', $3, 'sig')
               RETURNING id"#,
        )
        .bind(user_id)
        .bind(install_id)
        .bind("a".repeat(64))
        .fetch_one(pool)
        .await
        .unwrap();
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO public.listing_replay_runs (manifest_version, manifest_sha256, manifest_capture_count) VALUES (1, $1, 2) RETURNING id",
        )
        .bind("b".repeat(64))
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(sqlx::query(
            r#"INSERT INTO public.listing_replay_run_items (
                 run_id, plugin_submission_id, position, expected_rendered_html_sha256,
                 extraction_state, materialization_state, terminal_rejection_phase,
                 terminal_rejection_stage, terminal_rejection_reason_code
               ) VALUES ($1, $2, 0, $3, 'rejected', 'blocked', 'extraction',
                         'faa_aircraft_admission', 'missing_registration')"#,
        )
        .bind(run_id)
        .bind(submission_id)
        .bind("c".repeat(64))
        .execute(pool)
        .await
        .is_err());
        assert!(sqlx::query(
            r#"INSERT INTO public.listing_replay_run_items (
                 run_id, plugin_submission_id, position, expected_rendered_html_sha256,
                 extracted_listing_sha256, extraction_state, materialization_state,
                 last_failure_phase, last_failure_reason_code
               ) VALUES ($1, $2, 1, $3, $4, 'succeeded', 'failed',
                         'extraction', 'operation_failed')"#,
        )
        .bind(run_id)
        .bind(submission_id)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(pool)
        .await
        .is_err());
        assert!(sqlx::query(
            r#"INSERT INTO public.listing_replay_run_items (
                 run_id, plugin_submission_id, position, expected_rendered_html_sha256,
                 extracted_listing_sha256, extraction_state, materialization_state
               ) VALUES ($1, $2, 2, $3, $4, 'succeeded', 'succeeded')"#,
        )
        .bind(run_id)
        .bind(submission_id)
        .bind("f".repeat(64))
        .bind("0".repeat(64))
        .execute(pool)
        .await
        .is_err());
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_correction_validation_rejects_altered_search_path_and_namespace() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        for statement in split_sql_statements(POSTGRES_SCHEMA_SQL) {
            connection.execute(statement).await.unwrap();
        }
        drop(connection);
        let db = AppDb {
            backend: DatabaseBackend::Postgres(pool.clone()),
        };
        assert!(db
            .aircraft_listing_identity_correction_definitions_valid()
            .await
            .unwrap());
        pool.execute("DROP SCHEMA IF EXISTS attacker_schema CASCADE")
            .await
            .unwrap();
        pool.execute("CREATE SCHEMA attacker_schema").await.unwrap();
        pool.execute(
            "ALTER FUNCTION public.require_source_identity_correction_receipt() \
             SET search_path = attacker_schema, public",
        )
        .await
        .unwrap();
        assert!(!db
            .aircraft_listing_identity_correction_definitions_valid()
            .await
            .unwrap());
        pool.execute(
            "ALTER FUNCTION public.require_source_identity_correction_receipt() \
             SET search_path = pg_catalog",
        )
        .await
        .unwrap();
        assert!(db
            .aircraft_listing_identity_correction_definitions_valid()
            .await
            .unwrap());

        pool.execute(
            "ALTER FUNCTION public.require_source_identity_correction_receipt() \
             SET SCHEMA attacker_schema",
        )
        .await
        .unwrap();
        assert!(!db
            .aircraft_listing_identity_correction_definitions_valid()
            .await
            .unwrap());

        pool.execute(
            "ALTER FUNCTION attacker_schema.require_source_identity_correction_receipt() \
             SET SCHEMA public",
        )
        .await
        .unwrap();
        pool.execute("DROP SCHEMA attacker_schema").await.unwrap();
        assert!(db
            .aircraft_listing_identity_correction_definitions_valid()
            .await
            .unwrap());
    }

    #[test]
    fn canonical_sql_definitions_ignore_formatting_but_not_semantics() {
        let expected =
            "CREATE TRIGGER t BEFORE UPDATE ON x BEGIN SELECT RAISE(ABORT, 'Keep Case'); END";
        let reformatted =
            "create\n trigger t before update on x begin select raise(abort, 'Keep Case'); end";
        assert_eq!(
            canonical_sql_definition(expected),
            canonical_sql_definition(reformatted)
        );
        assert_ne!(
            canonical_sql_definition(expected),
            canonical_sql_definition(&expected.replace("BEGIN", "WHEN 0 BEGIN"))
        );
        assert_ne!(
            canonical_sql_definition(POSTGRES_CORRECTION_DECISION_FUNCTION_SOURCE),
            canonical_sql_definition("BEGIN RETURN NEW; END;")
        );
    }

    #[test]
    fn aircraft_listing_identity_correction_contract_has_backend_parity() {
        assert_eq!(AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_VERSION, 1);
        for contract_value in [
            AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_MIGRATION,
            AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_CONTRACT_FINGERPRINT,
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(contract_value));
            assert!(POSTGRES_SCHEMA_SQL.contains(contract_value));
            assert!(
                AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_SQLITE_MIGRATION_SQL.contains(contract_value)
            );
            assert!(AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_POSTGRES_MIGRATION_SQL
                .contains(contract_value));
        }
        for required_object in [
            "uq_plugin_submissions_signed_capture",
            "uq_aircraft_listing_identity_correction_receipt",
            "aircraft_listing_identity_corrections_immutable",
            "aircraft_identity_correction_observation_immutable",
            "aircraft_source_identity_receipt_gate",
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(required_object));
            assert!(POSTGRES_SCHEMA_SQL.contains(required_object));
            assert!(AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_SQLITE_MIGRATION_SQL
                .contains(required_object));
            assert!(AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_POSTGRES_MIGRATION_SQL
                .contains(required_object));
        }
    }

    #[tokio::test]
    async fn aircraft_listing_identity_correction_migration_reapplies_on_sqlite() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        for _ in 0..2 {
            let mut connection = pool.acquire().await.unwrap();
            for statement in
                split_sql_statements(AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_SQLITE_MIGRATION_SQL)
            {
                connection.execute(statement).await.unwrap();
            }
        }
        {
            let mut connection = pool.acquire().await.unwrap();
            for statement in
                split_sql_statements(AIRCRAFT_VISUAL_SOURCE_CORRECTIONS_SQLITE_MIGRATION_SQL)
            {
                connection.execute(statement).await.unwrap();
            }
        }
        db.ensure_required_migrations()
            .await
            .expect("the exact correction migration must safely reapply");
        let foreign_key_errors: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(foreign_key_errors, 0);
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[tokio::test]
    async fn aircraft_listing_identity_correction_migration_rejects_uncontracted_table() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        pool.execute(
            "CREATE TABLE aircraft_listing_identity_correction_decisions (id INTEGER PRIMARY KEY)",
        )
        .await
        .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        let mut rejected = None;
        for statement in
            split_sql_statements(AIRCRAFT_LISTING_IDENTITY_CORRECTIONS_SQLITE_MIGRATION_SQL)
        {
            if let Err(error) = connection.execute(statement).await {
                rejected = Some(error.to_string());
                break;
            }
        }
        let error = rejected.expect("an uncontracted same-name table must abort first install");
        assert!(error.contains("CHECK constraint failed"));
    }

    fn table_columns(schema: &str, table: &str) -> Vec<String> {
        let unqualified_marker = format!("CREATE TABLE IF NOT EXISTS {table} (");
        let qualified_marker = format!("CREATE TABLE IF NOT EXISTS public.{table} (");
        let (marker_start, marker_length) = schema
            .find(&unqualified_marker)
            .map(|start| (start, unqualified_marker.len()))
            .or_else(|| {
                schema
                    .find(&qualified_marker)
                    .map(|start| (start, qualified_marker.len()))
            })
            .unwrap_or_else(|| panic!("missing {table} in schema"));
        let start = marker_start + marker_length;
        let mut depth = 1_i64;
        let mut end = start;
        for (offset, character) in schema[start..].char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(depth, 0, "unterminated {table} declaration");

        let body = &schema[start..end];
        let mut columns = Vec::new();
        let mut segment_start = 0;
        let mut segment_depth = 0_i64;
        for (offset, character) in body.char_indices() {
            match character {
                '(' => segment_depth += 1,
                ')' => segment_depth -= 1,
                ',' if segment_depth == 0 => {
                    push_column(&mut columns, &body[segment_start..offset]);
                    segment_start = offset + 1;
                }
                _ => {}
            }
        }
        push_column(&mut columns, &body[segment_start..]);
        columns
    }

    fn push_column(columns: &mut Vec<String>, declaration: &str) {
        let Some(first) = declaration.split_whitespace().next() else {
            return;
        };
        if !matches!(
            first.to_ascii_uppercase().as_str(),
            "CHECK" | "UNIQUE" | "PRIMARY" | "FOREIGN" | "CONSTRAINT"
        ) {
            columns.push(first.trim_matches('"').to_string());
        }
    }

    #[test]
    fn schema_splitter_preserves_sqlite_trigger_bodies() {
        let sql = "CREATE TABLE example (id INTEGER);\n\
                   CREATE TRIGGER example_guard BEFORE INSERT ON example\n\
                   BEGIN\n\
                     SELECT RAISE(ABORT, 'invalid; value');\n\
                   END;\n\
                   CREATE INDEX example_id ON example (id);";
        let statements = split_sql_statements(sql);
        assert_eq!(statements.len(), 3);
        assert!(statements[1].contains("SELECT RAISE"));
        assert!(statements[1].ends_with("END"));
    }

    #[test]
    fn schema_splitter_preserves_commented_sqlite_trigger_bodies() {
        let sql = "CREATE TABLE example (id INTEGER);\n\
                   -- Approval is staged before this trigger.\n\
                   /* Keep the trigger body in one statement. */\n\
                   CREATE TRIGGER example_guard BEFORE INSERT ON example\n\
                   BEGIN\n\
                     SELECT RAISE(ABORT, 'invalid; value');\n\
                   END;\n\
                   CREATE INDEX example_id ON example (id);";
        let statements = split_sql_statements(sql);
        assert_eq!(statements.len(), 3);
        assert!(statements[1].contains("CREATE TRIGGER"));
        assert!(statements[1].contains("SELECT RAISE"));
        assert!(statements[1].ends_with("END"));
    }

    #[test]
    fn schema_splitter_keeps_replacement_triggers_in_execution_order() {
        let statements = split_sql_statements(SQLITE_SCHEMA_SQL);
        let drop_index = statements
            .iter()
            .position(|statement| {
                super::strip_leading_sql_comments(statement).trim()
                    == "DROP TRIGGER IF EXISTS aircraft_designation_faa_binding_requires_provenance"
            })
            .expect("fresh schema must drop the superseded FAA-binding trigger");
        let replacement_indexes = statements
            .iter()
            .enumerate()
            .filter_map(|(index, statement)| {
                super::strip_leading_sql_comments(statement)
                    .trim_start()
                    .starts_with(
                        "CREATE TRIGGER aircraft_designation_faa_binding_requires_provenance",
                    )
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(replacement_indexes.len(), 1);
        assert!(drop_index < replacement_indexes[0]);
    }

    #[test]
    fn schema_splitter_preserves_postgres_function_bodies() {
        let sql = "CREATE OR REPLACE FUNCTION guard() RETURNS TRIGGER\n\
                   LANGUAGE plpgsql AS $function$\n\
                   BEGIN\n\
                     RAISE EXCEPTION 'invalid; value';\n\
                     RETURN NEW;\n\
                   END;\n\
                   $function$;\n\
                   CREATE TRIGGER guard_insert BEFORE INSERT ON example\n\
                   FOR EACH ROW EXECUTE FUNCTION guard();";
        let statements = split_sql_statements(sql);
        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains("RETURN NEW;"));
        assert!(statements[1].starts_with("CREATE TRIGGER"));
    }

    #[tokio::test]
    async fn approved_concrete_model_sqlite_migration_is_idempotent_and_audits_legacy_rows() {
        async fn minimal_connection() -> SqliteConnection {
            let mut connection = SqliteConnection::connect("sqlite::memory:").await.unwrap();
            connection
                .execute(
                    "CREATE TABLE avionics_models (\
                       id INTEGER PRIMARY KEY, normalized_name TEXT NOT NULL, \
                       catalog_status TEXT NOT NULL\
                     )",
                )
                .await
                .unwrap();
            connection
        }
        async fn apply(connection: &mut SqliteConnection) -> Result<(), sqlx::Error> {
            for statement in
                split_sql_statements(AVIONICS_APPROVED_CONCRETE_MODEL_SQLITE_MIGRATION_SQL)
            {
                connection.execute(statement).await?;
            }
            Ok(())
        }

        let mut clean = minimal_connection().await;
        apply(&mut clean).await.unwrap();
        apply(&mut clean)
            .await
            .expect("the exact installed migration must be idempotent");
        let (receipt_count, trigger_count): (i64, i64) = sqlx::query_as(
            r#"
            SELECT
              (SELECT count(*) FROM schema_migration_contracts
               WHERE migration_name = '20260821_avionics_approved_concrete_model'
                 AND contract_version = 1
                 AND contract_fingerprint =
                   '1305564519a99b0ecdfb85a045b9924bf90a33b2914bb6822a219170d541a5f6'),
              (SELECT count(*) FROM sqlite_schema
               WHERE type = 'trigger'
                 AND name LIKE 'avionics_models_approved_concrete_model_%')
            "#,
        )
        .fetch_one(&mut clean)
        .await
        .unwrap();
        assert_eq!((receipt_count, trigger_count), (1, 2));

        let mut legacy = minimal_connection().await;
        legacy
            .execute(
                "INSERT INTO avionics_models (id, normalized_name, catalog_status) \
                 VALUES (7, 'autopilot', 'approved')",
            )
            .await
            .unwrap();
        let error = apply(&mut legacy)
            .await
            .expect_err("a pre-existing approved generic row must stop migration")
            .to_string();
        assert!(error.contains("canonicalize, correct, or demote it before retrying migration"));
        let retained: (String, String) = sqlx::query_as(
            "SELECT normalized_name, catalog_status FROM avionics_models WHERE id = 7",
        )
        .fetch_one(&mut legacy)
        .await
        .unwrap();
        assert_eq!(retained, ("autopilot".to_string(), "approved".to_string()));

        for (index, invalid_key) in ["pfd/mfd", "ads-b", "xm weather &amp; radio"]
            .into_iter()
            .enumerate()
        {
            let mut upgrade = minimal_connection().await;
            sqlx::query(
                "INSERT INTO avionics_models (id, normalized_name, catalog_status) \
                 VALUES (?, ?, 'approved')",
            )
            .bind(index as i64 + 20)
            .bind(invalid_key)
            .execute(&mut upgrade)
            .await
            .unwrap();
            let error = apply(&mut upgrade)
                .await
                .expect_err("noncanonical approved keys must stop upgrade")
                .to_string();
            assert!(
                error.contains("canonicalize, correct, or demote it before retrying migration"),
                "{invalid_key}: {error}"
            );
            let retained: String =
                sqlx::query_scalar("SELECT normalized_name FROM avionics_models")
                    .fetch_one(&mut upgrade)
                    .await
                    .unwrap();
            assert_eq!(retained, invalid_key);
        }

        for invalid_key in [
            "pfd/mfd",
            "ads-b",
            "xm weather &amp; radio",
            "pfd mfd",
            "ads b",
            "xm weather radio",
        ] {
            let error = sqlx::query(
                "INSERT INTO avionics_models (normalized_name, catalog_status) \
                 VALUES (?, 'approved')",
            )
            .bind(invalid_key)
            .execute(&mut clean)
            .await
            .expect_err("fresh invariant must reject generic or noncanonical approved keys")
            .to_string();
            assert!(
                error.contains("canonicalize, correct, or demote it before retrying migration"),
                "{invalid_key}: {error}"
            );
        }
        clean
            .execute(
                "INSERT INTO avionics_models (normalized_name, catalog_status) \
                 VALUES ('gns430w', 'approved')",
            )
            .await
            .expect("a canonical concrete product key must remain admissible");
    }

    #[tokio::test]
    async fn generic_feature_label_migration_is_idempotent_and_audits_approved_rows() {
        async fn minimal_connection() -> SqliteConnection {
            let mut connection = SqliteConnection::connect("sqlite::memory:").await.unwrap();
            connection
                .execute(
                    "CREATE TABLE avionics_models (\
                       id INTEGER PRIMARY KEY, normalized_name TEXT NOT NULL, \
                       catalog_status TEXT NOT NULL\
                     )",
                )
                .await
                .unwrap();
            connection
        }
        async fn apply(connection: &mut SqliteConnection) -> Result<(), sqlx::Error> {
            for statement in
                split_sql_statements(AVIONICS_GENERIC_FEATURE_LABELS_SQLITE_MIGRATION_SQL)
            {
                connection.execute(statement).await?;
            }
            Ok(())
        }
        async fn apply_predecessor(connection: &mut SqliteConnection) -> Result<(), sqlx::Error> {
            for statement in
                split_sql_statements(AVIONICS_APPROVED_CONCRETE_MODEL_SQLITE_MIGRATION_SQL)
            {
                connection.execute(statement).await?;
            }
            Ok(())
        }

        let mut clean = minimal_connection().await;
        clean
            .execute(
                "CREATE TABLE listing_avionics_authorizations (\
                   id INTEGER PRIMARY KEY, avionics_model_id INTEGER NOT NULL\
                 ); \
                 INSERT INTO avionics_models (id, normalized_name, catalog_status) \
                 VALUES (1, 'gns430w', 'approved'); \
                 INSERT INTO listing_avionics_authorizations (id, avionics_model_id) \
                 VALUES (1, 1); \
                 CREATE TRIGGER listing_avionics_authorizations_invalidate_model_proof_update \
                 AFTER UPDATE OF normalized_name ON avionics_models \
                 BEGIN \
                   DELETE FROM listing_avionics_authorizations \
                   WHERE avionics_model_id = OLD.id; \
                 END",
            )
            .await
            .unwrap();
        apply(&mut clean).await.unwrap();
        apply(&mut clean)
            .await
            .expect("the exact installed migration must be idempotent");
        let receipt_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM schema_migration_contracts \
             WHERE migration_name = ? AND contract_version = ? \
               AND contract_fingerprint = ?",
        )
        .bind(AVIONICS_GENERIC_FEATURE_LABELS_MIGRATION)
        .bind(AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_VERSION)
        .bind(AVIONICS_GENERIC_FEATURE_LABELS_CONTRACT_FINGERPRINT)
        .fetch_one(&mut clean)
        .await
        .unwrap();
        assert_eq!(receipt_count, 1);
        let (model_name, authorization_count): (String, i64) = sqlx::query_as(
            "SELECT model.normalized_name, \
               (SELECT count(*) FROM listing_avionics_authorizations) \
             FROM avionics_models model WHERE model.id = 1",
        )
        .fetch_one(&mut clean)
        .await
        .unwrap();
        assert_eq!((model_name.as_str(), authorization_count), ("gns430w", 1));

        let mut legacy = minimal_connection().await;
        legacy
            .execute(
                "INSERT INTO avionics_models (id, normalized_name, catalog_status) \
                 VALUES (21, 'synthetic vision', 'approved')",
            )
            .await
            .unwrap();
        let error = apply(&mut legacy)
            .await
            .expect_err("an approved feature-only row must stop migration")
            .to_string();
        assert!(error.contains("canonicalize, correct, or demote it before retrying migration"));
        let retained: (String, String) = sqlx::query_as(
            "SELECT normalized_name, catalog_status FROM avionics_models WHERE id = 21",
        )
        .fetch_one(&mut legacy)
        .await
        .unwrap();
        assert_eq!(
            retained,
            ("synthetic vision".to_string(), "approved".to_string())
        );

        for (invalid_key, tamper_predecessor) in [("autopilot", false), ("pfd/mfd", true)] {
            let mut upgrade = minimal_connection().await;
            if tamper_predecessor {
                apply_predecessor(&mut upgrade).await.unwrap();
                upgrade
                    .execute(
                        "DROP TRIGGER avionics_models_approved_concrete_model_insert; \
                         DROP TRIGGER avionics_models_approved_concrete_model_update",
                    )
                    .await
                    .unwrap();
            }
            sqlx::query(
                "INSERT INTO avionics_models (normalized_name, catalog_status) \
                 VALUES (?, 'approved')",
            )
            .bind(invalid_key)
            .execute(&mut upgrade)
            .await
            .unwrap();
            let error = apply(&mut upgrade)
                .await
                .expect_err(
                    "v2 must audit all approved rows even if the prior invariant is absent or tampered",
                )
                .to_string();
            assert!(
                error.contains("canonicalize, correct, or demote it before retrying migration"),
                "{invalid_key}: {error}"
            );
            let receipt_count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM schema_migration_contracts \
                 WHERE migration_name = '20260824_avionics_generic_feature_labels'",
            )
            .fetch_one(&mut upgrade)
            .await
            .unwrap();
            assert_eq!(receipt_count, 0, "{invalid_key}");
        }

        for invalid_key in [
            "synthetic vision",
            "svt",
            "safetaxi",
            "flitecharts",
            "electronic stability and protection",
            "wireless data loading",
            "ads b in out",
        ] {
            let error = sqlx::query(
                "INSERT INTO avionics_models (normalized_name, catalog_status) \
                 VALUES (?, 'approved')",
            )
            .bind(invalid_key)
            .execute(&mut clean)
            .await
            .expect_err("the installed invariant must reject feature-only approved keys")
            .to_string();
            assert!(
                error.contains("canonicalize, correct, or demote it before retrying migration"),
                "{invalid_key}: {error}"
            );
        }
        clean
            .execute(
                "INSERT INTO avionics_models (normalized_name, catalog_status) \
                 VALUES ('gtx345 ads b in out', 'approved')",
            )
            .await
            .expect("a concrete product with a feature annotation remains admissible");
    }

    #[tokio::test]
    async fn startup_requires_and_accepts_generic_feature_label_migration() {
        let (database_path, database_url) =
            unique_sqlite_test_database("generic-feature-migration");
        AppDb::connect(&database_url).await.unwrap().close().await;

        let mut connection = SqliteConnection::connect(&database_url).await.unwrap();
        connection
            .execute(
                "DELETE FROM schema_migration_contracts \
                 WHERE migration_name = '20260824_avionics_generic_feature_labels'",
            )
            .await
            .unwrap();
        for statement in split_sql_statements(AVIONICS_APPROVED_CONCRETE_MODEL_SQLITE_MIGRATION_SQL)
        {
            connection.execute(statement).await.unwrap();
        }
        connection.close().await.unwrap();

        let error = connect_error(AppDb::connect(&database_url).await);
        assert!(
            error.contains("migrations/20260824_avionics_generic_feature_labels.sqlite.sql"),
            "{error}"
        );

        let mut connection = SqliteConnection::connect(&database_url).await.unwrap();
        for statement in split_sql_statements(AVIONICS_GENERIC_FEATURE_LABELS_SQLITE_MIGRATION_SQL)
        {
            connection.execute(statement).await.unwrap();
        }
        connection.close().await.unwrap();

        AppDb::connect(&database_url).await.unwrap().close().await;
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn startup_rejects_noop_approved_concrete_model_trigger_bodies() {
        for (label, trigger_name, event) in [
            (
                "approved-concrete-noop-insert",
                "avionics_models_approved_concrete_model_insert",
                "INSERT",
            ),
            (
                "approved-concrete-noop-update",
                "avionics_models_approved_concrete_model_update",
                "UPDATE OF catalog_status, normalized_name",
            ),
        ] {
            let (database_path, database_url) = unique_sqlite_test_database(label);
            AppDb::connect(&database_url).await.unwrap().close().await;
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            pool.execute(format!("DROP TRIGGER {trigger_name}").as_str())
                .await
                .unwrap();
            pool.execute(
                format!(
                    "CREATE TRIGGER {trigger_name} BEFORE {event} ON avionics_models BEGIN SELECT 1; END"
                )
                .as_str(),
            )
            .await
            .unwrap();
            pool.close().await;

            let error = connect_error(AppDb::connect(&database_url).await);
            assert!(
                error.contains("approved concrete-model invariant"),
                "{error}"
            );
            std::fs::remove_file(database_path).unwrap();
        }
    }

    #[test]
    fn postgres_approved_concrete_model_object_contract_is_versioned_and_exact() {
        let mut expected = PostgresApprovedConcreteModelTriggerDefinition {
            trigger_name: "avionics_models_approved_concrete_model".to_string(),
            trigger_type: 23,
            has_no_when_clause: true,
            update_columns: "catalog_status,normalized_name".to_string(),
            trigger_enabled: "O".to_string(),
            trigger_argument_count: 0,
            relation_schema: "public".to_string(),
            relation_name: "avionics_models".to_string(),
            relation_oid_matches: true,
            function_name: "enforce_avionics_approved_concrete_model".to_string(),
            function_schema: "public".to_string(),
            function_oid_matches: true,
            function_source: POSTGRES_AVIONICS_APPROVED_CONCRETE_MODEL_FUNCTION_SOURCE.to_string(),
            function_configuration: String::new(),
            function_language: "plpgsql".to_string(),
            returns_trigger: true,
            function_argument_count: 0,
            ordinary_function: true,
            security_definer: false,
            strict: false,
            volatility: "v".to_string(),
            parallel_safety: "u".to_string(),
        };
        let fingerprint = versioned_avionics_approved_concrete_model_object_fingerprint(
            &postgres_approved_concrete_model_object_payload(&expected),
        );
        assert_eq!(
            fingerprint,
            POSTGRES_AVIONICS_APPROVED_CONCRETE_MODEL_OBJECT_CONTRACT_FINGERPRINT
        );
        expected.function_source = "BEGIN RETURN NEW; END;".to_string();
        assert_ne!(
            versioned_avionics_approved_concrete_model_object_fingerprint(
                &postgres_approved_concrete_model_object_payload(&expected),
            ),
            POSTGRES_AVIONICS_APPROVED_CONCRETE_MODEL_OBJECT_CONTRACT_FINGERPRINT,
            "a same-named no-op PostgreSQL function must not satisfy the object contract"
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_startup_rejects_same_named_noop_approved_concrete_model_function() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;
        AppDb::connect(&database_url).await.unwrap().close().await;
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        pool.execute(
            r#"
            CREATE OR REPLACE FUNCTION public.enforce_avionics_approved_concrete_model()
            RETURNS TRIGGER
            LANGUAGE plpgsql
            AS $function$
            BEGIN
              RETURN NEW;
            END;
            $function$
            "#,
        )
        .await
        .unwrap();
        pool.close().await;

        let error = connect_error(AppDb::connect(&database_url).await);
        assert!(
            error.contains("approved concrete-model invariant"),
            "{error}"
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_generic_feature_label_migration_audits_without_model_updates() {
        async fn reset_minimal(pool: &sqlx::PgPool) {
            sqlx::raw_sql(
                "DROP SCHEMA public CASCADE; \
                 CREATE SCHEMA public; \
                 CREATE TABLE public.avionics_models (\
                   id BIGINT PRIMARY KEY, \
                   normalized_name TEXT NOT NULL, \
                   catalog_status TEXT NOT NULL\
                 )",
            )
            .execute(pool)
            .await
            .unwrap();
        }

        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();

        for (invalid_key, tamper_predecessor) in [("autopilot", false), ("pfd/mfd", true)] {
            reset_minimal(&pool).await;
            if tamper_predecessor {
                sqlx::raw_sql(AVIONICS_APPROVED_CONCRETE_MODEL_POSTGRES_MIGRATION_SQL)
                    .execute(&pool)
                    .await
                    .unwrap();
                pool.execute(
                    "DROP TRIGGER avionics_models_approved_concrete_model \
                     ON public.avionics_models",
                )
                .await
                .unwrap();
            }
            sqlx::query(
                "INSERT INTO public.avionics_models (id, normalized_name, catalog_status) \
                 VALUES (1, $1, 'approved')",
            )
            .bind(invalid_key)
            .execute(&pool)
            .await
            .unwrap();
            let error = sqlx::raw_sql(AVIONICS_GENERIC_FEATURE_LABELS_POSTGRES_MIGRATION_SQL)
                .execute(&pool)
                .await
                .expect_err(
                    "v2 must audit all approved rows even if the prior invariant is absent or tampered",
                )
                .to_string();
            assert!(
                error.contains("canonicalize, correct, or demote it before retrying migration"),
                "{invalid_key}: {error}"
            );
            pool.execute("ROLLBACK").await.unwrap();
        }

        reset_minimal(&pool).await;
        sqlx::raw_sql(
            "CREATE TABLE public.listing_avionics_authorizations (\
               id BIGINT PRIMARY KEY, avionics_model_id BIGINT NOT NULL\
             ); \
             CREATE FUNCTION public.invalidate_model_authorization() \
             RETURNS TRIGGER LANGUAGE plpgsql AS $function$ \
             BEGIN \
               DELETE FROM public.listing_avionics_authorizations \
               WHERE avionics_model_id = OLD.id; \
               RETURN NEW; \
             END \
             $function$; \
             CREATE TRIGGER listing_avionics_authorizations_invalidate_model_proof_update \
             AFTER UPDATE OF normalized_name ON public.avionics_models \
             FOR EACH ROW EXECUTE FUNCTION public.invalidate_model_authorization(); \
             INSERT INTO public.avionics_models (id, normalized_name, catalog_status) \
             VALUES (1, 'gns430w', 'approved'); \
             INSERT INTO public.listing_avionics_authorizations (id, avionics_model_id) \
             VALUES (1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(AVIONICS_GENERIC_FEATURE_LABELS_POSTGRES_MIGRATION_SQL)
            .execute(&pool)
            .await
            .unwrap();
        let (model_name, authorization_count): (String, i64) = sqlx::query_as(
            "SELECT model.normalized_name, \
               (SELECT count(*) FROM public.listing_avionics_authorizations) \
             FROM public.avionics_models model WHERE model.id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((model_name.as_str(), authorization_count), ("gns430w", 1));
    }

    #[test]
    fn approved_concrete_model_sql_uses_the_complete_rust_generic_vocabulary() {
        let vocabulary = [
            "",
            "unknown",
            "generic",
            "standard",
            "factory",
            "oem",
            "various",
            "multiple",
            "avionics",
            "avionics suite",
            "integrated avionics",
            "integrated avionics suite",
            "glass panel",
            "flight instruments",
            "standard flight instruments",
            "standard vfr avionics",
            "standard ifr avionics",
            "radio",
            "radios",
            "nav",
            "com",
            "nav com",
            "gps nav com",
            "navigation system",
            "gps",
            "autopilot",
            "flight director",
            "transponder",
            "ads b",
            "ads b in",
            "ads b out",
            "ads b in out",
            "ads b in and out",
            "weather radar",
            "audio panel",
            "standard audio panel",
            "audio controller",
            "audio control panel",
            "display",
            "flight display",
            "pfd",
            "mfd",
            "pfd mfd",
            "navigation indicator",
            "traffic",
            "active traffic",
            "traffic advisory system",
            "datalink",
            "datalink weather",
            "xm",
            "xm weather",
            "xm radio",
            "xm weather radio",
            "lightning detection",
            "terrain awareness",
            "terrain awareness system",
            "terrain avoidance system",
            "taws",
            "synthetic vision",
            "synthetic vision system",
            "svt",
            "safetaxi",
            "safe taxi",
            "flitecharts",
            "flite charts",
            "charts",
            "electronic charts",
            "electronic stability and protection",
            "electronic stability protection",
            "stability and protection",
            "wireless data loading",
            "wireless database loading",
            "engine monitor",
            "engine fuel monitoring",
            "standby instrument",
            "backup instruments",
            "elt",
            "adf",
            "dme",
            "ahrs",
            "air data computer",
            "radar altimeter",
            "magnetometer",
            "clock timer",
            "waas",
            "waas gps",
            "dual waas",
            "remote transponder",
            "transponder ads b",
            "stormscope",
            "standard radio navigation",
            "equipment",
        ];
        for label in vocabulary {
            assert!(crate::normalize::is_generic_avionics_model_name(label));
            let quoted = format!("'{label}'");
            for definition in [
                SQLITE_SCHEMA_SQL,
                POSTGRES_SCHEMA_SQL,
                AVIONICS_GENERIC_FEATURE_LABELS_SQLITE_MIGRATION_SQL,
                AVIONICS_GENERIC_FEATURE_LABELS_POSTGRES_MIGRATION_SQL,
            ] {
                assert!(
                    definition.contains(&quoted),
                    "database policy is missing Rust generic label {label:?}"
                );
            }
        }
        for definition in [
            SQLITE_SCHEMA_SQL,
            AVIONICS_APPROVED_CONCRETE_MODEL_SQLITE_MIGRATION_SQL,
        ] {
            assert!(definition.contains("GLOB '*[^a-z0-9 ]*'"));
            assert!(definition.contains("instr(NEW.normalized_name, '  ') > 0"));
            assert!(definition.contains("canonicalize, correct, or demote"));
        }
        for definition in [
            POSTGRES_SCHEMA_SQL,
            AVIONICS_APPROVED_CONCRETE_MODEL_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(definition.contains("!~ '^[a-z0-9]+( [a-z0-9]+)*$'"));
            assert!(definition.contains("canonicalize, correct, or demote"));
        }
        for definition in [
            AVIONICS_GENERIC_FEATURE_LABELS_SQLITE_MIGRATION_SQL,
            AVIONICS_GENERIC_FEATURE_LABELS_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(
                !definition.contains("SET normalized_name = normalized_name"),
                "the migration audit must not fire model-update side effects"
            );
        }
        assert!(AVIONICS_GENERIC_FEATURE_LABELS_SQLITE_MIGRATION_SQL
            .contains("avionics_generic_feature_labels_audit_reject"));
        assert!(AVIONICS_GENERIC_FEATURE_LABELS_POSTGRES_MIGRATION_SQL
            .contains("FROM ONLY public.avionics_models"));
        for spelling in [
            "pfd/mfd",
            "ads-b",
            "xm weather &amp; radio",
            "ADS-B In/Out",
            "Electronic Stability &amp; Protection",
        ] {
            assert!(crate::normalize::is_generic_avionics_model_name(spelling));
        }
    }

    #[tokio::test]
    async fn legacy_listing_schema_requires_valuation_hardening_first() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY)",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("legacy listing schema must fail preflight")
            .to_string();
        assert!(error.contains("`aircraft_sale_listings` is missing `ingestion_state`"));
        assert!(error.contains("migrations/20260720_valuation_data_hardening.sqlite.sql"));
    }

    #[tokio::test]
    async fn hardened_listing_with_legacy_avionics_requires_catalog_migration() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY, ingestion_state TEXT)",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("legacy avionics schema must fail preflight")
            .to_string();
        assert!(error.contains("`avionics_models` is missing `catalog_status`"));
        assert!(error.contains("migrations/20260721_avionics_catalog_curation.sqlite.sql"));
    }

    #[tokio::test]
    async fn curated_catalog_requires_join_only_multi_type_migration() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY, ingestion_state TEXT)",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY, catalog_status TEXT, avionics_type_id INTEGER)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("scalar avionics catalog must fail preflight")
            .to_string();
        assert!(error.contains("`avionics_model_types` capability table"));
        assert!(error.contains("without scalar `avionics_models.avionics_type_id`"));
        assert!(error.contains("migrations/20260721_avionics_multi_type.sqlite.sql"));
    }

    #[tokio::test]
    async fn skeletal_catalog_objects_do_not_satisfy_migration_preflight() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY, ingestion_state TEXT CHECK (ingestion_state IN ('incomplete', 'pending_review', 'ready', 'quarantined')))",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY, catalog_status TEXT)",
            "CREATE TABLE avionics_model_types (avionics_model_id INTEGER, avionics_type_id INTEGER)",
            "CREATE TABLE aircraft_identity_observations (id INTEGER PRIMARY KEY)",
            "CREATE TABLE aircraft_engine_catalog_models (id INTEGER PRIMARY KEY)",
            "CREATE TABLE aircraft_propeller_catalog_models (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_snapshots (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_aircraft (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_aircraft_references (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_engine_references (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_coverage (id INTEGER PRIMARY KEY)",
            "CREATE TABLE aircraft_sale_listing_pending_reviews (id INTEGER PRIMARY KEY)",
            "CREATE TABLE avionics_manufacturer_canonical_keys (avionics_manufacturer_id INTEGER PRIMARY KEY, canonical_manufacturer_key TEXT)",
            "CREATE TABLE avionics_approved_product_identities (avionics_model_id INTEGER PRIMARY KEY)",
            "CREATE TABLE avionics_catalog_consolidation_guard (duplicate_model_id INTEGER PRIMARY KEY, survivor_model_id INTEGER)",
            "CREATE VIEW avionics_catalog_authorized_consolidations AS SELECT 1 AS duplicate_model_id, 2 AS survivor_model_id",
            "CREATE TABLE aircraft_designation_faa_bindings (aircraft_designation_id INTEGER PRIMARY KEY)",
            "CREATE TABLE aircraft_sale_listing_identity_assignments (id INTEGER PRIMARY KEY)",
            "CREATE TABLE aircraft_sale_listing_current_identity_assignments (aircraft_sale_listing_id INTEGER PRIMARY KEY)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("placeholder objects without enforcement or a marker must fail preflight")
            .to_string();
        assert!(error.contains("canonical approved-identity registry"));
        assert!(error.contains("20260725_identity_deduplication_postconditions.sqlite.sql"));
    }

    #[tokio::test]
    async fn existing_database_requires_clean_aircraft_reference_catalog() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY, ingestion_state TEXT)",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY, catalog_status TEXT)",
            "CREATE TABLE avionics_model_types (avionics_model_id INTEGER, avionics_type_id INTEGER)",
            "CREATE TABLE engine_models (id INTEGER PRIMARY KEY)",
            "CREATE TABLE propeller_models (id INTEGER PRIMARY KEY)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("legacy aircraft reference storage must fail preflight")
            .to_string();
        assert!(error.contains("clean aircraft identity/reference catalog"));
        assert!(error.contains("20260722_aircraft_reference_catalog.sqlite.sql"));
    }

    #[tokio::test]
    async fn empty_database_passes_preflight_and_initializes_fresh_schema() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("fresh database should initialize");
        db.ensure_required_migrations()
            .await
            .expect("fresh schema should pass subsequent preflight");
    }

    #[tokio::test]
    async fn existing_catalog_requires_identity_deduplication_postconditions() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TABLE avionics_catalog_consolidation_guard")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing canonical identity postconditions must fail preflight")
            .to_string();
        assert!(error.contains("canonical approved-identity registry"));
        assert!(error.contains("20260725_identity_deduplication_postconditions.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_identity_enforcement_trigger_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TRIGGER aircraft_sale_listing_avionics_semantic_unique_insert")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing identity enforcement must fail preflight")
            .to_string();
        assert!(error.contains("canonical approved-identity registry"));
        assert!(error.contains("20260725_identity_deduplication_postconditions.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_product_registry_sync_trigger_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TRIGGER avionics_models_canonical_identity_sync_update")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing approved-product registry synchronization must fail preflight")
            .to_string();
        assert!(error.contains("canonical approved-identity registry"));
        assert!(error.contains("20260725_identity_deduplication_postconditions.sqlite.sql"));
    }

    #[tokio::test]
    async fn altered_identity_contract_marker_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            UPDATE schema_migration_contracts
            SET contract_fingerprint =
              '0000000000000000000000000000000000000000000000000000000000000000'
            WHERE migration_name = '20260725_identity_deduplication_postconditions'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("altered migration marker must fail preflight")
            .to_string();
        assert!(error.contains("canonical approved-identity registry"));
        assert!(error.contains("20260725_identity_deduplication_postconditions.sqlite.sql"));
    }

    #[tokio::test]
    async fn existing_listings_require_immutable_aircraft_assignments() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TABLE aircraft_designation_faa_bindings")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing immutable aircraft assignments must fail preflight")
            .to_string();
        assert!(error.contains("FAA-backed aircraft identity assignments"));
        assert!(error.contains("20260725_listing_aircraft_identity.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_aircraft_contract_marker_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name = '20260725_listing_aircraft_identity'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing migration marker must fail preflight")
            .to_string();
        assert!(error.contains("FAA-backed aircraft identity assignments"));
        assert!(error.contains("20260725_listing_aircraft_identity.sqlite.sql"));
    }

    #[tokio::test]
    async fn stale_aircraft_identity_v1_contract_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            UPDATE schema_migration_contracts
            SET contract_version = 1,
                contract_fingerprint =
                  '305f5d269aa5561fad6845bcb9a76bd68e856a994ea528e585f6d32051adc968'
            WHERE migration_name = '20260725_listing_aircraft_identity'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("draft v1 aircraft identity contract must fail preflight")
            .to_string();
        assert!(error.contains("FAA-backed aircraft identity assignments"));
        assert!(error.contains("20260725_listing_aircraft_identity.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_aircraft_projection_object_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TABLE aircraft_valuation_projection_transitions")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing aircraft projection object must fail preflight")
            .to_string();
        assert!(error.contains("aircraft compatibility projection contract"));
        assert!(error.contains("20260726_listing_aircraft_compatibility_projection.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_exact_aircraft_projection_view_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP VIEW aircraft_sale_listing_exact_compatibility_projections")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing exact projection view must fail preflight")
            .to_string();
        assert!(error.contains("aircraft compatibility projection contract"));
        assert!(error.contains("20260726_listing_aircraft_compatibility_projection.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_aircraft_projection_enforcement_trigger_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TRIGGER aircraft_valuation_transition_execute")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing aircraft projection enforcement must fail preflight")
            .to_string();
        assert!(error.contains("aircraft compatibility projection contract"));
        assert!(error.contains("20260726_listing_aircraft_compatibility_projection.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_aircraft_projection_contract_marker_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name = '20260726_listing_aircraft_compatibility_projection'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing aircraft projection marker must fail preflight")
            .to_string();
        assert!(error.contains("aircraft compatibility projection contract"));
        assert!(error.contains("20260726_listing_aircraft_compatibility_projection.sqlite.sql"));
    }

    #[tokio::test]
    async fn altered_aircraft_projection_contract_marker_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            UPDATE schema_migration_contracts
            SET contract_version = 1,
                contract_fingerprint =
                  '0000000000000000000000000000000000000000000000000000000000000000'
            WHERE migration_name = '20260726_listing_aircraft_compatibility_projection'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("altered aircraft projection marker must fail preflight")
            .to_string();
        assert!(error.contains("aircraft compatibility projection contract"));
        assert!(error.contains("20260726_listing_aircraft_compatibility_projection.sqlite.sql"));
    }

    #[tokio::test]
    async fn legacy_aircraft_optional_decision_contract_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name =
              '20260728_aircraft_identity_no_supported_selection'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("legacy optional-decision contract must fail preflight")
            .to_string();
        assert!(error.contains("legacy optional-dimension rejection contract"));
        assert!(error.contains("20260728_aircraft_identity_no_supported_selection.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_no_supported_selection_claim_guard_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TRIGGER aircraft_identity_no_supported_selection_claim_insert")
            .await
            .unwrap();
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("missing no-evidence guard must fail preflight")
            .to_string();
        assert!(error.contains("legacy optional-dimension rejection contract"));
        assert!(error.contains("20260728_aircraft_identity_no_supported_selection.sqlite.sql"));
    }

    #[tokio::test]
    async fn retrieval_key_validators_cannot_replace_the_data_repair_contract() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT count(*)
                FROM sqlite_schema
                WHERE type = 'trigger'
                  AND name GLOB 'aircraft_*_retrieval_key_validate_*'
                "#,
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            8,
            "fresh-schema validators must be present before removing only the repair ledger"
        );
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name = '20260729_aircraft_catalog_retrieval_keys'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("validators alone must never attest that the data repair ran")
            .to_string();
        assert!(
            error.contains("deterministic retrieval-key data repair"),
            "{error}"
        );
        assert!(error.contains("20260729_aircraft_catalog_retrieval_keys.sqlite.sql"));
    }

    #[tokio::test]
    async fn startup_checks_repair_contract_before_fresh_schema_initialization() {
        let (database_path, database_url) =
            unique_sqlite_test_database("aircraft-retrieval-key-startup-gate");
        {
            let db = AppDb::connect(&database_url).await.unwrap();
            let DatabaseBackend::Sqlite(pool) = db.backend() else {
                unreachable!()
            };
            sqlx::query(
                r#"
                DELETE FROM schema_migration_contracts
                WHERE migration_name = '20260729_aircraft_catalog_retrieval_keys'
                "#,
            )
            .execute(pool)
            .await
            .unwrap();
        }

        let error = match AppDb::connect(&database_url).await {
            Ok(_) => panic!("startup must not install a missing repair contract from fresh schema"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("deterministic retrieval-key data repair"),
            "{error}"
        );

        let inspection_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT count(*)
                FROM schema_migration_contracts
                WHERE migration_name = '20260729_aircraft_catalog_retrieval_keys'
                "#,
            )
            .fetch_one(&inspection_pool)
            .await
            .unwrap(),
            0,
            "failed startup must not backfill the repair ledger from fresh-schema DDL"
        );
        inspection_pool.close().await;
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn missing_aircraft_retrieval_key_validator_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TRIGGER aircraft_make_retrieval_key_validate_update")
            .await
            .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("the repair marker cannot replace ongoing key validation")
            .to_string();
        assert!(error.contains("deterministic retrieval-key data repair"));
        assert!(error.contains("20260729_aircraft_catalog_retrieval_keys.sqlite.sql"));
    }

    #[tokio::test]
    async fn altered_aircraft_retrieval_key_contract_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            UPDATE schema_migration_contracts
            SET contract_fingerprint =
              '0000000000000000000000000000000000000000000000000000000000000000'
            WHERE migration_name = '20260729_aircraft_catalog_retrieval_keys'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("an altered repair contract must fail preflight")
            .to_string();
        assert!(error.contains("deterministic retrieval-key data repair"));
        assert!(error.contains("20260729_aircraft_catalog_retrieval_keys.sqlite.sql"));
    }

    #[tokio::test]
    async fn migrated_aircraft_schema_requires_listing_pending_review_migration() {
        let db = sqlite_db_with_statements(&[
            "CREATE TABLE aircraft_sale_listings (id INTEGER PRIMARY KEY, ingestion_state TEXT CHECK (ingestion_state IN ('incomplete', 'ready', 'quarantined')))",
            "CREATE TABLE avionics_models (id INTEGER PRIMARY KEY, catalog_status TEXT)",
            "CREATE TABLE avionics_model_types (avionics_model_id INTEGER, avionics_type_id INTEGER)",
            "CREATE TABLE aircraft_identity_observations (id INTEGER PRIMARY KEY)",
            "CREATE TABLE aircraft_engine_catalog_models (id INTEGER PRIMARY KEY)",
            "CREATE TABLE aircraft_propeller_catalog_models (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_snapshots (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_aircraft (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_aircraft_references (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_engine_references (id INTEGER PRIMARY KEY)",
            "CREATE TABLE faa_registry_coverage (id INTEGER PRIMARY KEY)",
        ])
        .await;
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("pre-review schema must fail preflight")
            .to_string();
        assert!(error.contains("pending-review handoff"));
        assert!(error.contains("20260724_listing_pending_reviews.sqlite.sql"));
    }

    #[tokio::test]
    async fn fresh_schema_reinitialization_is_idempotent() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        db.initialize().await.unwrap();
        db.ensure_required_migrations().await.unwrap();
    }

    #[tokio::test]
    async fn startup_preserves_reference_cutover_install_timestamp() {
        let (database_path, database_url) =
            unique_sqlite_test_database("reference-cutover-marker-time");
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let sentinel = "2000-01-01 00:00:00";
        sqlx::query(
            "UPDATE schema_migration_contracts SET installed_at = ? WHERE migration_name = ?",
        )
        .bind(sentinel)
        .bind(REFERENCE_CATALOG_CUTOVER_MIGRATION)
        .execute(pool)
        .await
        .unwrap();
        drop(db);

        let reopened = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = reopened.backend() else {
            unreachable!()
        };
        let installed_at: String = sqlx::query_scalar(
            "SELECT installed_at FROM schema_migration_contracts WHERE migration_name = ?",
        )
        .bind(REFERENCE_CATALOG_CUTOVER_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(installed_at, sentinel);
        drop(reopened);
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn schema_rerun_rejects_marker_mismatch_before_initialization() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE schema_migration_contracts SET contract_fingerprint = ? WHERE migration_name = ?",
        )
        .bind("0".repeat(64))
        .bind(REFERENCE_CATALOG_CUTOVER_MIGRATION)
        .execute(pool)
        .await
        .unwrap();

        let error = db
            .initialize()
            .await
            .expect_err("a mismatched cutover marker must reject schema rerun");
        let error = format!("{error:#}");
        assert!(error.contains("canonical price-basis and complete-fact-set contract"));
    }

    #[tokio::test]
    async fn schema_rerun_never_heals_marker_present_cutover_damage() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        pool.execute("DROP TABLE official_dollar_normalization_facts")
            .await
            .unwrap();

        let error = db
            .initialize()
            .await
            .expect_err("marker-present cutover damage must reject schema rerun");
        let error = format!("{error:#}");
        assert!(error.contains("canonical price-basis and complete-fact-set contract"));
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'official_dollar_normalization_facts'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(table_count, 0, "schema rerun must not heal the damage");
    }

    #[tokio::test]
    async fn missing_human_consolidation_contract_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name = '20260731_avionics_human_reviewed_consolidation'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a missing human-consolidation contract must fail preflight")
            .to_string();
        assert!(error.contains("evidence-backed human-review consolidation contract"));
        assert!(error.contains("20260731_avionics_human_reviewed_consolidation.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_descriptive_consolidation_contract_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name = '20260808_avionics_descriptive_consolidation'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a missing descriptive-consolidation contract must fail preflight")
            .to_string();
        assert!(error.contains("descriptive-equivalent human-consolidation contract"));
        assert!(error.contains("20260808_avionics_descriptive_consolidation.sqlite.sql"));
    }

    #[tokio::test]
    async fn descriptive_consolidation_migration_repairs_and_reapplies_on_sqlite() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("DROP TRIGGER avionics_catalog_human_consolidation_members_validate_insert")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DROP VIEW avionics_catalog_valid_human_consolidation_pairs")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name = '20260808_avionics_descriptive_consolidation'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        for _ in 0..2 {
            let mut connection = pool.acquire().await.unwrap();
            for statement in
                split_sql_statements(AVIONICS_DESCRIPTIVE_CONSOLIDATION_SQLITE_MIGRATION_SQL)
            {
                connection.execute(statement).await.unwrap();
            }
        }
        db.ensure_required_migrations()
            .await
            .expect("the descriptive-consolidation migration must repair and reapply");
    }

    #[tokio::test]
    async fn missing_grounded_exact_model_consolidation_contract_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name =
                  '20260810_avionics_grounded_exact_model_consolidation'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a missing grounded exact-model contract must fail preflight")
            .to_string();
        assert!(error.contains("grounded exact-model duplicate consolidation contract"));
        assert!(error.contains("20260810_avionics_grounded_exact_model_consolidation.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_grounded_exact_model_consolidation_view_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("DROP VIEW avionics_catalog_valid_grounded_consolidation_pairs")
            .execute(pool)
            .await
            .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a missing grounded exact-model view must fail preflight")
            .to_string();
        assert!(error.contains("grounded exact-model duplicate consolidation contract"));
        assert!(error.contains("20260810_avionics_grounded_exact_model_consolidation.sqlite.sql"));
    }

    #[tokio::test]
    async fn grounded_exact_model_consolidation_migration_repairs_and_reapplies_on_sqlite() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("DROP VIEW avionics_catalog_valid_grounded_consolidation_pairs")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name =
                  '20260810_avionics_grounded_exact_model_consolidation'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        for _ in 0..2 {
            let mut connection = pool.acquire().await.unwrap();
            for statement in split_sql_statements(
                AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_SQLITE_MIGRATION_SQL,
            ) {
                connection.execute(statement).await.unwrap();
            }
        }
        db.ensure_required_migrations()
            .await
            .expect("the grounded exact-model migration must repair and reapply");
    }

    #[tokio::test]
    async fn missing_avionics_source_origin_contract_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name = '20260801_avionics_authoritative_source_origins'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a missing source-origin contract must fail preflight")
            .to_string();
        assert!(error.contains("exact-origin authority approvals"));
        assert!(error.contains("20260801_avionics_authoritative_source_origins.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_garmin_origin_bootstrap_trigger_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("DROP TRIGGER avionics_garmin_authoritative_source_origins_bootstrap")
            .execute(pool)
            .await
            .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a missing delayed bootstrap trigger must fail preflight")
            .to_string();
        assert!(error.contains("exact-origin authority approvals"));
        assert!(error.contains("20260801_avionics_authoritative_source_origins.sqlite.sql"));
    }

    #[tokio::test]
    async fn corrupted_reuse_trigger_and_index_fail_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("DROP TRIGGER avionics_product_reuse_invalidate_origin_revocation")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            CREATE TRIGGER avionics_product_reuse_invalidate_origin_revocation
            AFTER INSERT ON users
            BEGIN
              SELECT 1;
            END
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("DROP INDEX idx_avionics_product_reuse_origin")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("CREATE INDEX idx_avionics_product_reuse_origin ON users(email)")
            .execute(pool)
            .await
            .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("same-name no-op reuse objects must not pass startup")
            .to_string();
        assert!(error.contains("target-aware current-policy reuse-attestation gate"));
        assert!(error.contains("20260807_avionics_product_reuse_v2.sqlite.sql"));
    }

    #[test]
    fn avionics_reuse_attestation_contract_has_backend_parity() {
        let table = "avionics_product_reuse_attestations";
        assert_eq!(
            table_columns(SQLITE_SCHEMA_SQL, table),
            table_columns(POSTGRES_SCHEMA_SQL, table),
            "SQLite/Postgres schema column mismatch for {table}"
        );
        assert_eq!(
            table_columns(AVIONICS_REUSE_ATTESTATIONS_SQLITE_MIGRATION_SQL, table),
            table_columns(AVIONICS_REUSE_ATTESTATIONS_POSTGRES_MIGRATION_SQL, table),
            "SQLite/Postgres migration column mismatch for {table}"
        );
        assert_eq!(AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_VERSION, 2);
        assert_eq!(AVIONICS_PRODUCT_REUSE_V2_CONTRACT_VERSION, 1);
        for contract_value in [
            AVIONICS_PRODUCT_REUSE_ATTESTATIONS_MIGRATION,
            AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_FINGERPRINT,
            AVIONICS_PRODUCT_REUSE_V2_MIGRATION,
            AVIONICS_PRODUCT_REUSE_V2_CONTRACT_FINGERPRINT,
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(contract_value));
            assert!(POSTGRES_SCHEMA_SQL.contains(contract_value));
            assert!(
                AVIONICS_REUSE_ATTESTATIONS_SQLITE_MIGRATION_SQL.contains(contract_value)
                    || AVIONICS_REUSE_V2_SQLITE_MIGRATION_SQL.contains(contract_value)
            );
            assert!(
                AVIONICS_REUSE_ATTESTATIONS_POSTGRES_MIGRATION_SQL.contains(contract_value)
                    || AVIONICS_REUSE_V2_POSTGRES_MIGRATION_SQL.contains(contract_value)
            );
        }
        for definition in [
            SQLITE_SCHEMA_SQL,
            POSTGRES_SCHEMA_SQL,
            AVIONICS_REUSE_V2_SQLITE_MIGRATION_SQL,
            AVIONICS_REUSE_V2_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(definition.contains("avionics_reuse_v2"));
        }
        assert!(!SQLITE_SCHEMA_SQL.contains("avionics_reuse_v1"));
        assert!(!POSTGRES_SCHEMA_SQL.contains("avionics_reuse_v1"));
        for repaired_object in [
            "DROP INDEX IF EXISTS idx_avionics_product_reuse_origin",
            "DROP TRIGGER IF EXISTS\n  avionics_product_reuse_attestations_validate_insert",
            "DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_insert",
            "DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_delete",
            "DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_type_update",
            "DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_capability_update",
            "DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_identity_update",
            "DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_origin_revocation",
            "DROP TRIGGER IF EXISTS listing_avionics_corroborations_validate_insert",
        ] {
            assert!(
                AVIONICS_REUSE_V2_SQLITE_MIGRATION_SQL.contains(repaired_object),
                "SQLite v2 repair migration is missing {repaired_object}"
            );
        }
        for repaired_object in [
            "DROP INDEX IF EXISTS idx_avionics_product_reuse_origin",
            "$drop_policy_constraints$",
            "ADD CONSTRAINT avionics_product_reuse_attestations_policy_version_check",
            "CREATE OR REPLACE FUNCTION validate_avionics_product_reuse_attestation()",
            "CREATE OR REPLACE FUNCTION preserve_avionics_product_reuse_attestation()",
            "CREATE OR REPLACE FUNCTION invalidate_avionics_product_reuse_for_type()",
            "CREATE OR REPLACE FUNCTION invalidate_avionics_product_reuse_for_capability()",
            "CREATE OR REPLACE FUNCTION invalidate_avionics_product_reuse_for_identity()",
            "CREATE OR REPLACE FUNCTION invalidate_avionics_product_reuse_for_revocation()",
            "DROP TRIGGER IF EXISTS avionics_product_reuse_invalidate_origin_revocation",
        ] {
            assert!(
                AVIONICS_REUSE_V2_POSTGRES_MIGRATION_SQL.contains(repaired_object),
                "Postgres v2 repair migration is missing {repaired_object}"
            );
        }
    }

    #[test]
    fn listing_avionics_authorization_contract_has_backend_parity() {
        let table = "aircraft_sale_listing_avionics_link_authorizations";
        let sqlite_columns = table_columns(SQLITE_SCHEMA_SQL, table);
        assert_eq!(
            sqlite_columns,
            table_columns(POSTGRES_SCHEMA_SQL, table),
            "SQLite/Postgres schema column mismatch for {table}"
        );
        assert_eq!(
            sqlite_columns,
            table_columns(
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL,
                table,
            ),
            "canonical schema and SQLite migration disagree for {table}"
        );
        assert_eq!(
            sqlite_columns,
            table_columns(
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
                table
            ),
            "canonical schema and Postgres migration disagree for {table}"
        );
        for definition in [
            SQLITE_SCHEMA_SQL,
            POSTGRES_SCHEMA_SQL,
            LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL,
            LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
        ] {
            for cleanup_trigger in [
                "listing_avionics_authorizations_invalidate_model_proof_update",
                "listing_avionics_authorizations_invalidate_model_type_insert",
                "listing_avionics_authorizations_invalidate_model_type_delete",
                "listing_avionics_authorizations_invalidate_model_type_update",
                "listing_avionics_authorizations_invalidate_type_update",
                "listing_avionics_authorizations_invalidate_graph_insert",
                "listing_avionics_authorizations_invalidate_graph_delete",
                "listing_avionics_authorizations_invalidate_graph_update",
                "listing_avionics_authorizations_invalidate_manufacturer_update",
                "listing_avionics_authorizations_invalidate_origin_revocation",
                "listing_avionics_authorizations_invalidate_capture_delete",
                "listing_avionics_authorizations_invalidate_capture_update",
            ] {
                assert!(definition.contains(cleanup_trigger));
            }
        }
    }

    #[test]
    fn listing_avionics_grounded_capability_contract_has_backend_parity() {
        let table = "aircraft_sale_listing_avionics_grounded_capabilities";
        let authorization_table = "aircraft_sale_listing_avionics_link_authorizations";
        let sqlite_columns = table_columns(SQLITE_SCHEMA_SQL, table);
        assert_eq!(
            sqlite_columns,
            table_columns(POSTGRES_SCHEMA_SQL, table),
            "SQLite/Postgres schema column mismatch for {table}"
        );
        assert_eq!(
            sqlite_columns,
            table_columns(
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL,
                table
            ),
            "canonical schema and SQLite upgrade disagree for {table}"
        );
        assert_eq!(
            sqlite_columns,
            table_columns(
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
                table
            ),
            "canonical schema and Postgres upgrade disagree for {table}"
        );
        assert_eq!(
            canonical_table_definition(SQLITE_SCHEMA_SQL, table),
            canonical_table_definition(
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL,
                table,
            ),
            "canonical SQLite schema and migration must agree on the complete table declaration"
        );
        assert_eq!(
            canonical_table_definition(POSTGRES_SCHEMA_SQL, table),
            canonical_table_definition(
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
                table,
            ),
            "canonical PostgreSQL schema and migration must agree on the complete table declaration"
        );
        assert_eq!(
            canonical_table_definition(SQLITE_SCHEMA_SQL, authorization_table),
            canonical_table_definition(
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL,
                authorization_table,
            ),
            "canonical SQLite schema and migration must agree on the exact authorization table"
        );
        assert_eq!(
            canonical_table_definition(POSTGRES_SCHEMA_SQL, authorization_table),
            canonical_table_definition(
                LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
                authorization_table,
            ),
            "canonical PostgreSQL schema and migration must agree on the exact authorization table"
        );
        for definition in [
            SQLITE_SCHEMA_SQL,
            POSTGRES_SCHEMA_SQL,
            LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL,
            LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(definition.contains(LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION));
            assert!(
                definition.contains(LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_FINGERPRINT)
            );
            assert!(definition.contains("listing_avionics_grounded_capability"));
            assert!(definition.contains("requested_quantity"));
            assert!(definition.contains("occurrence_index"));
            assert!(definition.contains("occurrence_role"));
        }
        for definition in [
            POSTGRES_SCHEMA_SQL,
            LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(definition.contains(
                "CREATE OR REPLACE FUNCTION public.validate_listing_avionics_grounded_capability()"
            ));
            assert!(definition.contains(
                "CREATE OR REPLACE FUNCTION public.reject_listing_avionics_grounded_capability_update()"
            ));
            assert!(definition.contains("FROM public.plugin_submissions submission"));
            assert!(definition
                .contains("FROM public.avionics_approved_product_graph_identities approved"));
            for function_name in [
                "public.validate_listing_avionics_grounded_capability",
                "public.reject_listing_avionics_grounded_capability_update",
            ] {
                assert!(definition.contains(&format!(
                    "CREATE OR REPLACE FUNCTION {function_name}()\nRETURNS TRIGGER\nLANGUAGE plpgsql\nSET search_path = pg_catalog"
                )));
            }
            assert!(definition.contains(
                "EXECUTE FUNCTION public.validate_listing_avionics_grounded_capability()"
            ));
            assert!(definition.contains(
                "EXECUTE FUNCTION public.reject_listing_avionics_grounded_capability_update()"
            ));
        }
        for object_name in [
            "idx_listing_avionics_grounded_capabilities_model",
            "idx_listing_avionics_grounded_capabilities_submission",
            "listing_avionics_grounded_capabilities_validate_insert",
            "listing_avionics_grounded_capabilities_immutable_update",
        ] {
            assert_eq!(
                canonical_sqlite_named_definition(SQLITE_SCHEMA_SQL, object_name),
                canonical_sqlite_named_definition(
                    LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL,
                    object_name,
                ),
                "canonical SQLite object and migration drifted for {object_name}"
            );
        }
        for trigger_name in [
            "listing_avionics_grounded_capabilities_validate_insert",
            "listing_avionics_grounded_capabilities_immutable_update",
        ] {
            assert_eq!(
                canonical_postgres_named_trigger_definition(POSTGRES_SCHEMA_SQL, trigger_name),
                canonical_postgres_named_trigger_definition(
                    LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
                    trigger_name,
                ),
                "canonical PostgreSQL trigger and migration drifted for {trigger_name}"
            );
        }
        for function_name in [
            "validate_listing_avionics_grounded_capability",
            "reject_listing_avionics_grounded_capability_update",
        ] {
            assert_eq!(
                postgres_function_source(POSTGRES_SCHEMA_SQL, function_name)
                    .map(canonical_sql_definition),
                postgres_function_source(
                    LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
                    function_name,
                )
                .map(canonical_sql_definition),
                "canonical PostgreSQL function and migration drifted for {function_name}"
            );
        }
        for object_name in [
            "idx_listing_avionics_authorizations_model",
            "listing_avionics_authorizations_immutable_update",
            "listing_avionics_authorizations_invalidate_capture_delete",
            "listing_avionics_authorizations_invalidate_capture_update",
            "listing_avionics_authorizations_invalidate_graph_delete",
            "listing_avionics_authorizations_invalidate_graph_insert",
            "listing_avionics_authorizations_invalidate_graph_update",
            "listing_avionics_authorizations_invalidate_link_update",
            "listing_avionics_authorizations_invalidate_manufacturer_update",
            "listing_avionics_authorizations_invalidate_model_proof_update",
            "listing_avionics_authorizations_invalidate_model_type_delete",
            "listing_avionics_authorizations_invalidate_model_type_insert",
            "listing_avionics_authorizations_invalidate_model_type_update",
            "listing_avionics_authorizations_invalidate_origin_revocation",
            "listing_avionics_authorizations_invalidate_reuse_delete",
            "listing_avionics_authorizations_invalidate_type_update",
            "listing_avionics_authorizations_validate_insert",
        ] {
            assert_eq!(
                canonical_sqlite_named_definition(SQLITE_SCHEMA_SQL, object_name),
                canonical_sqlite_named_definition(
                    LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL,
                    object_name,
                ),
                "canonical SQLite authorization object and migration drifted for {object_name}"
            );
        }
        for trigger_name in [
            "listing_avionics_authorizations_immutable_update",
            "listing_avionics_authorizations_invalidate_capture_delete",
            "listing_avionics_authorizations_invalidate_capture_update",
            "listing_avionics_authorizations_invalidate_graph_delete",
            "listing_avionics_authorizations_invalidate_graph_insert",
            "listing_avionics_authorizations_invalidate_graph_update",
            "listing_avionics_authorizations_invalidate_link_update",
            "listing_avionics_authorizations_invalidate_manufacturer_update",
            "listing_avionics_authorizations_invalidate_model_proof_update",
            "listing_avionics_authorizations_invalidate_model_type_delete",
            "listing_avionics_authorizations_invalidate_model_type_insert",
            "listing_avionics_authorizations_invalidate_model_type_update",
            "listing_avionics_authorizations_invalidate_origin_revocation",
            "listing_avionics_authorizations_invalidate_reuse_delete",
            "listing_avionics_authorizations_invalidate_type_update",
            "listing_avionics_authorizations_validate_insert",
        ] {
            assert_eq!(
                canonical_postgres_named_trigger_definition(POSTGRES_SCHEMA_SQL, trigger_name),
                canonical_postgres_named_trigger_definition(
                    LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
                    trigger_name,
                ),
                "canonical PostgreSQL authorization trigger and migration drifted for {trigger_name}"
            );
        }
        for function_name in [
            "validate_listing_avionics_authorization",
            "preserve_listing_avionics_authorization",
            "invalidate_listing_avionics_authorization_for_link",
            "invalidate_listing_avionics_authorization_for_reuse",
            "invalidate_listing_avionics_same_case_for_model_proof",
            "invalidate_listing_avionics_same_case_for_model_type",
            "invalidate_listing_avionics_same_case_for_type",
            "invalidate_listing_avionics_same_case_for_graph",
            "invalidate_listing_avionics_same_case_for_manufacturer",
            "invalidate_listing_avionics_same_case_for_origin_revocation",
            "invalidate_listing_avionics_authorization_for_capture",
        ] {
            assert_eq!(
                postgres_function_source(POSTGRES_SCHEMA_SQL, function_name)
                    .map(canonical_sql_definition),
                postgres_function_source(
                    LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL,
                    function_name,
                )
                .map(canonical_sql_definition),
                "canonical PostgreSQL authorization function and migration drifted for {function_name}"
            );
        }
        assert_eq!(LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_VERSION, 1);
        assert_eq!(
            LISTING_AVIONICS_GROUNDED_CAPABILITIES_CONTRACT_FINGERPRINT,
            "682ca4e44ced30b0d14da879c31e0fa4b24cc1b6fceb9f213ecc39d9abca0338"
        );
        assert!(!LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL
            .contains("DROP TABLE IF EXISTS aircraft_sale_listing_avionics_grounded_capabilities"));
        assert!(!LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL.contains(
            "DROP TABLE IF EXISTS\n  public.aircraft_sale_listing_avionics_grounded_capabilities"
        ));
    }

    #[tokio::test]
    async fn grounded_capability_migration_rerun_is_idempotent_and_unmarked_tables_fail_closed() {
        let options = "sqlite::memory:"
            .parse::<sqlx::sqlite::SqliteConnectOptions>()
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let db = AppDb {
            backend: DatabaseBackend::Sqlite(pool),
        };
        db.initialize_transactionally().await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };

        for _ in 0..2 {
            sqlx::raw_sql(LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL)
                .execute(pool)
                .await
                .expect("the exact installed grounded-capability migration should be idempotent");
        }
        db.ensure_required_migrations()
            .await
            .expect("startup should pass after exact migration reruns");

        sqlx::query("DELETE FROM schema_migration_contracts WHERE migration_name = ?")
            .bind(LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION)
            .execute(pool)
            .await
            .unwrap();
        let error = sqlx::raw_sql(LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL)
            .execute(pool)
            .await
            .expect_err("unmarked preexisting final tables must fail closed")
            .to_string();
        assert!(
            error.contains("CHECK constraint failed")
                || error.contains("listing_avionics_grounded_capabilities_migration_guard"),
            "unexpected startup error: {error}"
        );
        sqlx::raw_sql("ROLLBACK").execute(pool).await.unwrap();
        sqlx::raw_sql(
            "DROP TABLE aircraft_sale_listing_avionics_grounded_capabilities; \
             DROP TABLE aircraft_sale_listing_avionics_link_authorizations;",
        )
        .execute(pool)
        .await
        .unwrap();
        let protected_object_error =
            sqlx::raw_sql(LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL)
                .execute(pool)
                .await
                .expect_err("unmarked protected trigger names must fail closed")
                .to_string();
        sqlx::raw_sql("ROLLBACK").execute(pool).await.unwrap();
        assert!(
            protected_object_error.contains("CHECK constraint failed")
                || protected_object_error
                    .contains("listing_avionics_grounded_capabilities_migration_guard"),
            "unexpected protected-object error: {protected_object_error}"
        );
    }

    async fn sqlite_grounded_authorization_rows(pool: &sqlx::SqlitePool) -> (String, String) {
        let capabilities = sqlx::query_scalar::<_, String>(
            r#"
            SELECT group_concat(row_value, char(10))
            FROM (
              SELECT
                quote(listing_id) || '|' || quote(plugin_submission_id) || '|' ||
                quote(occurrence_index) || '|' || quote(occurrence_role) || '|' ||
                quote(avionics_model_id) || '|' || quote(requested_quantity) || '|' ||
                quote(configuration_action) || '|' || quote(request_sha256) || '|' ||
                quote(capability_sha256) || '|' || quote(grounded_resolution_sha256) || '|' ||
                quote(evidence_capture_sha256) || '|' || quote(extracted_listing_sha256) || '|' ||
                quote(product_fingerprint) || '|' || quote(collision_closure_sha256) || '|' ||
                quote(source_revocation_count) || '|' || quote(policy_version) || '|' ||
                quote(created_at) AS row_value
              FROM aircraft_sale_listing_avionics_grounded_capabilities
              ORDER BY listing_id, plugin_submission_id, occurrence_index, occurrence_role
            )
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let authorizations = sqlx::query_scalar::<_, String>(
            r#"
            SELECT group_concat(row_value, char(10))
            FROM (
              SELECT
                quote(listing_link_id) || '|' || quote(association_role) || '|' ||
                quote(avionics_model_id) || '|' || quote(authorization_kind) || '|' ||
                quote(observation_sha256) || '|' || quote(product_fingerprint) || '|' ||
                quote(grounded_resolution_sha256) || '|' || quote(evidence_capture_sha256) || '|' ||
                quote(plugin_submission_id) || '|' || quote(extracted_listing_sha256) || '|' ||
                quote(collision_closure_sha256) || '|' || quote(source_revocation_count) || '|' ||
                quote(policy_version) || '|' || quote(authorized_at) AS row_value
              FROM aircraft_sale_listing_avionics_link_authorizations
              ORDER BY listing_link_id, association_role
            )
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        (capabilities, authorizations)
    }

    async fn postgres_grounded_authorization_rows(pool: &sqlx::PgPool) -> (String, String) {
        let capabilities = sqlx::query_scalar::<_, String>(
            r#"
            SELECT COALESCE(
              jsonb_agg(to_jsonb(row_value) ORDER BY
                row_value.listing_id, row_value.plugin_submission_id,
                row_value.occurrence_index, row_value.occurrence_role
              ),
              '[]'::jsonb
            )::text
            FROM ONLY public.aircraft_sale_listing_avionics_grounded_capabilities
              AS row_value
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let authorizations = sqlx::query_scalar::<_, String>(
            r#"
            SELECT COALESCE(
              jsonb_agg(to_jsonb(row_value) ORDER BY
                row_value.listing_link_id, row_value.association_role
              ),
              '[]'::jsonb
            )::text
            FROM ONLY public.aircraft_sale_listing_avionics_link_authorizations
              AS row_value
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        (capabilities, authorizations)
    }

    #[tokio::test]
    async fn sqlite_grounded_capability_migration_rerun_preserves_nonempty_rows_exactly() {
        let options = "sqlite::memory:"
            .parse::<sqlx::sqlite::SqliteConnectOptions>()
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let db = AppDb {
            backend: DatabaseBackend::Sqlite(pool),
        };
        db.initialize_transactionally().await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let listing_link_insert_trigger: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' \
             AND name = 'aircraft_sale_listing_avionics_approved_insert'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let capability_insert_trigger: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' \
             AND name = 'listing_avionics_grounded_capabilities_validate_insert'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let authorization_insert_trigger: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' \
             AND name = 'listing_avionics_authorizations_validate_insert'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"
            DROP TRIGGER aircraft_sale_listing_avionics_approved_insert;
            DROP TRIGGER listing_avionics_grounded_capabilities_validate_insert;
            DROP TRIGGER listing_avionics_authorizations_validate_insert;

            INSERT INTO avionics_manufacturers (id, name, normalized_name)
            VALUES (91001, 'Rerun Test Avionics', 'rerun test avionics');
            INSERT INTO avionics_models (
              id, avionics_manufacturer_id, name, normalized_name
            ) VALUES (91002, 91001, 'Exact Unit', 'exact unit');

            INSERT INTO avionics_manufacturer_identities (
              id, canonical_name, normalized_identity_key,
              identity_evidence_kind, identity_source_url,
              identity_source_title, identity_evidence_text, identity_confidence
            ) VALUES (
              91003, 'Rerun Test Avionics', 'reruntestavionics',
              'authoritative_reference', 'https://rerun.example/catalog',
              'Rerun manufacturer catalog',
              'The manufacturer catalog establishes the test identity.',
              'very_high'
            );
            INSERT INTO avionics_authoritative_source_origins (
              id, authority_kind, avionics_manufacturer_identity_id,
              https_origin, evidence_source_url, evidence_source_title,
              evidence_text, approval_basis, approval_reason
            ) VALUES (
              91004, 'manufacturer_primary', 91003,
              'https://rerun.example', 'https://rerun.example/catalog',
              'Rerun manufacturer catalog',
              'The primary catalog identifies the exact test product.',
              'curated_bootstrap', 'Exact migration rerun regression fixture'
            );
            INSERT INTO avionics_authoritative_source_origin_revocations (
              avionics_authoritative_source_origin_id, revoked_by_user_id,
              reason, revoked_at
            ) SELECT 91004, id, 'Regression fixture revocation epoch',
                     '2026-08-25 09:00:00'
              FROM users WHERE email = 'developer@localhost';

            INSERT INTO aircraft_sale_listings (
              id, aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            )
            SELECT 91005, -1, id, 'https://rerun.example/listing/one',
                   2020, 250000, 500
            FROM users WHERE email = 'developer@localhost';
            INSERT INTO aircraft_sale_listings (
              id, aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            )
            SELECT 91006, -1, id, 'https://rerun.example/listing/two',
                   2021, 260000, 450
            FROM users WHERE email = 'developer@localhost';
            INSERT INTO plugin_installs (id, user_id, public_key_base64)
            SELECT 91007, id, 'rerun-test-key'
            FROM users WHERE email = 'developer@localhost';
            INSERT INTO plugin_submissions (
              id, user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              extraction_error, canonical_listing_id, submitted_at
            )
            SELECT 91008, id, 91007, 'https://rerun.example/listing/one',
                   '<p>Exact Unit</p>',
                   'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                   'rerun-signature-one', '{"avionics":[]}', NULL, 91005,
                   '2026-08-25 09:01:00'
            FROM users WHERE email = 'developer@localhost';
            INSERT INTO plugin_submissions (
              id, user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              extraction_error, canonical_listing_id, submitted_at
            )
            SELECT 91009, id, 91007, 'https://rerun.example/listing/two',
                   '<p>Exact Unit</p>',
                   'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
                   'rerun-signature-two', '{"avionics":[]}', NULL, 91006,
                   '2026-08-25 09:02:00'
            FROM users WHERE email = 'developer@localhost';
            INSERT INTO aircraft_sale_listing_avionics (
              id, aircraft_sale_listing_id, avionics_model_id, quantity,
              source, source_notes, configuration_action, source_confidence,
              created_at, updated_at
            ) VALUES
              (91010, 91005, 91002, 1, 'listing', 'Exact Unit', 'installed',
               'high', '2026-08-25 09:03:00', '2026-08-25 09:03:00'),
              (91011, 91006, 91002, 1, 'listing', 'Exact Unit', 'installed',
               'high', '2026-08-25 09:04:00', '2026-08-25 09:04:00');

            INSERT INTO aircraft_sale_listing_avionics_grounded_capabilities (
              listing_id, plugin_submission_id, occurrence_index,
              occurrence_role, avionics_model_id, requested_quantity,
              configuration_action, request_sha256, capability_sha256,
              grounded_resolution_sha256, evidence_capture_sha256,
              extracted_listing_sha256, product_fingerprint,
              collision_closure_sha256, source_revocation_count,
              policy_version, created_at
            ) VALUES (
              91005, 91008, 3, 'primary', 91002, 2, 'installed',
              '1111111111111111111111111111111111111111111111111111111111111111',
              '2222222222222222222222222222222222222222222222222222222222222222',
              '3333333333333333333333333333333333333333333333333333333333333333',
              'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
              '4444444444444444444444444444444444444444444444444444444444444444',
              '5555555555555555555555555555555555555555555555555555555555555555',
              '6666666666666666666666666666666666666666666666666666666666666666',
              1, 'listing_avionics_grounded_capability',
              '2026-08-25 09:05:00'
            );
            INSERT INTO aircraft_sale_listing_avionics_link_authorizations (
              listing_link_id, association_role, avionics_model_id,
              authorization_kind, observation_sha256, product_fingerprint,
              grounded_resolution_sha256, evidence_capture_sha256,
              plugin_submission_id, extracted_listing_sha256,
              collision_closure_sha256, source_revocation_count,
              policy_version, authorized_at
            ) VALUES
              (91010, 'installed', 91002, 'same_case_grounded',
               '7777777777777777777777777777777777777777777777777777777777777777',
               '5555555555555555555555555555555555555555555555555555555555555555',
               '3333333333333333333333333333333333333333333333333333333333333333',
               'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
               91008,
               '4444444444444444444444444444444444444444444444444444444444444444',
               '6666666666666666666666666666666666666666666666666666666666666666',
               1, 'listing_avionics_authorization',
               '2026-08-25 09:06:00'),
              (91011, 'installed', 91002, 'manufacturer_reuse',
               '8888888888888888888888888888888888888888888888888888888888888888',
               '9999999999999999999999999999999999999999999999999999999999999999',
               NULL,
               'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
               NULL, NULL,
               'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
               NULL, 'listing_avionics_authorization',
               '2026-08-25 09:07:00');
            "#,
        )
        .execute(pool)
        .await
        .expect("nonempty exact migration fixture should seed");
        sqlx::raw_sql(&listing_link_insert_trigger)
            .execute(pool)
            .await
            .unwrap();
        sqlx::raw_sql(&capability_insert_trigger)
            .execute(pool)
            .await
            .unwrap();
        sqlx::raw_sql(&authorization_insert_trigger)
            .execute(pool)
            .await
            .unwrap();

        let before = sqlite_grounded_authorization_rows(pool).await;
        let installed_at_before: String = sqlx::query_scalar(
            "SELECT installed_at FROM schema_migration_contracts WHERE migration_name = ?",
        )
        .bind(LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();

        sqlx::raw_sql(LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL)
            .execute(pool)
            .await
            .expect("the exact nonempty migration should rerun without data loss");

        assert_eq!(sqlite_grounded_authorization_rows(pool).await, before);
        let installed_at_after: String = sqlx::query_scalar(
            "SELECT installed_at FROM schema_migration_contracts WHERE migration_name = ?",
        )
        .bind(LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(installed_at_after, installed_at_before);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_link_authorizations",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pragma_foreign_key_check")
                .fetch_one(pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
                .fetch_one(pool)
                .await
                .unwrap(),
            "ok"
        );
        db.ensure_required_migrations()
            .await
            .expect("startup attestation should accept the preserved exact state");

        let canonical_capability_table: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' \
             AND name = 'aircraft_sale_listing_avionics_grounded_capabilities'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let hostile_capability_table = canonical_capability_table.replacen(
            "CHECK (occurrence_index >= 0)",
            "CHECK (occurrence_index != 0)",
            1,
        );
        assert_ne!(hostile_capability_table, canonical_capability_table);
        let mut catalog_connection = pool.acquire().await.unwrap();
        sqlx::raw_sql("PRAGMA writable_schema = ON")
            .execute(&mut *catalog_connection)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE sqlite_schema SET sql = ? WHERE type = 'table' \
             AND name = 'aircraft_sale_listing_avionics_grounded_capabilities'",
        )
        .bind(&hostile_capability_table)
        .execute(&mut *catalog_connection)
        .await
        .unwrap();
        sqlx::raw_sql("PRAGMA writable_schema = OFF")
            .execute(&mut *catalog_connection)
            .await
            .unwrap();
        drop(catalog_connection);
        let table_error =
            sqlx::raw_sql(LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL)
                .execute(pool)
                .await
                .expect_err("a marked same-column table mutation must fail before migration DDL")
                .to_string();
        sqlx::raw_sql("ROLLBACK").execute(pool).await.unwrap();
        assert!(table_error.contains("accepted = 1"), "{table_error}");
        assert_eq!(sqlite_grounded_authorization_rows(pool).await, before);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT installed_at FROM schema_migration_contracts WHERE migration_name = ?",
            )
            .bind(LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION)
            .fetch_one(pool)
            .await
            .unwrap(),
            installed_at_before
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' \
                 AND name = 'aircraft_sale_listing_avionics_grounded_capabilities'",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            hostile_capability_table
        );

        let mut catalog_connection = pool.acquire().await.unwrap();
        sqlx::raw_sql("PRAGMA writable_schema = ON")
            .execute(&mut *catalog_connection)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE sqlite_schema SET sql = ? WHERE type = 'table' \
             AND name = 'aircraft_sale_listing_avionics_grounded_capabilities'",
        )
        .bind(&canonical_capability_table)
        .execute(&mut *catalog_connection)
        .await
        .unwrap();
        sqlx::raw_sql("PRAGMA writable_schema = OFF")
            .execute(&mut *catalog_connection)
            .await
            .unwrap();
        drop(catalog_connection);
        let canonical_authorization_trigger: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' \
             AND name = 'listing_avionics_authorizations_validate_insert'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let hostile_authorization_trigger = canonical_authorization_trigger.replacen(
            "link.source_confidence = 'high'",
            "link.source_confidence = 'HIGH'",
            1,
        );
        assert_ne!(
            hostile_authorization_trigger,
            canonical_authorization_trigger
        );
        sqlx::query("DROP TRIGGER listing_avionics_authorizations_validate_insert")
            .execute(pool)
            .await
            .unwrap();
        sqlx::raw_sql(&hostile_authorization_trigger)
            .execute(pool)
            .await
            .unwrap();
        let trigger_error =
            sqlx::raw_sql(LISTING_AVIONICS_GROUNDED_CAPABILITIES_SQLITE_MIGRATION_SQL)
                .execute(pool)
                .await
                .expect_err("a marked trigger mutation must roll back instead of being healed")
                .to_string();
        sqlx::raw_sql("ROLLBACK").execute(pool).await.unwrap();
        assert!(trigger_error.contains("accepted = 3"), "{trigger_error}");
        assert_eq!(sqlite_grounded_authorization_rows(pool).await, before);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT installed_at FROM schema_migration_contracts WHERE migration_name = ?",
            )
            .bind(LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION)
            .fetch_one(pool)
            .await
            .unwrap(),
            installed_at_before
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT sql FROM sqlite_schema WHERE type = 'trigger' \
                 AND name = 'listing_avionics_authorizations_validate_insert'",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            hostile_authorization_trigger
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_grounded_capability_migration_rerun_preserves_nonempty_rows_exactly() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;

        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::raw_sql(
            r#"
            INSERT INTO public.avionics_manufacturers (id, name, normalized_name)
            VALUES (92001, 'Rerun Test Avionics', 'rerun test avionics');
            INSERT INTO public.avionics_manufacturer_identities (
              id, canonical_name, normalized_identity_key,
              identity_evidence_kind, identity_source_url,
              identity_source_title, identity_evidence_text, identity_confidence
            ) VALUES (
              92002, 'Rerun Test Avionics', 'reruntestavionics',
              'authoritative_reference', 'https://rerun.example/catalog',
              'Rerun manufacturer catalog',
              'The manufacturer catalog establishes the test identity.',
              'very_high'
            );
            INSERT INTO public.avionics_manufacturer_identity_memberships (
              avionics_manufacturer_id, avionics_manufacturer_identity_id,
              membership_basis, normalized_name_key, evidence_source_url,
              evidence_source_title, evidence_text, evidence_confidence
            ) VALUES (
              92001, 92002, 'authoritative_primary', 'reruntestavionics',
              'https://rerun.example/catalog', 'Rerun manufacturer catalog',
              'The manufacturer catalog establishes the exact manufacturer.',
              'very_high'
            );
            INSERT INTO public.avionics_authoritative_source_origins (
              id, authority_kind, avionics_manufacturer_identity_id,
              https_origin, evidence_source_url, evidence_source_title,
              evidence_text, approval_basis, approval_reason
            ) VALUES (
              92003, 'manufacturer_primary', 92002,
              'https://rerun.example', 'https://rerun.example/catalog',
              'Rerun manufacturer catalog',
              'The primary catalog identifies the exact test product.',
              'curated_bootstrap', 'Exact migration rerun regression fixture'
            );
            INSERT INTO public.avionics_types (id, name, normalized_name)
            VALUES (92004, 'Rerun Test Type', 'rerun test type');
            INSERT INTO public.avionics_models (
              id, avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_evidence_kind, identity_confidence
            ) VALUES (
              92005, 92001, 'Exact Unit', 'exact unit',
              'manufacturer_model_number', 'EXACT-UNIT', 'exact-unit',
              'https://rerun.example/catalog', 'Rerun manufacturer catalog',
              'The primary catalog identifies the exact unit model.',
              'authoritative_reference', 'very_high'
            );
            INSERT INTO public.avionics_model_types (
              avionics_model_id, avionics_type_id
            ) VALUES (92005, 92004);
            UPDATE public.avionics_models
            SET catalog_status = 'approved',
                catalog_reviewed_at = '2026-08-25 09:00:00'
            WHERE id = 92005;
            INSERT INTO public.avionics_product_reuse_attestations (
              avionics_model_id, avionics_authoritative_source_origin_id,
              policy_version, product_fingerprint, attested_at
            ) VALUES (
              92005, 92003, 'avionics_reuse_v2',
              '9999999999999999999999999999999999999999999999999999999999999999',
              '2026-08-25 09:01:00'
            );

            INSERT INTO public.aircraft_sale_listings (
              id, aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            )
            SELECT 92006, -1, id, 'https://rerun.example/listing/one',
                   2020, 250000, 500
            FROM public.users WHERE email = 'developer@localhost';
            INSERT INTO public.aircraft_sale_listings (
              id, aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            )
            SELECT 92007, -1, id, 'https://rerun.example/listing/two',
                   2021, 260000, 450
            FROM public.users WHERE email = 'developer@localhost';
            INSERT INTO public.plugin_installs (id, user_id, public_key_base64)
            SELECT 92008, id, 'rerun-test-key'
            FROM public.users WHERE email = 'developer@localhost';
            INSERT INTO public.plugin_submissions (
              id, user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              extraction_error, canonical_listing_id, submitted_at
            )
            SELECT 92009, id, 92008, 'https://rerun.example/listing/one',
                   '<p>Exact Unit</p>',
                   'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                   'rerun-signature-one', '{"avionics":[]}', NULL, 92006,
                   '2026-08-25 09:02:00'
            FROM public.users WHERE email = 'developer@localhost';
            INSERT INTO public.plugin_submissions (
              id, user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              extraction_error, canonical_listing_id, submitted_at
            )
            SELECT 92010, id, 92008, 'https://rerun.example/listing/two',
                   '<p>Exact Unit</p>',
                   'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
                   'rerun-signature-two', '{"avionics":[]}', NULL, 92007,
                   '2026-08-25 09:03:00'
            FROM public.users WHERE email = 'developer@localhost';
            INSERT INTO public.aircraft_sale_listing_avionics (
              id, aircraft_sale_listing_id, avionics_model_id, quantity,
              source, source_notes, configuration_action, source_confidence,
              created_at, updated_at
            ) VALUES
              (92011, 92006, 92005, 1, 'listing', 'Exact Unit', 'installed',
               'high', '2026-08-25 09:04:00', '2026-08-25 09:04:00'),
              (92012, 92007, 92005, 1, 'listing', 'Exact Unit', 'installed',
               'high', '2026-08-25 09:05:00', '2026-08-25 09:05:00');

            INSERT INTO public.aircraft_sale_listing_avionics_grounded_capabilities (
              listing_id, plugin_submission_id, occurrence_index,
              occurrence_role, avionics_model_id, requested_quantity,
              configuration_action, request_sha256, capability_sha256,
              grounded_resolution_sha256, evidence_capture_sha256,
              extracted_listing_sha256, product_fingerprint,
              collision_closure_sha256, source_revocation_count,
              policy_version, created_at
            ) VALUES (
              92006, 92009, 3, 'primary', 92005, 2, 'installed',
              '1111111111111111111111111111111111111111111111111111111111111111',
              '2222222222222222222222222222222222222222222222222222222222222222',
              '3333333333333333333333333333333333333333333333333333333333333333',
              'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
              '4444444444444444444444444444444444444444444444444444444444444444',
              '5555555555555555555555555555555555555555555555555555555555555555',
              '6666666666666666666666666666666666666666666666666666666666666666',
              0, 'listing_avionics_grounded_capability',
              '2026-08-25 09:06:00'
            );
            INSERT INTO public.aircraft_sale_listing_avionics_link_authorizations (
              listing_link_id, association_role, avionics_model_id,
              authorization_kind, observation_sha256, product_fingerprint,
              grounded_resolution_sha256, evidence_capture_sha256,
              plugin_submission_id, extracted_listing_sha256,
              collision_closure_sha256, source_revocation_count,
              policy_version, authorized_at
            ) VALUES
              (92011, 'installed', 92005, 'same_case_grounded',
               '7777777777777777777777777777777777777777777777777777777777777777',
               '5555555555555555555555555555555555555555555555555555555555555555',
               '3333333333333333333333333333333333333333333333333333333333333333',
               'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
               92009,
               '4444444444444444444444444444444444444444444444444444444444444444',
               '6666666666666666666666666666666666666666666666666666666666666666',
               0, 'listing_avionics_authorization',
               '2026-08-25 09:07:00'),
              (92012, 'installed', 92005, 'manufacturer_reuse',
               '8888888888888888888888888888888888888888888888888888888888888888',
               '9999999999999999999999999999999999999999999999999999999999999999',
               NULL,
               'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
               NULL, NULL,
               'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
               NULL, 'listing_avionics_authorization',
               '2026-08-25 09:08:00');
            "#,
        )
        .execute(pool)
        .await
        .expect("nonempty exact PostgreSQL migration fixture should seed");

        db.ensure_required_migrations()
            .await
            .expect("the exact pre-rerun PostgreSQL state should pass startup");
        let before = postgres_grounded_authorization_rows(pool).await;
        let receipt_before: (i64, String, String) = sqlx::query_as(
            "SELECT contract_version::bigint, contract_fingerprint, installed_at \
             FROM ONLY public.schema_migration_contracts WHERE migration_name = $1",
        )
        .bind(LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION)
        .fetch_one(pool)
        .await
        .unwrap();

        for rerun in 1..=2 {
            sqlx::raw_sql(LISTING_AVIONICS_GROUNDED_CAPABILITIES_POSTGRES_MIGRATION_SQL)
                .execute(pool)
                .await
                .unwrap_or_else(|error| panic!("exact PostgreSQL rerun {rerun} failed: {error}"));
            assert_eq!(postgres_grounded_authorization_rows(pool).await, before);
            let receipt_after: (i64, String, String) = sqlx::query_as(
                "SELECT contract_version::bigint, contract_fingerprint, installed_at \
                 FROM ONLY public.schema_migration_contracts WHERE migration_name = $1",
            )
            .bind(LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(receipt_after, receipt_before);
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM ONLY public.schema_migration_contracts \
                     WHERE migration_name = $1",
                )
                .bind(LISTING_AVIONICS_GROUNDED_CAPABILITIES_MIGRATION)
                .fetch_one(pool)
                .await
                .unwrap(),
                1
            );
        }

        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                r#"
                SELECT COUNT(*)::bigint,
                       COUNT(*) FILTER (WHERE NOT constraint_row.convalidated)::bigint
                FROM pg_catalog.pg_constraint constraint_row
                WHERE constraint_row.contype = 'f'
                  AND constraint_row.conrelid IN (
                    pg_catalog.to_regclass(
                      'public.aircraft_sale_listing_avionics_grounded_capabilities'
                    ),
                    pg_catalog.to_regclass(
                      'public.aircraft_sale_listing_avionics_link_authorizations'
                    )
                  )
                "#,
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            (6, 0)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT COUNT(*)::bigint
                FROM (
                  SELECT capability.listing_id
                  FROM ONLY public.aircraft_sale_listing_avionics_grounded_capabilities AS capability
                  LEFT JOIN public.aircraft_sale_listings listing
                    ON listing.id = capability.listing_id
                  LEFT JOIN public.plugin_submissions submission
                    ON submission.id = capability.plugin_submission_id
                  LEFT JOIN public.avionics_models model
                    ON model.id = capability.avionics_model_id
                  WHERE listing.id IS NULL OR submission.id IS NULL OR model.id IS NULL
                  UNION ALL
                  SELECT authorization_row.listing_link_id
                  FROM ONLY public.aircraft_sale_listing_avionics_link_authorizations AS authorization_row
                  LEFT JOIN public.aircraft_sale_listing_avionics link
                    ON link.id = authorization_row.listing_link_id
                  LEFT JOIN public.avionics_models model
                    ON model.id = authorization_row.avionics_model_id
                  LEFT JOIN public.plugin_submissions submission
                    ON submission.id = authorization_row.plugin_submission_id
                  WHERE link.id IS NULL OR model.id IS NULL
                    OR (authorization_row.plugin_submission_id IS NOT NULL
                        AND submission.id IS NULL)
                ) orphan
                "#,
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            0
        );
        db.ensure_required_migrations()
            .await
            .expect("startup should accept the preserved exact PostgreSQL state");
    }

    async fn assert_corrupt_grounded_capability_schema_rejected(statements: &[&str]) {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        for statement in statements {
            pool.execute(*statement).await.unwrap();
        }
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("tampered grounded-capability objects must fail startup")
            .to_string();
        assert!(error.contains("capture-bound grounded avionics resolutions"));
        assert!(error.contains("20260825_listing_avionics_grounded_capabilities.sqlite.sql"));
    }

    #[tokio::test]
    async fn grounded_capability_startup_rejects_wrong_index_and_noop_trigger() {
        assert_corrupt_grounded_capability_schema_rejected(&[
            "DROP INDEX idx_listing_avionics_grounded_capabilities_model",
            "CREATE INDEX idx_listing_avionics_grounded_capabilities_model ON users(email)",
        ])
        .await;
        assert_corrupt_grounded_capability_schema_rejected(&[
            "DROP TRIGGER listing_avionics_grounded_capabilities_validate_insert",
            "CREATE TRIGGER listing_avionics_grounded_capabilities_validate_insert BEFORE INSERT ON aircraft_sale_listing_avionics_grounded_capabilities WHEN 0 BEGIN SELECT RAISE(ABORT, 'grounded avionics capability requires its exact current capture-bound listing and approved product'); END",
        ])
        .await;
        assert_corrupt_grounded_capability_schema_rejected(&[
            "CREATE TRIGGER unexpected_listing_avionics_grounded_capability_trigger BEFORE DELETE ON aircraft_sale_listing_avionics_grounded_capabilities BEGIN SELECT RAISE(ABORT, 'unexpected trigger'); END",
        ])
        .await;
        assert_corrupt_grounded_capability_schema_rejected(&[
            "DROP TRIGGER listing_avionics_grounded_capabilities_validate_insert",
            "CREATE TRIGGER listing_avionics_grounded_capabilities_validate_insert BEFORE INSERT ON aircraft_sale_listing_avionics_grounded_capabilities BEGIN SELECT 1; END",
        ])
        .await;
    }

    #[tokio::test]
    async fn grounded_capability_startup_rejects_weakened_table_contract() {
        for (expected, replacement) in [
            (
                "CHECK (occurrence_index >= 0)",
                "CHECK (occurrence_index >= -1)",
            ),
            (
                "CHECK (occurrence_index >= 0)",
                "CHECK (TRUE /* occurrence_index >= 0 */ OR TRUE)",
            ),
            (
                "created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP",
                "created_at TEXT NOT NULL DEFAULT 'untrusted'",
            ),
            (
                "REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE",
                "REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT",
            ),
        ] {
            let db = AppDb::connect("sqlite::memory:").await.unwrap();
            let DatabaseBackend::Sqlite(pool) = db.backend() else {
                unreachable!()
            };
            let table_sql: String = sqlx::query_scalar(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?",
            )
            .bind("aircraft_sale_listing_avionics_grounded_capabilities")
            .fetch_one(pool)
            .await
            .unwrap();
            let object_sql: Vec<String> = sqlx::query_scalar(
                "SELECT sql FROM sqlite_schema WHERE tbl_name = ? AND type IN ('index', 'trigger') AND sql IS NOT NULL ORDER BY type, name",
            )
            .bind("aircraft_sale_listing_avionics_grounded_capabilities")
            .fetch_all(pool)
            .await
            .unwrap();
            assert!(table_sql.contains(expected));
            pool.execute("DROP TABLE aircraft_sale_listing_avionics_grounded_capabilities")
                .await
                .unwrap();
            pool.execute(table_sql.replace(expected, replacement).as_str())
                .await
                .unwrap();
            for statement in object_sql {
                pool.execute(statement.as_str()).await.unwrap();
            }
            let error = db
                .ensure_required_migrations()
                .await
                .expect_err("a weakened grounded-capability table must fail startup")
                .to_string();
            assert!(error.contains("capture-bound grounded avionics resolutions"));
            assert!(error.contains("20260825_listing_avionics_grounded_capabilities.sqlite.sql"));
        }
    }

    async fn assert_weakened_listing_avionics_authorization_table_rejected(
        expected: &str,
        replacement: &str,
    ) {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let table = "aircraft_sale_listing_avionics_link_authorizations";
        let table_sql: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?")
                .bind(table)
                .fetch_one(pool)
                .await
                .unwrap();
        let objects: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT type, name, sql FROM sqlite_schema \
             WHERE sql IS NOT NULL AND ( \
               (type = 'index' AND tbl_name = ?) OR \
               (type = 'trigger' AND instr(lower(sql), lower(?)) > 0) \
             ) ORDER BY type, name",
        )
        .bind(table)
        .bind(table)
        .fetch_all(pool)
        .await
        .unwrap();
        assert!(table_sql.contains(expected));
        for (_, name, _) in objects
            .iter()
            .filter(|(object_type, _, _)| object_type == "trigger")
        {
            pool.execute(format!("DROP TRIGGER \"{}\"", name.replace('"', "\"\"")).as_str())
                .await
                .unwrap();
        }
        pool.execute("DROP TABLE aircraft_sale_listing_avionics_link_authorizations")
            .await
            .unwrap();
        pool.execute(table_sql.replace(expected, replacement).as_str())
            .await
            .unwrap();
        for (_, _, statement) in objects {
            pool.execute(statement.as_str()).await.unwrap();
        }
        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a weakened listing-avionics authorization table must fail startup")
            .to_string();
        assert!(
            error.contains("capture-bound grounded avionics resolutions"),
            "unexpected startup error: {error}"
        );
        assert!(
            error.contains("20260825_listing_avionics_grounded_capabilities.sqlite.sql"),
            "unexpected startup error: {error}"
        );
    }

    #[tokio::test]
    async fn authorization_startup_rejects_weakened_scope_and_checkpoint_triggers() {
        assert_weakened_listing_avionics_authorization_table_rejected(
            "plugin_submission_id INTEGER\n      REFERENCES plugin_submissions(id) ON DELETE CASCADE",
            "plugin_submission_id INTEGER",
        )
        .await;
        assert_weakened_listing_avionics_authorization_table_rejected(
            ",\n    CHECK (\n      (authorization_kind = 'manufacturer_reuse'\n        AND grounded_resolution_sha256 IS NULL\n        AND plugin_submission_id IS NULL\n        AND extracted_listing_sha256 IS NULL\n        AND source_revocation_count IS NULL)\n      OR\n      (authorization_kind = 'same_case_grounded'\n        AND length(grounded_resolution_sha256) = 64\n        AND grounded_resolution_sha256 = lower(grounded_resolution_sha256)\n        AND grounded_resolution_sha256 NOT GLOB '*[^0-9a-f]*'\n        AND plugin_submission_id IS NOT NULL\n        AND extracted_listing_sha256 IS NOT NULL\n        AND source_revocation_count IS NOT NULL\n        AND source_revocation_count >= 0)\n    )",
            "",
        )
        .await;
        assert_corrupt_grounded_capability_schema_rejected(&[
            "DROP TRIGGER listing_avionics_authorizations_invalidate_capture_update",
            "CREATE TRIGGER listing_avionics_authorizations_invalidate_capture_update AFTER UPDATE OF canonical_listing_id, rendered_html, rendered_html_sha256, extracted_listing_json, extraction_error ON plugin_submissions WHEN 0 BEGIN DELETE FROM aircraft_sale_listing_avionics_link_authorizations WHERE plugin_submission_id = OLD.id; END",
        ])
        .await;
        assert_corrupt_grounded_capability_schema_rejected(&[
            "CREATE TRIGGER unexpected_listing_avionics_authorization_trigger AFTER UPDATE ON plugin_submissions BEGIN DELETE FROM aircraft_sale_listing_avionics_link_authorizations; END",
        ])
        .await;
        assert_corrupt_grounded_capability_schema_rejected(&[
            "DROP TRIGGER listing_avionics_authorizations_invalidate_origin_revocation",
            "CREATE TRIGGER listing_avionics_authorizations_invalidate_origin_revocation AFTER INSERT ON avionics_authoritative_source_origin_revocations BEGIN DELETE FROM aircraft_sale_listing_avionics_link_authorizations WHERE authorization_kind = 'same_case_grounded'; END",
        ])
        .await;
    }

    async fn assert_corrupt_postgres_grounded_capability_schema_rejected(
        database_url: &str,
        tamper_sql: &str,
    ) {
        let reset = reset_isolated_postgres(database_url).await;
        reset.close().await;
        let db = AppDb::connect(database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::raw_sql(tamper_sql).execute(pool).await.unwrap();
        drop(db);
        let error = connect_error(AppDb::connect(database_url).await);
        assert!(error.contains("capture-bound grounded avionics resolutions"));
        assert!(error.contains("20260825_listing_avionics_grounded_capabilities.postgres.sql"));
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_grounded_capability_functions_resist_hostile_search_path_and_tampering() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = reset_isolated_postgres(&database_url).await;
        reset.close().await;

        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        let function_configs: Vec<(String, String)> = sqlx::query_as(
            r#"
            SELECT routine.proname,
                   pg_catalog.array_to_string(routine.proconfig, E'\n')
            FROM pg_catalog.pg_proc routine
            JOIN pg_catalog.pg_namespace namespace
              ON namespace.oid = routine.pronamespace
            WHERE namespace.nspname = 'public'
              AND routine.proname IN (
                'validate_listing_avionics_grounded_capability',
                'reject_listing_avionics_grounded_capability_update'
              )
            ORDER BY routine.proname
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(
            function_configs,
            vec![
                (
                    "reject_listing_avionics_grounded_capability_update".to_owned(),
                    "search_path=pg_catalog".to_owned(),
                ),
                (
                    "validate_listing_avionics_grounded_capability".to_owned(),
                    "search_path=pg_catalog".to_owned(),
                ),
            ]
        );

        let mut connection = pool.acquire().await.unwrap();
        sqlx::raw_sql(
            "CREATE SCHEMA attacker_schema; \
                 CREATE TABLE attacker_schema.plugin_submissions ( \
                   id BIGINT, canonical_listing_id BIGINT, rendered_html_sha256 TEXT, \
                   extracted_listing_json TEXT, extraction_error TEXT \
                 ); \
                 CREATE TABLE attacker_schema.avionics_approved_product_graph_identities ( \
                   avionics_model_id BIGINT \
                 ); \
                 INSERT INTO attacker_schema.plugin_submissions VALUES ( \
                   70001, 70002, repeat('a', 64), '{}', NULL \
                 ); \
                 INSERT INTO attacker_schema.avionics_approved_product_graph_identities \
                   VALUES (70003); \
                 SET search_path = attacker_schema, public, pg_catalog",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        let error = connection
            .execute(
                "INSERT INTO public.aircraft_sale_listing_avionics_grounded_capabilities ( \
                   listing_id, plugin_submission_id, occurrence_index, occurrence_role, \
                   avionics_model_id, requested_quantity, configuration_action, \
                   request_sha256, capability_sha256, grounded_resolution_sha256, \
                   evidence_capture_sha256, extracted_listing_sha256, product_fingerprint, \
                   collision_closure_sha256, source_revocation_count, policy_version \
                 ) VALUES ( \
                   70002, 70001, 0, 'primary', 70003, 1, 'installed', \
                   repeat('a', 64), repeat('b', 64), repeat('c', 64), \
                   repeat('a', 64), repeat('d', 64), repeat('e', 64), \
                   repeat('f', 64), 0, 'listing_avionics_grounded_capability' \
                 )",
            )
            .await
            .expect_err("attacker shadow rows must not satisfy the public validation function")
            .to_string();
        assert!(error.contains(
            "grounded avionics capability requires its exact current capture-bound listing, approved product, and source-revocation epoch"
        ));
        connection.execute("RESET search_path").await.unwrap();
        drop(connection);
        drop(db);

        for tamper_sql in [
            "ALTER FUNCTION public.validate_listing_avionics_grounded_capability() SET search_path = public",
            "ALTER FUNCTION public.validate_listing_avionics_grounded_capability() SECURITY DEFINER",
            "DROP TRIGGER listing_avionics_grounded_capabilities_validate_insert ON public.aircraft_sale_listing_avionics_grounded_capabilities; \
             CREATE TRIGGER listing_avionics_grounded_capabilities_validate_insert \
             BEFORE INSERT ON public.aircraft_sale_listing_avionics_grounded_capabilities \
             FOR EACH ROW WHEN (false) EXECUTE FUNCTION public.validate_listing_avionics_grounded_capability()",
            "CREATE TRIGGER unexpected_listing_avionics_grounded_capability_trigger \
             BEFORE DELETE ON public.aircraft_sale_listing_avionics_grounded_capabilities \
             FOR EACH ROW EXECUTE FUNCTION public.reject_listing_avionics_grounded_capability_update()",
            "DO $tamper$ \
             DECLARE target_constraint text; \
             BEGIN \
               SELECT constraint_row.conname INTO STRICT target_constraint \
               FROM pg_catalog.pg_constraint constraint_row \
               WHERE constraint_row.conrelid = pg_catalog.to_regclass( \
                 'public.aircraft_sale_listing_avionics_grounded_capabilities' \
               ) AND constraint_row.contype = 'c' \
                 AND pg_catalog.strpos( \
                   pg_catalog.pg_get_constraintdef(constraint_row.oid), \
                   'occurrence_index' \
                 ) > 0; \
               EXECUTE pg_catalog.format( \
                 'ALTER TABLE public.aircraft_sale_listing_avionics_grounded_capabilities DROP CONSTRAINT %I', \
                 target_constraint \
               ); \
             END \
             $tamper$; \
             ALTER TABLE public.aircraft_sale_listing_avionics_grounded_capabilities \
             ADD CONSTRAINT hostile_occurrence_index_check \
             CHECK ((occurrence_index >= 0) OR TRUE)",
            "DO $tamper$ \
             DECLARE target_constraint text; \
             BEGIN \
               SELECT constraint_row.conname INTO STRICT target_constraint \
               FROM pg_catalog.pg_constraint constraint_row \
               WHERE constraint_row.conrelid = pg_catalog.to_regclass( \
                 'public.aircraft_sale_listing_avionics_link_authorizations' \
               ) AND constraint_row.contype = 'f' \
                 AND constraint_row.confrelid = pg_catalog.to_regclass( \
                   'public.plugin_submissions' \
                 ); \
               EXECUTE pg_catalog.format( \
                 'ALTER TABLE public.aircraft_sale_listing_avionics_link_authorizations DROP CONSTRAINT %I', \
                 target_constraint \
               ); \
             END \
             $tamper$",
            "DO $tamper$ \
             DECLARE target_constraint text; \
             BEGIN \
               SELECT constraint_row.conname INTO STRICT target_constraint \
               FROM pg_catalog.pg_constraint constraint_row \
               WHERE constraint_row.conrelid = pg_catalog.to_regclass( \
                 'public.aircraft_sale_listing_avionics_link_authorizations' \
               ) AND constraint_row.contype = 'c' \
                 AND pg_catalog.strpos( \
                   pg_catalog.pg_get_constraintdef(constraint_row.oid), \
                   'plugin_submission_id' \
                 ) > 0 \
                 AND pg_catalog.strpos( \
                   pg_catalog.pg_get_constraintdef(constraint_row.oid), \
                   'authorization_kind' \
                 ) > 0; \
               EXECUTE pg_catalog.format( \
                 'ALTER TABLE public.aircraft_sale_listing_avionics_link_authorizations DROP CONSTRAINT %I', \
                 target_constraint \
               ); \
             END \
             $tamper$",
            "ALTER TABLE public.plugin_submissions DISABLE TRIGGER listing_avionics_authorizations_invalidate_capture_update",
            "CREATE TRIGGER unexpected_listing_avionics_authorization_trigger \
             AFTER UPDATE ON public.plugin_submissions FOR EACH ROW \
             EXECUTE FUNCTION public.invalidate_listing_avionics_authorization_for_capture()",
        ] {
            assert_corrupt_postgres_grounded_capability_schema_rejected(
                &database_url,
                tamper_sql,
            )
            .await;
        }
    }
    #[test]
    fn pending_review_columns_have_backend_parity() {
        let table = "aircraft_sale_listing_pending_reviews";
        assert_eq!(
            table_columns(SQLITE_SCHEMA_SQL, table),
            table_columns(POSTGRES_SCHEMA_SQL, table),
            "SQLite/Postgres column mismatch for {table}"
        );
        assert_eq!(
            table_columns(LISTING_PENDING_REVIEWS_SQLITE_MIGRATION_SQL, table),
            table_columns(LISTING_PENDING_REVIEWS_POSTGRES_MIGRATION_SQL, table),
            "SQLite/Postgres migration column mismatch for {table}"
        );
        assert!(SQLITE_SCHEMA_SQL.contains("'pending_review'"));
        assert!(POSTGRES_SCHEMA_SQL.contains("'pending_review'"));
    }

    #[test]
    fn identity_postcondition_tables_have_backend_parity() {
        assert_eq!(
            table_columns(SQLITE_SCHEMA_SQL, "schema_migration_contracts"),
            table_columns(POSTGRES_SCHEMA_SQL, "schema_migration_contracts"),
            "SQLite/Postgres migration-contract ledger columns differ"
        );
        for table in [
            "avionics_manufacturer_canonical_keys",
            "avionics_approved_product_identities",
            "avionics_catalog_consolidation_guard",
        ] {
            assert_eq!(
                table_columns(SQLITE_SCHEMA_SQL, table),
                table_columns(POSTGRES_SCHEMA_SQL, table),
                "SQLite/Postgres schema column mismatch for {table}"
            );
            assert_eq!(
                table_columns(IDENTITY_POSTCONDITIONS_SQLITE_MIGRATION_SQL, table),
                table_columns(IDENTITY_POSTCONDITIONS_POSTGRES_MIGRATION_SQL, table),
                "SQLite/Postgres migration column mismatch for {table}"
            );
        }
        for contract in [
            "avionics_catalog_authorized_consolidations",
            "schema_migration_contracts",
            IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_FINGERPRINT,
            "avionics_manufacturer_alias_candidate_pending_insert",
            "avionics_manufacturer_identity_name_immutable",
            "avionics_models_canonical_identity_sync_update",
            "avionics_models_approved_delete_guard",
            "approved avionics product cannot be demoted or rewrite identity evidence",
            "avionics approval must be staged from an unreviewed product",
            "avionics_approved_registry_completeness_guard",
            "guarded avionics consolidation identities are immutable",
            "ready listing requires unique approved canonical avionics",
        ] {
            assert!(IDENTITY_POSTCONDITIONS_SQLITE_MIGRATION_SQL.contains(contract));
            assert!(IDENTITY_POSTCONDITIONS_POSTGRES_MIGRATION_SQL.contains(contract));
        }
    }

    #[test]
    fn human_consolidation_tables_and_contract_have_backend_parity() {
        for table in [
            "avionics_catalog_human_consolidation_authorizations",
            "avionics_catalog_human_consolidation_members",
            "avionics_catalog_human_consolidation_guard",
            "avionics_catalog_human_consolidation_claim",
        ] {
            assert_eq!(
                table_columns(SQLITE_SCHEMA_SQL, table),
                table_columns(POSTGRES_SCHEMA_SQL, table),
                "SQLite/Postgres schema column mismatch for {table}"
            );
            assert_eq!(
                table_columns(AVIONICS_HUMAN_CONSOLIDATION_SQLITE_MIGRATION_SQL, table),
                table_columns(AVIONICS_HUMAN_CONSOLIDATION_POSTGRES_MIGRATION_SQL, table),
                "SQLite/Postgres migration column mismatch for {table}"
            );
        }
        assert_eq!(AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_VERSION, 1);
        for contract_value in [
            AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_MIGRATION,
            AVIONICS_HUMAN_REVIEWED_CONSOLIDATION_CONTRACT_FINGERPRINT,
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(contract_value));
            assert!(POSTGRES_SCHEMA_SQL.contains(contract_value));
            assert!(AVIONICS_HUMAN_CONSOLIDATION_SQLITE_MIGRATION_SQL.contains(contract_value));
            assert!(AVIONICS_HUMAN_CONSOLIDATION_POSTGRES_MIGRATION_SQL.contains(contract_value));
        }
        for required_object in [
            "avionics_catalog_valid_human_consolidation_pairs",
            "avionics_catalog_human_consolidation_guard_validate_insert",
            "avionics_catalog_human_consolidation_claim_validate_insert",
            "avionics_catalog_human_consolidation_claim",
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(required_object));
            assert!(POSTGRES_SCHEMA_SQL.contains(required_object));
            assert!(AVIONICS_HUMAN_CONSOLIDATION_SQLITE_MIGRATION_SQL.contains(required_object));
            assert!(AVIONICS_HUMAN_CONSOLIDATION_POSTGRES_MIGRATION_SQL.contains(required_object));
        }
    }

    #[test]
    fn descriptive_consolidation_contract_has_backend_parity() {
        assert_eq!(AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_VERSION, 1);
        for contract_value in [
            AVIONICS_DESCRIPTIVE_CONSOLIDATION_MIGRATION,
            AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_FINGERPRINT,
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(contract_value));
            assert!(POSTGRES_SCHEMA_SQL.contains(contract_value));
            assert!(
                AVIONICS_DESCRIPTIVE_CONSOLIDATION_SQLITE_MIGRATION_SQL.contains(contract_value)
            );
            assert!(
                AVIONICS_DESCRIPTIVE_CONSOLIDATION_POSTGRES_MIGRATION_SQL.contains(contract_value)
            );
        }
        for required_semantic in [
            "NEW.member_role <> 'survivor'",
            "selected_member.canonical_model_key_snapshot",
            "member.canonical_model_key_snapshot",
            "authorization_sha256",
            "normalized_manufacturer_identifier",
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(required_semantic));
            assert!(POSTGRES_SCHEMA_SQL.contains(required_semantic));
            assert!(
                AVIONICS_DESCRIPTIVE_CONSOLIDATION_SQLITE_MIGRATION_SQL.contains(required_semantic)
            );
            assert!(AVIONICS_DESCRIPTIVE_CONSOLIDATION_POSTGRES_MIGRATION_SQL
                .contains(required_semantic));
        }
        for migration in [
            AVIONICS_DESCRIPTIVE_CONSOLIDATION_SQLITE_MIGRATION_SQL,
            AVIONICS_DESCRIPTIVE_CONSOLIDATION_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(!migration.contains(
                "NEW.canonical_model_key_snapshot = authorization.canonical_model_key_snapshot"
            ));
            assert!(!migration.contains(
                "NEW.canonical_model_key_snapshot = authorization_row.canonical_model_key_snapshot"
            ));
        }
    }

    #[test]
    fn grounded_exact_model_consolidation_contract_has_backend_parity() {
        assert_eq!(
            AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_VERSION,
            1
        );
        for contract_value in [
            AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_MIGRATION,
            AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_CONTRACT_FINGERPRINT,
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(contract_value));
            assert!(POSTGRES_SCHEMA_SQL.contains(contract_value));
            assert!(
                AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_SQLITE_MIGRATION_SQL
                    .contains(contract_value)
            );
            assert!(
                AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_POSTGRES_MIGRATION_SQL
                    .contains(contract_value)
            );
        }
        for required_semantic in [
            "avionics_catalog_grounded_consolidation_authorizations",
            "avionics_catalog_grounded_consolidation_guard",
            "avionics_catalog_grounded_consolidation_claim",
            "avionics_catalog_valid_grounded_consolidation_pairs",
            "expected_member_count - 1",
            "member.normalized_name",
            "normalized_model_key",
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(required_semantic));
            assert!(POSTGRES_SCHEMA_SQL.contains(required_semantic));
            assert!(
                AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_SQLITE_MIGRATION_SQL
                    .contains(required_semantic)
            );
            assert!(
                AVIONICS_GROUNDED_EXACT_MODEL_CONSOLIDATION_POSTGRES_MIGRATION_SQL
                    .contains(required_semantic)
            );
        }
    }

    #[test]
    fn avionics_source_origin_tables_and_contract_have_backend_parity() {
        for table in [
            "avionics_authoritative_source_origins",
            "avionics_authoritative_source_origin_revocations",
        ] {
            assert_eq!(
                table_columns(SQLITE_SCHEMA_SQL, table),
                table_columns(POSTGRES_SCHEMA_SQL, table),
                "SQLite/Postgres schema column mismatch for {table}"
            );
            assert_eq!(
                table_columns(AVIONICS_SOURCE_ORIGINS_SQLITE_MIGRATION_SQL, table),
                table_columns(AVIONICS_SOURCE_ORIGINS_POSTGRES_MIGRATION_SQL, table),
                "SQLite/Postgres migration column mismatch for {table}"
            );
        }
        assert_eq!(AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_VERSION, 2);
        for contract_value in [
            AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_MIGRATION,
            AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_FINGERPRINT,
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(contract_value));
            assert!(POSTGRES_SCHEMA_SQL.contains(contract_value));
            assert!(AVIONICS_SOURCE_ORIGINS_SQLITE_MIGRATION_SQL.contains(contract_value));
            assert!(AVIONICS_SOURCE_ORIGINS_POSTGRES_MIGRATION_SQL.contains(contract_value));
        }
        for required_object in [
            "avionics_active_authoritative_source_origins",
            "avionics_authoritative_source_origins_immutable",
            "avionics_authoritative_source_origin_revocations_immutable",
            "avionics_garmin_authoritative_source_origins_bootstrap",
            "https://www.garmin.com",
            "https://static.garmin.com",
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(required_object));
            assert!(POSTGRES_SCHEMA_SQL.contains(required_object));
            assert!(AVIONICS_SOURCE_ORIGINS_SQLITE_MIGRATION_SQL.contains(required_object));
            assert!(AVIONICS_SOURCE_ORIGINS_POSTGRES_MIGRATION_SQL.contains(required_object));
        }
    }

    #[test]
    fn aircraft_retrieval_key_repair_contract_has_backend_parity() {
        assert_eq!(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION, 1);
        for contract_value in [
            AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION,
            AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_FINGERPRINT,
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(contract_value));
            assert!(POSTGRES_SCHEMA_SQL.contains(contract_value));
            assert!(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_SQLITE_MIGRATION_SQL.contains(contract_value));
            assert!(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_POSTGRES_MIGRATION_SQL.contains(contract_value));
        }
        for trigger in [
            "aircraft_make_retrieval_key_validate_insert",
            "aircraft_make_retrieval_key_validate_update",
            "aircraft_family_retrieval_key_validate_insert",
            "aircraft_family_retrieval_key_validate_update",
            "aircraft_generation_retrieval_key_validate_insert",
            "aircraft_generation_retrieval_key_validate_update",
            "aircraft_package_retrieval_key_validate_insert",
            "aircraft_package_retrieval_key_validate_update",
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(trigger));
            assert!(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_SQLITE_MIGRATION_SQL.contains(trigger));
        }
        for object in [
            "aircraft_retrieval_key",
            "require_aircraft_catalog_retrieval_key",
            "aircraft_make_retrieval_key_validate",
            "aircraft_family_retrieval_key_validate",
            "aircraft_generation_retrieval_key_validate",
            "aircraft_package_retrieval_key_validate",
        ] {
            assert!(POSTGRES_SCHEMA_SQL.contains(object));
            assert!(AIRCRAFT_CATALOG_RETRIEVAL_KEYS_POSTGRES_MIGRATION_SQL.contains(object));
        }
    }

    #[test]
    fn aircraft_tcds_make_lineage_migration_has_backend_parity() {
        assert_eq!(AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_VERSION, 1);
        for contract_value in [
            AIRCRAFT_TCDS_MAKE_LINEAGE_MIGRATION,
            AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_FINGERPRINT,
        ] {
            assert!(AIRCRAFT_TCDS_MAKE_LINEAGE_SQLITE_MIGRATION_SQL.contains(contract_value));
            assert!(AIRCRAFT_TCDS_MAKE_LINEAGE_POSTGRES_MIGRATION_SQL.contains(contract_value));
        }
        assert_eq!(
            table_columns(
                AIRCRAFT_TCDS_MAKE_LINEAGE_SQLITE_MIGRATION_SQL,
                "aircraft_tcds_make_lineage_bindings",
            ),
            table_columns(
                AIRCRAFT_TCDS_MAKE_LINEAGE_POSTGRES_MIGRATION_SQL,
                "aircraft_tcds_make_lineage_bindings",
            ),
        );
        for object in [
            "tcds_former_holder_name",
            "tcds_current_holder_name",
            "tcds_selection_basis",
            "listing_identity_assignment",
            "listing_ready_requires_canonical_aircraft",
        ] {
            assert!(
                AIRCRAFT_TCDS_MAKE_LINEAGE_SQLITE_MIGRATION_SQL.contains(object),
                "SQLite lineage migration is missing {object}",
            );
            assert!(
                AIRCRAFT_TCDS_MAKE_LINEAGE_POSTGRES_MIGRATION_SQL.contains(object),
                "Postgres lineage migration is missing {object}",
            );
        }
        assert!(AIRCRAFT_TCDS_MAKE_LINEAGE_SQLITE_MIGRATION_SQL
            .contains("aircraft_tcds_make_lineage_no_overlap"));
        assert!(AIRCRAFT_TCDS_MAKE_LINEAGE_POSTGRES_MIGRATION_SQL
            .contains("validate_aircraft_tcds_make_lineage"));
        assert!(AIRCRAFT_TCDS_MAKE_LINEAGE_POSTGRES_MIGRATION_SQL
            .contains("aircraft_tcds_make_lineage_matches"));
    }

    #[test]
    fn postgres_faa_provenance_uses_namespace_locked_exact_contracts() {
        assert_eq!(FAA_REFERENCE_REACHABILITY_CONTRACT_VERSION, 1);
        for contract_value in [
            FAA_REFERENCE_REACHABILITY_MIGRATION,
            FAA_REFERENCE_REACHABILITY_CONTRACT_FINGERPRINT,
        ] {
            assert!(POSTGRES_SCHEMA_SQL.contains(contract_value));
            assert!(FAA_REFERENCE_REACHABILITY_POSTGRES_MIGRATION_SQL.contains(contract_value));
        }
        for function in [
            "public.validate_faa_snapshot_evidence()",
            "public.validate_faa_aircraft_reference_reachability()",
            "public.validate_faa_engine_reference_reachability()",
            "public.validate_faa_coverage()",
            "public.preserve_faa_registry_data()",
        ] {
            assert!(POSTGRES_SCHEMA_SQL.contains(function));
            assert!(FAA_REFERENCE_REACHABILITY_POSTGRES_MIGRATION_SQL.contains(function));
        }
        for sql in [
            POSTGRES_SCHEMA_SQL,
            FAA_REFERENCE_REACHABILITY_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(sql.contains("FROM public.faa_registry_aircraft aircraft"));
            assert!(sql.contains("SET search_path = pg_catalog"));
            assert!(!sql.contains("EXECUTE FUNCTION public.validate_faa_reference_reachability()"));
            assert!(sql.contains("EXECUTE FUNCTION public.validate_faa_snapshot_evidence()"));
            assert!(sql.contains("EXECUTE FUNCTION public.validate_faa_coverage()"));
            assert!(sql.contains("EXECUTE FUNCTION public.preserve_faa_registry_data()"));
        }
    }

    #[test]
    fn faa_record_hash_domain_contract_has_backend_parity() {
        assert_eq!(FAA_RECORD_HASH_DOMAIN_CONTRACT_VERSION, 1);
        for contract_value in [
            FAA_RECORD_HASH_DOMAIN_MIGRATION,
            FAA_RECORD_HASH_DOMAIN_CONTRACT_FINGERPRINT,
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(contract_value));
            assert!(POSTGRES_SCHEMA_SQL.contains(contract_value));
            assert!(FAA_RECORD_HASH_DOMAIN_SQLITE_MIGRATION_SQL.contains(contract_value));
            assert!(FAA_RECORD_HASH_DOMAIN_POSTGRES_MIGRATION_SQL.contains(contract_value));
        }
        for sql in [
            SQLITE_SCHEMA_SQL,
            POSTGRES_SCHEMA_SQL,
            FAA_RECORD_HASH_DOMAIN_SQLITE_MIGRATION_SQL,
            FAA_RECORD_HASH_DOMAIN_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(sql.contains("record_hash_domain"));
            assert!(sql.contains("aircost-faa-master-retained-aircraft-projection-v1"));
            assert!(sql.contains("ON CONFLICT (migration_name) DO NOTHING"));
        }
        for migration in [
            FAA_RECORD_HASH_DOMAIN_SQLITE_MIGRATION_SQL,
            FAA_RECORD_HASH_DOMAIN_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(migration.contains("exact"));
            assert!(migration.contains("archive"));
            assert!(!migration
                .to_ascii_lowercase()
                .contains("update set installed_at"));
        }
        assert_eq!(
            table_columns(SQLITE_SCHEMA_SQL, "faa_registry_snapshots"),
            table_columns(POSTGRES_SCHEMA_SQL, "faa_registry_snapshots")
        );
    }

    #[test]
    fn migration_messages_select_the_backend_specific_script() {
        let sqlite = migration_required_message(
            DatabaseKind::Sqlite,
            "aircraft_sale_listings",
            "ingestion_state",
            VALUATION_DATA_HARDENING_MIGRATION,
        );
        assert!(sqlite.contains("20260720_valuation_data_hardening.sqlite.sql"));

        let postgres = migration_required_message(
            DatabaseKind::Postgres,
            "avionics_models",
            "catalog_status",
            AVIONICS_CATALOG_CURATION_MIGRATION,
        );
        assert!(postgres.contains("20260721_avionics_catalog_curation.postgres.sql"));

        let multi_type = avionics_multi_type_migration_required_message(DatabaseKind::Postgres);
        assert!(multi_type.contains("20260721_avionics_multi_type.postgres.sql"));

        let aircraft_reference =
            aircraft_reference_catalog_migration_required_message(DatabaseKind::Sqlite);
        assert!(aircraft_reference.contains("20260722_aircraft_reference_catalog.sqlite.sql"));

        let faa_hash_domain =
            faa_record_hash_domain_migration_required_message(DatabaseKind::Postgres);
        assert!(faa_hash_domain.contains("20260820_faa_record_hash_domain.postgres.sql"));
        assert!(faa_hash_domain.contains("exact release archive"));

        let pending_reviews =
            listing_pending_reviews_migration_required_message(DatabaseKind::Postgres);
        assert!(pending_reviews.contains("20260724_listing_pending_reviews.postgres.sql"));

        let identity_postconditions =
            identity_deduplication_postconditions_migration_required_message(DatabaseKind::Sqlite);
        assert!(identity_postconditions
            .contains("20260725_identity_deduplication_postconditions.sqlite.sql"));

        let listing_aircraft_identity =
            listing_aircraft_identity_migration_required_message(DatabaseKind::Postgres);
        assert!(
            listing_aircraft_identity.contains("20260725_listing_aircraft_identity.postgres.sql")
        );

        let listing_aircraft_projection =
            listing_aircraft_compatibility_projection_migration_required_message(
                DatabaseKind::Sqlite,
            );
        assert!(listing_aircraft_projection
            .contains("20260726_listing_aircraft_compatibility_projection.sqlite.sql"));

        let no_supported_selection =
            aircraft_identity_no_supported_selection_migration_required_message(
                DatabaseKind::Postgres,
            );
        assert!(no_supported_selection
            .contains("20260728_aircraft_identity_no_supported_selection.postgres.sql"));

        let retrieval_keys =
            aircraft_catalog_retrieval_keys_migration_required_message(DatabaseKind::Postgres);
        assert!(retrieval_keys.contains("20260729_aircraft_catalog_retrieval_keys.postgres.sql"));

        let make_lineage =
            aircraft_tcds_make_lineage_migration_required_message(DatabaseKind::Sqlite);
        assert!(make_lineage.contains("20260730_aircraft_tcds_make_lineage.sqlite.sql"));

        let source_origins = avionics_authoritative_source_origins_migration_required_message(
            DatabaseKind::Postgres,
        );
        assert!(
            source_origins.contains("20260801_avionics_authoritative_source_origins.postgres.sql")
        );

        let reuse_attestations =
            avionics_product_reuse_attestations_migration_required_message(DatabaseKind::Sqlite);
        assert!(reuse_attestations.contains("20260807_avionics_product_reuse_v2.sqlite.sql"));

        let descriptive_consolidation =
            avionics_descriptive_consolidation_migration_required_message(DatabaseKind::Postgres);
        assert!(descriptive_consolidation
            .contains("20260808_avionics_descriptive_consolidation.postgres.sql"));

        let faa_reference_reachability =
            faa_registry_contract_required_message(DatabaseKind::Postgres, "test object");
        assert!(
            faa_reference_reachability.contains("20260819_faa_reference_reachability.postgres.sql")
        );
    }
}
