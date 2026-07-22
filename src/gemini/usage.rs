//! Durable accounting for Gemini API calls.
//!
//! One row represents one logical provider request. Transport retries belong to
//! that row; model corrections and adjudication passes are separate requests so
//! their cost remains attributable. Token counters are optional because an API
//! failure (or an older response shape) may not report usage. Callers should use
//! `Some(0)` when the provider explicitly reports zero and `None` when the value
//! is unknown.

use std::fmt;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};

/// Version of the paid-list pricing embedded in [`estimate_paid_list_cost`].
///
/// This must change whenever a rate or accounting assumption changes. Historical
/// rows retain the full snapshot used for their estimate.
pub const PRICING_VERSION: &str = "google-ai-developer-2026-07-21";
pub const PRICING_SOURCE_URL: &str = "https://ai.google.dev/gemini-api/docs/pricing";
const SEARCH_MICROUSD_PER_QUERY: u64 = 14_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiFamily {
    GenerateContent,
    Interactions,
}

impl ApiFamily {
    fn as_str(self) -> &'static str {
        match self {
            Self::GenerateContent => "generate_content",
            Self::Interactions => "interactions",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "generate_content" => Ok(Self::GenerateContent),
            "interactions" => Ok(Self::Interactions),
            other => bail!("unknown Gemini API family `{other}`"),
        }
    }
}

impl fmt::Display for ApiFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Completed,
    Failed,
    Cancelled,
    Incomplete,
    RequiresAction,
    BudgetExceeded,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Incomplete => "incomplete",
            Self::RequiresAction => "requires_action",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "incomplete" => Ok(Self::Incomplete),
            "requires_action" => Ok(Self::RequiresAction),
            "budget_exceeded" => Ok(Self::BudgetExceeded),
            other => bail!("unknown Gemini usage status `{other}`"),
        }
    }

    fn is_terminal(self) -> bool {
        self != Self::Pending
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    NotEvaluated,
    Accepted,
    Rejected,
}

impl ValidationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotEvaluated => "not_evaluated",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "not_evaluated" => Ok(Self::NotEvaluated),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            other => bail!("unknown Gemini validation status `{other}`"),
        }
    }
}

/// Provider-reported counters. `None` means the counter was not reported.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Metrics {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub thought_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub tool_tokens: Option<u64>,
    pub search_query_count: Option<u64>,
}

/// A calculated USD cost plus the rates and assumptions used to calculate it.
///
/// Costs are stored in millionths of a US dollar to avoid floating-point
/// rounding. The snapshot should include enough information to reproduce the
/// estimate after provider prices change.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CostEstimate {
    pub total_microusd: u64,
    pub pricing_snapshot: Value,
}

/// Estimate marginal paid-tier list cost from provider-reported usage.
///
/// The calculation deliberately ignores the shared free Search quota, account
/// credits, negotiated discounts, and explicit-cache storage fees. Cached input
/// is a subset of total input, while thinking tokens are additional billed
/// output. Tool tokens are retained in the snapshot but not added a second time
/// because they are a breakdown of tokens already represented by provider input
/// and output counters.
pub fn estimate_paid_list_cost(
    model: &str,
    service_tier: &str,
    metrics: &Metrics,
) -> Result<CostEstimate> {
    let model = model.trim().strip_prefix("models/").unwrap_or(model.trim());
    let service_tier = match service_tier.trim() {
        "" | "default" | "unspecified" | "standard" => "standard",
        "flex" => "flex",
        "priority" => "priority",
        other => bail!("no Gemini pricing is configured for service tier `{other}`"),
    };
    let rates = paid_rates(model, service_tier).with_context(|| {
        format!("no Gemini pricing is configured for model `{model}` on `{service_tier}`")
    })?;

    let input_tokens = required_metric(metrics.input_tokens, "input_tokens")?;
    let output_tokens = required_metric(metrics.output_tokens, "output_tokens")?;
    let thought_tokens = required_metric(metrics.thought_tokens, "thought_tokens")?;
    let cached_tokens = required_metric(metrics.cached_tokens, "cached_tokens")?;
    let search_query_count = required_metric(metrics.search_query_count, "search_query_count")?;
    let uncached_input_tokens = input_tokens.checked_sub(cached_tokens).with_context(|| {
        format!("cached_tokens ({cached_tokens}) exceeds input_tokens ({input_tokens})")
    })?;
    let billed_output_tokens = output_tokens
        .checked_add(thought_tokens)
        .context("output and thought token total overflowed")?;

    let input_microusd = token_cost(uncached_input_tokens, rates.input)?;
    let cached_input_microusd = token_cost(cached_tokens, rates.cached_input)?;
    let output_microusd = token_cost(billed_output_tokens, rates.output)?;
    let search_microusd = search_query_count
        .checked_mul(SEARCH_MICROUSD_PER_QUERY)
        .context("search query cost overflowed")?;
    let total_microusd = [
        input_microusd,
        cached_input_microusd,
        output_microusd,
        search_microusd,
    ]
    .into_iter()
    .try_fold(0_u64, |total, component| total.checked_add(component))
    .context("Gemini cost total overflowed")?;

    Ok(CostEstimate {
        total_microusd,
        pricing_snapshot: serde_json::json!({
            "version": PRICING_VERSION,
            "source_url": PRICING_SOURCE_URL,
            "currency": "USD",
            "estimate_basis": "marginal_paid_list_price",
            "model": model,
            "service_tier": service_tier,
            "rates": {
                "input_microusd_per_million_tokens": rates.input,
                "cached_input_microusd_per_million_tokens": rates.cached_input,
                "output_and_thinking_microusd_per_million_tokens": rates.output,
                "search_microusd_per_query": SEARCH_MICROUSD_PER_QUERY,
            },
            "billable_usage": {
                "uncached_input_tokens": uncached_input_tokens,
                "cached_input_tokens": cached_tokens,
                "output_tokens": output_tokens,
                "thought_tokens": thought_tokens,
                "search_query_count": search_query_count,
                "tool_tokens_informational": metrics.tool_tokens,
            },
            "components_microusd": {
                "input": input_microusd,
                "cached_input": cached_input_microusd,
                "output_and_thinking": output_microusd,
                "search": search_microusd,
            },
            "excluded": [
                "shared_free_search_quota",
                "account_credits_and_negotiated_discounts",
                "explicit_cache_storage",
                "tool_tokens_already_represented_in_provider_totals",
            ],
        }),
    })
}

#[derive(Clone, Copy, Debug)]
struct PaidRates {
    input: u64,
    cached_input: u64,
    output: u64,
}

fn paid_rates(model: &str, service_tier: &str) -> Option<PaidRates> {
    let (input, cached_input, output) = match (model, service_tier) {
        ("gemini-3.6-flash", "standard") => (1_500_000, 150_000, 7_500_000),
        ("gemini-3.6-flash", "flex") => (750_000, 75_000, 3_750_000),
        ("gemini-3.6-flash", "priority") => (2_700_000, 270_000, 13_500_000),
        ("gemini-3.5-flash", "standard") => (1_500_000, 150_000, 9_000_000),
        ("gemini-3.5-flash", "flex") => (750_000, 80_000, 4_500_000),
        ("gemini-3.5-flash", "priority") => (2_700_000, 270_000, 16_200_000),
        ("gemini-3.5-flash-lite", "standard") => (300_000, 30_000, 2_500_000),
        ("gemini-3.5-flash-lite", "flex") => (150_000, 20_000, 1_250_000),
        ("gemini-3.5-flash-lite", "priority") => (540_000, 50_000, 4_500_000),
        ("gemini-3.1-flash-lite", "standard") => (250_000, 25_000, 1_500_000),
        ("gemini-3.1-flash-lite", "flex") => (125_000, 12_500, 750_000),
        ("gemini-3.1-flash-lite", "priority") => (450_000, 45_000, 2_700_000),
        _ => return None,
    };
    Some(PaidRates {
        input,
        cached_input,
        output,
    })
}

fn required_metric(value: Option<u64>, name: &str) -> Result<u64> {
    value.with_context(|| format!("cannot estimate cost because `{name}` was not reported"))
}

fn token_cost(tokens: u64, rate_microusd_per_million: u64) -> Result<u64> {
    let numerator = u128::from(tokens)
        .checked_mul(u128::from(rate_microusd_per_million))
        .context("token cost overflowed")?;
    let rounded = numerator
        .checked_add(500_000)
        .context("token cost rounding overflowed")?
        / 1_000_000;
    rounded
        .try_into()
        .context("token cost exceeds the supported integer range")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceCorrelation {
    pub kind: String,
    pub id: String,
}

/// Static request information known before contacting Gemini.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Start {
    pub task: String,
    pub purpose: String,
    pub api_family: ApiFamily,
    pub api_version: Option<String>,
    pub model: String,
    pub service_tier: String,
    pub correlation_id: Option<String>,
    pub request_fingerprint: Option<String>,
    pub listing_id: Option<i64>,
    pub source: Option<SourceCorrelation>,
}

impl Start {
    pub fn new(
        task: impl Into<String>,
        purpose: impl Into<String>,
        api_family: ApiFamily,
        model: impl Into<String>,
    ) -> Self {
        Self {
            task: task.into(),
            purpose: purpose.into(),
            api_family,
            api_version: None,
            model: model.into(),
            service_tier: "standard".to_string(),
            correlation_id: None,
            request_fingerprint: None,
            listing_id: None,
            source: None,
        }
    }
}

/// Provider and application outcome known when the request finishes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Outcome {
    pub status: Status,
    pub validation_status: ValidationStatus,
    pub provider_request_id: Option<String>,
    pub metrics: Metrics,
    pub attempt_count: u32,
    pub retry_count: u32,
    pub error: Option<String>,
    pub cost: Option<CostEstimate>,
}

impl Outcome {
    pub fn completed(metrics: Metrics) -> Self {
        Self {
            status: Status::Completed,
            validation_status: ValidationStatus::NotEvaluated,
            provider_request_id: None,
            metrics,
            attempt_count: 1,
            retry_count: 0,
            error: None,
            cost: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: Status::Failed,
            validation_status: ValidationStatus::NotEvaluated,
            provider_request_id: None,
            metrics: Metrics::default(),
            attempt_count: 1,
            retry_count: 0,
            error: Some(error.into()),
            cost: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Record {
    pub id: i64,
    pub task: String,
    pub purpose: String,
    pub api_family: ApiFamily,
    pub api_version: Option<String>,
    pub model: String,
    pub service_tier: String,
    pub status: Status,
    pub validation_status: ValidationStatus,
    pub provider_request_id: Option<String>,
    pub correlation_id: Option<String>,
    pub request_fingerprint: Option<String>,
    pub listing_id: Option<i64>,
    pub source: Option<SourceCorrelation>,
    pub metrics: Metrics,
    pub attempt_count: u32,
    pub retry_count: u32,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub cost: Option<CostEstimate>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug)]
pub struct Attempt {
    id: i64,
    started: Instant,
}

impl Attempt {
    pub fn id(&self) -> i64 {
        self.id
    }
}

#[derive(Clone)]
pub struct Store {
    db: AppDb,
}

impl Store {
    pub fn new(db: &AppDb) -> Self {
        Self { db: db.clone() }
    }

    /// Insert a pending accounting row before making the provider request.
    pub async fn start(&self, start: &Start) -> Result<Attempt> {
        validate_start(start)?;
        let sql = self.db.sql(
            r#"
            INSERT INTO gemini_api_usage (
              task,
              purpose,
              api_family,
              api_version,
              model,
              service_tier,
              correlation_id,
              request_fingerprint,
              aircraft_sale_listing_id,
              source_kind,
              source_id,
              status,
              validation_status
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 'not_evaluated')
            RETURNING id
            "#,
        );
        let source_kind = start.source.as_ref().map(|source| source.kind.as_str());
        let source_id = start.source.as_ref().map(|source| source.id.as_str());
        let id = match self.db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, i64>(&sql)
                    .bind(start.task.trim())
                    .bind(start.purpose.trim())
                    .bind(start.api_family.as_str())
                    .bind(trimmed(start.api_version.as_deref()))
                    .bind(start.model.trim())
                    .bind(start.service_tier.trim())
                    .bind(trimmed(start.correlation_id.as_deref()))
                    .bind(trimmed(start.request_fingerprint.as_deref()))
                    .bind(start.listing_id)
                    .bind(source_kind)
                    .bind(source_id)
                    .fetch_one(pool)
                    .await?
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, i64>(&sql)
                    .bind(start.task.trim())
                    .bind(start.purpose.trim())
                    .bind(start.api_family.as_str())
                    .bind(trimmed(start.api_version.as_deref()))
                    .bind(start.model.trim())
                    .bind(start.service_tier.trim())
                    .bind(trimmed(start.correlation_id.as_deref()))
                    .bind(trimmed(start.request_fingerprint.as_deref()))
                    .bind(start.listing_id)
                    .bind(source_kind)
                    .bind(source_id)
                    .fetch_one(pool)
                    .await?
            }
        };
        Ok(Attempt {
            id,
            started: Instant::now(),
        })
    }

    /// Finalize a pending row. An attempt can only be finalized once.
    pub async fn finish(&self, attempt: Attempt, outcome: &Outcome) -> Result<Record> {
        validate_outcome(outcome)?;
        let latency_ms = to_i64(
            attempt.started.elapsed().as_millis(),
            "request latency in milliseconds",
        )?;
        let metrics = StoredMetrics::try_from(&outcome.metrics)?;
        let estimated_cost_microusd = outcome
            .cost
            .as_ref()
            .map(|cost| to_i64(cost.total_microusd, "estimated cost"))
            .transpose()?;
        let pricing_snapshot_json = outcome
            .cost
            .as_ref()
            .map(|cost| serde_json::to_string(&cost.pricing_snapshot))
            .transpose()
            .context("could not serialize Gemini pricing snapshot")?;
        let sql = self.db.sql(
            r#"
            UPDATE gemini_api_usage
            SET status = ?,
                validation_status = ?,
                provider_request_id = ?,
                input_tokens = ?,
                output_tokens = ?,
                thought_tokens = ?,
                cached_tokens = ?,
                tool_tokens = ?,
                search_query_count = ?,
                attempt_count = ?,
                retry_count = ?,
                latency_ms = ?,
                error_text = ?,
                estimated_cost_microusd = ?,
                pricing_snapshot_json = ?,
                completed_at = CURRENT_TIMESTAMP
            WHERE id = ? AND status = 'pending'
            "#,
        );
        let affected = match self.db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query(&sql)
                .bind(outcome.status.as_str())
                .bind(outcome.validation_status.as_str())
                .bind(trimmed(outcome.provider_request_id.as_deref()))
                .bind(metrics.input_tokens)
                .bind(metrics.output_tokens)
                .bind(metrics.thought_tokens)
                .bind(metrics.cached_tokens)
                .bind(metrics.tool_tokens)
                .bind(metrics.search_query_count)
                .bind(i64::from(outcome.attempt_count))
                .bind(i64::from(outcome.retry_count))
                .bind(latency_ms)
                .bind(trimmed(outcome.error.as_deref()))
                .bind(estimated_cost_microusd)
                .bind(pricing_snapshot_json.as_deref())
                .bind(attempt.id)
                .execute(pool)
                .await?
                .rows_affected(),
            DatabaseBackend::Postgres(pool) => sqlx::query(&sql)
                .bind(outcome.status.as_str())
                .bind(outcome.validation_status.as_str())
                .bind(trimmed(outcome.provider_request_id.as_deref()))
                .bind(metrics.input_tokens)
                .bind(metrics.output_tokens)
                .bind(metrics.thought_tokens)
                .bind(metrics.cached_tokens)
                .bind(metrics.tool_tokens)
                .bind(metrics.search_query_count)
                .bind(i64::from(outcome.attempt_count))
                .bind(i64::from(outcome.retry_count))
                .bind(latency_ms)
                .bind(trimmed(outcome.error.as_deref()))
                .bind(estimated_cost_microusd)
                .bind(pricing_snapshot_json.as_deref())
                .bind(attempt.id)
                .execute(pool)
                .await?
                .rows_affected(),
        };
        if affected != 1 {
            bail!(
                "Gemini usage attempt {} does not exist or is already finalized",
                attempt.id
            );
        }
        self.get(attempt.id)
            .await?
            .context("finalized Gemini usage row disappeared")
    }

    pub async fn get(&self, id: i64) -> Result<Option<Record>> {
        if id < 1 {
            bail!("Gemini usage id must be positive");
        }
        let sql = self.db.sql(
            r#"
            SELECT id, task, purpose, api_family, api_version, model, service_tier,
                   status, validation_status, provider_request_id, correlation_id,
                   request_fingerprint, aircraft_sale_listing_id, source_kind, source_id,
                   input_tokens, output_tokens, thought_tokens, cached_tokens, tool_tokens,
                   search_query_count, attempt_count, retry_count, latency_ms, error_text,
                   estimated_cost_microusd, pricing_snapshot_json, started_at, completed_at
            FROM gemini_api_usage
            WHERE id = ?
            "#,
        );
        let row = match self.db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, StoredRow>(&sql)
                    .bind(id)
                    .fetch_optional(pool)
                    .await?
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, StoredRow>(&sql)
                    .bind(id)
                    .fetch_optional(pool)
                    .await?
            }
        };
        row.map(Record::try_from).transpose()
    }

    /// Return all calls in a benchmark/job correlation in execution order.
    pub async fn for_correlation(&self, correlation_id: &str) -> Result<Vec<Record>> {
        require_nonempty("correlation_id", correlation_id)?;
        let sql = self.db.sql(
            r#"
            SELECT id, task, purpose, api_family, api_version, model, service_tier,
                   status, validation_status, provider_request_id, correlation_id,
                   request_fingerprint, aircraft_sale_listing_id, source_kind, source_id,
                   input_tokens, output_tokens, thought_tokens, cached_tokens, tool_tokens,
                   search_query_count, attempt_count, retry_count, latency_ms, error_text,
                   estimated_cost_microusd, pricing_snapshot_json, started_at, completed_at
            FROM gemini_api_usage
            WHERE correlation_id = ?
            ORDER BY id
            "#,
        );
        let rows = match self.db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, StoredRow>(&sql)
                    .bind(correlation_id.trim())
                    .fetch_all(pool)
                    .await?
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, StoredRow>(&sql)
                    .bind(correlation_id.trim())
                    .fetch_all(pool)
                    .await?
            }
        };
        rows.into_iter().map(Record::try_from).collect()
    }
}

#[derive(Debug, FromRow)]
struct StoredRow {
    id: i64,
    task: String,
    purpose: String,
    api_family: String,
    api_version: Option<String>,
    model: String,
    service_tier: String,
    status: String,
    validation_status: String,
    provider_request_id: Option<String>,
    correlation_id: Option<String>,
    request_fingerprint: Option<String>,
    aircraft_sale_listing_id: Option<i64>,
    source_kind: Option<String>,
    source_id: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    thought_tokens: Option<i64>,
    cached_tokens: Option<i64>,
    tool_tokens: Option<i64>,
    search_query_count: Option<i64>,
    attempt_count: i64,
    retry_count: i64,
    latency_ms: Option<i64>,
    error_text: Option<String>,
    estimated_cost_microusd: Option<i64>,
    pricing_snapshot_json: Option<String>,
    started_at: String,
    completed_at: Option<String>,
}

#[derive(Debug)]
struct StoredMetrics {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    thought_tokens: Option<i64>,
    cached_tokens: Option<i64>,
    tool_tokens: Option<i64>,
    search_query_count: Option<i64>,
}

impl TryFrom<&Metrics> for StoredMetrics {
    type Error = anyhow::Error;

    fn try_from(metrics: &Metrics) -> Result<Self> {
        Ok(Self {
            input_tokens: optional_i64(metrics.input_tokens, "input tokens")?,
            output_tokens: optional_i64(metrics.output_tokens, "output tokens")?,
            thought_tokens: optional_i64(metrics.thought_tokens, "thought tokens")?,
            cached_tokens: optional_i64(metrics.cached_tokens, "cached tokens")?,
            tool_tokens: optional_i64(metrics.tool_tokens, "tool tokens")?,
            search_query_count: optional_i64(metrics.search_query_count, "search query count")?,
        })
    }
}

impl TryFrom<StoredRow> for Record {
    type Error = anyhow::Error;

    fn try_from(row: StoredRow) -> Result<Self> {
        let source = match (row.source_kind, row.source_id) {
            (Some(kind), Some(id)) => Some(SourceCorrelation { kind, id }),
            (None, None) => None,
            _ => bail!(
                "Gemini usage row {} has incomplete source correlation",
                row.id
            ),
        };
        let pricing_snapshot = row
            .pricing_snapshot_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .with_context(|| format!("Gemini usage row {} has invalid pricing JSON", row.id))?;
        let cost = match (row.estimated_cost_microusd, pricing_snapshot) {
            (Some(total), Some(pricing_snapshot)) => Some(CostEstimate {
                total_microusd: from_i64(total, "estimated cost")?,
                pricing_snapshot,
            }),
            (None, None) => None,
            _ => bail!("Gemini usage row {} has incomplete cost accounting", row.id),
        };
        Ok(Self {
            id: row.id,
            task: row.task,
            purpose: row.purpose,
            api_family: ApiFamily::parse(&row.api_family)?,
            api_version: row.api_version,
            model: row.model,
            service_tier: row.service_tier,
            status: Status::parse(&row.status)?,
            validation_status: ValidationStatus::parse(&row.validation_status)?,
            provider_request_id: row.provider_request_id,
            correlation_id: row.correlation_id,
            request_fingerprint: row.request_fingerprint,
            listing_id: row.aircraft_sale_listing_id,
            source,
            metrics: Metrics {
                input_tokens: optional_u64(row.input_tokens, "input tokens")?,
                output_tokens: optional_u64(row.output_tokens, "output tokens")?,
                thought_tokens: optional_u64(row.thought_tokens, "thought tokens")?,
                cached_tokens: optional_u64(row.cached_tokens, "cached tokens")?,
                tool_tokens: optional_u64(row.tool_tokens, "tool tokens")?,
                search_query_count: optional_u64(row.search_query_count, "search query count")?,
            },
            attempt_count: from_i64(row.attempt_count, "attempt count")?
                .try_into()
                .context("attempt count exceeds u32")?,
            retry_count: from_i64(row.retry_count, "retry count")?
                .try_into()
                .context("retry count exceeds u32")?,
            latency_ms: optional_u64(row.latency_ms, "latency")?,
            error: row.error_text,
            cost,
            started_at: row.started_at,
            completed_at: row.completed_at,
        })
    }
}

fn validate_start(start: &Start) -> Result<()> {
    require_nonempty("task", &start.task)?;
    require_nonempty("purpose", &start.purpose)?;
    require_nonempty("model", &start.model)?;
    require_nonempty("service_tier", &start.service_tier)?;
    optional_nonempty("api_version", start.api_version.as_deref())?;
    optional_nonempty("correlation_id", start.correlation_id.as_deref())?;
    optional_nonempty("request_fingerprint", start.request_fingerprint.as_deref())?;
    if start.listing_id.is_some_and(|listing_id| listing_id < 1) {
        bail!("listing_id must be positive");
    }
    if let Some(source) = &start.source {
        require_nonempty("source kind", &source.kind)?;
        require_nonempty("source id", &source.id)?;
    }
    Ok(())
}

fn validate_outcome(outcome: &Outcome) -> Result<()> {
    if !outcome.status.is_terminal() {
        bail!("a Gemini usage outcome cannot remain pending");
    }
    if outcome.attempt_count < 1 {
        bail!("attempt_count must be at least one");
    }
    if outcome.retry_count != outcome.attempt_count - 1 {
        bail!("retry_count must equal attempt_count minus one");
    }
    optional_nonempty(
        "provider_request_id",
        outcome.provider_request_id.as_deref(),
    )?;
    optional_nonempty("error", outcome.error.as_deref())?;
    if outcome.status == Status::Failed && outcome.error.is_none() {
        bail!("failed Gemini usage outcomes require an error");
    }
    if outcome.status == Status::Completed && outcome.error.is_some() {
        bail!("completed Gemini usage outcomes cannot contain an error");
    }
    if outcome.status != Status::Completed
        && outcome.validation_status != ValidationStatus::NotEvaluated
    {
        bail!("only completed Gemini calls can have an application validation result");
    }
    if let Some(cost) = &outcome.cost {
        if !cost.pricing_snapshot.is_object() {
            bail!("pricing_snapshot must be a JSON object");
        }
    }
    Ok(())
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim)
}

fn require_nonempty(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{name} cannot be empty");
    }
    Ok(())
}

fn optional_nonempty(name: &str, value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        bail!("{name} cannot be empty when provided");
    }
    Ok(())
}

fn optional_i64(value: Option<u64>, name: &str) -> Result<Option<i64>> {
    value.map(|value| to_i64(value, name)).transpose()
}

fn optional_u64(value: Option<i64>, name: &str) -> Result<Option<u64>> {
    value.map(|value| from_i64(value, name)).transpose()
}

fn to_i64<T>(value: T, name: &str) -> Result<i64>
where
    T: TryInto<i64> + Copy + fmt::Display,
{
    value
        .try_into()
        .map_err(|_| anyhow::anyhow!("{name} `{value}` exceeds the database integer range"))
}

fn from_i64(value: i64, name: &str) -> Result<u64> {
    value
        .try_into()
        .map_err(|_| anyhow::anyhow!("{name} cannot be negative"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        estimate_paid_list_cost, ApiFamily, CostEstimate, Metrics, Outcome, SourceCorrelation,
        Start, Status, Store, ValidationStatus, PRICING_VERSION,
    };
    use crate::db::AppDb;

    #[tokio::test]
    async fn records_completed_usage_without_conflating_unknown_and_zero() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(&db);
        let mut start = Start::new(
            "avionics_curation",
            "identity_evidence",
            ApiFamily::GenerateContent,
            "gemini-test-lite",
        );
        start.api_version = Some("v1beta".to_string());
        start.service_tier = "flex".to_string();
        start.correlation_id = Some("comparison-7".to_string());
        start.request_fingerprint = Some("sha256:request".to_string());
        start.source = Some(SourceCorrelation {
            kind: "retained_submission".to_string(),
            id: "submission-44".to_string(),
        });
        let attempt = store.start(&start).await.unwrap();
        let pending = store.get(attempt.id()).await.unwrap().unwrap();
        assert_eq!(pending.status, Status::Pending);
        assert_eq!(pending.metrics, Metrics::default());
        assert!(pending.completed_at.is_none());

        let mut outcome = Outcome::completed(Metrics {
            input_tokens: Some(1_200),
            output_tokens: Some(80),
            thought_tokens: None,
            cached_tokens: Some(0),
            tool_tokens: Some(11),
            search_query_count: Some(2),
        });
        outcome.validation_status = ValidationStatus::Accepted;
        outcome.provider_request_id = Some("provider-request-1".to_string());
        outcome.attempt_count = 2;
        outcome.retry_count = 1;
        outcome.cost = Some(CostEstimate {
            total_microusd: 42_500,
            pricing_snapshot: json!({
                "version": "2026-07-21",
                "input_usd_per_million_tokens": 0.30,
                "search_usd_per_thousand_queries": 14.0
            }),
        });

        let record = store.finish(attempt, &outcome).await.unwrap();
        assert_eq!(record.status, Status::Completed);
        assert_eq!(record.validation_status, ValidationStatus::Accepted);
        assert_eq!(record.metrics.thought_tokens, None);
        assert_eq!(record.metrics.cached_tokens, Some(0));
        assert_eq!(record.metrics.search_query_count, Some(2));
        assert_eq!(record.attempt_count, 2);
        assert_eq!(record.retry_count, 1);
        assert_eq!(record.cost, outcome.cost);
        assert!(record.completed_at.is_some());

        let correlated = store.for_correlation("comparison-7").await.unwrap();
        assert_eq!(correlated, vec![record]);
    }

    #[tokio::test]
    async fn records_failures_even_when_usage_is_unavailable() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(&db);
        let start = Start::new(
            "aircraft_identity",
            "visual_registration",
            ApiFamily::Interactions,
            "gemini-test",
        );
        let attempt = store.start(&start).await.unwrap();
        let record = store
            .finish(attempt, &Outcome::failed("provider timeout"))
            .await
            .unwrap();
        assert_eq!(record.status, Status::Failed);
        assert_eq!(record.error.as_deref(), Some("provider timeout"));
        assert_eq!(record.metrics, Metrics::default());
    }

    #[tokio::test]
    async fn rejects_invalid_lifecycle_values_before_writing_them() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(&db);
        let invalid = Start::new(
            " ",
            "listing_extraction",
            ApiFamily::GenerateContent,
            "gemini-test",
        );
        assert!(store
            .start(&invalid)
            .await
            .unwrap_err()
            .to_string()
            .contains("task"));

        let start = Start::new(
            "listing_ingestion",
            "listing_extraction",
            ApiFamily::GenerateContent,
            "gemini-test",
        );
        let attempt = store.start(&start).await.unwrap();
        let mut invalid_outcome = Outcome::completed(Metrics::default());
        invalid_outcome.attempt_count = 3;
        invalid_outcome.retry_count = 1;
        let error = store
            .finish(attempt, &invalid_outcome)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("retry_count"));
    }

    #[test]
    fn estimates_versioned_token_cache_thinking_and_search_costs() {
        let metrics = Metrics {
            input_tokens: Some(10_000),
            output_tokens: Some(1_000),
            thought_tokens: Some(500),
            cached_tokens: Some(2_000),
            tool_tokens: Some(12),
            search_query_count: Some(2),
        };
        let standard =
            estimate_paid_list_cost("gemini-3.5-flash-lite", "standard", &metrics).unwrap();
        assert_eq!(standard.total_microusd, 34_210);
        assert_eq!(standard.pricing_snapshot["version"], PRICING_VERSION);
        assert_eq!(
            standard.pricing_snapshot["components_microusd"],
            json!({
                "input": 2_400,
                "cached_input": 60,
                "output_and_thinking": 3_750,
                "search": 28_000,
            })
        );
        assert_eq!(
            standard.pricing_snapshot["billable_usage"]["tool_tokens_informational"],
            12
        );

        let flex =
            estimate_paid_list_cost("models/gemini-3.5-flash-lite", "flex", &metrics).unwrap();
        assert_eq!(flex.total_microusd, 31_115);
    }

    #[test]
    fn refuses_to_guess_when_pricing_or_usage_is_unknown() {
        let incomplete = Metrics {
            input_tokens: Some(10),
            output_tokens: Some(5),
            thought_tokens: None,
            cached_tokens: Some(0),
            tool_tokens: None,
            search_query_count: Some(0),
        };
        assert!(
            estimate_paid_list_cost("gemini-3.1-flash-lite", "standard", &incomplete)
                .unwrap_err()
                .to_string()
                .contains("thought_tokens")
        );

        let complete = Metrics {
            thought_tokens: Some(0),
            ..incomplete
        };
        assert!(
            estimate_paid_list_cost("gemini-future", "standard", &complete)
                .unwrap_err()
                .to_string()
                .contains("no Gemini pricing")
        );

        let invalid_cache = Metrics {
            input_tokens: Some(1),
            cached_tokens: Some(2),
            ..complete
        };
        assert!(
            estimate_paid_list_cost("gemini-3.1-flash-lite", "standard", &invalid_cache)
                .unwrap_err()
                .to_string()
                .contains("exceeds input_tokens")
        );
    }
}
