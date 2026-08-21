//! Clean replay of an explicit set of signed plugin captures.
//!
//! The source database remains read-only. Import is allowed only into a target
//! with no captures, copies exactly the manifest selection, and resets every
//! derived submission field. Existing listings, catalog rows, reviews, and
//! provider artifacts are never copied by this workflow.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::extract::validate_source_url;
use crate::plugin::{sha256_hex, verify_submission_signature};

pub mod run;
pub mod source;

pub use crate::listing::avionics::disposition::OccurrenceDispositionReconciliation;

pub async fn reconcile_replay_occurrence_dispositions(
    db: &AppDb,
    listing_id: i64,
    submission_id: i64,
    actor_user_id: i64,
    apply: bool,
) -> Result<OccurrenceDispositionReconciliation, String> {
    crate::listing::avionics::disposition::reconcile_bound_occurrence_dispositions(
        db,
        listing_id,
        submission_id,
        actor_user_id,
        apply,
    )
    .await
}

const MANIFEST_VERSION: u32 = 1;
const MANIFEST_HASH_DOMAIN: &[u8] = b"aircost:trusted-capture-manifest:v1\0";
const MAX_CAPTURE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrustedCaptureManifest {
    pub version: u32,
    pub captures: Vec<TrustedCaptureEntry>,
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrustedCaptureEntry {
    pub submission_id: i64,
    pub user_id: i64,
    pub user_email: String,
    pub user_display_name: String,
    pub user_auth_provider: String,
    pub user_auth_subject: String,
    pub plugin_install_id: i64,
    pub plugin_public_key_base64: String,
    pub plugin_install_created_at: String,
    pub plugin_install_revoked_at: Option<String>,
    pub source_url: String,
    pub submitted_at: String,
    pub rendered_html_sha256: String,
    pub signature_base64: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CaptureImportReport {
    pub selected_capture_count: usize,
    pub selected_user_count: usize,
    pub selected_plugin_install_count: usize,
    pub imported_capture_count: usize,
    pub derived_fields_reset: bool,
    pub dry_run: bool,
}

pub async fn trusted_bound_capture_ids(source: &AppDb) -> Result<Vec<i64>, String> {
    let ambiguous_sql = source.sql(
        r#"
        SELECT listing.id
        FROM aircraft_sale_listings listing
        LEFT JOIN plugin_submissions submission
          ON submission.canonical_listing_id = listing.id
        GROUP BY listing.id
        HAVING COUNT(submission.id) <> 1
        ORDER BY listing.id
        "#,
    );
    let ambiguous: Vec<i64> = match source.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(&ambiguous_sql).fetch_all(pool).await,
        DatabaseBackend::Postgres(pool) => sqlx::query_scalar(&ambiguous_sql).fetch_all(pool).await,
    }
    .map_err(database_error)?;
    if !ambiguous.is_empty() {
        return Err(format!(
            "each replay listing must have exactly one bound capture; ambiguous listing IDs: {}",
            ambiguous
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    let selected_sql = source.sql(
        r#"
        SELECT submission.id
        FROM aircraft_sale_listings listing
        JOIN plugin_submissions submission
          ON submission.canonical_listing_id = listing.id
        ORDER BY listing.id
        "#,
    );
    let selected = match source.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(&selected_sql).fetch_all(pool).await,
        DatabaseBackend::Postgres(pool) => sqlx::query_scalar(&selected_sql).fetch_all(pool).await,
    }
    .map_err(database_error)?;
    validated_selection(&selected)
}

#[derive(Clone, Debug, FromRow)]
struct SourceCaptureRow {
    submission_id: i64,
    user_id: i64,
    user_email: String,
    user_display_name: String,
    user_auth_provider: String,
    user_auth_subject: String,
    user_created_at: String,
    user_updated_at: String,
    plugin_install_id: i64,
    plugin_public_key_base64: String,
    plugin_install_created_at: String,
    plugin_install_revoked_at: Option<String>,
    source_url: String,
    submitted_at: String,
    rendered_html: String,
    rendered_html_sha256: String,
    signature_base64: String,
}

pub async fn build_trusted_capture_manifest(
    source: &AppDb,
    submission_ids: &[i64],
) -> Result<TrustedCaptureManifest, String> {
    let selection = validated_selection(submission_ids)?;
    let mut captures = Vec::with_capacity(selection.len());
    for submission_id in selection {
        let row = load_source_capture(source, submission_id).await?;
        validate_source_capture(&row)?;
        captures.push(entry_from_row(&row));
    }
    let manifest_sha256 = manifest_fingerprint(&captures)?;
    Ok(TrustedCaptureManifest {
        version: MANIFEST_VERSION,
        captures,
        manifest_sha256,
    })
}

pub fn validate_trusted_capture_manifest(manifest: &TrustedCaptureManifest) -> Result<(), String> {
    if manifest.version != MANIFEST_VERSION {
        return Err(format!(
            "unsupported trusted capture manifest version {}",
            manifest.version
        ));
    }
    let ids = manifest
        .captures
        .iter()
        .map(|entry| entry.submission_id)
        .collect::<Vec<_>>();
    let selection = validated_selection(&ids)?;
    if ids != selection {
        return Err("trusted capture manifest entries must be sorted by submission ID".to_string());
    }
    if manifest.manifest_sha256 != manifest_fingerprint(&manifest.captures)? {
        return Err("trusted capture manifest fingerprint does not match its entries".to_string());
    }
    Ok(())
}

pub async fn import_trusted_capture_manifest(
    source: &AppDb,
    target: &AppDb,
    manifest: &TrustedCaptureManifest,
    apply: bool,
) -> Result<CaptureImportReport, String> {
    validate_trusted_capture_manifest(manifest)?;
    let mut rows = Vec::with_capacity(manifest.captures.len());
    for expected in &manifest.captures {
        let row = load_source_capture(source, expected.submission_id).await?;
        validate_source_capture(&row)?;
        if entry_from_row(&row) != *expected {
            return Err(format!(
                "source capture {} changed after the manifest was created",
                expected.submission_id
            ));
        }
        rows.push(row);
    }

    let selected_user_count = rows
        .iter()
        .map(|row| row.user_id)
        .collect::<BTreeSet<_>>()
        .len();
    let selected_plugin_install_count = rows
        .iter()
        .map(|row| row.plugin_install_id)
        .collect::<BTreeSet<_>>()
        .len();
    if !apply {
        return Ok(CaptureImportReport {
            selected_capture_count: rows.len(),
            selected_user_count,
            selected_plugin_install_count,
            imported_capture_count: 0,
            derived_fields_reset: true,
            dry_run: true,
        });
    }

    let existing = count_target_captures(target).await?;
    if existing != 0 {
        return Err(format!(
            "clean replay target already contains {existing} plugin submissions"
        ));
    }

    macro_rules! insert_rows {
        ($transaction:expr) => {{
            for row in &rows {
                let insert_user = target.sql(
                    r#"
                    INSERT INTO users (
                      id, email, display_name, auth_provider, auth_subject, created_at, updated_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT (id) DO NOTHING
                    "#,
                );
                sqlx::query(&insert_user)
                    .bind(row.user_id)
                    .bind(&row.user_email)
                    .bind(&row.user_display_name)
                    .bind(&row.user_auth_provider)
                    .bind(&row.user_auth_subject)
                    .bind(&row.user_created_at)
                    .bind(&row.user_updated_at)
                    .execute(&mut **$transaction)
                    .await
                    .map_err(database_error)?;
                let stored_user: (String, String, String, String) = sqlx::query_as(
                    &target.sql(
                        "SELECT email, display_name, auth_provider, auth_subject FROM users WHERE id = ?",
                    ),
                )
                .bind(row.user_id)
                .fetch_one(&mut **$transaction)
                .await
                .map_err(database_error)?;
                if stored_user
                    != (
                        row.user_email.clone(),
                        row.user_display_name.clone(),
                        row.user_auth_provider.clone(),
                        row.user_auth_subject.clone(),
                    )
                {
                    return Err(format!(
                        "target user id {} conflicts with the selected capture owner",
                        row.user_id
                    ));
                }

                sqlx::query(&target.sql(
                    r#"
                    INSERT INTO plugin_installs (id, user_id, public_key_base64, created_at, revoked_at)
                    VALUES (?, ?, ?, ?, ?)
                    ON CONFLICT (id) DO NOTHING
                    "#,
                ))
                .bind(row.plugin_install_id)
                .bind(row.user_id)
                .bind(&row.plugin_public_key_base64)
                .bind(&row.plugin_install_created_at)
                .bind(&row.plugin_install_revoked_at)
                .execute(&mut **$transaction)
                .await
                .map_err(database_error)?;
                let stored_install: (i64, String, Option<String>) = sqlx::query_as(
                    &target.sql(
                        "SELECT user_id, public_key_base64, revoked_at FROM plugin_installs WHERE id = ?",
                    ),
                )
                .bind(row.plugin_install_id)
                .fetch_one(&mut **$transaction)
                .await
                .map_err(database_error)?;
                if stored_install
                    != (
                        row.user_id,
                        row.plugin_public_key_base64.clone(),
                        row.plugin_install_revoked_at.clone(),
                    )
                {
                    return Err(format!(
                        "target plugin install id {} conflicts with the selected signed capture",
                        row.plugin_install_id
                    ));
                }

                sqlx::query(&target.sql(
                    r#"
                    INSERT INTO plugin_submissions (
                      id, user_id, plugin_install_id, source_url, submitted_at, rendered_html,
                      rendered_html_sha256, signature_base64, extracted_listing_json,
                      extraction_error, canonical_listing_id
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL)
                    "#,
                ))
                .bind(row.submission_id)
                .bind(row.user_id)
                .bind(row.plugin_install_id)
                .bind(&row.source_url)
                .bind(&row.submitted_at)
                .bind(&row.rendered_html)
                .bind(&row.rendered_html_sha256)
                .bind(&row.signature_base64)
                .execute(&mut **$transaction)
                .await
                .map_err(database_error)?;
            }
        }};
    }

    match target.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await.map_err(database_error)?;
            insert_rows!(&mut transaction);
            transaction.commit().await.map_err(database_error)?;
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await.map_err(database_error)?;
            insert_rows!(&mut transaction);
            for table in ["users", "plugin_installs", "plugin_submissions"] {
                let sql = format!(
                    "SELECT setval(pg_get_serial_sequence('{table}', 'id'), COALESCE((SELECT MAX(id) FROM {table}), 1), (SELECT COUNT(*) > 0 FROM {table}))"
                );
                sqlx::query(&sql)
                    .execute(&mut *transaction)
                    .await
                    .map_err(database_error)?;
            }
            transaction.commit().await.map_err(database_error)?;
        }
    }

    Ok(CaptureImportReport {
        selected_capture_count: rows.len(),
        selected_user_count,
        selected_plugin_install_count,
        imported_capture_count: rows.len(),
        derived_fields_reset: true,
        dry_run: false,
    })
}

fn validated_selection(submission_ids: &[i64]) -> Result<Vec<i64>, String> {
    if submission_ids.is_empty() {
        return Err("at least one explicit --submission-id is required".to_string());
    }
    if submission_ids.iter().any(|id| *id <= 0) {
        return Err("submission IDs must be positive".to_string());
    }
    let selection = submission_ids.iter().copied().collect::<BTreeSet<_>>();
    if selection.len() != submission_ids.len() {
        return Err("submission selection contains duplicate IDs".to_string());
    }
    Ok(selection.into_iter().collect())
}

fn entry_from_row(row: &SourceCaptureRow) -> TrustedCaptureEntry {
    TrustedCaptureEntry {
        submission_id: row.submission_id,
        user_id: row.user_id,
        user_email: row.user_email.clone(),
        user_display_name: row.user_display_name.clone(),
        user_auth_provider: row.user_auth_provider.clone(),
        user_auth_subject: row.user_auth_subject.clone(),
        plugin_install_id: row.plugin_install_id,
        plugin_public_key_base64: row.plugin_public_key_base64.clone(),
        plugin_install_created_at: row.plugin_install_created_at.clone(),
        plugin_install_revoked_at: row.plugin_install_revoked_at.clone(),
        source_url: row.source_url.clone(),
        submitted_at: row.submitted_at.clone(),
        rendered_html_sha256: row.rendered_html_sha256.clone(),
        signature_base64: row.signature_base64.clone(),
    }
}

fn validate_source_capture(row: &SourceCaptureRow) -> Result<(), String> {
    if row.submission_id <= 0 || row.user_id <= 0 || row.plugin_install_id <= 0 {
        return Err("capture, owner, and plugin install IDs must be positive".to_string());
    }
    validate_source_url(&row.source_url).map_err(|error| error.to_string())?;
    if row.rendered_html.trim().is_empty() || row.rendered_html.len() > MAX_CAPTURE_BYTES {
        return Err(format!(
            "capture {} HTML is empty or exceeds the admission limit",
            row.submission_id
        ));
    }
    let recomputed = sha256_hex(row.rendered_html.as_bytes());
    if recomputed != row.rendered_html_sha256 {
        return Err(format!(
            "capture {} rendered HTML hash is corrupt",
            row.submission_id
        ));
    }
    verify_submission_signature(
        &row.plugin_public_key_base64,
        row.plugin_install_id,
        &row.source_url,
        &recomputed,
        &row.signature_base64,
    )
    .map_err(|error| format!("capture {}: {error}", row.submission_id))
}

fn manifest_fingerprint(entries: &[TrustedCaptureEntry]) -> Result<String, String> {
    let encoded = serde_json::to_vec(entries)
        .map_err(|error| format!("could not serialize trusted capture manifest: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(MANIFEST_HASH_DOMAIN);
    hasher.update(encoded);
    Ok(format!("{:x}", hasher.finalize()))
}

async fn load_source_capture(
    source: &AppDb,
    submission_id: i64,
) -> Result<SourceCaptureRow, String> {
    let sql = source.sql(
        r#"
        SELECT submission.id AS submission_id,
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
        WHERE submission.id = ?
        "#,
    );
    let row = match source.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, SourceCaptureRow>(&sql)
                .bind(submission_id)
                .fetch_optional(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, SourceCaptureRow>(&sql)
                .bind(submission_id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(database_error)?;
    row.ok_or_else(|| format!("selected plugin submission {submission_id} does not exist"))
}

async fn count_target_captures(target: &AppDb) -> Result<i64, String> {
    let sql = target.sql("SELECT COUNT(*) FROM plugin_submissions");
    match target.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(&sql).fetch_one(pool).await,
        DatabaseBackend::Postgres(pool) => sqlx::query_scalar(&sql).fetch_one(pool).await,
    }
    .map_err(database_error)
}

fn database_error(error: sqlx::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

    use super::*;
    use crate::plugin::signature_message;

    #[test]
    fn manifest_requires_explicit_unique_sorted_ids() {
        assert!(validated_selection(&[]).is_err());
        assert!(validated_selection(&[1, 1]).is_err());
        assert_eq!(validated_selection(&[3, 1, 2]).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn manifest_fingerprint_detects_metadata_changes() {
        let mut entry = TrustedCaptureEntry {
            submission_id: 1,
            user_id: 1,
            user_email: "owner@example.test".to_string(),
            user_display_name: "Owner".to_string(),
            user_auth_provider: "local".to_string(),
            user_auth_subject: "owner".to_string(),
            plugin_install_id: 7,
            plugin_public_key_base64: "key".to_string(),
            plugin_install_created_at: "2026-01-01".to_string(),
            plugin_install_revoked_at: None,
            source_url: "https://example.test/listing".to_string(),
            submitted_at: "2026-01-02".to_string(),
            rendered_html_sha256: "a".repeat(64),
            signature_base64: "signature".to_string(),
        };
        let before = manifest_fingerprint(&[entry.clone()]).unwrap();
        entry.submitted_at = "2026-01-03".to_string();
        assert_ne!(before, manifest_fingerprint(&[entry]).unwrap());
    }

    #[tokio::test]
    async fn import_copies_only_selected_signed_capture_and_resets_derived_fields() {
        let source = AppDb::connect("sqlite::memory:").await.unwrap();
        let target = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = source.current_user(None).await.unwrap();
        let DatabaseBackend::Sqlite(source_pool) = source.backend() else {
            unreachable!()
        };
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let keys = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let public_key = BASE64_STANDARD.encode(keys.public_key().as_ref());
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, ?) RETURNING id",
        )
        .bind(user.id)
        .bind(public_key)
        .fetch_one(source_pool)
        .await
        .unwrap();
        let mut ids = Vec::new();
        for ordinal in 1..=2 {
            let url = format!("https://example.test/capture-{ordinal}");
            let html = format!("<html>capture {ordinal}</html>");
            let hash = sha256_hex(html.as_bytes());
            let signature = BASE64_STANDARD.encode(
                keys.sign(&rng, signature_message(install_id, &url, &hash).as_bytes())
                    .unwrap()
                    .as_ref(),
            );
            let id: i64 = sqlx::query_scalar(
                r#"INSERT INTO plugin_submissions
                   (user_id, plugin_install_id, source_url, submitted_at, rendered_html,
                    rendered_html_sha256, signature_base64, extracted_listing_json,
                    extraction_error)
                   VALUES (?, ?, ?, ?, ?, ?, ?, '{"derived":true}', 'old error') RETURNING id"#,
            )
            .bind(user.id)
            .bind(install_id)
            .bind(&url)
            .bind(format!("2026-07-2{ordinal} 10:00:00"))
            .bind(&html)
            .bind(&hash)
            .bind(&signature)
            .fetch_one(source_pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let manifest = build_trusted_capture_manifest(&source, &[ids[1]])
            .await
            .unwrap();
        let report = import_trusted_capture_manifest(&source, &target, &manifest, true)
            .await
            .unwrap();
        assert_eq!(report.imported_capture_count, 1);
        let DatabaseBackend::Sqlite(target_pool) = target.backend() else {
            unreachable!()
        };
        let row: (i64, i64, String, Option<String>, Option<String>, Option<i64>) =
            sqlx::query_as(
                "SELECT COUNT(*), id, submitted_at, extracted_listing_json, extraction_error, canonical_listing_id FROM plugin_submissions",
            )
            .fetch_one(target_pool)
            .await
            .unwrap();
        assert_eq!(row.0, 1);
        assert_eq!(row.1, ids[1]);
        assert_eq!(row.2, "2026-07-22 10:00:00");
        assert_eq!((row.3, row.4, row.5), (None, None, None));
    }
}
