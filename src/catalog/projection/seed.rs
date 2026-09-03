//! Transactional writer for one authenticated current catalog projection.
//!
//! The writer has no retrieval or provider surface. It accepts only the opaque
//! projection selected and fingerprinted by [`super::current`], and it refuses
//! every target that is not a freshly initialized replay database containing
//! only schema bootstrap rows and optional immutable signed captures.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use sqlx::{Connection, PgConnection, SqliteConnection};

use super::current::CurrentCatalogProjection;
use super::{canonical_row, primary_key_predicate, quoted_identifier, ProjectionRow};
use crate::db::{
    canonical_startup_migration_contract_receipts, AppDb, DatabaseBackend, DatabaseKind,
    DEVELOPER_EMAIL,
};
use crate::listing::replay::{authenticate_retained_capture, RetainedCaptureAuthentication};

const POSTGRES_SEED_ADVISORY_LOCK_KEY: i64 = 0x0041_4952_5345_4544;

const CLEAN_TARGET_NONEMPTY_TABLES: &[&str] = &[
    "schema_migration_contracts",
    "users",
    "plugin_installs",
    "plugin_submissions",
    "listing_replay_submission_inventory_lock",
    "aircraft_markets",
    "aircraft_manufacturers",
    "aircraft_models",
    "aircraft_model_variants",
    "aircraft_sale_listing_pending_compatibility_placeholder",
];

const GENERATED_TABLES: &[&str] = &[
    "avionics_manufacturer_canonical_keys",
    "avionics_approved_product_identities",
];

const POST_APPROVAL_TABLES: &[&str] = &[
    "avionics_suite_components",
    "avionics_product_reuse_attestations",
];

// This is deliberately owned by the writer. Projection rows are stored in
// canonical order for deterministic equality and fingerprinting; dependency
// order is an insertion concern and must never depend on loader fetch order.
const MATERIALIZATION_TABLES: &[&str] = &[
    "curation_evidence_sources",
    "curation_evidence_claims",
    "faa_registry_snapshots",
    "faa_registry_aircraft",
    "faa_registry_aircraft_references",
    "faa_registry_engine_references",
    "faa_registry_coverage",
    "aircraft_identity_observations",
    "aircraft_identity_resolution_cases",
    "aircraft_identity_decisions",
    "aircraft_identity_decision_claims",
    "aircraft_markets",
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
    "aircraft_engine_catalog_models",
    "aircraft_propeller_catalog_models",
    "aircraft_serial_number_schemes",
    "aircraft_feature_definitions",
    "aircraft_tcds_make_lineage_bindings",
    "aircraft_designation_faa_bindings",
    "avionics_manufacturers",
    "avionics_manufacturer_canonical_keys",
    "avionics_manufacturer_identities",
    "avionics_manufacturer_identity_memberships",
    "avionics_authoritative_source_origins",
    "avionics_types",
    "avionics_models",
    "avionics_approved_product_identities",
    "avionics_model_types",
    "avionics_suite_components",
    "avionics_product_reuse_attestations",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CatalogSeedReport {
    pub dry_run: bool,
    pub provider_calls: usize,
    pub projection_fingerprint_sha256: String,
    pub projection_table_counts: BTreeMap<String, usize>,
    pub required_user_count: usize,
    pub retained_capture_count: usize,
    pub materialized_rows: usize,
}

enum SeedConnection<'connection> {
    Sqlite(&'connection mut SqliteConnection),
    Postgres(&'connection mut PgConnection),
}

#[derive(sqlx::FromRow)]
struct RetainedCaptureRow {
    submission_id: i64,
    submission_user_id: i64,
    plugin_install_id: i64,
    plugin_install_user_id: i64,
    plugin_public_key_base64: String,
    source_url: String,
    rendered_html: String,
    rendered_html_sha256: String,
    signature_base64: String,
    timestamp_chronology_valid: i64,
}

pub(crate) struct PreparedCatalogSeed {
    projection: CurrentCatalogProjection,
    apply: bool,
}

pub(crate) async fn prepare_verified_catalog(
    source: &AppDb,
    expected_fingerprint_sha256: Option<&str>,
    apply: bool,
) -> Result<PreparedCatalogSeed> {
    if apply && expected_fingerprint_sha256.is_none() {
        bail!("catalog seed --apply requires one reviewed catalog fingerprint");
    }
    if let Some(expected_fingerprint_sha256) = expected_fingerprint_sha256 {
        require_lower_sha256(expected_fingerprint_sha256)?;
    }
    let projection = CurrentCatalogProjection::load(source).await?;
    if let Some(expected_fingerprint_sha256) = expected_fingerprint_sha256 {
        if projection.fingerprint_sha256() != expected_fingerprint_sha256 {
            bail!(
                "source catalog fingerprint {} differs from required fingerprint {}",
                projection.fingerprint_sha256(),
                expected_fingerprint_sha256
            );
        }
    }

    Ok(PreparedCatalogSeed { projection, apply })
}

pub(crate) async fn seed_prepared_verified_catalog(
    prepared: &PreparedCatalogSeed,
    target: &AppDb,
) -> Result<CatalogSeedReport> {
    let retained_capture_count = if prepared.apply {
        materialize(target, &prepared.projection).await?
    } else {
        inspect_clean_target(target, &prepared.projection).await?
    };

    if prepared.apply {
        prepared.projection.require_reloaded_match(target).await?;
    }

    let summary = prepared.projection.summary();
    Ok(CatalogSeedReport {
        dry_run: !prepared.apply,
        provider_calls: 0,
        projection_fingerprint_sha256: summary.fingerprint_sha256.clone(),
        projection_table_counts: summary.table_counts.clone(),
        required_user_count: summary.required_users.len(),
        retained_capture_count,
        materialized_rows: if prepared.apply {
            summary.table_counts.values().sum()
        } else {
            0
        },
    })
}

#[cfg(test)]
async fn seed_verified_catalog(
    source: &AppDb,
    target: &AppDb,
    expected_fingerprint_sha256: Option<&str>,
    apply: bool,
) -> Result<CatalogSeedReport> {
    let prepared = prepare_verified_catalog(source, expected_fingerprint_sha256, apply).await?;
    seed_prepared_verified_catalog(&prepared, target).await
}

async fn inspect_clean_target(
    target: &AppDb,
    projection: &CurrentCatalogProjection,
) -> Result<usize> {
    match target.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut connection = pool.acquire().await?;
            let mut target = SeedConnection::Sqlite(&mut connection);
            validate_clean_target(&mut target, projection).await
        }
        DatabaseBackend::Postgres(pool) => {
            let mut connection = pool.acquire().await?;
            let mut target = SeedConnection::Postgres(&mut connection);
            validate_clean_target(&mut target, projection).await
        }
    }
}

async fn materialize(target: &AppDb, projection: &CurrentCatalogProjection) -> Result<usize> {
    match target.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut connection = pool.acquire().await?;
            let mut transaction = connection
                .begin_with("BEGIN IMMEDIATE")
                .await
                .context("could not serialize SQLite catalog seed")?;
            let result = async {
                let mut target = SeedConnection::Sqlite(&mut transaction);
                let retained = validate_clean_target(&mut target, projection).await?;
                insert_projection(&mut target, projection).await?;
                reset_sequences(&mut target).await?;
                require_integrity(&mut target).await?;
                require_transaction_projection_match(&mut target, projection).await?;
                Ok::<_, anyhow::Error>(retained)
            }
            .await;
            match result {
                Ok(retained) => {
                    transaction.commit().await?;
                    Ok(retained)
                }
                Err(error) => {
                    transaction.rollback().await?;
                    Err(error)
                }
            }
        }
        DatabaseBackend::Postgres(pool) => {
            let mut connection = pool.acquire().await?;
            let mut transaction = connection
                .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
                .await
                .context("could not begin PostgreSQL catalog seed")?;
            let result = async {
                sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
                    .bind(POSTGRES_SEED_ADVISORY_LOCK_KEY)
                    .execute(&mut *transaction)
                    .await
                    .context("could not serialize PostgreSQL catalog seed")?;
                let before = postgres_base_tables(&mut transaction).await?;
                lock_postgres_tables(&mut transaction, &before).await?;
                let after = postgres_base_tables(&mut transaction).await?;
                if before != after {
                    bail!("PostgreSQL base-table inventory changed while acquiring seed locks");
                }
                let mut target = SeedConnection::Postgres(&mut transaction);
                let retained = validate_clean_target_with_tables(&mut target, projection, before)
                    .await
                    .context("PostgreSQL catalog seed clean-target validation failed")?;
                insert_projection(&mut target, projection)
                    .await
                    .context("PostgreSQL catalog seed row materialization failed")?;
                reset_sequences(&mut target)
                    .await
                    .context("PostgreSQL catalog seed sequence reset failed")?;
                require_integrity(&mut target)
                    .await
                    .context("PostgreSQL catalog seed integrity check failed")?;
                require_transaction_projection_match(&mut target, projection)
                    .await
                    .context("PostgreSQL catalog seed in-transaction parity check failed")?;
                Ok::<_, anyhow::Error>(retained)
            }
            .await;
            match result {
                Ok(retained) => {
                    transaction.commit().await?;
                    Ok(retained)
                }
                Err(error) => {
                    transaction.rollback().await?;
                    Err(error)
                }
            }
        }
    }
}

async fn validate_clean_target(
    target: &mut SeedConnection<'_>,
    projection: &CurrentCatalogProjection,
) -> Result<usize> {
    let tables = base_tables(target).await?;
    validate_clean_target_with_tables(target, projection, tables).await
}

async fn validate_clean_target_with_tables(
    target: &mut SeedConnection<'_>,
    projection: &CurrentCatalogProjection,
    tables: Vec<String>,
) -> Result<usize> {
    let admitted = CLEAN_TARGET_NONEMPTY_TABLES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    for table in &tables {
        let count = count_rows(target, table).await?;
        if count != 0 && !admitted.contains(table.as_str()) {
            bail!(
                "clean catalog target has {count} preexisting rows in non-bootstrap table {table}"
            );
        }
    }
    for required in CLEAN_TARGET_NONEMPTY_TABLES {
        if !tables.iter().any(|table| table == required) {
            bail!("clean catalog target is missing required base table {required}");
        }
    }

    validate_schema_bootstrap(target).await?;
    validate_capture_domain(target, projection).await
}

async fn validate_schema_bootstrap(target: &mut SeedConnection<'_>) -> Result<()> {
    validate_schema_migration_contracts(target).await?;
    require_exact_count(
        target,
        "aircraft_markets",
        r#"SELECT COUNT(*) FROM aircraft_markets market
           WHERE (market.code = 'GLOBAL' AND market.name = 'Global'
                  AND market.parent_market_id IS NULL)
              OR (market.code = 'US' AND market.name = 'United States'
                  AND market.parent_market_id = (
                    SELECT global.id FROM aircraft_markets global WHERE global.code = 'GLOBAL'
                  ))"#,
        2,
    )
    .await?;
    require_exact_count(
        target,
        "aircraft_manufacturers",
        "SELECT COUNT(*) FROM aircraft_manufacturers WHERE id = -1 AND name = 'Pending FAA curation' AND normalized_name = '__aircost_pending_faa_make__'",
        1,
    )
    .await?;
    require_exact_count(
        target,
        "aircraft_models",
        "SELECT COUNT(*) FROM aircraft_models WHERE id = -1 AND aircraft_manufacturer_id = -1 AND name = 'Pending FAA curation' AND normalized_name = '__aircost_pending_faa_family__'",
        1,
    )
    .await?;
    require_exact_count(
        target,
        "aircraft_model_variants",
        "SELECT COUNT(*) FROM aircraft_model_variants WHERE id = -1 AND aircraft_model_id = -1 AND name = 'Pending FAA curation' AND normalized_name = '__aircost_pending_faa_identity__'",
        1,
    )
    .await?;
    require_exact_count(
        target,
        "aircraft_sale_listing_pending_compatibility_placeholder",
        "SELECT COUNT(*) FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1 AND aircraft_manufacturer_id = -1 AND aircraft_model_id = -1 AND aircraft_model_variant_id = -1",
        1,
    )
    .await?;
    Ok(())
}

async fn validate_schema_migration_contracts(target: &mut SeedConnection<'_>) -> Result<()> {
    let (kind, mut actual): (DatabaseKind, Vec<(String, i64, String, String)>) = match target {
        SeedConnection::Sqlite(connection) => (
            DatabaseKind::Sqlite,
            sqlx::query_as(
                "SELECT migration_name, contract_version, contract_fingerprint, installed_at \
                 FROM schema_migration_contracts ORDER BY migration_name",
            )
            .fetch_all(&mut **connection)
            .await?,
        ),
        SeedConnection::Postgres(connection) => (
            DatabaseKind::Postgres,
            sqlx::query_as(
                "SELECT migration_name, contract_version::BIGINT, contract_fingerprint, installed_at \
                 FROM ONLY public.schema_migration_contracts ORDER BY migration_name",
            )
            .fetch_all(&mut **connection)
            .await?,
        ),
    };
    actual.sort_by(|left, right| left.0.cmp(&right.0));
    let expected = canonical_startup_migration_contract_receipts(kind);
    let actual_contracts = actual
        .iter()
        .map(|(name, version, fingerprint, _)| (name.as_str(), *version, fingerprint.as_str()))
        .collect::<Vec<_>>();
    let expected_contracts = expected
        .iter()
        .map(|receipt| {
            (
                receipt.migration_name,
                receipt.contract_version,
                receipt.contract_fingerprint,
            )
        })
        .collect::<Vec<_>>();
    if actual_contracts != expected_contracts {
        bail!("clean catalog target has non-canonical schema migration receipts");
    }
    let invalid_installed_at = match target {
        SeedConnection::Sqlite(connection) => {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM schema_migration_contracts \
             WHERE julianday(installed_at) IS NULL",
            )
            .fetch_one(&mut **connection)
            .await?
        }
        SeedConnection::Postgres(connection) => {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM ONLY public.schema_migration_contracts \
             WHERE CAST(installed_at AS TIMESTAMPTZ) IS NULL",
            )
            .fetch_one(&mut **connection)
            .await?
        }
    };
    if invalid_installed_at != 0 {
        bail!("clean catalog target has invalid schema migration receipt timestamps");
    }
    Ok(())
}

async fn require_exact_count(
    target: &mut SeedConnection<'_>,
    table: &str,
    exact_sql: &str,
    expected: i64,
) -> Result<()> {
    let total = count_rows(target, table).await?;
    let exact = query_i64(target, exact_sql).await?;
    if total != expected || exact != expected {
        bail!("clean catalog target has non-canonical bootstrap rows in {table}");
    }
    Ok(())
}

async fn validate_capture_domain(
    target: &mut SeedConnection<'_>,
    projection: &CurrentCatalogProjection,
) -> Result<usize> {
    let developer_count = query_i64(
        target,
        &format!(
            "SELECT COUNT(*) FROM users WHERE email = {} AND display_name = 'Developer' \
             AND auth_provider = 'local' AND auth_subject = 'developer'",
            sql_literal(&Value::String(DEVELOPER_EMAIL.to_string()))?
        ),
    )
    .await?;
    if developer_count != 1 {
        bail!("clean catalog target lacks the exact startup developer user");
    }
    for user in &projection.summary().required_users {
        let sql = format!(
            "SELECT COUNT(*) FROM users WHERE id = {} AND email = {} AND display_name = {} \
             AND auth_provider = {} AND auth_subject = {}",
            user.id,
            sql_literal(&Value::String(user.email.clone()))?,
            sql_literal(&Value::String(user.display_name.clone()))?,
            sql_literal(&Value::String(user.auth_provider.clone()))?,
            sql_literal(&Value::String(user.auth_subject.clone()))?,
        );
        if query_i64(target, &sql).await? != 1 {
            bail!(
                "required catalog reviewer user {} is absent or differs",
                user.id
            );
        }
    }
    let derived_captures = query_i64(
        target,
        "SELECT COUNT(*) FROM plugin_submissions WHERE extracted_listing_json IS NOT NULL OR extraction_error IS NOT NULL OR canonical_listing_id IS NOT NULL",
    )
    .await?;
    if derived_captures != 0 {
        bail!("clean catalog target contains derived capture state");
    }
    validate_retained_captures(target).await?;

    let required_ids = projection
        .summary()
        .required_users
        .iter()
        .map(|user| user.id.to_string())
        .collect::<Vec<_>>();
    let required_predicate = if required_ids.is_empty() {
        "FALSE".to_string()
    } else {
        format!("user_row.id IN ({})", required_ids.join(","))
    };
    let unowned_users = query_i64(
        target,
        &format!(
            "SELECT COUNT(*) FROM users user_row WHERE user_row.email <> {} \
             AND NOT ({required_predicate}) \
             AND NOT EXISTS (SELECT 1 FROM plugin_installs install WHERE install.user_id = user_row.id) \
             AND NOT EXISTS (SELECT 1 FROM plugin_submissions submission WHERE submission.user_id = user_row.id)",
            sql_literal(&Value::String(DEVELOPER_EMAIL.to_string()))?
        ),
    )
    .await?;
    if unowned_users != 0 {
        bail!("clean catalog target contains users unrelated to captures or catalog approval");
    }
    let orphan_installs = query_i64(
        target,
        "SELECT COUNT(*) FROM plugin_installs install WHERE NOT EXISTS (SELECT 1 FROM plugin_submissions submission WHERE submission.plugin_install_id = install.id AND submission.user_id = install.user_id)",
    )
    .await?;
    if orphan_installs != 0 {
        bail!("clean catalog target contains plugin installs without retained captures");
    }
    require_exact_count(
        target,
        "listing_replay_submission_inventory_lock",
        r#"SELECT COUNT(*)
           FROM listing_replay_submission_inventory_lock inventory
           WHERE inventory.singleton_id = 1
             AND inventory.active_run_id IS NULL
             AND inventory.concurrency_token >= (
               SELECT COUNT(*) FROM plugin_submissions
             )
             AND (
               (SELECT COUNT(*) FROM plugin_submissions) <> 0
               OR inventory.concurrency_token = 0
             )"#,
        1,
    )
    .await?;
    Ok(count_rows(target, "plugin_submissions").await? as usize)
}

async fn validate_retained_captures(target: &mut SeedConnection<'_>) -> Result<()> {
    let rows: Vec<RetainedCaptureRow> = match target {
        SeedConnection::Sqlite(connection) => {
            sqlx::query_as(
                r#"SELECT submission.id AS submission_id,
                      submission.user_id AS submission_user_id,
                      install.id AS plugin_install_id,
                      install.user_id AS plugin_install_user_id,
                      install.public_key_base64 AS plugin_public_key_base64,
                      submission.source_url,
                      submission.rendered_html,
                      submission.rendered_html_sha256,
                      submission.signature_base64,
                      CASE WHEN julianday(install.created_at) IS NOT NULL
                             AND julianday(submission.submitted_at) IS NOT NULL
                             AND julianday(install.created_at)
                               <= julianday(submission.submitted_at)
                             AND (install.revoked_at IS NULL OR (
                               julianday(install.revoked_at) IS NOT NULL
                               AND julianday(submission.submitted_at)
                                 <= julianday(install.revoked_at)
                             ))
                           THEN 1 ELSE 0 END AS timestamp_chronology_valid
               FROM plugin_submissions submission
               JOIN plugin_installs install ON install.id = submission.plugin_install_id
               ORDER BY submission.id"#,
            )
            .fetch_all(&mut **connection)
            .await?
        }
        SeedConnection::Postgres(connection) => {
            sqlx::query_as(
                r#"SELECT submission.id AS submission_id,
                      submission.user_id AS submission_user_id,
                      install.id AS plugin_install_id,
                      install.user_id AS plugin_install_user_id,
                      install.public_key_base64 AS plugin_public_key_base64,
                      submission.source_url,
                      submission.rendered_html,
                      submission.rendered_html_sha256,
                      submission.signature_base64,
                      CASE WHEN CAST(install.created_at AS TIMESTAMPTZ) IS NOT NULL
                             AND CAST(submission.submitted_at AS TIMESTAMPTZ) IS NOT NULL
                             AND CAST(install.created_at AS TIMESTAMPTZ)
                               <= CAST(submission.submitted_at AS TIMESTAMPTZ)
                             AND (install.revoked_at IS NULL
                               OR CAST(submission.submitted_at AS TIMESTAMPTZ)
                                  <= CAST(install.revoked_at AS TIMESTAMPTZ))
                           THEN 1::BIGINT ELSE 0::BIGINT END
                           AS timestamp_chronology_valid
               FROM ONLY public.plugin_submissions submission
               JOIN ONLY public.plugin_installs install
                 ON install.id = submission.plugin_install_id
               ORDER BY submission.id"#,
            )
            .fetch_all(&mut **connection)
            .await?
        }
    };
    for row in rows {
        authenticate_retained_capture(RetainedCaptureAuthentication {
            submission_id: row.submission_id,
            submission_user_id: row.submission_user_id,
            plugin_install_id: row.plugin_install_id,
            plugin_install_user_id: row.plugin_install_user_id,
            plugin_public_key_base64: &row.plugin_public_key_base64,
            source_url: &row.source_url,
            rendered_html: &row.rendered_html,
            rendered_html_sha256: &row.rendered_html_sha256,
            signature_base64: &row.signature_base64,
            timestamp_chronology_valid: row.timestamp_chronology_valid == 1,
        })
        .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

async fn insert_projection(
    target: &mut SeedConnection<'_>,
    projection: &CurrentCatalogProjection,
) -> Result<()> {
    let mut approved_models = Vec::new();
    let mut generated_products = Vec::new();
    let mut post_approval = Vec::new();
    let mut materialized_rows = 0usize;

    for table in MATERIALIZATION_TABLES {
        for row in projection.rows().iter().filter(|row| row.table == *table) {
            materialized_rows += 1;
            match row.table.as_str() {
                "aircraft_markets" if row_exists(target, row).await? => {
                    require_existing_row(target, row).await?
                }
                "aircraft_markets" => insert_row(target, row).await?,
                "avionics_manufacturer_canonical_keys" => require_existing_row(target, row).await?,
                "avionics_approved_product_identities" => generated_products.push(row),
                "avionics_models" => {
                    let mut staged = row.clone();
                    staged.set("catalog_status", Value::String("unreviewed".into()))?;
                    staged.set("verification_method", Value::Null)?;
                    staged.set("verified_by_user_id", Value::Null)?;
                    staged.set("structure_verified_by_user_id", Value::Null)?;
                    staged.set("structure_reviewed_at", Value::Null)?;
                    insert_row(target, &staged).await?;
                    approved_models.push(row.clone());
                }
                table if POST_APPROVAL_TABLES.contains(&table) => post_approval.push(row),
                "avionics_authoritative_source_origins" if row_exists(target, row).await? => {
                    require_existing_row(target, row).await?
                }
                table if GENERATED_TABLES.contains(&table) => {
                    unreachable!("all generated projection tables are handled explicitly: {table}")
                }
                _ => insert_row(target, row).await?,
            }
        }
    }
    if materialized_rows != projection.rows().len() {
        let unknown = projection
            .rows()
            .iter()
            .map(|row| row.table.as_str())
            .filter(|table| !MATERIALIZATION_TABLES.contains(table))
            .collect::<BTreeSet<_>>();
        bail!("current projection contains unphased materialization tables: {unknown:?}");
    }

    for model in &approved_models {
        let model_id = model.integer("id")?;
        let verification_method = model
            .value("verification_method")
            .context("approved avionics projection is missing verification_method")?;
        let verified_by_user_id = model
            .value("verified_by_user_id")
            .context("approved avionics projection is missing verified_by_user_id")?;
        let changed = execute_sql(
            target,
            &format!(
                "UPDATE avionics_models \
                 SET catalog_status = 'approved', \
                     verification_method = {}, \
                     verified_by_user_id = {} \
                 WHERE id = {model_id} AND catalog_status = 'unreviewed'",
                sql_literal(verification_method)?,
                sql_literal(verified_by_user_id)?,
            ),
        )
        .await?;
        if changed != 1 {
            bail!("could not promote staged avionics model {model_id}");
        }
    }
    for row in generated_products {
        require_existing_row(target, row).await?;
    }
    for row in post_approval {
        insert_row(target, row).await?;
    }
    for model in &approved_models {
        let structure_verified_by_user_id = model
            .value("structure_verified_by_user_id")
            .context("approved avionics projection is missing structure_verified_by_user_id")?;
        let structure_reviewed_at = model
            .value("structure_reviewed_at")
            .context("approved avionics projection is missing structure_reviewed_at")?;
        if structure_verified_by_user_id.is_null() && structure_reviewed_at.is_null() {
            continue;
        }
        let model_id = model.integer("id")?;
        let changed = execute_sql(
            target,
            &format!(
                "UPDATE avionics_models \
                 SET structure_verified_by_user_id = {}, \
                     structure_reviewed_at = {} \
                 WHERE id = {model_id} AND catalog_status = 'approved'",
                sql_literal(structure_verified_by_user_id)?,
                sql_literal(structure_reviewed_at)?,
            ),
        )
        .await?;
        if changed != 1 {
            bail!("could not restore reviewed avionics structure for model {model_id}");
        }
    }
    Ok(())
}

async fn row_exists(target: &mut SeedConnection<'_>, row: &ProjectionRow) -> Result<bool> {
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE {}",
        quoted_identifier(&row.table),
        primary_key_predicate(row)?
    );
    Ok(query_i64(target, &sql).await? != 0)
}

async fn require_existing_row(target: &mut SeedConnection<'_>, row: &ProjectionRow) -> Result<()> {
    let equality = row
        .columns
        .iter()
        .zip(&row.values)
        .map(|(column, value)| {
            Ok(format!(
                "{} IS NOT DISTINCT FROM {}",
                quoted_identifier(column),
                sql_literal(value)?
            ))
        })
        .collect::<Result<Vec<_>>>()?
        .join(" AND ");
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE ({}) AND ({equality})",
        quoted_identifier(&row.table),
        primary_key_predicate(row)?
    );
    if query_i64(target, &sql).await? != 1 {
        bail!(
            "schema-generated or bootstrap {} row differs from projection: {}",
            row.table,
            canonical_row(row)
        );
    }
    Ok(())
}

async fn insert_row(target: &mut SeedConnection<'_>, row: &ProjectionRow) -> Result<()> {
    let mut columns = row
        .columns
        .iter()
        .map(|column| quoted_identifier(column))
        .collect::<Vec<_>>();
    let mut values = row
        .values
        .iter()
        .map(sql_literal)
        .collect::<Result<Vec<_>>>()?;
    // FAA evidence retrieval time is intentionally excluded from the portable
    // projection because this seed is a new local materialization. The column
    // remains mandatory in the operational schema, so generate it here exactly
    // as the normal FAA importer does.
    if row.table == "curation_evidence_sources"
        && !row.columns.iter().any(|column| column == "retrieved_at")
    {
        columns.push(quoted_identifier("retrieved_at"));
        values.push("CURRENT_TIMESTAMP".into());
    }
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quoted_identifier(&row.table),
        columns.join(", "),
        values.join(", ")
    );
    execute_sql(target, &sql)
        .await
        .with_context(|| format!("could not seed {} row {}", row.table, canonical_row(row)))?;
    Ok(())
}

fn sql_literal(value: &Value) -> Result<String> {
    match value {
        Value::Null => Ok("NULL".into()),
        Value::Bool(true) => Ok("TRUE".into()),
        Value::Bool(false) => Ok("FALSE".into()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(format!("'{}'", value.replace('\'', "''"))),
        Value::Array(_) | Value::Object(_) => Ok(format!(
            "'{}'",
            serde_json::to_string(value)?.replace('\'', "''")
        )),
    }
}

async fn reset_sequences(target: &mut SeedConnection<'_>) -> Result<()> {
    match target {
        SeedConnection::Sqlite(connection) => {
            let names: Vec<String> =
                sqlx::query_scalar("SELECT name FROM sqlite_sequence ORDER BY name")
                    .fetch_all(&mut **connection)
                    .await?;
            for name in names {
                let sql = format!(
                    "UPDATE sqlite_sequence SET seq = (SELECT COALESCE(MAX(id), 0) FROM {}) \
                     WHERE name = {}",
                    quoted_identifier(&name),
                    sql_literal(&Value::String(name))?
                );
                sqlx::query(&sql).execute(&mut **connection).await?;
            }
        }
        SeedConnection::Postgres(connection) => {
            let sequences: Vec<(String, String, String)> = sqlx::query_as(
                r#"SELECT table_relation.relname,
                          sequence_namespace.nspname,
                          sequence_relation.relname
                   FROM pg_catalog.pg_class table_relation
                   JOIN pg_catalog.pg_namespace table_namespace
                     ON table_namespace.oid = table_relation.relnamespace
                   JOIN pg_catalog.pg_attribute id_column
                     ON id_column.attrelid = table_relation.oid
                    AND id_column.attname = 'id'
                    AND id_column.attnum > 0
                    AND NOT id_column.attisdropped
                   JOIN pg_catalog.pg_depend ownership
                     ON ownership.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass
                    AND ownership.refobjid = table_relation.oid
                    AND ownership.refobjsubid = id_column.attnum
                    AND ownership.classid = 'pg_catalog.pg_class'::pg_catalog.regclass
                    AND ownership.deptype IN ('a', 'i')
                   JOIN pg_catalog.pg_class sequence_relation
                     ON sequence_relation.oid = ownership.objid
                    AND sequence_relation.relkind = 'S'
                   JOIN pg_catalog.pg_namespace sequence_namespace
                     ON sequence_namespace.oid = sequence_relation.relnamespace
                   WHERE table_namespace.nspname = 'public'
                     AND table_relation.relkind IN ('r', 'p')
                   ORDER BY table_relation.relname"#,
            )
            .fetch_all(&mut **connection)
            .await
            .context("could not discover PostgreSQL owned id sequences")?;
            for (table, sequence_namespace, sequence_name) in sequences {
                let next = sqlx::query_scalar::<_, i64>(&format!(
                    "SELECT GREATEST(COALESCE(MAX(id), 0) + 1, 1) FROM public.{}",
                    quoted_identifier(&table)
                ))
                .fetch_one(&mut **connection)
                .await
                .with_context(|| format!("could not inspect PostgreSQL sequence table {table}"))?;
                if sequence_namespace != "public" {
                    bail!(
                        "id sequence {sequence_namespace}.{sequence_name} is outside public schema"
                    );
                }
                sqlx::query(&format!(
                    "ALTER SEQUENCE public.{} RESTART WITH {next}",
                    quoted_identifier(&sequence_name)
                ))
                .execute(&mut **connection)
                .await
                .with_context(|| {
                    format!(
                        "could not restart PostgreSQL sequence \
                         {sequence_namespace}.{sequence_name} for {table}"
                    )
                })?;
            }
        }
    }
    Ok(())
}

async fn require_integrity(target: &mut SeedConnection<'_>) -> Result<()> {
    match target {
        SeedConnection::Sqlite(connection) => {
            let violations: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
                    .fetch_one(&mut **connection)
                    .await?;
            if violations != 0 {
                bail!("catalog seed produced {violations} SQLite foreign-key violations");
            }
        }
        SeedConnection::Postgres(connection) => {
            sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
                .execute(&mut **connection)
                .await?;
        }
    }
    Ok(())
}

async fn require_transaction_projection_match(
    target: &mut SeedConnection<'_>,
    projection: &CurrentCatalogProjection,
) -> Result<()> {
    let reloaded = match target {
        SeedConnection::Sqlite(connection) => {
            CurrentCatalogProjection::load_sqlite_connection(&mut **connection).await?
        }
        SeedConnection::Postgres(connection) => {
            CurrentCatalogProjection::load_postgres_connection(&mut **connection).await?
        }
    };
    projection.require_exact_match(reloaded, "in-transaction materialized")
}

async fn base_tables(target: &mut SeedConnection<'_>) -> Result<Vec<String>> {
    match target {
        SeedConnection::Sqlite(connection) => Ok(sqlx::query_scalar(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&mut **connection)
        .await?),
        SeedConnection::Postgres(connection) => postgres_base_tables(&mut **connection).await,
    }
}

async fn postgres_base_tables(connection: &mut PgConnection) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        r#"SELECT relation.relname
           FROM pg_catalog.pg_class relation
           JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace
           WHERE namespace.nspname = 'public' AND relation.relkind IN ('r', 'p')
           ORDER BY relation.relname"#,
    )
    .fetch_all(&mut *connection)
    .await?)
}

async fn lock_postgres_tables(connection: &mut PgConnection, tables: &[String]) -> Result<()> {
    if tables.is_empty() {
        bail!("PostgreSQL catalog seed target has no public base tables");
    }
    let relations = tables
        .iter()
        .map(|table| format!("ONLY public.{}", quoted_identifier(table)))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query(&format!(
        "LOCK TABLE {relations} IN SHARE ROW EXCLUSIVE MODE"
    ))
    .execute(&mut *connection)
    .await?;
    Ok(())
}

async fn count_rows(target: &mut SeedConnection<'_>, table: &str) -> Result<i64> {
    query_i64(
        target,
        &format!("SELECT COUNT(*) FROM {}", quoted_identifier(table)),
    )
    .await
}

async fn query_i64(target: &mut SeedConnection<'_>, sql: &str) -> Result<i64> {
    match target {
        SeedConnection::Sqlite(connection) => {
            Ok(sqlx::query_scalar(sql).fetch_one(&mut **connection).await?)
        }
        SeedConnection::Postgres(connection) => {
            Ok(sqlx::query_scalar(sql).fetch_one(&mut **connection).await?)
        }
    }
}

async fn execute_sql(target: &mut SeedConnection<'_>, sql: &str) -> Result<u64> {
    match target {
        SeedConnection::Sqlite(connection) => Ok(sqlx::query(sql)
            .execute(&mut **connection)
            .await?
            .rows_affected()),
        SeedConnection::Postgres(connection) => Ok(sqlx::query(sql)
            .execute(&mut **connection)
            .await?
            .rows_affected()),
    }
}

fn require_lower_sha256(value: &str) -> Result<()> {
    if value.len() != 64
        || value != value.to_ascii_lowercase()
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("expected catalog fingerprint is not one lowercase SHA-256 digest");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

    use crate::avionics::manufacturer::ensure_test_manufacturer_identity;
    use crate::avionics::reuse::{refresh_reuse_attestation_sqlite, AVIONICS_REUSE_POLICY_VERSION};
    use crate::plugin::{sha256_hex, signature_message};
    use sqlx::Executor;

    struct SignedCapture {
        public_key_base64: String,
        rendered_html_sha256: String,
        signature_base64: String,
    }

    fn signed_capture(install_id: i64, source_url: &str, rendered_html: &str) -> SignedCapture {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let keys = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let rendered_html_sha256 = sha256_hex(rendered_html.as_bytes());
        let signature_base64 = BASE64_STANDARD.encode(
            keys.sign(
                &rng,
                signature_message(install_id, source_url, &rendered_html_sha256).as_bytes(),
            )
            .unwrap()
            .as_ref(),
        );
        SignedCapture {
            public_key_base64: BASE64_STANDARD.encode(keys.public_key().as_ref()),
            rendered_html_sha256,
            signature_base64,
        }
    }

    async fn insert_sqlite_signed_capture(pool: &sqlx::SqlitePool, user_id: i64) -> SignedCapture {
        let capture = signed_capture(71, "https://listing.example/72", "<html>exact</html>");
        sqlx::query(
            "INSERT INTO plugin_installs (id, user_id, public_key_base64, created_at, revoked_at) VALUES (71, ?, ?, '2026-08-01 01:02:03', NULL)",
        )
        .bind(user_id)
        .bind(&capture.public_key_base64)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO plugin_submissions (id, user_id, plugin_install_id, source_url, submitted_at, rendered_html, rendered_html_sha256, signature_base64, extracted_listing_json, extraction_error, canonical_listing_id) VALUES (72, ?, 71, 'https://listing.example/72', '2026-08-01 02:03:04', '<html>exact</html>', ?, ?, NULL, NULL, NULL)",
        )
        .bind(user_id)
        .bind(&capture.rendered_html_sha256)
        .bind(&capture.signature_base64)
        .execute(pool)
        .await
        .unwrap();
        capture
    }

    async fn reset_sqlite_inventory_token(pool: &sqlx::SqlitePool) {
        sqlx::query(
            "UPDATE listing_replay_submission_inventory_lock \
             SET concurrency_token = (SELECT COUNT(*) FROM plugin_submissions) \
             WHERE singleton_id = 1 AND active_run_id IS NULL",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[derive(Debug, Eq, PartialEq)]
    struct SqliteReplayBoundary {
        inventory: (i64, Option<i64>, i64),
        users: Vec<(i64, String, String, String, String, String, String)>,
        installs: Vec<(i64, i64, String, String, Option<String>)>,
        submissions: Vec<(
            i64,
            i64,
            i64,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
        )>,
    }

    async fn sqlite_replay_boundary(pool: &sqlx::SqlitePool) -> SqliteReplayBoundary {
        SqliteReplayBoundary {
            inventory: sqlx::query_as(
                "SELECT singleton_id, active_run_id, concurrency_token \
                 FROM listing_replay_submission_inventory_lock",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            users: sqlx::query_as(
                "SELECT id, email, display_name, auth_provider, auth_subject, created_at, updated_at FROM users ORDER BY id",
            )
            .fetch_all(pool)
            .await
            .unwrap(),
            installs: sqlx::query_as(
                "SELECT id, user_id, public_key_base64, created_at, revoked_at FROM plugin_installs ORDER BY id",
            )
            .fetch_all(pool)
            .await
            .unwrap(),
            submissions: sqlx::query_as(
                "SELECT id, user_id, plugin_install_id, source_url, submitted_at, rendered_html, rendered_html_sha256, signature_base64, extracted_listing_json, extraction_error, canonical_listing_id FROM plugin_submissions ORDER BY id",
            )
            .fetch_all(pool)
            .await
            .unwrap(),
        }
    }

    async fn postgres_replay_boundary(
        pool: &sqlx::PgPool,
    ) -> (String, String, String, (i64, Option<i64>, i64)) {
        let users = sqlx::query_scalar(
            r#"SELECT COALESCE(
                 pg_catalog.json_agg(pg_catalog.row_to_json(boundary) ORDER BY boundary.id),
                 '[]'::json
               )::text
               FROM (
                 SELECT id, email, display_name, auth_provider, auth_subject,
                        created_at, updated_at
                 FROM users
               ) boundary"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let installs = sqlx::query_scalar(
            r#"SELECT COALESCE(
                 pg_catalog.json_agg(pg_catalog.row_to_json(boundary) ORDER BY boundary.id),
                 '[]'::json
               )::text
               FROM (
                 SELECT id, user_id, public_key_base64, created_at, revoked_at
                 FROM plugin_installs
               ) boundary"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let submissions = sqlx::query_scalar(
            r#"SELECT COALESCE(
                 pg_catalog.json_agg(pg_catalog.row_to_json(boundary) ORDER BY boundary.id),
                 '[]'::json
               )::text
               FROM (
                 SELECT id, user_id, plugin_install_id, source_url, submitted_at,
                        rendered_html, rendered_html_sha256, signature_base64,
                        extracted_listing_json, extraction_error, canonical_listing_id
                 FROM plugin_submissions
               ) boundary"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let inventory = sqlx::query_as(
            "SELECT singleton_id, active_run_id, concurrency_token \
             FROM listing_replay_submission_inventory_lock",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        (users, installs, submissions, inventory)
    }

    #[test]
    fn clean_target_allowlist_is_explicit_and_catalog_free() {
        let allowlist = CLEAN_TARGET_NONEMPTY_TABLES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert!(allowlist.contains("plugin_submissions"));
        assert!(allowlist.contains("listing_replay_submission_inventory_lock"));
        assert!(allowlist.contains("schema_migration_contracts"));
        assert!(!allowlist.contains("aircraft_sale_listings"));
        assert!(!allowlist.contains("avionics_models"));
        assert!(!allowlist.contains("gemini_api_usage"));
    }

    #[test]
    fn dynamic_sql_escapes_values_and_identifiers() {
        assert_eq!(
            sql_literal(&Value::String("pilot's unit".into())).unwrap(),
            "'pilot''s unit'"
        );
        assert_eq!(quoted_identifier("strange\"table"), "\"strange\"\"table\"");
    }

    #[test]
    fn expected_fingerprint_is_exact() {
        assert!(require_lower_sha256(&"a".repeat(64)).is_ok());
        assert!(require_lower_sha256(&"A".repeat(64)).is_err());
        assert!(require_lower_sha256("short").is_err());
    }

    async fn minimal_current_source(database_url: &str) -> AppDb {
        let source = AppDb::connect(database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = source.backend() else {
            panic!("catalog seed fixture must be SQLite")
        };
        let archive = "a".repeat(64);
        sqlx::query(
            r#"INSERT INTO curation_evidence_sources (
                 id, source_url, resolved_url, source_title, publisher,
                 source_domain, source_tier, content_sha256, retrieved_at
               ) VALUES (
                 101,
                 'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
                 'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
                 'FAA Releasable Aircraft Registry 2026-08-01',
                 'Federal Aviation Administration', 'faa.gov',
                 'regulator_primary', ?, CURRENT_TIMESTAMP
               )"#,
        )
        .bind(&archive)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO faa_registry_snapshots (
                 id, evidence_source_id, snapshot_date, source_url, archive_sha256,
                 source_manifest_sha256, target_set_sha256,
                 master_member_name, master_member_sha256,
                 aircraft_member_name, aircraft_member_sha256,
                 engine_member_name, engine_member_sha256, record_hash_domain
               ) VALUES (
                 102, 101, '2026-08-01',
                 'https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download',
                 ?, ?, ?, 'MASTER.txt', ?, 'ACFTREF.txt', ?, 'ENGINE.txt', ?,
                 'aircost-faa-master-retained-aircraft-projection-v1'
               )"#,
        )
        .bind(&archive)
        .bind("b".repeat(64))
        .bind("c".repeat(64))
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .bind("f".repeat(64))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO faa_registry_aircraft (
                 snapshot_id, n_number, manufacturer_serial_raw,
                 manufacturer_serial_key, aircraft_code, engine_code,
                 year_manufactured, source_record_sha256
               ) VALUES (102, 'N1', 'SERIAL-1', 'SERIAL1', 'TEST1', NULL, 2020, ?)"#,
        )
        .bind("0".repeat(64))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO faa_registry_aircraft_references (
                 snapshot_id, aircraft_code, manufacturer_name, model_name,
                 aircraft_type_code, engine_type_code, category_code,
                 certification_indicator_code, engine_count, seat_count,
                 weight_class_code, cruise_speed_mph,
                 type_certificate_data_sheet, type_certificate_holder
               ) VALUES (
                 102, 'TEST1', 'FIXTURE AIRCRAFT', 'MODEL 1', '4', '1',
                 '1', '0', 1, 4, 'CLASS 1', 120, 'T1', 'FIXTURE AIRCRAFT'
               )"#,
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status) VALUES (102, 'N1', 'matched')",
        )
        .execute(pool)
        .await
        .unwrap();

        let manufacturer_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_manufacturers (id, name, normalized_name) VALUES (201, 'Fixture Avionics', 'fixture avionics') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let membership = ensure_test_manufacturer_identity(&source, manufacturer_id)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO avionics_authoritative_source_origins (
                 id, authority_kind, avionics_manufacturer_identity_id,
                 regulator_key, https_origin, evidence_source_url,
                 evidence_source_title, evidence_text, approval_basis,
                 approved_by_user_id, approval_reason
               ) VALUES (
                 401, 'manufacturer_primary', ?, NULL,
                 'https://manufacturer.example',
                 'https://manufacturer.example/test-fixture',
                 'Fixture manufacturer product catalog',
                 'The exact first-party fixture origin identifies products made by Fixture Avionics.',
                 'curated_bootstrap', NULL,
                 'Reviewed provider-free catalog seed integration fixture'
               )"#,
        )
        .bind(membership.avionics_manufacturer_identity_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_types (id, name, normalized_name) VALUES (501, 'Navigator', 'navigator')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO avionics_models (
                 id, avionics_manufacturer_id, name, normalized_name,
                 manufacturer_identifier_kind, manufacturer_identifier,
                 normalized_manufacturer_identifier, identity_source_url,
                 identity_source_title, identity_evidence_text,
                 identity_evidence_kind, identity_confidence, catalog_reviewed_at
               ) VALUES (
                 601, 201, 'NAV 1', 'nav 1', 'manufacturer_model_number',
                 'NAV-1', 'nav1', 'https://manufacturer.example/test-fixture',
                 'Fixture NAV 1 product page',
                 'The manufacturer identifies NAV 1 as its exact avionics product.',
                 'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
               )"#,
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (601, 501)",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE avionics_models SET catalog_status = 'approved', verification_method = 'automated', verified_by_user_id = NULL WHERE id = 601",
        )
            .execute(pool)
            .await
            .unwrap();
        source
    }

    async fn attest_sqlite_fixture_product(source: &AppDb) -> String {
        let DatabaseBackend::Sqlite(pool) = source.backend() else {
            panic!("catalog seed fixture must be SQLite")
        };
        let mut transaction = pool.begin().await.unwrap();
        assert!(refresh_reuse_attestation_sqlite(
            source,
            &mut transaction,
            601,
            "https://manufacturer.example/test-fixture",
        )
        .await
        .unwrap());
        transaction.commit().await.unwrap();
        sqlx::query_scalar(
            "SELECT product_fingerprint FROM avionics_product_reuse_attestations WHERE avionics_model_id = 601",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn sqlite_current_reuse_attestation_is_seedable() {
        let directory = tempfile::tempdir().unwrap();
        let source_url = format!(
            "sqlite://{}",
            directory.path().join("source.sqlite3").display()
        );
        let target_url = format!(
            "sqlite://{}",
            directory.path().join("target.sqlite3").display()
        );
        let source = minimal_current_source(&source_url).await;
        let fingerprint = attest_sqlite_fixture_product(&source).await;
        let projection = CurrentCatalogProjection::load(&source).await.unwrap();
        let target = AppDb::connect(&target_url).await.unwrap();

        let report = seed_verified_catalog(
            &source,
            &target,
            Some(projection.fingerprint_sha256()),
            true,
        )
        .await
        .unwrap();

        assert_eq!(fingerprint.len(), 64);
        assert!(!report.dry_run);
        assert_eq!(
            report
                .projection_table_counts
                .get("avionics_product_reuse_attestations"),
            Some(&1)
        );
        assert!(report.materialized_rows > 0);
        let DatabaseBackend::Sqlite(pool) = target.backend() else {
            panic!("catalog seed target must be SQLite")
        };
        let target_fingerprint: String = sqlx::query_scalar(
            "SELECT product_fingerprint FROM avionics_product_reuse_attestations WHERE avionics_model_id = 601",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(target_fingerprint, fingerprint);
    }

    #[tokio::test]
    async fn stale_sqlite_reuse_attestation_rejects_seed_before_target_initialization() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.sqlite3");
        let target_path = directory.path().join("target.sqlite3");
        let source_url = format!("sqlite://{}", source_path.display());
        let target_url = format!("sqlite://{}", target_path.display());
        let source = minimal_current_source(&source_url).await;
        let current_fingerprint = attest_sqlite_fixture_product(&source).await;
        let stale_fingerprint = if current_fingerprint.starts_with('a') {
            "b".repeat(64)
        } else {
            "a".repeat(64)
        };
        let DatabaseBackend::Sqlite(pool) = source.backend() else {
            panic!("catalog seed fixture must be SQLite")
        };
        sqlx::query(
            "DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = 601",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO avionics_product_reuse_attestations (
                 avionics_model_id, avionics_authoritative_source_origin_id,
                 policy_version, product_fingerprint
               ) VALUES (601, 401, ?, ?)"#,
        )
        .bind(AVIONICS_REUSE_POLICY_VERSION)
        .bind(stale_fingerprint)
        .execute(pool)
        .await
        .unwrap();
        drop(source);

        let error = crate::listing::replay::catalog::seed_replay_verified_catalog(
            crate::listing::replay::catalog::SeedVerifiedCatalogRequest {
                source_database_url: &source_url,
                target_database_url: &target_url,
                expected_fingerprint_sha256: Some(&"c".repeat(64)),
                apply: true,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "source catalog contains stale avionics reuse-attestation fingerprints for model IDs: 601"
        );
        assert!(!target_path.exists());
    }

    #[tokio::test]
    async fn sqlite_dry_run_without_fingerprint_reports_projection_and_writes_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let source_url = format!(
            "sqlite://{}",
            directory.path().join("source.sqlite3").display()
        );
        let target_url = format!(
            "sqlite://{}",
            directory.path().join("target.sqlite3").display()
        );
        let source = minimal_current_source(&source_url).await;
        let target = AppDb::connect(&target_url).await.unwrap();
        let projection = CurrentCatalogProjection::load(&source).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = target.backend() else {
            panic!("catalog seed target must be SQLite")
        };
        let replay_before = sqlite_replay_boundary(pool).await;

        let report = seed_verified_catalog(&source, &target, None, false)
            .await
            .unwrap();

        assert!(report.dry_run);
        assert_eq!(report.provider_calls, 0);
        assert_eq!(
            report.projection_fingerprint_sha256,
            projection.fingerprint_sha256()
        );
        assert_eq!(
            report.projection_table_counts,
            projection.summary().table_counts
        );
        assert_eq!(report.materialized_rows, 0);
        assert_eq!(sqlite_replay_boundary(pool).await, replay_before);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM faa_registry_snapshots")
                .fetch_one(pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM avionics_models")
                .fetch_one(pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn stale_apply_fingerprint_does_not_initialize_absent_sqlite_target() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.sqlite3");
        let target_path = directory.path().join("target.sqlite3");
        let source_url = format!("sqlite://{}", source_path.display());
        let target_url = format!("sqlite://{}", target_path.display());
        let source = minimal_current_source(&source_url).await;
        let projection = CurrentCatalogProjection::load(&source).await.unwrap();
        let stale_fingerprint = if projection.fingerprint_sha256().starts_with('a') {
            "b".repeat(64)
        } else {
            "a".repeat(64)
        };
        drop(source);

        let error = crate::listing::replay::catalog::seed_replay_verified_catalog(
            crate::listing::replay::catalog::SeedVerifiedCatalogRequest {
                source_database_url: &source_url,
                target_database_url: &target_url,
                expected_fingerprint_sha256: Some(&stale_fingerprint),
                apply: true,
            },
        )
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("differs from required fingerprint"));
        assert!(!target_path.exists());
    }

    #[tokio::test]
    async fn catalog_seed_apply_without_fingerprint_fails_before_database_work() {
        let source = AppDb::connect("sqlite::memory:").await.unwrap();
        let target = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = target.backend() else {
            panic!("catalog seed target must be SQLite")
        };
        let replay_before = sqlite_replay_boundary(pool).await;

        let error = seed_verified_catalog(&source, &target, None, true)
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "catalog seed --apply requires one reviewed catalog fingerprint"
        );
        assert_eq!(sqlite_replay_boundary(pool).await, replay_before);
    }

    #[tokio::test]
    async fn sqlite_round_trip_reloads_exact_projection_and_rejects_rerun() {
        let directory = tempfile::tempdir().unwrap();
        let source_url = format!(
            "sqlite://{}",
            directory.path().join("source.sqlite3").display()
        );
        let target_url = format!(
            "sqlite://{}",
            directory.path().join("target.sqlite3").display()
        );
        let source = minimal_current_source(&source_url).await;
        let target = AppDb::connect(&target_url).await.unwrap();
        let projection = CurrentCatalogProjection::load(&source).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = target.backend() else {
            panic!("catalog seed target must be SQLite")
        };
        let developer_id: i64 =
            sqlx::query_scalar("SELECT id FROM users WHERE email = 'developer@localhost'")
                .fetch_one(pool)
                .await
                .unwrap();
        insert_sqlite_signed_capture(pool, developer_id).await;
        let replay_before = sqlite_replay_boundary(pool).await;
        assert_eq!(replay_before.inventory, (1, None, 1));
        let report = seed_verified_catalog(
            &source,
            &target,
            Some(projection.fingerprint_sha256()),
            true,
        )
        .await
        .unwrap();
        assert!(!report.dry_run);
        assert_eq!(report.provider_calls, 0);
        assert_eq!(report.retained_capture_count, 1);
        let serialized_report = serde_json::to_value(&report).unwrap();
        assert!(serialized_report.get("version").is_none());
        assert!(serialized_report.get("projection_version").is_none());
        assert_eq!(sqlite_replay_boundary(pool).await, replay_before);
        projection.require_reloaded_match(&target).await.unwrap();
        let error = seed_verified_catalog(
            &source,
            &target,
            Some(projection.fingerprint_sha256()),
            true,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("non-bootstrap table"));

        let next_model_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_models (avionics_manufacturer_id, name, normalized_name) VALUES (201, 'NEXT 1', 'next 1') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(next_model_id, 602);
        let violations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(violations, 0);
    }

    #[tokio::test]
    async fn sqlite_clean_boundary_rejects_drift_and_failure_rolls_back() {
        let directory = tempfile::tempdir().unwrap();
        let source_url = format!(
            "sqlite://{}",
            directory.path().join("source.sqlite3").display()
        );
        let target_url = format!(
            "sqlite://{}",
            directory.path().join("target.sqlite3").display()
        );
        let source = minimal_current_source(&source_url).await;
        let target = AppDb::connect(&target_url).await.unwrap();
        let projection = CurrentCatalogProjection::load(&source).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = target.backend() else {
            panic!("catalog seed target must be SQLite")
        };

        sqlx::query("UPDATE users SET display_name = 'Drifted' WHERE email = ?")
            .bind(DEVELOPER_EMAIL)
            .execute(pool)
            .await
            .unwrap();
        assert!(inspect_clean_target(&target, &projection)
            .await
            .unwrap_err()
            .to_string()
            .contains("developer user"));
        sqlx::query("UPDATE users SET display_name = 'Developer' WHERE email = ?")
            .bind(DEVELOPER_EMAIL)
            .execute(pool)
            .await
            .unwrap();

        sqlx::query("UPDATE aircraft_markets SET name = 'Drifted' WHERE code = 'GLOBAL'")
            .execute(pool)
            .await
            .unwrap();
        assert!(inspect_clean_target(&target, &projection)
            .await
            .unwrap_err()
            .to_string()
            .contains("bootstrap rows"));
        sqlx::query("UPDATE aircraft_markets SET name = 'Global' WHERE code = 'GLOBAL'")
            .execute(pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO schema_migration_contracts \
             (migration_name, contract_version, contract_fingerprint) \
             VALUES ('20990101_unreviewed_receipt', 1, ?)",
        )
        .bind("a".repeat(64))
        .execute(pool)
        .await
        .unwrap();
        assert!(inspect_clean_target(&target, &projection)
            .await
            .unwrap_err()
            .to_string()
            .contains("non-canonical schema migration receipts"));
        sqlx::query(
            "DELETE FROM schema_migration_contracts \
             WHERE migration_name = '20990101_unreviewed_receipt'",
        )
        .execute(pool)
        .await
        .unwrap();

        sqlx::query("INSERT INTO avionics_types (name, normalized_name) VALUES ('Stale', 'stale')")
            .execute(pool)
            .await
            .unwrap();
        assert!(inspect_clean_target(&target, &projection)
            .await
            .unwrap_err()
            .to_string()
            .contains("non-bootstrap table avionics_types"));
        sqlx::query("DELETE FROM avionics_types WHERE normalized_name = 'stale'")
            .execute(pool)
            .await
            .unwrap();

        let developer_id: i64 =
            sqlx::query_scalar("SELECT id FROM users WHERE email = 'developer@localhost'")
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO plugin_installs (id, user_id, public_key_base64) VALUES (91, ?, 'key')",
        )
        .bind(developer_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO plugin_submissions (id, user_id, plugin_install_id, source_url, rendered_html, rendered_html_sha256, signature_base64, extracted_listing_json) VALUES (92, ?, 91, 'https://listing.example/92', '<html/>', 'hash', 'sig', '{}')",
        )
        .bind(developer_id)
        .execute(pool)
        .await
        .unwrap();
        assert!(inspect_clean_target(&target, &projection)
            .await
            .unwrap_err()
            .to_string()
            .contains("derived capture state"));
        sqlx::query("DELETE FROM plugin_submissions WHERE id = 92")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM plugin_installs WHERE id = 91")
            .execute(pool)
            .await
            .unwrap();
        reset_sqlite_inventory_token(pool).await;

        sqlx::query(
            r#"CREATE TRIGGER reject_catalog_seed
               BEFORE INSERT ON faa_registry_snapshots
               BEGIN SELECT RAISE(ABORT, 'fixture phase failure'); END"#,
        )
        .execute(pool)
        .await
        .unwrap();
        let error = materialize(&target, &projection).await.unwrap_err();
        assert!(format!("{error:#}").contains("fixture phase failure"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM curation_evidence_sources")
                .fetch_one(pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM faa_registry_snapshots")
                .fetch_one(pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn sqlite_rejects_every_unauthenticated_or_misowned_capture() {
        let directory = tempfile::tempdir().unwrap();
        let source_url = format!(
            "sqlite://{}",
            directory.path().join("source.sqlite3").display()
        );
        let target_url = format!(
            "sqlite://{}",
            directory.path().join("target.sqlite3").display()
        );
        let source = minimal_current_source(&source_url).await;
        let target = AppDb::connect(&target_url).await.unwrap();
        let projection = CurrentCatalogProjection::load(&source).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = target.backend() else {
            panic!("catalog seed target must be SQLite")
        };
        let developer_id: i64 =
            sqlx::query_scalar("SELECT id FROM users WHERE email = 'developer@localhost'")
                .fetch_one(pool)
                .await
                .unwrap();
        let capture = insert_sqlite_signed_capture(pool, developer_id).await;
        let valid_boundary = sqlite_replay_boundary(pool).await;

        sqlx::query("UPDATE plugin_submissions SET rendered_html_sha256 = ? WHERE id = 72")
            .bind("0".repeat(64))
            .execute(pool)
            .await
            .unwrap();
        let error = materialize(&target, &projection).await.unwrap_err();
        assert!(format!("{error:#}").contains("rendered HTML hash is corrupt"));
        sqlx::query("UPDATE plugin_submissions SET rendered_html_sha256 = ? WHERE id = 72")
            .bind(&capture.rendered_html_sha256)
            .execute(pool)
            .await
            .unwrap();

        sqlx::query("UPDATE plugin_submissions SET signature_base64 = ? WHERE id = 72")
            .bind(BASE64_STANDARD.encode([0_u8; 64]))
            .execute(pool)
            .await
            .unwrap();
        let error = materialize(&target, &projection).await.unwrap_err();
        assert!(format!("{error:#}").contains("invalid plugin signature"));
        sqlx::query("UPDATE plugin_submissions SET signature_base64 = ? WHERE id = 72")
            .bind(&capture.signature_base64)
            .execute(pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO users (id, email, display_name, auth_provider, auth_subject) \
             VALUES (73, 'other@example.test', 'Other', 'local', 'other')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_installs SET user_id = 73 WHERE id = 71")
            .execute(pool)
            .await
            .unwrap();
        let error = materialize(&target, &projection).await.unwrap_err();
        assert!(format!("{error:#}").contains("owner differs from plugin install"));
        sqlx::query("UPDATE plugin_installs SET user_id = ? WHERE id = 71")
            .bind(developer_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = 73")
            .execute(pool)
            .await
            .unwrap();

        sqlx::query("UPDATE plugin_installs SET created_at = '2026-08-01 03:03:04' WHERE id = 71")
            .execute(pool)
            .await
            .unwrap();
        let error = materialize(&target, &projection).await.unwrap_err();
        assert!(format!("{error:#}").contains("timestamp chronology"));
        sqlx::query("UPDATE plugin_installs SET created_at = '2026-08-01 01:02:03' WHERE id = 71")
            .execute(pool)
            .await
            .unwrap();

        sqlx::query("UPDATE plugin_installs SET revoked_at = '2026-08-01 01:30:00' WHERE id = 71")
            .execute(pool)
            .await
            .unwrap();
        let error = materialize(&target, &projection).await.unwrap_err();
        assert!(format!("{error:#}").contains("timestamp chronology"));
        sqlx::query("UPDATE plugin_installs SET revoked_at = NULL WHERE id = 71")
            .execute(pool)
            .await
            .unwrap();

        reset_sqlite_inventory_token(pool).await;
        assert_eq!(sqlite_replay_boundary(pool).await, valid_boundary);
        materialize(&target, &projection).await.unwrap();
        assert_eq!(sqlite_replay_boundary(pool).await, valid_boundary);
    }

    #[tokio::test]
    async fn sqlite_mutating_trigger_fails_precommit_parity_and_rolls_back_sequences() {
        let directory = tempfile::tempdir().unwrap();
        let source_url = format!(
            "sqlite://{}",
            directory.path().join("source.sqlite3").display()
        );
        let target_url = format!(
            "sqlite://{}",
            directory.path().join("target.sqlite3").display()
        );
        let source = minimal_current_source(&source_url).await;
        let target = AppDb::connect(&target_url).await.unwrap();
        let projection = CurrentCatalogProjection::load(&source).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = target.backend() else {
            panic!("catalog seed target must be SQLite")
        };
        let sequences_before: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, seq FROM sqlite_sequence ORDER BY name")
                .fetch_all(pool)
                .await
                .unwrap();
        sqlx::query(
            r#"CREATE TRIGGER mutate_catalog_seed
               AFTER INSERT ON avionics_types
               BEGIN
                 UPDATE avionics_types SET name = name || ' MUTATED' WHERE id = NEW.id;
               END"#,
        )
        .execute(pool)
        .await
        .unwrap();

        let error = materialize(&target, &projection).await.unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("in-transaction materialized"), "{error}");
        for table in MATERIALIZATION_TABLES {
            if *table == "aircraft_markets" {
                continue;
            }
            let count: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {}",
                quoted_identifier(table)
            ))
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(count, 0, "{table} retained a rolled-back seed row");
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM aircraft_markets")
                .fetch_one(pool)
                .await
                .unwrap(),
            2,
            "bootstrap aircraft markets changed across rolled-back seed"
        );
        let sequences_after: Vec<(String, i64)> =
            sqlx::query_as("SELECT name, seq FROM sqlite_sequence ORDER BY name")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(sequences_after, sequences_before);
    }

    #[tokio::test]
    async fn sqlite_seed_waits_behind_existing_writer() {
        let directory = tempfile::tempdir().unwrap();
        let source_url = format!(
            "sqlite://{}",
            directory.path().join("source.sqlite3").display()
        );
        let target_url = format!(
            "sqlite://{}",
            directory.path().join("target.sqlite3").display()
        );
        let source = minimal_current_source(&source_url).await;
        let target = AppDb::connect(&target_url).await.unwrap();
        let projection = CurrentCatalogProjection::load(&source).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = target.backend() else {
            panic!("catalog seed target must be SQLite")
        };
        let mut blocker = pool.acquire().await.unwrap();
        blocker.execute("BEGIN IMMEDIATE").await.unwrap();
        let waiting = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            materialize(&target, &projection),
        )
        .await;
        assert!(
            waiting.is_err(),
            "seed did not wait behind the target writer"
        );
        blocker.execute("ROLLBACK").await.unwrap();
        materialize(&target, &projection).await.unwrap();
        projection.require_reloaded_match(&target).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_round_trip_fences_writers_preserves_replay_and_rejects_rerun() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let directory = tempfile::tempdir().unwrap();
        let source_url = format!(
            "sqlite://{}",
            directory.path().join("source.sqlite3").display()
        );
        let source = minimal_current_source(&source_url).await;
        let source_projection = CurrentCatalogProjection::load(&source).await.unwrap();

        let raw_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        raw_pool
            .execute("DROP SCHEMA public CASCADE")
            .await
            .unwrap();
        raw_pool.execute("CREATE SCHEMA public").await.unwrap();
        drop(raw_pool);
        let target = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = target.backend() else {
            panic!("catalog seed target must be PostgreSQL")
        };
        let developer_id: i64 =
            sqlx::query_scalar("SELECT id FROM users WHERE email = 'developer@localhost'")
                .fetch_one(pool)
                .await
                .unwrap();
        let capture = signed_capture(71, "https://listing.example/72", "<html>exact</html>");
        sqlx::query(
            "INSERT INTO plugin_installs (id, user_id, public_key_base64, created_at, revoked_at) VALUES (71, $1, $2, '2026-08-01 01:02:03', NULL)",
        )
        .bind(developer_id)
        .bind(&capture.public_key_base64)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO plugin_submissions (id, user_id, plugin_install_id, source_url, submitted_at, rendered_html, rendered_html_sha256, signature_base64, extracted_listing_json, extraction_error, canonical_listing_id) VALUES (72, $1, 71, 'https://listing.example/72', '2026-08-01 02:03:04', '<html>exact</html>', $2, $3, NULL, NULL, NULL)",
        )
        .bind(developer_id)
        .bind(&capture.rendered_html_sha256)
        .bind(&capture.signature_base64)
        .execute(pool)
        .await
        .unwrap();
        let replay_before = postgres_replay_boundary(pool).await;
        assert_eq!(replay_before.3, (1, None, 1));

        // The same deterministic lock set used by apply conflicts with the
        // ROW EXCLUSIVE lock taken by ordinary INSERT/UPDATE/DELETE traffic.
        let mut seed_lock = pool.begin().await.unwrap();
        let tables = postgres_base_tables(&mut seed_lock).await.unwrap();
        lock_postgres_tables(&mut seed_lock, &tables).await.unwrap();
        let mut rogue = pool.begin().await.unwrap();
        rogue
            .execute("SET LOCAL lock_timeout = '100ms'")
            .await
            .unwrap();
        let blocked = rogue
            .execute("UPDATE users SET display_name = display_name WHERE id = 1")
            .await
            .unwrap_err();
        assert!(blocked.to_string().contains("lock timeout"));
        rogue.rollback().await.unwrap();
        seed_lock.rollback().await.unwrap();

        let report = seed_verified_catalog(
            &source,
            &target,
            Some(source_projection.fingerprint_sha256()),
            true,
        )
        .await
        .unwrap();
        assert_eq!(report.provider_calls, 0);
        assert_eq!(report.retained_capture_count, 1);
        assert_eq!(
            report.projection_table_counts,
            source_projection.summary().table_counts
        );
        assert_eq!(postgres_replay_boundary(pool).await, replay_before);
        source_projection
            .require_reloaded_match(&target)
            .await
            .unwrap();
        let rerun = seed_verified_catalog(
            &source,
            &target,
            Some(source_projection.fingerprint_sha256()),
            true,
        )
        .await
        .unwrap_err();
        let rerun = format!("{rerun:#}");
        assert!(rerun.contains("clean catalog target has"), "{rerun}");
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_sequence_reset_covers_identity_and_serial_owned_ids() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        pool.execute("DROP SCHEMA public CASCADE").await.unwrap();
        pool.execute("CREATE SCHEMA public").await.unwrap();
        drop(pool);
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            panic!("catalog sequence test must be PostgreSQL")
        };

        // ALTER SEQUENCE ... RESTART is transactional. A failed seed must not
        // strand either an IDENTITY or legacy BIGSERIAL sequence at the
        // projection's explicit high IDs.
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, auth_provider, auth_subject) VALUES (900001, 'sequence@example.test', 'Sequence', 'local', 'sequence-test')",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO valuation_snapshots (id, capture_time, input_sha256, selection_policy_json, feature_schema_version, included_count, excluded_count) VALUES (800001, 'now', $1, '{}', 1, 0, 0)",
        )
        .bind("1".repeat(64))
        .execute(&mut *transaction)
        .await
        .unwrap();
        {
            let mut connection = SeedConnection::Postgres(&mut transaction);
            reset_sequences(&mut connection).await.unwrap();
        }
        transaction.rollback().await.unwrap();
        let rollback_identity_id: i64 = sqlx::query_scalar(
            "INSERT INTO users (email, display_name, auth_provider, auth_subject) VALUES ('rollback@example.test', 'Rollback', 'local', 'rollback-test') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(rollback_identity_id, 2);
        let rollback_serial_id: i64 = sqlx::query_scalar(
            "INSERT INTO valuation_snapshots (capture_time, input_sha256, selection_policy_json, feature_schema_version, included_count, excluded_count) VALUES ('rollback', $1, '{}', 1, 0, 0) RETURNING id",
        )
        .bind("2".repeat(64))
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(rollback_serial_id, 1);

        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, auth_provider, auth_subject) VALUES (900001, 'sequence@example.test', 'Sequence', 'local', 'sequence-test')",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO valuation_snapshots (id, capture_time, input_sha256, selection_policy_json, feature_schema_version, included_count, excluded_count) VALUES (800001, 'now', $1, '{}', 1, 0, 0)",
        )
        .bind("3".repeat(64))
        .execute(&mut *transaction)
        .await
        .unwrap();
        {
            let mut connection = SeedConnection::Postgres(&mut transaction);
            reset_sequences(&mut connection).await.unwrap();
        }
        transaction.commit().await.unwrap();
        let next_identity_id: i64 = sqlx::query_scalar(
            "INSERT INTO users (email, display_name, auth_provider, auth_subject) VALUES ('next@example.test', 'Next', 'local', 'next-test') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(next_identity_id, 900002);
        let next_serial_id: i64 = sqlx::query_scalar(
            "INSERT INTO valuation_snapshots (capture_time, input_sha256, selection_policy_json, feature_schema_version, included_count, excluded_count) VALUES ('next', $1, '{}', 1, 0, 0) RETURNING id",
        )
        .bind("4".repeat(64))
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(next_serial_id, 800002);
    }
}
