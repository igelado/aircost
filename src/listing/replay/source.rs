//! Isolated conversion of the one attested legacy SQLite source used by the
//! clean-replay rebuild.
//!
//! This is an administrative conversion boundary, not a runtime compatibility
//! path. Unknown source shapes fail closed and the source is always opened
//! read-only.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::Sha256;
use sha3::{Digest, Sha3_256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Acquire, FromRow, QueryBuilder, Row, Sqlite, SqliteConnection, SqlitePool};
use tempfile::{NamedTempFile, TempPath};

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
pub const LEGACY_SOURCE_DATABASE_SHA256: &str =
    "3468cd90ff2799d3640764ed0097dd07aa28164b249a4a9134e646e98158f8fc";
pub const LEGACY_SOURCE_DATABASE_BYTES: u64 = 50_282_496;
pub const LEGACY_CAPTURE_MANIFEST_SHA256: &str =
    "345b1566ec491488d3ba4d1db2855eb9ea8e9b1258a7fc799418c581581b5d00";
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
    pub expected_source_database_sha256: &'a str,
    pub manifest: &'a TrustedCaptureManifest,
    pub expected_manifest_sha256: &'a str,
    pub faa_archive: &'a Path,
    pub expected_faa_archive_sha256: &'a str,
    pub output: &'a Path,
    pub apply: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrepareLegacyReplaySourceReport {
    pub dry_run: bool,
    pub provider_calls: u64,
    pub source_database_sha256: String,
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

type StoredCaptureBoundary = (
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
);

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

struct FrozenSourceSnapshot {
    path: TempPath,
    sha256: String,
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
    let frozen_source = snapshot_frozen_source(request.source_database).await?;
    let source_pool = open_frozen_snapshot(&frozen_source.path).await?;
    let mut source_connection = source_pool.acquire().await?;
    let mut source_snapshot = source_connection.begin().await?;
    let attestation = attest_frozen_source(&mut source_snapshot).await?;
    let captures = load_legacy_capture_selection(&mut source_snapshot, request.manifest).await?;
    let representatives = required_faa_representatives(&mut source_snapshot).await?;

    let output_parent = output_parent(request.output)?;
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
    let source_database_sha256 = frozen_source.sha256;
    drop(frozen_source.path);

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
        persist_prepared_output(file, request.output)?;
        true
    } else {
        false
    };
    Ok(PrepareLegacyReplaySourceReport {
        dry_run: !request.apply,
        provider_calls: 0,
        source_database_sha256,
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
    validate_source_database_boundary(request.expected_source_database_sha256)?;
    validate_manifest_boundary(request.manifest, request.expected_manifest_sha256)?;
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
    let parent = output_parent(request.output)?;
    if !parent.is_dir() {
        bail!("prepared replay-source output parent does not exist");
    }
    let output_url = database_url_from_arg(Some(
        parent
            .join(
                request
                    .output
                    .file_name()
                    .context("prepared replay-source output has no file name")?,
            )
            .to_string_lossy()
            .into_owned(),
    ));
    if sqlite_database_urls_equal(request.source_database, &output_url)? {
        bail!("legacy source and prepared replay-source output must be different files");
    }
    Ok(())
}

fn validate_source_database_boundary(expected_sha256: &str) -> Result<()> {
    if expected_sha256 != LEGACY_SOURCE_DATABASE_SHA256 {
        bail!(
            "legacy replay-source bridge accepts only frozen source database SHA-256 {LEGACY_SOURCE_DATABASE_SHA256}"
        );
    }
    Ok(())
}

fn validate_manifest_boundary(
    manifest: &TrustedCaptureManifest,
    expected_sha256: &str,
) -> Result<()> {
    validate_trusted_capture_manifest(manifest).map_err(anyhow::Error::msg)?;
    if expected_sha256 != LEGACY_CAPTURE_MANIFEST_SHA256 {
        bail!(
            "legacy replay-source bridge accepts only capture manifest SHA-256 {LEGACY_CAPTURE_MANIFEST_SHA256}"
        );
    }
    if manifest.manifest_sha256 != expected_sha256 {
        bail!(
            "legacy replay-source manifest fingerprint {} does not match required fingerprint {expected_sha256}",
            manifest.manifest_sha256
        );
    }
    Ok(())
}

fn output_parent(output: &Path) -> Result<&Path> {
    if output.file_name().is_none() {
        bail!("prepared replay-source output has no file name");
    }
    Ok(output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new(".")))
}

async fn snapshot_frozen_source(source_database: &str) -> Result<FrozenSourceSnapshot> {
    let source_url = database_url_from_arg(Some(source_database.to_string()));
    if source_url == "sqlite::memory:"
        || source_url.starts_with("postgres://")
        || source_url.starts_with("postgresql://")
        || !source_url.starts_with("sqlite://")
    {
        bail!("legacy replay source must be a file-backed SQLite database");
    }
    let source_path = sqlite_path(&source_url)?;
    tokio::task::spawn_blocking(move || {
        copy_private_source_snapshot(
            &source_path,
            LEGACY_SOURCE_DATABASE_SHA256,
            LEGACY_SOURCE_DATABASE_BYTES,
        )
    })
    .await
    .context("frozen-source snapshot worker failed")?
}

fn copy_private_source_snapshot(
    source_path: &Path,
    expected_sha256: &str,
    expected_bytes: u64,
) -> Result<FrozenSourceSnapshot> {
    let initial_path_metadata = inspect_unaliased_source_path(source_path)?;
    reject_sqlite_sidecars(&source_path)?;
    reject_changed_source_path(source_path, &initial_path_metadata)?;
    let mut source = OpenOptions::new()
        .read(true)
        .open(&source_path)
        .with_context(|| {
            format!(
                "could not open frozen legacy replay source {}",
                source_path.display()
            )
        })?;
    let opened_metadata = source.metadata().with_context(|| {
        format!(
            "could not inspect legacy replay source {}",
            source_path.display()
        )
    })?;
    if !opened_metadata.is_file() {
        bail!("legacy replay source must be a regular SQLite file");
    }
    if opened_metadata.nlink() != 1 {
        bail!("legacy replay source must have exactly one hard link");
    }
    if opened_metadata.dev() != initial_path_metadata.dev()
        || opened_metadata.ino() != initial_path_metadata.ino()
        || opened_metadata.len() != initial_path_metadata.len()
    {
        bail!(
            "frozen legacy replay source path changed while it was being opened: {}",
            source_path.display()
        );
    }
    if opened_metadata.len() != expected_bytes {
        bail!(
            "frozen legacy replay source size {} does not match required size {expected_bytes}",
            opened_metadata.len()
        );
    }

    let mut snapshot =
        NamedTempFile::new().context("could not create private frozen-source snapshot")?;
    snapshot
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .context("could not restrict frozen-source snapshot permissions")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    let mut copied_bytes = 0_u64;
    loop {
        let read = source.read(&mut buffer).with_context(|| {
            format!(
                "could not read frozen legacy replay source {}",
                source_path.display()
            )
        })?;
        if read == 0 {
            break;
        }
        copied_bytes = copied_bytes
            .checked_add(read as u64)
            .context("frozen legacy replay source byte count overflowed")?;
        if copied_bytes > expected_bytes {
            bail!("frozen legacy replay source grew while it was being snapshotted");
        }
        snapshot
            .write_all(&buffer[..read])
            .context("could not write private frozen-source snapshot")?;
        hasher.update(&buffer[..read]);
    }
    snapshot
        .flush()
        .context("could not flush private frozen-source snapshot")?;
    snapshot
        .as_file()
        .sync_all()
        .context("could not synchronize private frozen-source snapshot")?;
    reject_changed_source_path(&source_path, &opened_metadata)?;
    reject_sqlite_sidecars(&source_path)?;
    reject_changed_source_path(&source_path, &opened_metadata)?;
    if copied_bytes != expected_bytes {
        bail!(
            "frozen legacy replay source copy has {copied_bytes} bytes instead of {expected_bytes}"
        );
    }
    let sha256 = format!("{:x}", hasher.finalize());
    if sha256 != expected_sha256 {
        bail!(
            "frozen legacy replay source SHA-256 {sha256} does not match required SHA-256 {expected_sha256}"
        );
    }
    Ok(FrozenSourceSnapshot {
        path: snapshot.into_temp_path(),
        sha256,
    })
}

fn inspect_unaliased_source_path(source_path: &Path) -> Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(source_path).with_context(|| {
        format!(
            "could not inspect frozen legacy replay source path {}",
            source_path.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        bail!(
            "legacy replay source path must be a regular file, not a symbolic link or special file: {}",
            source_path.display()
        );
    }
    if metadata.nlink() != 1 {
        bail!(
            "legacy replay source path must have exactly one hard link: {}",
            source_path.display()
        );
    }
    Ok(metadata)
}

async fn open_frozen_snapshot(snapshot_path: &Path) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(snapshot_path)
        .create_if_missing(false)
        .read_only(true)
        .immutable(true)
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
                "could not open private frozen-source snapshot {} read-only",
                snapshot_path.display()
            )
        })?;
    Ok(pool)
}

fn reject_sqlite_sidecars(source_path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = source_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(_) => bail!(
                "frozen legacy replay source has forbidden SQLite sidecar {}",
                sidecar.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not inspect SQLite sidecar {}", sidecar.display())
                });
            }
        }
    }
    Ok(())
}

fn reject_changed_source_path(source_path: &Path, opened: &std::fs::Metadata) -> Result<()> {
    let current = std::fs::symlink_metadata(source_path).with_context(|| {
        format!(
            "frozen legacy replay source path changed while it was being snapshotted: {}",
            source_path.display()
        )
    })?;
    if !current.file_type().is_file()
        || current.nlink() != 1
        || current.dev() != opened.dev()
        || current.ino() != opened.ino()
        || current.len() != opened.len()
    {
        bail!(
            "frozen legacy replay source path changed while it was being snapshotted: {}",
            source_path.display()
        );
    }
    Ok(())
}

fn persist_prepared_output(file: NamedTempFile, output: &Path) -> Result<()> {
    file.as_file().sync_all().with_context(|| {
        format!(
            "could not synchronize prepared replay source temporary file for {}",
            output.display()
        )
    })?;
    let published = file.persist_noclobber(output).with_context(|| {
        format!(
            "could not atomically publish prepared replay source {}",
            output.display()
        )
    })?;
    published.sync_all().with_context(|| {
        format!(
            "could not synchronize published replay source {}",
            output.display()
        )
    })?;
    drop(published);
    sync_parent_directory(output_parent(output)?)?;
    Ok(())
}

fn sync_parent_directory(parent: &Path) -> Result<()> {
    File::open(parent)
        .with_context(|| format!("could not open output parent {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("could not synchronize output parent {}", parent.display()))
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
        let stored: StoredCaptureBoundary = sqlx::query_as(
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
    let obsolete_needles = obsolete_hashes
        .iter()
        .map(|digest| {
            Ok((
                digest.as_bytes().to_vec(),
                decode_sha256(digest).with_context(|| {
                    format!("could not decode legacy FAA taint digest {digest}")
                })?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
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
        let columns = sqlx::query(&format!("PRAGMA table_xinfo({})", quote_identifier(table)))
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
            for needles in obsolete_needles.chunks(200) {
                if column_contains_any_taint(pool, table, &name, needles).await? {
                    bail!(
                        "prepared replay source retained obsolete FAA hash material in {table}.{name}"
                    );
                }
            }
        }
    }
    Ok(())
}

async fn column_contains_any_taint(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    needles: &[(Vec<u8>, Vec<u8>)],
) -> Result<bool> {
    if needles.is_empty() {
        return Ok(false);
    }
    let mut query =
        QueryBuilder::<Sqlite>::new("WITH obsolete(ascii_digest, binary_digest) AS (VALUES ");
    for (index, (ascii, binary)) in needles.iter().enumerate() {
        if index != 0 {
            query.push(", ");
        }
        query
            .push("(")
            .push_bind(ascii.clone())
            .push(", ")
            .push_bind(binary.clone())
            .push(")");
    }
    query
        .push(") SELECT EXISTS (SELECT 1 FROM ")
        .push(quote_identifier(table))
        .push(" CROSS JOIN obsolete WHERE instr(CAST(")
        .push(quote_identifier(column))
        .push(" AS BLOB), obsolete.ascii_digest) > 0 OR instr(CAST(")
        .push(quote_identifier(column))
        .push(" AS BLOB), obsolete.binary_digest) > 0)");
    let present: i64 = query.build_query_scalar().fetch_one(pool).await?;
    Ok(present != 0)
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

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn self_consistent_subset_manifest() -> TrustedCaptureManifest {
        let captures = vec![super::super::TrustedCaptureEntry {
            submission_id: 1,
            user_id: 1,
            user_email: "review@example.test".into(),
            user_display_name: "Review".into(),
            user_auth_provider: "local".into(),
            user_auth_subject: "review".into(),
            plugin_install_id: 1,
            plugin_public_key_base64: "key".into(),
            plugin_install_created_at: "2026-01-01 00:00:00".into(),
            plugin_install_revoked_at: None,
            source_url: "https://example.test/listing".into(),
            submitted_at: "2026-01-02 00:00:00".into(),
            rendered_html_sha256: "a".repeat(64),
            signature_base64: "signature".into(),
        }];
        TrustedCaptureManifest {
            version: 1,
            manifest_sha256: super::super::manifest_fingerprint(&captures).unwrap(),
            captures,
        }
    }

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
    fn frozen_source_boundary_accepts_only_the_reviewed_digest() {
        validate_source_database_boundary(LEGACY_SOURCE_DATABASE_SHA256).unwrap();
        for rejected in ["A".repeat(64), "0".repeat(64), "not-a-digest".into()] {
            let error = validate_source_database_boundary(&rejected).unwrap_err();
            assert!(error.to_string().contains(LEGACY_SOURCE_DATABASE_SHA256));
        }
    }

    #[test]
    fn self_consistent_subset_manifest_cannot_cross_the_reviewed_boundary() {
        let manifest = self_consistent_subset_manifest();
        validate_trusted_capture_manifest(&manifest).unwrap();
        let error =
            validate_manifest_boundary(&manifest, LEGACY_CAPTURE_MANIFEST_SHA256).unwrap_err();
        assert!(error
            .to_string()
            .contains("does not match required fingerprint"));
    }

    #[test]
    fn private_snapshot_authenticates_the_copied_bytes_and_mode() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.sqlite3");
        let bytes = b"exact frozen source bytes";
        std::fs::write(&source_path, bytes).unwrap();
        let snapshot =
            copy_private_source_snapshot(&source_path, &sha256(bytes), bytes.len() as u64).unwrap();
        assert_eq!(std::fs::read(&snapshot.path).unwrap(), bytes);
        assert_eq!(snapshot.sha256, sha256(bytes));
        assert_eq!(
            std::fs::metadata(&snapshot.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn same_size_data_tampering_is_rejected_by_the_source_digest() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.sqlite3");
        let reviewed = b"reviewed";
        std::fs::write(&source_path, reviewed).unwrap();
        let reviewed_sha256 = sha256(reviewed);
        std::fs::write(&source_path, b"tampered").unwrap();
        let error =
            copy_private_source_snapshot(&source_path, &reviewed_sha256, reviewed.len() as u64)
                .err()
                .expect("same-size tampering must fail");
        assert!(error
            .to_string()
            .contains("does not match required SHA-256"));
    }

    #[test]
    fn every_sqlite_sidecar_shape_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.sqlite3");
        let bytes = b"source";
        std::fs::write(&source_path, bytes).unwrap();
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = source_path.as_os_str().to_os_string();
            sidecar.push(suffix);
            let sidecar = PathBuf::from(sidecar);
            std::fs::write(&sidecar, []).unwrap();
            let error =
                copy_private_source_snapshot(&source_path, &sha256(bytes), bytes.len() as u64)
                    .err()
                    .expect("sidecar must fail closed");
            assert!(error.to_string().contains("forbidden SQLite sidecar"));
            std::fs::remove_file(sidecar).unwrap();
        }

        let mut dangling = source_path.as_os_str().to_os_string();
        dangling.push("-wal");
        std::os::unix::fs::symlink(directory.path().join("missing"), &dangling).unwrap();
        let error = copy_private_source_snapshot(&source_path, &sha256(bytes), bytes.len() as u64)
            .err()
            .expect("dangling sidecar must fail closed");
        assert!(error.to_string().contains("forbidden SQLite sidecar"));
    }

    #[test]
    fn symlink_source_cannot_hide_a_real_target_wal() {
        let directory = tempfile::tempdir().unwrap();
        let real_source = directory.path().join("real.sqlite3");
        let alias_source = directory.path().join("alias.sqlite3");
        let bytes = b"source";
        std::fs::write(&real_source, bytes).unwrap();
        std::fs::write(directory.path().join("real.sqlite3-wal"), b"uncheckpointed").unwrap();
        std::os::unix::fs::symlink(&real_source, &alias_source).unwrap();

        let error = copy_private_source_snapshot(&alias_source, &sha256(bytes), bytes.len() as u64)
            .err()
            .expect("a symlink source must fail before alias-relative sidecar checks");
        assert!(error.to_string().contains("symbolic link"));
    }

    #[test]
    fn hard_link_alias_source_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let real_source = directory.path().join("real.sqlite3");
        let alias_source = directory.path().join("alias.sqlite3");
        let bytes = b"source";
        std::fs::write(&real_source, bytes).unwrap();
        std::fs::hard_link(&real_source, &alias_source).unwrap();

        let error = copy_private_source_snapshot(&alias_source, &sha256(bytes), bytes.len() as u64)
            .err()
            .expect("a multiply-linked source inode must fail closed");
        assert!(error.to_string().contains("exactly one hard link"));
    }

    #[tokio::test]
    async fn immutable_snapshot_survives_source_replacement_and_rejects_writes() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.sqlite3");
        let options = SqliteConnectOptions::new()
            .filename(&source_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE retained (value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO retained (value) VALUES ('exact')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        reject_sqlite_sidecars(&source_path).unwrap();
        let source_bytes = std::fs::read(&source_path).unwrap();
        let snapshot = copy_private_source_snapshot(
            &source_path,
            &sha256(&source_bytes),
            source_bytes.len() as u64,
        )
        .unwrap();

        let displaced = directory.path().join("displaced.sqlite3");
        std::fs::rename(&source_path, displaced).unwrap();
        std::fs::write(&source_path, b"replacement").unwrap();
        let snapshot_pool = open_frozen_snapshot(&snapshot.path).await.unwrap();
        let value: String = sqlx::query_scalar("SELECT value FROM retained")
            .fetch_one(&snapshot_pool)
            .await
            .unwrap();
        assert_eq!(value, "exact");
        assert!(
            sqlx::query("INSERT INTO retained (value) VALUES ('forbidden')")
                .execute(&snapshot_pool)
                .await
                .is_err()
        );
        snapshot_pool.close().await;
        reject_sqlite_sidecars(&snapshot.path).unwrap();
    }

    #[test]
    fn publication_syncs_the_parent_and_remains_no_clobber() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("prepared.sqlite3");
        let mut first = NamedTempFile::new_in(directory.path()).unwrap();
        first.write_all(b"prepared").unwrap();
        persist_prepared_output(first, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"prepared");

        let mut second = NamedTempFile::new_in(directory.path()).unwrap();
        second.write_all(b"replacement").unwrap();
        assert!(persist_prepared_output(second, &output).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"prepared");
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

    #[test]
    fn relative_output_file_uses_the_current_directory() {
        assert_eq!(
            output_parent(Path::new("prepared.sqlite3")).unwrap(),
            Path::new(".")
        );
        assert_eq!(
            output_parent(Path::new("artifacts/prepared.sqlite3")).unwrap(),
            Path::new("artifacts")
        );
        assert!(output_parent(Path::new("/")).is_err());
    }

    #[tokio::test]
    async fn batched_taint_scan_finds_ascii_and_binary_digests() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE retained (text_value TEXT, blob_value BLOB)")
            .execute(&pool)
            .await
            .unwrap();
        let digest = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let needles = vec![(digest.as_bytes().to_vec(), decode_sha256(digest).unwrap())];
        assert!(
            !column_contains_any_taint(&pool, "retained", "text_value", &needles)
                .await
                .unwrap()
        );

        sqlx::query("INSERT INTO retained (text_value) VALUES (?)")
            .bind(format!("typed receipt {digest}"))
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            column_contains_any_taint(&pool, "retained", "text_value", &needles)
                .await
                .unwrap()
        );

        sqlx::query("DELETE FROM retained")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO retained (blob_value) VALUES (?)")
            .bind([b"prefix".as_slice(), needles[0].1.as_slice()].concat())
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            column_contains_any_taint(&pool, "retained", "blob_value", &needles)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn fresh_current_schema_satisfies_the_prepared_output_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("prepared.sqlite3");
        let database_url = format!("sqlite://{}", database_path.display());
        let target = AppDb::connect(&database_url).await.unwrap();
        audit_prepared_target(&target, &BTreeSet::new())
            .await
            .unwrap();
        target.close().await;
    }
}
