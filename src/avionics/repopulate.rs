use std::collections::HashMap;
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

use crate::aircraft::faa::require_listing_faa_admission;
use crate::avionics::catalog::{
    preview_avionics_identity, resolve_avionics_identity, resolve_verified_local_avionics_identity,
    unique_exact_avionics_review_candidate, ApprovedAvionicsIdentity, AvionicsIdentityOutcome,
    AvionicsIdentityRequest,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::GeminiListingExtractor;
use crate::gemini::interactions::RetryPolicy;
use crate::gemini::usage::SourceCorrelation;
use crate::html::clean::clean_listing_html;
use crate::listing::avionics::{
    approved_avionics_product_key, preview_avionics_product_key,
    validate_canonical_avionics_actions, CanonicalAvionicsAction,
};
use crate::listing::evidence::{identity_span_has_boundaries, ListingEvidenceContext};
use crate::listing::review::automation::{
    apply_automated_avionics_review, AutomatedAvionicsLink, AutomatedReviewApplyRequest,
};
use crate::listing::review::{
    PendingReviewAspect, ReviewAction, ReviewAspectId, ReviewProduct, StableIdentifier,
};
use crate::models::ParsedAvionics;
use crate::plugin::sha256_hex;

#[derive(Debug)]
pub enum AvionicsRepopulationError {
    Validation(String),
    Database(String),
}

impl fmt::Display for AvionicsRepopulationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Database(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for AvionicsRepopulationError {}

impl From<sqlx::Error> for AvionicsRepopulationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

pub type RepopulationResult<T> = Result<T, AvionicsRepopulationError>;

#[derive(Clone, Debug, Default, Serialize)]
pub struct AvionicsRepopulationSummary {
    pub listings_selected: usize,
    pub listings_faa_rejected: usize,
    pub listings_previewed: usize,
    pub listings_applied: usize,
    pub listings_blocked: usize,
    pub listings_missing_source: usize,
    pub listing_errors: usize,
    pub listings_reextraction_required: usize,
    pub listing_reextraction_attempts: usize,
    pub listings_reextracted: usize,
    pub listing_reextraction_errors: usize,
    pub identity_candidates: usize,
    pub identity_resolution_attempts: usize,
    pub existing: usize,
    pub new: usize,
    pub promoted: usize,
    pub rejected: usize,
    pub unresolved: usize,
    pub errors: usize,
    pub accepted: usize,
    pub safely_discarded: usize,
    pub remaining_review_aspects: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsRepopulationReport {
    pub mode: String,
    pub requested_limit: i64,
    pub requested_listing_id: Option<i64>,
    pub requested_after_listing_id: Option<i64>,
    pub checkpoint: AvionicsRepopulationCheckpoint,
    pub provider_request_plan: AvionicsProviderRequestPlan,
    pub reextraction_policy_note: String,
    pub listings: Vec<AvionicsRepopulationListingReport>,
    pub summary: AvionicsRepopulationSummary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvionicsRepopulationExecutionMode {
    Preview,
    Apply,
}

impl AvionicsRepopulationExecutionMode {
    fn applies(self) -> bool {
        self == Self::Apply
    }

    fn label(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Apply => "apply",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AvionicsRepopulationScope {
    pub limit: i64,
    pub listing_id: Option<i64>,
    pub after_listing_id: Option<i64>,
}

impl AvionicsRepopulationScope {
    pub fn new(limit: i64, listing_id: Option<i64>, after_listing_id: Option<i64>) -> Self {
        Self {
            limit,
            listing_id,
            after_listing_id,
        }
    }

    fn validate(&self) -> RepopulationResult<()> {
        if self.limit < 1 {
            return Err(AvionicsRepopulationError::Validation(
                "limit must be at least 1".to_string(),
            ));
        }
        if self.listing_id.is_some_and(|listing_id| listing_id < 1) {
            return Err(AvionicsRepopulationError::Validation(
                "listing_id must be a positive integer".to_string(),
            ));
        }
        if self
            .after_listing_id
            .is_some_and(|listing_id| listing_id < 1)
        {
            return Err(AvionicsRepopulationError::Validation(
                "after_listing_id must be a positive integer".to_string(),
            ));
        }
        if self.listing_id.is_some() && self.after_listing_id.is_some() {
            return Err(AvionicsRepopulationError::Validation(
                "listing_id and after_listing_id are mutually exclusive".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsRepopulationCheckpoint {
    pub requested_after_listing_id: Option<i64>,
    pub page_first_listing_id: Option<i64>,
    pub page_last_listing_id: Option<i64>,
    pub resume_after_listing_id: Option<i64>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AvionicsProviderRequestPlan {
    pub listings_requiring_legacy_reextraction: usize,
    pub listing_extraction_provider_requests_baseline: usize,
    pub listing_extraction_provider_requests_validation_envelope: usize,
    pub retained_identity_components: usize,
    pub verified_local_identity_components: usize,
    pub gemini_initial_identity_components: usize,
    pub gemini_conditional_relationship_components: usize,
    pub initial_grounded_provider_requests_baseline: usize,
    pub initial_grounded_provider_requests_nonpositive_validation_envelope: usize,
    pub positive_identity_provider_requests_baseline: usize,
    pub positive_identity_provider_requests_validation_envelope: usize,
    pub known_total_provider_requests_minimum_baseline: usize,
    pub known_total_provider_requests_all_positive_baseline: usize,
    pub known_total_provider_requests_validation_envelope_maximum: usize,
    pub legacy_reextraction_identity_outputs_unknown: bool,
    pub logical_provider_request_counts_include_transport_retries: bool,
    pub default_max_transport_attempts_per_logical_request: usize,
    pub grounded_pass_note: String,
    pub transport_retry_note: String,
    pub uncertainty_note: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AvionicsRepopulationPreflightSummary {
    pub listings_selected: usize,
    pub listings_ready_with_retained_extraction: usize,
    pub listings_requiring_legacy_reextraction: usize,
    pub listings_faa_rejected: usize,
    pub listings_blocked: usize,
    pub retained_identity_components: usize,
    pub verified_local_identity_components: usize,
    pub gemini_initial_identity_components: usize,
    pub gemini_conditional_relationship_components: usize,
    pub invalid_retained_observations: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsRepopulationPreflightReport {
    pub mode: String,
    pub requested_limit: i64,
    pub requested_listing_id: Option<i64>,
    pub requested_after_listing_id: Option<i64>,
    pub checkpoint: AvionicsRepopulationCheckpoint,
    pub provider_request_plan: AvionicsProviderRequestPlan,
    pub listings: Vec<AvionicsRepopulationPreflightListingReport>,
    pub summary: AvionicsRepopulationPreflightSummary,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsRepopulationPreflightListingReport {
    pub listing_id: i64,
    pub status: String,
    pub reextraction_required: bool,
    pub retained_identity_components: usize,
    pub verified_local_identity_components: usize,
    pub gemini_initial_identity_components: usize,
    pub gemini_conditional_relationship_components: usize,
    pub invalid_retained_observations: usize,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsRepopulationListingReport {
    pub listing_id: i64,
    pub submission_id: Option<i64>,
    pub source_match: Option<String>,
    pub source_extraction_error: Option<String>,
    pub raw_avionics_source: String,
    pub reextraction_required: bool,
    pub reextraction_attempted: bool,
    pub reextraction_succeeded: bool,
    pub reextraction_reason: Option<String>,
    pub reextraction_error: Option<String>,
    pub source_url: Option<String>,
    pub aircraft_manufacturer: String,
    pub aircraft_model: String,
    pub aircraft_variant: String,
    pub model_year: i64,
    pub old_link_count: i64,
    pub prepared_link_count: usize,
    pub accepted: usize,
    pub safely_discarded: usize,
    pub remaining_review_aspects: usize,
    pub status: String,
    pub applied: bool,
    pub candidates: Vec<AvionicsRepopulationCandidateReport>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsRepopulationCandidateReport {
    pub candidate_index: usize,
    pub role: String,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub quantity: i64,
    pub configuration_action: String,
    pub source_evidence_text: Option<String>,
    pub source_confidence: Option<String>,
    pub resolution_attempted: bool,
    pub status: String,
    pub catalog_id: Option<i64>,
    pub canonical_manufacturer: Option<String>,
    pub canonical_model: Option<String>,
    pub canonical_types: Vec<String>,
    pub reason: String,
}

#[derive(Debug, FromRow)]
struct ListingSourceRow {
    listing_id: i64,
    listing_owner_user_id: i64,
    listing_source_url: Option<String>,
    aircraft_manufacturer: String,
    aircraft_model: String,
    aircraft_variant: String,
    model_year: i64,
    old_link_count: i64,
    pending_aspect_count: i64,
    review_payload_json: String,
    review_payload_sha256: String,
    submission_id: Option<i64>,
    submission_owner_user_id: Option<i64>,
    submission_canonical_listing_id: Option<i64>,
    submission_source_url: Option<String>,
    rendered_html: Option<String>,
    rendered_html_sha256: Option<String>,
    extracted_listing_json: Option<String>,
    submission_extraction_error: Option<String>,
}

#[derive(Debug, FromRow)]
struct CatalogStatusRow {
    id: i64,
    catalog_status: String,
}

#[derive(Clone, Debug)]
struct PreparedLink {
    identity_key: String,
    avionics_model_id: i64,
    quantity: i64,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    replacement_identity_key: Option<String>,
}

struct IdentityInput<'a> {
    manufacturer: &'a str,
    model: &'a str,
    avionics_types: &'a [String],
    quantity: i64,
}

struct IdentityAttempt {
    report: AvionicsRepopulationCandidateReport,
    approved_id: Option<i64>,
    identity_key: Option<String>,
    suggested_product: Option<ReviewProduct>,
}

/// Automate only listings already waiting in the review queue. The exact
/// plugin submission attached to the pending bundle is replayed; no newer
/// same-URL submission may silently replace its evidence. Legacy extraction
/// payloads are never transformed: when they do not contain capability arrays,
/// the tool runs the current Gemini listing extractor against the retained,
/// hash-verified HTML and uses that transient result. Dry-run still makes paid
/// preview calls without domain writes. Apply mode atomically accepts only
/// grounded products with exact high-confidence listing evidence, safely
/// discards grounded garbage, and leaves every other aspect pending. Signed
/// plugin payloads are never overwritten.
pub async fn repopulate_listing_avionics(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    mode: AvionicsRepopulationExecutionMode,
    scope: &AvionicsRepopulationScope,
) -> RepopulationResult<AvionicsRepopulationReport> {
    scope.validate()?;
    let page = load_listing_sources(db, scope).await?;
    if scope.listing_id.is_some() && page.rows.is_empty() {
        return Err(AvionicsRepopulationError::Validation(format!(
            "listing {} has no pending review to automate",
            scope.listing_id.unwrap_or_default()
        )));
    }
    let preflight = build_preflight_report(db, scope, &page).await;
    let checkpoint = preflight.checkpoint.clone();
    let provider_request_plan = preflight.provider_request_plan;
    let apply = mode.applies();
    let mut catalog_statuses = load_catalog_statuses(db).await?;
    let mut listings = Vec::with_capacity(page.rows.len());
    for row in page.rows {
        listings.push(process_listing(db, extractor, apply, &row, &mut catalog_statuses).await);
    }
    let summary = summarize(&listings);
    Ok(AvionicsRepopulationReport {
        mode: mode.label().to_string(),
        requested_limit: scope.limit,
        requested_listing_id: scope.listing_id,
        requested_after_listing_id: scope.after_listing_id,
        checkpoint,
        provider_request_plan,
        reextraction_policy_note: if apply {
            "Apply mode verifies the exact pending-review submission, re-extracts incompatible legacy payloads from retained HTML, then atomically persists high-confidence approved links plus only the residual review aspects; signed plugin payloads are never overwritten."
                .to_string()
        } else {
            "Preview mode makes Gemini requests, including legacy re-extraction when required, but neither the generated extraction nor catalog/listing changes are persisted."
                .to_string()
        },
        listings,
        summary,
    })
}

/// Inspect the selected page and produce a provider-request plan without
/// constructing a Gemini client or making any provider request.
pub async fn preflight_listing_avionics_repopulation(
    db: &AppDb,
    scope: &AvionicsRepopulationScope,
) -> RepopulationResult<AvionicsRepopulationPreflightReport> {
    scope.validate()?;
    let page = load_listing_sources(db, scope).await?;
    if scope.listing_id.is_some() && page.rows.is_empty() {
        return Err(AvionicsRepopulationError::Validation(format!(
            "listing {} has no pending review to automate",
            scope.listing_id.unwrap_or_default()
        )));
    }
    Ok(build_preflight_report(db, scope, &page).await)
}

struct ListingSourcePage {
    rows: Vec<ListingSourceRow>,
    has_more: bool,
}

impl ListingSourcePage {
    fn checkpoint(
        &self,
        requested_after_listing_id: Option<i64>,
    ) -> AvionicsRepopulationCheckpoint {
        let page_first_listing_id = self.rows.first().map(|row| row.listing_id);
        let page_last_listing_id = self.rows.last().map(|row| row.listing_id);
        AvionicsRepopulationCheckpoint {
            requested_after_listing_id,
            page_first_listing_id,
            page_last_listing_id,
            resume_after_listing_id: page_last_listing_id.or(requested_after_listing_id),
            has_more: self.has_more,
        }
    }
}

async fn build_preflight_report(
    db: &AppDb,
    scope: &AvionicsRepopulationScope,
    page: &ListingSourcePage,
) -> AvionicsRepopulationPreflightReport {
    let mut listings = Vec::with_capacity(page.rows.len());
    for row in &page.rows {
        listings.push(preflight_listing(db, row).await);
    }
    let mut summary = AvionicsRepopulationPreflightSummary {
        listings_selected: listings.len(),
        ..AvionicsRepopulationPreflightSummary::default()
    };
    for listing in &listings {
        summary.retained_identity_components += listing.retained_identity_components;
        summary.verified_local_identity_components += listing.verified_local_identity_components;
        summary.gemini_initial_identity_components += listing.gemini_initial_identity_components;
        summary.gemini_conditional_relationship_components +=
            listing.gemini_conditional_relationship_components;
        summary.invalid_retained_observations += listing.invalid_retained_observations;
        match listing.status.as_str() {
            "ready_retained_extraction" => {
                summary.listings_ready_with_retained_extraction += 1;
            }
            "ready_legacy_reextraction" => {
                summary.listings_requiring_legacy_reextraction += 1;
            }
            "faa_rejected" => summary.listings_faa_rejected += 1,
            _ => summary.listings_blocked += 1,
        }
    }
    let provider_request_plan = provider_request_plan(&summary);
    AvionicsRepopulationPreflightReport {
        mode: "preflight".to_string(),
        requested_limit: scope.limit,
        requested_listing_id: scope.listing_id,
        requested_after_listing_id: scope.after_listing_id,
        checkpoint: page.checkpoint(scope.after_listing_id),
        provider_request_plan,
        listings,
        summary,
    }
}

async fn preflight_listing(
    db: &AppDb,
    row: &ListingSourceRow,
) -> AvionicsRepopulationPreflightListingReport {
    let mut report = AvionicsRepopulationPreflightListingReport {
        listing_id: row.listing_id,
        status: "blocked".to_string(),
        reextraction_required: false,
        retained_identity_components: 0,
        verified_local_identity_components: 0,
        gemini_initial_identity_components: 0,
        gemini_conditional_relationship_components: 0,
        invalid_retained_observations: 0,
        note: String::new(),
    };
    if let Err(error) = validate_pending_source_binding(row) {
        report.note = error;
        return report;
    }
    if let Err(error) = require_listing_faa_admission(db, row.listing_id).await {
        report.status = "faa_rejected".to_string();
        report.note = error.to_string();
        return report;
    }
    let raw_avionics = match retained_avionics_source(row.extracted_listing_json.as_deref()) {
        RetainedAvionicsSource::Current(avionics) => avionics,
        RetainedAvionicsSource::RequiresReextraction { reason } => {
            report.reextraction_required = true;
            let Some(rendered_html) = row.rendered_html.as_deref() else {
                report.note =
                    "current-schema re-extraction requires retained rendered_html".to_string();
                return report;
            };
            let Some(source_url) = row.submission_source_url.as_deref() else {
                report.note =
                    "current-schema re-extraction requires the retained submission source URL"
                        .to_string();
                return report;
            };
            if let Err(error) = prepare_stored_listing_text(source_url, rendered_html) {
                report.note = error;
                return report;
            }
            report.status = "ready_legacy_reextraction".to_string();
            report.note = format!(
                "{reason}; one listing-extraction request is planned, but its identity component count is unknowable until extraction completes"
            );
            return report;
        }
    };
    let listing_context = ListingEvidenceContext::from_rendered_html(row.rendered_html.as_deref());
    let raw_avionics = coalesce_explicit_numbered_instances(raw_avionics, &listing_context);
    for raw in &raw_avionics {
        if raw_candidate_issue(raw).is_some() {
            report.invalid_retained_observations += 1;
            continue;
        }
        let primary_is_local = match preflight_identity_component(
            db,
            row,
            row.submission_source_url.as_deref(),
            &listing_context,
            IdentityInput {
                manufacturer: &raw.manufacturer,
                model: &raw.model,
                avionics_types: &raw.avionics_types,
                quantity: raw.quantity,
            },
            raw.source_evidence_text.as_deref(),
        )
        .await
        {
            Ok(is_local) => is_local,
            Err(error) => {
                report.note = format!("verified-local identity preflight failed: {error}");
                return report;
            }
        };
        report.retained_identity_components += 1;
        if primary_is_local {
            report.verified_local_identity_components += 1;
        } else {
            report.gemini_initial_identity_components += 1;
        }

        let Some(replacement) = raw.replaces.as_ref() else {
            continue;
        };
        let replacement_is_local = match preflight_identity_component(
            db,
            row,
            row.submission_source_url.as_deref(),
            &listing_context,
            IdentityInput {
                manufacturer: &replacement.manufacturer,
                model: &replacement.model,
                avionics_types: &replacement.avionics_types,
                quantity: 1,
            },
            raw.source_evidence_text.as_deref(),
        )
        .await
        {
            Ok(is_local) => is_local,
            Err(error) => {
                report.note = format!("verified-local replacement preflight failed: {error}");
                return report;
            }
        };
        report.retained_identity_components += 1;
        if replacement_is_local {
            report.verified_local_identity_components += 1;
        } else if primary_is_local {
            // A locally approved primary cannot be rejected, so the execution
            // path will necessarily evaluate its relationship target.
            report.gemini_initial_identity_components += 1;
        } else {
            // A grounded rejection of the primary stops before the target.
            report.gemini_conditional_relationship_components += 1;
        }
    }
    report.status = "ready_retained_extraction".to_string();
    report.note = if report.invalid_retained_observations == 0 {
        "retained current-schema observations are ready".to_string()
    } else {
        format!(
            "{} retained observation(s) are invalid and will remain for review without a Gemini identity request",
            report.invalid_retained_observations
        )
    };
    report
}

async fn preflight_identity_component(
    db: &AppDb,
    row: &ListingSourceRow,
    source_url: Option<&str>,
    listing_context: &ListingEvidenceContext,
    identity: IdentityInput<'_>,
    source_evidence_text: Option<&str>,
) -> Result<bool, String> {
    let request = identity_request(
        row,
        source_url,
        listing_context,
        &identity,
        source_evidence_text,
    );
    resolve_verified_local_avionics_identity(db, &request)
        .await
        .map(|identity| identity.is_some())
        .map_err(|error| error.to_string())
}

fn provider_request_plan(
    summary: &AvionicsRepopulationPreflightSummary,
) -> AvionicsProviderRequestPlan {
    let reextractions = summary.listings_requiring_legacy_reextraction;
    let initial = summary.gemini_initial_identity_components;
    let all_grounded_components =
        initial.saturating_add(summary.gemini_conditional_relationship_components);
    AvionicsProviderRequestPlan {
        listings_requiring_legacy_reextraction: reextractions,
        listing_extraction_provider_requests_baseline: reextractions,
        listing_extraction_provider_requests_validation_envelope: reextractions.saturating_mul(2),
        retained_identity_components: summary.retained_identity_components,
        verified_local_identity_components: summary.verified_local_identity_components,
        gemini_initial_identity_components: initial,
        gemini_conditional_relationship_components: summary
            .gemini_conditional_relationship_components,
        initial_grounded_provider_requests_baseline: initial.saturating_mul(3),
        initial_grounded_provider_requests_nonpositive_validation_envelope: initial
            .saturating_mul(8),
        positive_identity_provider_requests_baseline: all_grounded_components.saturating_mul(6),
        positive_identity_provider_requests_validation_envelope: all_grounded_components
            .saturating_mul(14),
        known_total_provider_requests_minimum_baseline: reextractions
            .saturating_add(initial.saturating_mul(3)),
        known_total_provider_requests_all_positive_baseline: reextractions
            .saturating_add(all_grounded_components.saturating_mul(6)),
        known_total_provider_requests_validation_envelope_maximum: reextractions
            .saturating_mul(2)
            .saturating_add(all_grounded_components.saturating_mul(14)),
        legacy_reextraction_identity_outputs_unknown: reextractions > 0,
        logical_provider_request_counts_include_transport_retries: false,
        default_max_transport_attempts_per_logical_request: usize::from(
            RetryPolicy::default().max_attempts(),
        ),
        grounded_pass_note: "One fresh grounded identity pass has three logical provider requests at baseline (Search, URL Context, structure) and at most six after per-stage validation fallbacks. One reused-evidence identity correction can raise that pass envelope to eight. A positive identity adds an independent collision pass, but its review and optional domain correction share one two-structure-call budget: six requests at baseline and up to fourteen in the complete validation envelope. A nonpositive identity does not run collision review."
            .to_string(),
        transport_retry_note: "Logical provider-request counts do not multiply transport retries. The default interactions retry policy may make up to four transport attempts for one logical request."
            .to_string(),
        uncertainty_note: "The minimum baseline assumes every conditional relationship target is skipped because its primary identity is rejected. The all-positive baseline includes every conditional target and collision pass. Both use the catalog as it exists at preflight time; earlier apply pages can approve identities that later pages resolve locally with zero Gemini requests. Legacy re-extraction output counts and correction/fallback outcomes are unknowable before execution, so no dollar estimate is inferred."
            .to_string(),
    }
}

async fn process_listing(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    row: &ListingSourceRow,
    catalog_statuses: &mut HashMap<i64, String>,
) -> AvionicsRepopulationListingReport {
    let source_url = row.submission_source_url.clone();
    let mut listing_report = AvionicsRepopulationListingReport {
        listing_id: row.listing_id,
        submission_id: row.submission_id,
        source_match: None,
        source_extraction_error: row.submission_extraction_error.clone(),
        raw_avionics_source: "unavailable".to_string(),
        reextraction_required: false,
        reextraction_attempted: false,
        reextraction_succeeded: false,
        reextraction_reason: None,
        reextraction_error: None,
        source_url: source_url.clone(),
        aircraft_manufacturer: row.aircraft_manufacturer.clone(),
        aircraft_model: row.aircraft_model.clone(),
        aircraft_variant: row.aircraft_variant.clone(),
        model_year: row.model_year,
        old_link_count: row.old_link_count,
        prepared_link_count: 0,
        accepted: 0,
        safely_discarded: 0,
        remaining_review_aspects: row.pending_aspect_count.max(0) as usize,
        status: "blocked".to_string(),
        applied: false,
        candidates: Vec::new(),
        error: None,
    };

    let submission_id = match validate_pending_source_binding(row) {
        Ok((submission_id, source_match)) => {
            listing_report.source_match = Some(source_match);
            submission_id
        }
        Err(error) => {
            listing_report.error = Some(error);
            return listing_report;
        }
    };
    let scoped_extractor = extractor.clone().with_usage_scope(
        format!("auto-review-listing-{}", row.listing_id),
        Some(row.listing_id),
        Some(SourceCorrelation {
            kind: "plugin_submission".to_string(),
            id: submission_id.to_string(),
        }),
    );

    // Avionics curation only needs to prove that the source listing belongs
    // to a current FAA-backed N-number. Requiring the separately curated
    // aircraft product hierarchy here would deadlock the two independent
    // curation queues: neither could make progress until the other finished.
    let faa_grounding = match require_listing_faa_admission(db, row.listing_id).await {
        Ok(grounding) => grounding,
        Err(error) => {
            listing_report.status = "faa_rejected".to_string();
            listing_report.error = Some(error.to_string());
            return listing_report;
        }
    };
    let raw_avionics = match retained_avionics_source(row.extracted_listing_json.as_deref()) {
        RetainedAvionicsSource::Current(avionics) => {
            listing_report.raw_avionics_source = "retained_extraction".to_string();
            avionics
        }
        RetainedAvionicsSource::RequiresReextraction { reason } => {
            listing_report.reextraction_required = true;
            listing_report.reextraction_reason = Some(reason);
            let Some(rendered_html) = row
                .rendered_html
                .as_deref()
                .filter(|rendered_html| !rendered_html.trim().is_empty())
            else {
                let error =
                    "current-schema re-extraction requires retained rendered_html".to_string();
                listing_report.status = "missing_source".to_string();
                listing_report.reextraction_error = Some(error.clone());
                listing_report.error = Some(error);
                return listing_report;
            };
            let Some(source_url) = source_url
                .as_deref()
                .filter(|source_url| !source_url.trim().is_empty())
            else {
                let error =
                    "current-schema re-extraction requires the retained submission or listing source URL"
                        .to_string();
                listing_report.status = "missing_source".to_string();
                listing_report.reextraction_error = Some(error.clone());
                listing_report.error = Some(error);
                return listing_report;
            };
            let listing_text = match prepare_stored_listing_text(source_url, rendered_html) {
                Ok(listing_text) => listing_text,
                Err(error) => {
                    listing_report.status = "missing_source".to_string();
                    listing_report.reextraction_error = Some(error.clone());
                    listing_report.error = Some(error);
                    return listing_report;
                }
            };
            listing_report.reextraction_attempted = true;
            match reextract_avionics(&scoped_extractor, &listing_text).await {
                Ok(avionics) => {
                    listing_report.raw_avionics_source = "gemini_reextraction".to_string();
                    listing_report.reextraction_succeeded = true;
                    avionics
                }
                Err(error) => {
                    listing_report.status = "error".to_string();
                    listing_report.reextraction_error = Some(error.clone());
                    listing_report.error = Some(format!(
                        "current-schema Gemini re-extraction failed; old links were retained: {error}"
                    ));
                    return listing_report;
                }
            }
        }
    };
    if raw_avionics.is_empty() {
        listing_report.status = "blocked".to_string();
        listing_report.error = Some(
            "current extraction has no avionics observations; the pending review was retained because an empty extraction is not evidence that every prior observation is garbage"
                .to_string(),
        );
        return listing_report;
    }
    let listing_context = ListingEvidenceContext::from_rendered_html(row.rendered_html.as_deref());
    let raw_avionics = coalesce_explicit_numbered_instances(raw_avionics, &listing_context);
    let mut prepared: Vec<PreparedLink> = Vec::new();
    let mut residual_aspects = Vec::new();
    let mut blocking_reasons = Vec::new();

    for (candidate_index, raw) in raw_avionics.iter().enumerate() {
        if let Some(issue) = raw_candidate_issue(raw) {
            listing_report.candidates.push(input_error_report(
                candidate_index,
                "primary",
                &raw.manufacturer,
                &raw.model,
                &raw.avionics_types,
                raw.quantity,
                &raw.configuration_action,
                raw.source_evidence_text.clone(),
                raw.source_confidence.clone(),
                &issue,
            ));
            residual_aspects.extend(input_error_aspects(candidate_index, raw, &issue));
            continue;
        }

        let mut primary = resolve_identity_attempt(
            db,
            &scoped_extractor,
            apply,
            row,
            source_url.as_deref(),
            &listing_context,
            candidate_index,
            "primary",
            IdentityInput {
                manufacturer: &raw.manufacturer,
                model: &raw.model,
                avionics_types: &raw.avionics_types,
                quantity: raw.quantity,
            },
            &raw.configuration_action,
            raw.source_evidence_text.as_deref(),
            raw.source_confidence.as_deref(),
            catalog_statuses,
        )
        .await;
        if primary.report.status == "rejected" {
            listing_report.candidates.push(primary.report);
            listing_report.safely_discarded += 1;
            continue;
        }

        let primary_is_approved = identity_is_approved(&primary);
        let primary_has_high_evidence = raw.source_confidence.as_deref() == Some("high");
        if primary_is_approved && !primary_has_high_evidence {
            mark_weak_listing_evidence(&mut primary.report);
        }

        if raw.configuration_action == "installed" {
            if can_automatically_accept(&primary, raw.source_confidence.as_deref()) {
                let primary_id = primary
                    .approved_id
                    .expect("approved attempt has a catalog id");
                let primary_identity_key = primary
                    .identity_key
                    .clone()
                    .expect("approved attempt has a product key");
                let incoming_link = PreparedLink {
                    identity_key: primary_identity_key,
                    avionics_model_id: primary_id,
                    quantity: raw.quantity,
                    source_notes: raw.source_evidence_text.clone(),
                    source_confidence: Some("high".to_string()),
                    configuration_action: raw.configuration_action.clone(),
                    replaces_avionics_model_id: None,
                    replacement_identity_key: None,
                };
                if let Err(error) =
                    merge_prepared_link_for_candidate(&mut prepared, incoming_link, &mut primary)
                {
                    blocking_reasons.push(format!("candidate {candidate_index}: {error}"));
                }
            } else {
                let aspect = primary_residual_aspect(
                    candidate_index,
                    raw,
                    primary.report.reason.clone(),
                    primary.suggested_product.clone(),
                    None,
                );
                match attach_unique_exact_review_candidate(
                    db,
                    aspect,
                    &raw.manufacturer,
                    &raw.model,
                    &raw.avionics_types,
                )
                .await
                {
                    Ok(aspect) => residual_aspects.push(aspect),
                    Err(error) => blocking_reasons.push(format!(
                        "candidate {candidate_index}: exact catalog review retrieval failed: {error}"
                    )),
                }
            }
            listing_report.candidates.push(primary.report);
            continue;
        }

        let replacement = raw
            .replaces
            .as_ref()
            .expect("raw_candidate_issue requires replacement identity");
        let mut replacement_attempt = resolve_identity_attempt(
            db,
            &scoped_extractor,
            apply,
            row,
            source_url.as_deref(),
            &listing_context,
            candidate_index,
            "replacement",
            IdentityInput {
                manufacturer: &replacement.manufacturer,
                model: &replacement.model,
                avionics_types: &replacement.avionics_types,
                quantity: 1,
            },
            &raw.configuration_action,
            raw.source_evidence_text.as_deref(),
            raw.source_confidence.as_deref(),
            catalog_statuses,
        )
        .await;
        let replacement_is_approved = identity_is_approved(&replacement_attempt);
        if replacement_is_approved && !primary_has_high_evidence {
            mark_weak_listing_evidence(&mut replacement_attempt.report);
        }

        let both_approved_with_high_evidence =
            primary_is_approved && replacement_is_approved && primary_has_high_evidence;
        if !both_approved_with_high_evidence {
            let primary_reason = if primary_is_approved {
                if primary_has_high_evidence {
                    format!(
                        "the product identity is verified, but its {} relationship has an unresolved target",
                        raw.configuration_action
                    )
                } else {
                    primary.report.reason.clone()
                }
            } else {
                primary.report.reason.clone()
            };
            let replacement_reason = if replacement_attempt.report.status == "rejected" {
                format!(
                    "the grounded classifier rejected this relationship target, so the dependent {} observation requires review: {}",
                    raw.configuration_action, replacement_attempt.report.reason
                )
            } else {
                replacement_attempt.report.reason.clone()
            };
            let mut dependent_aspects = dependent_residual_aspects(
                candidate_index,
                raw,
                &primary_reason,
                primary.suggested_product.clone(),
                &replacement_reason,
                replacement_attempt.suggested_product.clone(),
            );
            let primary_aspect = dependent_aspects.remove(0);
            match attach_unique_exact_review_candidate(
                db,
                primary_aspect,
                &raw.manufacturer,
                &raw.model,
                &raw.avionics_types,
            )
            .await
            {
                Ok(aspect) => residual_aspects.push(aspect),
                Err(error) => blocking_reasons.push(format!(
                    "candidate {candidate_index}: exact primary catalog review retrieval failed: {error}"
                )),
            }
            let replacement_aspect = dependent_aspects
                .pop()
                .expect("dependent review has one replacement aspect");
            match attach_unique_exact_review_candidate(
                db,
                replacement_aspect,
                &replacement.manufacturer,
                &replacement.model,
                &replacement.avionics_types,
            )
            .await
            {
                Ok(aspect) => residual_aspects.push(aspect),
                Err(error) => blocking_reasons.push(format!(
                    "candidate {candidate_index}: exact replacement catalog review retrieval failed: {error}"
                )),
            }
            listing_report.candidates.push(primary.report);
            listing_report.candidates.push(replacement_attempt.report);
            continue;
        }

        let primary_id = primary
            .approved_id
            .expect("approved primary has a catalog id");
        let primary_identity_key = primary
            .identity_key
            .clone()
            .expect("approved primary has a product key");
        let replacement_id = replacement_attempt
            .approved_id
            .expect("approved replacement has a catalog id");
        let replacement_identity_key = replacement_attempt
            .identity_key
            .clone()
            .expect("approved replacement has a product key");
        let same_identity = replacement_identity_key == primary_identity_key;
        let same_catalog_id = primary_id > 0 && replacement_id > 0 && replacement_id == primary_id;
        let invalid_relationship = match raw.configuration_action.as_str() {
            "replaces" => same_identity || same_catalog_id,
            "removes" => !same_identity || (apply && !same_catalog_id),
            _ => false,
        };
        if invalid_relationship {
            let reason = match raw.configuration_action.as_str() {
                "removes" => format!(
                    "removal must resolve subject and displaced target to the same catalog product (subject {primary_id}, target {replacement_id})"
                ),
                _ => format!("catalog product {primary_id} cannot replace itself"),
            };
            primary.report.status = "unresolved".to_string();
            primary.report.reason = reason.clone();
            replacement_attempt.report.status = "unresolved".to_string();
            replacement_attempt.report.reason = reason.clone();
            residual_aspects.extend(dependent_residual_aspects(
                candidate_index,
                raw,
                &reason,
                primary.suggested_product.clone(),
                &reason,
                replacement_attempt.suggested_product.clone(),
            ));
            listing_report.candidates.push(primary.report);
            listing_report.candidates.push(replacement_attempt.report);
            continue;
        }

        let incoming_link = PreparedLink {
            identity_key: primary_identity_key,
            avionics_model_id: primary_id,
            quantity: raw.quantity,
            source_notes: raw.source_evidence_text.clone(),
            source_confidence: Some("high".to_string()),
            configuration_action: raw.configuration_action.clone(),
            replaces_avionics_model_id: Some(replacement_id),
            replacement_identity_key: Some(replacement_identity_key),
        };
        if let Err(error) =
            merge_prepared_link_for_candidate(&mut prepared, incoming_link, &mut primary)
        {
            blocking_reasons.push(format!("candidate {candidate_index}: {error}"));
        }
        listing_report.candidates.push(primary.report);
        listing_report.candidates.push(replacement_attempt.report);
    }

    if let Err(error) = validate_prepared_links(&prepared) {
        blocking_reasons.push(format!("listing avionics action graph is invalid: {error}"));
    }
    listing_report.prepared_link_count = prepared.len();
    listing_report.accepted = prepared.len();
    if !blocking_reasons.is_empty() {
        listing_report.status = "blocked".to_string();
        listing_report.error = Some(blocking_reasons.join("; "));
        return listing_report;
    }
    listing_report.remaining_review_aspects = residual_aspects.len();
    if !apply {
        listing_report.status = "previewed".to_string();
        return listing_report;
    }

    let accepted_links = prepared
        .into_iter()
        .map(|link| AutomatedAvionicsLink {
            avionics_model_id: link.avionics_model_id,
            quantity: link.quantity,
            source_notes: link.source_notes,
            source_confidence: link.source_confidence,
            configuration_action: link.configuration_action,
            replaces_avionics_model_id: link.replaces_avionics_model_id,
        })
        .collect();
    let request = AutomatedReviewApplyRequest {
        listing_id: row.listing_id,
        plugin_submission_id: submission_id,
        expected_review_payload_sha256: row.review_payload_sha256.clone(),
        expected_rendered_html_sha256: row
            .rendered_html_sha256
            .clone()
            .expect("source validation requires a rendered HTML hash"),
        expected_faa_snapshot_id: faa_grounding.snapshot.id,
        expected_faa_source_record_sha256: faa_grounding.source_record_sha256,
        accepted_links,
        residual_aspects,
    };
    match apply_automated_avionics_review(db, &request).await {
        Ok(_) => {
            listing_report.status = "applied".to_string();
            listing_report.applied = true;
        }
        Err(error) => {
            listing_report.status = "blocked".to_string();
            listing_report.remaining_review_aspects = row.pending_aspect_count.max(0) as usize;
            listing_report.error = Some(format!(
                "automated review apply was rejected; the prior links and pending review were retained: {error}"
            ));
        }
    }
    listing_report
}

fn validate_pending_source_binding(row: &ListingSourceRow) -> Result<(i64, String), String> {
    let submission_id = row.submission_id.ok_or_else(|| {
        "pending review has no usable plugin submission; automated review requires exact retained source provenance"
            .to_string()
    })?;
    if submission_id <= 0 {
        return Err("pending review references an invalid plugin submission ID".to_string());
    }
    if row.submission_owner_user_id != Some(row.listing_owner_user_id) {
        return Err(format!(
            "plugin submission {submission_id} does not belong to listing {} owner",
            row.listing_id
        ));
    }
    let submission_source_url = row
        .submission_source_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("plugin submission {submission_id} has no retained source URL"))?;
    crate::extract::validate_source_url(submission_source_url).map_err(|error| {
        format!("plugin submission {submission_id} source URL is invalid: {error}")
    })?;
    let source_match = if row.submission_canonical_listing_id == Some(row.listing_id) {
        "canonical_listing_id"
    } else if row.submission_canonical_listing_id.is_none()
        && row.submission_source_url.as_deref().is_some()
        && row.submission_source_url == row.listing_source_url
    {
        "exact_source_url"
    } else {
        return Err(format!(
            "plugin submission {submission_id} is not canonically or exact-source bound to listing {}",
            row.listing_id
        ));
    };
    if !valid_sha256(&row.review_payload_sha256)
        || sha256_hex(row.review_payload_json.as_bytes()) != row.review_payload_sha256
    {
        return Err(
            "pending review payload does not match its stored SHA-256; restage before automation"
                .to_string(),
        );
    }
    let rendered_html = row
        .rendered_html
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            "pending review plugin submission has no retained rendered HTML".to_string()
        })?;
    let rendered_html_sha256 = row.rendered_html_sha256.as_deref().ok_or_else(|| {
        "pending review plugin submission has no rendered HTML SHA-256".to_string()
    })?;
    if !valid_sha256(rendered_html_sha256)
        || sha256_hex(rendered_html.as_bytes()) != rendered_html_sha256
    {
        return Err(
            "retained rendered HTML does not match its signed SHA-256; refusing automated review"
                .to_string(),
        );
    }
    Ok((submission_id, source_match.to_string()))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn merge_prepared_link_for_candidate(
    prepared: &mut Vec<PreparedLink>,
    incoming: PreparedLink,
    attempt: &mut IdentityAttempt,
) -> Result<(), String> {
    match merge_or_push_prepared_link(prepared, incoming) {
        Ok(true) => {
            attempt.report.reason.push_str(
                "; coalesced with another independently resolved capability row for the same verified product",
            );
            Ok(())
        }
        Ok(false) => Ok(()),
        Err(error) => {
            attempt.report.status = "error".to_string();
            attempt.report.reason = error.clone();
            Err(error)
        }
    }
}

fn mark_weak_listing_evidence(report: &mut AvionicsRepopulationCandidateReport) {
    report.status = "unresolved".to_string();
    report.reason = format!(
        "the product identity was verified, but the listing occurrence requires exact high-confidence source evidence; {}",
        report.reason
    );
}

fn identity_is_approved(attempt: &IdentityAttempt) -> bool {
    attempt.approved_id.is_some() && attempt.identity_key.is_some()
}

fn can_automatically_accept(attempt: &IdentityAttempt, source_confidence: Option<&str>) -> bool {
    identity_is_approved(attempt) && source_confidence == Some("high")
}

async fn attach_unique_exact_review_candidate(
    db: &AppDb,
    mut aspect: PendingReviewAspect,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
) -> Result<PendingReviewAspect, String> {
    if aspect
        .suggested_product
        .as_ref()
        .and_then(|product| product.id)
        .is_some()
        || aspect
            .proposed_product
            .as_ref()
            .and_then(|product| product.id)
            .is_some()
    {
        return Ok(aspect);
    }
    let Some(candidate) =
        unique_exact_avionics_review_candidate(db, manufacturer, model, avionics_types)
            .await
            .map_err(|error| error.to_string())?
    else {
        return Ok(aspect);
    };
    let mut product = if candidate.catalog_status == "approved" {
        ReviewProduct::verified(
            candidate.id,
            candidate.manufacturer,
            candidate.model,
            candidate.avionics_types,
        )
    } else {
        ReviewProduct::unreviewed_catalog_candidate(
            candidate.id,
            candidate.manufacturer,
            candidate.model,
            candidate.avionics_types,
        )
    };
    if !candidate.manufacturer_identifier_kind.trim().is_empty()
        && !candidate.manufacturer_identifier.trim().is_empty()
    {
        product = product.with_stable_identifier(
            candidate.manufacturer_identifier_kind,
            candidate.manufacturer_identifier,
        );
    }
    if candidate.catalog_status == "approved" {
        aspect.suggested_product = Some(product);
    } else {
        aspect.proposed_product = Some(product);
    }
    Ok(aspect)
}

fn input_error_aspects(
    candidate_index: usize,
    raw: &ParsedAvionics,
    issue: &str,
) -> Vec<PendingReviewAspect> {
    let mut safe = raw.clone();
    safe.quantity = safe.quantity.max(1);
    safe.source_confidence = safe
        .source_confidence
        .filter(|value| matches!(value.as_str(), "high" | "medium" | "low"));
    if safe.avionics_types.is_empty() {
        safe.avionics_types.push("Unknown".to_string());
    }
    if !matches!(
        safe.configuration_action.as_str(),
        "installed" | "replaces" | "removes"
    ) || (safe.configuration_action == "installed" && safe.replaces.is_some())
    {
        safe.configuration_action = "installed".to_string();
        safe.replaces = None;
    }
    if matches!(safe.configuration_action.as_str(), "replaces" | "removes")
        && safe.replaces.is_none()
    {
        safe.replaces = Some(crate::models::ParsedAvionicsReference {
            manufacturer: "Unknown".to_string(),
            model: format!(
                "{} target",
                if safe.configuration_action == "removes" {
                    "Removal"
                } else {
                    "Replacement"
                }
            ),
            avionics_types: safe.avionics_types.clone(),
        });
    }
    let reason =
        format!("automated review could not safely interpret this listing observation: {issue}");
    if safe.configuration_action == "installed" {
        vec![primary_residual_aspect(
            candidate_index,
            &safe,
            reason,
            None,
            None,
        )]
    } else {
        dependent_residual_aspects(
            candidate_index,
            &safe,
            &reason,
            None,
            "automated review could not safely interpret this listing observation's relationship target",
            None,
        )
    }
}

fn dependent_residual_aspects(
    candidate_index: usize,
    raw: &ParsedAvionics,
    primary_reason: &str,
    primary_suggested_product: Option<ReviewProduct>,
    replacement_reason: &str,
    replacement_suggested_product: Option<ReviewProduct>,
) -> Vec<PendingReviewAspect> {
    let replacement_id = ReviewAspectId::String(format!("avionics:{candidate_index}:replacement"));
    vec![
        primary_residual_aspect(
            candidate_index,
            raw,
            primary_reason.to_string(),
            primary_suggested_product,
            Some(replacement_id.clone()),
        ),
        replacement_residual_aspect(
            candidate_index,
            raw,
            replacement_reason.to_string(),
            replacement_suggested_product,
        ),
    ]
}

fn primary_residual_aspect(
    candidate_index: usize,
    raw: &ParsedAvionics,
    reason: String,
    suggested_product: Option<ReviewProduct>,
    replacement_aspect_id: Option<ReviewAspectId>,
) -> PendingReviewAspect {
    PendingReviewAspect {
        id: ReviewAspectId::String(format!("avionics:{candidate_index}:primary")),
        kind: "avionics".to_string(),
        label: observation_label(&raw.manufacturer, &raw.model),
        observed_text: observation_text(
            &raw.manufacturer,
            &raw.model,
            &raw.avionics_types,
            raw.quantity,
            &raw.configuration_action,
        ),
        required: true,
        reason,
        suggested_product,
        proposed_product: Some(ReviewProduct::proposed(
            observation_component(&raw.manufacturer, "Unknown manufacturer"),
            observation_component(&raw.model, "Unknown model"),
            raw.avionics_types.clone(),
        )),
        allowed_actions: vec![
            ReviewAction::UseVerifiedProduct,
            ReviewAction::CreateVerifiedProduct,
            ReviewAction::Discard,
        ],
        quantity: raw.quantity.max(1),
        configuration_action: raw.configuration_action.clone(),
        source_evidence_text: raw.source_evidence_text.clone(),
        source_confidence: raw.source_confidence.clone(),
        replaces_product_id: None,
        replacement_aspect_id,
        covered_associations: Vec::new(),
        reuse_attestation_target_id: None,
    }
}

fn replacement_residual_aspect(
    candidate_index: usize,
    raw: &ParsedAvionics,
    reason: String,
    suggested_product: Option<ReviewProduct>,
) -> PendingReviewAspect {
    let replacement = raw
        .replaces
        .as_ref()
        .expect("dependent residual requires a replacement observation");
    PendingReviewAspect {
        id: ReviewAspectId::String(format!("avionics:{candidate_index}:replacement")),
        kind: "avionics".to_string(),
        label: observation_label(&replacement.manufacturer, &replacement.model),
        observed_text: observation_text(
            &replacement.manufacturer,
            &replacement.model,
            &replacement.avionics_types,
            1,
            "installed",
        ),
        required: true,
        reason,
        suggested_product,
        proposed_product: Some(ReviewProduct::proposed(
            observation_component(&replacement.manufacturer, "Unknown manufacturer"),
            observation_component(&replacement.model, "Unknown model"),
            replacement.avionics_types.clone(),
        )),
        allowed_actions: vec![
            ReviewAction::UseVerifiedProduct,
            ReviewAction::CreateVerifiedProduct,
            ReviewAction::Discard,
        ],
        quantity: 1,
        configuration_action: "installed".to_string(),
        source_evidence_text: raw.source_evidence_text.clone(),
        source_confidence: raw.source_confidence.clone(),
        replaces_product_id: None,
        replacement_aspect_id: None,
        covered_associations: Vec::new(),
        reuse_attestation_target_id: None,
    }
}

fn observation_label(manufacturer: &str, model: &str) -> String {
    let label = format!("{} {}", manufacturer.trim(), model.trim())
        .trim()
        .to_string();
    if label.is_empty() {
        "Unknown avionics observation".to_string()
    } else {
        label
    }
}

fn observation_component(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn observation_text(
    manufacturer: &str,
    model: &str,
    capabilities: &[String],
    quantity: i64,
    configuration_action: &str,
) -> String {
    format!(
        "{} · {} · quantity {} · {}",
        observation_label(manufacturer, model),
        if capabilities.is_empty() {
            "unknown capability".to_string()
        } else {
            capabilities.join(", ")
        },
        quantity.max(1),
        configuration_action
    )
}

fn merge_or_push_prepared_link(
    prepared: &mut Vec<PreparedLink>,
    incoming: PreparedLink,
) -> Result<bool, String> {
    if let Some(index) = prepared
        .iter()
        .position(|link| link.identity_key == incoming.identity_key)
    {
        merge_duplicate_link(&mut prepared[index], &incoming)?;
        return Ok(true);
    }
    prepared.push(incoming);
    Ok(false)
}

fn merge_duplicate_link(
    existing: &mut PreparedLink,
    incoming: &PreparedLink,
) -> Result<(), String> {
    let same_action = existing.configuration_action == incoming.configuration_action;
    let compatible_replacement = match existing.configuration_action.as_str() {
        "installed" => {
            existing.replaces_avionics_model_id.is_none()
                && incoming.replaces_avionics_model_id.is_none()
        }
        "replaces" | "removes" => {
            existing.replacement_identity_key.is_some()
                && existing.replacement_identity_key == incoming.replacement_identity_key
        }
        _ => false,
    };
    if !same_action || !compatible_replacement {
        return Err(format!(
            "catalog id {} resolved from multiple raw rows with conflicting action or replacement semantics",
            existing.avionics_model_id
        ));
    }
    existing.quantity = existing.quantity.max(incoming.quantity);
    existing.source_notes = combine_source_notes(
        existing.source_notes.as_deref(),
        incoming.source_notes.as_deref(),
    );
    existing.source_confidence = conservative_confidence(
        existing.source_confidence.as_deref(),
        incoming.source_confidence.as_deref(),
    );
    Ok(())
}

fn combine_source_notes(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left, right) {
        (None, None) => None,
        (Some(value), None) | (None, Some(value)) => Some(value.to_string()),
        (Some(left), Some(right)) if left == right => Some(left.to_string()),
        (Some(left), Some(right)) => Some(format!("{left}\n{right}")),
    }
}

fn conservative_confidence(left: Option<&str>, right: Option<&str>) -> Option<String> {
    let rank = |confidence: &str| match confidence {
        "low" => 0,
        "medium" => 1,
        "high" => 2,
        _ => -1,
    };
    match (left, right) {
        (Some(left), Some(right)) => Some(
            if rank(left) <= rank(right) {
                left
            } else {
                right
            }
            .to_string(),
        ),
        _ => None,
    }
}

/// Repair one narrow extraction-shape error without treating repeated prose as
/// extra hardware. Gemini occasionally emits one row per radio position even
/// though the listing extractor contract requires one product row with a
/// quantity. We combine only exact raw installed identities whose independently
/// retained source excerpts prove distinct numbered positions (for example an
/// unnumbered first row plus `#2`, or `#1` plus `#2`).
///
/// Manufacturer and model spellings remain byte-for-byte equal and capability
/// labels are compared only as an order-independent set of exact strings. Row
/// count never determines quantity, and ambiguous duplicates remain separate.
fn coalesce_explicit_numbered_instances(
    observations: Vec<ParsedAvionics>,
    listing_context: &ListingEvidenceContext,
) -> Vec<ParsedAvionics> {
    let mut consumed = vec![false; observations.len()];
    let mut coalesced = Vec::with_capacity(observations.len());

    for seed_index in 0..observations.len() {
        if consumed[seed_index] {
            continue;
        }
        let Some((mut selected_indices, highest_ordinal)) =
            explicit_numbered_instance_group(&observations, seed_index, listing_context)
        else {
            consumed[seed_index] = true;
            coalesced.push(observations[seed_index].clone());
            continue;
        };

        selected_indices.sort_unstable();
        let mut merged = observations[selected_indices[0]].clone();
        let mut evidence = Vec::with_capacity(selected_indices.len());
        let mut quantity = highest_ordinal;
        let mut confidence = merged.source_confidence.clone();
        for (position, index) in selected_indices.iter().copied().enumerate() {
            let observation = &observations[index];
            consumed[index] = true;
            quantity = quantity.max(observation.quantity);
            let excerpt = observation
                .source_evidence_text
                .as_deref()
                .expect("numbered-instance group requires retained evidence")
                .trim();
            if !evidence.iter().any(|existing| existing == excerpt) {
                evidence.push(excerpt.to_string());
            }
            if position > 0 {
                confidence = conservative_confidence(
                    confidence.as_deref(),
                    observation.source_confidence.as_deref(),
                );
            }
        }
        merged.quantity = quantity;
        merged.source_evidence_text = Some(evidence.join("\n"));
        merged.source_confidence = confidence;
        coalesced.push(merged);
    }

    coalesced
}

fn explicit_numbered_instance_group(
    observations: &[ParsedAvionics],
    seed_index: usize,
    listing_context: &ListingEvidenceContext,
) -> Option<(Vec<usize>, i64)> {
    let seed = observations.get(seed_index)?;
    if !eligible_installed_observation(seed) {
        return None;
    }

    let mut unnumbered = Vec::<usize>::new();
    let mut numbered = Vec::<(i64, usize)>::new();
    for (index, observation) in observations.iter().enumerate().skip(seed_index) {
        if !exact_installed_observation_match(seed, observation) {
            continue;
        }
        let evidence = observation.source_evidence_text.as_deref()?.trim();
        if evidence.is_empty()
            || !listing_context
                .for_candidate(
                    &observation.manufacturer,
                    &observation.model,
                    Some(evidence),
                )
                .contains(evidence)
        {
            continue;
        }
        match explicit_unit_ordinal(evidence, &observation.model) {
            Some(ordinal)
                if !numbered
                    .iter()
                    .any(|(existing_ordinal, _)| *existing_ordinal == ordinal) =>
            {
                numbered.push((ordinal, index));
            }
            Some(_) => {}
            None if !unnumbered.iter().any(|existing_index| {
                observations[*existing_index]
                    .source_evidence_text
                    .as_deref()
                    == observation.source_evidence_text.as_deref()
            }) =>
            {
                unnumbered.push(index);
            }
            None => {}
        }
    }
    numbered.sort_unstable_by_key(|(ordinal, _)| *ordinal);
    let highest_ordinal = numbered.last().map(|(ordinal, _)| *ordinal)?;
    if highest_ordinal < 2 {
        return None;
    }

    let mut selected = numbered.iter().map(|(_, index)| *index).collect::<Vec<_>>();
    if selected.len() < 2 {
        selected.push(*unnumbered.first()?);
    }
    if !selected.contains(&seed_index) {
        return None;
    }
    Some((selected, highest_ordinal))
}

fn eligible_installed_observation(observation: &ParsedAvionics) -> bool {
    observation.configuration_action == "installed"
        && observation.replaces.is_none()
        && observation.quantity > 0
        && !observation.manufacturer.is_empty()
        && !observation.model.is_empty()
        && !observation.avionics_types.is_empty()
}

fn exact_installed_observation_match(left: &ParsedAvionics, right: &ParsedAvionics) -> bool {
    if !eligible_installed_observation(right)
        || left.manufacturer != right.manufacturer
        || left.model != right.model
        || left.configuration_action != right.configuration_action
        || left.replaces != right.replaces
    {
        return false;
    }
    exact_capability_set(&left.avionics_types) == exact_capability_set(&right.avionics_types)
}

fn exact_capability_set(capabilities: &[String]) -> Vec<&str> {
    let mut capabilities = capabilities.iter().map(String::as_str).collect::<Vec<_>>();
    capabilities.sort_unstable();
    capabilities.dedup();
    capabilities
}

fn explicit_unit_ordinal(evidence: &str, model: &str) -> Option<i64> {
    if model.is_empty() {
        return None;
    }
    let evidence_lower = evidence.to_ascii_lowercase();
    let model_lower = model.to_ascii_lowercase();
    evidence_lower
        .match_indices(&model_lower)
        .filter(|(model_start, _)| {
            identity_span_has_boundaries(evidence, *model_start, *model_start + model_lower.len())
        })
        .find_map(|(model_start, _)| {
            let model_end = model_start + model_lower.len();
            let suffix = evidence.get(model_end..)?;
            let bounded_suffix = suffix.get(..suffix.len().min(96)).unwrap_or(suffix);
            let hash_offset = bounded_suffix.find('#')?;
            let after_hash = bounded_suffix.get(hash_offset + 1..)?.trim_start();
            let digit_count = after_hash.bytes().take_while(u8::is_ascii_digit).count();
            if digit_count == 0
                || after_hash
                    .as_bytes()
                    .get(digit_count)
                    .is_some_and(u8::is_ascii_alphanumeric)
            {
                return None;
            }
            let ordinal = after_hash.get(..digit_count)?.parse::<i64>().ok()?;
            (1..=64).contains(&ordinal).then_some(ordinal)
        })
}

fn identity_request(
    row: &ListingSourceRow,
    source_url: Option<&str>,
    listing_context: &ListingEvidenceContext,
    identity: &IdentityInput<'_>,
    source_evidence_text: Option<&str>,
) -> AvionicsIdentityRequest {
    AvionicsIdentityRequest {
        aircraft_manufacturer: row.aircraft_manufacturer.clone(),
        aircraft_model: row.aircraft_model.clone(),
        aircraft_variant: row.aircraft_variant.clone(),
        model_year: row.model_year,
        source_url: source_url.unwrap_or_default().to_string(),
        listing_context: listing_context.for_candidate(
            identity.manufacturer,
            identity.model,
            source_evidence_text,
        ),
        requires_listing_evidence: true,
        authoritative_direct_source_urls: Vec::new(),
        authoritative_identity_anchors: Vec::new(),
        manufacturer: identity.manufacturer.to_string(),
        model: identity.model.to_string(),
        avionics_types: identity.avionics_types.to_vec(),
        quantity: identity.quantity,
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_identity_attempt(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    row: &ListingSourceRow,
    source_url: Option<&str>,
    listing_context: &ListingEvidenceContext,
    candidate_index: usize,
    role: &str,
    identity: IdentityInput<'_>,
    configuration_action: &str,
    source_evidence_text: Option<&str>,
    source_confidence: Option<&str>,
    catalog_statuses: &mut HashMap<i64, String>,
) -> IdentityAttempt {
    let request = identity_request(
        row,
        source_url,
        listing_context,
        &identity,
        source_evidence_text,
    );
    let outcome = if apply {
        resolve_avionics_identity(db, extractor, &request).await
    } else {
        preview_avionics_identity(db, extractor, &request).await
    };
    match outcome {
        Ok(AvionicsIdentityOutcome::Approved(approved)) => {
            let suggested_product = Some(review_product_from_approved(&approved));
            let stored_status = catalog_statuses.get(&approved.id).map(String::as_str);
            let identity_key =
                if requires_persisted_graph_identity(apply, approved.id, stored_status) {
                    match load_approved_graph_identity_key(db, approved.id).await {
                        Ok(key) => Some(key),
                        Err(error) => {
                            return IdentityAttempt {
                                report: outcome_report(
                                    candidate_index,
                                    role,
                                    &identity,
                                    configuration_action,
                                    source_evidence_text,
                                    source_confidence,
                                    "error",
                                    Some(approved.id),
                                    Some(approved.manufacturer),
                                    Some(approved.model),
                                    approved.avionics_types,
                                    error.to_string(),
                                ),
                                approved_id: None,
                                identity_key: None,
                                suggested_product,
                            };
                        }
                    }
                } else {
                    // A dry-run promotion deliberately leaves the legacy row
                    // unreviewed, so it cannot yet have an approved graph
                    // identity. Keep the preview useful with an ephemeral key;
                    // apply mode must persist and reload the graph identity.
                    preview_approved_identity_key(&approved)
                };
            approved_attempt(
                apply,
                candidate_index,
                role,
                &identity,
                configuration_action,
                source_evidence_text,
                source_confidence,
                approved,
                identity_key,
                catalog_statuses,
            )
        }
        Ok(AvionicsIdentityOutcome::Rejected { reason }) => IdentityAttempt {
            report: outcome_report(
                candidate_index,
                role,
                &identity,
                configuration_action,
                source_evidence_text,
                source_confidence,
                "rejected",
                None,
                None,
                None,
                Vec::new(),
                reason,
            ),
            approved_id: None,
            identity_key: None,
            suggested_product: None,
        },
        Ok(AvionicsIdentityOutcome::Unresolved { reason }) => IdentityAttempt {
            report: outcome_report(
                candidate_index,
                role,
                &identity,
                configuration_action,
                source_evidence_text,
                source_confidence,
                "unresolved",
                None,
                None,
                None,
                Vec::new(),
                reason,
            ),
            approved_id: None,
            identity_key: None,
            suggested_product: None,
        },
        Err(error) => IdentityAttempt {
            report: outcome_report(
                candidate_index,
                role,
                &identity,
                configuration_action,
                source_evidence_text,
                source_confidence,
                "error",
                None,
                None,
                None,
                Vec::new(),
                error.to_string(),
            ),
            approved_id: None,
            identity_key: None,
            suggested_product: None,
        },
    }
}

fn requires_persisted_graph_identity(
    apply: bool,
    avionics_model_id: i64,
    stored_status: Option<&str>,
) -> bool {
    avionics_model_id > 0 && (apply || stored_status == Some("approved"))
}

#[allow(clippy::too_many_arguments)]
fn approved_attempt(
    apply: bool,
    candidate_index: usize,
    role: &str,
    input: &IdentityInput<'_>,
    configuration_action: &str,
    source_evidence_text: Option<&str>,
    source_confidence: Option<&str>,
    approved: ApprovedAvionicsIdentity,
    identity_key: Option<String>,
    catalog_statuses: &mut HashMap<i64, String>,
) -> IdentityAttempt {
    let suggested_product = Some(review_product_from_approved(&approved));
    let status = if approved.id == 0 {
        "new"
    } else {
        match catalog_statuses.get(&approved.id).map(String::as_str) {
            Some("approved") => "existing",
            Some("unreviewed") => "promoted",
            Some(_) => "error",
            None => "new",
        }
    };
    if apply && approved.id <= 0 {
        return IdentityAttempt {
            report: outcome_report(
                candidate_index,
                role,
                input,
                configuration_action,
                source_evidence_text,
                source_confidence,
                "error",
                None,
                Some(approved.manufacturer),
                Some(approved.model),
                approved.avionics_types,
                "apply-mode resolver did not return a positive approved catalog id".to_string(),
            ),
            approved_id: None,
            identity_key: None,
            suggested_product,
        };
    }
    let Some(identity_key) = identity_key else {
        return IdentityAttempt {
            report: outcome_report(
                candidate_index,
                role,
                input,
                configuration_action,
                source_evidence_text,
                source_confidence,
                "error",
                None,
                Some(approved.manufacturer),
                Some(approved.model),
                approved.avionics_types,
                "approved identity has no stable manufacturer/identifier product key".to_string(),
            ),
            approved_id: None,
            identity_key: None,
            suggested_product,
        };
    };
    if apply && approved.id > 0 {
        catalog_statuses.insert(approved.id, "approved".to_string());
    }
    let approved_id = (approved.id > 0).then_some(approved.id);
    IdentityAttempt {
        report: outcome_report(
            candidate_index,
            role,
            input,
            configuration_action,
            source_evidence_text,
            source_confidence,
            status,
            approved_id,
            Some(approved.manufacturer),
            Some(approved.model),
            approved.avionics_types,
            approved.reason,
        ),
        // A dry-run `new` identity deliberately has id 0, but it is still a
        // complete preview and therefore may contribute to prepared counts.
        approved_id: if apply {
            approved_id
        } else {
            Some(approved.id)
        },
        identity_key: Some(identity_key),
        suggested_product,
    }
}

fn review_product_from_approved(approved: &ApprovedAvionicsIdentity) -> ReviewProduct {
    ReviewProduct {
        id: (approved.id > 0).then_some(approved.id),
        manufacturer: approved.manufacturer.clone(),
        model: approved.model.clone(),
        capabilities: approved.avionics_types.clone(),
        stable_identifier: Some(StableIdentifier {
            kind: approved.manufacturer_identifier_kind.clone(),
            value: approved.manufacturer_identifier.clone(),
        }),
        identity_source_url: Some(approved.evidence_url.clone()),
        identity_source_title: Some(approved.evidence_title.clone()),
        identity_evidence_text: Some(approved.evidence.clone()),
    }
}

fn preview_approved_identity_key(approved: &ApprovedAvionicsIdentity) -> Option<String> {
    let identity = preview_avionics_product_key(&approved.manufacturer, &approved.model);
    if identity == "preview:\u{1f}" {
        return None;
    }
    Some(identity)
}

async fn load_approved_graph_identity_key(
    db: &AppDb,
    avionics_model_id: i64,
) -> RepopulationResult<String> {
    let sql = db.sql(
        r#"
        SELECT avionics_manufacturer_identity_id, canonical_product_key
        FROM avionics_approved_product_graph_identities
        WHERE avionics_model_id = ?
        "#,
    );
    let identity: Option<(i64, String)> = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as(&sql)
                .bind(avionics_model_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as(&sql)
                .bind(avionics_model_id)
                .fetch_optional(pool)
                .await?
        }
    };
    let (manufacturer_identity_id, product_key) = identity.ok_or_else(|| {
        AvionicsRepopulationError::Validation(format!(
            "approved catalog id {avionics_model_id} has no stable product identity"
        ))
    })?;
    approved_avionics_product_key(manufacturer_identity_id, &product_key)
        .map_err(AvionicsRepopulationError::Validation)
}

#[allow(clippy::too_many_arguments)]
fn outcome_report(
    candidate_index: usize,
    role: &str,
    identity: &IdentityInput<'_>,
    configuration_action: &str,
    source_evidence_text: Option<&str>,
    source_confidence: Option<&str>,
    status: &str,
    catalog_id: Option<i64>,
    canonical_manufacturer: Option<String>,
    canonical_model: Option<String>,
    canonical_types: Vec<String>,
    reason: String,
) -> AvionicsRepopulationCandidateReport {
    AvionicsRepopulationCandidateReport {
        candidate_index,
        role: role.to_string(),
        manufacturer: identity.manufacturer.to_string(),
        model: identity.model.to_string(),
        avionics_types: identity.avionics_types.to_vec(),
        quantity: identity.quantity,
        configuration_action: configuration_action.to_string(),
        source_evidence_text: source_evidence_text.map(ToString::to_string),
        source_confidence: source_confidence.map(ToString::to_string),
        resolution_attempted: true,
        status: status.to_string(),
        catalog_id,
        canonical_manufacturer,
        canonical_model,
        canonical_types,
        reason,
    }
}

#[allow(clippy::too_many_arguments)]
fn input_error_report(
    candidate_index: usize,
    role: &str,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    quantity: i64,
    configuration_action: &str,
    source_evidence_text: Option<String>,
    source_confidence: Option<String>,
    reason: &str,
) -> AvionicsRepopulationCandidateReport {
    AvionicsRepopulationCandidateReport {
        candidate_index,
        role: role.to_string(),
        manufacturer: manufacturer.to_string(),
        model: model.to_string(),
        avionics_types: avionics_types.to_vec(),
        quantity,
        configuration_action: configuration_action.to_string(),
        source_evidence_text,
        source_confidence,
        resolution_attempted: false,
        status: "error".to_string(),
        catalog_id: None,
        canonical_manufacturer: None,
        canonical_model: None,
        canonical_types: Vec::new(),
        reason: reason.to_string(),
    }
}

fn raw_candidate_issue(raw: &ParsedAvionics) -> Option<String> {
    if raw.avionics_types.is_empty()
        || raw
            .avionics_types
            .iter()
            .any(|avionics_type| avionics_type.trim().is_empty())
    {
        return Some("at least one non-empty avionics capability type is required".to_string());
    }
    if raw.quantity < 1 {
        return Some("quantity must be at least 1".to_string());
    }
    if !matches!(
        raw.configuration_action.as_str(),
        "installed" | "replaces" | "removes"
    ) {
        return Some(format!(
            "unsupported configuration_action {}",
            raw.configuration_action
        ));
    }
    if let Some(confidence) = raw.source_confidence.as_deref() {
        if !matches!(confidence, "high" | "medium" | "low") {
            return Some(format!(
                "source_confidence must be high, medium, low, or absent; got {confidence}"
            ));
        }
    }
    match raw.configuration_action.as_str() {
        "installed" if raw.replaces.is_some() => {
            Some("installed candidate must not include a replacement identity".to_string())
        }
        "replaces" | "removes" if raw.replaces.is_none() => Some(format!(
            "{} candidate requires a concrete replacement identity",
            raw.configuration_action
        )),
        "replaces" | "removes"
            if raw.replaces.as_ref().is_some_and(|replacement| {
                replacement.avionics_types.is_empty()
                    || replacement
                        .avionics_types
                        .iter()
                        .any(|avionics_type| avionics_type.trim().is_empty())
            }) =>
        {
            Some("replacement identity requires at least one capability type".to_string())
        }
        _ => None,
    }
}

enum RetainedAvionicsSource {
    Current(Vec<ParsedAvionics>),
    RequiresReextraction { reason: String },
}

fn retained_avionics_source(raw_json: Option<&str>) -> RetainedAvionicsSource {
    let Some(raw_json) = raw_json.filter(|raw_json| !raw_json.trim().is_empty()) else {
        return RetainedAvionicsSource::RequiresReextraction {
            reason: "the retained plugin submission has no extracted listing JSON".to_string(),
        };
    };
    match parse_raw_avionics(raw_json) {
        Ok(avionics) if avionics.is_empty() => {
            RetainedAvionicsSource::RequiresReextraction {
                reason: "the retained plugin extraction contains no avionics capability arrays"
                    .to_string(),
            }
        }
        Ok(avionics) => RetainedAvionicsSource::Current(avionics),
        Err(error) => RetainedAvionicsSource::RequiresReextraction {
            reason: format!(
                "the retained plugin extraction is not compatible with the current capability-array schema: {error}"
            ),
        },
    }
}

fn prepare_stored_listing_text(source_url: &str, rendered_html: &str) -> Result<String, String> {
    crate::extract::validate_source_url(source_url)
        .map_err(|error| format!("retained source URL is invalid: {error}"))?;
    let listing_text = clean_listing_html(rendered_html);
    if listing_text.trim().is_empty() {
        return Err("retained rendered_html contains no usable listing text".to_string());
    }
    Ok(listing_text)
}

async fn reextract_avionics(
    extractor: &GeminiListingExtractor,
    listing_text: &str,
) -> Result<Vec<ParsedAvionics>, String> {
    let extracted = extractor
        .extract(listing_text)
        .await
        .map_err(|error| format!("Gemini listing extraction request failed: {error}"))?;
    parse_raw_avionics_value(&extracted).map_err(|error| {
        format!(
            "Gemini returned output incompatible with the current capability-array schema: {error}"
        )
    })
}

fn parse_raw_avionics(raw_json: &str) -> Result<Vec<ParsedAvionics>, String> {
    let value: Value =
        serde_json::from_str(raw_json).map_err(|error| format!("invalid JSON: {error}"))?;
    parse_raw_avionics_value(&value)
}

fn parse_raw_avionics_value(value: &Value) -> Result<Vec<ParsedAvionics>, String> {
    let values = value
        .get("avionics")
        .and_then(Value::as_array)
        .ok_or_else(|| "top-level avionics array is missing".to_string())?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            validate_capability_array(value, &format!("avionics[{index}]"))?;
            if let Some(replacement) = value.get("replaces").filter(|value| !value.is_null()) {
                validate_capability_array(replacement, &format!("avionics[{index}].replaces"))?;
            }
            serde_json::from_value::<ParsedAvionics>(value.clone())
                .map_err(|error| format!("avionics[{index}] is invalid: {error}"))
        })
        .collect()
}

fn validate_capability_array(value: &Value, path: &str) -> Result<(), String> {
    let Some(types) = value.get("types").and_then(Value::as_array) else {
        return Err(format!(
            "{path}.types must be a non-empty array; scalar type payloads are intentionally unsupported"
        ));
    };
    if types.is_empty()
        || types.iter().any(|avionics_type| {
            avionics_type
                .as_str()
                .is_none_or(|value| value.trim().is_empty())
        })
    {
        return Err(format!(
            "{path}.types must contain at least one non-empty string"
        ));
    }
    Ok(())
}

async fn load_listing_sources(
    db: &AppDb,
    scope: &AvionicsRepopulationScope,
) -> RepopulationResult<ListingSourcePage> {
    let predicate = if scope.listing_id.is_some() {
        "WHERE listing.id = ?"
    } else if scope.after_listing_id.is_some() {
        "WHERE listing.id > ?"
    } else {
        ""
    };
    let sql = format!(
        r#"
        SELECT
          listing.id AS listing_id,
          listing.created_by_user_id AS listing_owner_user_id,
          listing.source_url AS listing_source_url,
          aircraft_mfr.name AS aircraft_manufacturer,
          aircraft_model.name AS aircraft_model,
          variant.name AS aircraft_variant,
          listing.model_year,
          (
            SELECT COUNT(*)
            FROM aircraft_sale_listing_avionics old_link
            WHERE old_link.aircraft_sale_listing_id = listing.id
          ) AS old_link_count,
          pending.pending_aspect_count,
          pending.review_payload_json,
          pending.review_payload_sha256,
          submission.id AS submission_id,
          submission.user_id AS submission_owner_user_id,
          submission.canonical_listing_id AS submission_canonical_listing_id,
          submission.source_url AS submission_source_url,
          submission.rendered_html,
          submission.rendered_html_sha256,
          submission.extracted_listing_json,
          submission.extraction_error AS submission_extraction_error
        FROM aircraft_sale_listings listing
        JOIN aircraft_sale_listing_pending_reviews pending
          ON pending.listing_id = listing.id
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models aircraft_model
          ON aircraft_model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers aircraft_mfr
          ON aircraft_mfr.id = aircraft_model.aircraft_manufacturer_id
        LEFT JOIN plugin_submissions submission
          ON submission.id = pending.plugin_submission_id
        {predicate}
        ORDER BY listing.id
        LIMIT ?
        "#
    );
    let sql = db.sql(&sql);
    let fetch_limit = scope.limit.saturating_add(1);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let query = sqlx::query_as::<_, ListingSourceRow>(&sql);
            if let Some(listing_id) = scope.listing_id {
                query
                    .bind(listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await?
            } else if let Some(after_listing_id) = scope.after_listing_id {
                query
                    .bind(after_listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await?
            } else {
                query.bind(fetch_limit).fetch_all(pool).await?
            }
        }
        DatabaseBackend::Postgres(pool) => {
            let query = sqlx::query_as::<_, ListingSourceRow>(&sql);
            if let Some(listing_id) = scope.listing_id {
                query
                    .bind(listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await?
            } else if let Some(after_listing_id) = scope.after_listing_id {
                query
                    .bind(after_listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await?
            } else {
                query.bind(fetch_limit).fetch_all(pool).await?
            }
        }
    };
    let mut rows = rows;
    let has_more = scope.listing_id.is_none() && rows.len() > scope.limit as usize;
    rows.truncate(scope.limit as usize);
    Ok(ListingSourcePage { rows, has_more })
}

async fn load_catalog_statuses(db: &AppDb) -> RepopulationResult<HashMap<i64, String>> {
    let sql = db.sql("SELECT id, catalog_status FROM avionics_models ORDER BY id");
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, CatalogStatusRow>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, CatalogStatusRow>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows
        .into_iter()
        .map(|row| (row.id, row.catalog_status))
        .collect())
}

fn validate_prepared_links(links: &[PreparedLink]) -> RepopulationResult<()> {
    let mut canonical_actions = Vec::with_capacity(links.len());
    for link in links {
        match link.configuration_action.as_str() {
            "installed"
                if link.replaces_avionics_model_id.is_none()
                    && link.replacement_identity_key.is_none() => {}
            "replaces"
                if link.replaces_avionics_model_id.is_some()
                    && link
                        .replacement_identity_key
                        .as_ref()
                        .is_some_and(|target| {
                            target != &link.identity_key
                                && (link.avionics_model_id == 0
                                    || link.replaces_avionics_model_id == Some(0)
                                    || link.replaces_avionics_model_id
                                        != Some(link.avionics_model_id))
                        }) => {}
            "removes"
                if link.replaces_avionics_model_id.is_some()
                    && link.replacement_identity_key.as_ref() == Some(&link.identity_key)
                    && (link.avionics_model_id == 0
                        || link.replaces_avionics_model_id == Some(0)
                        || link.replaces_avionics_model_id == Some(link.avionics_model_id)) => {}
            _ => {
                return Err(AvionicsRepopulationError::Validation(format!(
                    "catalog id {} has invalid {} subject/target semantics",
                    link.avionics_model_id, link.configuration_action
                )))
            }
        }
        canonical_actions.push(CanonicalAvionicsAction::new(
            link.identity_key.clone(),
            link.configuration_action.clone(),
            link.replacement_identity_key.clone(),
        ));
    }
    validate_canonical_avionics_actions(&canonical_actions)
        .map_err(AvionicsRepopulationError::Validation)?;
    Ok(())
}

fn summarize(listings: &[AvionicsRepopulationListingReport]) -> AvionicsRepopulationSummary {
    let mut summary = AvionicsRepopulationSummary {
        listings_selected: listings.len(),
        ..AvionicsRepopulationSummary::default()
    };
    for listing in listings {
        summary.accepted += listing.accepted;
        summary.safely_discarded += listing.safely_discarded;
        summary.remaining_review_aspects += listing.remaining_review_aspects;
        summary.listings_reextraction_required += usize::from(listing.reextraction_required);
        summary.listing_reextraction_attempts += usize::from(listing.reextraction_attempted);
        summary.listings_reextracted += usize::from(listing.reextraction_succeeded);
        summary.listing_reextraction_errors += usize::from(listing.reextraction_error.is_some());
        match listing.status.as_str() {
            "faa_rejected" => summary.listings_faa_rejected += 1,
            "previewed" => summary.listings_previewed += 1,
            "applied" => summary.listings_applied += 1,
            "blocked" => summary.listings_blocked += 1,
            "missing_source" => summary.listings_missing_source += 1,
            "error" => summary.listing_errors += 1,
            _ => {}
        }
        for candidate in &listing.candidates {
            summary.identity_candidates += 1;
            summary.identity_resolution_attempts += usize::from(candidate.resolution_attempted);
            match candidate.status.as_str() {
                "existing" => summary.existing += 1,
                "new" => summary.new += 1,
                "promoted" => summary.promoted += 1,
                "rejected" => summary.rejected += 1,
                "unresolved" => summary.unresolved += 1,
                "error" => summary.errors += 1,
                _ => {}
            }
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_request_plan_counts_logical_stages_not_transport_attempts() {
        let plan = provider_request_plan(&AvionicsRepopulationPreflightSummary {
            listings_requiring_legacy_reextraction: 2,
            retained_identity_components: 5,
            verified_local_identity_components: 1,
            gemini_initial_identity_components: 3,
            gemini_conditional_relationship_components: 1,
            ..AvionicsRepopulationPreflightSummary::default()
        });

        assert_eq!(plan.listing_extraction_provider_requests_baseline, 2);
        assert_eq!(
            plan.listing_extraction_provider_requests_validation_envelope,
            4
        );
        assert_eq!(plan.initial_grounded_provider_requests_baseline, 9);
        assert_eq!(
            plan.initial_grounded_provider_requests_nonpositive_validation_envelope,
            24
        );
        assert_eq!(plan.positive_identity_provider_requests_baseline, 24);
        assert_eq!(
            plan.positive_identity_provider_requests_validation_envelope,
            56
        );
        assert_eq!(plan.known_total_provider_requests_minimum_baseline, 11);
        assert_eq!(plan.known_total_provider_requests_all_positive_baseline, 26);
        assert_eq!(
            plan.known_total_provider_requests_validation_envelope_maximum,
            60
        );
        assert!(plan.legacy_reextraction_identity_outputs_unknown);
        assert!(!plan.logical_provider_request_counts_include_transport_retries);
        assert_eq!(plan.default_max_transport_attempts_per_logical_request, 4);
        assert!(plan
            .transport_retry_note
            .contains("four transport attempts"));
    }

    #[test]
    fn repopulation_scope_rejects_ambiguous_cursor() {
        let error = AvionicsRepopulationScope::new(10, Some(12), Some(11))
            .validate()
            .expect_err("exact and cursor selection cannot be combined");
        assert!(error.to_string().contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn preflight_pages_forward_past_residual_low_listing_ids() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let mut listing_ids = Vec::new();
        for suffix in ["first", "second", "third"] {
            let source_url = format!("https://example.test/listing/{suffix}");
            let listing_id = seed_listing(&db, &source_url).await;
            seed_submission_and_review(
                &db,
                listing_id,
                &source_url,
                "<p>Garmin GTX 345R installed</p>",
                Some(
                    r#"{"avionics":[{"manufacturer":"Garmin","model":"GTX 345R","types":["Transponder"],"source_confidence":"high"}]}"#,
                ),
                Some(listing_id),
                None,
            )
            .await;
            listing_ids.push(listing_id);
        }

        let first = preflight_listing_avionics_repopulation(
            &db,
            &AvionicsRepopulationScope::new(1, None, None),
        )
        .await
        .unwrap();
        assert_eq!(first.mode, "preflight");
        assert_eq!(first.checkpoint.page_first_listing_id, Some(listing_ids[0]));
        assert_eq!(first.checkpoint.page_last_listing_id, Some(listing_ids[0]));
        assert_eq!(
            first.checkpoint.resume_after_listing_id,
            Some(listing_ids[0])
        );
        assert!(first.checkpoint.has_more);

        let second = preflight_listing_avionics_repopulation(
            &db,
            &AvionicsRepopulationScope::new(1, None, Some(listing_ids[0])),
        )
        .await
        .unwrap();
        assert_eq!(
            second.checkpoint.requested_after_listing_id,
            Some(listing_ids[0])
        );
        assert_eq!(
            second.checkpoint.page_first_listing_id,
            Some(listing_ids[1])
        );
        assert_eq!(
            second.checkpoint.resume_after_listing_id,
            Some(listing_ids[1])
        );
        assert!(second.checkpoint.has_more);
    }

    #[tokio::test]
    async fn repopulation_rejects_before_gemini_and_link_replacement() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_listing(&db, "https://example.test/listing/faa-gate").await;
        seed_submission_and_review(
            &db,
            listing_id,
            "https://example.test/listing/faa-gate",
            "<p>Garmin GTX 345R installed</p>",
            Some(
                r#"{"avionics":[{"manufacturer":"Garmin","model":"GTX 345R","types":["Transponder"],"source_confidence":"high"}]}"#,
            ),
            Some(listing_id),
            None,
        )
        .await;
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        let report = repopulate_listing_avionics(
            &db,
            &extractor,
            AvionicsRepopulationExecutionMode::Apply,
            &AvionicsRepopulationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();

        assert_eq!(report.listings.len(), 1);
        assert_eq!(report.listings[0].status, "faa_rejected");
        assert!(report.listings[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("missing_registration")));
        assert_eq!(report.summary.listings_faa_rejected, 1);
        assert_eq!(
            report
                .provider_request_plan
                .listing_extraction_provider_requests_baseline,
            0
        );
        assert_eq!(report.summary.identity_resolution_attempts, 0);
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let links: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(links, 0);
    }

    #[test]
    fn raw_parser_preserves_capability_arrays_and_action_defaults() {
        let parsed = parse_raw_avionics(
            r#"{
              "avionics": [
                {"manufacturer":"Garmin","model":"GTX 345R","types":["Transponder"],"quantity":1},
                {
                  "manufacturer":"Garmin","model":"GTN 750Xi","types":["GPS","NAV","COM"],"quantity":1,
                  "configuration_action":"replaces",
                  "replaces":{"manufacturer":"Garmin","model":"GNS 530W","types":["GPS","NAV","COM"]},
                  "source_evidence_text":"GTN 750Xi replaces GNS 530W",
                  "source_confidence":"medium"
                }
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(parsed[0].configuration_action, "installed");
        assert_eq!(parsed[0].avionics_types, vec!["Transponder"]);
        assert!(parsed[0].replaces.is_none());
        assert!(parsed[0].source_evidence_text.is_none());
        assert!(parsed[0].source_confidence.is_none());
        assert_eq!(parsed[1].configuration_action, "replaces");
        assert_eq!(parsed[1].avionics_types, vec!["GPS", "NAV", "COM"]);
        assert_eq!(
            parsed[1].replaces,
            Some(crate::models::ParsedAvionicsReference {
                manufacturer: "Garmin".to_string(),
                model: "GNS 530W".to_string(),
                avionics_types: vec!["GPS".to_string(), "NAV".to_string(), "COM".to_string()],
            })
        );
        assert_eq!(parsed[1].source_confidence.as_deref(), Some("medium"));
    }

    fn installed_observation(
        capabilities: &[&str],
        quantity: i64,
        evidence: &str,
    ) -> ParsedAvionics {
        ParsedAvionics {
            manufacturer: "King".to_string(),
            model: "KX-170B".to_string(),
            avionics_types: capabilities
                .iter()
                .map(|capability| (*capability).to_string())
                .collect(),
            quantity,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some(evidence.to_string()),
            source_confidence: Some("high".to_string()),
        }
    }

    #[test]
    fn explicit_base_and_number_two_become_one_quantity_two_observation() {
        let first = "King KX-170B Nav-Com w/ VOR, Localizer & Glideslope";
        let second = "King KX-170B Nav-Com #2 w/ Vor & Localizer";
        let context = ListingEvidenceContext::from_cleaned_text(format!("{first}\n{second}"));
        let observations = vec![
            installed_observation(&["NAV", "COM"], 1, first),
            installed_observation(&["COM", "NAV"], 1, second),
        ];

        let coalesced = coalesce_explicit_numbered_instances(observations, &context);

        assert_eq!(coalesced.len(), 1);
        assert_eq!(coalesced[0].quantity, 2);
        assert_eq!(
            coalesced[0].source_evidence_text.as_deref(),
            Some(
                "King KX-170B Nav-Com w/ VOR, Localizer & Glideslope\nKing KX-170B Nav-Com #2 w/ Vor & Localizer"
            )
        );
        assert_eq!(coalesced[0].avionics_types, vec!["NAV", "COM"]);
    }

    #[test]
    fn explicit_numbered_pair_uses_the_highest_supported_ordinal() {
        let first = "King KX-170B Nav-Com #1 installed";
        let second = "King KX-170B Nav-Com #2 installed";
        let context = ListingEvidenceContext::from_cleaned_text(format!("{first}\n{second}"));

        let coalesced = coalesce_explicit_numbered_instances(
            vec![
                installed_observation(&["NAV", "COM"], 1, first),
                installed_observation(&["NAV", "COM"], 1, second),
            ],
            &context,
        );

        assert_eq!(coalesced.len(), 1);
        assert_eq!(coalesced[0].quantity, 2);
    }

    #[test]
    fn repeated_unnumbered_narrative_is_not_counted_as_extra_hardware() {
        let evidence = "King KX-170B Nav-Com installed";
        let context = ListingEvidenceContext::from_cleaned_text(evidence);
        let observations = vec![
            installed_observation(&["NAV", "COM"], 1, evidence),
            installed_observation(&["NAV", "COM"], 1, evidence),
        ];

        let unchanged = coalesce_explicit_numbered_instances(observations, &context);

        assert_eq!(unchanged.len(), 2);
        assert!(unchanged
            .iter()
            .all(|observation| observation.quantity == 1));
    }

    #[test]
    fn differing_capabilities_or_actions_do_not_form_a_numbered_group() {
        let first = "King KX-170B Nav-Com installed";
        let second = "King KX-170B Nav #2 installed";
        let replacement = "King KX-170B Nav-Com #2 replaces King KX-165";
        let context =
            ListingEvidenceContext::from_cleaned_text(format!("{first}\n{second}\n{replacement}"));
        let mut replacement_observation = installed_observation(&["NAV", "COM"], 1, replacement);
        replacement_observation.configuration_action = "replaces".to_string();
        replacement_observation.replaces = Some(crate::models::ParsedAvionicsReference {
            manufacturer: "King".to_string(),
            model: "KX-165".to_string(),
            avionics_types: vec!["NAV".to_string(), "COM".to_string()],
        });

        let unchanged = coalesce_explicit_numbered_instances(
            vec![
                installed_observation(&["NAV", "COM"], 1, first),
                installed_observation(&["NAV"], 1, second),
                replacement_observation,
            ],
            &context,
        );

        assert_eq!(unchanged.len(), 3);
        assert!(unchanged
            .iter()
            .all(|observation| observation.quantity == 1));
    }

    #[test]
    fn numbered_evidence_must_be_an_exact_retained_source_excerpt() {
        let first = "King KX-170B Nav-Com installed";
        let context = ListingEvidenceContext::from_cleaned_text(first);
        let observations = vec![
            installed_observation(&["NAV", "COM"], 1, first),
            installed_observation(&["NAV", "COM"], 1, "King KX-170B Nav-Com #2 installed"),
        ];

        let unchanged = coalesce_explicit_numbered_instances(observations, &context);

        assert_eq!(unchanged.len(), 2);
    }

    #[test]
    fn numbered_instance_model_must_not_match_inside_a_longer_product_code() {
        let first = "Garmin G5 display installed";
        let second = "Garmin G500 display #2 installed";
        let context = ListingEvidenceContext::from_cleaned_text(format!("{first}\n{second}"));
        let observations = vec![
            ParsedAvionics {
                manufacturer: "Garmin".to_string(),
                model: "G5".to_string(),
                avionics_types: vec!["PFD".to_string()],
                quantity: 1,
                configuration_action: "installed".to_string(),
                replaces: None,
                source_evidence_text: Some(first.to_string()),
                source_confidence: Some("high".to_string()),
            },
            ParsedAvionics {
                manufacturer: "Garmin".to_string(),
                model: "G5".to_string(),
                avionics_types: vec!["PFD".to_string()],
                quantity: 1,
                configuration_action: "installed".to_string(),
                replaces: None,
                source_evidence_text: Some(second.to_string()),
                source_confidence: Some("high".to_string()),
            },
        ];

        let unchanged = coalesce_explicit_numbered_instances(observations, &context);

        assert_eq!(unchanged.len(), 2);
        assert!(unchanged
            .iter()
            .all(|observation| observation.quantity == 1));
    }

    #[test]
    fn automatic_acceptance_requires_exact_high_listing_evidence() {
        let capabilities = vec!["GPS".to_string()];
        let input = IdentityInput {
            manufacturer: "Garmin",
            model: "GNX 375",
            avionics_types: &capabilities,
            quantity: 1,
        };
        let approved = ApprovedAvionicsIdentity {
            id: 42,
            manufacturer: "Garmin".to_string(),
            model: "GNX 375".to_string(),
            avionics_types: capabilities.clone(),
            manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
            manufacturer_identifier: "GNX-375".to_string(),
            evidence_url: "https://www.garmin.com/gnx375".to_string(),
            evidence_title: "GNX 375".to_string(),
            evidence: "Manufacturer evidence identifies the product.".to_string(),
            reason: "verified exact product".to_string(),
            grounded_claim_source_urls: Vec::new(),
        };
        let attempt = approved_attempt(
            false,
            0,
            "primary",
            &input,
            "installed",
            Some("GNX 375 installed"),
            Some("high"),
            approved,
            Some("catalog:42".to_string()),
            &mut HashMap::new(),
        );

        assert!(can_automatically_accept(&attempt, Some("high")));
        assert!(!can_automatically_accept(&attempt, Some("medium")));
        assert!(!can_automatically_accept(&attempt, Some("High")));
        assert!(!can_automatically_accept(&attempt, None));
    }

    #[test]
    fn dry_run_promotion_uses_an_ephemeral_identity_until_apply_persists_the_graph() {
        assert!(!requires_persisted_graph_identity(
            false,
            10,
            Some("unreviewed")
        ));
        assert!(requires_persisted_graph_identity(
            false,
            15,
            Some("approved")
        ));
        assert!(requires_persisted_graph_identity(
            true,
            10,
            Some("unreviewed")
        ));
        assert!(!requires_persisted_graph_identity(false, 0, None));
    }

    #[test]
    fn unresolved_relationship_keeps_subject_and_replacement_together() {
        let raw = ParsedAvionics {
            manufacturer: "Garmin".to_string(),
            model: "GTN 750Xi".to_string(),
            avionics_types: vec!["GPS".to_string(), "NAV".to_string(), "COM".to_string()],
            quantity: 1,
            configuration_action: "replaces".to_string(),
            replaces: Some(crate::models::ParsedAvionicsReference {
                manufacturer: "Garmin".to_string(),
                model: "GNS 530W".to_string(),
                avionics_types: vec!["GPS".to_string(), "NAV".to_string(), "COM".to_string()],
            }),
            source_evidence_text: Some("GTN 750Xi replaces GNS 530W".to_string()),
            source_confidence: Some("high".to_string()),
        };
        let aspects = dependent_residual_aspects(
            7,
            &raw,
            "verified subject depends on unresolved target",
            Some(ReviewProduct::verified(
                10,
                "Garmin",
                "GTN 750Xi",
                raw.avionics_types.clone(),
            )),
            "target could not be verified",
            None,
        );

        assert_eq!(aspects.len(), 2);
        assert_eq!(
            aspects[0].replacement_aspect_id.as_ref(),
            Some(&aspects[1].id)
        );
        assert_eq!(aspects[0].suggested_product.as_ref().unwrap().id, Some(10));
        assert_eq!(aspects[1].configuration_action, "installed");
    }

    #[test]
    fn malformed_candidate_becomes_residual_review_instead_of_blocking_listing() {
        let raw = ParsedAvionics {
            manufacturer: "Garmin".to_string(),
            model: "GTN 750Xi".to_string(),
            avionics_types: vec!["GPS".to_string()],
            quantity: 0,
            configuration_action: "replaces".to_string(),
            replaces: None,
            source_evidence_text: Some("GTN 750Xi".to_string()),
            source_confidence: Some("medium".to_string()),
        };

        let aspects = input_error_aspects(3, &raw, raw_candidate_issue(&raw).as_deref().unwrap());

        assert_eq!(aspects.len(), 2);
        assert_eq!(aspects[0].quantity, 1);
        assert_eq!(
            aspects[0].replacement_aspect_id.as_ref(),
            Some(&aspects[1].id)
        );
        assert!(aspects[0].reason.contains("could not safely interpret"));
        assert!(aspects[1].reason.contains("could not safely interpret"));
    }

    #[test]
    fn legacy_scalar_type_requires_reextraction_without_mechanical_conversion() {
        let legacy = r#"{
          "avionics": [
            {"manufacturer":"Garmin","model":"GNX 375","type":"GPS","quantity":1}
          ]
        }"#;

        let source = retained_avionics_source(Some(legacy));

        let RetainedAvionicsSource::RequiresReextraction { reason } = source else {
            panic!("a scalar legacy type must never be replayed or converted locally")
        };
        assert!(reason.contains("scalar type payloads are intentionally unsupported"));
    }

    #[test]
    fn current_multi_capability_payload_is_replayed_without_reextraction() {
        let current = r#"{
          "avionics": [
            {
              "manufacturer":"Garmin",
              "model":"GNX 375",
              "types":["GPS","Transponder"],
              "quantity":1
            }
          ]
        }"#;

        let source = retained_avionics_source(Some(current));

        let RetainedAvionicsSource::Current(avionics) = source else {
            panic!("a current capability-array payload should be replayable")
        };
        assert_eq!(avionics.len(), 1);
        assert_eq!(avionics[0].avionics_types, vec!["GPS", "Transponder"]);
    }

    #[test]
    fn missing_or_invalid_capability_arrays_fail_closed_to_reextraction() {
        assert!(matches!(
            retained_avionics_source(None),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert!(matches!(
            retained_avionics_source(Some(r#"{"avionics":[]}"#)),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert!(matches!(
            retained_avionics_source(Some(
                r#"{"avionics":[{"manufacturer":"Garmin","model":"GTN 750Xi","types":[]}]}"#
            )),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert!(matches!(
            retained_avionics_source(Some(
                r#"{
                  "avionics":[{
                    "manufacturer":"Garmin","model":"GTN 750Xi","types":["GPS"],
                    "configuration_action":"replaces",
                    "replaces":{"manufacturer":"Garmin","model":"GNS 530W","type":"GPS"}
                  }]
                }"#
            )),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
    }

    #[test]
    fn reextraction_requires_a_valid_source_url_and_usable_stored_html() {
        assert!(prepare_stored_listing_text("not-a-url", "<p>GNX 375</p>").is_err());
        assert!(prepare_stored_listing_text(
            "https://example.test/listing/375",
            "<script>only non-listing content</script>"
        )
        .is_err());

        let text = prepare_stored_listing_text(
            "https://example.test/listing/375",
            "<p>Garmin GNX 375 installed</p>",
        )
        .unwrap();
        assert!(text.contains("Garmin GNX 375 installed"));
    }

    #[test]
    fn candidate_context_is_source_only_targeted_and_capped() {
        let filler = (0..2_000)
            .map(|index| format!("<p>Boilerplate listing detail {index}</p>"))
            .collect::<String>();
        let html = format!(
            "<html><head><title>2020 Cessna 182T</title></head><body>{filler}<p>Garmin GTX 345R transponder installed</p></body></html>"
        );
        let context = ListingEvidenceContext::from_rendered_html(Some(&html)).for_candidate(
            "Garmin",
            "GTX345R",
            Some("INJECTED RAW JSON EVIDENCE"),
        );

        assert!(context.contains("Garmin GTX 345R transponder installed"));
        assert!(!context.contains("INJECTED RAW JSON EVIDENCE"));
        assert!(context.len() <= crate::listing::evidence::MAX_LISTING_EVIDENCE_CONTEXT_BYTES);
    }

    #[test]
    fn duplicate_capability_rows_merge_conservatively() {
        let mut existing = PreparedLink {
            identity_key: "catalog:42".to_string(),
            avionics_model_id: 42,
            quantity: 1,
            source_notes: Some("GPS navigator".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_identity_key: None,
        };
        let incoming = PreparedLink {
            identity_key: "catalog:42".to_string(),
            avionics_model_id: 42,
            quantity: 2,
            source_notes: Some("Mode S transponder".to_string()),
            source_confidence: Some("medium".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_identity_key: None,
        };

        merge_duplicate_link(&mut existing, &incoming).unwrap();

        assert_eq!(existing.quantity, 2);
        assert_eq!(
            existing.source_notes.as_deref(),
            Some("GPS navigator\nMode S transponder")
        );
        assert_eq!(existing.source_confidence.as_deref(), Some("medium"));

        let no_confidence = PreparedLink {
            source_confidence: None,
            ..incoming
        };
        merge_duplicate_link(&mut existing, &no_confidence).unwrap();
        assert_eq!(existing.source_confidence, None);
    }

    #[test]
    fn gnx_375_gps_and_transponder_rows_become_one_physical_link() {
        let mut prepared = [PreparedLink {
            identity_key: "catalog:375".to_string(),
            avionics_model_id: 375,
            quantity: 1,
            source_notes: Some("GNX 375 GPS navigator installed".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_identity_key: None,
        }];
        let transponder_row = PreparedLink {
            identity_key: "catalog:375".to_string(),
            avionics_model_id: 375,
            quantity: 1,
            source_notes: Some("GNX 375 transponder installed".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_identity_key: None,
        };

        let existing = prepared
            .iter_mut()
            .find(|link| link.avionics_model_id == transponder_row.avionics_model_id)
            .expect("both capability rows resolve to the same catalog product");
        merge_duplicate_link(existing, &transponder_row).unwrap();

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].quantity, 1, "capabilities are not extra units");
        assert_eq!(
            prepared[0].source_notes.as_deref(),
            Some("GNX 375 GPS navigator installed\nGNX 375 transponder installed")
        );
    }

    #[test]
    fn dry_run_new_capability_rows_coalesce_by_verified_product_identifier() {
        let gps = ApprovedAvionicsIdentity {
            id: 0,
            manufacturer: "Garmin".to_string(),
            model: "GNX 375".to_string(),
            avionics_types: vec!["GPS".to_string()],
            manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
            manufacturer_identifier: "GNX-375".to_string(),
            evidence_url: "https://www.garmin.com/gnx375".to_string(),
            evidence_title: "GNX 375".to_string(),
            evidence: "Manufacturer product evidence".to_string(),
            reason: "Verified GPS capability".to_string(),
            grounded_claim_source_urls: Vec::new(),
        };
        let transponder = ApprovedAvionicsIdentity {
            avionics_types: vec!["Transponder".to_string()],
            reason: "Verified transponder capability".to_string(),
            ..gps.clone()
        };
        let gps_key = preview_approved_identity_key(&gps).unwrap();
        let transponder_key = preview_approved_identity_key(&transponder).unwrap();
        assert_eq!(gps_key, transponder_key);

        let mut prepared = Vec::new();
        assert!(!merge_or_push_prepared_link(
            &mut prepared,
            PreparedLink {
                identity_key: gps_key,
                avionics_model_id: 0,
                quantity: 1,
                source_notes: Some("GNX 375 GPS navigator".to_string()),
                source_confidence: Some("high".to_string()),
                configuration_action: "installed".to_string(),
                replaces_avionics_model_id: None,
                replacement_identity_key: None,
            },
        )
        .unwrap());
        assert!(merge_or_push_prepared_link(
            &mut prepared,
            PreparedLink {
                identity_key: transponder_key,
                avionics_model_id: 0,
                quantity: 1,
                source_notes: Some("GNX 375 transponder".to_string()),
                source_confidence: Some("high".to_string()),
                configuration_action: "installed".to_string(),
                replaces_avionics_model_id: None,
                replacement_identity_key: None,
            },
        )
        .unwrap());
        assert_eq!(prepared.len(), 1);
        assert_eq!(
            prepared[0].source_notes.as_deref(),
            Some("GNX 375 GPS navigator\nGNX 375 transponder")
        );
    }

    #[test]
    fn duplicate_capability_rows_with_conflicting_semantics_are_rejected() {
        let mut existing = PreparedLink {
            identity_key: "catalog:42".to_string(),
            avionics_model_id: 42,
            quantity: 1,
            source_notes: None,
            source_confidence: None,
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_identity_key: None,
        };
        let conflicting = PreparedLink {
            configuration_action: "replaces".to_string(),
            replaces_avionics_model_id: Some(7),
            ..existing.clone()
        };

        assert!(merge_duplicate_link(&mut existing, &conflicting).is_err());
        assert_eq!(existing.configuration_action, "installed");
        assert_eq!(existing.replaces_avionics_model_id, None);

        let mut unresolved_replacement = PreparedLink {
            configuration_action: "replaces".to_string(),
            replaces_avionics_model_id: Some(0),
            ..existing.clone()
        };
        let same_unresolved_replacement = unresolved_replacement.clone();
        assert!(
            merge_duplicate_link(&mut unresolved_replacement, &same_unresolved_replacement)
                .is_err()
        );
    }

    fn prepared_action(
        id: i64,
        key: &str,
        action: &str,
        target_id: Option<i64>,
        target_key: Option<&str>,
    ) -> PreparedLink {
        PreparedLink {
            identity_key: key.to_string(),
            avionics_model_id: id,
            quantity: 1,
            source_notes: None,
            source_confidence: Some("high".to_string()),
            configuration_action: action.to_string(),
            replaces_avionics_model_id: target_id,
            replacement_identity_key: target_key.map(ToString::to_string),
        }
    }

    #[test]
    fn prepared_link_validation_accepts_pure_removal() {
        let removal = prepared_action(
            42,
            "7\u{1f}gns430w",
            "removes",
            Some(42),
            Some("7\u{1f}gns430w"),
        );

        validate_prepared_links(&[removal]).unwrap();
    }

    #[test]
    fn prepared_link_validation_uses_preview_identity_keys_when_ids_are_zero() {
        let replacement = prepared_action(
            0,
            "preview:new-unit",
            "replaces",
            Some(0),
            Some("preview:old-unit"),
        );
        let removal = prepared_action(
            0,
            "preview:removed-unit",
            "removes",
            Some(0),
            Some("preview:removed-unit"),
        );

        validate_prepared_links(&[replacement]).unwrap();
        validate_prepared_links(&[removal]).unwrap();
    }

    #[test]
    fn prepared_link_validation_rejects_self_replacement_and_graph_cycles() {
        let self_replacement = prepared_action(
            42,
            "7\u{1f}gns430w",
            "replaces",
            Some(42),
            Some("7\u{1f}gns430w"),
        );
        assert!(validate_prepared_links(&[self_replacement]).is_err());

        let cycle = [
            prepared_action(10, "7\u{1f}newa", "replaces", Some(11), Some("7\u{1f}newb")),
            prepared_action(11, "7\u{1f}newb", "replaces", Some(10), Some("7\u{1f}newa")),
        ];
        assert!(validate_prepared_links(&cycle).is_err());
    }

    #[test]
    fn prepared_link_validation_rejects_duplicate_displacement_targets() {
        let links = [
            prepared_action(10, "7\u{1f}newa", "replaces", Some(12), Some("7\u{1f}old")),
            prepared_action(11, "7\u{1f}newb", "replaces", Some(12), Some("7\u{1f}old")),
        ];

        assert!(validate_prepared_links(&links).is_err());
    }

    #[tokio::test]
    async fn source_loader_uses_only_the_submission_attached_to_pending_review() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let listing_id = seed_listing(&db, "https://example.test/listing/51").await;
        let attached_id = seed_submission_and_review(
            &db,
            listing_id,
            "https://example.test/listing/51",
            "<p>Attached GTX 345R installed</p>",
            Some(
                r#"{"avionics":[{"manufacturer":"Garmin","model":"Attached","types":["Transponder"]}]}"#,
            ),
            None,
            Some("prior extraction warning"),
        )
        .await;
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              canonical_listing_id
            ) VALUES (
              1, 1, 'https://example.test/listing/51', '<p>Newer same URL</p>',
              ?, 'newer-signature',
              '{"avionics":[{"manufacturer":"Garmin","model":"Newer","types":["GPS"]}]}',
              NULL
            )
            "#,
        )
        .bind(sha256_hex("<p>Newer same URL</p>".as_bytes()))
        .execute(pool)
        .await
        .unwrap();

        let page = load_listing_sources(
            &db,
            &AvionicsRepopulationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();
        let rows = page.rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].submission_id, Some(attached_id));
        assert_eq!(rows[0].submission_canonical_listing_id, None);
        assert_eq!(
            rows[0].submission_extraction_error.as_deref(),
            Some("prior extraction warning")
        );
        let raw = parse_raw_avionics(rows[0].extracted_listing_json.as_deref().unwrap()).unwrap();
        assert_eq!(raw[0].model, "Attached");
        assert_eq!(
            validate_pending_source_binding(&rows[0]).unwrap().1,
            "exact_source_url"
        );
    }

    #[tokio::test]
    async fn source_loader_retains_html_even_when_prior_extraction_is_missing() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_listing(&db, "https://example.test/listing/52").await;
        seed_submission_and_review(
            &db,
            listing_id,
            "https://example.test/listing/52",
            "<p>Garmin GNX 375 installed</p>",
            None,
            Some(listing_id),
            Some("legacy extraction unavailable"),
        )
        .await;

        let page = load_listing_sources(
            &db,
            &AvionicsRepopulationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();
        let rows = page.rows;

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].submission_canonical_listing_id, Some(listing_id));
        assert!(rows[0].extracted_listing_json.is_none());
        assert_eq!(
            rows[0].rendered_html.as_deref(),
            Some("<p>Garmin GNX 375 installed</p>")
        );
        assert!(matches!(
            retained_avionics_source(rows[0].extracted_listing_json.as_deref()),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert_eq!(
            validate_pending_source_binding(&rows[0]).unwrap().1,
            "canonical_listing_id"
        );
    }

    #[tokio::test]
    async fn source_binding_rejects_owner_and_content_hash_mismatches() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_listing(&db, "https://example.test/listing/29").await;
        seed_submission_and_review(
            &db,
            listing_id,
            "https://example.test/listing/29",
            "<p>Canonical evidence</p>",
            Some(r#"{"avionics":[{"manufacturer":"Garmin","model":"Canonical","types":["GPS"]}]}"#),
            Some(listing_id),
            None,
        )
        .await;
        let mut rows = load_listing_sources(
            &db,
            &AvionicsRepopulationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap()
        .rows;
        let mut row = rows.pop().unwrap();
        assert!(validate_pending_source_binding(&row).is_ok());

        row.submission_owner_user_id = Some(row.listing_owner_user_id + 1);
        assert!(validate_pending_source_binding(&row)
            .unwrap_err()
            .contains("does not belong"));
        row.submission_owner_user_id = Some(row.listing_owner_user_id);
        row.rendered_html_sha256 = Some("f".repeat(64));
        assert!(validate_pending_source_binding(&row)
            .unwrap_err()
            .contains("rendered HTML"));
        row.rendered_html_sha256 =
            Some(sha256_hex(row.rendered_html.as_deref().unwrap().as_bytes()));
        row.review_payload_sha256 = "e".repeat(64);
        assert!(validate_pending_source_binding(&row)
            .unwrap_err()
            .contains("review payload"));
    }

    async fn seed_listing(db: &AppDb, source_url: &str) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (
              (
                SELECT aircraft_model_variant_id
                FROM aircraft_sale_listing_pending_compatibility_placeholder
                WHERE singleton_id = 1
              ),
              1, ?, 2020, 300000, 1000
            )
            RETURNING id
            "#,
        )
        .bind(source_url)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_submission_and_review(
        db: &AppDb,
        listing_id: i64,
        source_url: &str,
        rendered_html: &str,
        extracted_listing_json: Option<&str>,
        canonical_listing_id: Option<i64>,
        extraction_error: Option<&str>,
    ) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (1, 'fixture') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let submission_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              extraction_error, canonical_listing_id
            ) VALUES (1, ?, ?, ?, ?, 'fixture-signature', ?, ?, ?)
            RETURNING id
            "#,
        )
        .bind(install_id)
        .bind(source_url)
        .bind(rendered_html)
        .bind(sha256_hex(rendered_html.as_bytes()))
        .bind(extracted_listing_json)
        .bind(extraction_error)
        .bind(canonical_listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let aspect = PendingReviewAspect::avionics(
            "fixture:avionics:0",
            "avionics",
            "Garmin fixture",
            "Garmin fixture · GPS · quantity 1 · installed",
            "fixture pending review",
            1,
            "installed",
            Some("Garmin fixture".to_string()),
            Some("medium".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "Fixture",
            vec!["GPS".to_string()],
        ));
        crate::listing::review::stage_pending_review(
            db,
            listing_id,
            Some(submission_id),
            &[aspect],
        )
        .await
        .unwrap();
        submission_id
    }
}
