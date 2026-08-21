//! Isolated conversion of the one attested legacy SQLite source used by the
//! clean-replay rebuild.
//!
//! This is an administrative conversion boundary, not a runtime compatibility
//! path. Unknown source shapes fail closed and the source is always opened
//! read-only.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha3::{Digest, Sha3_256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Acquire, FromRow, Row, SqliteConnection, SqlitePool};
use tempfile::NamedTempFile;

use super::{
    entry_from_row, validate_source_capture, validate_trusted_capture_manifest, SourceCaptureRow,
    TrustedCaptureManifest,
};
use crate::aircraft::faa::bridge::rebuild_faa_projection;
use crate::aircraft::faa::normalize_n_number;
use crate::catalog::projection::{project_reusable_catalog, required_faa_representatives};
use crate::db::{database_url_from_arg, sqlite_database_urls_equal, AppDb, DatabaseBackend};

pub const LEGACY_SQLITE_SCHEMA_OBJECT_COUNT: usize = 575;
pub const LEGACY_SQLITE_SCHEMA_SHA3_256: &str =
    "527552c4fbe674eaca5de3a1228bfcde0fd99f05c7f2924f28ffdba687ec5957";
pub const LEGACY_SCHEMA_RECEIPT_COUNT: usize = 19;
pub const LEGACY_SCHEMA_RECEIPT_SHA3_256: &str =
    "7081e6098b4e11367b7a371301c4b6ff1e916104174d65906a723c41284dd639";
pub const LEGACY_FAA_ARCHIVE_SHA256: &str =
    "14885735825e5f46babdac8bf851c77c7ce7b104ae0f86395ef594e6e467c724";

const PREPARED_REPLAY_SOURCE_NONEMPTY_TABLES: &[&str] = &[
    "aircraft_designation_aliases",
    "aircraft_designation_faa_bindings",
    "aircraft_designation_identifiers",
    "aircraft_designations",
    "aircraft_engine_catalog_models",
    "aircraft_factory_packages",
    "aircraft_family_aliases",
    "aircraft_feature_definitions",
    "aircraft_generation_designations",
    "aircraft_generations",
    "aircraft_identity_decision_claims",
    "aircraft_identity_decisions",
    "aircraft_identity_observations",
    "aircraft_identity_resolution_cases",
    "aircraft_make_aliases",
    "aircraft_makes",
    "aircraft_manufacturers",
    "aircraft_markets",
    "aircraft_model_families",
    "aircraft_model_variants",
    "aircraft_models",
    "aircraft_package_applicability",
    "aircraft_propeller_catalog_models",
    "aircraft_sale_listing_pending_compatibility_placeholder",
    "aircraft_serial_number_schemes",
    "aircraft_tcds_make_lineage_bindings",
    "avionics_approved_product_identities",
    "avionics_authoritative_source_origins",
    "avionics_manufacturer_canonical_keys",
    "avionics_manufacturer_identities",
    "avionics_manufacturer_identity_memberships",
    "avionics_manufacturers",
    "avionics_model_types",
    "avionics_models",
    "avionics_product_reuse_attestations",
    "avionics_suite_components",
    "avionics_types",
    "component_depreciation_profiles",
    "curation_evidence_claims",
    "curation_evidence_sources",
    "depreciation_profiles",
    "faa_registry_aircraft",
    "faa_registry_aircraft_references",
    "faa_registry_coverage",
    "faa_registry_engine_references",
    "faa_registry_snapshots",
    "plugin_installs",
    "plugin_submissions",
    "schema_migration_contracts",
    "users",
];

pub struct PrepareLegacyReplaySourceRequest<'a> {
    pub source_database: &'a str,
    pub manifest: &'a TrustedCaptureManifest,
    pub faa_archive: &'a Path,
    pub expected_faa_archive_sha256: &'a str,
    pub output: &'a Path,
    pub apply: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrepareLegacyReplaySourceReport {
    pub dry_run: bool,
    pub provider_calls: u64,
    pub source_schema_object_count: usize,
    pub source_schema_sha3_256: String,
    pub source_receipt_count: usize,
    pub source_receipt_sha3_256: String,
    pub manifest_sha256: String,
    pub capture_count: usize,
    pub n_number_count: usize,
    pub faa_archive_sha256: String,
    pub faa_snapshot_date: String,
    pub catalog_fingerprint_sha256: String,
    pub applied_rows: usize,
    pub output_created: bool,
}

#[derive(Clone, Copy)]
struct FrozenSourceContract {
    schema_object_count: usize,
    schema_sha3_256: &'static str,
    receipt_count: usize,
    receipt_sha3_256: &'static str,
}

const FROZEN_SOURCE_CONTRACT: FrozenSourceContract = FrozenSourceContract {
    schema_object_count: LEGACY_SQLITE_SCHEMA_OBJECT_COUNT,
    schema_sha3_256: LEGACY_SQLITE_SCHEMA_SHA3_256,
    receipt_count: LEGACY_SCHEMA_RECEIPT_COUNT,
    receipt_sha3_256: LEGACY_SCHEMA_RECEIPT_SHA3_256,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct FrozenSourceAttestation {
    schema_object_count: usize,
    schema_sha3_256: String,
    receipt_count: usize,
    receipt_sha3_256: String,
}

#[derive(Clone, Debug)]
struct LegacyCaptureSelection {
    rows: Vec<SourceCaptureRow>,
    n_numbers: Vec<String>,
}

#[derive(FromRow)]
struct SchemaObjectRow {
    object_type: String,
    name: String,
    table_name: String,
    sql: Option<String>,
}

#[derive(FromRow)]
struct MigrationReceiptRow {
    migration_name: String,
    contract_version: i64,
    contract_fingerprint: String,
    installed_at: String,
}

pub async fn prepare_legacy_replay_source(
    request: PrepareLegacyReplaySourceRequest<'_>,
) -> Result<PrepareLegacyReplaySourceReport> {
    validate_prepare_request(&request)?;
    let source_pool = open_frozen_source(request.source_database).await?;
    let mut source_connection = source_pool.acquire().await?;
    let mut source_snapshot = source_connection.begin().await?;
    let attestation = attest_frozen_source(&mut source_snapshot).await?;
    let captures = load_legacy_capture_selection(&mut source_snapshot, request.manifest).await?;
    let representatives = required_faa_representatives(&mut source_snapshot).await?;

    let output_parent = request.output.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = request
        .apply
        .then(|| NamedTempFile::new_in(output_parent))
        .transpose()
        .with_context(|| {
            format!(
                "could not create sibling temporary output in {}",
                output_parent.display()
            )
        })?;
    let target_url = temporary
        .as_ref()
        .map(|file| database_url_from_arg(Some(file.path().to_string_lossy().into_owned())))
        .unwrap_or_else(|| "sqlite::memory:".to_string());
    let target = AppDb::connect(&target_url).await?;
    let capture_rows = import_legacy_captures(&target, &captures).await?;
    let faa = rebuild_faa_projection(
        &mut source_snapshot,
        &target,
        request.faa_archive,
        request.expected_faa_archive_sha256,
        &captures.n_numbers,
        &representatives,
    )
    .await?;
    let catalog = project_reusable_catalog(&mut source_snapshot, &target, &faa).await?;
    audit_prepared_target(&target, &faa.obsolete_hashes).await?;
    source_snapshot.rollback().await?;
    drop(source_connection);
    source_pool.close().await;

    let faa_archive_sha256 = faa.report.archive_sha256.clone();
    let faa_snapshot_date = faa.report.snapshot_date.clone();
    let applied_rows = capture_rows + catalog.applied_rows;
    if request.apply {
        checkpoint_prepared_target(&target).await?;
    }
    target.close().await;
    let output_created = if let Some(file) = temporary.take() {
        let diagnostic = AppDb::connect_diagnostic(&target_url).await?;
        audit_prepared_target(&diagnostic, &faa.obsolete_hashes).await?;
        diagnostic.close().await;
        file.as_file().sync_all().with_context(|| {
            format!(
                "could not synchronize prepared replay source temporary file for {}",
                request.output.display()
            )
        })?;
        file.persist_noclobber(request.output).with_context(|| {
            format!(
                "could not atomically publish prepared replay source {}",
                request.output.display()
            )
        })?;
        true
    } else {
        false
    };
    Ok(PrepareLegacyReplaySourceReport {
        dry_run: !request.apply,
        provider_calls: 0,
        source_schema_object_count: attestation.schema_object_count,
        source_schema_sha3_256: attestation.schema_sha3_256,
        source_receipt_count: attestation.receipt_count,
        source_receipt_sha3_256: attestation.receipt_sha3_256,
        manifest_sha256: request.manifest.manifest_sha256.clone(),
        capture_count: captures.rows.len(),
        n_number_count: captures.n_numbers.len(),
        faa_archive_sha256,
        faa_snapshot_date,
        catalog_fingerprint_sha256: catalog.fingerprint_sha256,
        applied_rows,
        output_created,
    })
}

fn validate_prepare_request(request: &PrepareLegacyReplaySourceRequest<'_>) -> Result<()> {
    validate_trusted_capture_manifest(request.manifest).map_err(anyhow::Error::msg)?;
    if request.expected_faa_archive_sha256 != LEGACY_FAA_ARCHIVE_SHA256 {
        bail!(
            "legacy replay-source bridge accepts only historical FAA archive SHA-256 {LEGACY_FAA_ARCHIVE_SHA256}"
        );
    }
    if request.output.exists() {
        bail!(
            "prepared replay-source output already exists: {}",
            request.output.display()
        );
    }
    let parent = request.output.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        bail!("prepared replay-source output parent does not exist");
    }
    let output_url = database_url_from_arg(Some(request.output.to_string_lossy().into_owned()));
    if sqlite_database_urls_equal(request.source_database, &output_url)? {
        bail!("legacy source and prepared replay-source output must be different files");
    }
    Ok(())
}

pub(crate) async fn open_frozen_source(source_database: &str) -> Result<SqlitePool> {
    let source_url = database_url_from_arg(Some(source_database.to_string()));
    if source_url == "sqlite::memory:"
        || source_url.starts_with("postgres://")
        || source_url.starts_with("postgresql://")
        || !source_url.starts_with("sqlite://")
    {
        bail!("legacy replay source must be a file-backed SQLite database");
    }
    let source_path = sqlite_path(&source_url)?;
    let metadata = std::fs::metadata(&source_path).with_context(|| {
        format!(
            "could not inspect legacy replay source {}",
            source_path.display()
        )
    })?;
    if !metadata.is_file() {
        bail!("legacy replay source must be a regular SQLite file");
    }
    let options = SqliteConnectOptions::from_str(&source_url)
        .with_context(|| format!("invalid legacy SQLite database URL {source_url}"))?
        .create_if_missing(false)
        .read_only(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("PRAGMA query_only = ON")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .with_context(|| {
            format!(
                "could not open legacy replay source {} read-only",
                source_path.display()
            )
        })?;
    Ok(pool)
}

fn sqlite_path(database_url: &str) -> Result<PathBuf> {
    let raw = database_url
        .strip_prefix("sqlite://")
        .context("legacy replay source must use a file-backed SQLite URL")?;
    if raw.is_empty() || raw.contains('?') || raw.contains('#') {
        bail!("legacy replay source SQLite URL must name one plain filesystem path");
    }
    Ok(PathBuf::from(raw))
}

async fn attest_frozen_source(source: &mut SqliteConnection) -> Result<FrozenSourceAttestation> {
    attest_source_against(source, FROZEN_SOURCE_CONTRACT).await
}

async fn attest_source_against(
    source: &mut SqliteConnection,
    expected: FrozenSourceContract,
) -> Result<FrozenSourceAttestation> {
    let application_id: i64 = sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(&mut *source)
        .await?;
    let user_version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *source)
        .await?;
    if application_id != 0 || user_version != 0 {
        bail!(
            "legacy replay source has unexpected SQLite header versions (application_id={application_id}, user_version={user_version})"
        );
    }
    let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&mut *source)
        .await?;
    if integrity != "ok" {
        bail!("legacy replay source failed SQLite quick_check: {integrity}");
    }
    let foreign_key_errors: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
            .fetch_one(&mut *source)
            .await?;
    if foreign_key_errors != 0 {
        bail!("legacy replay source has {foreign_key_errors} foreign-key violations");
    }

    let objects = sqlx::query_as::<_, SchemaObjectRow>(
        r#"SELECT type AS object_type, name, tbl_name AS table_name, sql
           FROM sqlite_schema
           WHERE name NOT LIKE 'sqlite_stat%'
           ORDER BY type, name, tbl_name"#,
    )
    .fetch_all(&mut *source)
    .await?;
    let schema_sha3_256 = hash_rows(objects.iter().map(|row| {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            row.object_type,
            row.name,
            row.table_name,
            row.sql.as_deref().unwrap_or("")
        )
    }));
    if objects.len() != expected.schema_object_count || schema_sha3_256 != expected.schema_sha3_256
    {
        bail!(
            "legacy replay source schema is not the frozen conversion input (objects={}, sha3_256={schema_sha3_256})",
            objects.len()
        );
    }

    let receipts = sqlx::query_as::<_, MigrationReceiptRow>(
        r#"SELECT migration_name, contract_version, contract_fingerprint, installed_at
           FROM schema_migration_contracts ORDER BY migration_name"#,
    )
    .fetch_all(&mut *source)
    .await?;
    let receipt_sha3_256 = hash_rows(receipts.iter().map(|row| {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            row.migration_name, row.contract_version, row.contract_fingerprint, row.installed_at
        )
    }));
    if receipts.len() != expected.receipt_count || receipt_sha3_256 != expected.receipt_sha3_256 {
        bail!(
            "legacy replay source migration receipts are not the frozen conversion input (receipts={}, sha3_256={receipt_sha3_256})",
            receipts.len()
        );
    }
    Ok(FrozenSourceAttestation {
        schema_object_count: objects.len(),
        schema_sha3_256,
        receipt_count: receipts.len(),
        receipt_sha3_256,
    })
}

fn hash_rows(rows: impl IntoIterator<Item = String>) -> String {
    let mut hasher = Sha3_256::new();
    for row in rows {
        hasher.update(row.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

async fn load_legacy_capture_selection(
    source: &mut SqliteConnection,
    manifest: &TrustedCaptureManifest,
) -> Result<LegacyCaptureSelection> {
    validate_trusted_capture_manifest(manifest).map_err(anyhow::Error::msg)?;
    let mut rows = Vec::with_capacity(manifest.captures.len());
    let mut n_numbers = BTreeSet::new();
    for expected in &manifest.captures {
        let row = sqlx::query_as::<_, SourceCaptureRow>(
            r#"SELECT submission.id AS submission_id,
                      owner.id AS user_id,
                      owner.email AS user_email,
                      owner.display_name AS user_display_name,
                      owner.auth_provider AS user_auth_provider,
                      owner.auth_subject AS user_auth_subject,
                      owner.created_at AS user_created_at,
                      owner.updated_at AS user_updated_at,
                      install.id AS plugin_install_id,
                      install.public_key_base64 AS plugin_public_key_base64,
                      install.created_at AS plugin_install_created_at,
                      install.revoked_at AS plugin_install_revoked_at,
                      submission.source_url,
                      submission.submitted_at,
                      submission.rendered_html,
                      submission.rendered_html_sha256,
                      submission.signature_base64
               FROM plugin_submissions submission
               JOIN users owner ON owner.id = submission.user_id
               JOIN plugin_installs install
                 ON install.id = submission.plugin_install_id
                AND install.user_id = submission.user_id
               WHERE submission.id = ?"#,
        )
        .bind(expected.submission_id)
        .fetch_optional(&mut *source)
        .await?
        .with_context(|| {
            format!(
                "selected plugin submission {} does not exist",
                expected.submission_id
            )
        })?;
        validate_source_capture(&row).map_err(anyhow::Error::msg)?;
        validate_capture_timestamps(source, &row).await?;
        if entry_from_row(&row) != *expected {
            bail!(
                "source capture {} changed after the manifest was created",
                expected.submission_id
            );
        }
        let registration: Option<String> = sqlx::query_scalar(
            r#"SELECT listing.registration_number
               FROM plugin_submissions submission
               JOIN aircraft_sale_listings listing
                 ON listing.id = submission.canonical_listing_id
               WHERE submission.id = ?"#,
        )
        .bind(expected.submission_id)
        .fetch_optional(&mut *source)
        .await?
        .flatten();
        let registration = registration.with_context(|| {
            format!(
                "selected capture {} is not bound to a listing with an N-number",
                expected.submission_id
            )
        })?;
        let normalized = normalize_n_number(&registration).with_context(|| {
            format!(
                "selected capture {} has invalid or non-N registration {:?}",
                expected.submission_id, registration
            )
        })?;
        n_numbers.insert(normalized);
        rows.push(row);
    }
    if n_numbers.len() != rows.len() {
        bail!(
            "legacy replay manifest requires one distinct N-number per capture (captures={}, distinct_n_numbers={})",
            rows.len(),
            n_numbers.len()
        );
    }
    Ok(LegacyCaptureSelection {
        rows,
        n_numbers: n_numbers.into_iter().collect(),
    })
}

async fn validate_capture_timestamps(
    source: &mut SqliteConnection,
    row: &SourceCaptureRow,
) -> Result<()> {
    let valid: i64 = sqlx::query_scalar(
        r#"SELECT
             julianday(?) IS NOT NULL
             AND julianday(?) IS NOT NULL
             AND julianday(?) IS NOT NULL
             AND (? IS NULL OR julianday(?) IS NOT NULL)
             AND julianday(?) IS NOT NULL
             AND julianday(?) >= julianday(?)
             AND julianday(?) >= julianday(?)
             AND (? IS NULL OR julianday(?) >= julianday(?))
             AND julianday(?) >= julianday(?)
             AND (? IS NULL OR julianday(?) <= julianday(?))"#,
    )
    .bind(&row.user_created_at)
    .bind(&row.user_updated_at)
    .bind(&row.plugin_install_created_at)
    .bind(&row.plugin_install_revoked_at)
    .bind(&row.plugin_install_revoked_at)
    .bind(&row.submitted_at)
    .bind(&row.user_updated_at)
    .bind(&row.user_created_at)
    .bind(&row.plugin_install_created_at)
    .bind(&row.user_created_at)
    .bind(&row.plugin_install_revoked_at)
    .bind(&row.plugin_install_revoked_at)
    .bind(&row.plugin_install_created_at)
    .bind(&row.submitted_at)
    .bind(&row.plugin_install_created_at)
    .bind(&row.plugin_install_revoked_at)
    .bind(&row.submitted_at)
    .bind(&row.plugin_install_revoked_at)
    .fetch_one(&mut *source)
    .await?;
    if valid != 1 {
        bail!(
            "capture {} has invalid owner/install/submission timestamp chronology",
            row.submission_id
        );
    }
    Ok(())
}

async fn import_legacy_captures(
    target: &AppDb,
    selection: &LegacyCaptureSelection,
) -> Result<usize> {
    let DatabaseBackend::Sqlite(pool) = target.backend() else {
        bail!("legacy replay-source output must be SQLite");
    };
    let mut transaction = pool.begin().await?;
    for row in &selection.rows {
        let existing_user: Option<(String, String, String, String, String, String)> =
            sqlx::query_as(
                r#"SELECT email, display_name, auth_provider, auth_subject,
                          created_at, updated_at FROM users WHERE id = ?"#,
            )
            .bind(row.user_id)
            .fetch_optional(&mut *transaction)
            .await?;
        match existing_user {
            None => {
                sqlx::query(
                    r#"INSERT INTO users
                         (id, email, display_name, auth_provider, auth_subject,
                          created_at, updated_at)
                       VALUES (?, ?, ?, ?, ?, ?, ?)"#,
                )
                .bind(row.user_id)
                .bind(&row.user_email)
                .bind(&row.user_display_name)
                .bind(&row.user_auth_provider)
                .bind(&row.user_auth_subject)
                .bind(&row.user_created_at)
                .bind(&row.user_updated_at)
                .execute(&mut *transaction)
                .await?;
            }
            Some((email, display_name, provider, subject, created_at, updated_at)) => {
                if (email, display_name, provider, subject)
                    != (
                        row.user_email.clone(),
                        row.user_display_name.clone(),
                        row.user_auth_provider.clone(),
                        row.user_auth_subject.clone(),
                    )
                {
                    bail!(
                        "canonical target user id {} conflicts with selected capture owner",
                        row.user_id
                    );
                }
                if created_at != row.user_created_at || updated_at != row.user_updated_at {
                    if row.user_email != crate::db::DEVELOPER_EMAIL {
                        bail!(
                            "target user id {} has non-canonical timestamp collision",
                            row.user_id
                        );
                    }
                    sqlx::query("UPDATE users SET created_at = ?, updated_at = ? WHERE id = ?")
                        .bind(&row.user_created_at)
                        .bind(&row.user_updated_at)
                        .bind(row.user_id)
                        .execute(&mut *transaction)
                        .await?;
                }
            }
        }

        sqlx::query(
            r#"INSERT INTO plugin_installs
                 (id, user_id, public_key_base64, created_at, revoked_at)
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT (id) DO NOTHING"#,
        )
        .bind(row.plugin_install_id)
        .bind(row.user_id)
        .bind(&row.plugin_public_key_base64)
        .bind(&row.plugin_install_created_at)
        .bind(&row.plugin_install_revoked_at)
        .execute(&mut *transaction)
        .await?;
        let installed: (i64, String, String, Option<String>) = sqlx::query_as(
            "SELECT user_id, public_key_base64, created_at, revoked_at FROM plugin_installs WHERE id = ?",
        )
        .bind(row.plugin_install_id)
        .fetch_one(&mut *transaction)
        .await?;
        if installed
            != (
                row.user_id,
                row.plugin_public_key_base64.clone(),
                row.plugin_install_created_at.clone(),
                row.plugin_install_revoked_at.clone(),
            )
        {
            bail!(
                "target plugin install id {} conflicts with selected signed capture",
                row.plugin_install_id
            );
        }

        sqlx::query(
            r#"INSERT INTO plugin_submissions
                 (id, user_id, plugin_install_id, source_url, submitted_at,
                  rendered_html, rendered_html_sha256, signature_base64,
                  extracted_listing_json, extraction_error, canonical_listing_id)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL)"#,
        )
        .bind(row.submission_id)
        .bind(row.user_id)
        .bind(row.plugin_install_id)
        .bind(&row.source_url)
        .bind(&row.submitted_at)
        .bind(&row.rendered_html)
        .bind(&row.rendered_html_sha256)
        .bind(&row.signature_base64)
        .execute(&mut *transaction)
        .await?;
        let stored: (
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
        ) = sqlx::query_as(
            r#"SELECT user_id, plugin_install_id, source_url, submitted_at,
                      rendered_html, rendered_html_sha256, signature_base64,
                      extracted_listing_json, extraction_error,
                      canonical_listing_id
               FROM plugin_submissions WHERE id = ?"#,
        )
        .bind(row.submission_id)
        .fetch_one(&mut *transaction)
        .await?;
        if stored
            != (
                row.user_id,
                row.plugin_install_id,
                row.source_url.clone(),
                row.submitted_at.clone(),
                row.rendered_html.clone(),
                row.rendered_html_sha256.clone(),
                row.signature_base64.clone(),
                None,
                None,
                None,
            )
        {
            bail!(
                "target capture {} differs from its frozen source boundary",
                row.submission_id
            );
        }
    }
    transaction.commit().await?;
    Ok(selection.rows.len())
}

async fn audit_prepared_target(target: &AppDb, obsolete_hashes: &BTreeSet<String>) -> Result<()> {
    let DatabaseBackend::Sqlite(pool) = target.backend() else {
        bail!("prepared replay-source audit requires SQLite");
    };
    let integrity: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(pool)
        .await?;
    if integrity.as_slice() != ["ok"] {
        bail!(
            "prepared replay source failed SQLite integrity_check: {}",
            integrity.join("; ")
        );
    }
    let foreign_key_errors: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
            .fetch_one(pool)
            .await?;
    if foreign_key_errors != 0 {
        bail!("prepared replay source has {foreign_key_errors} foreign-key violations");
    }
    let provider_calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
        .fetch_one(pool)
        .await?;
    if provider_calls != 0 {
        bail!("prepared replay source contains {provider_calls} provider-accounting rows");
    }
    let tables = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    for table in &tables {
        if !PREPARED_REPLAY_SOURCE_NONEMPTY_TABLES.contains(&table.as_str()) {
            let count: i64 =
                sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", quote_identifier(table)))
                    .fetch_one(pool)
                    .await?;
            if count != 0 {
                bail!("prepared replay source contains {count} excluded artifact rows in {table}");
            }
        }
        let columns = sqlx::query(&format!("PRAGMA table_xinfo({})", quote_identifier(&table)))
            .fetch_all(pool)
            .await?;
        for column in columns {
            let name: String = column.try_get("name")?;
            let declared_type: String = column.try_get("type")?;
            let upper = declared_type.to_ascii_uppercase();
            let lower_name = name.to_ascii_lowercase();
            let text_bearing = upper.contains("TEXT")
                || upper.contains("CHAR")
                || upper.contains("CLOB")
                || upper.contains("BLOB")
                || lower_name.contains("json")
                || lower_name.contains("hash")
                || lower_name.contains("sha256");
            if !text_bearing {
                continue;
            }
            let sql = format!(
                "SELECT COUNT(*) FROM {} WHERE instr(CAST({} AS BLOB), ?) > 0",
                quote_identifier(&table),
                quote_identifier(&name)
            );
            for obsolete in obsolete_hashes {
                let ascii_count: i64 = sqlx::query_scalar(&sql)
                    .bind(obsolete.as_bytes())
                    .fetch_one(pool)
                    .await?;
                let binary_count: i64 = sqlx::query_scalar(&sql)
                    .bind(decode_sha256(obsolete)?)
                    .fetch_one(pool)
                    .await?;
                if ascii_count != 0 || binary_count != 0 {
                    bail!(
                        "prepared replay source retained obsolete FAA hash material in {table}.{name}"
                    );
                }
            }
        }
    }
    Ok(())
}

async fn checkpoint_prepared_target(target: &AppDb) -> Result<()> {
    let DatabaseBackend::Sqlite(pool) = target.backend() else {
        bail!("prepared replay-source checkpoint requires SQLite");
    };
    let (busy, _, _): (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(pool)
        .await?;
    if busy != 0 {
        bail!("prepared replay source WAL checkpoint remained busy");
    }
    Ok(())
}

fn decode_sha256(value: &str) -> Result<Vec<u8>> {
    if value.len() != 64
        || value != value.to_ascii_lowercase()
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("obsolete FAA taint value is not a SHA-256 digest");
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).context("SHA-256 digest is not ASCII")?;
            u8::from_str_radix(digits, 16).context("SHA-256 digest is not lowercase hexadecimal")
        })
        .collect()
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_digest_material_includes_the_final_newline() {
        let actual = hash_rows(["one\u{1f}two".to_string()]);
        let mut expected = Sha3_256::new();
        expected.update(b"one\x1ftwo\n");
        assert_eq!(actual, format!("{:x}", expected.finalize()));
    }

    #[test]
    fn taint_digest_decodes_to_exact_binary_sha256() {
        let digest = format!("00{}ff", "ab".repeat(30));
        let decoded = decode_sha256(&digest).unwrap();
        assert_eq!(decoded.len(), 32);
        assert_eq!(decoded[0], 0);
        assert!(decoded[1..31].iter().all(|byte| *byte == 0xab));
        assert_eq!(decoded[31], 0xff);
        assert!(decode_sha256("not-a-digest").is_err());
    }

    #[test]
    fn prepared_output_allowlist_excludes_every_derived_listing_artifact_class() {
        for excluded in [
            "aircraft_sale_listings",
            "aircraft_sale_listing_identity_assignments",
            "aircraft_listing_identity_correction_decisions",
            "aircraft_sale_listing_pending_reviews",
            "listing_replay_runs",
            "listing_verification_runs",
            "valuation_snapshots",
            "valuation_model_versions",
            "gemini_api_usage",
            "aircraft_identity_resolution_candidates",
            "avionics_manufacturer_alias_candidates",
        ] {
            assert!(!PREPARED_REPLAY_SOURCE_NONEMPTY_TABLES.contains(&excluded));
        }
        for retained in [
            "plugin_submissions",
            "faa_registry_snapshots",
            "aircraft_identity_decisions",
            "avionics_models",
        ] {
            assert!(PREPARED_REPLAY_SOURCE_NONEMPTY_TABLES.contains(&retained));
        }
    }

    #[test]
    fn legacy_source_url_rejects_uri_options() {
        assert!(sqlite_path("sqlite:///tmp/frozen.sqlite3?mode=rw").is_err());
        assert!(sqlite_path("sqlite:///tmp/frozen.sqlite3#fragment").is_err());
        assert_eq!(
            sqlite_path("sqlite:///tmp/frozen.sqlite3").unwrap(),
            PathBuf::from("/tmp/frozen.sqlite3")
        );
    }
}
