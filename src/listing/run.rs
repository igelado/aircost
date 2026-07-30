//! Durable operational coordination for automatic listing verification.
//!
//! Runs contain only scheduling state and sanitized verifier outcomes. Gemini
//! prompts, evidence dossiers, source documents, and provider responses never
//! belong in this store.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::listing::verification::ListingVerificationOutcome;

pub const MAX_VERIFICATION_RUN_ITEMS: usize = 1_000;
pub const MAX_VERIFICATION_RUN_ITEM_PAGE_SIZE: i64 = 100;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationRunStatus {
    Queued,
    Running,
    Cancelling,
    Completed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationRunItemStatus {
    Queued,
    Running,
    Verified,
    PendingReview,
    PendingReference,
    Blocked,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateVerificationRunRequest {
    pub owner_user_id: i64,
    pub idempotency_key: String,
    /// Listing order is significant and becomes the durable claim order.
    pub listing_ids: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VerificationRun {
    pub id: i64,
    pub owner_user_id: i64,
    pub status: VerificationRunStatus,
    pub request_fingerprint: String,
    pub total_items: i64,
    pub queued_items: i64,
    pub running_items: i64,
    pub verified_items: i64,
    pub pending_review_items: i64,
    pub pending_reference_items: i64,
    pub blocked_items: i64,
    pub failed_items: i64,
    pub cancelled_items: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_listing_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CreateVerificationRunResult {
    pub run: VerificationRun,
    /// False means the same owner/idempotency key and ordered request already
    /// existed and was returned without inserting another run.
    pub created: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VerificationRunItemsQuery {
    pub limit: Option<i64>,
    pub after_item_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VerificationRunItem {
    pub id: i64,
    pub run_id: i64,
    pub listing_id: i64,
    pub position: i64,
    pub status: VerificationRunItemStatus,
    pub attempt_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VerificationRunItemPage {
    pub items: Vec<VerificationRunItem>,
    pub checkpoint: VerificationRunItemCheckpoint,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VerificationRunItemCheckpoint {
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_after_item_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ClaimedVerificationRunItem {
    pub item_id: i64,
    pub run_id: i64,
    pub owner_user_id: i64,
    pub listing_id: i64,
    pub position: i64,
    pub attempt_count: i64,
    pub lease_expires_at_epoch_seconds: i64,
}

#[derive(Debug)]
pub enum VerificationRunError {
    Validation(String),
    NotFound(String),
    Conflict(String),
    IdempotencyConflict { run_id: i64 },
    ActiveListingConflict { run_id: i64, listing_id: i64 },
    LeaseConflict { item_id: i64 },
    Database(String),
}

impl fmt::Display for VerificationRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Database(message) => formatter.write_str(message),
            Self::IdempotencyConflict { run_id } => write!(
                formatter,
                "idempotency key already belongs to verification run {run_id} with a different request"
            ),
            Self::ActiveListingConflict { run_id, listing_id } => write!(
                formatter,
                "listing {listing_id} is already active in verification run {run_id}"
            ),
            Self::LeaseConflict { item_id } => write!(
                formatter,
                "verification run item {item_id} is no longer owned by the supplied lease"
            ),
        }
    }
}

impl std::error::Error for VerificationRunError {}

impl From<sqlx::Error> for VerificationRunError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

pub type VerificationRunResult<T> = Result<T, VerificationRunError>;

#[derive(Debug, FromRow)]
struct ExistingRunRow {
    id: i64,
    request_fingerprint: String,
}

#[derive(Debug, FromRow)]
struct ActiveListingRow {
    run_id: i64,
    listing_id: i64,
}

#[derive(Debug, FromRow)]
struct RunViewRow {
    id: i64,
    owner_user_id: i64,
    status: String,
    request_fingerprint: String,
    total_items: i64,
    queued_items: i64,
    running_items: i64,
    verified_items: i64,
    pending_review_items: i64,
    pending_reference_items: i64,
    blocked_items: i64,
    failed_items: i64,
    cancelled_items: i64,
    current_listing_id: Option<i64>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

#[derive(Debug, FromRow)]
struct RunItemRow {
    id: i64,
    run_id: i64,
    listing_id: i64,
    position: i64,
    status: String,
    attempt_count: i64,
    outcome_json: Option<String>,
    reason_code: Option<String>,
    reason: Option<String>,
    created_at: String,
    updated_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
}

#[derive(Debug, FromRow)]
struct ClaimRow {
    item_id: i64,
    run_id: i64,
    owner_user_id: i64,
    listing_id: i64,
    position: i64,
    attempt_count: i64,
    lease_expires_at_epoch_seconds: i64,
}

fn parse_run_status(value: &str) -> VerificationRunResult<VerificationRunStatus> {
    match value {
        "queued" => Ok(VerificationRunStatus::Queued),
        "running" => Ok(VerificationRunStatus::Running),
        "cancelling" => Ok(VerificationRunStatus::Cancelling),
        "completed" => Ok(VerificationRunStatus::Completed),
        "cancelled" => Ok(VerificationRunStatus::Cancelled),
        other => Err(VerificationRunError::Database(format!(
            "stored verification run has invalid status {other:?}"
        ))),
    }
}

fn parse_item_status(value: &str) -> VerificationRunResult<VerificationRunItemStatus> {
    match value {
        "queued" => Ok(VerificationRunItemStatus::Queued),
        "running" => Ok(VerificationRunItemStatus::Running),
        "verified" => Ok(VerificationRunItemStatus::Verified),
        "pending_review" => Ok(VerificationRunItemStatus::PendingReview),
        "pending_reference" => Ok(VerificationRunItemStatus::PendingReference),
        "blocked" => Ok(VerificationRunItemStatus::Blocked),
        "failed" => Ok(VerificationRunItemStatus::Failed),
        "cancelled" => Ok(VerificationRunItemStatus::Cancelled),
        other => Err(VerificationRunError::Database(format!(
            "stored verification run item has invalid status {other:?}"
        ))),
    }
}

impl RunViewRow {
    fn project(self) -> VerificationRunResult<VerificationRun> {
        Ok(VerificationRun {
            id: self.id,
            owner_user_id: self.owner_user_id,
            status: parse_run_status(&self.status)?,
            request_fingerprint: self.request_fingerprint,
            total_items: self.total_items,
            queued_items: self.queued_items,
            running_items: self.running_items,
            verified_items: self.verified_items,
            pending_review_items: self.pending_review_items,
            pending_reference_items: self.pending_reference_items,
            blocked_items: self.blocked_items,
            failed_items: self.failed_items,
            cancelled_items: self.cancelled_items,
            current_listing_id: self.current_listing_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
            completed_at: self.completed_at,
        })
    }
}

impl RunItemRow {
    fn project(self) -> VerificationRunResult<VerificationRunItem> {
        let outcome = self
            .outcome_json
            .map(|json| {
                serde_json::from_str::<Value>(&json).map_err(|error| {
                    VerificationRunError::Database(format!(
                        "stored verification outcome is invalid JSON: {error}"
                    ))
                })
            })
            .transpose()?;
        if outcome.as_ref().is_some_and(|value| !value.is_object()) {
            return Err(VerificationRunError::Database(
                "stored verification outcome must be a JSON object".to_string(),
            ));
        }
        Ok(VerificationRunItem {
            id: self.id,
            run_id: self.run_id,
            listing_id: self.listing_id,
            position: self.position,
            status: parse_item_status(&self.status)?,
            attempt_count: self.attempt_count,
            outcome,
            reason_code: self.reason_code,
            reason: self.reason,
            created_at: self.created_at,
            updated_at: self.updated_at,
            started_at: self.started_at,
            completed_at: self.completed_at,
        })
    }
}

fn validate_create_request(request: &CreateVerificationRunRequest) -> VerificationRunResult<()> {
    if request.owner_user_id <= 0 {
        return Err(VerificationRunError::Validation(
            "owner_user_id must be positive".to_string(),
        ));
    }
    let idempotency_key = request.idempotency_key.trim();
    if idempotency_key.is_empty() || idempotency_key.len() > 200 {
        return Err(VerificationRunError::Validation(
            "idempotency_key must contain between 1 and 200 characters".to_string(),
        ));
    }
    if request.listing_ids.is_empty() || request.listing_ids.len() > MAX_VERIFICATION_RUN_ITEMS {
        return Err(VerificationRunError::Validation(format!(
            "listing_ids must contain between 1 and {MAX_VERIFICATION_RUN_ITEMS} items"
        )));
    }
    let mut unique = std::collections::HashSet::new();
    for listing_id in &request.listing_ids {
        if *listing_id <= 0 {
            return Err(VerificationRunError::Validation(
                "listing_ids must contain positive integers".to_string(),
            ));
        }
        if !unique.insert(*listing_id) {
            return Err(VerificationRunError::Validation(format!(
                "listing_id {listing_id} appears more than once"
            )));
        }
    }
    Ok(())
}

fn request_fingerprint(request: &CreateVerificationRunRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aircost:listing-verification-run-request:v1\0");
    hasher.update(request.owner_user_id.to_le_bytes());
    hasher.update((request.listing_ids.len() as u64).to_le_bytes());
    for listing_id in &request.listing_ids {
        hasher.update(listing_id.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub async fn create_verification_run(
    db: &AppDb,
    request: &CreateVerificationRunRequest,
) -> VerificationRunResult<CreateVerificationRunResult> {
    validate_create_request(request)?;
    let fingerprint = request_fingerprint(request);
    let insert_run = db.sql(
        r#"
        INSERT INTO listing_verification_runs (
          owner_user_id, idempotency_key, request_fingerprint
        ) VALUES (?, ?, ?)
        ON CONFLICT (owner_user_id, idempotency_key) DO NOTHING
        RETURNING id
        "#,
    );
    let select_existing = db.sql(
        r#"
        SELECT id, request_fingerprint
        FROM listing_verification_runs
        WHERE owner_user_id = ? AND idempotency_key = ?
        "#,
    );
    let select_owner = db.sql("SELECT created_by_user_id FROM aircraft_sale_listings WHERE id = ?");
    let select_active = db.sql(
        r#"
        SELECT run_id, listing_id
        FROM listing_verification_run_items
        WHERE listing_id = ? AND status IN ('queued', 'running')
        LIMIT 1
        "#,
    );
    let insert_item = db.sql(
        r#"
        INSERT INTO listing_verification_run_items (
          run_id, listing_id, position
        ) VALUES (?, ?, ?)
        "#,
    );

    macro_rules! create_in_transaction {
        ($pool:expr, $postgres:expr) => {{
            let mut transaction = $pool.begin().await?;
            let inserted_id = sqlx::query_scalar::<_, i64>(&insert_run)
                .bind(request.owner_user_id)
                .bind(request.idempotency_key.trim())
                .bind(&fingerprint)
                .fetch_optional(&mut *transaction)
                .await?;
            if let Some(run_id) = inserted_id {
                if $postgres {
                    sqlx::query(
                        "LOCK TABLE listing_verification_run_items IN SHARE ROW EXCLUSIVE MODE",
                    )
                    .execute(&mut *transaction)
                    .await?;
                }
                for (position, listing_id) in request.listing_ids.iter().enumerate() {
                    let owner = sqlx::query_scalar::<_, i64>(&select_owner)
                        .bind(listing_id)
                        .fetch_optional(&mut *transaction)
                        .await?;
                    if owner != Some(request.owner_user_id) {
                        return Err(VerificationRunError::NotFound(
                            "one or more listings were not found for the current owner".to_string(),
                        ));
                    }
                    if let Some(active) = sqlx::query_as::<_, ActiveListingRow>(&select_active)
                        .bind(listing_id)
                        .fetch_optional(&mut *transaction)
                        .await?
                    {
                        return Err(VerificationRunError::ActiveListingConflict {
                            run_id: active.run_id,
                            listing_id: active.listing_id,
                        });
                    }
                    sqlx::query(&insert_item)
                        .bind(run_id)
                        .bind(listing_id)
                        .bind(position as i64)
                        .execute(&mut *transaction)
                        .await?;
                }
                transaction.commit().await?;
                (run_id, true)
            } else {
                let existing = sqlx::query_as::<_, ExistingRunRow>(&select_existing)
                    .bind(request.owner_user_id)
                    .bind(request.idempotency_key.trim())
                    .fetch_one(&mut *transaction)
                    .await?;
                if existing.request_fingerprint != fingerprint {
                    return Err(VerificationRunError::IdempotencyConflict {
                        run_id: existing.id,
                    });
                }
                transaction.commit().await?;
                (existing.id, false)
            }
        }};
    }

    let (run_id, created) = match db.backend() {
        DatabaseBackend::Sqlite(pool) => create_in_transaction!(pool, false),
        DatabaseBackend::Postgres(pool) => create_in_transaction!(pool, true),
    };
    Ok(CreateVerificationRunResult {
        run: get_verification_run(db, request.owner_user_id, run_id).await?,
        created,
    })
}

pub async fn get_verification_run(
    db: &AppDb,
    owner_user_id: i64,
    run_id: i64,
) -> VerificationRunResult<VerificationRun> {
    if owner_user_id <= 0 || run_id <= 0 {
        return Err(VerificationRunError::NotFound(
            "verification run was not found".to_string(),
        ));
    }
    let sql = db.sql(
        r#"
        SELECT
          run.id,
          run.owner_user_id,
          run.status,
          run.request_fingerprint,
          COUNT(item.id) AS total_items,
          COALESCE(SUM(CASE WHEN item.status = 'queued' THEN 1 ELSE 0 END), 0)
            AS queued_items,
          COALESCE(SUM(CASE WHEN item.status = 'running' THEN 1 ELSE 0 END), 0)
            AS running_items,
          COALESCE(SUM(CASE WHEN item.status = 'verified' THEN 1 ELSE 0 END), 0)
            AS verified_items,
          COALESCE(SUM(CASE WHEN item.status = 'pending_review' THEN 1 ELSE 0 END), 0)
            AS pending_review_items,
          COALESCE(SUM(CASE WHEN item.status = 'pending_reference' THEN 1 ELSE 0 END), 0)
            AS pending_reference_items,
          COALESCE(SUM(CASE WHEN item.status = 'blocked' THEN 1 ELSE 0 END), 0)
            AS blocked_items,
          COALESCE(SUM(CASE WHEN item.status = 'failed' THEN 1 ELSE 0 END), 0)
            AS failed_items,
          COALESCE(SUM(CASE WHEN item.status = 'cancelled' THEN 1 ELSE 0 END), 0)
            AS cancelled_items,
          MIN(CASE WHEN item.status = 'running' THEN item.listing_id END)
            AS current_listing_id,
          run.created_at,
          run.updated_at,
          run.completed_at
        FROM listing_verification_runs run
        LEFT JOIN listing_verification_run_items item ON item.run_id = run.id
        WHERE run.id = ? AND run.owner_user_id = ?
        GROUP BY
          run.id, run.owner_user_id, run.status, run.request_fingerprint,
          run.created_at, run.updated_at, run.completed_at
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, RunViewRow>(&sql)
                .bind(run_id)
                .bind(owner_user_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, RunViewRow>(&sql)
                .bind(run_id)
                .bind(owner_user_id)
                .fetch_optional(pool)
                .await?
        }
    }
    .ok_or_else(|| VerificationRunError::NotFound("verification run was not found".to_string()))?;
    row.project()
}

pub async fn list_verification_run_items(
    db: &AppDb,
    owner_user_id: i64,
    run_id: i64,
    query: &VerificationRunItemsQuery,
) -> VerificationRunResult<VerificationRunItemPage> {
    let limit = query.limit.unwrap_or(25);
    if !(1..=MAX_VERIFICATION_RUN_ITEM_PAGE_SIZE).contains(&limit) {
        return Err(VerificationRunError::Validation(format!(
            "limit must be between 1 and {MAX_VERIFICATION_RUN_ITEM_PAGE_SIZE}"
        )));
    }
    if query.after_item_id.is_some_and(|id| id <= 0) {
        return Err(VerificationRunError::Validation(
            "after_item_id must be positive".to_string(),
        ));
    }
    get_verification_run(db, owner_user_id, run_id).await?;
    let sql = db.sql(
        r#"
        SELECT
          item.id, item.run_id, item.listing_id, item.position, item.status,
          item.attempt_count, item.outcome_json, item.reason_code, item.reason,
          item.created_at, item.updated_at, item.started_at, item.completed_at
        FROM listing_verification_run_items item
        JOIN listing_verification_runs run ON run.id = item.run_id
        WHERE item.run_id = ?
          AND run.owner_user_id = ?
          AND item.id > ?
        ORDER BY item.id
        LIMIT ?
        "#,
    );
    let fetch_limit = limit + 1;
    let mut rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, RunItemRow>(&sql)
                .bind(run_id)
                .bind(owner_user_id)
                .bind(query.after_item_id.unwrap_or(0))
                .bind(fetch_limit)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, RunItemRow>(&sql)
                .bind(run_id)
                .bind(owner_user_id)
                .bind(query.after_item_id.unwrap_or(0))
                .bind(fetch_limit)
                .fetch_all(pool)
                .await?
        }
    };
    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }
    let items = rows
        .into_iter()
        .map(RunItemRow::project)
        .collect::<VerificationRunResult<Vec<_>>>()?;
    let resume_after_item_id = items.last().map(|item| item.id).or(query.after_item_id);
    Ok(VerificationRunItemPage {
        items,
        checkpoint: VerificationRunItemCheckpoint {
            has_more,
            resume_after_item_id,
        },
    })
}

pub async fn claim_next_verification_run_item(
    db: &AppDb,
    lease_token: &str,
    lease_duration: Duration,
) -> VerificationRunResult<Option<ClaimedVerificationRunItem>> {
    validate_lease(lease_token, lease_duration)?;
    let now = epoch_seconds()?;
    let expires = now
        .checked_add(lease_duration.as_secs() as i64)
        .ok_or_else(|| VerificationRunError::Validation("lease duration is too large".into()))?;
    let reclaim_cancelled = reclaim_cancelled_sql(db);
    let reclaim_active = reclaim_active_sql(db);
    let refresh_runs = refresh_run_status_sql(db);
    let claim = db.sql(
        r#"
        UPDATE listing_verification_run_items
        SET status = 'running',
            attempt_count = attempt_count + 1,
            lease_token = ?,
            lease_expires_at_epoch_seconds = ?,
            outcome_json = NULL,
            reason_code = NULL,
            reason = NULL,
            started_at = CURRENT_TIMESTAMP,
            completed_at = NULL,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = (
          SELECT candidate.id
          FROM listing_verification_run_items candidate
          JOIN listing_verification_runs run ON run.id = candidate.run_id
          WHERE candidate.status = 'queued'
            AND run.status IN ('queued', 'running')
            AND NOT EXISTS (
              SELECT 1
              FROM listing_verification_run_items active
              WHERE active.run_id = candidate.run_id
                AND active.status = 'running'
            )
          ORDER BY run.id, candidate.position, candidate.id
          LIMIT 1
        )
          AND status = 'queued'
        RETURNING id
        "#,
    );
    let select_claim = db.sql(
        r#"
        SELECT item.id AS item_id, item.run_id, run.owner_user_id,
          item.listing_id, item.position, item.attempt_count,
          item.lease_expires_at_epoch_seconds
        FROM listing_verification_run_items item
        JOIN listing_verification_runs run ON run.id = item.run_id
        WHERE item.id = ?
        "#,
    );
    let mark_run = db.sql(
        r#"
        UPDATE listing_verification_runs
        SET status = 'running', completed_at = NULL, updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND status IN ('queued', 'running')
        "#,
    );
    macro_rules! claim_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&reclaim_cancelled)
                .bind(now)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&reclaim_active)
                .bind(now)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&refresh_runs)
                .execute(&mut *transaction)
                .await?;
            let item_id = sqlx::query_scalar::<_, i64>(&claim)
                .bind(lease_token.trim())
                .bind(expires)
                .fetch_optional(&mut *transaction)
                .await?;
            let row = if let Some(item_id) = item_id {
                let row = sqlx::query_as::<_, ClaimRow>(&select_claim)
                    .bind(item_id)
                    .fetch_one(&mut *transaction)
                    .await?;
                sqlx::query(&mark_run)
                    .bind(row.run_id)
                    .execute(&mut *transaction)
                    .await?;
                Some(row)
            } else {
                None
            };
            transaction.commit().await?;
            row
        }};
    }
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => claim_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => claim_in_transaction!(pool),
    };
    Ok(row.map(|row| ClaimedVerificationRunItem {
        item_id: row.item_id,
        run_id: row.run_id,
        owner_user_id: row.owner_user_id,
        listing_id: row.listing_id,
        position: row.position,
        attempt_count: row.attempt_count,
        lease_expires_at_epoch_seconds: row.lease_expires_at_epoch_seconds,
    }))
}

pub async fn complete_verification_run_item(
    db: &AppDb,
    item_id: i64,
    lease_token: &str,
    outcome: &ListingVerificationOutcome,
) -> VerificationRunResult<VerificationRunItem> {
    if item_id <= 0 {
        return Err(VerificationRunError::NotFound(
            "verification run item was not found".to_string(),
        ));
    }
    validate_token(lease_token)?;
    if !matches!(
        outcome.status.as_str(),
        "verified"
            | "already_verified"
            | "pending_review"
            | "pending_reference"
            | "blocked"
            | "stale"
    ) {
        return Err(VerificationRunError::Validation(
            "failed or unknown verification outcomes must use the sanitized failure boundary"
                .to_string(),
        ));
    }
    let (status, reason_code, reason) = terminal_outcome(outcome);
    let outcome_json = serde_json::to_string(outcome).map_err(|error| {
        VerificationRunError::Validation(format!(
            "verification outcome could not be serialized: {error}"
        ))
    })?;
    if outcome_json.len() > 65_536 {
        return Err(VerificationRunError::Validation(
            "verification outcome exceeds the 65536-byte operational result limit".to_string(),
        ));
    }
    finish_item(
        db,
        item_id,
        lease_token,
        status,
        Some(&outcome_json),
        reason_code.as_deref(),
        reason.as_deref(),
    )
    .await
}

pub async fn fail_verification_run_item(
    db: &AppDb,
    item_id: i64,
    lease_token: &str,
    reason_code: &str,
    reason: &str,
) -> VerificationRunResult<VerificationRunItem> {
    if item_id <= 0 {
        return Err(VerificationRunError::NotFound(
            "verification run item was not found".to_string(),
        ));
    }
    validate_token(lease_token)?;
    validate_reason(reason_code, reason)?;
    finish_item(
        db,
        item_id,
        lease_token,
        VerificationRunItemStatus::Failed,
        None,
        Some(reason_code.trim()),
        Some(reason.trim()),
    )
    .await
}

pub async fn reclaim_expired_verification_run_leases(db: &AppDb) -> VerificationRunResult<u64> {
    reclaim_expired_verification_run_leases_at(db, epoch_seconds()?).await
}

pub async fn cancel_verification_run(
    db: &AppDb,
    owner_user_id: i64,
    run_id: i64,
) -> VerificationRunResult<VerificationRun> {
    if owner_user_id <= 0 || run_id <= 0 {
        return Err(VerificationRunError::NotFound(
            "verification run was not found".to_string(),
        ));
    }
    let select_status =
        db.sql("SELECT status FROM listing_verification_runs WHERE id = ? AND owner_user_id = ?");
    let cancel_items = db.sql(
        r#"
        UPDATE listing_verification_run_items
        SET status = 'cancelled',
            reason_code = 'run_cancelled',
            reason = 'The verification run was cancelled.',
            completed_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP
        WHERE run_id = ? AND status = 'queued'
        "#,
    );
    let cancel_run = db.sql(
        r#"
        UPDATE listing_verification_runs
        SET status = CASE
              WHEN EXISTS (
                SELECT 1
                FROM listing_verification_run_items item
                WHERE item.run_id = listing_verification_runs.id
                  AND item.status = 'running'
              ) THEN 'cancelling'
              ELSE 'cancelled'
            END,
            completed_at = CASE
              WHEN EXISTS (
                SELECT 1
                FROM listing_verification_run_items item
                WHERE item.run_id = listing_verification_runs.id
                  AND item.status = 'running'
              ) THEN NULL
              ELSE COALESCE(completed_at, CURRENT_TIMESTAMP)
            END,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND status IN ('queued', 'running')
        "#,
    );
    macro_rules! cancel_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let status = sqlx::query_scalar::<_, String>(&select_status)
                .bind(run_id)
                .bind(owner_user_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    VerificationRunError::NotFound("verification run was not found".to_string())
                })?;
            if status == "completed" {
                return Err(VerificationRunError::Conflict(
                    "completed verification runs cannot be cancelled".to_string(),
                ));
            }
            if status != "cancelled" && status != "cancelling" {
                sqlx::query(&cancel_items)
                    .bind(run_id)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(&cancel_run)
                    .bind(run_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            transaction.commit().await?;
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => cancel_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => cancel_in_transaction!(pool),
    }
    get_verification_run(db, owner_user_id, run_id).await
}

fn validate_token(token: &str) -> VerificationRunResult<()> {
    if token.trim().is_empty() || token.trim().len() > 200 {
        return Err(VerificationRunError::Validation(
            "lease_token must contain between 1 and 200 characters".to_string(),
        ));
    }
    Ok(())
}

fn validate_lease(token: &str, duration: Duration) -> VerificationRunResult<()> {
    validate_token(token)?;
    if duration.is_zero() || duration > Duration::from_secs(3_600) {
        return Err(VerificationRunError::Validation(
            "lease duration must be between 1 and 3600 seconds".to_string(),
        ));
    }
    Ok(())
}

fn validate_reason(reason_code: &str, reason: &str) -> VerificationRunResult<()> {
    let reason_code = reason_code.trim();
    if reason_code.is_empty()
        || reason_code.len() > 100
        || !reason_code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(VerificationRunError::Validation(
            "reason_code must be stable lowercase snake_case".to_string(),
        ));
    }
    if reason.trim().is_empty() || reason.trim().len() > 2_000 {
        return Err(VerificationRunError::Validation(
            "reason must contain between 1 and 2000 characters".to_string(),
        ));
    }
    Ok(())
}

fn epoch_seconds() -> VerificationRunResult<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| VerificationRunError::Database(error.to_string()))?
        .as_secs();
    i64::try_from(seconds)
        .map_err(|_| VerificationRunError::Database("system clock overflow".to_string()))
}

fn item_status_label(status: VerificationRunItemStatus) -> &'static str {
    match status {
        VerificationRunItemStatus::Queued => "queued",
        VerificationRunItemStatus::Running => "running",
        VerificationRunItemStatus::Verified => "verified",
        VerificationRunItemStatus::PendingReview => "pending_review",
        VerificationRunItemStatus::PendingReference => "pending_reference",
        VerificationRunItemStatus::Blocked => "blocked",
        VerificationRunItemStatus::Failed => "failed",
        VerificationRunItemStatus::Cancelled => "cancelled",
    }
}

fn terminal_outcome(
    outcome: &ListingVerificationOutcome,
) -> (VerificationRunItemStatus, Option<String>, Option<String>) {
    let status = match outcome.status.as_str() {
        "verified" | "already_verified" => VerificationRunItemStatus::Verified,
        "pending_review" => VerificationRunItemStatus::PendingReview,
        "pending_reference" => VerificationRunItemStatus::PendingReference,
        "blocked" | "stale" => VerificationRunItemStatus::Blocked,
        _ => VerificationRunItemStatus::Failed,
    };
    if outcome.finalization.reason_code.is_some() {
        (
            status,
            outcome.finalization.reason_code.clone(),
            outcome.finalization.reason.clone(),
        )
    } else if outcome.avionics.reason_code.is_some() {
        (
            status,
            outcome.avionics.reason_code.clone(),
            outcome.avionics.reason.clone(),
        )
    } else {
        (
            status,
            outcome.aircraft.reason_code.clone(),
            outcome.aircraft.reason.clone(),
        )
    }
}

fn refresh_run_status_sql(db: &AppDb) -> String {
    db.sql(
        r#"
        UPDATE listing_verification_runs
        SET status = CASE
              WHEN status = 'cancelled' THEN 'cancelled'
              WHEN status = 'cancelling' AND EXISTS (
                SELECT 1 FROM listing_verification_run_items item
                WHERE item.run_id = listing_verification_runs.id
                  AND item.status = 'running'
              ) THEN 'cancelling'
              WHEN status = 'cancelling' THEN 'cancelled'
              WHEN EXISTS (
                SELECT 1 FROM listing_verification_run_items item
                WHERE item.run_id = listing_verification_runs.id
                  AND item.status = 'running'
              ) THEN 'running'
              WHEN EXISTS (
                SELECT 1 FROM listing_verification_run_items item
                WHERE item.run_id = listing_verification_runs.id
                  AND item.status = 'queued'
              ) THEN 'queued'
              ELSE 'completed'
            END,
            completed_at = CASE
              WHEN status = 'cancelled' THEN completed_at
              WHEN status = 'cancelling' AND EXISTS (
                SELECT 1 FROM listing_verification_run_items item
                WHERE item.run_id = listing_verification_runs.id
                  AND item.status = 'running'
              ) THEN NULL
              WHEN status = 'cancelling'
                THEN COALESCE(completed_at, CURRENT_TIMESTAMP)
              WHEN EXISTS (
                SELECT 1 FROM listing_verification_run_items item
                WHERE item.run_id = listing_verification_runs.id
                  AND item.status IN ('queued', 'running')
              ) THEN NULL
              ELSE COALESCE(completed_at, CURRENT_TIMESTAMP)
            END,
            updated_at = CURRENT_TIMESTAMP
        WHERE status IN ('queued', 'running', 'cancelling')
        "#,
    )
    .into_owned()
}

fn reclaim_cancelled_sql(db: &AppDb) -> String {
    db.sql(
        r#"
        UPDATE listing_verification_run_items
        SET status = 'cancelled',
            lease_token = NULL,
            lease_expires_at_epoch_seconds = NULL,
            reason_code = 'run_cancelled',
            reason = 'The verification run was cancelled.',
            completed_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP
        WHERE status = 'running'
          AND lease_expires_at_epoch_seconds <= ?
          AND EXISTS (
            SELECT 1 FROM listing_verification_runs run
            WHERE run.id = listing_verification_run_items.run_id
              AND run.status IN ('cancelling', 'cancelled')
          )
        "#,
    )
    .into_owned()
}

fn reclaim_active_sql(db: &AppDb) -> String {
    db.sql(
        r#"
        UPDATE listing_verification_run_items
        SET status = 'queued',
            lease_token = NULL,
            lease_expires_at_epoch_seconds = NULL,
            reason_code = 'worker_lease_expired',
            reason = 'The previous worker stopped before completion; this item was safely requeued.',
            started_at = NULL,
            completed_at = NULL,
            updated_at = CURRENT_TIMESTAMP
        WHERE status = 'running'
          AND lease_expires_at_epoch_seconds <= ?
          AND EXISTS (
            SELECT 1 FROM listing_verification_runs run
            WHERE run.id = listing_verification_run_items.run_id
              AND run.status IN ('queued', 'running')
          )
        "#,
    )
    .into_owned()
}

async fn load_verification_run_item(
    db: &AppDb,
    item_id: i64,
) -> VerificationRunResult<VerificationRunItem> {
    let sql = db.sql(
        r#"
        SELECT
          id, run_id, listing_id, position, status, attempt_count,
          outcome_json, reason_code, reason, created_at, updated_at,
          started_at, completed_at
        FROM listing_verification_run_items
        WHERE id = ?
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, RunItemRow>(&sql)
                .bind(item_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, RunItemRow>(&sql)
                .bind(item_id)
                .fetch_optional(pool)
                .await?
        }
    }
    .ok_or_else(|| {
        VerificationRunError::NotFound("verification run item was not found".to_string())
    })?;
    row.project()
}

async fn finish_item(
    db: &AppDb,
    item_id: i64,
    lease_token: &str,
    status: VerificationRunItemStatus,
    outcome_json: Option<&str>,
    reason_code: Option<&str>,
    reason: Option<&str>,
) -> VerificationRunResult<VerificationRunItem> {
    let update = db.sql(
        r#"
        UPDATE listing_verification_run_items
        SET status = ?,
            lease_token = NULL,
            lease_expires_at_epoch_seconds = NULL,
            outcome_json = ?,
            reason_code = ?,
            reason = ?,
            completed_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND status = 'running' AND lease_token = ?
        RETURNING run_id, listing_id
        "#,
    );
    let refresh = refresh_run_status_sql(db);
    macro_rules! finish_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let updated = sqlx::query_as::<_, (i64, i64)>(&update)
                .bind(item_status_label(status))
                .bind(outcome_json)
                .bind(reason_code)
                .bind(reason)
                .bind(item_id)
                .bind(lease_token.trim())
                .fetch_optional(&mut *transaction)
                .await?;
            let Some((_run_id, listing_id)) = updated else {
                return Err(VerificationRunError::LeaseConflict { item_id });
            };
            if let Some(outcome) = outcome_json {
                let value: Value = serde_json::from_str(outcome).map_err(|error| {
                    VerificationRunError::Validation(format!(
                        "verification outcome is invalid JSON: {error}"
                    ))
                })?;
                if value.get("listing_id").and_then(Value::as_i64) != Some(listing_id) {
                    return Err(VerificationRunError::Validation(
                        "verification outcome belongs to a different listing".to_string(),
                    ));
                }
            }
            sqlx::query(&refresh).execute(&mut *transaction).await?;
            transaction.commit().await?;
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => finish_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => finish_in_transaction!(pool),
    }
    load_verification_run_item(db, item_id).await
}

async fn reclaim_expired_verification_run_leases_at(
    db: &AppDb,
    now_epoch_seconds: i64,
) -> VerificationRunResult<u64> {
    let cancel = reclaim_cancelled_sql(db);
    let requeue = reclaim_active_sql(db);
    let refresh = refresh_run_status_sql(db);
    macro_rules! reclaim_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let cancelled = sqlx::query(&cancel)
                .bind(now_epoch_seconds)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            let requeued = sqlx::query(&requeue)
                .bind(now_epoch_seconds)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            sqlx::query(&refresh).execute(&mut *transaction).await?;
            transaction.commit().await?;
            cancelled + requeued
        }};
    }
    Ok(match db.backend() {
        DatabaseBackend::Sqlite(pool) => reclaim_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => reclaim_in_transaction!(pool),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DEVELOPER_EMAIL;
    use crate::listing::verification::{
        ListingAvionicsVerificationStage, ListingVerificationStage,
    };

    fn sqlite_pool(db: &AppDb) -> &sqlx::SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("verification run tests require SQLite");
        };
        pool
    }

    async fn developer_id(db: &AppDb) -> i64 {
        sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
            .bind(DEVELOPER_EMAIL)
            .fetch_one(sqlite_pool(db))
            .await
            .unwrap()
    }

    async fn insert_owner(db: &AppDb, suffix: &str) -> i64 {
        sqlx::query_scalar(
            r#"
            INSERT INTO users (
              email, display_name, auth_provider, auth_subject
            ) VALUES (?, 'Run Test Owner', 'local', ?)
            RETURNING id
            "#,
        )
        .bind(format!("run-{suffix}@example.test"))
        .bind(format!("run-{suffix}"))
        .fetch_one(sqlite_pool(db))
        .await
        .unwrap()
    }

    async fn insert_listing(db: &AppDb, owner_user_id: i64, suffix: &str) -> i64 {
        sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id,
              created_by_user_id,
              source_url,
              model_year,
              asking_price_usd,
              ingestion_state,
              registration_number,
              airframe_hours
            )
            SELECT
              placeholder.aircraft_model_variant_id,
              ?,
              ?,
              2024,
              500000,
              'incomplete',
              'N4242T',
              250
            FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
            WHERE placeholder.singleton_id = 1
            RETURNING id
            "#,
        )
        .bind(owner_user_id)
        .bind(format!("https://example.test/run-{suffix}"))
        .fetch_one(sqlite_pool(db))
        .await
        .unwrap()
    }

    fn request(
        owner_user_id: i64,
        key: &str,
        listing_ids: Vec<i64>,
    ) -> CreateVerificationRunRequest {
        CreateVerificationRunRequest {
            owner_user_id,
            idempotency_key: key.to_string(),
            listing_ids,
        }
    }

    fn outcome(listing_id: i64, status: &str) -> ListingVerificationOutcome {
        ListingVerificationOutcome {
            listing_id,
            status: status.to_string(),
            initial_ingestion_state: "incomplete".to_string(),
            final_ingestion_state: if matches!(status, "verified" | "already_verified") {
                "ready".to_string()
            } else {
                "pending_review".to_string()
            },
            aircraft: ListingVerificationStage {
                status: "current".to_string(),
                reason_code: None,
                reason: None,
                gemini_used: false,
                catalog_writes: 0,
            },
            avionics: ListingAvionicsVerificationStage {
                status: "already_complete".to_string(),
                reason_code: None,
                reason: None,
                accepted: 0,
                safely_discarded: 0,
                remaining_review_aspects: usize::from(status == "pending_review"),
                gemini_used: false,
            },
            finalization: ListingVerificationStage {
                status: if matches!(status, "verified" | "already_verified") {
                    "ready".to_string()
                } else {
                    "not_attempted".to_string()
                },
                reason_code: None,
                reason: None,
                gemini_used: false,
                catalog_writes: 0,
            },
        }
    }

    #[tokio::test]
    async fn create_is_owner_scoped_idempotent_and_blocks_active_duplicates() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner = developer_id(&db).await;
        let foreign_owner = insert_owner(&db, "foreign").await;
        let first = insert_listing(&db, owner, "create-first").await;
        let second = insert_listing(&db, owner, "create-second").await;
        let foreign = insert_listing(&db, foreign_owner, "create-foreign").await;

        let created =
            create_verification_run(&db, &request(owner, "same-key", vec![first, second]))
                .await
                .unwrap();
        assert!(created.created);
        assert_eq!(created.run.total_items, 2);
        assert_eq!(created.run.queued_items, 2);

        let replay = create_verification_run(&db, &request(owner, "same-key", vec![first, second]))
            .await
            .unwrap();
        assert!(!replay.created);
        assert_eq!(replay.run.id, created.run.id);

        assert!(matches!(
            create_verification_run(&db, &request(owner, "same-key", vec![second, first])).await,
            Err(VerificationRunError::IdempotencyConflict { run_id })
                if run_id == created.run.id
        ));
        assert!(matches!(
            create_verification_run(&db, &request(owner, "other-key", vec![first])).await,
            Err(VerificationRunError::ActiveListingConflict {
                run_id,
                listing_id
            }) if run_id == created.run.id && listing_id == first
        ));
        assert!(matches!(
            create_verification_run(&db, &request(owner, "foreign-key", vec![foreign])).await,
            Err(VerificationRunError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn claims_are_sequential_and_require_the_current_lease() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner = developer_id(&db).await;
        let first = insert_listing(&db, owner, "claim-first").await;
        let second = insert_listing(&db, owner, "claim-second").await;
        let run = create_verification_run(&db, &request(owner, "claim", vec![first, second]))
            .await
            .unwrap()
            .run;

        let claim = claim_next_verification_run_item(&db, "lease-one", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.run_id, run.id);
        assert_eq!(claim.listing_id, first);
        assert_eq!(claim.position, 0);
        assert_eq!(claim.attempt_count, 1);
        assert!(
            claim_next_verification_run_item(&db, "lease-two", Duration::from_secs(60))
                .await
                .unwrap()
                .is_none(),
            "one run may have only one running item"
        );
        assert!(matches!(
            complete_verification_run_item(
                &db,
                claim.item_id,
                "wrong-lease",
                &outcome(first, "verified")
            )
            .await,
            Err(VerificationRunError::LeaseConflict { .. })
        ));

        complete_verification_run_item(
            &db,
            claim.item_id,
            "lease-one",
            &outcome(first, "verified"),
        )
        .await
        .unwrap();
        let second_claim =
            claim_next_verification_run_item(&db, "lease-two", Duration::from_secs(60))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(second_claim.listing_id, second);
        assert_eq!(second_claim.position, 1);
    }

    #[tokio::test]
    async fn complete_and_failure_boundaries_preserve_only_sanitized_results() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner = developer_id(&db).await;
        let listing = insert_listing(&db, owner, "finish").await;
        let run = create_verification_run(&db, &request(owner, "finish", vec![listing]))
            .await
            .unwrap()
            .run;
        let claim = claim_next_verification_run_item(&db, "finish-lease", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            complete_verification_run_item(
                &db,
                claim.item_id,
                "finish-lease",
                &outcome(listing + 1, "verified")
            )
            .await,
            Err(VerificationRunError::Validation(_))
        ));
        let completed = complete_verification_run_item(
            &db,
            claim.item_id,
            "finish-lease",
            &outcome(listing, "verified"),
        )
        .await
        .unwrap();
        assert_eq!(completed.status, VerificationRunItemStatus::Verified);
        assert_eq!(
            completed
                .outcome
                .as_ref()
                .and_then(|value| value["listing_id"].as_i64()),
            Some(listing)
        );
        assert_eq!(
            get_verification_run(&db, owner, run.id)
                .await
                .unwrap()
                .status,
            VerificationRunStatus::Completed
        );

        let failed_run = create_verification_run(&db, &request(owner, "failure", vec![listing]))
            .await
            .unwrap()
            .run;
        let failed_claim =
            claim_next_verification_run_item(&db, "failure-lease", Duration::from_secs(60))
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(
            complete_verification_run_item(
                &db,
                failed_claim.item_id,
                "failure-lease",
                &outcome(listing, "failed")
            )
            .await,
            Err(VerificationRunError::Validation(_))
        ));
        let failed = fail_verification_run_item(
            &db,
            failed_claim.item_id,
            "failure-lease",
            "automatic_verification_failed",
            "The listing could not be verified automatically.",
        )
        .await
        .unwrap();
        assert_eq!(failed.status, VerificationRunItemStatus::Failed);
        assert!(failed.outcome.is_none());
        assert_eq!(
            failed.reason_code.as_deref(),
            Some("automatic_verification_failed")
        );
        assert_eq!(
            get_verification_run(&db, owner, failed_run.id)
                .await
                .unwrap()
                .failed_items,
            1
        );
    }

    #[tokio::test]
    async fn expired_lease_is_requeued_and_increments_the_next_attempt() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner = developer_id(&db).await;
        let listing = insert_listing(&db, owner, "expiry").await;
        create_verification_run(&db, &request(owner, "expiry", vec![listing]))
            .await
            .unwrap();
        let first = claim_next_verification_run_item(&db, "expired", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            reclaim_expired_verification_run_leases_at(&db, first.lease_expires_at_epoch_seconds)
                .await
                .unwrap(),
            1
        );
        let page = list_verification_run_items(
            &db,
            owner,
            first.run_id,
            &VerificationRunItemsQuery::default(),
        )
        .await
        .unwrap();
        assert_eq!(page.items[0].status, VerificationRunItemStatus::Queued);
        assert!(page.items[0].started_at.is_none());
        let second = claim_next_verification_run_item(&db, "retry", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.item_id, first.item_id);
        assert_eq!(second.attempt_count, 2);
    }

    #[tokio::test]
    async fn cancellation_waits_for_the_current_item_then_becomes_terminal() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner = developer_id(&db).await;
        let first = insert_listing(&db, owner, "cancel-first").await;
        let second = insert_listing(&db, owner, "cancel-second").await;
        let run = create_verification_run(&db, &request(owner, "cancel", vec![first, second]))
            .await
            .unwrap()
            .run;
        let claim = claim_next_verification_run_item(&db, "cancel-lease", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();

        let cancelling = cancel_verification_run(&db, owner, run.id).await.unwrap();
        assert_eq!(cancelling.status, VerificationRunStatus::Cancelling);
        assert_eq!(cancelling.running_items, 1);
        assert_eq!(cancelling.cancelled_items, 1);
        assert!(cancelling.completed_at.is_none());
        assert!(
            claim_next_verification_run_item(&db, "unused", Duration::from_secs(60))
                .await
                .unwrap()
                .is_none()
        );

        complete_verification_run_item(
            &db,
            claim.item_id,
            "cancel-lease",
            &outcome(first, "verified"),
        )
        .await
        .unwrap();
        let cancelled = get_verification_run(&db, owner, run.id).await.unwrap();
        assert_eq!(cancelled.status, VerificationRunStatus::Cancelled);
        assert_eq!(cancelled.verified_items, 1);
        assert_eq!(cancelled.cancelled_items, 1);
        assert!(cancelled.completed_at.is_some());
    }

    #[tokio::test]
    async fn item_pages_are_owner_scoped_and_resume_by_item_id() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner = developer_id(&db).await;
        let other = insert_owner(&db, "page-other").await;
        let mut listings = Vec::new();
        for suffix in ["page-one", "page-two", "page-three"] {
            listings.push(insert_listing(&db, owner, suffix).await);
        }
        let run = create_verification_run(&db, &request(owner, "page", listings))
            .await
            .unwrap()
            .run;

        let first = list_verification_run_items(
            &db,
            owner,
            run.id,
            &VerificationRunItemsQuery {
                limit: Some(2),
                after_item_id: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(first.items.len(), 2);
        assert!(first.checkpoint.has_more);
        let checkpoint = first.checkpoint.resume_after_item_id.unwrap();
        let second = list_verification_run_items(
            &db,
            owner,
            run.id,
            &VerificationRunItemsQuery {
                limit: Some(2),
                after_item_id: Some(checkpoint),
            },
        )
        .await
        .unwrap();
        assert_eq!(second.items.len(), 1);
        assert!(!second.checkpoint.has_more);
        assert!(matches!(
            list_verification_run_items(&db, other, run.id, &VerificationRunItemsQuery::default())
                .await,
            Err(VerificationRunError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn sqlite_migration_is_idempotent_and_preserves_integrity() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let pool = sqlite_pool(&db);
        let migration =
            include_str!("../../migrations/20260809_listing_verification_runs.sqlite.sql");
        let mut connection = pool.acquire().await.unwrap();
        for _ in 0..2 {
            for statement in migration.split(';').map(str::trim) {
                if !statement.is_empty() {
                    sqlx::query(statement)
                        .execute(&mut *connection)
                        .await
                        .unwrap();
                }
            }
        }
        let contract: (i64, String) = sqlx::query_as(
            r#"
            SELECT contract_version, contract_fingerprint
            FROM schema_migration_contracts
            WHERE migration_name = '20260809_listing_verification_runs'
            "#,
        )
        .fetch_one(&mut *connection)
        .await
        .unwrap();
        assert_eq!(contract.0, 1);
        assert_eq!(
            contract.1,
            "a8beda24d71517ba07e4a81b2802b2fef97296ae6b2256a7ff493d6af5235232"
        );
        let foreign_key_errors: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
        assert_eq!(foreign_key_errors, 0);
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
    }
}
