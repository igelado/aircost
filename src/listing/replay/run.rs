//! Durable, resumable coordination for trusted-capture replay.
//!
//! The ledger stores only manifest correlation, typed operational state, and
//! bounded outcome codes. Capture bytes and extraction/provider payloads stay
//! in `plugin_submissions` and the existing domain stores.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use super::{validate_trusted_capture_manifest, TrustedCaptureManifest};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::GeminiListingExtractor;
use crate::gemini::usage::{
    Record as GeminiUsageRecord, SourceCorrelation, Store as GeminiUsageStore,
};
use crate::plugin::{
    checkpoint_plugin_submission_extraction, inspect_plugin_replay_capture_state,
    materialize_plugin_submission_checkpoint, plugin_submission_owner, PluginListingReplayOutcome,
    PluginStoreError,
};

const STALE_RECOVERY_THRESHOLD: Duration = Duration::from_secs(60 * 60);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
static TOKEN_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayPhase {
    Extraction,
    Materialization,
}

impl ReplayPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Extraction => "extraction",
            Self::Materialization => "materialization",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhaseState {
    Queued,
    Running,
    Succeeded,
    Rejected,
    Failed,
    Blocked,
}

impl PhaseState {
    fn parse(value: &str) -> ReplayRunResult<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "rejected" => Ok(Self::Rejected),
            "failed" => Ok(Self::Failed),
            "blocked" => Ok(Self::Blocked),
            other => Err(ReplayRunError::Database(format!(
                "stored replay phase has invalid state {other:?}"
            ))),
        }
    }

    #[cfg(test)]
    fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Rejected)
    }
}

#[derive(Clone, Debug)]
pub struct ReplayCapturesRequest<'a> {
    pub manifest: &'a TrustedCaptureManifest,
    pub phase: ReplayPhase,
    pub submission_id: Option<i64>,
    pub apply: bool,
    pub recover_stale: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReplayCapturesCounts {
    pub selected: usize,
    pub ready: usize,
    pub already_complete: usize,
    pub blocked: usize,
    pub succeeded: usize,
    pub rejected: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReplayCapturesReport {
    pub dry_run: bool,
    pub manifest_sha256: String,
    pub run_id: Option<i64>,
    pub phase: ReplayPhase,
    pub gemini_usage: ReplayGeminiUsage,
    pub counts: ReplayCapturesCounts,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReplayGeminiUsage {
    pub scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub logical_requests: usize,
    pub transport_attempts: u64,
    pub retries: u64,
    pub billable_usage_complete: bool,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub thought_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub tool_tokens: Option<u64>,
    pub search_queries: Option<u64>,
    pub estimated_cost_microusd: Option<u64>,
}

#[derive(Debug)]
pub enum ReplayRunError {
    Validation(String),
    Conflict(String),
    Database(String),
}

impl fmt::Display for ReplayRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Conflict(message) | Self::Database(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for ReplayRunError {}

impl From<sqlx::Error> for ReplayRunError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

pub type ReplayRunResult<T> = Result<T, ReplayRunError>;

#[derive(Debug, FromRow)]
struct CaptureRow {
    submission_id: i64,
    rendered_html_sha256: String,
}

#[derive(Debug, FromRow)]
struct ExistingRunRow {
    id: i64,
    manifest_version: i64,
    manifest_capture_count: i64,
    status: String,
    active_phase: Option<String>,
    heartbeat_at_epoch_seconds: Option<i64>,
}

#[derive(Debug, FromRow)]
struct ItemRow {
    plugin_submission_id: i64,
    position: i64,
    expected_rendered_html_sha256: String,
    extraction_state: String,
    materialization_state: String,
}

#[derive(Clone, Debug, FromRow)]
struct ClaimedItem {
    id: i64,
    submission_id: i64,
    extracted_listing_sha256: Option<String>,
}

pub async fn replay_captures(
    db: &AppDb,
    extractor: Option<&GeminiListingExtractor>,
    request: &ReplayCapturesRequest<'_>,
) -> ReplayRunResult<ReplayCapturesReport> {
    validate_trusted_capture_manifest(request.manifest).map_err(ReplayRunError::Validation)?;
    let selected = selected_submission_ids(request)?;
    validate_target_captures(db, request.manifest).await?;
    let existing_run = find_run(db, &request.manifest.manifest_sha256).await?;
    if !request.apply {
        return dry_run_report(db, existing_run.as_ref(), request, &selected).await;
    }
    if extractor.is_none() {
        return Err(ReplayRunError::Validation(format!(
            "--apply {} requires configured listing services",
            request.phase.label()
        )));
    }
    let run = ensure_run(db, request.manifest).await?;
    let usage_correlation_id = replay_usage_correlation(request.manifest, request.phase);
    if run.status == "completed" {
        let gemini_usage = gemini_usage_for_phase(db, usage_correlation_id).await?;
        return report_from_ledger(db, run.id, request, &selected, gemini_usage).await;
    }
    let owner_token = new_owner_token(request.manifest, request.phase)?;
    acquire_run(
        db,
        run.id,
        request.phase,
        &owner_token,
        request.recover_stale,
    )
    .await?;

    let processing: ReplayRunResult<()> = async {
        for submission_id in selected.iter().copied() {
            heartbeat_run(db, run.id, &owner_token).await?;
            if request.phase == ReplayPhase::Materialization {
                match plugin_submission_owner(db, submission_id).await {
                    Ok(owner) => {
                        match inspect_plugin_replay_capture_state(db, owner.id, submission_id).await
                        {
                            Ok(state)
                                if reconcile_materialization_domain_state(
                                    db,
                                    run.id,
                                submission_id,
                                &owner_token,
                                state.checkpoint.as_ref(),
                                state.materialization_receipt_listing_id,
                                )
                                .await? =>
                            {
                                continue;
                            }
                            Ok(_) => {}
                            Err(PluginStoreError::Database(message)) => {
                                return Err(ReplayRunError::Database(message));
                            }
                            Err(_) => {}
                        }
                    }
                    Err(PluginStoreError::Database(message)) => {
                        return Err(ReplayRunError::Database(message));
                    }
                    Err(error) => {
                        if reject_unclaimable_capture(
                            db,
                            run.id,
                            submission_id,
                            &owner_token,
                            match error {
                                PluginStoreError::Permission(_) => "capture_authentication_failed",
                                PluginStoreError::NotFound(_) => "capture_not_found",
                                PluginStoreError::Validation(_) => "capture_validation_failed",
                                PluginStoreError::Database(_) => unreachable!(),
                                PluginStoreError::AircraftAdmission(error) => {
                                    return Err(ReplayRunError::Validation(error.to_string()));
                                }
                                PluginStoreError::AdmissionBlocked(reason) => {
                                    return Err(ReplayRunError::Validation(format!(
                                        "replay admission is blocked: {}",
                                        reason.code()
                                    )));
                                }
                            },
                        )
                        .await?
                        {
                            continue;
                        }
                    }
                }
            }
            let Some(claimed) =
                claim_item(db, run.id, submission_id, request.phase, &owner_token).await?
            else {
                continue;
            };
            let owner = match plugin_submission_owner(db, submission_id).await {
                Ok(owner) => owner,
                Err(error) => {
                    finish_capture_admission_error(
                        db,
                        run.id,
                        claimed,
                        request.phase,
                        &owner_token,
                        &error,
                    )
                    .await?;
                    continue;
                }
            };
            let state = match inspect_plugin_replay_capture_state(db, owner.id, submission_id).await
            {
                Ok(state) => state,
                Err(error) => {
                    finish_capture_admission_error(
                        db,
                        run.id,
                        claimed,
                        request.phase,
                        &owner_token,
                        &error,
                    )
                    .await?;
                    continue;
                }
            };
            let operation = match request.phase {
                ReplayPhase::Extraction => {
                    if state.checkpoint.is_some() || state.canonical_listing_id.is_some() {
                        let checkpoint_sha256 = state
                            .checkpoint
                            .as_ref()
                            .map(|checkpoint| checkpoint.extracted_listing_sha256.as_str())
                            .ok_or_else(|| {
                                ReplayRunError::Validation(
                                    "a bound replay capture is missing its extraction checkpoint"
                                        .to_string(),
                                )
                            })?;
                        finish_succeeded(
                            db,
                            run.id,
                            claimed,
                            request.phase,
                            &owner_token,
                            Some(checkpoint_sha256),
                            state.canonical_listing_id,
                        )
                        .await
                    } else {
                        let extractor = extractor
                            .expect("extraction apply checked above")
                            .clone()
                            .with_usage_scope(
                                usage_correlation_id.clone(),
                                None,
                                Some(SourceCorrelation {
                                    kind: "plugin_submission".to_string(),
                                    id: submission_id.to_string(),
                                }),
                            );
                        let result = with_heartbeat(db, run.id, &owner_token, async {
                            checkpoint_plugin_submission_extraction(
                                db,
                                &owner,
                                submission_id,
                                &extractor,
                            )
                            .await
                        })
                        .await?;
                        match result {
                            Ok(checkpoint) => {
                                finish_succeeded(
                                    db,
                                    run.id,
                                    claimed,
                                    request.phase,
                                    &owner_token,
                                    Some(&checkpoint.extracted_listing_sha256),
                                    None,
                                )
                                .await
                            }
                            Err(error) => {
                                finish_operation_error(
                                    db,
                                    run.id,
                                    claimed,
                                    request.phase,
                                    &owner_token,
                                    &error,
                                )
                                .await
                            }
                        }
                    }
                }
                ReplayPhase::Materialization => {
                    if let Some(listing_id) = state.materialization_receipt_listing_id {
                        finish_succeeded(
                            db,
                            run.id,
                            claimed,
                            request.phase,
                            &owner_token,
                            None,
                            Some(listing_id),
                        )
                        .await
                    } else if state.checkpoint.is_none() {
                        finish_failed(
                            db,
                            run.id,
                            claimed,
                            request.phase,
                            &owner_token,
                            "operation_failed",
                        )
                        .await
                    } else {
                        let extractor = extractor
                            .expect("materialization apply checked above")
                            .clone()
                            .with_usage_scope(
                                usage_correlation_id.clone(),
                                None,
                                Some(SourceCorrelation {
                                    kind: "plugin_submission".to_string(),
                                    id: submission_id.to_string(),
                                }),
                            );
                        let result = with_heartbeat(db, run.id, &owner_token, async {
                            materialize_plugin_submission_checkpoint(
                                db,
                                &owner,
                                submission_id,
                                claimed.extracted_listing_sha256.as_deref().ok_or_else(|| {
                                    PluginStoreError::Validation(
                                        "materialization claim is missing its pinned checkpoint hash"
                                            .to_string(),
                                    )
                                })?,
                                &extractor,
                            )
                            .await
                        })
                        .await?;
                        match result {
                            Ok(PluginListingReplayOutcome::Materialized { listing, .. }) => {
                                finish_succeeded(
                                    db,
                                    run.id,
                                    claimed,
                                    request.phase,
                                    &owner_token,
                                    None,
                                    Some(listing.id),
                                )
                                .await
                            }
                            Ok(PluginListingReplayOutcome::Rejected { rejection, .. }) => {
                                finish_rejected(
                                    db,
                                    run.id,
                                    claimed,
                                    request.phase,
                                    &owner_token,
                                    rejection.stage(),
                                    rejection.code(),
                                )
                                .await
                            }
                            Err(error) => {
                                finish_operation_error(
                                    db,
                                    run.id,
                                    claimed,
                                    request.phase,
                                    &owner_token,
                                    &error,
                                )
                                .await
                            }
                        }
                    }
                }
            };
            operation?;
        }
        Ok(())
    }
    .await;
    match processing {
        Ok(()) => {}
        Err(error @ ReplayRunError::Conflict(_)) => return Err(error),
        Err(error) => {
            let _ = release_run(db, run.id, &owner_token).await;
            return Err(error);
        }
    }
    release_run(db, run.id, &owner_token).await?;
    let gemini_usage = gemini_usage_for_phase(db, usage_correlation_id).await?;
    report_from_ledger(db, run.id, request, &selected, gemini_usage).await
}

/// A malformed capture without a resolvable owner cannot satisfy extraction,
/// so a materialization run records the capture-level terminal result even
/// though the normal materialization claim is intentionally blocked.
async fn reject_unclaimable_capture(
    db: &AppDb,
    run_id: i64,
    submission_id: i64,
    owner_token: &str,
    reason_code: &str,
) -> ReplayRunResult<bool> {
    validate_closed_rejection("capture_admission", reason_code)?;
    let sql = db.sql(
        r#"UPDATE listing_replay_run_items
           SET extraction_state = 'rejected', materialization_state = 'blocked',
               extraction_attempt_count = extraction_attempt_count + 1,
               extraction_started_at = CURRENT_TIMESTAMP,
               extraction_completed_at = CURRENT_TIMESTAMP,
               terminal_rejection_phase = 'extraction',
               terminal_rejection_stage = 'capture_admission',
               terminal_rejection_reason_code = ?,
               last_failure_phase = NULL, last_failure_reason_code = NULL,
               updated_at = CURRENT_TIMESTAMP
           WHERE run_id = ? AND plugin_submission_id = ?
             AND extraction_state IN ('queued', 'failed')
             AND materialization_state = 'blocked'
             AND EXISTS (SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
               AND run.status = 'running' AND run.active_phase = 'materialization'
               AND run.owner_token = ?)"#,
    );
    Ok(execute(
        db,
        &sql,
        &[
            Bind::Text(reason_code),
            Bind::I64(run_id),
            Bind::I64(submission_id),
            Bind::I64(run_id),
            Bind::Text(owner_token),
        ],
    )
    .await?
        == 1)
}

/// Reconcile authoritative domain commits before materialization can claim an
/// item. Only an exact completion receipt proves materialization succeeded; a
/// binding without that receipt is a resumable partial commit. A valid
/// checkpoint proves extraction succeeded. The owner token fences a recovered
/// worker.
async fn reconcile_materialization_domain_state(
    db: &AppDb,
    run_id: i64,
    submission_id: i64,
    owner_token: &str,
    checkpoint: Option<&crate::plugin::PluginExtractionCheckpoint>,
    listing_id: Option<i64>,
) -> ReplayRunResult<bool> {
    if let Some(listing_id) = listing_id {
        let checkpoint = checkpoint.ok_or_else(|| {
            ReplayRunError::Validation(
                "a bound replay capture is missing its extraction checkpoint".to_string(),
            )
        })?;
        let sql = db.sql(
            r#"UPDATE listing_replay_run_items
               SET extraction_state = 'succeeded', materialization_state = 'succeeded',
                   resulting_listing_id = ?, extracted_listing_sha256 = ?,
                   extraction_completed_at = COALESCE(extraction_completed_at, CURRENT_TIMESTAMP),
                   materialization_completed_at = CURRENT_TIMESTAMP,
                   last_failure_phase = NULL, last_failure_reason_code = NULL,
                   updated_at = CURRENT_TIMESTAMP
               WHERE run_id = ? AND plugin_submission_id = ?
                 AND extraction_state <> 'rejected' AND materialization_state <> 'rejected'
                 AND (extracted_listing_sha256 IS NULL OR extracted_listing_sha256 = ?)
                 AND EXISTS (SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                   AND run.status = 'running' AND run.active_phase = 'materialization'
                   AND run.owner_token = ?)"#,
        );
        let changed = execute(
            db,
            &sql,
            &[
                Bind::I64(listing_id),
                Bind::Text(&checkpoint.extracted_listing_sha256),
                Bind::I64(run_id),
                Bind::I64(submission_id),
                Bind::Text(&checkpoint.extracted_listing_sha256),
                Bind::I64(run_id),
                Bind::Text(owner_token),
            ],
        )
        .await?;
        if changed != 1 {
            return Err(ReplayRunError::Conflict(
                "replay ownership or terminal state changed during binding reconciliation"
                    .to_string(),
            ));
        }
        return Ok(true);
    }
    if let Some(checkpoint) = checkpoint {
        let sql = db.sql(
            r#"UPDATE listing_replay_run_items
               SET extraction_state = 'succeeded',
                   extracted_listing_sha256 = ?,
                   materialization_state = CASE
                     WHEN materialization_state = 'blocked' THEN 'queued'
                     ELSE materialization_state END,
                   extraction_completed_at = COALESCE(extraction_completed_at, CURRENT_TIMESTAMP),
                   last_failure_phase = CASE WHEN last_failure_phase = 'extraction'
                     THEN NULL ELSE last_failure_phase END,
                   last_failure_reason_code = CASE WHEN last_failure_phase = 'extraction'
                     THEN NULL ELSE last_failure_reason_code END,
                   updated_at = CURRENT_TIMESTAMP
               WHERE run_id = ? AND plugin_submission_id = ?
                 AND extraction_state <> 'rejected'
                 AND (extracted_listing_sha256 IS NULL OR extracted_listing_sha256 = ?)
                 AND EXISTS (SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                   AND run.status = 'running' AND run.active_phase = 'materialization'
                   AND run.owner_token = ?)"#,
        );
        let changed = execute(
            db,
            &sql,
            &[
                Bind::Text(&checkpoint.extracted_listing_sha256),
                Bind::I64(run_id),
                Bind::I64(submission_id),
                Bind::Text(&checkpoint.extracted_listing_sha256),
                Bind::I64(run_id),
                Bind::Text(owner_token),
            ],
        )
        .await?;
        if changed != 1 {
            return Err(ReplayRunError::Conflict(
                "replay ownership or terminal state changed during checkpoint reconciliation"
                    .to_string(),
            ));
        }
    }
    Ok(false)
}

async fn with_heartbeat<F, T>(
    db: &AppDb,
    run_id: i64,
    owner_token: &str,
    operation: F,
) -> ReplayRunResult<T>
where
    F: std::future::Future<Output = T>,
{
    with_heartbeat_interval(db, run_id, owner_token, HEARTBEAT_INTERVAL, operation).await
}

async fn with_heartbeat_interval<F, T>(
    db: &AppDb,
    run_id: i64,
    owner_token: &str,
    interval: Duration,
    operation: F,
) -> ReplayRunResult<T>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(operation);
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep(interval) => {
                if heartbeat_run(db, run_id, owner_token).await? == 0 {
                    return Err(ReplayRunError::Conflict(
                        "replay ownership changed while an operation was running".to_string()
                    ));
                }
            }
            result = &mut operation => return Ok(result),
        }
    }
}

fn selected_submission_ids(request: &ReplayCapturesRequest<'_>) -> ReplayRunResult<BTreeSet<i64>> {
    let manifest_ids = request
        .manifest
        .captures
        .iter()
        .map(|entry| entry.submission_id)
        .collect::<BTreeSet<_>>();
    if let Some(submission_id) = request.submission_id {
        if !manifest_ids.contains(&submission_id) {
            return Err(ReplayRunError::Validation(format!(
                "submission {submission_id} is not a member of this manifest"
            )));
        }
        Ok(BTreeSet::from([submission_id]))
    } else {
        Ok(manifest_ids)
    }
}

async fn validate_target_captures(
    db: &AppDb,
    manifest: &TrustedCaptureManifest,
) -> ReplayRunResult<()> {
    let expected = manifest
        .captures
        .iter()
        .map(|entry| (entry.submission_id, entry.rendered_html_sha256.as_str()))
        .collect::<BTreeMap<_, _>>();
    let sql = db.sql(
        "SELECT id AS submission_id, rendered_html_sha256 FROM plugin_submissions ORDER BY id",
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, CaptureRow>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, CaptureRow>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    let actual = rows
        .into_iter()
        .filter(|row| expected.contains_key(&row.submission_id))
        .map(|row| (row.submission_id, row.rendered_html_sha256))
        .collect::<BTreeMap<_, _>>();
    for (submission_id, expected_sha) in expected {
        match actual.get(&submission_id) {
            None => {
                return Err(ReplayRunError::Validation(format!(
                    "manifest submission {submission_id} is not present in the replay target"
                )))
            }
            Some(actual_sha) if actual_sha != expected_sha => {
                return Err(ReplayRunError::Validation(format!(
                    "manifest submission {submission_id} capture hash drifted in the replay target"
                )))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

async fn find_run(db: &AppDb, manifest_sha256: &str) -> ReplayRunResult<Option<ExistingRunRow>> {
    let sql = db.sql(
        r#"SELECT id, manifest_version, manifest_capture_count, status, active_phase,
                  heartbeat_at_epoch_seconds
           FROM listing_replay_runs WHERE manifest_sha256 = ?"#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as(&sql)
            .bind(manifest_sha256)
            .fetch_optional(pool)
            .await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as(&sql)
            .bind(manifest_sha256)
            .fetch_optional(pool)
            .await?),
    }
}

async fn ensure_run(
    db: &AppDb,
    manifest: &TrustedCaptureManifest,
) -> ReplayRunResult<ExistingRunRow> {
    let insert_run = db.sql(
        r#"INSERT INTO listing_replay_runs
             (manifest_version, manifest_sha256, manifest_capture_count)
           VALUES (?, ?, ?) ON CONFLICT (manifest_sha256) DO NOTHING RETURNING id"#,
    );
    let insert_item = db.sql(
        r#"INSERT INTO listing_replay_run_items
             (run_id, plugin_submission_id, position, expected_rendered_html_sha256)
           VALUES (?, ?, ?, ?)"#,
    );
    macro_rules! ensure_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let inserted = sqlx::query_scalar::<_, i64>(&insert_run)
                .bind(manifest.version as i64)
                .bind(&manifest.manifest_sha256)
                .bind(manifest.captures.len() as i64)
                .fetch_optional(&mut *transaction)
                .await?;
            if let Some(run_id) = inserted {
                for (position, entry) in manifest.captures.iter().enumerate() {
                    sqlx::query(&insert_item)
                        .bind(run_id)
                        .bind(entry.submission_id)
                        .bind(position as i64)
                        .bind(&entry.rendered_html_sha256)
                        .execute(&mut *transaction)
                        .await?;
                }
            }
            transaction.commit().await?;
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => ensure_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => ensure_in_transaction!(pool),
    }
    let run = find_run(db, &manifest.manifest_sha256)
        .await?
        .ok_or_else(|| ReplayRunError::Database("replay run disappeared after creation".into()))?;
    validate_run_membership(db, &run, manifest).await?;
    Ok(run)
}

async fn validate_run_membership(
    db: &AppDb,
    run: &ExistingRunRow,
    manifest: &TrustedCaptureManifest,
) -> ReplayRunResult<()> {
    if run.manifest_version != manifest.version as i64
        || run.manifest_capture_count != manifest.captures.len() as i64
    {
        return Err(ReplayRunError::Conflict(
            "stored replay run does not match the manifest header".to_string(),
        ));
    }
    let sql = db.sql(
        r#"SELECT plugin_submission_id, position, expected_rendered_html_sha256,
                  extraction_state, materialization_state
           FROM listing_replay_run_items WHERE run_id = ? ORDER BY position"#,
    );
    let rows: Vec<ItemRow> = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_as(&sql).bind(run.id).fetch_all(pool).await?,
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as(&sql).bind(run.id).fetch_all(pool).await?
        }
    };
    if rows.len() != manifest.captures.len()
        || rows.iter().zip(&manifest.captures).any(|(row, entry)| {
            row.position < 0
                || row.position as usize >= manifest.captures.len()
                || row.plugin_submission_id != entry.submission_id
                || row.expected_rendered_html_sha256 != entry.rendered_html_sha256
        })
    {
        return Err(ReplayRunError::Conflict(
            "stored replay run membership drifted from the manifest".to_string(),
        ));
    }
    Ok(())
}

async fn acquire_run(
    db: &AppDb,
    run_id: i64,
    phase: ReplayPhase,
    owner_token: &str,
    recover_stale: bool,
) -> ReplayRunResult<()> {
    let now = epoch_seconds()?;
    let stale_before = now - STALE_RECOVERY_THRESHOLD.as_secs() as i64;
    let running = current_running_run(db).await?;
    if let Some(active) = running {
        if !recover_stale {
            return Err(ReplayRunError::Conflict(format!(
                "replay run {} is active in phase {}; use --recover-stale only after confirming its worker stopped",
                active.id,
                active.active_phase.as_deref().unwrap_or("unknown")
            )));
        }
        if active
            .heartbeat_at_epoch_seconds
            .is_none_or(|heartbeat| heartbeat > stale_before)
        {
            return Err(ReplayRunError::Conflict(format!(
                "replay run {} has a recent heartbeat and cannot be recovered",
                active.id
            )));
        }
        recover_stale_run(db, active.id, now).await?;
    }
    let sql = db.sql(
        r#"UPDATE listing_replay_runs
           SET status = 'running', active_phase = ?, owner_token = ?,
               heartbeat_at_epoch_seconds = ?, started_at = CURRENT_TIMESTAMP,
               completed_at = NULL, updated_at = CURRENT_TIMESTAMP
           WHERE id = ? AND status = 'queued'"#,
    );
    let changed = execute(
        db,
        &sql,
        &[
            Bind::Text(phase.label()),
            Bind::Text(owner_token),
            Bind::I64(now),
            Bind::I64(run_id),
        ],
    )
    .await?;
    if changed != 1 {
        return Err(ReplayRunError::Conflict(
            "replay run is not available for execution".to_string(),
        ));
    }
    Ok(())
}

async fn current_running_run(db: &AppDb) -> ReplayRunResult<Option<ExistingRunRow>> {
    let sql = db.sql(
        r#"SELECT id, manifest_version, manifest_capture_count, status, active_phase,
                  heartbeat_at_epoch_seconds
           FROM listing_replay_runs WHERE status = 'running' LIMIT 1"#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as(&sql).fetch_optional(pool).await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as(&sql).fetch_optional(pool).await?),
    }
}

async fn recover_stale_run(db: &AppDb, run_id: i64, now: i64) -> ReplayRunResult<()> {
    let extraction = db.sql(
        r#"UPDATE listing_replay_run_items SET extraction_state = 'failed',
             materialization_state = 'blocked', last_failure_phase = 'extraction',
             last_failure_reason_code = 'operation_failed', extraction_completed_at = CURRENT_TIMESTAMP,
             updated_at = CURRENT_TIMESTAMP
           WHERE run_id = ? AND extraction_state = 'running'"#,
    );
    let materialization = db.sql(
        r#"UPDATE listing_replay_run_items SET materialization_state = 'failed',
             last_failure_phase = 'materialization', last_failure_reason_code = 'operation_failed',
             materialization_completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
           WHERE run_id = ? AND materialization_state = 'running'"#,
    );
    let run = db.sql(
        r#"UPDATE listing_replay_runs SET status = 'queued', active_phase = NULL,
             owner_token = NULL, heartbeat_at_epoch_seconds = NULL, completed_at = NULL,
             updated_at = CURRENT_TIMESTAMP
           WHERE id = ? AND status = 'running' AND heartbeat_at_epoch_seconds <= ?"#,
    );
    macro_rules! recover_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&extraction)
                .bind(run_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&materialization)
                .bind(run_id)
                .execute(&mut *transaction)
                .await?;
            let changed = sqlx::query(&run)
                .bind(run_id)
                .bind(now - STALE_RECOVERY_THRESHOLD.as_secs() as i64)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if changed != 1 {
                return Err(ReplayRunError::Conflict(
                    "stale replay ownership changed before recovery".to_string(),
                ));
            }
            transaction.commit().await?;
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => recover_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => recover_in_transaction!(pool),
    }
    Ok(())
}

async fn heartbeat_run(db: &AppDb, run_id: i64, owner_token: &str) -> ReplayRunResult<u64> {
    let sql = db.sql(
        r#"UPDATE listing_replay_runs SET heartbeat_at_epoch_seconds = ?,
             updated_at = CURRENT_TIMESTAMP
           WHERE id = ? AND status = 'running' AND owner_token = ?"#,
    );
    execute(
        db,
        &sql,
        &[
            Bind::I64(epoch_seconds()?),
            Bind::I64(run_id),
            Bind::Text(owner_token),
        ],
    )
    .await
}

async fn claim_item(
    db: &AppDb,
    run_id: i64,
    submission_id: i64,
    phase: ReplayPhase,
    owner_token: &str,
) -> ReplayRunResult<Option<ClaimedItem>> {
    let sql = match phase {
        ReplayPhase::Extraction => db.sql(
            r#"UPDATE listing_replay_run_items SET extraction_state = 'running',
                 extraction_attempt_count = extraction_attempt_count + 1,
                 extraction_started_at = CURRENT_TIMESTAMP, extraction_completed_at = NULL,
                 terminal_rejection_phase = NULL, terminal_rejection_stage = NULL,
                 terminal_rejection_reason_code = NULL, last_failure_phase = NULL,
                 last_failure_reason_code = NULL, updated_at = CURRENT_TIMESTAMP
               WHERE run_id = ? AND plugin_submission_id = ?
                 AND extraction_state IN ('queued', 'failed')
                 AND EXISTS (SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                   AND run.status = 'running' AND run.active_phase = 'extraction'
                   AND run.owner_token = ?)
               RETURNING id, plugin_submission_id AS submission_id,
                         extracted_listing_sha256"#,
        ),
        ReplayPhase::Materialization => db.sql(
            r#"UPDATE listing_replay_run_items SET materialization_state = 'running',
                 materialization_attempt_count = materialization_attempt_count + 1,
                 materialization_started_at = CURRENT_TIMESTAMP, materialization_completed_at = NULL,
                 terminal_rejection_phase = NULL, terminal_rejection_stage = NULL,
                 terminal_rejection_reason_code = NULL, last_failure_phase = NULL,
                 last_failure_reason_code = NULL, updated_at = CURRENT_TIMESTAMP
               WHERE run_id = ? AND plugin_submission_id = ?
                 AND extraction_state = 'succeeded'
                 AND materialization_state IN ('queued', 'failed')
                 AND EXISTS (SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                   AND run.status = 'running' AND run.active_phase = 'materialization'
                   AND run.owner_token = ?)
               RETURNING id, plugin_submission_id AS submission_id,
                         extracted_listing_sha256"#,
        ),
    };
    let item = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as(&sql)
                .bind(run_id)
                .bind(submission_id)
                .bind(run_id)
                .bind(owner_token)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as(&sql)
                .bind(run_id)
                .bind(submission_id)
                .bind(run_id)
                .bind(owner_token)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(item)
}

async fn finish_succeeded(
    db: &AppDb,
    run_id: i64,
    item: ClaimedItem,
    phase: ReplayPhase,
    owner_token: &str,
    checkpoint_sha256: Option<&str>,
    listing_id: Option<i64>,
) -> ReplayRunResult<()> {
    let sql = match phase {
        ReplayPhase::Extraction => db.sql(
            r#"UPDATE listing_replay_run_items SET extraction_state = 'succeeded',
                 materialization_state = 'queued', extraction_completed_at = CURRENT_TIMESTAMP,
                 extracted_listing_sha256 = ?,
                 last_failure_phase = NULL, last_failure_reason_code = NULL,
                 updated_at = CURRENT_TIMESTAMP
               WHERE id = ? AND run_id = ? AND plugin_submission_id = ?
                 AND extraction_state = 'running'
                 AND ? IS NOT NULL
                 AND (extracted_listing_sha256 IS NULL OR extracted_listing_sha256 = ?)
                 AND EXISTS (
                   SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                     AND run.status = 'running' AND run.owner_token = ?)"#,
        ),
        ReplayPhase::Materialization => db.sql(
            r#"UPDATE listing_replay_run_items SET materialization_state = 'succeeded',
                 resulting_listing_id = ?, materialization_completed_at = CURRENT_TIMESTAMP,
                 last_failure_phase = NULL, last_failure_reason_code = NULL,
                 updated_at = CURRENT_TIMESTAMP
               WHERE id = ? AND run_id = ? AND plugin_submission_id = ?
                 AND materialization_state = 'running' AND EXISTS (
                   SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                     AND run.status = 'running' AND run.owner_token = ?)"#,
        ),
    };
    let changed = match (db.backend(), phase) {
        (DatabaseBackend::Sqlite(pool), ReplayPhase::Extraction) => sqlx::query(&sql)
            .bind(checkpoint_sha256)
            .bind(item.id)
            .bind(run_id)
            .bind(item.submission_id)
            .bind(checkpoint_sha256)
            .bind(checkpoint_sha256)
            .bind(run_id)
            .bind(owner_token)
            .execute(pool)
            .await?
            .rows_affected(),
        (DatabaseBackend::Postgres(pool), ReplayPhase::Extraction) => sqlx::query(&sql)
            .bind(checkpoint_sha256)
            .bind(item.id)
            .bind(run_id)
            .bind(item.submission_id)
            .bind(checkpoint_sha256)
            .bind(checkpoint_sha256)
            .bind(run_id)
            .bind(owner_token)
            .execute(pool)
            .await?
            .rows_affected(),
        (DatabaseBackend::Sqlite(pool), ReplayPhase::Materialization) => sqlx::query(&sql)
            .bind(listing_id)
            .bind(item.id)
            .bind(run_id)
            .bind(item.submission_id)
            .bind(run_id)
            .bind(owner_token)
            .execute(pool)
            .await?
            .rows_affected(),
        (DatabaseBackend::Postgres(pool), ReplayPhase::Materialization) => sqlx::query(&sql)
            .bind(listing_id)
            .bind(item.id)
            .bind(run_id)
            .bind(item.submission_id)
            .bind(run_id)
            .bind(owner_token)
            .execute(pool)
            .await?
            .rows_affected(),
    };
    require_owned_transition(changed)
}

async fn finish_rejected(
    db: &AppDb,
    run_id: i64,
    item: ClaimedItem,
    phase: ReplayPhase,
    owner_token: &str,
    stage: &str,
    reason_code: &str,
) -> ReplayRunResult<()> {
    validate_closed_rejection(stage, reason_code)?;
    let sql = match phase {
        ReplayPhase::Extraction => db.sql(
            r#"UPDATE listing_replay_run_items SET extraction_state = 'rejected',
                 materialization_state = 'blocked', extraction_completed_at = CURRENT_TIMESTAMP,
                 terminal_rejection_phase = 'extraction', terminal_rejection_stage = ?,
                 terminal_rejection_reason_code = ?, updated_at = CURRENT_TIMESTAMP
               WHERE id = ? AND run_id = ? AND extraction_state = 'running'
                 AND EXISTS (SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                   AND run.status = 'running' AND run.owner_token = ?)"#,
        ),
        ReplayPhase::Materialization => db.sql(
            r#"UPDATE listing_replay_run_items SET materialization_state = 'rejected',
                 materialization_completed_at = CURRENT_TIMESTAMP,
                 terminal_rejection_phase = 'materialization', terminal_rejection_stage = ?,
                 terminal_rejection_reason_code = ?, updated_at = CURRENT_TIMESTAMP
               WHERE id = ? AND run_id = ? AND materialization_state = 'running'
                 AND EXISTS (SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                   AND run.status = 'running' AND run.owner_token = ?)"#,
        ),
    };
    let changed = execute(
        db,
        &sql,
        &[
            Bind::Text(stage),
            Bind::Text(reason_code),
            Bind::I64(item.id),
            Bind::I64(run_id),
            Bind::I64(run_id),
            Bind::Text(owner_token),
        ],
    )
    .await?;
    require_owned_transition(changed)
}

async fn finish_capture_admission_error(
    db: &AppDb,
    run_id: i64,
    item: ClaimedItem,
    phase: ReplayPhase,
    owner_token: &str,
    error: &PluginStoreError,
) -> ReplayRunResult<()> {
    match error {
        PluginStoreError::Database(_) => {
            finish_failed(db, run_id, item, phase, owner_token, "database_error").await
        }
        PluginStoreError::Permission(_) => {
            finish_rejected(
                db,
                run_id,
                item,
                phase,
                owner_token,
                "capture_admission",
                "capture_authentication_failed",
            )
            .await
        }
        PluginStoreError::NotFound(_) => {
            finish_rejected(
                db,
                run_id,
                item,
                phase,
                owner_token,
                "capture_admission",
                "capture_not_found",
            )
            .await
        }
        PluginStoreError::Validation(_) => {
            finish_rejected(
                db,
                run_id,
                item,
                phase,
                owner_token,
                "capture_admission",
                "capture_validation_failed",
            )
            .await
        }
        PluginStoreError::AdmissionBlocked(reason) => {
            finish_failed(db, run_id, item, phase, owner_token, reason.code()).await
        }
        PluginStoreError::AircraftAdmission(_) => {
            finish_failed(db, run_id, item, phase, owner_token, "operation_failed").await
        }
    }
}

async fn finish_operation_error(
    db: &AppDb,
    run_id: i64,
    item: ClaimedItem,
    phase: ReplayPhase,
    owner_token: &str,
    error: &PluginStoreError,
) -> ReplayRunResult<()> {
    let reason_code = match error {
        PluginStoreError::Database(_) => "database_error",
        PluginStoreError::AdmissionBlocked(reason) => reason.code(),
        PluginStoreError::Validation(_)
        | PluginStoreError::Permission(_)
        | PluginStoreError::NotFound(_)
        | PluginStoreError::AircraftAdmission(_) => "operation_failed",
    };
    finish_failed(db, run_id, item, phase, owner_token, reason_code).await
}

async fn finish_failed(
    db: &AppDb,
    run_id: i64,
    item: ClaimedItem,
    phase: ReplayPhase,
    owner_token: &str,
    reason_code: &str,
) -> ReplayRunResult<()> {
    validate_closed_failure(reason_code)?;
    let sql = match phase {
        ReplayPhase::Extraction => db.sql(
            r#"UPDATE listing_replay_run_items SET extraction_state = 'failed',
                 materialization_state = 'blocked', extraction_completed_at = CURRENT_TIMESTAMP,
                 last_failure_phase = 'extraction', last_failure_reason_code = ?,
                 updated_at = CURRENT_TIMESTAMP
               WHERE id = ? AND run_id = ? AND extraction_state = 'running'
                 AND EXISTS (SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                   AND run.status = 'running' AND run.owner_token = ?)"#,
        ),
        ReplayPhase::Materialization => db.sql(
            r#"UPDATE listing_replay_run_items SET materialization_state = 'failed',
                 materialization_completed_at = CURRENT_TIMESTAMP,
                 last_failure_phase = 'materialization', last_failure_reason_code = ?,
                 updated_at = CURRENT_TIMESTAMP
               WHERE id = ? AND run_id = ? AND materialization_state = 'running'
                 AND EXISTS (SELECT 1 FROM listing_replay_runs run WHERE run.id = ?
                   AND run.status = 'running' AND run.owner_token = ?)"#,
        ),
    };
    let changed = execute(
        db,
        &sql,
        &[
            Bind::Text(reason_code),
            Bind::I64(item.id),
            Bind::I64(run_id),
            Bind::I64(run_id),
            Bind::Text(owner_token),
        ],
    )
    .await?;
    require_owned_transition(changed)
}

fn require_owned_transition(changed: u64) -> ReplayRunResult<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(ReplayRunError::Conflict(
            "replay ownership changed before the item transition committed".to_string(),
        ))
    }
}

async fn release_run(db: &AppDb, run_id: i64, owner_token: &str) -> ReplayRunResult<()> {
    let sql = db.sql(
        r#"UPDATE listing_replay_runs SET
             status = CASE WHEN NOT EXISTS (
               SELECT 1 FROM listing_replay_run_items item WHERE item.run_id = ?
                 AND item.materialization_state NOT IN ('succeeded', 'rejected')
                 AND item.extraction_state <> 'rejected'
             ) THEN 'completed' ELSE 'queued' END,
             active_phase = NULL, owner_token = NULL, heartbeat_at_epoch_seconds = NULL,
             completed_at = CASE WHEN NOT EXISTS (
               SELECT 1 FROM listing_replay_run_items item WHERE item.run_id = ?
                 AND item.materialization_state NOT IN ('succeeded', 'rejected')
                 AND item.extraction_state <> 'rejected'
             ) THEN CURRENT_TIMESTAMP ELSE NULL END,
             updated_at = CURRENT_TIMESTAMP
           WHERE id = ? AND status = 'running' AND owner_token = ?"#,
    );
    let changed = execute(
        db,
        &sql,
        &[
            Bind::I64(run_id),
            Bind::I64(run_id),
            Bind::I64(run_id),
            Bind::Text(owner_token),
        ],
    )
    .await?;
    require_owned_transition(changed)
}

async fn dry_run_report(
    db: &AppDb,
    run: Option<&ExistingRunRow>,
    request: &ReplayCapturesRequest<'_>,
    selected: &BTreeSet<i64>,
) -> ReplayRunResult<ReplayCapturesReport> {
    let mut counts = ReplayCapturesCounts {
        selected: selected.len(),
        ..Default::default()
    };
    for submission_id in selected {
        let owner = plugin_submission_owner(db, *submission_id)
            .await
            .map_err(|error| ReplayRunError::Validation(error.to_string()))?;
        let state = inspect_plugin_replay_capture_state(db, owner.id, *submission_id)
            .await
            .map_err(|error| ReplayRunError::Validation(error.to_string()))?;
        match request.phase {
            ReplayPhase::Extraction
                if state.checkpoint.is_some() || state.canonical_listing_id.is_some() =>
            {
                counts.already_complete += 1
            }
            ReplayPhase::Extraction => counts.ready += 1,
            ReplayPhase::Materialization if state.materialization_receipt_listing_id.is_some() => {
                counts.already_complete += 1
            }
            ReplayPhase::Materialization if state.checkpoint.is_some() => counts.ready += 1,
            ReplayPhase::Materialization => counts.blocked += 1,
        }
    }
    let gemini_usage = gemini_usage_for_phase(
        db,
        replay_usage_correlation(request.manifest, request.phase),
    )
    .await?;
    Ok(ReplayCapturesReport {
        dry_run: true,
        manifest_sha256: request.manifest.manifest_sha256.clone(),
        run_id: run.map(|run| run.id),
        phase: request.phase,
        gemini_usage,
        counts,
    })
}

async fn report_from_ledger(
    db: &AppDb,
    run_id: i64,
    request: &ReplayCapturesRequest<'_>,
    selected: &BTreeSet<i64>,
    gemini_usage: ReplayGeminiUsage,
) -> ReplayRunResult<ReplayCapturesReport> {
    let sql = db.sql(
        r#"SELECT plugin_submission_id, position, expected_rendered_html_sha256,
                  extraction_state, materialization_state
           FROM listing_replay_run_items WHERE run_id = ? ORDER BY position"#,
    );
    let rows: Vec<ItemRow> = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_as(&sql).bind(run_id).fetch_all(pool).await?,
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as(&sql).bind(run_id).fetch_all(pool).await?
        }
    };
    let mut counts = ReplayCapturesCounts {
        selected: selected.len(),
        ..Default::default()
    };
    for row in rows
        .into_iter()
        .filter(|row| selected.contains(&row.plugin_submission_id))
    {
        let state = PhaseState::parse(match request.phase {
            ReplayPhase::Extraction => &row.extraction_state,
            ReplayPhase::Materialization => &row.materialization_state,
        })?;
        match state {
            PhaseState::Succeeded => counts.succeeded += 1,
            PhaseState::Rejected => counts.rejected += 1,
            PhaseState::Failed => counts.failed += 1,
            PhaseState::Blocked => counts.blocked += 1,
            PhaseState::Queued | PhaseState::Running => counts.ready += 1,
        }
    }
    Ok(ReplayCapturesReport {
        dry_run: false,
        manifest_sha256: request.manifest.manifest_sha256.clone(),
        run_id: Some(run_id),
        phase: request.phase,
        gemini_usage,
        counts,
    })
}

fn replay_usage_correlation(manifest: &TrustedCaptureManifest, phase: ReplayPhase) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aircost:listing-replay-usage:v1\0");
    hasher.update(manifest.manifest_sha256.as_bytes());
    hasher.update(phase.label().as_bytes());
    format!("listing-replay:{:x}", hasher.finalize())
}

async fn gemini_usage_for_phase(
    db: &AppDb,
    correlation_id: String,
) -> ReplayRunResult<ReplayGeminiUsage> {
    let records = GeminiUsageStore::new(db)
        .for_correlation(&correlation_id)
        .await
        .map_err(|error| ReplayRunError::Database(error.to_string()))?;
    Ok(aggregate_gemini_usage(Some(correlation_id), &records))
}

fn aggregate_gemini_usage(
    correlation_id: Option<String>,
    records: &[GeminiUsageRecord],
) -> ReplayGeminiUsage {
    ReplayGeminiUsage {
        scope: "manifest_phase_cumulative",
        correlation_id,
        logical_requests: records.len(),
        transport_attempts: records.iter().fold(0, |sum, record| {
            sum.saturating_add(u64::from(record.attempt_count))
        }),
        retries: records.iter().fold(0, |sum, record| {
            sum.saturating_add(u64::from(record.retry_count))
        }),
        billable_usage_complete: records.iter().all(|record| {
            record.metrics.input_tokens.is_some()
                && record.metrics.output_tokens.is_some()
                && record.metrics.thought_tokens.is_some()
                && record.metrics.cached_tokens.is_some()
                && record.metrics.tool_tokens.is_some()
                && record.metrics.search_query_count.is_some()
                && record.cost.is_some()
        }),
        input_tokens: sum_optional_usage(records, |record| record.metrics.input_tokens),
        output_tokens: sum_optional_usage(records, |record| record.metrics.output_tokens),
        thought_tokens: sum_optional_usage(records, |record| record.metrics.thought_tokens),
        cached_tokens: sum_optional_usage(records, |record| record.metrics.cached_tokens),
        tool_tokens: sum_optional_usage(records, |record| record.metrics.tool_tokens),
        search_queries: sum_optional_usage(records, |record| record.metrics.search_query_count),
        estimated_cost_microusd: sum_optional_usage(records, |record| {
            record.cost.as_ref().map(|cost| cost.total_microusd)
        }),
    }
}

fn sum_optional_usage(
    records: &[GeminiUsageRecord],
    metric: impl Fn(&GeminiUsageRecord) -> Option<u64>,
) -> Option<u64> {
    records.iter().try_fold(0_u64, |sum, record| {
        metric(record).map(|value| sum.saturating_add(value))
    })
}

fn validate_closed_rejection(stage: &str, reason_code: &str) -> ReplayRunResult<()> {
    let valid = match stage {
        "capture_admission" => matches!(
            reason_code,
            "capture_authentication_failed" | "capture_not_found" | "capture_validation_failed"
        ),
        "faa_aircraft_admission" => matches!(
            reason_code,
            "missing_registration" | "non_n_registration" | "invalid_n_number" | "serial_conflict"
        ),
        _ => false,
    };
    if !valid {
        return Err(ReplayRunError::Validation(
            "replay rejection stage/reason pairing is outside the closed operational vocabulary"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_closed_failure(reason_code: &str) -> ReplayRunResult<()> {
    const CODES: &[&str] = &[
        "database_error",
        "operation_failed",
        "faa_lookup_failed",
        "faa_listing_not_found",
        "faa_registry_snapshot_unavailable",
        "faa_registration_not_found",
        "faa_registration_not_covered",
        "faa_ambiguous_registration",
        "faa_registry_aircraft_identity_unavailable",
        "faa_aircraft_manufacturer_mismatch",
        "faa_aircraft_model_mismatch",
        "faa_canonical_identity_assignment_missing",
        "faa_canonical_identity_assignment_mismatch",
    ];
    if !CODES.contains(&reason_code) {
        return Err(ReplayRunError::Validation(
            "replay failure is outside the closed operational vocabulary".to_string(),
        ));
    }
    Ok(())
}

fn new_owner_token(
    manifest: &TrustedCaptureManifest,
    phase: ReplayPhase,
) -> ReplayRunResult<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ReplayRunError::Database(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(b"aircost:listing-replay-owner:v1\0");
    hasher.update(manifest.manifest_sha256.as_bytes());
    hasher.update(phase.label().as_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(now.as_nanos().to_le_bytes());
    hasher.update(TOKEN_NONCE.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn epoch_seconds() -> ReplayRunResult<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| ReplayRunError::Database(error.to_string()))?
            .as_secs(),
    )
    .map_err(|_| ReplayRunError::Database("system clock overflow".to_string()))
}

enum Bind<'a> {
    I64(i64),
    Text(&'a str),
}

async fn execute(db: &AppDb, sql: &str, binds: &[Bind<'_>]) -> ReplayRunResult<u64> {
    macro_rules! bound_query {
        ($pool:expr) => {{
            let mut query = sqlx::query(sql);
            for bind in binds {
                query = match bind {
                    Bind::I64(value) => query.bind(*value),
                    Bind::Text(value) => query.bind(*value),
                };
            }
            query.execute($pool).await?.rows_affected()
        }};
    }
    Ok(match db.backend() {
        DatabaseBackend::Sqlite(pool) => bound_query!(pool),
        DatabaseBackend::Postgres(pool) => bound_query!(pool),
    })
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::Engine as _;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde_json::json;

    use super::*;
    use crate::listing::replay::build_trusted_capture_manifest;
    use crate::plugin::{sha256_hex, signature_message};

    async fn signed_checkpoint(db: &AppDb) -> (TrustedCaptureManifest, i64, crate::models::User) {
        let user = db.current_user(None).await.unwrap();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let keys = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, ?) RETURNING id",
        )
        .bind(user.id)
        .bind(BASE64_STANDARD.encode(keys.public_key().as_ref()))
        .fetch_one(pool)
        .await
        .unwrap();
        let source_url = "https://example.test/resumable-capture";
        let html = "<html><body>2020 Cessna 182T N182PF</body></html>";
        let rendered_sha = sha256_hex(html.as_bytes());
        let signature = BASE64_STANDARD.encode(
            keys.sign(
                &rng,
                signature_message(install_id, source_url, &rendered_sha).as_bytes(),
            )
            .unwrap()
            .as_ref(),
        );
        let extraction = json!({
            "manufacturer": "Cessna", "model": "182", "variant": "182T",
            "model_year": 2020, "asking_price_usd": 200000.0, "currency": "USD",
            "airframe_hours": 500.0, "engine_hours": null,
            "engine_time_basis": "unknown", "engine_time_evidence": null,
            "engine_time_confidence": null, "propeller_hours": null,
            "propeller_time_basis": "unknown", "propeller_time_evidence": null,
            "propeller_time_confidence": null, "installed_engine": null,
            "installed_propeller": null, "registration_number": "N182PF",
            "serial_number": "182TEST", "status": "active",
            "avionics": [], "valuation_facts": []
        })
        .to_string();
        let submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, submitted_at, rendered_html,
                 rendered_html_sha256, signature_base64, extracted_listing_json
               ) VALUES (?, ?, ?, '2026-08-19 12:00:00', ?, ?, ?, ?) RETURNING id"#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind(source_url)
        .bind(html)
        .bind(&rendered_sha)
        .bind(signature)
        .bind(extraction)
        .fetch_one(pool)
        .await
        .unwrap();
        let manifest = build_trusted_capture_manifest(db, &[submission_id])
            .await
            .unwrap();
        (manifest, submission_id, user)
    }

    #[test]
    fn rejection_vocabulary_is_closed() {
        assert!(
            validate_closed_rejection("faa_aircraft_admission", "missing_registration").is_ok()
        );
        assert!(validate_closed_rejection(
            "faa_aircraft_admission",
            "canonical_identity_assignment_missing"
        )
        .is_err());
        assert!(validate_closed_failure("faa_canonical_identity_assignment_missing").is_ok());
        assert!(validate_closed_rejection("gemini", "raw provider error").is_err());
    }

    #[test]
    fn phase_states_distinguish_retryable_and_terminal_results() {
        assert!(PhaseState::parse("failed").unwrap().is_terminal() == false);
        assert!(PhaseState::parse("rejected").unwrap().is_terminal());
        assert!(PhaseState::parse("succeeded").unwrap().is_terminal());
        assert!(PhaseState::parse("pending").is_err());
    }

    #[tokio::test]
    async fn committed_checkpoint_is_reconciled_without_a_provider_retry() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (manifest, submission_id, _) = signed_checkpoint(&db).await;
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let report = replay_captures(
            &db,
            Some(&extractor),
            &ReplayCapturesRequest {
                manifest: &manifest,
                phase: ReplayPhase::Extraction,
                submission_id: None,
                apply: true,
                recover_stale: false,
            },
        )
        .await
        .expect("the existing checkpoint must close the interrupted ledger transition");
        assert_eq!(report.gemini_usage.logical_requests, 0);
        assert_eq!(report.gemini_usage.transport_attempts, 0);
        assert_eq!(report.counts.succeeded, 1);
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let stored: (String, i64) = sqlx::query_as(
            "SELECT extraction_state, extraction_attempt_count FROM listing_replay_run_items WHERE plugin_submission_id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored, ("succeeded".to_string(), 1));
    }

    #[tokio::test]
    async fn provider_failure_is_retryable_and_a_resumed_domain_commit_succeeds() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (manifest, submission_id, _) = signed_checkpoint(&db).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let checkpoint: String = sqlx::query_scalar(
            "SELECT extracted_listing_json FROM plugin_submissions WHERE id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_submissions SET extracted_listing_json = NULL WHERE id = ?")
            .bind(submission_id)
            .execute(pool)
            .await
            .unwrap();
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9")
            .with_usage_store(GeminiUsageStore::new(&db));
        let failed = replay_captures(
            &db,
            Some(&extractor),
            &ReplayCapturesRequest {
                manifest: &manifest,
                phase: ReplayPhase::Extraction,
                submission_id: None,
                apply: true,
                recover_stale: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(failed.counts.failed, 1);
        assert_eq!(failed.counts.rejected, 0);
        assert_eq!(failed.gemini_usage.logical_requests, 1);
        let stored_failure: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT extraction_state, last_failure_reason_code, terminal_rejection_reason_code FROM listing_replay_run_items WHERE plugin_submission_id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            stored_failure,
            (
                "failed".to_string(),
                Some("operation_failed".to_string()),
                None
            )
        );

        sqlx::query("UPDATE plugin_submissions SET extracted_listing_json = ? WHERE id = ?")
            .bind(checkpoint)
            .bind(submission_id)
            .execute(pool)
            .await
            .unwrap();
        let resumed = replay_captures(
            &db,
            Some(&extractor),
            &ReplayCapturesRequest {
                manifest: &manifest,
                phase: ReplayPhase::Extraction,
                submission_id: None,
                apply: true,
                recover_stale: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(resumed.counts.succeeded, 1);
        assert_eq!(resumed.gemini_usage.logical_requests, 1);
        assert_eq!(
            resumed.gemini_usage.correlation_id,
            failed.gemini_usage.correlation_id
        );
        let stored_success: (String, i64, Option<String>) = sqlx::query_as(
            "SELECT extraction_state, extraction_attempt_count, last_failure_reason_code FROM listing_replay_run_items WHERE plugin_submission_id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored_success, ("succeeded".to_string(), 2, None));
    }

    #[tokio::test]
    async fn dry_run_is_provider_free_and_does_not_create_a_ledger() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (manifest, _, _) = signed_checkpoint(&db).await;
        let report = replay_captures(
            &db,
            None,
            &ReplayCapturesRequest {
                manifest: &manifest,
                phase: ReplayPhase::Materialization,
                submission_id: None,
                apply: false,
                recover_stale: false,
            },
        )
        .await
        .unwrap();
        assert!(report.dry_run);
        assert_eq!(report.gemini_usage.logical_requests, 0);
        assert_eq!(report.gemini_usage.estimated_cost_microusd, Some(0));
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM listing_replay_runs")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(runs, 0);
    }

    #[tokio::test]
    async fn materialization_records_missing_owner_and_continues_the_batch() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (mut manifest, first_submission_id, user) = signed_checkpoint(&db).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("UPDATE plugin_submissions SET user_id = 999999 WHERE id = ?")
            .bind(first_submission_id)
            .execute(&mut *connection)
            .await
            .unwrap();
        let second_submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, submitted_at, rendered_html,
                 rendered_html_sha256, signature_base64, extracted_listing_json
               ) SELECT ?, plugin_install_id, source_url, submitted_at, rendered_html,
                        rendered_html_sha256, signature_base64, extracted_listing_json
                 FROM plugin_submissions WHERE id = ? RETURNING id"#,
        )
        .bind(user.id)
        .bind(first_submission_id)
        .fetch_one(&mut *connection)
        .await
        .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await
            .unwrap();
        drop(connection);
        let mut second_entry = manifest.captures[0].clone();
        second_entry.submission_id = second_submission_id;
        manifest.captures.push(second_entry);
        manifest.manifest_sha256 = super::super::manifest_fingerprint(&manifest.captures).unwrap();

        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let report = replay_captures(
            &db,
            Some(&extractor),
            &ReplayCapturesRequest {
                manifest: &manifest,
                phase: ReplayPhase::Materialization,
                submission_id: None,
                apply: true,
                recover_stale: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(report.counts.selected, 2);
        assert_eq!(report.counts.blocked, 1);
        assert_eq!(report.counts.rejected, 0);
        assert_eq!(report.counts.failed, 1);
        let first: (String, String, String) = sqlx::query_as(
            "SELECT extraction_state, terminal_rejection_stage, terminal_rejection_reason_code FROM listing_replay_run_items WHERE plugin_submission_id = ?",
        )
        .bind(first_submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            first,
            (
                "rejected".to_string(),
                "capture_admission".to_string(),
                "capture_not_found".to_string()
            )
        );
        let second_attempts: i64 = sqlx::query_scalar(
            "SELECT materialization_attempt_count FROM listing_replay_run_items WHERE plugin_submission_id = ?",
        )
        .bind(second_submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(second_attempts, 1);
    }

    #[tokio::test]
    async fn committed_completion_receipt_closes_both_phases_without_a_provider_retry() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (manifest, submission_id, user) = signed_checkpoint(&db).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let variant_id: i64 =
            sqlx::query_scalar("SELECT id FROM aircraft_model_variants ORDER BY id LIMIT 1")
                .fetch_one(pool)
                .await
                .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO aircraft_sale_listings (
                 aircraft_model_variant_id, created_by_user_id, source_url, model_year,
                 asking_price_usd, airframe_hours
               ) VALUES (?, ?, 'https://example.test/resumable-capture', 2020, 200000, 500)
               RETURNING id"#,
        )
        .bind(variant_id)
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_submissions SET canonical_listing_id = ? WHERE id = ?")
            .bind(listing_id)
            .bind(submission_id)
            .execute(pool)
            .await
            .unwrap();
        let state = inspect_plugin_replay_capture_state(&db, user.id, submission_id)
            .await
            .unwrap();
        let checkpoint = state.checkpoint.unwrap();
        sqlx::query(
            r#"INSERT INTO plugin_submission_materialization_receipts (
                 plugin_submission_id, aircraft_sale_listing_id,
                 rendered_html_sha256, extracted_listing_sha256
               ) VALUES (?, ?, ?, ?)"#,
        )
        .bind(submission_id)
        .bind(listing_id)
        .bind(checkpoint.rendered_html_sha256)
        .bind(checkpoint.extracted_listing_sha256)
        .execute(pool)
        .await
        .unwrap();
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let report = replay_captures(
            &db,
            Some(&extractor),
            &ReplayCapturesRequest {
                manifest: &manifest,
                phase: ReplayPhase::Materialization,
                submission_id: None,
                apply: true,
                recover_stale: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(report.gemini_usage.logical_requests, 0);
        assert_eq!(report.gemini_usage.transport_attempts, 0);
        assert_eq!(report.counts.succeeded, 1);
        let stored: (String, String, Option<i64>, i64) = sqlx::query_as(
            "SELECT extraction_state, materialization_state, resulting_listing_id, materialization_attempt_count FROM listing_replay_run_items WHERE plugin_submission_id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            stored,
            (
                "succeeded".to_string(),
                "succeeded".to_string(),
                Some(listing_id),
                0
            )
        );
    }

    #[tokio::test]
    async fn binding_without_completion_receipt_remains_ready_for_recovery() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (manifest, submission_id, user) = signed_checkpoint(&db).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let variant_id: i64 =
            sqlx::query_scalar("SELECT id FROM aircraft_model_variants ORDER BY id LIMIT 1")
                .fetch_one(pool)
                .await
                .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO aircraft_sale_listings (
                 aircraft_model_variant_id, created_by_user_id, source_url, model_year,
                 asking_price_usd, airframe_hours
               ) VALUES (?, ?, 'https://example.test/resumable-capture', 2020, 200000, 500)
               RETURNING id"#,
        )
        .bind(variant_id)
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_submissions SET canonical_listing_id = ? WHERE id = ?")
            .bind(listing_id)
            .bind(submission_id)
            .execute(pool)
            .await
            .unwrap();

        let report = replay_captures(
            &db,
            None,
            &ReplayCapturesRequest {
                manifest: &manifest,
                phase: ReplayPhase::Materialization,
                submission_id: None,
                apply: false,
                recover_stale: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(report.counts.already_complete, 0);
        assert_eq!(report.counts.ready, 1);
    }

    #[tokio::test]
    async fn stored_run_membership_cannot_drift_from_the_manifest() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (manifest, submission_id, _) = signed_checkpoint(&db).await;
        let run = ensure_run(&db, &manifest).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE listing_replay_run_items SET expected_rendered_html_sha256 = ? WHERE plugin_submission_id = ?",
        )
        .bind("f".repeat(64))
        .bind(submission_id)
        .execute(pool)
        .await
        .unwrap();
        assert!(matches!(
            validate_run_membership(&db, &run, &manifest).await,
            Err(ReplayRunError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn stale_recovery_retries_and_fences_the_old_owner() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (manifest, submission_id, _) = signed_checkpoint(&db).await;
        let run = ensure_run(&db, &manifest).await.unwrap();
        acquire_run(&db, run.id, ReplayPhase::Extraction, "owner-one", false)
            .await
            .unwrap();
        let first = claim_item(
            &db,
            run.id,
            submission_id,
            ReplayPhase::Extraction,
            "owner-one",
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            acquire_run(&db, run.id, ReplayPhase::Extraction, "owner-two", false).await,
            Err(ReplayRunError::Conflict(_))
        ));
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("UPDATE listing_replay_runs SET heartbeat_at_epoch_seconds = ? WHERE id = ?")
            .bind(epoch_seconds().unwrap() - STALE_RECOVERY_THRESHOLD.as_secs() as i64 - 1)
            .bind(run.id)
            .execute(pool)
            .await
            .unwrap();
        acquire_run(&db, run.id, ReplayPhase::Extraction, "owner-two", true)
            .await
            .unwrap();
        assert!(matches!(
            finish_succeeded(
                &db,
                run.id,
                first,
                ReplayPhase::Extraction,
                "owner-one",
                None,
                None
            )
            .await,
            Err(ReplayRunError::Conflict(_))
        ));
        let second = claim_item(
            &db,
            run.id,
            submission_id,
            ReplayPhase::Extraction,
            "owner-two",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(second.submission_id, submission_id);
        let attempts: i64 = sqlx::query_scalar(
            "SELECT extraction_attempt_count FROM listing_replay_run_items WHERE id = ?",
        )
        .bind(second.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(attempts, 2);
    }

    #[tokio::test]
    async fn heartbeat_ownership_loss_promptly_drops_the_inflight_operation() {
        struct PendingOperation(std::sync::Arc<std::sync::atomic::AtomicBool>);

        impl std::future::Future for PendingOperation {
            type Output = ();

            fn poll(
                self: std::pin::Pin<&mut Self>,
                _context: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                std::task::Poll::Pending
            }
        }

        impl Drop for PendingOperation {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (manifest, _, _) = signed_checkpoint(&db).await;
        let run = ensure_run(&db, &manifest).await.unwrap();
        acquire_run(&db, run.id, ReplayPhase::Extraction, "owner-one", false)
            .await
            .unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("UPDATE listing_replay_runs SET owner_token = 'owner-two' WHERE id = ?")
            .bind(run.id)
            .execute(pool)
            .await
            .unwrap();
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let result = tokio::time::timeout(
            Duration::from_millis(200),
            with_heartbeat_interval(
                &db,
                run.id,
                "owner-one",
                Duration::from_millis(5),
                PendingOperation(dropped.clone()),
            ),
        )
        .await
        .expect("ownership loss must cancel before the timeout");
        assert!(matches!(result, Err(ReplayRunError::Conflict(_))));
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_live_run_excludes_a_competing_manifest() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        for fingerprint in ["a".repeat(64), "b".repeat(64)] {
            sqlx::query(
                "INSERT INTO listing_replay_runs (manifest_version, manifest_sha256, manifest_capture_count) VALUES (1, ?, 1)",
            )
            .bind(fingerprint)
            .execute(pool)
            .await
            .unwrap();
        }
        acquire_run(&db, 1, ReplayPhase::Extraction, "owner-one", false)
            .await
            .unwrap();
        assert!(matches!(
            acquire_run(&db, 2, ReplayPhase::Extraction, "owner-two", false).await,
            Err(ReplayRunError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn faa_setup_gap_is_retryable_without_raw_reason_text() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (manifest, submission_id, _) = signed_checkpoint(&db).await;
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let report = replay_captures(
            &db,
            Some(&extractor),
            &ReplayCapturesRequest {
                manifest: &manifest,
                phase: ReplayPhase::Materialization,
                submission_id: None,
                apply: true,
                recover_stale: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(report.counts.failed, 1);
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let stored: (String, Option<String>, String) = sqlx::query_as(
            "SELECT materialization_state, terminal_rejection_reason_code, last_failure_reason_code FROM listing_replay_run_items WHERE plugin_submission_id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            stored,
            (
                "failed".to_string(),
                None,
                "faa_registry_snapshot_unavailable".to_string()
            )
        );
    }
}
