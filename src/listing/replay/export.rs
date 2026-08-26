//! Snapshot-consistent, provider-free replay manifest export.
//!
//! Selection, inventory, and capture bytes are read through one connection and
//! one read transaction. Validation happens only after that transaction has
//! released its snapshot, and the report never serializes retained source
//! bytes or capture credentials.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use serde::Serialize;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, Postgres, QueryBuilder, Row, Sqlite};
use url::Url;

use super::{
    database_error, entry_from_row, manifest_fingerprint, parse_replay_timestamp,
    retained_capture_timestamp_chronology_valid, validated_selection, SourceCaptureRow,
    TrustedCaptureManifest, MAX_CAPTURE_BYTES,
};
use crate::aircraft::faa::normalize_n_number;
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::validate_source_url;
use crate::plugin::{sha256_hex, verify_submission_signature};

const MAX_EXPORT_CAPTURES: usize = 1_000;
const MAX_RETAINED_ISSUES: usize = 256;
const MAX_EXCLUDED_UNBOUND_IDS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayCaptureSelection {
    AllBound {
        expected_capture_count: Option<usize>,
    },
    SubmissionIds(Vec<i64>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayManifestExportRequest {
    pub selection: ReplayCaptureSelection,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReplayManifestExport {
    pub manifest: Option<TrustedCaptureManifest>,
    pub readiness: ReplaySourceReadinessReport,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReplaySourceReadinessReport {
    pub ready: bool,
    pub provider_calls: u64,
    pub database: DatabaseReadiness,
    pub inventory: ReplaySourceInventory,
    pub captures: Vec<CaptureReadiness>,
    pub excluded_unbound_submission_ids: Vec<i64>,
    pub omitted_excluded_unbound_submission_count: usize,
    pub issues: Vec<ReadinessIssue>,
    pub blocking_issue_count: usize,
    pub warning_issue_count: usize,
    pub omitted_issue_count: usize,
    pub manifest_sha256: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DatabaseReadiness {
    pub backend: ReadinessDatabaseBackend,
    pub schema_contract_attested: bool,
    pub physical_integrity_check: ReadinessCheckStatus,
    pub foreign_key_check: ReadinessCheckStatus,
    pub foreign_key_violation_count: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessDatabaseBackend {
    Sqlite,
    Postgres,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessCheckStatus {
    Passed,
    Failed,
    NotApplicable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReplaySourceInventory {
    pub listing_count: usize,
    pub submission_count: usize,
    pub bound_submission_count: usize,
    pub unbound_submission_count: usize,
    pub selected_capture_count: usize,
    pub ready_capture_count: usize,
    pub blocked_capture_count: usize,
    pub ambiguous_listing_count: usize,
    pub distinct_n_number_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CaptureReadiness {
    pub listing_id: Option<i64>,
    pub submission_id: i64,
    pub normalized_n_number: Option<String>,
    pub rendered_html_bytes: usize,
    pub rendered_html_sha256: String,
    pub ready: bool,
    pub issue_codes: Vec<ReadinessIssueCode>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ReadinessIssue {
    pub severity: ReadinessSeverity,
    pub code: ReadinessIssueCode,
    pub listing_id: Option<i64>,
    pub submission_id: Option<i64>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessSeverity {
    Blocking,
    Warning,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessIssueCode {
    NoCapturesSelected,
    ExpectedCaptureCountMismatch,
    ListingCaptureMissing,
    ListingCaptureAmbiguous,
    SelectedCaptureMissing,
    CaptureIdentityInvalid,
    CaptureOwnerMissing,
    CaptureInstallMissing,
    CaptureInstallOwnerMismatch,
    SourceUrlInvalid,
    RenderedHtmlEmpty,
    RenderedHtmlTooLarge,
    RenderedHtmlHashMismatch,
    CaptureSignatureInvalid,
    ListingRegistrationMissing,
    ListingRegistrationInvalid,
    ListingOwnerMismatch,
    ListingSourceUrlMismatch,
    DuplicateRegistration,
    DuplicateSourceUrl,
    ExcludedUnboundSubmission,
    DatabaseIntegrityCheckFailed,
    ForeignKeyCheckFailed,
    CaptureTimestampChronologyInvalid,
}

#[derive(Clone, Debug)]
enum ValidatedSelection {
    AllBound {
        expected_capture_count: Option<usize>,
    },
    SubmissionIds(Vec<i64>),
}

#[derive(Clone, Debug, FromRow)]
struct ListingBindingRow {
    listing_id: i64,
    submission_id: Option<i64>,
}

#[derive(Clone, Debug)]
struct RawCaptureRow {
    submission_id: i64,
    submission_user_id: i64,
    submission_plugin_install_id: i64,
    source_url: String,
    submitted_at: String,
    rendered_html: String,
    rendered_html_sha256: String,
    signature_base64: String,
    canonical_listing_id: Option<i64>,
    owner_id: Option<i64>,
    owner_email: Option<String>,
    owner_display_name: Option<String>,
    owner_auth_provider: Option<String>,
    owner_auth_subject: Option<String>,
    owner_created_at: Option<String>,
    owner_updated_at: Option<String>,
    install_id: Option<i64>,
    install_user_id: Option<i64>,
    install_public_key_base64: Option<String>,
    install_created_at: Option<String>,
    install_revoked_at: Option<String>,
    listing_created_by_user_id: Option<i64>,
    listing_source_url: Option<String>,
    registration_number: Option<String>,
}

impl<'row> FromRow<'row, SqliteRow> for RawCaptureRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        Self::from_sqlite_row(row)
    }
}

impl<'row> FromRow<'row, PgRow> for RawCaptureRow {
    fn from_row(row: &'row PgRow) -> Result<Self, sqlx::Error> {
        Self::from_postgres_row(row)
    }
}

impl RawCaptureRow {
    fn from_sqlite_row(row: &SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            submission_id: row.try_get("submission_id")?,
            submission_user_id: row.try_get("submission_user_id")?,
            submission_plugin_install_id: row.try_get("submission_plugin_install_id")?,
            source_url: row.try_get("source_url")?,
            submitted_at: row.try_get("submitted_at")?,
            rendered_html: row.try_get("rendered_html")?,
            rendered_html_sha256: row.try_get("rendered_html_sha256")?,
            signature_base64: row.try_get("signature_base64")?,
            canonical_listing_id: row.try_get("canonical_listing_id")?,
            owner_id: row.try_get("owner_id")?,
            owner_email: row.try_get("owner_email")?,
            owner_display_name: row.try_get("owner_display_name")?,
            owner_auth_provider: row.try_get("owner_auth_provider")?,
            owner_auth_subject: row.try_get("owner_auth_subject")?,
            owner_created_at: row.try_get("owner_created_at")?,
            owner_updated_at: row.try_get("owner_updated_at")?,
            install_id: row.try_get("install_id")?,
            install_user_id: row.try_get("install_user_id")?,
            install_public_key_base64: row.try_get("install_public_key_base64")?,
            install_created_at: row.try_get("install_created_at")?,
            install_revoked_at: row.try_get("install_revoked_at")?,
            listing_created_by_user_id: row.try_get("listing_created_by_user_id")?,
            listing_source_url: row.try_get("listing_source_url")?,
            registration_number: row.try_get("registration_number")?,
        })
    }

    fn from_postgres_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            submission_id: row.try_get("submission_id")?,
            submission_user_id: row.try_get("submission_user_id")?,
            submission_plugin_install_id: row.try_get("submission_plugin_install_id")?,
            source_url: row.try_get("source_url")?,
            submitted_at: row.try_get("submitted_at")?,
            rendered_html: row.try_get("rendered_html")?,
            rendered_html_sha256: row.try_get("rendered_html_sha256")?,
            signature_base64: row.try_get("signature_base64")?,
            canonical_listing_id: row.try_get("canonical_listing_id")?,
            owner_id: row.try_get("owner_id")?,
            owner_email: row.try_get("owner_email")?,
            owner_display_name: row.try_get("owner_display_name")?,
            owner_auth_provider: row.try_get("owner_auth_provider")?,
            owner_auth_subject: row.try_get("owner_auth_subject")?,
            owner_created_at: row.try_get("owner_created_at")?,
            owner_updated_at: row.try_get("owner_updated_at")?,
            install_id: row.try_get("install_id")?,
            install_user_id: row.try_get("install_user_id")?,
            install_public_key_base64: row.try_get("install_public_key_base64")?,
            install_created_at: row.try_get("install_created_at")?,
            install_revoked_at: row.try_get("install_revoked_at")?,
            listing_created_by_user_id: row.try_get("listing_created_by_user_id")?,
            listing_source_url: row.try_get("listing_source_url")?,
            registration_number: row.try_get("registration_number")?,
        })
    }
}

#[derive(Clone, Debug)]
struct SnapshotRows {
    database: DatabaseReadiness,
    bindings: Vec<ListingBindingRow>,
    submission_count: usize,
    unbound_submission_ids: Vec<i64>,
    selected_submission_ids: Vec<i64>,
    captures: Vec<RawCaptureRow>,
    selection_issues: Vec<ReadinessIssue>,
    database_issues: Vec<ReadinessIssue>,
}

struct IssueCollector {
    retained: Vec<ReadinessIssue>,
    blocking: usize,
    warning: usize,
    omitted: usize,
}

impl IssueCollector {
    fn new() -> Self {
        Self {
            retained: Vec::new(),
            blocking: 0,
            warning: 0,
            omitted: 0,
        }
    }

    fn push(&mut self, issue: ReadinessIssue) {
        match issue.severity {
            ReadinessSeverity::Blocking => self.blocking += 1,
            ReadinessSeverity::Warning => self.warning += 1,
        }
        if self.retained.len() < MAX_RETAINED_ISSUES {
            self.retained.push(issue);
        } else {
            self.omitted += 1;
        }
    }

    fn finish(mut self) -> (Vec<ReadinessIssue>, usize, usize, usize) {
        self.retained.sort();
        (self.retained, self.blocking, self.warning, self.omitted)
    }
}

pub async fn export_replay_manifest(
    source: &AppDb,
    request: ReplayManifestExportRequest,
) -> Result<ReplayManifestExport, String> {
    let selection = validate_request(request.selection)?;
    let snapshot = match source.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut connection = pool.acquire().await.map_err(database_error)?;
            load_read_only_sqlite_snapshot(&mut connection, &selection, || async {}).await?
        }
        DatabaseBackend::Postgres(pool) => {
            let mut connection = pool.acquire().await.map_err(database_error)?;
            load_postgres_snapshot(&mut connection, &selection).await?
        }
    };
    build_export(selection, snapshot)
}

async fn load_read_only_sqlite_snapshot<F, Fut>(
    connection: &mut sqlx::SqliteConnection,
    selection: &ValidatedSelection,
    after_inventory: F,
) -> Result<SnapshotRows, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let prior_query_only: i64 = sqlx::query_scalar("PRAGMA query_only")
        .fetch_one(&mut *connection)
        .await
        .map_err(database_error)?;
    sqlx::query("PRAGMA query_only = ON")
        .execute(&mut *connection)
        .await
        .map_err(database_error)?;
    let result = load_sqlite_snapshot(connection, selection, after_inventory).await;
    let restore_statement = if prior_query_only == 0 {
        "PRAGMA query_only = OFF"
    } else {
        "PRAGMA query_only = ON"
    };
    let restore = sqlx::query(restore_statement)
        .execute(&mut *connection)
        .await
        .map_err(database_error);
    match (result, restore) {
        (Ok(snapshot), Ok(_)) => Ok(snapshot),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn validate_request(selection: ReplayCaptureSelection) -> Result<ValidatedSelection, String> {
    match selection {
        ReplayCaptureSelection::AllBound {
            expected_capture_count,
        } => {
            if expected_capture_count == Some(0) {
                return Err("expected capture count must be positive".to_string());
            }
            Ok(ValidatedSelection::AllBound {
                expected_capture_count,
            })
        }
        ReplayCaptureSelection::SubmissionIds(ids) => {
            let ids = validated_selection(&ids)?;
            if ids.len() > MAX_EXPORT_CAPTURES {
                return Err(format!(
                    "capture selection exceeds the export limit of {MAX_EXPORT_CAPTURES}"
                ));
            }
            Ok(ValidatedSelection::SubmissionIds(ids))
        }
    }
}

async fn load_sqlite_snapshot<F, Fut>(
    connection: &mut sqlx::SqliteConnection,
    selection: &ValidatedSelection,
    after_inventory: F,
) -> Result<SnapshotRows, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut transaction = sqlx::Connection::begin(connection)
        .await
        .map_err(database_error)?;
    let result = async {
        let integrity_check = sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        let foreign_key_violations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
        let foreign_key_violation_count =
            count_to_usize(foreign_key_violations, "foreign-key violation")?;
        let integrity_ok = integrity_check.as_slice() == ["ok"];
        let mut database_issues = Vec::new();
        if !integrity_ok {
            database_issues.push(blocking_issue(
                ReadinessIssueCode::DatabaseIntegrityCheckFailed,
                None,
                None,
                format!(
                    "SQLite integrity_check returned {} non-success result rows",
                    integrity_check.len()
                ),
            ));
        }
        if foreign_key_violation_count != 0 {
            database_issues.push(blocking_issue(
                ReadinessIssueCode::ForeignKeyCheckFailed,
                None,
                None,
                format!("SQLite foreign_key_check found {foreign_key_violation_count} violations"),
            ));
        }
        let bindings = sqlx::query_as::<_, ListingBindingRow>(LISTING_BINDINGS_SQL)
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        after_inventory().await;
        let submission_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plugin_submissions")
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
        let unbound_submission_ids = sqlx::query_scalar::<_, i64>(UNBOUND_SUBMISSIONS_SQL)
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        let (selected_submission_ids, selection_issues) = resolve_selection(selection, &bindings)?;
        let captures = load_sqlite_captures(&mut transaction, &selected_submission_ids).await?;
        Ok(SnapshotRows {
            database: DatabaseReadiness {
                backend: ReadinessDatabaseBackend::Sqlite,
                schema_contract_attested: true,
                physical_integrity_check: if integrity_ok {
                    ReadinessCheckStatus::Passed
                } else {
                    ReadinessCheckStatus::Failed
                },
                foreign_key_check: if foreign_key_violation_count == 0 {
                    ReadinessCheckStatus::Passed
                } else {
                    ReadinessCheckStatus::Failed
                },
                foreign_key_violation_count: Some(foreign_key_violation_count),
            },
            bindings,
            submission_count: count_to_usize(submission_count, "submission")?,
            unbound_submission_ids,
            selected_submission_ids,
            captures,
            selection_issues,
            database_issues,
        })
    }
    .await;
    let rollback = transaction.rollback().await.map_err(database_error);
    match (result, rollback) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

async fn load_postgres_snapshot(
    connection: &mut sqlx::PgConnection,
    selection: &ValidatedSelection,
) -> Result<SnapshotRows, String> {
    let mut transaction = connection
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await
        .map_err(database_error)?;
    let result = async {
        let unvalidated_foreign_keys: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)
               FROM pg_catalog.pg_constraint constraint_row
               JOIN pg_catalog.pg_namespace namespace
                 ON namespace.oid = constraint_row.connamespace
               WHERE namespace.nspname = 'public'
                 AND constraint_row.contype = 'f'
                 AND NOT constraint_row.convalidated"#,
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let foreign_key_violation_count =
            count_to_usize(unvalidated_foreign_keys, "unvalidated foreign-key")?;
        let mut database_issues = Vec::new();
        if foreign_key_violation_count != 0 {
            database_issues.push(blocking_issue(
                ReadinessIssueCode::ForeignKeyCheckFailed,
                None,
                None,
                format!(
                    "PostgreSQL has {foreign_key_violation_count} unvalidated public foreign keys"
                ),
            ));
        }
        let bindings = sqlx::query_as::<_, ListingBindingRow>(LISTING_BINDINGS_SQL)
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        let submission_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plugin_submissions")
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
        let unbound_submission_ids = sqlx::query_scalar::<_, i64>(UNBOUND_SUBMISSIONS_SQL)
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        let (selected_submission_ids, selection_issues) = resolve_selection(selection, &bindings)?;
        let captures = load_postgres_captures(&mut transaction, &selected_submission_ids).await?;
        Ok(SnapshotRows {
            database: DatabaseReadiness {
                backend: ReadinessDatabaseBackend::Postgres,
                schema_contract_attested: true,
                physical_integrity_check: ReadinessCheckStatus::NotApplicable,
                foreign_key_check: if foreign_key_violation_count == 0 {
                    ReadinessCheckStatus::Passed
                } else {
                    ReadinessCheckStatus::Failed
                },
                foreign_key_violation_count: Some(foreign_key_violation_count),
            },
            bindings,
            submission_count: count_to_usize(submission_count, "submission")?,
            unbound_submission_ids,
            selected_submission_ids,
            captures,
            selection_issues,
            database_issues,
        })
    }
    .await;
    let rollback = transaction.rollback().await.map_err(database_error);
    match (result, rollback) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn resolve_selection(
    selection: &ValidatedSelection,
    bindings: &[ListingBindingRow],
) -> Result<(Vec<i64>, Vec<ReadinessIssue>), String> {
    let ValidatedSelection::AllBound {
        expected_capture_count,
    } = selection
    else {
        let ValidatedSelection::SubmissionIds(ids) = selection else {
            unreachable!()
        };
        return Ok((ids.clone(), Vec::new()));
    };

    let mut by_listing: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for binding in bindings {
        let submissions = by_listing.entry(binding.listing_id).or_default();
        if let Some(submission_id) = binding.submission_id {
            submissions.push(submission_id);
        }
    }
    let mut selected = Vec::new();
    let mut issues = Vec::new();
    for (listing_id, mut submissions) in by_listing {
        submissions.sort_unstable();
        match submissions.as_slice() {
            [] => issues.push(blocking_issue(
                ReadinessIssueCode::ListingCaptureMissing,
                Some(listing_id),
                None,
                "listing has no bound retained capture",
            )),
            [submission_id] => selected.push(*submission_id),
            _ => issues.push(blocking_issue(
                ReadinessIssueCode::ListingCaptureAmbiguous,
                Some(listing_id),
                None,
                format!(
                    "listing has {} bound retained captures instead of exactly one",
                    submissions.len()
                ),
            )),
        }
    }
    selected.sort_unstable();
    if let Some(expected) = expected_capture_count {
        if selected.len() != *expected {
            issues.push(blocking_issue(
                ReadinessIssueCode::ExpectedCaptureCountMismatch,
                None,
                None,
                format!(
                    "selected capture count {} does not match required count {expected}",
                    selected.len()
                ),
            ));
        }
    }
    if selected.is_empty() {
        issues.push(blocking_issue(
            ReadinessIssueCode::NoCapturesSelected,
            None,
            None,
            "no captures are eligible for manifest export",
        ));
    }
    if selected.len() > MAX_EXPORT_CAPTURES {
        return Err(format!(
            "capture selection exceeds the export limit of {MAX_EXPORT_CAPTURES}"
        ));
    }
    Ok((selected, issues))
}

async fn load_sqlite_captures(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    submission_ids: &[i64],
) -> Result<Vec<RawCaptureRow>, String> {
    if submission_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new(CAPTURE_ROWS_SQL_PREFIX);
    push_capture_ids(&mut query, submission_ids);
    query
        .build_query_as::<RawCaptureRow>()
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)
}

async fn load_postgres_captures(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    submission_ids: &[i64],
) -> Result<Vec<RawCaptureRow>, String> {
    if submission_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Postgres>::new(CAPTURE_ROWS_SQL_PREFIX);
    push_capture_ids(&mut query, submission_ids);
    query
        .build_query_as::<RawCaptureRow>()
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)
}

fn push_capture_ids<'args, DB>(query: &mut QueryBuilder<'args, DB>, submission_ids: &'args [i64])
where
    DB: sqlx::Database,
    i64: sqlx::Encode<'args, DB> + sqlx::Type<DB>,
{
    query.push(" WHERE submission.id IN (");
    let mut separated = query.separated(", ");
    for id in submission_ids {
        separated.push_bind(*id);
    }
    separated.push_unseparated(") ORDER BY submission.id");
}

fn build_export(
    selection: ValidatedSelection,
    snapshot: SnapshotRows,
) -> Result<ReplayManifestExport, String> {
    let all_bound = matches!(selection, ValidatedSelection::AllBound { .. });
    let mut collector = IssueCollector::new();
    for issue in snapshot.database_issues {
        collector.push(issue);
    }
    for issue in snapshot.selection_issues {
        collector.push(issue);
    }

    let raw_by_id = snapshot
        .captures
        .into_iter()
        .map(|row| (row.submission_id, row))
        .collect::<BTreeMap<_, _>>();
    let mut manifest_rows = Vec::new();
    let mut capture_reports = Vec::new();
    let mut registrations: BTreeMap<String, Vec<(usize, Option<i64>, i64)>> = BTreeMap::new();
    let mut source_urls: BTreeMap<String, Vec<(usize, Option<i64>, i64)>> = BTreeMap::new();

    for submission_id in &snapshot.selected_submission_ids {
        let Some(raw) = raw_by_id.get(submission_id) else {
            collector.push(blocking_issue(
                ReadinessIssueCode::SelectedCaptureMissing,
                None,
                Some(*submission_id),
                "selected capture is missing from the snapshot",
            ));
            continue;
        };
        let (mut report, manifest_row, row_issues) = inspect_capture(raw, all_bound);
        for issue in row_issues {
            collector.push(issue);
        }
        if let Some(registration) = &report.normalized_n_number {
            registrations
                .entry(registration.clone())
                .or_default()
                .push((
                    capture_reports.len(),
                    report.listing_id,
                    report.submission_id,
                ));
        }
        if let Some(source_url) = canonical_source_url(&raw.source_url) {
            source_urls.entry(source_url).or_default().push((
                capture_reports.len(),
                report.listing_id,
                report.submission_id,
            ));
        }
        if let Some(row) = manifest_row {
            manifest_rows.push(row);
        }
        report.issue_codes.sort_unstable();
        report.issue_codes.dedup();
        report.ready = report.issue_codes.is_empty();
        capture_reports.push(report);
    }

    if all_bound {
        for (registration, members) in registrations {
            if members.len() <= 1 {
                continue;
            }
            for (report_index, listing_id, submission_id) in members {
                let report = &mut capture_reports[report_index];
                report
                    .issue_codes
                    .push(ReadinessIssueCode::DuplicateRegistration);
                report.issue_codes.sort_unstable();
                report.issue_codes.dedup();
                report.ready = false;
                collector.push(blocking_issue(
                    ReadinessIssueCode::DuplicateRegistration,
                    listing_id,
                    Some(submission_id),
                    format!("normalized registration {registration} is not unique"),
                ));
            }
        }
        for (_source_url, members) in source_urls {
            if members.len() <= 1 {
                continue;
            }
            for (report_index, listing_id, submission_id) in members {
                let report = &mut capture_reports[report_index];
                report
                    .issue_codes
                    .push(ReadinessIssueCode::DuplicateSourceUrl);
                report.issue_codes.sort_unstable();
                report.issue_codes.dedup();
                report.ready = false;
                collector.push(blocking_issue(
                    ReadinessIssueCode::DuplicateSourceUrl,
                    listing_id,
                    Some(submission_id),
                    "canonical source URL is not unique in the all-bound selection",
                ));
            }
        }
    }

    let unbound_submission_count = snapshot.unbound_submission_ids.len();
    let excluded_ids = if all_bound {
        snapshot.unbound_submission_ids
    } else {
        Vec::new()
    };
    let retained_excluded_ids = excluded_ids
        .iter()
        .take(MAX_EXCLUDED_UNBOUND_IDS)
        .copied()
        .collect::<Vec<_>>();
    for submission_id in &excluded_ids {
        collector.push(warning_issue(
            ReadinessIssueCode::ExcludedUnboundSubmission,
            None,
            Some(*submission_id),
            "unbound submission is excluded from the all-bound manifest",
        ));
    }
    let omitted_excluded = excluded_ids
        .len()
        .saturating_sub(retained_excluded_ids.len());

    let ready_capture_count = capture_reports.iter().filter(|row| row.ready).count();
    let blocked_capture_count = snapshot
        .selected_submission_ids
        .len()
        .saturating_sub(ready_capture_count);
    let listing_count = snapshot
        .bindings
        .iter()
        .map(|row| row.listing_id)
        .collect::<BTreeSet<_>>()
        .len();
    let ambiguous_listing_count = snapshot
        .bindings
        .iter()
        .fold(BTreeMap::<i64, usize>::new(), |mut counts, row| {
            *counts.entry(row.listing_id).or_default() += usize::from(row.submission_id.is_some());
            counts
        })
        .values()
        .filter(|count| **count != 1)
        .count();
    let distinct_n_number_count = capture_reports
        .iter()
        .filter_map(|row| row.normalized_n_number.as_ref())
        .collect::<BTreeSet<_>>()
        .len();

    let (issues, blocking_issue_count, warning_issue_count, omitted_issue_count) =
        collector.finish();
    let manifest = if blocking_issue_count == 0 {
        let mut entries = manifest_rows.iter().map(entry_from_row).collect::<Vec<_>>();
        entries.sort_by_key(|row| row.submission_id);
        let manifest_sha256 = manifest_fingerprint(&entries)?;
        Some(TrustedCaptureManifest {
            captures: entries,
            manifest_sha256,
        })
    } else {
        None
    };
    let manifest_sha256 = manifest
        .as_ref()
        .map(|manifest| manifest.manifest_sha256.clone());
    let bound_submission_count = snapshot
        .submission_count
        .checked_sub(unbound_submission_count)
        .ok_or_else(|| "unbound submission count exceeds total submission count".to_string())?;

    Ok(ReplayManifestExport {
        readiness: ReplaySourceReadinessReport {
            ready: manifest.is_some(),
            provider_calls: 0,
            database: snapshot.database,
            inventory: ReplaySourceInventory {
                listing_count,
                submission_count: snapshot.submission_count,
                bound_submission_count,
                unbound_submission_count,
                selected_capture_count: snapshot.selected_submission_ids.len(),
                ready_capture_count,
                blocked_capture_count,
                ambiguous_listing_count,
                distinct_n_number_count,
            },
            captures: capture_reports,
            excluded_unbound_submission_ids: retained_excluded_ids,
            omitted_excluded_unbound_submission_count: omitted_excluded,
            issues,
            blocking_issue_count,
            warning_issue_count,
            omitted_issue_count,
            manifest_sha256,
        },
        manifest,
    })
}

fn inspect_capture(
    raw: &RawCaptureRow,
    require_listing_registration: bool,
) -> (
    CaptureReadiness,
    Option<SourceCaptureRow>,
    Vec<ReadinessIssue>,
) {
    let mut codes = Vec::new();
    let mut issues = Vec::new();
    let listing_id = raw.canonical_listing_id;
    let submission_id = raw.submission_id;
    let mut reject = |code, message: String| {
        codes.push(code);
        issues.push(blocking_issue(
            code,
            listing_id,
            Some(submission_id),
            message,
        ));
    };

    if raw.submission_id <= 0
        || raw.submission_user_id <= 0
        || raw.submission_plugin_install_id <= 0
    {
        reject(
            ReadinessIssueCode::CaptureIdentityInvalid,
            "capture, owner, and plugin install IDs must be positive".to_string(),
        );
    }
    if raw.owner_id != Some(raw.submission_user_id) {
        reject(
            ReadinessIssueCode::CaptureOwnerMissing,
            "capture owner row is missing or does not match the submission".to_string(),
        );
    }
    if raw.install_id != Some(raw.submission_plugin_install_id) {
        reject(
            ReadinessIssueCode::CaptureInstallMissing,
            "plugin install row is missing or does not match the submission".to_string(),
        );
    }
    if raw.install_id.is_some() && raw.install_user_id != Some(raw.submission_user_id) {
        reject(
            ReadinessIssueCode::CaptureInstallOwnerMismatch,
            "plugin install owner does not match the capture owner".to_string(),
        );
    }
    if validate_source_url(&raw.source_url).is_err() {
        reject(
            ReadinessIssueCode::SourceUrlInvalid,
            "capture source URL is invalid".to_string(),
        );
    }
    if raw.rendered_html.trim().is_empty() {
        reject(
            ReadinessIssueCode::RenderedHtmlEmpty,
            "retained capture HTML is empty".to_string(),
        );
    }
    if raw.rendered_html.len() > MAX_CAPTURE_BYTES {
        reject(
            ReadinessIssueCode::RenderedHtmlTooLarge,
            format!("retained capture HTML exceeds {MAX_CAPTURE_BYTES} bytes"),
        );
    }
    let recomputed_hash = sha256_hex(raw.rendered_html.as_bytes());
    if recomputed_hash != raw.rendered_html_sha256 {
        reject(
            ReadinessIssueCode::RenderedHtmlHashMismatch,
            "retained capture HTML does not match its stored digest".to_string(),
        );
    }
    if let Some(public_key) = raw.install_public_key_base64.as_deref() {
        if verify_submission_signature(
            public_key,
            raw.submission_plugin_install_id,
            &raw.source_url,
            &recomputed_hash,
            &raw.signature_base64,
        )
        .is_err()
        {
            reject(
                ReadinessIssueCode::CaptureSignatureInvalid,
                "retained capture signature is invalid".to_string(),
            );
        }
    }
    if !capture_timestamp_chronology_valid(raw) {
        reject(
            ReadinessIssueCode::CaptureTimestampChronologyInvalid,
            "owner, install, submission, or revocation timestamps are invalid or out of order"
                .to_string(),
        );
    }

    if require_listing_registration {
        if raw.listing_created_by_user_id != Some(raw.submission_user_id) {
            reject(
                ReadinessIssueCode::ListingOwnerMismatch,
                "capture owner does not match the bound listing creator".to_string(),
            );
        }
        if raw.listing_source_url.as_deref() != Some(raw.source_url.as_str()) {
            reject(
                ReadinessIssueCode::ListingSourceUrlMismatch,
                "signed capture source URL does not exactly match the bound listing source URL"
                    .to_string(),
            );
        }
    }

    let normalized_n_number = if require_listing_registration {
        match raw.registration_number.as_deref().map(str::trim) {
            None | Some("") => {
                reject(
                    ReadinessIssueCode::ListingRegistrationMissing,
                    "bound listing has no registration number".to_string(),
                );
                None
            }
            Some(registration) => match normalize_n_number(registration) {
                Some(normalized) => Some(normalized),
                None => {
                    reject(
                        ReadinessIssueCode::ListingRegistrationInvalid,
                        "bound listing registration is not a valid N-number".to_string(),
                    );
                    None
                }
            },
        }
    } else {
        raw.registration_number
            .as_deref()
            .and_then(normalize_n_number)
    };

    codes.sort_unstable();
    codes.dedup();
    let row = if codes.is_empty() {
        Some(SourceCaptureRow {
            submission_id: raw.submission_id,
            user_id: raw.submission_user_id,
            user_email: raw.owner_email.clone().unwrap_or_default(),
            user_display_name: raw.owner_display_name.clone().unwrap_or_default(),
            user_auth_provider: raw.owner_auth_provider.clone().unwrap_or_default(),
            user_auth_subject: raw.owner_auth_subject.clone().unwrap_or_default(),
            user_created_at: raw.owner_created_at.clone().unwrap_or_default(),
            user_updated_at: raw.owner_updated_at.clone().unwrap_or_default(),
            plugin_install_id: raw.submission_plugin_install_id,
            plugin_public_key_base64: raw.install_public_key_base64.clone().unwrap_or_default(),
            plugin_install_created_at: raw.install_created_at.clone().unwrap_or_default(),
            plugin_install_revoked_at: raw.install_revoked_at.clone(),
            source_url: raw.source_url.clone(),
            submitted_at: raw.submitted_at.clone(),
            rendered_html: raw.rendered_html.clone(),
            rendered_html_sha256: raw.rendered_html_sha256.clone(),
            signature_base64: raw.signature_base64.clone(),
        })
    } else {
        None
    };
    (
        CaptureReadiness {
            listing_id,
            submission_id,
            normalized_n_number,
            rendered_html_bytes: raw.rendered_html.len(),
            rendered_html_sha256: raw.rendered_html_sha256.clone(),
            ready: codes.is_empty(),
            issue_codes: codes,
        },
        row,
        issues,
    )
}

fn canonical_source_url(value: &str) -> Option<String> {
    let mut parsed = Url::parse(value.trim()).ok()?;
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

fn capture_timestamp_chronology_valid(row: &RawCaptureRow) -> bool {
    let Some(user_created_at) = row
        .owner_created_at
        .as_deref()
        .and_then(parse_replay_timestamp)
    else {
        return false;
    };
    let Some(user_updated_at) = row
        .owner_updated_at
        .as_deref()
        .and_then(parse_replay_timestamp)
    else {
        return false;
    };
    let Some(install_created_at) = row
        .install_created_at
        .as_deref()
        .and_then(parse_replay_timestamp)
    else {
        return false;
    };
    user_updated_at >= user_created_at
        && install_created_at >= user_created_at
        && retained_capture_timestamp_chronology_valid(
            row.install_created_at.as_deref().unwrap_or_default(),
            &row.submitted_at,
            row.install_revoked_at.as_deref(),
        )
}

fn count_to_usize(count: i64, label: &str) -> Result<usize, String> {
    usize::try_from(count).map_err(|_| format!("{label} count is negative or too large"))
}

fn blocking_issue(
    code: ReadinessIssueCode,
    listing_id: Option<i64>,
    submission_id: Option<i64>,
    message: impl Into<String>,
) -> ReadinessIssue {
    ReadinessIssue {
        severity: ReadinessSeverity::Blocking,
        code,
        listing_id,
        submission_id,
        message: message.into(),
    }
}

fn warning_issue(
    code: ReadinessIssueCode,
    listing_id: Option<i64>,
    submission_id: Option<i64>,
    message: impl Into<String>,
) -> ReadinessIssue {
    ReadinessIssue {
        severity: ReadinessSeverity::Warning,
        code,
        listing_id,
        submission_id,
        message: message.into(),
    }
}

const LISTING_BINDINGS_SQL: &str = r#"
SELECT listing.id AS listing_id,
       submission.id AS submission_id
FROM aircraft_sale_listings listing
LEFT JOIN plugin_submissions submission
  ON submission.canonical_listing_id = listing.id
ORDER BY listing.id, submission.id
"#;

const UNBOUND_SUBMISSIONS_SQL: &str = r#"
SELECT id
FROM plugin_submissions
WHERE canonical_listing_id IS NULL
ORDER BY id
"#;

const CAPTURE_ROWS_SQL_PREFIX: &str = r#"
SELECT submission.id AS submission_id,
       submission.user_id AS submission_user_id,
       submission.plugin_install_id AS submission_plugin_install_id,
       submission.source_url,
       submission.submitted_at,
       submission.rendered_html,
       submission.rendered_html_sha256,
       submission.signature_base64,
       submission.canonical_listing_id,
       owner.id AS owner_id,
       owner.email AS owner_email,
       owner.display_name AS owner_display_name,
       owner.auth_provider AS owner_auth_provider,
       owner.auth_subject AS owner_auth_subject,
       owner.created_at AS owner_created_at,
       owner.updated_at AS owner_updated_at,
       install.id AS install_id,
       install.user_id AS install_user_id,
       install.public_key_base64 AS install_public_key_base64,
       install.created_at AS install_created_at,
       install.revoked_at AS install_revoked_at,
       listing.created_by_user_id AS listing_created_by_user_id,
       listing.source_url AS listing_source_url,
       listing.registration_number
FROM plugin_submissions submission
LEFT JOIN users owner ON owner.id = submission.user_id
LEFT JOIN plugin_installs install ON install.id = submission.plugin_install_id
LEFT JOIN aircraft_sale_listings listing ON listing.id = submission.canonical_listing_id
"#;

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

    use super::*;
    use crate::listing::replay::import_trusted_capture_manifest;
    use crate::listing::replay::run::{replay_captures, ReplayCapturesRequest, ReplayPhase};
    use crate::plugin::signature_message;

    struct Fixture {
        db: AppDb,
        keys: EcdsaKeyPair,
        user_id: i64,
        listing_ids: Vec<i64>,
        submission_ids: Vec<i64>,
    }

    async fn fixture(bound: usize, unbound: usize) -> Fixture {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE users SET created_at = '2025-01-01 00:00:00', updated_at = '2025-01-02 00:00:00' WHERE id = ?",
        )
        .bind(user.id)
        .execute(pool)
        .await
        .unwrap();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let keys = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let mut listing_ids = Vec::new();
        let mut submission_ids = Vec::new();
        for index in 0..bound {
            let source_url = format!("https://example.test/listing/{index}");
            let registration = format!("N{}AA", 100 + index);
            let listing_id = insert_listing(&db, user.id, &source_url, &registration).await;
            let submission_id = insert_capture(
                &db,
                &keys,
                user.id,
                Some(listing_id),
                &source_url,
                &format!("<html>bound capture {index}</html>"),
            )
            .await;
            listing_ids.push(listing_id);
            submission_ids.push(submission_id);
        }
        for index in 0..unbound {
            let submission_id = insert_capture(
                &db,
                &keys,
                user.id,
                None,
                &format!("https://example.test/unbound/{index}"),
                &format!("<html>unbound capture {index}</html>"),
            )
            .await;
            submission_ids.push(submission_id);
        }
        Fixture {
            db,
            keys,
            user_id: user.id,
            listing_ids,
            submission_ids,
        }
    }

    async fn insert_listing(db: &AppDb, user_id: i64, source_url: &str, registration: &str) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let variant_id: i64 =
            sqlx::query_scalar("SELECT id FROM aircraft_model_variants ORDER BY id LIMIT 1")
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query_scalar(
            r#"INSERT INTO aircraft_sale_listings (
                 aircraft_model_variant_id, created_by_user_id, source_url,
                 model_year, asking_price_usd, airframe_hours, registration_number
               ) VALUES (?, ?, ?, 2020, 200000, 500, ?) RETURNING id"#,
        )
        .bind(variant_id)
        .bind(user_id)
        .bind(source_url)
        .bind(registration)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn insert_user(db: &AppDb, ordinal: usize) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query_scalar(
            r#"INSERT INTO users (
                 email, display_name, auth_provider, auth_subject, created_at, updated_at
               ) VALUES (?, 'Hostile Owner', 'local', ?,
                         '2025-01-01 00:00:00', '2025-01-02 00:00:00')
               RETURNING id"#,
        )
        .bind(format!("hostile-{ordinal}@example.test"))
        .bind(format!("hostile-{ordinal}"))
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn insert_capture(
        db: &AppDb,
        keys: &EcdsaKeyPair,
        user_id: i64,
        listing_id: Option<i64>,
        source_url: &str,
        html: &str,
    ) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let install_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_installs (
                 user_id, public_key_base64, created_at
               ) VALUES (?, ?, '2026-01-01 00:00:00') RETURNING id"#,
        )
        .bind(user_id)
        .bind(BASE64_STANDARD.encode(keys.public_key().as_ref()))
        .fetch_one(pool)
        .await
        .unwrap();
        let hash = sha256_hex(html.as_bytes());
        let rng = SystemRandom::new();
        let signature = BASE64_STANDARD.encode(
            keys.sign(
                &rng,
                signature_message(install_id, source_url, &hash).as_bytes(),
            )
            .unwrap()
            .as_ref(),
        );
        sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, submitted_at,
                 rendered_html, rendered_html_sha256, signature_base64,
                 canonical_listing_id
               ) VALUES (?, ?, ?, '2026-01-02 00:00:00', ?, ?, ?, ?) RETURNING id"#,
        )
        .bind(user_id)
        .bind(install_id)
        .bind(source_url)
        .bind(html)
        .bind(hash)
        .bind(signature)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn all_bound(db: &AppDb, expected: usize) -> ReplayManifestExport {
        export_replay_manifest(
            db,
            ReplayManifestExportRequest {
                selection: ReplayCaptureSelection::AllBound {
                    expected_capture_count: Some(expected),
                },
            },
        )
        .await
        .unwrap()
    }

    fn issue_codes(report: &ReplaySourceReadinessReport) -> BTreeSet<ReadinessIssueCode> {
        report.issues.iter().map(|issue| issue.code).collect()
    }

    #[tokio::test]
    async fn all_bound_export_is_ready_and_reports_every_excluded_unbound_capture() {
        let fixture = fixture(3, 7).await;
        let export = all_bound(&fixture.db, 3).await;
        let manifest = export.manifest.as_ref().expect("ready manifest");
        assert!(export.readiness.ready);
        assert_eq!(manifest.captures.len(), 3);
        assert_eq!(export.readiness.provider_calls, 0);
        assert_eq!(export.readiness.inventory.listing_count, 3);
        assert_eq!(export.readiness.inventory.submission_count, 10);
        assert_eq!(export.readiness.inventory.bound_submission_count, 3);
        assert_eq!(export.readiness.inventory.unbound_submission_count, 7);
        assert_eq!(export.readiness.inventory.selected_capture_count, 3);
        assert_eq!(export.readiness.inventory.ready_capture_count, 3);
        assert_eq!(export.readiness.warning_issue_count, 7);
        assert_eq!(
            export.readiness.excluded_unbound_submission_ids,
            fixture.submission_ids[3..]
        );
        assert!(export
            .readiness
            .issues
            .iter()
            .all(|issue| issue.code == ReadinessIssueCode::ExcludedUnboundSubmission));
        assert_eq!(
            export.readiness.database.physical_integrity_check,
            ReadinessCheckStatus::Passed
        );
        assert_eq!(
            export.readiness.database.foreign_key_check,
            ReadinessCheckStatus::Passed
        );

        let serialized = serde_json::to_string(&export.readiness).unwrap();
        assert!(!serialized.contains("bound capture 0"));
        assert!(!serialized.contains("developer@localhost"));
        assert!(!serialized.contains(&manifest.captures[0].plugin_public_key_base64));
        assert!(!serialized.contains(&manifest.captures[0].signature_base64));
    }

    #[tokio::test]
    async fn all_bound_import_keeps_the_seven_unbound_exclusions_out_of_the_target() {
        let fixture = fixture(3, 7).await;
        let export = all_bound(&fixture.db, 3).await;
        let manifest = export.manifest.as_ref().expect("ready manifest");
        let target = AppDb::connect("sqlite::memory:").await.unwrap();

        let import = import_trusted_capture_manifest(&fixture.db, &target, manifest, true)
            .await
            .unwrap();
        assert_eq!(import.imported_capture_count, 3);
        let DatabaseBackend::Sqlite(pool) = target.backend() else {
            unreachable!()
        };
        let target_inventory: (i64, i64) =
            sqlx::query_as("SELECT COUNT(*), COUNT(canonical_listing_id) FROM plugin_submissions")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(target_inventory, (3, 0));
        for excluded_id in &export.readiness.excluded_unbound_submission_ids {
            let retained: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM plugin_submissions WHERE id = ?")
                    .bind(excluded_id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            assert_eq!(retained, 0, "excluded source submission {excluded_id}");
        }

        let replay = replay_captures(
            &target,
            None,
            &ReplayCapturesRequest {
                manifest,
                phase: ReplayPhase::Extraction,
                submission_id: None,
                apply: false,
                recover_stale: false,
            },
        )
        .await
        .unwrap();
        assert!(replay.dry_run);
        assert_eq!(replay.counts.selected, 3);
    }

    #[tokio::test]
    async fn all_bound_report_collects_expected_count_ambiguous_and_missing_blockers() {
        let fixture = fixture(1, 0).await;
        let missing_listing = insert_listing(
            &fixture.db,
            fixture.user_id,
            "https://example.test/missing",
            "N999AA",
        )
        .await;
        insert_capture(
            &fixture.db,
            &fixture.keys,
            fixture.user_id,
            Some(fixture.listing_ids[0]),
            "https://example.test/ambiguous",
            "<html>ambiguous</html>",
        )
        .await;

        let export = all_bound(&fixture.db, 2).await;
        assert!(export.manifest.is_none());
        assert!(!export.readiness.ready);
        assert_eq!(export.readiness.inventory.ambiguous_listing_count, 2);
        let codes = issue_codes(&export.readiness);
        assert!(codes.contains(&ReadinessIssueCode::ListingCaptureAmbiguous));
        assert!(codes.contains(&ReadinessIssueCode::ListingCaptureMissing));
        assert!(codes.contains(&ReadinessIssueCode::ExpectedCaptureCountMismatch));
        assert!(codes.contains(&ReadinessIssueCode::NoCapturesSelected));
        assert!(export
            .readiness
            .issues
            .iter()
            .any(|issue| issue.listing_id == Some(missing_listing)));
    }

    #[tokio::test]
    async fn corrupt_hash_signature_and_timestamp_are_all_reported() {
        let fixture = fixture(3, 0).await;
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        sqlx::query("UPDATE plugin_submissions SET rendered_html_sha256 = ? WHERE id = ?")
            .bind("0".repeat(64))
            .bind(fixture.submission_ids[0])
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("UPDATE plugin_submissions SET signature_base64 = 'invalid' WHERE id = ?")
            .bind(fixture.submission_ids[1])
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE plugin_installs SET created_at = 'not-a-timestamp' WHERE id = (SELECT plugin_install_id FROM plugin_submissions WHERE id = ?)",
        )
        .bind(fixture.submission_ids[2])
        .execute(pool)
        .await
        .unwrap();

        let export = all_bound(&fixture.db, 3).await;
        assert!(export.manifest.is_none());
        let codes = issue_codes(&export.readiness);
        assert!(codes.contains(&ReadinessIssueCode::RenderedHtmlHashMismatch));
        assert!(codes.contains(&ReadinessIssueCode::CaptureSignatureInvalid));
        assert!(codes.contains(&ReadinessIssueCode::CaptureTimestampChronologyInvalid));
        assert_eq!(export.readiness.inventory.blocked_capture_count, 3);
    }

    #[tokio::test]
    async fn duplicate_registration_and_canonical_source_url_block_both_captures() {
        let fixture = fixture(0, 0).await;
        let shared_url = "https://EXAMPLE.test/listing/shared#first";
        for fragment in ["first", "second"] {
            let listing_id = insert_listing(
                &fixture.db,
                fixture.user_id,
                &format!("https://example.test/listing/shared#{fragment}"),
                "N500AA",
            )
            .await;
            insert_capture(
                &fixture.db,
                &fixture.keys,
                fixture.user_id,
                Some(listing_id),
                &shared_url.replace("first", fragment),
                &format!("<html>{fragment}</html>"),
            )
            .await;
        }

        let export = all_bound(&fixture.db, 2).await;
        assert!(export.manifest.is_none());
        let codes = issue_codes(&export.readiness);
        assert!(codes.contains(&ReadinessIssueCode::DuplicateRegistration));
        assert!(codes.contains(&ReadinessIssueCode::DuplicateSourceUrl));
        assert_eq!(export.readiness.inventory.blocked_capture_count, 2);
    }

    #[tokio::test]
    async fn all_bound_rejects_hostile_owner_and_source_bindings() {
        let fixture = fixture(0, 0).await;
        let hostile_user_id = insert_user(&fixture.db, 1).await;

        let owner_listing_url = "https://example.test/listing/hostile-owner";
        let owner_listing =
            insert_listing(&fixture.db, fixture.user_id, owner_listing_url, "N501AA").await;
        let owner_submission = insert_capture(
            &fixture.db,
            &fixture.keys,
            hostile_user_id,
            Some(owner_listing),
            owner_listing_url,
            "<html>hostile owner binding</html>",
        )
        .await;

        let source_listing = insert_listing(
            &fixture.db,
            fixture.user_id,
            "https://example.test/listing/authentic-source",
            "N502AA",
        )
        .await;
        let source_submission = insert_capture(
            &fixture.db,
            &fixture.keys,
            fixture.user_id,
            Some(source_listing),
            "https://EXAMPLE.test/listing/authentic-source",
            "<html>hostile source binding</html>",
        )
        .await;

        let export = all_bound(&fixture.db, 2).await;
        assert!(export.manifest.is_none());
        let owner_report = export
            .readiness
            .captures
            .iter()
            .find(|capture| capture.submission_id == owner_submission)
            .unwrap();
        assert_eq!(
            owner_report.issue_codes,
            vec![ReadinessIssueCode::ListingOwnerMismatch]
        );
        let source_report = export
            .readiness
            .captures
            .iter()
            .find(|capture| capture.submission_id == source_submission)
            .unwrap();
        assert_eq!(
            source_report.issue_codes,
            vec![ReadinessIssueCode::ListingSourceUrlMismatch]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sqlite_export_reads_one_snapshot_and_restores_prior_query_only() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("snapshot.sqlite3");
        let database_url = format!("sqlite://{}", database_path.display());
        let db = AppDb::connect(&database_url).await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("PRAGMA journal_mode = WAL")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE users SET created_at = '2025-01-01 00:00:00', updated_at = '2025-01-02 00:00:00' WHERE id = ?",
        )
        .bind(user.id)
        .execute(pool)
        .await
        .unwrap();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let keys = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let listing_id =
            insert_listing(&db, user.id, "https://example.test/snapshot", "N700AA").await;
        let submission_id = insert_capture(
            &db,
            &keys,
            user.id,
            Some(listing_id),
            "https://example.test/snapshot",
            "<html>snapshot</html>",
        )
        .await;

        let selection = ValidatedSelection::AllBound {
            expected_capture_count: Some(1),
        };
        let writer = pool.clone();
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA query_only = ON")
            .execute(&mut *connection)
            .await
            .unwrap();
        let snapshot = load_read_only_sqlite_snapshot(&mut connection, &selection, || async move {
            sqlx::query("UPDATE plugin_submissions SET rendered_html_sha256 = ? WHERE id = ?")
                .bind("0".repeat(64))
                .bind(submission_id)
                .execute(&writer)
                .await
                .unwrap();
        })
        .await
        .unwrap();
        let restored_query_only: i64 = sqlx::query_scalar("PRAGMA query_only")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(restored_query_only, 1);
        sqlx::query("PRAGMA query_only = OFF")
            .execute(&mut *connection)
            .await
            .unwrap();
        let before_write = build_export(selection, snapshot).unwrap();
        assert!(before_write.manifest.is_some());

        let after_write = all_bound(&db, 1).await;
        assert!(after_write.manifest.is_none());
        assert!(issue_codes(&after_write.readiness)
            .contains(&ReadinessIssueCode::RenderedHtmlHashMismatch));

        let correct_hash = sha256_hex(b"<html>snapshot</html>");
        sqlx::query("UPDATE plugin_submissions SET rendered_html_sha256 = ? WHERE id = ?")
            .bind(correct_hash)
            .bind(submission_id)
            .execute(pool)
            .await
            .unwrap();
        let selection = ValidatedSelection::AllBound {
            expected_capture_count: Some(1),
        };
        load_read_only_sqlite_snapshot(&mut connection, &selection, || async {})
            .await
            .unwrap();
        let restored_query_only: i64 = sqlx::query_scalar("PRAGMA query_only")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(restored_query_only, 0);
        assert!(all_bound(&db, 1).await.manifest.is_some());
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_export_matches_the_sqlite_readiness_contract() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::query("DROP SCHEMA public CASCADE")
            .execute(&reset)
            .await
            .unwrap();
        sqlx::query("CREATE SCHEMA public")
            .execute(&reset)
            .await
            .unwrap();
        reset.close().await;

        let db = AppDb::connect(&database_url).await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE users SET created_at = '2025-01-01 00:00:00', updated_at = '2025-01-02 00:00:00' WHERE id = $1",
        )
        .bind(user.id)
        .execute(pool)
        .await
        .unwrap();
        let variant_id: i64 =
            sqlx::query_scalar("SELECT id FROM aircraft_model_variants ORDER BY id LIMIT 1")
                .fetch_one(pool)
                .await
                .unwrap();
        let source_url = "https://example.test/postgres-replay";
        let listing_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO aircraft_sale_listings (
                 aircraft_model_variant_id, created_by_user_id, source_url,
                 model_year, asking_price_usd, airframe_hours, registration_number
               ) VALUES ($1, $2, $3, 2020, 200000, 500, 'N800AA') RETURNING id"#,
        )
        .bind(variant_id)
        .bind(user.id)
        .bind(source_url)
        .fetch_one(pool)
        .await
        .unwrap();

        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let keys = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let install_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_installs (user_id, public_key_base64, created_at)
               VALUES ($1, $2, '2026-01-01 00:00:00') RETURNING id"#,
        )
        .bind(user.id)
        .bind(BASE64_STANDARD.encode(keys.public_key().as_ref()))
        .fetch_one(pool)
        .await
        .unwrap();
        let html = "<html>postgres replay capture</html>";
        let hash = sha256_hex(html.as_bytes());
        let signature = BASE64_STANDARD.encode(
            keys.sign(
                &rng,
                signature_message(install_id, source_url, &hash).as_bytes(),
            )
            .unwrap()
            .as_ref(),
        );
        sqlx::query(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, submitted_at, rendered_html,
                 rendered_html_sha256, signature_base64, canonical_listing_id
               ) VALUES ($1, $2, $3, '2026-01-02 00:00:00', $4, $5, $6, $7)"#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind(source_url)
        .bind(html)
        .bind(hash)
        .bind(signature)
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();

        let export = all_bound(&db, 1).await;
        assert!(export.manifest.is_some());
        assert!(export.readiness.ready);
        assert_eq!(
            export.readiness.database.backend,
            ReadinessDatabaseBackend::Postgres
        );
        assert!(export.readiness.database.schema_contract_attested);
        assert_eq!(
            export.readiness.database.physical_integrity_check,
            ReadinessCheckStatus::NotApplicable
        );
        assert_eq!(
            export.readiness.database.foreign_key_check,
            ReadinessCheckStatus::Passed
        );
        assert_eq!(export.readiness.provider_calls, 0);
    }

    #[test]
    fn timestamp_parser_accepts_supported_offsets_and_rejects_invalid_dates() {
        assert_eq!(
            parse_replay_timestamp("2026-01-02 03:04:05"),
            parse_replay_timestamp("2026-01-02T03:04:05Z")
        );
        assert_eq!(
            parse_replay_timestamp("2026-01-02T04:04:05+01:00"),
            parse_replay_timestamp("2026-01-02T03:04:05Z")
        );
        assert!(parse_replay_timestamp("2026-02-29 00:00:00").is_none());
        assert!(parse_replay_timestamp("not-a-timestamp").is_none());
    }
}
