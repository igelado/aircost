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

pub(crate) mod admission;
pub mod catalog;
pub mod export;
pub mod run;

pub use export::{
    export_replay_manifest, CaptureReadiness, DatabaseReadiness, ReadinessCheckStatus,
    ReadinessDatabaseBackend, ReadinessIssue, ReadinessIssueCode, ReadinessSeverity,
    ReplayCaptureSelection, ReplayManifestExport, ReplayManifestExportRequest,
    ReplaySourceInventory, ReplaySourceReadinessReport,
};

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

const MANIFEST_HASH_DOMAIN: &[u8] = b"aircost:trusted-capture-manifest\0";
const MAX_CAPTURE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedCaptureManifest {
    pub captures: Vec<TrustedCaptureEntry>,
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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

pub(crate) struct RetainedCaptureAuthentication<'capture> {
    pub submission_id: i64,
    pub submission_user_id: i64,
    pub plugin_install_id: i64,
    pub plugin_install_user_id: i64,
    pub plugin_public_key_base64: &'capture str,
    pub source_url: &'capture str,
    pub rendered_html: &'capture str,
    pub rendered_html_sha256: &'capture str,
    pub signature_base64: &'capture str,
    pub timestamp_chronology_valid: bool,
}

pub(crate) fn authenticate_retained_capture(
    capture: RetainedCaptureAuthentication<'_>,
) -> Result<(), String> {
    if capture.submission_user_id != capture.plugin_install_user_id {
        return Err(format!(
            "capture {} owner differs from plugin install {} owner",
            capture.submission_id, capture.plugin_install_id
        ));
    }
    if !capture.timestamp_chronology_valid {
        return Err(format!(
            "capture {} has invalid install/submission/revocation timestamp chronology",
            capture.submission_id
        ));
    }
    validate_capture_authenticity(
        capture.submission_id,
        capture.submission_user_id,
        capture.plugin_install_id,
        capture.plugin_public_key_base64,
        capture.source_url,
        capture.rendered_html,
        capture.rendered_html_sha256,
        capture.signature_base64,
    )
}

pub(crate) fn retained_capture_timestamp_chronology_valid(
    install_created_at: &str,
    submitted_at: &str,
    revoked_at: Option<&str>,
) -> bool {
    let Some(install_created_at) = parse_replay_timestamp(install_created_at) else {
        return false;
    };
    let Some(submitted_at) = parse_replay_timestamp(submitted_at) else {
        return false;
    };
    let revoked_at = match revoked_at {
        Some(value) => match parse_replay_timestamp(value) {
            Some(parsed) => Some(parsed),
            None => return false,
        },
        None => None,
    };
    submitted_at >= install_created_at
        && revoked_at
            .map(|revoked| revoked >= install_created_at && submitted_at <= revoked)
            .unwrap_or(true)
}

/// Parses the timestamp shapes emitted by SQLite, PostgreSQL, and the plugin
/// API without allowing malformed text to abort a PostgreSQL query. Naive
/// timestamps are interpreted as UTC, matching application storage.
pub(crate) fn parse_replay_timestamp(value: &str) -> Option<i128> {
    let value = value.trim();
    if value.len() < 19 {
        return None;
    }
    let bytes = value.as_bytes();
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b' ' | b'T'))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let year = parse_timestamp_digits(bytes.get(0..4)?)? as i64;
    let month = parse_timestamp_digits(bytes.get(5..7)?)? as u32;
    let day = parse_timestamp_digits(bytes.get(8..10)?)? as u32;
    let hour = parse_timestamp_digits(bytes.get(11..13)?)? as u32;
    let minute = parse_timestamp_digits(bytes.get(14..16)?)? as u32;
    let second = parse_timestamp_digits(bytes.get(17..19)?)? as u32;
    if year == 0
        || !(1..=12).contains(&month)
        || day == 0
        || day > timestamp_days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let mut cursor = 19;
    let mut nanos = 0_i128;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let fraction_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        let fraction = bytes.get(fraction_start..cursor)?;
        if fraction.is_empty() || fraction.len() > 9 {
            return None;
        }
        nanos =
            parse_timestamp_digits(fraction)? as i128 * 10_i128.pow((9 - fraction.len()) as u32);
    }

    let offset_seconds = match bytes.get(cursor..) {
        Some([]) | Some([b'Z']) | Some([b'z']) => 0_i64,
        Some(zone) if matches!(zone.first(), Some(b'+' | b'-')) => {
            let sign = if zone[0] == b'+' { 1_i64 } else { -1_i64 };
            let (hours, minutes) = match zone {
                [_, h1, h2] => (parse_timestamp_digits(&[*h1, *h2])?, 0),
                [_, h1, h2, m1, m2] => (
                    parse_timestamp_digits(&[*h1, *h2])?,
                    parse_timestamp_digits(&[*m1, *m2])?,
                ),
                [_, h1, h2, b':', m1, m2] => (
                    parse_timestamp_digits(&[*h1, *h2])?,
                    parse_timestamp_digits(&[*m1, *m2])?,
                ),
                _ => return None,
            };
            if hours > 23 || minutes > 59 {
                return None;
            }
            sign * i64::from(hours * 3_600 + minutes * 60)
        }
        _ => return None,
    };

    let days = timestamp_days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour * 3_600 + minute * 60 + second))?
        .checked_sub(offset_seconds)?;
    Some(i128::from(seconds) * 1_000_000_000 + nanos)
}

fn parse_timestamp_digits(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    bytes.iter().try_fold(0_u32, |value, digit| {
        value.checked_mul(10)?.checked_add(u32::from(*digit - b'0'))
    })
}

fn timestamp_days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn timestamp_days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub async fn build_trusted_capture_manifest(
    source: &AppDb,
    submission_ids: &[i64],
) -> Result<TrustedCaptureManifest, String> {
    let export = export_replay_manifest(
        source,
        ReplayManifestExportRequest {
            selection: ReplayCaptureSelection::SubmissionIds(submission_ids.to_vec()),
        },
    )
    .await?;
    export.manifest.ok_or_else(|| {
        format!(
            "selected captures are not ready for replay manifest export (blocking_issues={}, omitted_issues={})",
            export.readiness.blocking_issue_count, export.readiness.omitted_issue_count
        )
    })
}

pub fn validate_trusted_capture_manifest(manifest: &TrustedCaptureManifest) -> Result<(), String> {
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
    validate_capture_authenticity(
        row.submission_id,
        row.user_id,
        row.plugin_install_id,
        &row.plugin_public_key_base64,
        &row.source_url,
        &row.rendered_html,
        &row.rendered_html_sha256,
        &row.signature_base64,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_capture_authenticity(
    submission_id: i64,
    user_id: i64,
    plugin_install_id: i64,
    plugin_public_key_base64: &str,
    source_url: &str,
    rendered_html: &str,
    rendered_html_sha256: &str,
    signature_base64: &str,
) -> Result<(), String> {
    if submission_id <= 0 || user_id <= 0 || plugin_install_id <= 0 {
        return Err("capture, owner, and plugin install IDs must be positive".to_string());
    }
    validate_source_url(source_url).map_err(|error| error.to_string())?;
    if rendered_html.trim().is_empty() || rendered_html.len() > MAX_CAPTURE_BYTES {
        return Err(format!(
            "capture {} HTML is empty or exceeds the admission limit",
            submission_id
        ));
    }
    let recomputed = sha256_hex(rendered_html.as_bytes());
    if recomputed != rendered_html_sha256 {
        return Err(format!(
            "capture {} rendered HTML hash is corrupt",
            submission_id
        ));
    }
    verify_submission_signature(
        plugin_public_key_base64,
        plugin_install_id,
        source_url,
        &recomputed,
        signature_base64,
    )
    .map_err(|error| format!("capture {submission_id}: {error}"))
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

    fn fixture_manifest_entry() -> TrustedCaptureEntry {
        TrustedCaptureEntry {
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
        }
    }

    #[test]
    fn manifest_requires_explicit_unique_sorted_ids() {
        assert!(validated_selection(&[]).is_err());
        assert!(validated_selection(&[1, 1]).is_err());
        assert_eq!(validated_selection(&[3, 1, 2]).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn manifest_fingerprint_detects_metadata_changes() {
        let mut entry = fixture_manifest_entry();
        let before = manifest_fingerprint(&[entry.clone()]).unwrap();
        entry.submitted_at = "2026-01-03".to_string();
        assert_ne!(before, manifest_fingerprint(&[entry]).unwrap());
    }

    #[test]
    fn manifest_fingerprint_uses_the_unversioned_domain() {
        let entries = vec![fixture_manifest_entry()];
        let encoded = serde_json::to_vec(&entries).unwrap();
        let mut expected = Sha256::new();
        expected.update(b"aircost:trusted-capture-manifest\0");
        expected.update(encoded);
        assert_eq!(
            manifest_fingerprint(&entries).unwrap(),
            format!("{:x}", expected.finalize())
        );
    }

    #[test]
    fn manifest_json_is_unversioned_and_rejects_unknown_fields() {
        let entries = vec![fixture_manifest_entry()];
        let manifest = TrustedCaptureManifest {
            manifest_sha256: manifest_fingerprint(&entries).unwrap(),
            captures: entries,
        };
        let value = serde_json::to_value(&manifest).unwrap();
        assert!(value.get("version").is_none());

        let mut unknown_manifest = value.clone();
        unknown_manifest
            .as_object_mut()
            .unwrap()
            .insert("version".to_string(), serde_json::json!(1));
        assert!(serde_json::from_value::<TrustedCaptureManifest>(unknown_manifest).is_err());

        let mut unknown_entry = value;
        unknown_entry["captures"][0]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<TrustedCaptureManifest>(unknown_entry).is_err());
    }

    #[tokio::test]
    async fn import_copies_only_selected_signed_capture_and_resets_derived_fields() {
        let source = AppDb::connect("sqlite::memory:").await.unwrap();
        let target = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = source.current_user(None).await.unwrap();
        let DatabaseBackend::Sqlite(source_pool) = source.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE users SET created_at = '2026-07-01 00:00:00', updated_at = '2026-07-02 00:00:00' WHERE id = ?",
        )
        .bind(user.id)
        .execute(source_pool)
        .await
        .unwrap();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let keys = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let public_key = BASE64_STANDARD.encode(keys.public_key().as_ref());
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64, created_at) VALUES (?, ?, '2026-07-03 00:00:00') RETURNING id",
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
