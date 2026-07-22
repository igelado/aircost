//! Paid Gemini execution adapter for [`super::benchmark`].
//!
//! The adapter intentionally treats retained submissions and catalog rows as
//! immutable benchmark inputs. Its only database writes are the per-request
//! rows produced by [`Store`]. In particular, the avionics path calls the
//! extractor's classification and collision-review primitives directly rather
//! than a catalog resolution/persistence workflow.

use std::collections::BTreeSet;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use super::benchmark::{
    BenchmarkAttempt, BenchmarkCase, BenchmarkInput, BenchmarkRunFuture, BenchmarkRunner,
    BenchmarkUsage, BenchmarkVisualAsset,
};
use super::config::{GeminiRuntimeConfig, GeminiTask};
use super::interactions::{GeminiInteractionsClient, InteractionAccountingContext};
use super::usage::{Record as UsageRecord, SourceCorrelation, Status as UsageStatus, Store};
use crate::aircraft::curation::visual::{
    resolve_visible_aircraft_identifiers_with_accounting, ListingPhotoInput, VisualIdentifierConfig,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{
    AvionicsCatalogCandidate, AvionicsCatalogCollisionReviewContext, AvionicsMetadataContext,
    AvionicsProposedIdentity, AvionicsUnitResolutionCandidate, AvionicsUnitResolutionContext,
    GeminiListingExtractor, GroundedJsonResponse,
};
use crate::html::listing::download::download_identity_images;
use crate::html::listing::media::{discover, MediaReference};

/// A network-backed benchmark runner using retained production inputs.
///
/// Construction performs no paid request. `run` is deliberately sequentially
/// safe and also supports concurrent callers by allocating a unique accounting
/// correlation for every invocation.
pub struct LiveBenchmarkRunner {
    db: AppDb,
    runtime_config: GeminiRuntimeConfig,
    usage_store: Store,
    visual_client: GeminiInteractionsClient,
    correlation_sequence: AtomicU64,
}

impl LiveBenchmarkRunner {
    /// Construct a paid runner from validated runtime routing and
    /// `GEMINI_API_KEY`.
    pub fn from_environment(db: &AppDb, runtime_config: GeminiRuntimeConfig) -> Result<Self> {
        runtime_config
            .validate()
            .context("invalid runtime Gemini configuration for benchmark")?;
        let api_key = env::var("GEMINI_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("GEMINI_API_KEY must be set for a live benchmark"))?;
        let usage_store = Store::new(db);
        let visual_client = GeminiInteractionsClient::new(api_key)
            .context("could not create Gemini Interactions benchmark client")?
            .with_usage_store(usage_store.clone());
        Ok(Self {
            db: db.clone(),
            runtime_config,
            usage_store,
            visual_client,
            correlation_sequence: AtomicU64::new(0),
        })
    }

    async fn run_case(&self, model: &str, case: &BenchmarkCase) -> BenchmarkAttempt {
        let correlation_id = self.next_correlation_id(model, case);
        let task_result = match &case.input {
            BenchmarkInput::ListingExtraction { listing_text, .. } => {
                self.run_listing_extraction(model, case, listing_text, &correlation_id)
                    .await
            }
            BenchmarkInput::GroundedMetadata {
                candidate,
                value_reference_year,
            } => {
                self.run_grounded_metadata(
                    model,
                    case,
                    candidate,
                    *value_reference_year,
                    &correlation_id,
                )
                .await
            }
            BenchmarkInput::AvionicsGroundingReview {
                aircraft,
                candidate,
                listing_evidence,
                catalog_candidates,
            } => {
                self.run_avionics_review(
                    model,
                    case,
                    aircraft,
                    candidate,
                    listing_evidence,
                    catalog_candidates,
                    &correlation_id,
                )
                .await
            }
            BenchmarkInput::VisualIdentity {
                assets,
                prior_audit,
            } => {
                self.run_visual_identity(model, case, assets, prior_audit.as_ref(), &correlation_id)
                    .await
            }
        };

        let mut execution = match task_result {
            Ok(execution) => execution,
            Err(error) => TaskExecution::failed(format!("{error:#}")),
        };
        match self.usage_store.for_correlation(&correlation_id).await {
            Ok(records) => {
                execution.usage = aggregate_usage(&records, &execution.evidence);
            }
            Err(error) => {
                execution.error = Some(combine_errors(
                    execution.error.take(),
                    format!("could not load benchmark usage accounting: {error:#}"),
                ));
            }
        }

        BenchmarkAttempt {
            output: execution.output,
            usage: execution.usage,
            error: execution.error,
        }
    }

    async fn run_listing_extraction(
        &self,
        model: &str,
        case: &BenchmarkCase,
        listing_text: &str,
        correlation_id: &str,
    ) -> Result<TaskExecution> {
        let config = self.config_for_model(model, &[GeminiTask::ListingExtraction])?;
        let extractor = self.scoped_extractor(config, case, correlation_id)?;
        let output = extractor
            .extract(listing_text)
            .await
            .context("live listing extraction failed")?;
        Ok(TaskExecution::success(output))
    }

    async fn run_grounded_metadata(
        &self,
        model: &str,
        case: &BenchmarkCase,
        candidate: &crate::models::ParsedAvionics,
        value_reference_year: i64,
        correlation_id: &str,
    ) -> Result<TaskExecution> {
        let config = self.config_for_model(model, &[GeminiTask::GroundedMetadata])?;
        let extractor = self.scoped_extractor(config, case, correlation_id)?;
        let response = extractor
            .estimate_avionics_metadata(&AvionicsMetadataContext {
                manufacturer: &candidate.manufacturer,
                model: &candidate.model,
                avionics_types: &candidate.avionics_types,
                value_reference_year,
            })
            .await
            .context("live grounded avionics metadata estimation failed")?;
        let mut execution = TaskExecution::success(response.value.clone());
        execution.evidence.observe_grounded_response(&response);
        Ok(execution)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_avionics_review(
        &self,
        model: &str,
        case: &BenchmarkCase,
        aircraft: &super::benchmark::BenchmarkAircraftContext,
        candidate: &crate::models::ParsedAvionics,
        listing_evidence: &str,
        catalog_candidates: &[super::benchmark::BenchmarkCatalogCandidate],
        correlation_id: &str,
    ) -> Result<TaskExecution> {
        let config = self.config_for_model(
            model,
            &[GeminiTask::AvionicsIdentity, GeminiTask::AvionicsReview],
        )?;
        let extractor = self.scoped_extractor(config, case, correlation_id)?;
        let context = AvionicsUnitResolutionContext {
            aircraft_manufacturer: aircraft.manufacturer.clone().unwrap_or_default(),
            aircraft_model: aircraft.model.clone().unwrap_or_default(),
            aircraft_variant: aircraft.variant.clone().unwrap_or_default(),
            model_year: aircraft.model_year.unwrap_or_default(),
            source_url: case.source_url.clone(),
            listing_context: listing_evidence.to_string(),
            requires_listing_evidence: true,
            candidate: AvionicsUnitResolutionCandidate {
                manufacturer: candidate.manufacturer.clone(),
                model: candidate.model.clone(),
                avionics_types: candidate.avionics_types.clone(),
                quantity: candidate.quantity,
            },
            catalog_candidates: catalog_candidates
                .iter()
                .map(|candidate| AvionicsCatalogCandidate {
                    id: candidate.id,
                    manufacturer: candidate.manufacturer.clone(),
                    model: candidate.model.clone(),
                    avionics_types: candidate.avionics_types.clone(),
                    manufacturer_identifier_kind: candidate
                        .manufacturer_identifier_kind
                        .clone()
                        .unwrap_or_else(|| "none".to_string()),
                    manufacturer_identifier: candidate
                        .manufacturer_identifier
                        .clone()
                        .unwrap_or_default(),
                    catalog_status: candidate.catalog_status.clone(),
                })
                .collect(),
        };

        let classification = extractor
            .resolve_avionics_unit(&context)
            .await
            .context("live grounded avionics classification failed")?;
        let mut execution = TaskExecution::success(json!({
            "classification": classification.value,
        }));
        execution
            .evidence
            .observe_grounded_response(&classification);

        let positive = classification
            .value
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| matches!(status, "existing_match" | "propose_new"));
        if !positive {
            return Ok(execution);
        }

        let proposed = match proposed_identity(&classification.value) {
            Ok(proposed) => proposed,
            Err(error) => {
                execution.error = Some(format!(
                    "positive avionics classification could not be independently reviewed: {error:#}"
                ));
                return Ok(execution);
            }
        };
        let review_context = AvionicsCatalogCollisionReviewContext {
            classification_context: context,
            proposed_identity: proposed,
        };
        match extractor
            .review_avionics_catalog_collisions(&review_context)
            .await
        {
            Ok(review) => {
                execution.evidence.observe_grounded_response(&review);
                execution.output = Some(json!({
                    "classification": classification.value,
                    "review": review.value,
                }));
            }
            Err(error) => {
                execution.error = Some(format!(
                    "independent grounded avionics collision review failed: {error:#}"
                ));
            }
        }
        Ok(execution)
    }

    async fn run_visual_identity(
        &self,
        model: &str,
        case: &BenchmarkCase,
        assets: &[BenchmarkVisualAsset],
        prior_audit: Option<&Value>,
        correlation_id: &str,
    ) -> Result<TaskExecution> {
        let selected = select_one_visual_asset(assets, prior_audit)
            .context("visual benchmark case has no downloadable image asset")?;
        let (source_url, retained_html) = self.retained_submission(case.submission_id).await?;
        if source_url != case.source_url {
            bail!(
                "retained submission {} source URL changed after suite creation",
                case.submission_id
            );
        }
        let mut discovery = discover(&source_url, &retained_html)
            .map_err(|error| anyhow!(error))
            .context("could not rediscover retained listing media")?;
        discovery
            .aircraft_photos
            .retain(|reference| selected.matches(reference));
        discovery
            .logbook_attachments
            .retain(|reference| selected.matches(reference));
        if discovery.aircraft_photos.is_empty() && discovery.logbook_attachments.is_empty() {
            bail!(
                "selected visual asset {} is no longer present in retained HTML",
                selected.image_id
            );
        }

        // The downloader re-validates DNS, the public address, host/path, MIME
        // type, redirect policy, and byte limits. The filtered discovery makes
        // it impossible for more than this one asset to be downloaded or sent.
        let mut downloads = download_identity_images(&discovery)
            .await
            .context("could not download the selected benchmark image")?;
        let image = downloads.images.pop().ok_or_else(|| {
            let failures = downloads
                .failures
                .into_iter()
                .map(|failure| format!("{}: {}", failure.asset_id, failure.message))
                .collect::<Vec<_>>()
                .join("; ");
            anyhow!("selected benchmark image could not be downloaded: {failures}")
        })?;
        if image.bytes.len() > selected.maximum_bytes {
            bail!(
                "downloaded visual asset exceeds benchmark case limit ({} > {})",
                image.bytes.len(),
                selected.maximum_bytes
            );
        }
        let photo = ListingPhotoInput::new(selected.image_id.clone(), image.mime_type, image.bytes);

        let config = self.config_for_model(model, &[GeminiTask::AircraftVisualIdentity])?;
        let visual_config = VisualIdentifierConfig::from_runtime_config(&config)
            .context("invalid visual benchmark route")?;
        let accounting = visual_accounting_context(case, correlation_id);
        let resolution = resolve_visible_aircraft_identifiers_with_accounting(
            &self.visual_client,
            &[photo],
            &visual_config,
            accounting,
        )
        .await
        .context("live visual identity benchmark failed")?;

        let observations = resolution
            .candidates
            .iter()
            .flat_map(|candidate| {
                candidate.evidence.iter().map(move |evidence| {
                    json!({
                        "image_id": evidence.image_id,
                        "identifier_kind": candidate.kind,
                        "visible_text": evidence.visible_text,
                        "confidence": evidence.confidence,
                        "box_2d": evidence.box_2d,
                        "visibility_basis": evidence.visibility_basis,
                        "location_description": evidence.location_description,
                    })
                })
            })
            .collect::<Vec<_>>();
        Ok(TaskExecution::success(json!({
            "status": resolution.status,
            "observations": observations,
            "refusal_reason": resolution.refusal_reason,
            "registration_consensus": resolution.registration_consensus,
            "model": resolution.model,
            "prompt_version": resolution.prompt_version,
        })))
    }

    fn scoped_extractor(
        &self,
        config: GeminiRuntimeConfig,
        case: &BenchmarkCase,
        correlation_id: &str,
    ) -> Result<GeminiListingExtractor> {
        Ok(GeminiListingExtractor::from_environment_with_config(config)
            .context("could not create Gemini benchmark extractor")?
            .with_usage_store(self.usage_store.clone())
            .with_usage_scope(
                correlation_id.to_string(),
                case.listing_id,
                Some(submission_source(case)),
            ))
    }

    fn config_for_model(&self, model: &str, tasks: &[GeminiTask]) -> Result<GeminiRuntimeConfig> {
        let model = model.trim();
        if model.is_empty() {
            bail!("benchmark model must not be blank");
        }
        let mut config = self.runtime_config.clone();
        for task in tasks {
            let route = config
                .tasks
                .get_mut(task)
                .ok_or_else(|| anyhow!("runtime config is missing task {task:?}"))?;
            route.model = model.to_string();
        }
        config
            .validate()
            .with_context(|| format!("invalid benchmark route for model {model}"))?;
        Ok(config)
    }

    async fn retained_submission(&self, submission_id: i64) -> Result<(String, String)> {
        if submission_id < 1 {
            bail!("benchmark submission id must be positive");
        }
        let sql = self
            .db
            .sql("SELECT source_url, rendered_html FROM plugin_submissions WHERE id = ?");
        let row = match self.db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, (String, String)>(&sql)
                    .bind(submission_id)
                    .fetch_optional(pool)
                    .await?
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, (String, String)>(&sql)
                    .bind(submission_id)
                    .fetch_optional(pool)
                    .await?
            }
        };
        row.with_context(|| format!("plugin submission {submission_id} no longer exists"))
    }

    fn next_correlation_id(&self, model: &str, case: &BenchmarkCase) -> String {
        let sequence = self.correlation_sequence.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!(
            "gemini-benchmark:{}:{}:{}:{}:{}",
            std::process::id(),
            timestamp,
            sequence,
            model.trim(),
            case.id
        )
    }
}

impl BenchmarkRunner for LiveBenchmarkRunner {
    fn run<'a>(&'a self, model: &'a str, case: &'a BenchmarkCase) -> BenchmarkRunFuture<'a> {
        Box::pin(async move { self.run_case(model, case).await })
    }
}

#[derive(Default)]
struct TaskEvidence {
    grounded_requests: u64,
    successful_google_search_calls: u64,
    citation_urls: BTreeSet<String>,
}

impl TaskEvidence {
    fn observe_grounded_response(&mut self, response: &GroundedJsonResponse) {
        self.grounded_requests = self.grounded_requests.saturating_add(1);
        if response.google_search_used {
            self.successful_google_search_calls =
                self.successful_google_search_calls.saturating_add(1);
        }
        self.citation_urls.extend(
            response
                .grounding_sources
                .iter()
                .map(|source| source.url.trim().to_string())
                .filter(|url| !url.is_empty()),
        );
    }
}

struct TaskExecution {
    output: Option<Value>,
    usage: BenchmarkUsage,
    error: Option<String>,
    evidence: TaskEvidence,
}

impl TaskExecution {
    fn success(output: Value) -> Self {
        Self {
            output: Some(output),
            usage: BenchmarkUsage::default(),
            error: None,
            evidence: TaskEvidence::default(),
        }
    }

    fn failed(error: String) -> Self {
        Self {
            output: None,
            usage: BenchmarkUsage::default(),
            error: Some(error),
            evidence: TaskEvidence::default(),
        }
    }
}

fn aggregate_usage(records: &[UsageRecord], evidence: &TaskEvidence) -> BenchmarkUsage {
    let completed_grounded_requests = records
        .iter()
        .filter(|record| {
            record.status == UsageStatus::Completed
                && matches!(
                    record.task.as_str(),
                    "grounded_metadata" | "avionics_identity" | "avionics_review"
                )
        })
        .count() as u64;
    let recorded_successful_searches = records
        .iter()
        .filter(|record| {
            record.status == UsageStatus::Completed
                && record.metrics.search_query_count.unwrap_or_default() > 0
        })
        .count() as u64;

    BenchmarkUsage {
        total_input_tokens: sum_records(records, |record| record.metrics.input_tokens),
        cached_input_tokens: sum_records(records, |record| record.metrics.cached_tokens),
        total_output_tokens: sum_records(records, |record| record.metrics.output_tokens),
        thought_tokens: sum_records(records, |record| record.metrics.thought_tokens),
        tool_use_tokens: sum_records(records, |record| record.metrics.tool_tokens),
        grounded_requests: completed_grounded_requests.max(evidence.grounded_requests),
        successful_google_search_calls: recorded_successful_searches
            .max(evidence.successful_google_search_calls),
        search_queries: sum_records(records, |record| record.metrics.search_query_count),
        successful_url_context_calls: 0,
        citation_url_count: evidence.citation_urls.len() as u64,
        attempts: records.iter().fold(0_u64, |sum, record| {
            sum.saturating_add(u64::from(record.attempt_count))
        }),
        billable_usage_complete: records.iter().all(has_complete_billable_usage),
    }
}

fn sum_records(records: &[UsageRecord], metric: impl Fn(&UsageRecord) -> Option<u64>) -> u64 {
    records.iter().fold(0_u64, |sum, record| {
        sum.saturating_add(metric(record).unwrap_or_default())
    })
}

fn has_complete_billable_usage(record: &UsageRecord) -> bool {
    record.metrics.input_tokens.is_some()
        && record.metrics.output_tokens.is_some()
        && record.metrics.thought_tokens.is_some()
        && record.metrics.cached_tokens.is_some()
        && record.metrics.search_query_count.is_some()
}

fn proposed_identity(response: &Value) -> Result<AvionicsProposedIdentity> {
    let required_string = |field: &str| -> Result<String> {
        response
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .with_context(|| format!("positive classification is missing {field}"))
    };
    let canonical_types = response
        .get("canonical_types")
        .and_then(Value::as_array)
        .context("positive classification is missing canonical_types")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .context("positive classification contains an invalid canonical type")
        })
        .collect::<Result<Vec<_>>>()?;
    if canonical_types.is_empty() {
        bail!("positive classification has no canonical types");
    }
    Ok(AvionicsProposedIdentity {
        canonical_manufacturer: required_string("canonical_manufacturer")?,
        canonical_model: required_string("canonical_model")?,
        canonical_types,
        manufacturer_identifier_kind: required_string("manufacturer_identifier_kind")?,
        manufacturer_identifier: required_string("manufacturer_identifier")?,
    })
}

fn select_one_visual_asset<'a>(
    assets: &'a [BenchmarkVisualAsset],
    prior_audit: Option<&Value>,
) -> Option<&'a BenchmarkVisualAsset> {
    let mut supporting_ids = BTreeSet::new();
    if let Some(prior_audit) = prior_audit {
        collect_supporting_image_ids(prior_audit, &mut supporting_ids);
    }
    assets
        .iter()
        .find(|asset| {
            supporting_ids
                .iter()
                .any(|id| image_ids_refer_to_same_asset(id, &asset.image_id))
        })
        .or_else(|| {
            assets
                .iter()
                .find(|asset| asset.media_kind == "aircraft_photo" && asset.is_original)
        })
        .or_else(|| {
            assets
                .iter()
                .find(|asset| asset.media_kind == "aircraft_photo")
        })
        .or_else(|| assets.first())
}

fn collect_supporting_image_ids(value: &Value, output: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "supporting_image_ids" {
                    if let Some(ids) = value.as_array() {
                        output.extend(
                            ids.iter()
                                .filter_map(Value::as_str)
                                .map(str::trim)
                                .filter(|id| !id.is_empty())
                                .map(ToString::to_string),
                        );
                    }
                } else {
                    collect_supporting_image_ids(value, output);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_supporting_image_ids(value, output);
            }
        }
        _ => {}
    }
}

fn image_ids_refer_to_same_asset(left: &str, right: &str) -> bool {
    left == right
        || left.strip_prefix("asset-").is_some_and(|value| {
            value == right
                || value
                    .rsplit_once('-')
                    .is_some_and(|(asset, index)| asset == right && index.parse::<usize>().is_ok())
        })
        || right.strip_prefix("asset-").is_some_and(|value| {
            value == left
                || value
                    .rsplit_once('-')
                    .is_some_and(|(asset, index)| asset == left && index.parse::<usize>().is_ok())
        })
}

impl BenchmarkVisualAsset {
    fn matches(&self, reference: &MediaReference) -> bool {
        self.image_id == reference.asset_id
            && self.media_url == reference.media_url
            && self.media_host == reference.media_host
    }
}

fn visual_accounting_context(
    case: &BenchmarkCase,
    correlation_id: &str,
) -> InteractionAccountingContext {
    let mut context = InteractionAccountingContext::new(
        GeminiTask::AircraftVisualIdentity,
        "benchmark_visible_aircraft_identifier_resolution",
    )
    .with_correlation_id(correlation_id.to_string())
    .with_source("plugin_submission", case.submission_id.to_string());
    if let Some(listing_id) = case.listing_id {
        context = context.with_listing_id(listing_id);
    }
    context
}

fn submission_source(case: &BenchmarkCase) -> SourceCorrelation {
    SourceCorrelation {
        kind: "plugin_submission".to_string(),
        id: case.submission_id.to_string(),
    }
}

fn combine_errors(existing: Option<String>, additional: String) -> String {
    match existing {
        Some(existing) => format!("{existing}; {additional}"),
        None => additional,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gemini::benchmark::BenchmarkPricing;
    use crate::gemini::usage::{
        estimate_paid_list_cost, ApiFamily, Metrics as UsageMetrics, ValidationStatus,
    };

    fn usage_record(id: i64, metrics: UsageMetrics) -> UsageRecord {
        let cost = estimate_paid_list_cost("gemini-3.5-flash", "standard", &metrics).ok();
        UsageRecord {
            id,
            task: "listing_extraction".to_string(),
            purpose: "benchmark_listing_extraction".to_string(),
            api_family: ApiFamily::GenerateContent,
            api_version: Some("v1beta".to_string()),
            model: "gemini-3.5-flash".to_string(),
            service_tier: "standard".to_string(),
            status: UsageStatus::Completed,
            validation_status: ValidationStatus::NotEvaluated,
            provider_request_id: Some(format!("request-{id}")),
            correlation_id: Some("benchmark-correlation".to_string()),
            request_fingerprint: None,
            listing_id: None,
            source: None,
            metrics,
            attempt_count: 1,
            retry_count: 0,
            latency_ms: Some(10),
            error: None,
            cost,
            started_at: "2026-07-21T00:00:00Z".to_string(),
            completed_at: Some("2026-07-21T00:00:01Z".to_string()),
        }
    }

    #[test]
    fn aggregate_preserves_reported_tokens_but_withholds_incomplete_cost() {
        let no_request_usage = aggregate_usage(&[], &TaskEvidence::default());
        assert!(no_request_usage.billable_usage_complete);
        assert_eq!(
            BenchmarkPricing::official_standard_defaults()
                .estimate("gemini-3.5-flash", &no_request_usage),
            Some(0.0)
        );

        let complete_metrics = UsageMetrics {
            input_tokens: Some(100),
            output_tokens: Some(20),
            thought_tokens: Some(5),
            cached_tokens: Some(10),
            tool_tokens: Some(3),
            search_query_count: Some(2),
        };
        let complete = usage_record(1, complete_metrics.clone());
        let incomplete = usage_record(
            2,
            UsageMetrics {
                input_tokens: Some(50),
                output_tokens: Some(7),
                thought_tokens: None,
                cached_tokens: Some(5),
                tool_tokens: Some(1),
                search_query_count: Some(1),
            },
        );
        assert!(complete.cost.is_some());
        assert!(incomplete.cost.is_none());

        let required_counter_gaps = [
            (
                "input_tokens",
                UsageMetrics {
                    input_tokens: None,
                    ..complete_metrics.clone()
                },
            ),
            (
                "output_tokens",
                UsageMetrics {
                    output_tokens: None,
                    ..complete_metrics.clone()
                },
            ),
            (
                "thought_tokens",
                UsageMetrics {
                    thought_tokens: None,
                    ..complete_metrics.clone()
                },
            ),
            (
                "cached_tokens",
                UsageMetrics {
                    cached_tokens: None,
                    ..complete_metrics.clone()
                },
            ),
            (
                "search_query_count",
                UsageMetrics {
                    search_query_count: None,
                    ..complete_metrics
                },
            ),
        ];
        for (index, (missing, metrics)) in required_counter_gaps.into_iter().enumerate() {
            let usage = aggregate_usage(
                &[usage_record(index as i64 + 10, metrics)],
                &TaskEvidence::default(),
            );
            assert!(
                !usage.billable_usage_complete,
                "missing {missing} must make billable usage incomplete"
            );
        }

        let complete_usage = aggregate_usage(&[complete.clone()], &TaskEvidence::default());
        assert!(complete_usage.billable_usage_complete);
        assert!(BenchmarkPricing::official_standard_defaults()
            .estimate("gemini-3.5-flash", &complete_usage)
            .is_some());

        let mut complete_without_embedded_cost = usage_record(
            3,
            UsageMetrics {
                input_tokens: Some(40),
                output_tokens: Some(8),
                thought_tokens: Some(0),
                cached_tokens: Some(0),
                tool_tokens: None,
                search_query_count: Some(0),
            },
        );
        complete_without_embedded_cost.cost = None;
        let repriced_usage =
            aggregate_usage(&[complete_without_embedded_cost], &TaskEvidence::default());
        assert!(repriced_usage.billable_usage_complete);
        assert!(BenchmarkPricing::official_standard_defaults()
            .estimate("gemini-3.5-flash", &repriced_usage)
            .is_some());

        let usage = aggregate_usage(&[complete, incomplete], &TaskEvidence::default());
        assert_eq!(usage.total_input_tokens, 150);
        assert_eq!(usage.cached_input_tokens, 15);
        assert_eq!(usage.total_output_tokens, 27);
        assert_eq!(usage.thought_tokens, 5);
        assert_eq!(usage.tool_use_tokens, 4);
        assert_eq!(usage.search_queries, 3);
        assert!(!usage.billable_usage_complete);
        assert!(BenchmarkPricing::official_standard_defaults()
            .estimate("gemini-3.5-flash", &usage)
            .is_none());
    }

    #[test]
    fn aggregate_counts_grounded_metadata_search_and_citations() {
        let mut record = usage_record(
            20,
            UsageMetrics {
                input_tokens: Some(100),
                output_tokens: Some(20),
                thought_tokens: Some(0),
                cached_tokens: Some(0),
                tool_tokens: Some(0),
                search_query_count: Some(1),
            },
        );
        record.task = "grounded_metadata".to_string();
        let response = GroundedJsonResponse {
            value: json!({}),
            google_search_used: true,
            grounding_sources: vec![crate::extract::GeminiGroundingSource {
                chunk_index: 0,
                url: "https://www.garmin.com/example".to_string(),
                title: "Garmin".to_string(),
            }],
            grounding_supports: Vec::new(),
        };
        let mut evidence = TaskEvidence::default();
        evidence.observe_grounded_response(&response);

        let usage = aggregate_usage(&[record], &evidence);
        assert_eq!(usage.grounded_requests, 1);
        assert_eq!(usage.successful_google_search_calls, 1);
        assert_eq!(usage.citation_url_count, 1);
    }

    #[test]
    fn visual_asset_ids_match_historical_audit_suffixes() {
        assert!(image_ids_refer_to_same_asset(
            "asset-11002236043-1",
            "11002236043"
        ));
        assert!(image_ids_refer_to_same_asset("11002236043", "11002236043"));
        assert!(!image_ids_refer_to_same_asset(
            "asset-11002236043-1",
            "11002236044"
        ));
    }

    #[test]
    fn proposed_identity_rejects_empty_positive_identity() {
        let error = proposed_identity(&json!({
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GNX 375",
            "canonical_types": [],
            "manufacturer_identifier_kind": "manufacturer_model_number",
            "manufacturer_identifier": "GNX 375"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("no canonical types"));
    }
}
