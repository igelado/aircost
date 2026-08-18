use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, PgPool, SqlitePool};

use crate::depreciation::default_avionics_profile;
use crate::models::User;

pub const DEFAULT_DATABASE_PATH: &str = "data/aircost.sqlite3";
pub const DEFAULT_DATABASE_URL: &str = "sqlite://data/aircost.sqlite3";
pub const DEVELOPER_EMAIL: &str = "developer@localhost";
const DEVELOPER_AUTH_SUBJECT: &str = "developer";
const SQLITE_SCHEMA_SQL: &str = include_str!("../schema/sqlite.sql");
const POSTGRES_SCHEMA_SQL: &str = include_str!("../schema/postgres.sql");
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
const DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_MIGRATION: &str =
    "20260802_default_avionics_candidate_quarantine";
const DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_CONTRACT_VERSION: i64 = 2;
const DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_CONTRACT_FINGERPRINT: &str =
    "b8a6ecd15acc0ce14f67bf37ff4387c0ded4d1c6669d2fc4698b6c0a6c209ba4";
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
const LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_MIGRATION: &str =
    "20260818_listing_avionics_association_authorizations";
const LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_CONTRACT_VERSION: i64 = 1;
const LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_CONTRACT_FINGERPRINT: &str =
    "bbb76c8535647f2ecaab3179d5ef483bdef9ca23a0e14e3fd0888912fc3d90f9";
const LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_MIGRATION: &str =
    "20260818_listing_avionics_authorization_hash_domain_reset";
const LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_VERSION: i64 = 1;
const LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_FINGERPRINT: &str =
    "cd0c1e10c508017f7053d0ab418e627ef993029ab7523a045eb7b66b802d5033";

#[derive(Clone)]
pub struct AppDb {
    backend: DatabaseBackend,
}

#[derive(Clone)]
pub(crate) enum DatabaseBackend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DatabaseKind {
    Sqlite,
    Postgres,
}

impl AppDb {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let database_url = normalize_database_url(database_url);
        if is_postgres_url(&database_url) {
            let pool = PgPoolOptions::new()
                .max_connections(5)
                .connect(&database_url)
                .await
                .with_context(|| {
                    format!("could not connect to Postgres database {database_url}")
                })?;
            let db = Self {
                backend: DatabaseBackend::Postgres(pool),
            };
            db.ensure_required_migrations().await?;
            db.initialize().await?;
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
            db.ensure_required_migrations().await?;
            db.initialize().await?;
            Ok(db)
        }
    }

    pub(crate) fn backend(&self) -> &DatabaseBackend {
        &self.backend
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
        let missing_valuation_hardening = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
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

        let missing_avionics_curation = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
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

        let missing_avionics_multi_type = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        if missing_avionics_multi_type {
            bail!(avionics_multi_type_migration_required_message(self.kind()));
        }

        let missing_aircraft_reference_catalog = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        if missing_aircraft_reference_catalog {
            bail!(aircraft_reference_catalog_migration_required_message(
                self.kind()
            ));
        }

        let missing_listing_pending_reviews = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        if missing_listing_pending_reviews {
            bail!(listing_pending_reviews_migration_required_message(
                self.kind()
            ));
        }

        let missing_identity_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                        ('trigger', 'aircraft_model_variant_default_avionics_approved_insert',
                          'aircraft_model_variant_default_avionics'),
                        ('trigger', 'aircraft_model_variant_default_avionics_approved_update',
                          'aircraft_model_variant_default_avionics'),
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                        ('aircraft_model_variant_default_avionics',
                          'aircraft_model_variant_default_avionics_approved_insert'),
                        ('aircraft_model_variant_default_avionics',
                          'aircraft_model_variant_default_avionics_approved_update'),
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_identity_deduplication_postconditions = missing_identity_objects
            || self
                .migration_contract_missing(
                    "avionics_models",
                    IDENTITY_DEDUPLICATION_POSTCONDITIONS_MIGRATION,
                    IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_VERSION,
                    IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_identity_deduplication_postconditions {
            bail!(identity_deduplication_postconditions_migration_required_message(self.kind()));
        }

        let missing_listing_aircraft_identity_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_listing_aircraft_identity = missing_listing_aircraft_identity_objects
            || self
                .migration_contract_missing(
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

        let missing_listing_aircraft_compatibility_projection_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_listing_aircraft_compatibility_projection =
            missing_listing_aircraft_compatibility_projection_objects
                || self
                    .migration_contract_missing(
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

        let missing_no_supported_selection_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_no_supported_selection = missing_no_supported_selection_objects
            || self
                .migration_contract_missing(
                    "aircraft_identity_decisions",
                    AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_MIGRATION,
                    AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_CONTRACT_VERSION,
                    AIRCRAFT_IDENTITY_NO_SUPPORTED_SELECTION_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_no_supported_selection {
            bail!(aircraft_identity_no_supported_selection_migration_required_message(self.kind()));
        }

        let missing_aircraft_catalog_retrieval_key_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_aircraft_catalog_retrieval_keys = missing_aircraft_catalog_retrieval_key_objects
            || self
                .migration_contract_missing(
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

        let missing_aircraft_tcds_make_lineage_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_aircraft_tcds_make_lineage = missing_aircraft_tcds_make_lineage_objects
            || self
                .migration_contract_missing(
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

        let missing_human_consolidation_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_human_consolidation = missing_human_consolidation_objects
            || self
                .migration_contract_missing(
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
            .migration_contract_missing(
                "avionics_catalog_valid_human_consolidation_pairs",
                AVIONICS_DESCRIPTIVE_CONSOLIDATION_MIGRATION,
                AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_VERSION,
                AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_FINGERPRINT,
            )
            .await?;
        if missing_descriptive_consolidation {
            bail!(avionics_descriptive_consolidation_migration_required_message(self.kind()));
        }
        let missing_grounded_exact_model_consolidation_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                          'avionics_catalog_grounded_consolidation_authorization_validate_insert'),
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_grounded_exact_model_consolidation =
            missing_grounded_exact_model_consolidation_objects
                || self
                    .migration_contract_missing(
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

        let missing_avionics_source_origin_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_avionics_source_origins = missing_avionics_source_origin_objects
            || self
                .migration_contract_missing(
                    "avionics_manufacturers",
                    AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_MIGRATION,
                    AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_VERSION,
                    AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_avionics_source_origins {
            bail!(avionics_authoritative_source_origins_migration_required_message(self.kind()));
        }

        let default_avionics_table_exists = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1
                      FROM sqlite_schema
                      WHERE type = 'table'
                        AND name = 'aircraft_model_variant_default_avionics'
                    )
                    "#,
                )
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    "SELECT to_regclass('aircraft_model_variant_default_avionics') IS NOT NULL",
                )
                .fetch_one(pool)
                .await?
            }
        };
        let missing_default_avionics_candidate_objects = if !default_avionics_table_exists {
            false
        } else {
            match self.backend() {
                DatabaseBackend::Sqlite(pool) => {
                    sqlx::query_scalar::<_, i64>(
                        r#"
                    WITH required_objects(object_type, object_name) AS (
                      VALUES
                        ('table', 'aircraft_model_variant_default_avionics_candidates'),
                        ('trigger', 'aircraft_default_avionics_candidate_active_conflict_insert'),
                        ('trigger', 'aircraft_default_avionics_candidate_claim_immutable'),
                        ('trigger', 'aircraft_default_avionics_candidate_admission_guard'),
                        ('trigger', 'aircraft_default_avionics_candidate_admission_move')
                    )
                    SELECT
                      EXISTS (
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
                    .fetch_one(pool)
                    .await?
                        != 0
                }
                DatabaseBackend::Postgres(pool) => {
                    sqlx::query_scalar::<_, bool>(
                        r#"
                    WITH required_relations(object_name) AS (
                      VALUES
                        ('aircraft_model_variant_default_avionics_candidates')
                    ),
                    required_triggers(
                      parent_name, trigger_name, function_signature, trigger_type,
                      definition_fragment, requires_semantic_lock
                    ) AS (
                      VALUES
                        ('aircraft_model_variant_default_avionics_candidates',
                          'aircraft_default_avionics_candidate_active_conflict_insert',
                          'reject_active_default_avionics_candidate()', 7,
                          'default avionics claim already exists in the canonical table',
                          TRUE),
                        ('aircraft_model_variant_default_avionics_candidates',
                          'aircraft_default_avionics_candidate_claim_immutable',
                          'preserve_pending_default_avionics_claim()', 19,
                          'pending default avionics claims must be replaced',
                          FALSE),
                        ('aircraft_model_variant_default_avionics',
                          'aircraft_default_avionics_candidate_admission_guard',
                          'require_exact_pending_default_avionics_admission()', 7,
                          'canonical default admission must exactly match its pending claim',
                          TRUE),
                        ('aircraft_model_variant_default_avionics',
                          'aircraft_default_avionics_candidate_admission_move',
                          'move_admitted_default_avionics_candidate()', 5,
                          'DELETE FROM aircraft_model_variant_default_avionics_candidates',
                          FALSE)
                    )
                    SELECT
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
                            AND actual.tgfoid =
                              to_regprocedure(required.function_signature)
                            AND actual.tgtype = required.trigger_type
                            AND actual.tgenabled IN ('O', 'A')
                            AND NOT actual.tgisinternal
                            AND POSITION(
                              required.definition_fragment
                              IN pg_get_functiondef(actual.tgfoid)
                            ) > 0
                            AND (
                              NOT required.requires_semantic_lock
                              OR POSITION(
                                'pg_advisory_xact_lock'
                                IN pg_get_functiondef(actual.tgfoid)
                              ) > 0
                            )
                        )
                      )
                    "#,
                    )
                    .fetch_one(pool)
                    .await?
                }
            }
        };
        let invalid_default_avionics_candidate_state =
            if !default_avionics_table_exists || missing_default_avionics_candidate_objects {
                false
            } else {
                match self.backend() {
                    DatabaseBackend::Sqlite(pool) => {
                        sqlx::query_scalar::<_, i64>(
                            r#"
                            SELECT
                              EXISTS (
                                SELECT 1
                                FROM aircraft_model_variant_default_avionics default_avionics
                                JOIN avionics_models model
                                  ON model.id = default_avionics.avionics_model_id
                                WHERE model.catalog_status <> 'approved'
                              )
                              OR EXISTS (
                                SELECT 1
                                FROM aircraft_model_variant_default_avionics active
                                JOIN aircraft_model_variant_default_avionics_candidates candidate
                                  ON candidate.aircraft_model_variant_id =
                                     active.aircraft_model_variant_id
                                 AND candidate.model_year = active.model_year
                                 AND candidate.avionics_model_id =
                                     active.avionics_model_id
                              )
                            "#,
                        )
                        .fetch_one(pool)
                        .await?
                            != 0
                    }
                    DatabaseBackend::Postgres(pool) => {
                        sqlx::query_scalar::<_, bool>(
                            r#"
                            SELECT
                              EXISTS (
                                SELECT 1
                                FROM aircraft_model_variant_default_avionics default_avionics
                                JOIN avionics_models model
                                  ON model.id = default_avionics.avionics_model_id
                                WHERE model.catalog_status <> 'approved'
                              )
                              OR EXISTS (
                                SELECT 1
                                FROM aircraft_model_variant_default_avionics active
                                JOIN aircraft_model_variant_default_avionics_candidates candidate
                                  ON candidate.aircraft_model_variant_id =
                                     active.aircraft_model_variant_id
                                 AND candidate.model_year = active.model_year
                                 AND candidate.avionics_model_id =
                                     active.avionics_model_id
                              )
                            "#,
                        )
                        .fetch_one(pool)
                        .await?
                    }
                }
            };
        let missing_default_avionics_candidate_quarantine =
            missing_default_avionics_candidate_objects
                || invalid_default_avionics_candidate_state
                || self
                    .migration_contract_missing(
                        "aircraft_model_variant_default_avionics",
                        DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_MIGRATION,
                        DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_CONTRACT_VERSION,
                        DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_CONTRACT_FINGERPRINT,
                    )
                    .await?;
        if missing_default_avionics_candidate_quarantine {
            bail!(default_avionics_candidate_quarantine_migration_required_message(self.kind()));
        }

        let missing_avionics_reuse_attestation_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
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
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
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
                .fetch_one(pool)
                .await?
            }
        };
        let missing_avionics_reuse_attestations = missing_avionics_reuse_attestation_objects
            || self
                .migration_contract_missing(
                    "avionics_models",
                    AVIONICS_PRODUCT_REUSE_ATTESTATIONS_MIGRATION,
                    AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_VERSION,
                    AVIONICS_PRODUCT_REUSE_ATTESTATIONS_CONTRACT_FINGERPRINT,
                )
                .await?
            || self
                .migration_contract_missing(
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
            .migration_contract_missing(
                "avionics_models",
                AVIONICS_GROUNDED_EVIDENCE_REFRESH_MIGRATION,
                AVIONICS_GROUNDED_EVIDENCE_REFRESH_CONTRACT_VERSION,
                AVIONICS_GROUNDED_EVIDENCE_REFRESH_CONTRACT_FINGERPRINT,
            )
            .await?
        {
            bail!(avionics_grounded_evidence_refresh_migration_required_message(self.kind()));
        }
        let missing_listing_avionics_authorization_objects = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
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
                              'aircraft_sale_listing_avionics_authorizations'
                        )
                        OR NOT EXISTS (
                          SELECT 1 FROM sqlite_schema
                          WHERE type = 'index'
                            AND name =
                              'idx_listing_avionics_authorizations_model'
                        )
                        OR (
                          SELECT COUNT(*)
                          FROM sqlite_schema
                          WHERE type = 'trigger'
                            AND name IN (
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
                        ) <> 16
                        OR (
                          SELECT COUNT(*)
                          FROM pragma_foreign_key_list(
                            'aircraft_sale_listing_avionics_authorizations'
                          )
                        ) <> 2
                      )
                    "#,
                )
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT
                      to_regclass('aircraft_sale_listing_avionics') IS NOT NULL
                      AND (
                        to_regclass(
                          'aircraft_sale_listing_avionics_authorizations'
                        ) IS NULL
                        OR to_regclass(
                          'idx_listing_avionics_authorizations_model'
                        ) IS NULL
                        OR (
                          SELECT COUNT(*)
                          FROM pg_trigger
                          WHERE tgrelid IN (
                            to_regclass(
                              'aircraft_sale_listing_avionics_authorizations'
                            ),
                            to_regclass('aircraft_sale_listing_avionics'),
                            to_regclass('avionics_product_reuse_attestations'),
                            to_regclass('avionics_models'),
                            to_regclass('avionics_model_types'),
                            to_regclass('avionics_types'),
                            to_regclass('avionics_approved_product_identities'),
                            to_regclass('avionics_manufacturers'),
                            to_regclass(
                              'avionics_authoritative_source_origin_revocations'
                            ),
                            to_regclass('plugin_submissions')
                          )
                            AND tgname IN (
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
                            AND NOT tgisinternal
                        ) <> 16
                      )
                    "#,
                )
                .fetch_one(pool)
                .await?
            }
        };
        let missing_listing_avionics_authorizations = missing_listing_avionics_authorization_objects
            || self
                .migration_contract_missing(
                    "aircraft_sale_listing_avionics",
                    LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_MIGRATION,
                    LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_CONTRACT_VERSION,
                    LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_CONTRACT_FINGERPRINT,
                )
                .await?;
        if missing_listing_avionics_authorizations {
            bail!(
                listing_avionics_association_authorizations_migration_required_message(self.kind())
            );
        }
        if self
            .migration_contract_missing(
                "aircraft_sale_listing_avionics_authorizations",
                LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_MIGRATION,
                LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_VERSION,
                LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_FINGERPRINT,
            )
            .await?
        {
            bail!(
                listing_avionics_authorization_hash_domain_reset_migration_required_message(
                    self.kind()
                )
            );
        }
        Ok(())
    }

    async fn migration_contract_missing(
        &self,
        anchor_object: &str,
        migration_name: &str,
        contract_version: i64,
        contract_fingerprint: &str,
    ) -> Result<bool> {
        let anchor_exists = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE name = ?)",
                )
                .bind(anchor_object)
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>("SELECT to_regclass($1::text) IS NOT NULL")
                    .bind(anchor_object)
                    .fetch_one(pool)
                    .await?
            }
        };
        if !anchor_exists {
            return Ok(false);
        }

        let ledger_has_expected_shape = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT
                      EXISTS (
                        SELECT 1 FROM sqlite_schema
                        WHERE type = 'table' AND name = 'schema_migration_contracts'
                      )
                      AND (
                        SELECT count(*)
                        FROM pragma_table_info('schema_migration_contracts')
                        WHERE (name = 'migration_name' AND upper(type) = 'TEXT')
                           OR (name = 'contract_version' AND upper(type) = 'INTEGER')
                           OR (name = 'contract_fingerprint' AND upper(type) = 'TEXT')
                           OR (name = 'installed_at' AND upper(type) = 'TEXT')
                      ) = 4
                    "#,
                )
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT
                      EXISTS (
                        SELECT 1
                        FROM pg_class actual
                        WHERE actual.oid = to_regclass('schema_migration_contracts')
                          AND actual.relkind = 'r'
                      )
                      AND NOT EXISTS (
                        SELECT 1
                        FROM (
                          VALUES
                            ('migration_name', 'text'::regtype),
                            ('contract_version', 'integer'::regtype),
                            ('contract_fingerprint', 'text'::regtype),
                            ('installed_at', 'text'::regtype)
                        ) required(column_name, type_oid)
                        WHERE NOT EXISTS (
                          SELECT 1
                          FROM pg_attribute actual
                          WHERE actual.attrelid = to_regclass('schema_migration_contracts')
                            AND actual.attname = required.column_name
                            AND actual.atttypid = required.type_oid::oid
                            AND NOT actual.attisdropped
                        )
                      )
                    "#,
                )
                .fetch_one(pool)
                .await?
            }
        };
        if !ledger_has_expected_shape {
            return Ok(true);
        }

        let exact_contract_exists = match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1
                      FROM schema_migration_contracts
                      WHERE migration_name = ?
                        AND contract_version = ?
                        AND contract_fingerprint = ?
                    )
                    "#,
                )
                .bind(migration_name)
                .bind(contract_version)
                .bind(contract_fingerprint)
                .fetch_one(pool)
                .await?
                    != 0
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, bool>(
                    r#"
                    SELECT EXISTS (
                      SELECT 1
                      FROM schema_migration_contracts
                      WHERE migration_name = $1
                        AND contract_version = $2
                        AND contract_fingerprint = $3
                    )
                    "#,
                )
                .bind(migration_name)
                .bind(contract_version)
                .bind(contract_fingerprint)
                .fetch_one(pool)
                .await?
            }
        };
        Ok(!exact_contract_exists)
    }

    async fn initialize(&self) -> Result<()> {
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                let mut connection = pool.acquire().await?;
                for statement in split_sql_statements(SQLITE_SCHEMA_SQL) {
                    connection.execute(statement).await?;
                }
            }
            DatabaseBackend::Postgres(pool) => {
                let mut connection = pool.acquire().await?;
                for statement in split_sql_statements(POSTGRES_SCHEMA_SQL) {
                    connection.execute(statement).await?;
                }
            }
        }
        self.seed_developer_user().await?;
        self.seed_depreciation_profile().await?;
        self.seed_component_depreciation_profiles().await?;
        Ok(())
    }

    async fn seed_developer_user(&self) -> Result<()> {
        let sql = self.sql(
            r#"
            INSERT INTO users (
              email,
              display_name,
              auth_provider,
              auth_subject
            )
            VALUES (?, ?, ?, ?)
            ON CONFLICT (auth_subject) DO NOTHING
            "#,
        );
        match self.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(&sql)
                    .bind(DEVELOPER_EMAIL)
                    .bind("Developer")
                    .bind("local")
                    .bind(DEVELOPER_AUTH_SUBJECT)
                    .execute(pool)
                    .await?;
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query(&sql)
                    .bind(DEVELOPER_EMAIL)
                    .bind("Developer")
                    .bind("local")
                    .bind(DEVELOPER_AUTH_SUBJECT)
                    .execute(pool)
                    .await?;
            }
        }
        Ok(())
    }

    async fn seed_depreciation_profile(&self) -> Result<()> {
        let profile = crate::depreciation::AircraftProfile {
            name: "generic:all".to_string(),
            age_decay_rate: 0.05,
            long_run_residual_fraction: 0.28,
            new_to_used_discount_fraction: 0.08,
            new_to_used_discount_years: 1.0,
            airframe_doubling_discount: 0.15,
            max_airframe_premium: 0.12,
            max_airframe_discount: 0.30,
            replacement_floor_fraction: 0.0,
            minimum_value_fraction: 0.05,
            high_time_threshold_hours: Some(10_000.0),
            high_time_discount_at_double_threshold: 0.12,
        };
        for profile in [profile] {
            let sql = self.sql(
                r#"
                INSERT INTO depreciation_profiles (
                  name,
                  age_decay_rate,
                  long_run_residual_fraction,
                  new_to_used_discount_fraction,
                  new_to_used_discount_years,
                  airframe_doubling_discount,
                  max_airframe_premium,
                  max_airframe_discount,
                  replacement_floor_fraction,
                  minimum_value_fraction,
                  high_time_threshold_hours,
                  high_time_discount_at_double_threshold,
                  is_system_profile
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (name) DO NOTHING
                "#,
            );
            match self.backend() {
                DatabaseBackend::Sqlite(pool) => {
                    sqlx::query(&sql)
                        .bind(profile.name.as_str())
                        .bind(profile.age_decay_rate)
                        .bind(profile.long_run_residual_fraction)
                        .bind(profile.new_to_used_discount_fraction)
                        .bind(profile.new_to_used_discount_years)
                        .bind(profile.airframe_doubling_discount)
                        .bind(profile.max_airframe_premium)
                        .bind(profile.max_airframe_discount)
                        .bind(profile.replacement_floor_fraction)
                        .bind(profile.minimum_value_fraction)
                        .bind(profile.high_time_threshold_hours)
                        .bind(profile.high_time_discount_at_double_threshold)
                        .bind(true)
                        .execute(pool)
                        .await?;
                }
                DatabaseBackend::Postgres(pool) => {
                    sqlx::query(&sql)
                        .bind(profile.name.as_str())
                        .bind(profile.age_decay_rate)
                        .bind(profile.long_run_residual_fraction)
                        .bind(profile.new_to_used_discount_fraction)
                        .bind(profile.new_to_used_discount_years)
                        .bind(profile.airframe_doubling_discount)
                        .bind(profile.max_airframe_premium)
                        .bind(profile.max_airframe_discount)
                        .bind(profile.replacement_floor_fraction)
                        .bind(profile.minimum_value_fraction)
                        .bind(profile.high_time_threshold_hours)
                        .bind(profile.high_time_discount_at_double_threshold)
                        .bind(true)
                        .execute(pool)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn seed_component_depreciation_profiles(&self) -> Result<()> {
        let avionics = default_avionics_profile();
        let rows = [
            ("engine", None, None, Some(0.5)),
            ("propeller", None, None, Some(0.5)),
            (
                "avionics",
                Some(avionics.age_decay_rate),
                Some(avionics.long_run_residual_fraction),
                None,
            ),
        ];

        for (component_type, age_decay_rate, long_run_residual_fraction, baseline_life_fraction) in
            rows
        {
            let sql = self.sql(
                r#"
                INSERT INTO component_depreciation_profiles (
                  component_type,
                  age_decay_rate,
                  long_run_residual_fraction,
                  baseline_life_fraction
                )
                VALUES (?, ?, ?, ?)
                ON CONFLICT (component_type) DO NOTHING
                "#,
            );
            match self.backend() {
                DatabaseBackend::Sqlite(pool) => {
                    sqlx::query(&sql)
                        .bind(component_type)
                        .bind(age_decay_rate)
                        .bind(long_run_residual_fraction)
                        .bind(baseline_life_fraction)
                        .execute(pool)
                        .await?;
                }
                DatabaseBackend::Postgres(pool) => {
                    sqlx::query(&sql)
                        .bind(component_type)
                        .bind(age_decay_rate)
                        .bind(long_run_residual_fraction)
                        .bind(baseline_life_fraction)
                        .execute(pool)
                        .await?;
                }
            }
        }
        Ok(())
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

fn default_avionics_candidate_quarantine_migration_required_message(kind: DatabaseKind) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: unapproved factory-default avionics claims \
         must be isolated from canonical valuation inputs; back up the database, apply \
         `migrations/{DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_MIGRATION}.{backend}.sql`, then \
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

fn listing_avionics_association_authorizations_migration_required_message(
    kind: DatabaseKind,
) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: exact listing-avionics associations must use \
         current manufacturer-reuse or same-case grounded authorization; back up the database, apply \
         `migrations/{LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_MIGRATION}.{backend}.sql`, then \
         restart aircost"
    )
}

fn listing_avionics_authorization_hash_domain_reset_migration_required_message(
    kind: DatabaseKind,
) -> String {
    let backend = match kind {
        DatabaseKind::Sqlite => "sqlite",
        DatabaseKind::Postgres => "postgres",
    };
    format!(
        "database migration required before startup: incompatible derived manufacturer-reuse \
         receipts must be invalidated without changing listing links or catalog products; back up \
         the database, apply \
         `migrations/{LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_MIGRATION}.{backend}.sql`, \
         then restart aircost"
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
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

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
        identity_deduplication_postconditions_migration_required_message,
        listing_aircraft_compatibility_projection_migration_required_message,
        listing_aircraft_identity_migration_required_message,
        listing_pending_reviews_migration_required_message, migration_required_message,
        split_sql_statements, AppDb, DatabaseBackend, DatabaseKind,
        AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_FINGERPRINT,
        AIRCRAFT_CATALOG_RETRIEVAL_KEYS_CONTRACT_VERSION,
        AIRCRAFT_CATALOG_RETRIEVAL_KEYS_MIGRATION, AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_FINGERPRINT,
        AIRCRAFT_TCDS_MAKE_LINEAGE_CONTRACT_VERSION, AIRCRAFT_TCDS_MAKE_LINEAGE_MIGRATION,
        AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_FINGERPRINT,
        AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_CONTRACT_VERSION,
        AVIONICS_AUTHORITATIVE_SOURCE_ORIGINS_MIGRATION, AVIONICS_CATALOG_CURATION_MIGRATION,
        AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_FINGERPRINT,
        AVIONICS_DESCRIPTIVE_CONSOLIDATION_CONTRACT_VERSION,
        AVIONICS_DESCRIPTIVE_CONSOLIDATION_MIGRATION,
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
        AVIONICS_PRODUCT_REUSE_V2_MIGRATION,
        DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_CONTRACT_FINGERPRINT,
        DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_CONTRACT_VERSION,
        DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_MIGRATION,
        IDENTITY_DEDUPLICATION_POSTCONDITIONS_CONTRACT_FINGERPRINT,
        LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_CONTRACT_FINGERPRINT,
        LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_MIGRATION,
        LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_FINGERPRINT,
        LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_VERSION,
        LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_MIGRATION, POSTGRES_SCHEMA_SQL,
        SQLITE_SCHEMA_SQL, VALUATION_DATA_HARDENING_MIGRATION,
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
    const DEFAULT_AVIONICS_CANDIDATES_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260802_default_avionics_candidate_quarantine.sqlite.sql");
    const DEFAULT_AVIONICS_CANDIDATES_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260802_default_avionics_candidate_quarantine.postgres.sql");
    const AVIONICS_REUSE_ATTESTATIONS_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260803_avionics_product_reuse_attestations.sqlite.sql");
    const AVIONICS_REUSE_ATTESTATIONS_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260803_avionics_product_reuse_attestations.postgres.sql");
    const AVIONICS_REUSE_V2_SQLITE_MIGRATION_SQL: &str =
        include_str!("../migrations/20260807_avionics_product_reuse_v2.sqlite.sql");
    const AVIONICS_REUSE_V2_POSTGRES_MIGRATION_SQL: &str =
        include_str!("../migrations/20260807_avionics_product_reuse_v2.postgres.sql");
    const LISTING_AVIONICS_AUTHORIZATIONS_SQLITE_MIGRATION_SQL: &str = include_str!(
        "../migrations/20260818_listing_avionics_association_authorizations.sqlite.sql"
    );
    const LISTING_AVIONICS_AUTHORIZATIONS_POSTGRES_MIGRATION_SQL: &str = include_str!(
        "../migrations/20260818_listing_avionics_association_authorizations.postgres.sql"
    );
    const LISTING_AVIONICS_AUTHORIZATION_HASH_RESET_SQLITE_MIGRATION_SQL: &str = include_str!(
        "../migrations/20260818_listing_avionics_authorization_hash_domain_reset.sqlite.sql"
    );
    const LISTING_AVIONICS_AUTHORIZATION_HASH_RESET_POSTGRES_MIGRATION_SQL: &str = include_str!(
        "../migrations/20260818_listing_avionics_authorization_hash_domain_reset.postgres.sql"
    );
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

    fn table_columns(schema: &str, table: &str) -> Vec<String> {
        let marker = format!("CREATE TABLE IF NOT EXISTS {table} (");
        let start = schema
            .find(&marker)
            .unwrap_or_else(|| panic!("missing {table} in schema"))
            + marker.len();
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
        assert!(error.contains("deterministic retrieval-key data repair"));
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
        assert!(error.contains("deterministic retrieval-key data repair"));

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
    async fn missing_default_avionics_candidate_contract_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            r#"
            DELETE FROM schema_migration_contracts
            WHERE migration_name = '20260802_default_avionics_candidate_quarantine'
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a missing default-candidate contract must fail preflight")
            .to_string();
        assert!(error.contains("isolated from canonical valuation inputs"));
        assert!(error.contains("20260802_default_avionics_candidate_quarantine.sqlite.sql"));
    }

    #[tokio::test]
    async fn missing_default_avionics_admission_guard_fails_migration_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("DROP TRIGGER aircraft_default_avionics_candidate_admission_guard")
            .execute(pool)
            .await
            .unwrap();

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a missing candidate admission guard must fail preflight")
            .to_string();
        assert!(error.contains("isolated from canonical valuation inputs"));
        assert!(error.contains("20260802_default_avionics_candidate_quarantine.sqlite.sql"));
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

    #[tokio::test]
    async fn canonical_and_pending_default_overlap_fails_migration_preflight() {
        let (path, url) = unique_sqlite_test_database("default-avionics-overlap");
        let db = AppDb::connect(&url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER aircraft_model_variant_default_avionics_approved_insert")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER aircraft_default_avionics_candidate_admission_move")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_model_variant_default_avionics_candidates (
              aircraft_model_variant_id, model_year, avionics_model_id,
              quantity, source_url, source_title, source_notes,
              source_confidence
            ) VALUES (
              987001, 2010, 987002, 1, 'https://example.test/default',
              'Pending default', 'Pending source claim', 'high'
            )
            "#,
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_model_variant_default_avionics (
              aircraft_model_variant_id, model_year, avionics_model_id,
              quantity, source_url, source_title, source_notes,
              source_confidence
            ) VALUES (
              987001, 2010, 987002, 1, 'https://example.test/default',
              'Pending default', 'Pending source claim', 'high'
            )
            "#,
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query(
            r#"
            CREATE TRIGGER aircraft_model_variant_default_avionics_approved_insert
            BEFORE INSERT ON aircraft_model_variant_default_avionics
            WHEN NOT EXISTS (
              SELECT 1
              FROM avionics_models model
              WHERE model.id = NEW.avionics_model_id
                AND model.catalog_status = 'approved'
            )
            BEGIN
              SELECT RAISE(
                ABORT,
                'default avionics association requires an approved catalog entry'
              );
            END
            "#,
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query(
            r#"
            CREATE TRIGGER aircraft_default_avionics_candidate_admission_move
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
            END
            "#,
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await
            .unwrap();
        drop(connection);

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("canonical/pending semantic overlap must fail preflight")
            .to_string();
        assert!(error.contains("isolated from canonical valuation inputs"));
        assert!(error.contains("20260802_default_avionics_candidate_quarantine.sqlite.sql"));

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn default_avionics_candidate_table_and_contract_have_backend_parity() {
        let table = "aircraft_model_variant_default_avionics_candidates";
        assert_eq!(
            table_columns(SQLITE_SCHEMA_SQL, table),
            table_columns(POSTGRES_SCHEMA_SQL, table),
            "SQLite/Postgres schema column mismatch for {table}"
        );
        assert_eq!(
            table_columns(DEFAULT_AVIONICS_CANDIDATES_SQLITE_MIGRATION_SQL, table),
            table_columns(DEFAULT_AVIONICS_CANDIDATES_POSTGRES_MIGRATION_SQL, table),
            "SQLite/Postgres migration column mismatch for {table}"
        );
        for contract in [
            DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_MIGRATION,
            DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_CONTRACT_FINGERPRINT,
            "catalog_product_unverified",
            "factory_default_claim_unverified",
            "aircraft_default_avionics_candidate_admission_guard",
            "aircraft_default_avionics_candidate_admission_move",
        ] {
            assert!(SQLITE_SCHEMA_SQL.contains(contract));
            assert!(POSTGRES_SCHEMA_SQL.contains(contract));
            assert!(DEFAULT_AVIONICS_CANDIDATES_SQLITE_MIGRATION_SQL.contains(contract));
            assert!(DEFAULT_AVIONICS_CANDIDATES_POSTGRES_MIGRATION_SQL.contains(contract));
        }
        assert_eq!(DEFAULT_AVIONICS_CANDIDATE_QUARANTINE_CONTRACT_VERSION, 2);
        let postgres_lock =
            "LOCK TABLE avionics_models, aircraft_model_variant_default_avionics,\n  aircraft_model_variant_default_avionics_candidates\n  IN SHARE ROW EXCLUSIVE MODE";
        let lock_position = DEFAULT_AVIONICS_CANDIDATES_POSTGRES_MIGRATION_SQL
            .find(postgres_lock)
            .expect("Postgres migration must take the shared catalog lock order");
        let copy_position = DEFAULT_AVIONICS_CANDIDATES_POSTGRES_MIGRATION_SQL
            .find("INSERT INTO aircraft_model_variant_default_avionics_candidates")
            .expect("Postgres migration must copy pending candidates");
        assert!(lock_position < copy_position);
        assert!(DEFAULT_AVIONICS_CANDIDATES_POSTGRES_MIGRATION_SQL
            .contains("default avionics claim exists in both canonical and pending tables"));
        assert!(DEFAULT_AVIONICS_CANDIDATES_SQLITE_MIGRATION_SQL
            .contains("JOIN aircraft_model_variant_default_avionics_candidates candidate"));
        assert_eq!(
            POSTGRES_SCHEMA_SQL.matches("pg_advisory_xact_lock").count(),
            2,
        );
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
        let table = "aircraft_sale_listing_avionics_authorizations";
        let sqlite_columns = table_columns(SQLITE_SCHEMA_SQL, table);
        assert_eq!(
            sqlite_columns,
            table_columns(POSTGRES_SCHEMA_SQL, table),
            "SQLite/Postgres schema column mismatch for {table}"
        );
        assert_eq!(
            sqlite_columns,
            table_columns(LISTING_AVIONICS_AUTHORIZATIONS_SQLITE_MIGRATION_SQL, table),
            "canonical schema and SQLite upgrade disagree for {table}"
        );
        assert_eq!(
            sqlite_columns,
            table_columns(
                LISTING_AVIONICS_AUTHORIZATIONS_POSTGRES_MIGRATION_SQL,
                table
            ),
            "canonical schema and Postgres upgrade disagree for {table}"
        );
        for definition in [SQLITE_SCHEMA_SQL, POSTGRES_SCHEMA_SQL] {
            assert!(!definition.contains("aircraft_sale_listing_avionics_corroborations"));
            assert!(!definition.contains("aircraft_sale_listing_avionics_corroboration_scopes"));
            assert!(definition.contains(LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_MIGRATION));
            assert!(definition
                .contains(LISTING_AVIONICS_ASSOCIATION_AUTHORIZATIONS_CONTRACT_FINGERPRINT));
            assert!(definition.contains(LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_MIGRATION));
            assert!(definition
                .contains(LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_FINGERPRINT));
        }
        assert_eq!(
            LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_VERSION,
            1
        );
        for migration in [
            LISTING_AVIONICS_AUTHORIZATIONS_SQLITE_MIGRATION_SQL,
            LISTING_AVIONICS_AUTHORIZATIONS_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(migration.contains("'manufacturer_reuse'"));
            assert!(migration.contains("'same_case_grounded'"));
            assert!(migration.contains("DROP TABLE aircraft_sale_listing_avionics_corroborations"));
            assert!(migration
                .contains("DROP TABLE aircraft_sale_listing_avionics_corroboration_scopes"));
            assert!(migration.contains("link.source_confidence = 'high'"));
            assert!(
                migration.contains("corroboration.observation_sha256"),
                "the already-applied transition must remain immutable"
            );
        }
        for migration in [
            LISTING_AVIONICS_AUTHORIZATION_HASH_RESET_SQLITE_MIGRATION_SQL,
            LISTING_AVIONICS_AUTHORIZATION_HASH_RESET_POSTGRES_MIGRATION_SQL,
        ] {
            assert!(migration.contains(LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_MIGRATION));
            assert!(migration
                .contains(LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_FINGERPRINT));
            assert!(migration.contains("DELETE FROM aircraft_sale_listing_avionics_authorizations"));
            assert!(migration.contains("WHERE authorization_kind = 'manufacturer_reuse'"));
            assert!(migration.contains("Listing links and catalog rows"));
        }
        assert!(LISTING_AVIONICS_AUTHORIZATION_HASH_RESET_POSTGRES_MIGRATION_SQL.contains(
            "LOCK TABLE aircraft_sale_listing_avionics_authorizations\nIN SHARE ROW EXCLUSIVE MODE"
        ));
        for definition in [
            SQLITE_SCHEMA_SQL,
            POSTGRES_SCHEMA_SQL,
            LISTING_AVIONICS_AUTHORIZATIONS_SQLITE_MIGRATION_SQL,
            LISTING_AVIONICS_AUTHORIZATIONS_POSTGRES_MIGRATION_SQL,
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

    #[tokio::test]
    async fn sqlite_listing_avionics_authorization_upgrade_is_idempotent_and_integral() {
        let mut connection = SqliteConnection::connect("sqlite::memory:")
            .await
            .expect("SQLite migration fixture should connect");
        sqlx::raw_sql(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE schema_migration_contracts (
              migration_name TEXT PRIMARY KEY,
              contract_version INTEGER NOT NULL,
              contract_fingerprint TEXT NOT NULL,
              installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            INSERT INTO schema_migration_contracts
              (migration_name, contract_version, contract_fingerprint)
            VALUES
              ('20260805_listing_avionics_association_corroborations', 1,
               '2c4661b8bf76e1a28d5ab5c636ed100f5d73f845c44b9515e5f46c5827e66fc9'),
              ('20260806_listing_avionics_collision_closure', 1,
               '363fd039068667cca351c0009c0621e55942186a5d63804cf0e7da8212fa26b3'),
              ('20260807_avionics_product_reuse_v2', 1,
               'efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc');
            CREATE TABLE avionics_models (
              id INTEGER PRIMARY KEY,
              identity_source_url TEXT
            );
            INSERT INTO avionics_models (id, identity_source_url)
            VALUES (7, 'https://www.garmin.com/en-US/aviation/');
            CREATE TABLE avionics_manufacturers (id INTEGER PRIMARY KEY);
            CREATE TABLE avionics_types (
              id INTEGER PRIMARY KEY,
              name TEXT,
              normalized_name TEXT
            );
            CREATE TABLE avionics_model_types (
              avionics_model_id INTEGER NOT NULL,
              avionics_type_id INTEGER NOT NULL
            );
            CREATE TABLE avionics_approved_product_identities (
              avionics_model_id INTEGER PRIMARY KEY,
              avionics_manufacturer_identity_id INTEGER,
              canonical_product_key TEXT,
              manufacturer_identifier_kind TEXT,
              canonical_identifier_key TEXT
            );
            CREATE TABLE avionics_approved_product_graph_identities (
              avionics_model_id INTEGER PRIMARY KEY,
              avionics_manufacturer_identity_id INTEGER
            );
            INSERT INTO avionics_approved_product_graph_identities
              (avionics_model_id, avionics_manufacturer_identity_id)
            VALUES (7, 3);
            CREATE TABLE avionics_manufacturer_effective_identities (
              identity_id INTEGER PRIMARY KEY,
              avionics_manufacturer_identity_id INTEGER NOT NULL
            );
            INSERT INTO avionics_manufacturer_effective_identities
              (identity_id, avionics_manufacturer_identity_id)
            VALUES (3, 3);
            CREATE TABLE avionics_authoritative_source_origins (
              id INTEGER PRIMARY KEY,
              authority_kind TEXT NOT NULL,
              avionics_manufacturer_identity_id INTEGER,
              https_origin TEXT NOT NULL
            );
            INSERT INTO avionics_authoritative_source_origins VALUES (
              5, 'manufacturer_primary', 3, 'https://www.garmin.com'
            );
            CREATE TABLE avionics_authoritative_source_origin_revocations (
              avionics_authoritative_source_origin_id INTEGER PRIMARY KEY
            );
            CREATE TABLE avionics_product_reuse_attestations (
              avionics_model_id INTEGER PRIMARY KEY,
              product_fingerprint TEXT NOT NULL,
              policy_version TEXT NOT NULL
            );
            INSERT INTO avionics_product_reuse_attestations
              (avionics_model_id, product_fingerprint, policy_version)
            VALUES (7,
              'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
              'avionics_reuse_v2');
            CREATE TABLE aircraft_sale_listing_avionics (
              id INTEGER PRIMARY KEY,
              aircraft_sale_listing_id INTEGER NOT NULL,
              avionics_model_id INTEGER NOT NULL,
              quantity INTEGER NOT NULL,
              source_notes TEXT,
              source_confidence TEXT,
              configuration_action TEXT NOT NULL,
              replaces_avionics_model_id INTEGER
            );
            INSERT INTO aircraft_sale_listing_avionics
              (id, aircraft_sale_listing_id, avionics_model_id, quantity,
               source_notes, source_confidence, configuration_action)
            VALUES
              (11, 23, 7, 1, 'Garmin GTX 345', 'high', 'installed'),
              (12, 24, 7, 1, 'Garmin GTX 345', 'medium', 'installed'),
              (13, 25, 7, 1, 'Garmin GTX 345', 'high', 'installed');
            CREATE TABLE plugin_submissions (
              canonical_listing_id INTEGER,
              rendered_html TEXT NOT NULL,
              rendered_html_sha256 TEXT NOT NULL
            );
            INSERT INTO plugin_submissions
              (canonical_listing_id, rendered_html, rendered_html_sha256)
            VALUES
              (23, '<p>Garmin GTX 345</p>',
               'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'),
              (24, '<p>Garmin GTX 345</p>',
               'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'),
              (25, '<p>Garmin GTX 345</p>',
               '7777777777777777777777777777777777777777777777777777777777777777');
            CREATE TABLE aircraft_sale_listing_avionics_corroborations (
              listing_link_id INTEGER NOT NULL,
              association_role TEXT NOT NULL,
              avionics_model_id INTEGER NOT NULL,
              observation_sha256 TEXT NOT NULL,
              product_fingerprint TEXT NOT NULL,
              policy_version TEXT NOT NULL,
              corroborated_at TEXT NOT NULL,
              PRIMARY KEY (listing_link_id, association_role)
            );
            INSERT INTO aircraft_sale_listing_avionics_corroborations VALUES
              (11, 'installed', 7,
               'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
               'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
               'listing_avionics_association_v1', '2026-08-18 12:00:00'),
              (12, 'installed', 7,
               'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
               'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
               'listing_avionics_association_v1', '2026-08-18 12:00:00');
            CREATE TABLE aircraft_sale_listing_avionics_corroboration_scopes (
              listing_link_id INTEGER NOT NULL,
              association_role TEXT NOT NULL,
              collision_closure_sha256 TEXT NOT NULL,
              policy_version TEXT NOT NULL,
              PRIMARY KEY (listing_link_id, association_role)
            );
            INSERT INTO aircraft_sale_listing_avionics_corroboration_scopes VALUES
              (11, 'installed',
               'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
               'listing_avionics_collision_closure_v1'),
              (12, 'installed',
               'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
               'listing_avionics_collision_closure_v1');
            "#,
        )
        .execute(&mut connection)
        .await
        .expect("legacy authorization fixture should initialize");

        for _ in 0..2 {
            sqlx::raw_sql(LISTING_AVIONICS_AUTHORIZATIONS_SQLITE_MIGRATION_SQL)
                .execute(&mut connection)
                .await
                .expect("authorization upgrade should be safely repeatable");
        }

        let migrated: (i64, String, String, String, String) = sqlx::query_as(
            r#"
            SELECT avionics_model_id, authorization_kind, product_fingerprint,
                   evidence_capture_sha256, policy_version
            FROM aircraft_sale_listing_avionics_authorizations
            WHERE listing_link_id = 11 AND association_role = 'installed'
            "#,
        )
        .fetch_one(&mut connection)
        .await
        .expect("the valid predecessor proof should migrate once");
        assert_eq!(migrated.0, 7);
        assert_eq!(migrated.1, "manufacturer_reuse");
        assert_eq!(migrated.2, "a".repeat(64));
        assert_eq!(migrated.3, "b".repeat(64));
        assert_eq!(migrated.4, "listing_avionics_authorization_v1");
        let downgraded_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = 12",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            downgraded_count, 0,
            "a downgraded predecessor link must not acquire authorization"
        );
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics_authorizations (
              listing_link_id, association_role, avionics_model_id,
              authorization_kind, observation_sha256, product_fingerprint,
              grounded_resolution_sha256, evidence_capture_sha256,
              collision_closure_sha256, policy_version
            ) VALUES (
              13, 'installed', 7, 'same_case_grounded',
              '9999999999999999999999999999999999999999999999999999999999999999',
              'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
              '8888888888888888888888888888888888888888888888888888888888888888',
              '7777777777777777777777777777777777777777777777777777777777777777',
              '6666666666666666666666666666666666666666666666666666666666666666',
              'listing_avionics_authorization_v1'
            )
            "#,
        )
        .execute(&mut connection)
        .await
        .expect("a same-case authorization should be admitted before revocation");
        sqlx::query("INSERT INTO avionics_authoritative_source_origin_revocations VALUES (5)")
            .execute(&mut connection)
            .await
            .expect("the exact source origin should be revocable");
        let revoked_same_case_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = 13",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            revoked_same_case_count, 0,
            "revoking the exact product-proof origin must invalidate same-case authorization"
        );
        let old_object_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM sqlite_schema
            WHERE name IN (
              'aircraft_sale_listing_avionics_corroborations',
              'aircraft_sale_listing_avionics_corroboration_scopes'
            )
            "#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(old_object_count, 0);
        let foreign_key_errors: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(foreign_key_errors, 0);
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[tokio::test]
    async fn sqlite_listing_avionics_authorization_hash_reset_is_fail_closed_and_idempotent() {
        let mut connection = SqliteConnection::connect("sqlite::memory:")
            .await
            .expect("SQLite hash-reset fixture should connect");
        sqlx::raw_sql(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE schema_migration_contracts (
              migration_name TEXT PRIMARY KEY,
              contract_version INTEGER NOT NULL,
              contract_fingerprint TEXT NOT NULL,
              installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            INSERT INTO schema_migration_contracts
              (migration_name, contract_version, contract_fingerprint)
            VALUES (
              '20260818_listing_avionics_association_authorizations',
              1,
              'bbb76c8535647f2ecaab3179d5ef483bdef9ca23a0e14e3fd0888912fc3d90f9'
            );
            CREATE TABLE aircraft_sale_listing_avionics (
              id INTEGER PRIMARY KEY
            );
            INSERT INTO aircraft_sale_listing_avionics VALUES (11), (12), (13);
            CREATE TABLE avionics_models (
              id INTEGER PRIMARY KEY
            );
            INSERT INTO avionics_models VALUES (7);
            CREATE TABLE aircraft_sale_listing_avionics_authorizations (
              listing_link_id INTEGER NOT NULL,
              authorization_kind TEXT NOT NULL
            );
            INSERT INTO aircraft_sale_listing_avionics_authorizations VALUES
              (11, 'manufacturer_reuse'),
              (12, 'same_case_grounded');
            "#,
        )
        .execute(&mut connection)
        .await
        .expect("hash-reset fixture should initialize");

        sqlx::raw_sql(LISTING_AVIONICS_AUTHORIZATION_HASH_RESET_SQLITE_MIGRATION_SQL)
            .execute(&mut connection)
            .await
            .expect("hash reset should invalidate predecessor-derived receipts");

        let retained_after_reset: Vec<(i64, String)> = sqlx::query_as(
            "SELECT listing_link_id, authorization_kind \
             FROM aircraft_sale_listing_avionics_authorizations ORDER BY listing_link_id",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            retained_after_reset,
            vec![(12, "same_case_grounded".to_string())]
        );
        let retained_links: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aircraft_sale_listing_avionics")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        let retained_models: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(retained_links, 3, "listing links are not receipts");
        assert_eq!(retained_models, 1, "catalog products are not receipts");

        sqlx::query("INSERT INTO aircraft_sale_listing_avionics_authorizations VALUES (?, ?)")
            .bind(13_i64)
            .bind("manufacturer_reuse")
            .execute(&mut connection)
            .await
            .expect("current workflow should be able to issue a new receipt");
        sqlx::raw_sql(LISTING_AVIONICS_AUTHORIZATION_HASH_RESET_SQLITE_MIGRATION_SQL)
            .execute(&mut connection)
            .await
            .expect("verified migration reapplication should be a no-op");

        let retained_after_reapply: Vec<(i64, String)> = sqlx::query_as(
            "SELECT listing_link_id, authorization_kind \
             FROM aircraft_sale_listing_avionics_authorizations ORDER BY listing_link_id",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            retained_after_reapply,
            vec![
                (12, "same_case_grounded".to_string()),
                (13, "manufacturer_reuse".to_string()),
            ]
        );
        let reset_contract: (i64, String) = sqlx::query_as(
            "SELECT contract_version, contract_fingerprint \
             FROM schema_migration_contracts WHERE migration_name = ?",
        )
        .bind(LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_MIGRATION)
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(reset_contract.0, 1);
        assert_eq!(
            reset_contract.1,
            LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_CONTRACT_FINGERPRINT
        );
    }

    #[tokio::test]
    async fn stale_manufacturer_reuse_receipt_requires_hash_reset_before_startup() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let validation_trigger_sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema \
             WHERE type = 'trigger' \
               AND name = 'listing_avionics_authorizations_validate_insert'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER listing_avionics_authorizations_validate_insert")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics_authorizations (
              listing_link_id, association_role, avionics_model_id,
              authorization_kind, observation_sha256, product_fingerprint,
              grounded_resolution_sha256, evidence_capture_sha256,
              collision_closure_sha256, policy_version
            ) VALUES (?, 'installed', ?, 'manufacturer_reuse', ?, ?, NULL, ?, ?, ?)
            "#,
        )
        .bind(999_i64)
        .bind(999_i64)
        .bind("a".repeat(64))
        .bind("b".repeat(64))
        .bind("c".repeat(64))
        .bind("d".repeat(64))
        .bind("listing_avionics_authorization_v1")
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::raw_sql(&validation_trigger_sql)
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("DELETE FROM schema_migration_contracts WHERE migration_name = ?")
            .bind(LISTING_AVIONICS_AUTHORIZATION_HASH_DOMAIN_RESET_MIGRATION)
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await
            .unwrap();
        drop(connection);

        let error = db
            .ensure_required_migrations()
            .await
            .expect_err("a predecessor receipt without the reset contract must fail startup")
            .to_string();
        assert!(error.contains("incompatible derived manufacturer-reuse receipts"));
        assert!(
            error.contains("20260818_listing_avionics_authorization_hash_domain_reset.sqlite.sql")
        );

        sqlx::raw_sql(LISTING_AVIONICS_AUTHORIZATION_HASH_RESET_SQLITE_MIGRATION_SQL)
            .execute(pool)
            .await
            .expect("the explicit reset migration should invalidate the stale receipt");
        db.ensure_required_migrations()
            .await
            .expect("startup should pass after the reset contract is installed");
        let stale_receipts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations \
             WHERE authorization_kind = 'manufacturer_reuse'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stale_receipts, 0);
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
    }
}
