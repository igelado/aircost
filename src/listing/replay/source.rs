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
use sqlx::{FromRow, SqliteConnection, SqlitePool};

use super::{
    entry_from_row, validate_source_capture, validate_trusted_capture_manifest, SourceCaptureRow,
    TrustedCaptureManifest,
};
use crate::aircraft::faa::normalize_n_number;
use crate::db::{database_url_from_arg, AppDb, DatabaseBackend};

pub const LEGACY_SQLITE_SCHEMA_OBJECT_COUNT: usize = 575;
pub const LEGACY_SQLITE_SCHEMA_SHA3_256: &str =
    "527552c4fbe674eaca5de3a1228bfcde0fd99f05c7f2924f28ffdba687ec5957";
pub const LEGACY_SCHEMA_RECEIPT_COUNT: usize = 19;
pub const LEGACY_SCHEMA_RECEIPT_SHA3_256: &str =
    "7081e6098b4e11367b7a371301c4b6ff1e916104174d65906a723c41284dd639";
pub const LEGACY_FAA_ARCHIVE_SHA256: &str =
    "14885735825e5f46babdac8bf851c77c7ce7b104ae0f86395ef594e6e467c724";

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
    _request: PrepareLegacyReplaySourceRequest<'_>,
) -> Result<PrepareLegacyReplaySourceReport> {
    anyhow::bail!("legacy replay-source bridge is not yet initialized")
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
