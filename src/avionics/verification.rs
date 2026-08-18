use std::collections::HashMap;
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

use crate::aircraft::faa::require_listing_faa_admission;
use crate::avionics::catalog::{
    classify_invalid_generic_avionics_observation, plan_avionics_identity_verification_route,
    preview_avionics_identity, resolve_avionics_identity_for_automated_review,
    resolve_verified_local_avionics_identity, unique_exact_avionics_review_candidate,
    ApprovedAvionicsIdentity, AvionicsIdentityOutcome, AvionicsIdentityRequest,
    AvionicsIdentityVerificationRoute, GroundedAvionicsResolutionReceipt,
};
use crate::avionics::consolidation::PendingReviewRevisionReceipt;
use crate::avionics::reuse::product_reuse_attestation_is_current;
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
    apply_automated_avionics_review, AutomatedAssociationAuthorization, AutomatedAvionicsLink,
    AutomatedPreservedAssociationGuard, AutomatedReviewApplyRequest,
};
use crate::listing::review::{
    active_collision_closure_revision_sha256, approved_catalog_revision_sha256,
    evaluate_existing_product_associations, grounded_collision_closure_revision_sha256,
    parse_current_pending_review_aspects, ExistingProductAssociationCommit,
    ExistingProductAssociationEvaluation, ListingAssociationRole, PendingReviewAspect,
    ReviewAction, ReviewAspectId, ReviewProduct, StableIdentifier,
};
use crate::models::ParsedAvionics;
use crate::normalize::is_generic_avionics_model_name;
use crate::plugin::sha256_hex;

#[derive(Debug)]
pub enum AvionicsVerificationError {
    Validation(String),
    Database(String),
}

impl fmt::Display for AvionicsVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Database(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for AvionicsVerificationError {}

impl From<sqlx::Error> for AvionicsVerificationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

pub type VerificationResult<T> = Result<T, AvionicsVerificationError>;

#[derive(Clone, Debug, Default, Serialize)]
pub struct AvionicsVerificationSummary {
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
pub struct AvionicsVerificationReport {
    pub mode: String,
    pub requested_limit: i64,
    pub requested_listing_id: Option<i64>,
    pub requested_after_listing_id: Option<i64>,
    pub checkpoint: AvionicsVerificationCheckpoint,
    pub provider_request_plan: AvionicsProviderRequestPlan,
    pub reextraction_policy_note: String,
    pub listings: Vec<AvionicsVerificationListingReport>,
    pub summary: AvionicsVerificationSummary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvionicsVerificationExecutionMode {
    Preview,
    Apply,
}

impl AvionicsVerificationExecutionMode {
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
pub struct AvionicsVerificationScope {
    pub limit: i64,
    pub listing_id: Option<i64>,
    pub after_listing_id: Option<i64>,
}

impl AvionicsVerificationScope {
    pub fn new(limit: i64, listing_id: Option<i64>, after_listing_id: Option<i64>) -> Self {
        Self {
            limit,
            listing_id,
            after_listing_id,
        }
    }

    fn validate(&self) -> VerificationResult<()> {
        if self.limit < 1 {
            return Err(AvionicsVerificationError::Validation(
                "limit must be at least 1".to_string(),
            ));
        }
        if self.listing_id.is_some_and(|listing_id| listing_id < 1) {
            return Err(AvionicsVerificationError::Validation(
                "listing_id must be a positive integer".to_string(),
            ));
        }
        if self
            .after_listing_id
            .is_some_and(|listing_id| listing_id < 1)
        {
            return Err(AvionicsVerificationError::Validation(
                "after_listing_id must be a positive integer".to_string(),
            ));
        }
        if self.listing_id.is_some() && self.after_listing_id.is_some() {
            return Err(AvionicsVerificationError::Validation(
                "listing_id and after_listing_id are mutually exclusive".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsVerificationCheckpoint {
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
    pub candidate_adjudication_identity_components: usize,
    pub candidate_adjudication_conditional_relationship_components: usize,
    pub candidate_adjudication_provider_requests_baseline: usize,
    pub candidate_grounded_fallback_provider_requests_baseline_maximum: usize,
    pub candidate_triage_identity_components: usize,
    pub candidate_triage_conditional_relationship_components: usize,
    pub candidate_triage_provider_requests_baseline: usize,
    pub candidate_triage_grounded_fallback_provider_requests_baseline_maximum: usize,
    pub grounded_initial_identity_components: usize,
    pub grounded_conditional_relationship_components: usize,
    pub generic_invalid_identity_components: usize,
    pub generic_invalid_classifier_provider_requests: usize,
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
pub struct AvionicsVerificationPreflightSummary {
    pub listings_selected: usize,
    pub listings_ready_with_retained_observations: usize,
    pub listings_requiring_legacy_reextraction: usize,
    pub listings_faa_rejected: usize,
    pub listings_blocked: usize,
    pub retained_identity_components: usize,
    pub verified_local_identity_components: usize,
    pub candidate_adjudication_identity_components: usize,
    pub candidate_adjudication_conditional_relationship_components: usize,
    pub candidate_triage_identity_components: usize,
    pub candidate_triage_conditional_relationship_components: usize,
    pub grounded_initial_identity_components: usize,
    pub grounded_conditional_relationship_components: usize,
    pub generic_invalid_identity_components: usize,
    pub invalid_retained_observations: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsVerificationPreflightReport {
    pub mode: String,
    pub requested_limit: i64,
    pub requested_listing_id: Option<i64>,
    pub requested_after_listing_id: Option<i64>,
    pub checkpoint: AvionicsVerificationCheckpoint,
    pub provider_request_plan: AvionicsProviderRequestPlan,
    pub listings: Vec<AvionicsVerificationPreflightListingReport>,
    pub summary: AvionicsVerificationPreflightSummary,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsVerificationPreflightListingReport {
    pub listing_id: i64,
    pub status: String,
    pub reextraction_required: bool,
    pub retained_identity_components: usize,
    pub verified_local_identity_components: usize,
    pub candidate_adjudication_identity_components: usize,
    pub candidate_adjudication_conditional_relationship_components: usize,
    pub candidate_triage_identity_components: usize,
    pub candidate_triage_conditional_relationship_components: usize,
    pub grounded_initial_identity_components: usize,
    pub grounded_conditional_relationship_components: usize,
    pub generic_invalid_identity_components: usize,
    pub invalid_retained_observations: usize,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsVerificationListingReport {
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
    pub candidates: Vec<AvionicsVerificationCandidateReport>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsVerificationCandidateReport {
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

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ListingAvionicsVerificationPreflight {
    NoPendingReview {
        listing_id: i64,
        ingestion_state: String,
        is_verified: bool,
    },
    PendingReview {
        report: AvionicsVerificationPreflightListingReport,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ListingAvionicsVerification {
    NoPendingReview {
        listing_id: i64,
        ingestion_state: String,
        is_verified: bool,
    },
    Processed {
        report: AvionicsVerificationListingReport,
    },
}

#[derive(Debug, FromRow)]
struct ListingSourceRow {
    listing_id: i64,
    pending_review_id: i64,
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
struct ListingVerificationStateRow {
    ingestion_state: String,
    is_verified: bool,
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
    authorization: Option<AutomatedAssociationAuthorization>,
    expected_collision_closure_sha256: Option<String>,
    quantity: i64,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    replacement_authorization: Option<AutomatedAssociationAuthorization>,
    replacement_identity_key: Option<String>,
    expected_replacement_collision_closure_sha256: Option<String>,
    preserved_association_guard: Option<AutomatedPreservedAssociationGuard>,
}

struct IdentityInput<'a> {
    manufacturer: &'a str,
    model: &'a str,
    avionics_types: &'a [String],
    quantity: i64,
}

struct IdentityAttempt {
    report: AvionicsVerificationCandidateReport,
    approved_id: Option<i64>,
    identity_key: Option<String>,
    collision_closure_sha256: Option<String>,
    authorization: Option<AutomatedAssociationAuthorization>,
    suggested_product: Option<ReviewProduct>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingReviewRevisionCursor {
    listing_id: i64,
    pending_review_id: i64,
    current_sha256: String,
    stale_reason: Option<String>,
}

impl PendingReviewRevisionCursor {
    fn from_listing(row: &ListingSourceRow) -> Self {
        Self {
            listing_id: row.listing_id,
            pending_review_id: row.pending_review_id,
            current_sha256: row.review_payload_sha256.clone(),
            stale_reason: None,
        }
    }

    fn advance(&mut self, receipts: &[PendingReviewRevisionReceipt]) -> Result<(), String> {
        if let Some(reason) = self.stale_reason.as_ref() {
            return Err(reason.clone());
        }
        for receipt in receipts {
            if receipt.listing_id != self.listing_id {
                continue;
            }
            if receipt.pending_review_id != self.pending_review_id {
                let reason = format!(
                    "grounded consolidation rewrote pending review {} for listing {}, but automatic review loaded pending review {}",
                    receipt.pending_review_id, self.listing_id, self.pending_review_id
                );
                self.stale_reason = Some(reason.clone());
                return Err(reason);
            }
            if receipt.before_sha256 != self.current_sha256 {
                let reason = format!(
                    "grounded consolidation review revision chain is stale for listing {}; expected {}, receipt starts at {}",
                    self.listing_id, self.current_sha256, receipt.before_sha256
                );
                self.stale_reason = Some(reason.clone());
                return Err(reason);
            }
            self.current_sha256 = receipt.after_sha256.clone();
        }
        Ok(())
    }
}

/// Automate only listings already waiting in the review queue. The exact
/// plugin submission attached to the pending bundle is replayed; no newer
/// same-URL submission may silently replace its evidence. Current, hash-bound
/// pending-review observations are reused only while their exact evidence
/// remains present in that submission. Otherwise, legacy extraction payloads
/// are never transformed: the tool runs the current Gemini listing extractor
/// against the retained, hash-verified HTML and uses that transient result.
/// Dry-run still makes paid preview calls without domain writes. Apply mode
/// atomically accepts only grounded products with exact high-confidence
/// listing evidence, safely discards grounded garbage, and leaves every other
/// aspect pending. Signed plugin payloads are never overwritten.
pub async fn verify_listing_avionics_page(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    mode: AvionicsVerificationExecutionMode,
    scope: &AvionicsVerificationScope,
) -> VerificationResult<AvionicsVerificationReport> {
    scope.validate()?;
    let page = load_listing_sources(db, scope).await?;
    if scope.listing_id.is_some() && page.rows.is_empty() {
        return Err(AvionicsVerificationError::Validation(format!(
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
    Ok(AvionicsVerificationReport {
        mode: mode.label().to_string(),
        requested_limit: scope.limit,
        requested_listing_id: scope.listing_id,
        requested_after_listing_id: scope.after_listing_id,
        checkpoint,
        provider_request_plan,
        reextraction_policy_note: if apply {
            "Apply mode first reuses current hash-bound review observations whose exact excerpts remain in the retained submission, otherwise re-extracts incompatible legacy payloads from retained HTML, then atomically persists high-confidence approved links plus only the residual review aspects; signed plugin payloads are never overwritten."
                .to_string()
        } else {
            "Preview mode reuses current exact review observations before making Gemini requests, including legacy re-extraction only when required; neither generated extraction nor catalog/listing changes are persisted."
                .to_string()
        },
        listings,
        summary,
    })
}

/// Inspect one listing without making a provider request.
///
/// A listing with no pending avionics review is a typed no-op rather than an
/// error. This lets ingestion, retry workers, and administrative pages share
/// one idempotent domain entry point.
pub async fn preflight_listing_avionics(
    db: &AppDb,
    listing_id: i64,
) -> VerificationResult<ListingAvionicsVerificationPreflight> {
    if listing_id < 1 {
        return Err(AvionicsVerificationError::Validation(
            "listing_id must be a positive integer".to_string(),
        ));
    }
    let scope = AvionicsVerificationScope::new(1, Some(listing_id), None);
    let page = load_listing_sources(db, &scope).await?;
    match page.rows.as_slice() {
        [row] => Ok(ListingAvionicsVerificationPreflight::PendingReview {
            report: preflight_listing(db, row).await,
        }),
        [] => {
            let state = load_listing_verification_state(db, listing_id).await?;
            Ok(ListingAvionicsVerificationPreflight::NoPendingReview {
                listing_id,
                ingestion_state: state.ingestion_state,
                is_verified: state.is_verified,
            })
        }
        _ => unreachable!("an exact listing verification scope loads at most one row"),
    }
}

/// Verify one pending listing's avionics through the shared local-first
/// workflow and atomic apply boundary.
///
/// Repeating the call after the review has cleared returns `NoPendingReview`
/// and performs no catalog, link, review, or provider work.
pub async fn verify_listing_avionics(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    mode: AvionicsVerificationExecutionMode,
    listing_id: i64,
) -> VerificationResult<ListingAvionicsVerification> {
    if listing_id < 1 {
        return Err(AvionicsVerificationError::Validation(
            "listing_id must be a positive integer".to_string(),
        ));
    }
    let scope = AvionicsVerificationScope::new(1, Some(listing_id), None);
    let page = load_listing_sources(db, &scope).await?;
    match page.rows.as_slice() {
        [row] => {
            let mut catalog_statuses = load_catalog_statuses(db).await?;
            Ok(ListingAvionicsVerification::Processed {
                report: process_listing(db, extractor, mode.applies(), row, &mut catalog_statuses)
                    .await,
            })
        }
        [] => {
            let state = load_listing_verification_state(db, listing_id).await?;
            Ok(ListingAvionicsVerification::NoPendingReview {
                listing_id,
                ingestion_state: state.ingestion_state,
                is_verified: state.is_verified,
            })
        }
        _ => unreachable!("an exact listing verification scope loads at most one row"),
    }
}

/// Inspect the selected page and produce a provider-request plan without
/// constructing a Gemini client or making any provider request.
pub async fn preflight_listing_avionics_page(
    db: &AppDb,
    scope: &AvionicsVerificationScope,
) -> VerificationResult<AvionicsVerificationPreflightReport> {
    scope.validate()?;
    let page = load_listing_sources(db, scope).await?;
    if scope.listing_id.is_some() && page.rows.is_empty() {
        return Err(AvionicsVerificationError::Validation(format!(
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
    ) -> AvionicsVerificationCheckpoint {
        let page_first_listing_id = self.rows.first().map(|row| row.listing_id);
        let page_last_listing_id = self.rows.last().map(|row| row.listing_id);
        AvionicsVerificationCheckpoint {
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
    scope: &AvionicsVerificationScope,
    page: &ListingSourcePage,
) -> AvionicsVerificationPreflightReport {
    let mut listings = Vec::with_capacity(page.rows.len());
    for row in &page.rows {
        listings.push(preflight_listing(db, row).await);
    }
    let summary = summarize_preflight(&listings);
    let provider_request_plan = provider_request_plan(&summary);
    AvionicsVerificationPreflightReport {
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

fn summarize_preflight(
    listings: &[AvionicsVerificationPreflightListingReport],
) -> AvionicsVerificationPreflightSummary {
    let mut summary = AvionicsVerificationPreflightSummary {
        listings_selected: listings.len(),
        ..AvionicsVerificationPreflightSummary::default()
    };
    for listing in listings {
        summary.retained_identity_components += listing.retained_identity_components;
        summary.verified_local_identity_components += listing.verified_local_identity_components;
        summary.candidate_adjudication_identity_components +=
            listing.candidate_adjudication_identity_components;
        summary.candidate_adjudication_conditional_relationship_components +=
            listing.candidate_adjudication_conditional_relationship_components;
        summary.candidate_triage_identity_components +=
            listing.candidate_triage_identity_components;
        summary.candidate_triage_conditional_relationship_components +=
            listing.candidate_triage_conditional_relationship_components;
        summary.grounded_initial_identity_components +=
            listing.grounded_initial_identity_components;
        summary.grounded_conditional_relationship_components +=
            listing.grounded_conditional_relationship_components;
        summary.generic_invalid_identity_components += listing.generic_invalid_identity_components;
        summary.invalid_retained_observations += listing.invalid_retained_observations;
        match listing.status.as_str() {
            "ready_retained_observations" => {
                summary.listings_ready_with_retained_observations += 1;
            }
            "ready_legacy_reextraction" => {
                summary.listings_requiring_legacy_reextraction += 1;
            }
            "faa_rejected" => summary.listings_faa_rejected += 1,
            _ => summary.listings_blocked += 1,
        }
    }
    summary
}

/// Aggregate the exact logical provider plan for an arbitrary listing page.
///
/// The top-level aircraft-and-avionics verifier owns its own listing
/// selection, so it uses this boundary instead of approximating costs from a
/// separately selected avionics page.
pub fn provider_request_plan_for_listing_preflights(
    listings: &[AvionicsVerificationPreflightListingReport],
) -> AvionicsProviderRequestPlan {
    provider_request_plan(&summarize_preflight(listings))
}

async fn preflight_listing(
    db: &AppDb,
    row: &ListingSourceRow,
) -> AvionicsVerificationPreflightListingReport {
    let mut report = AvionicsVerificationPreflightListingReport {
        listing_id: row.listing_id,
        status: "blocked".to_string(),
        reextraction_required: false,
        retained_identity_components: 0,
        verified_local_identity_components: 0,
        candidate_adjudication_identity_components: 0,
        candidate_adjudication_conditional_relationship_components: 0,
        candidate_triage_identity_components: 0,
        candidate_triage_conditional_relationship_components: 0,
        grounded_initial_identity_components: 0,
        grounded_conditional_relationship_components: 0,
        generic_invalid_identity_components: 0,
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
    let listing_context = ListingEvidenceContext::from_rendered_html(row.rendered_html.as_deref());
    let raw_avionics = match retained_observation_source(db, row, &listing_context).await {
        Ok(RetainedObservationSource::Review { avionics, .. })
        | Ok(RetainedObservationSource::Extraction { avionics, .. }) => avionics,
        Ok(RetainedObservationSource::RequiresReextraction { reason, .. }) => {
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
        Err(error) => {
            report.note = error;
            return report;
        }
    };
    let raw_avionics = coalesce_explicit_numbered_instances(raw_avionics, &listing_context);
    for raw in &raw_avionics {
        if raw_candidate_structure_issue(raw).is_some() {
            report.invalid_retained_observations += 1;
            continue;
        }
        if generic_model_issue(raw).is_some() {
            report.invalid_retained_observations += 1;
            report.generic_invalid_identity_components += 1;
            continue;
        }
        let primary_route = match preflight_identity_component(
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
            Ok(route) => route,
            Err(error) => {
                report.note = format!("verified-local identity preflight failed: {error}");
                return report;
            }
        };
        report.retained_identity_components += 1;
        match primary_route {
            AvionicsIdentityVerificationRoute::VerifiedLocal => {
                report.verified_local_identity_components += 1;
            }
            AvionicsIdentityVerificationRoute::CandidateAdjudication => {
                report.candidate_adjudication_identity_components += 1;
            }
            AvionicsIdentityVerificationRoute::CandidateTriage => {
                report.candidate_triage_identity_components += 1;
            }
            AvionicsIdentityVerificationRoute::GroundedCuration => {
                report.grounded_initial_identity_components += 1;
            }
        }

        let Some(replacement) = raw.replaces.as_ref() else {
            continue;
        };
        let replacement_route = match preflight_identity_component(
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
            Ok(route) => route,
            Err(error) => {
                report.note = format!("verified-local replacement preflight failed: {error}");
                return report;
            }
        };
        report.retained_identity_components += 1;
        match replacement_route {
            AvionicsIdentityVerificationRoute::VerifiedLocal => {
                report.verified_local_identity_components += 1;
            }
            AvionicsIdentityVerificationRoute::CandidateAdjudication
                if primary_route == AvionicsIdentityVerificationRoute::VerifiedLocal =>
            {
                report.candidate_adjudication_identity_components += 1;
            }
            AvionicsIdentityVerificationRoute::CandidateAdjudication => {
                report.candidate_adjudication_conditional_relationship_components += 1;
            }
            AvionicsIdentityVerificationRoute::CandidateTriage
                if primary_route == AvionicsIdentityVerificationRoute::VerifiedLocal =>
            {
                report.candidate_triage_identity_components += 1;
            }
            AvionicsIdentityVerificationRoute::CandidateTriage => {
                report.candidate_triage_conditional_relationship_components += 1;
            }
            AvionicsIdentityVerificationRoute::GroundedCuration
                if primary_route == AvionicsIdentityVerificationRoute::VerifiedLocal =>
            {
                // A locally approved primary cannot be rejected, so the
                // execution path necessarily evaluates its relationship.
                report.grounded_initial_identity_components += 1;
            }
            AvionicsIdentityVerificationRoute::GroundedCuration => {
                // A rejected nonlocal primary stops before its target.
                report.grounded_conditional_relationship_components += 1;
            }
        }
    }
    report.status = "ready_retained_observations".to_string();
    report.note = if report.invalid_retained_observations == 0 {
        "retained current-schema observations are ready".to_string()
    } else if report.generic_invalid_identity_components > 0 {
        format!(
            "{} retained observation(s) are invalid; {} structurally valid generic-label observation(s) will receive one tools-disabled discard-classifier request, while every non-rejected or otherwise invalid observation remains for review",
            report.invalid_retained_observations, report.generic_invalid_identity_components
        )
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
) -> Result<AvionicsIdentityVerificationRoute, String> {
    let request = identity_request(
        row,
        source_url,
        listing_context,
        &identity,
        source_evidence_text,
    );
    plan_avionics_identity_verification_route(db, &request)
        .await
        .map_err(|error| error.to_string())
}

fn provider_request_plan(
    summary: &AvionicsVerificationPreflightSummary,
) -> AvionicsProviderRequestPlan {
    let reextractions = summary.listings_requiring_legacy_reextraction;
    let candidate = summary.candidate_adjudication_identity_components;
    let conditional_candidate = summary.candidate_adjudication_conditional_relationship_components;
    let all_candidate_components = candidate.saturating_add(conditional_candidate);
    let triage = summary.candidate_triage_identity_components;
    let conditional_triage = summary.candidate_triage_conditional_relationship_components;
    let all_triage_components = triage.saturating_add(conditional_triage);
    let grounded = summary.grounded_initial_identity_components;
    let conditional_grounded = summary.grounded_conditional_relationship_components;
    let all_grounded_components = grounded.saturating_add(conditional_grounded);
    let generic_invalid = summary.generic_invalid_identity_components;
    AvionicsProviderRequestPlan {
        listings_requiring_legacy_reextraction: reextractions,
        listing_extraction_provider_requests_baseline: reextractions,
        listing_extraction_provider_requests_validation_envelope: reextractions.saturating_mul(2),
        retained_identity_components: summary.retained_identity_components,
        verified_local_identity_components: summary.verified_local_identity_components,
        candidate_adjudication_identity_components: candidate,
        candidate_adjudication_conditional_relationship_components: conditional_candidate,
        candidate_adjudication_provider_requests_baseline: candidate,
        candidate_grounded_fallback_provider_requests_baseline_maximum: all_candidate_components
            .saturating_mul(4),
        candidate_triage_identity_components: triage,
        candidate_triage_conditional_relationship_components: conditional_triage,
        candidate_triage_provider_requests_baseline: triage,
        candidate_triage_grounded_fallback_provider_requests_baseline_maximum:
            all_triage_components.saturating_mul(4),
        grounded_initial_identity_components: grounded,
        grounded_conditional_relationship_components: conditional_grounded,
        generic_invalid_identity_components: generic_invalid,
        generic_invalid_classifier_provider_requests: generic_invalid,
        initial_grounded_provider_requests_baseline: grounded.saturating_mul(4),
        initial_grounded_provider_requests_nonpositive_validation_envelope: grounded
            .saturating_mul(9),
        positive_identity_provider_requests_baseline: all_grounded_components.saturating_mul(7),
        positive_identity_provider_requests_validation_envelope: all_grounded_components
            .saturating_mul(15),
        known_total_provider_requests_minimum_baseline: reextractions
            .saturating_add(candidate)
            .saturating_add(triage)
            .saturating_add(generic_invalid)
            .saturating_add(grounded.saturating_mul(4))
            .saturating_add(triage.saturating_mul(4)),
        known_total_provider_requests_all_positive_baseline: reextractions
            .saturating_add(all_candidate_components)
            .saturating_add(all_triage_components)
            .saturating_add(generic_invalid)
            .saturating_add(all_grounded_components.saturating_mul(7))
            .saturating_add(all_triage_components.saturating_mul(7)),
        known_total_provider_requests_validation_envelope_maximum: reextractions
            .saturating_mul(2)
            .saturating_add(all_candidate_components)
            .saturating_add(all_triage_components)
            .saturating_add(generic_invalid)
            .saturating_add(
                all_grounded_components
                    .saturating_add(all_candidate_components)
                    .saturating_add(all_triage_components)
                    .saturating_mul(15),
            ),
        legacy_reextraction_identity_outputs_unknown: reextractions > 0,
        logical_provider_request_counts_include_transport_retries: false,
        default_max_transport_attempts_per_logical_request: usize::from(
            RetryPolicy::default().max_attempts(),
        ),
        grounded_pass_note: "Every identity that reaches the grounded route first uses exactly one tools-disabled concreteness-classifier request. A structurally valid current-schema observation rejected locally only because its model is a generic label uses that same one-call classifier without entering the grounded route. A strict very-high-confidence generic result stops there; malformed, ambiguous, weaker, or concrete results on the invalid-observation path remain for review. The fresh grounded identity pass has three logical provider requests at baseline (Search, URL Context, structure) and at most six after per-stage validation fallbacks. One reused-evidence identity correction can raise the grounded portion to eight. Including the classifier, a positive identity and its independent collision pass use seven requests at baseline and up to fifteen in the complete validation envelope. A nonpositive grounded identity does not run collision review."
            .to_string(),
        transport_retry_note: "Logical provider-request counts do not multiply transport retries. The default interactions retry policy may make up to four transport attempts for one logical request."
            .to_string(),
        uncertainty_note: "The minimum baseline assumes every bounded approved-candidate adjudication succeeds without Search, every global candidate-triage call produces a usable hint, and every conditional relationship target is skipped. Each comparison is exactly one tools-disabled request. A successful approved-candidate decision does not run the concreteness classifier and still passes the unchanged local reuse gates. Because preflight cannot know the triage decision, it conservatively includes the ordinary classifier and grounded route for every triage component; a current approved singleton that passes reuse can be cheaper, while every unreviewed result must take that grounded route. An uncertain, negative, invalid, or stale answer falls through normally and then incurs exactly one classifier request before grounded research. Structurally valid generic-label observations contribute exactly one classifier request to every plan total and never continue to grounding from the invalid-observation path. The all-positive baseline includes every conditional target but assumes candidate comparison succeeds. The maximum validation envelope includes classifier plus grounded fallback for every candidate. Verified-local identities use neither request. All counts use the catalog as it exists at preflight time; earlier apply pages can approve identities that later pages resolve locally with zero Gemini requests. Legacy re-extraction output counts and correction/fallback outcomes are unknowable before execution, so no dollar estimate is inferred."
            .to_string(),
    }
}

async fn process_listing(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    row: &ListingSourceRow,
    catalog_statuses: &mut HashMap<i64, String>,
) -> AvionicsVerificationListingReport {
    let source_url = row.submission_source_url.clone();
    let mut listing_report = AvionicsVerificationListingReport {
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
    let listing_context = ListingEvidenceContext::from_rendered_html(row.rendered_html.as_deref());
    let mut ordinary_review_forced_fallback = false;
    let (raw_avionics, mut residual_aspects) = match retained_observation_source(
        db,
        row,
        &listing_context,
    )
    .await
    {
        Ok(RetainedObservationSource::Review {
            avionics,
            preserved_aspects,
        }) => {
            listing_report.raw_avionics_source = "pending_review".to_string();
            (avionics, preserved_aspects)
        }
        Ok(RetainedObservationSource::Extraction {
            avionics,
            preserved_aspects,
        }) => {
            ordinary_review_forced_fallback = true;
            listing_report.raw_avionics_source = "retained_extraction".to_string();
            (avionics, preserved_aspects)
        }
        Ok(RetainedObservationSource::RequiresReextraction {
            reason,
            preserved_aspects,
        }) => {
            ordinary_review_forced_fallback = true;
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
            match reextract_avionics(&scoped_extractor, &listing_text, &listing_context).await {
                Ok(avionics) => {
                    listing_report.raw_avionics_source = "gemini_reextraction".to_string();
                    listing_report.reextraction_succeeded = true;
                    (avionics, preserved_aspects)
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
        Err(error) => {
            listing_report.status = "blocked".to_string();
            listing_report.error = Some(error);
            return listing_report;
        }
    };
    if ordinary_review_forced_fallback && raw_avionics.is_empty() {
        listing_report.status = "blocked".to_string();
        listing_report.error = Some(
            "fallback extraction returned no avionics observations; the complete prior review and links were retained because empty extraction is not evidence that prior observations are garbage"
                .to_string(),
        );
        return listing_report;
    }
    let mut prepared: Vec<PreparedLink> = Vec::new();
    let mut blocking_reasons = Vec::new();
    let mut review_revision = PendingReviewRevisionCursor::from_listing(row);

    match prepare_current_preserved_associations(db, row, residual_aspects).await {
        Ok((preserved_links, remaining_aspects)) => {
            residual_aspects = remaining_aspects;
            for link in preserved_links {
                if let Err(error) = merge_or_push_prepared_link(&mut prepared, link) {
                    blocking_reasons.push(format!(
                        "preserved association conflicts with another accepted link: {error}"
                    ));
                }
            }
        }
        Err(error) => {
            listing_report.status = "blocked".to_string();
            listing_report.error = Some(format!(
                "preserved-association local verification failed: {error}"
            ));
            return listing_report;
        }
    }

    if raw_avionics.is_empty() && prepared.is_empty() {
        listing_report.status = "blocked".to_string();
        listing_report.error = Some(
            "the pending review has no replayable avionics observations or independently verifiable preserved associations; it was retained because absence is not evidence that prior observations are garbage"
                .to_string(),
        );
        return listing_report;
    }
    let raw_avionics = coalesce_explicit_numbered_instances(raw_avionics, &listing_context);

    for (candidate_index, raw) in raw_avionics.iter().enumerate() {
        if let Some(issue) = raw_candidate_structure_issue(raw) {
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
        if let Some(issue) = generic_model_issue(raw) {
            let identity = IdentityInput {
                manufacturer: &raw.manufacturer,
                model: &raw.model,
                avionics_types: &raw.avionics_types,
                quantity: raw.quantity,
            };
            let request = identity_request(
                row,
                source_url.as_deref(),
                &listing_context,
                &identity,
                raw.source_evidence_text.as_deref(),
            );
            if let Some(reason) =
                classify_invalid_generic_avionics_observation(&scoped_extractor, &request).await
            {
                listing_report.candidates.push(outcome_report(
                    candidate_index,
                    "primary",
                    &identity,
                    &raw.configuration_action,
                    raw.source_evidence_text.as_deref(),
                    raw.source_confidence.as_deref(),
                    "rejected",
                    None,
                    None,
                    None,
                    Vec::new(),
                    reason,
                ));
                listing_report.safely_discarded += 1;
                continue;
            }
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
            &mut review_revision,
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
                    authorization: primary.authorization.clone(),
                    expected_collision_closure_sha256: primary.collision_closure_sha256.clone(),
                    quantity: raw.quantity,
                    source_notes: raw.source_evidence_text.clone(),
                    source_confidence: Some("high".to_string()),
                    configuration_action: raw.configuration_action.clone(),
                    replaces_avionics_model_id: None,
                    replacement_authorization: None,
                    replacement_identity_key: None,
                    expected_replacement_collision_closure_sha256: None,
                    preserved_association_guard: None,
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
            &mut review_revision,
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
            authorization: primary.authorization.clone(),
            expected_collision_closure_sha256: primary.collision_closure_sha256.clone(),
            quantity: raw.quantity,
            source_notes: raw.source_evidence_text.clone(),
            source_confidence: Some("high".to_string()),
            configuration_action: raw.configuration_action.clone(),
            replaces_avionics_model_id: Some(replacement_id),
            replacement_authorization: replacement_attempt.authorization.clone(),
            replacement_identity_key: Some(replacement_identity_key),
            expected_replacement_collision_closure_sha256: replacement_attempt
                .collision_closure_sha256
                .clone(),
            preserved_association_guard: None,
        };
        if let Err(error) =
            merge_prepared_link_for_candidate(&mut prepared, incoming_link, &mut primary)
        {
            blocking_reasons.push(format!("candidate {candidate_index}: {error}"));
        }
        listing_report.candidates.push(primary.report);
        listing_report.candidates.push(replacement_attempt.report);
    }

    if let Some(reason) = review_revision.stale_reason.as_ref() {
        blocking_reasons.push(format!(
            "pending review revision could not be advanced safely: {reason}"
        ));
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

    let mut accepted_links = Vec::with_capacity(prepared.len());
    for link in prepared {
        let Some(authorization) = link.authorization else {
            listing_report.status = "blocked".to_string();
            listing_report.error = Some(format!(
                "catalog id {} lost its automatic association authorization",
                link.avionics_model_id
            ));
            return listing_report;
        };
        let Some(expected_collision_closure_sha256) = link.expected_collision_closure_sha256 else {
            listing_report.status = "blocked".to_string();
            listing_report.error = Some(format!(
                "catalog id {} lost its resolution-time collision-closure revision",
                link.avionics_model_id
            ));
            return listing_report;
        };
        if link.replaces_avionics_model_id.is_some()
            && (link.expected_replacement_collision_closure_sha256.is_none()
                || link.replacement_authorization.is_none())
        {
            listing_report.status = "blocked".to_string();
            listing_report.error = Some(format!(
                "replacement for catalog id {} lost its resolution-time collision-closure revision",
                link.avionics_model_id
            ));
            return listing_report;
        }
        accepted_links.push(AutomatedAvionicsLink {
            avionics_model_id: link.avionics_model_id,
            authorization,
            expected_collision_closure_sha256,
            quantity: link.quantity,
            source_notes: link.source_notes,
            source_confidence: link.source_confidence,
            configuration_action: link.configuration_action,
            replaces_avionics_model_id: link.replaces_avionics_model_id,
            replacement_authorization: link.replacement_authorization,
            expected_replacement_collision_closure_sha256: link
                .expected_replacement_collision_closure_sha256,
            preserved_association_guard: link.preserved_association_guard,
        });
    }
    let request = AutomatedReviewApplyRequest {
        listing_id: row.listing_id,
        plugin_submission_id: submission_id,
        expected_review_payload_sha256: review_revision.current_sha256,
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

fn mark_weak_listing_evidence(report: &mut AvionicsVerificationCandidateReport) {
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
        reviewer_correction_association_binding: None,
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
        reviewer_correction_association_binding: None,
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
                && existing
                    .expected_replacement_collision_closure_sha256
                    .is_none()
                && incoming
                    .expected_replacement_collision_closure_sha256
                    .is_none()
        }
        "replaces" | "removes" => {
            existing.replacement_identity_key.is_some()
                && existing.replacement_identity_key == incoming.replacement_identity_key
                && existing.expected_replacement_collision_closure_sha256
                    == incoming.expected_replacement_collision_closure_sha256
        }
        _ => false,
    };
    if !same_action
        || !compatible_replacement
        || existing.expected_collision_closure_sha256 != incoming.expected_collision_closure_sha256
        || existing.preserved_association_guard != incoming.preserved_association_guard
        || existing.authorization != incoming.authorization
        || existing.replacement_authorization != incoming.replacement_authorization
    {
        return Err(format!(
            "catalog id {} resolved from multiple raw rows with conflicting action, replacement, or collision-closure semantics",
            existing.avionics_model_id
        ));
    }
    existing.quantity = existing.quantity.max(incoming.quantity);
    // One durable association corroboration must point to one exact source
    // span. Joining independently extracted excerpts manufactures text that
    // may not occur contiguously in the retained listing.
    if existing.source_notes.is_none() {
        existing.source_notes = incoming.source_notes.clone();
    }
    existing.source_confidence = conservative_confidence(
        existing.source_confidence.as_deref(),
        incoming.source_confidence.as_deref(),
    );
    Ok(())
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
    review_revision: &mut PendingReviewRevisionCursor,
) -> IdentityAttempt {
    let request = identity_request(
        row,
        source_url,
        listing_context,
        &identity,
        source_evidence_text,
    );
    let mut grounded_receipt: Option<GroundedAvionicsResolutionReceipt> = None;
    let outcome = if apply {
        match resolve_avionics_identity_for_automated_review(
            db,
            extractor,
            row.listing_id,
            &request,
        )
        .await
        {
            Ok(resolution) => {
                if let Err(error) =
                    review_revision.advance(&resolution.pending_review_revision_receipts)
                {
                    return IdentityAttempt {
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
                            error,
                        ),
                        approved_id: None,
                        identity_key: None,
                        collision_closure_sha256: None,
                        authorization: None,
                        suggested_product: None,
                    };
                }
                grounded_receipt = resolution.grounded_receipt;
                Ok(resolution.outcome)
            }
            Err(error) => Err(error),
        }
    } else {
        preview_avionics_identity(db, extractor, &request).await
    };
    match outcome {
        Ok(AvionicsIdentityOutcome::Approved(approved)) => {
            let suggested_product = Some(review_product_from_approved(&approved));
            let mut authorization = None;
            if apply && approved.id > 0 {
                let reuse_is_current = match product_reuse_attestation_is_current(db, approved.id)
                    .await
                {
                    Ok(reuse_is_current) => reuse_is_current,
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
                                    format!(
                                        "approved product reuse eligibility could not be checked: {error}"
                                    ),
                                ),
                                approved_id: None,
                                identity_key: None,
                                collision_closure_sha256: None,
                                authorization: None,
                                suggested_product,
                            };
                    }
                };
                if reuse_is_current {
                    authorization = Some(AutomatedAssociationAuthorization::ManufacturerReuse);
                } else if let Some(receipt) = grounded_receipt.take().filter(|receipt| {
                    receipt.listing_id == row.listing_id
                        && receipt.avionics_model_id == approved.id
                        && receipt.resolution_sha256.len() == 64
                        && receipt
                            .resolution_sha256
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                }) {
                    authorization =
                        Some(AutomatedAssociationAuthorization::SameCaseGrounded(receipt));
                } else {
                    if approved.id > 0 {
                        catalog_statuses.insert(approved.id, "approved".to_string());
                    }
                    return IdentityAttempt {
                        report: outcome_report(
                            candidate_index,
                            role,
                            &identity,
                            configuration_action,
                            source_evidence_text,
                            source_confidence,
                            "unresolved",
                            Some(approved.id),
                            Some(approved.manufacturer),
                            Some(approved.model),
                            approved.avionics_types,
                            "the product identity is approved, but this resolution produced neither manufacturer reuse nor a same-case grounded association authorization"
                                .to_string(),
                        ),
                        approved_id: None,
                        identity_key: None,
                        collision_closure_sha256: None,
                        authorization: None,
                        suggested_product,
                    };
                }
            }
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
                                collision_closure_sha256: None,
                                authorization: None,
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
            let mut attempt = approved_attempt(
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
            );
            attempt.authorization = authorization;
            if apply {
                if let Some(model_id) = attempt.approved_id {
                    let collision_revision = match attempt.authorization.as_ref() {
                        Some(AutomatedAssociationAuthorization::ManufacturerReuse) => {
                            active_collision_closure_revision_sha256(db, model_id).await
                        }
                        Some(AutomatedAssociationAuthorization::SameCaseGrounded(_)) => {
                            grounded_collision_closure_revision_sha256(db, model_id).await
                        }
                        None => Err(crate::listing::review::ReviewError::Conflict(format!(
                            "catalog id {model_id} has no automatic association authorization"
                        ))),
                    };
                    match collision_revision {
                        Ok(revision) => attempt.collision_closure_sha256 = Some(revision),
                        Err(error) => {
                            attempt.report.status = "error".to_string();
                            attempt.report.reason = format!(
                                "approved identity could not be bound to its active collision closure: {error}"
                            );
                            attempt.approved_id = None;
                            attempt.identity_key = None;
                        }
                    }
                }
            }
            attempt
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
            collision_closure_sha256: None,
            authorization: None,
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
            collision_closure_sha256: None,
            authorization: None,
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
            collision_closure_sha256: None,
            authorization: None,
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
            collision_closure_sha256: None,
            authorization: None,
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
            collision_closure_sha256: None,
            authorization: None,
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
        collision_closure_sha256: None,
        authorization: None,
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
) -> VerificationResult<String> {
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
        AvionicsVerificationError::Validation(format!(
            "approved catalog id {avionics_model_id} has no stable product identity"
        ))
    })?;
    approved_avionics_product_key(manufacturer_identity_id, &product_key)
        .map_err(AvionicsVerificationError::Validation)
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
) -> AvionicsVerificationCandidateReport {
    AvionicsVerificationCandidateReport {
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
) -> AvionicsVerificationCandidateReport {
    AvionicsVerificationCandidateReport {
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

fn generic_model_issue(raw: &ParsedAvionics) -> Option<String> {
    if is_generic_avionics_model_name(&raw.model) {
        return Some(format!(
            "{} is a capability, service, or generic equipment label rather than a specific avionics product",
            raw.model.trim()
        ));
    }
    None
}

fn raw_candidate_structure_issue(raw: &ParsedAvionics) -> Option<String> {
    if raw.manufacturer.trim().is_empty() || raw.model.trim().is_empty() {
        return Some("manufacturer and model must both be non-empty identity labels".to_string());
    }
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
                replacement.manufacturer.trim().is_empty() || replacement.model.trim().is_empty()
            }) =>
        {
            Some(
                "replacement identity requires non-empty manufacturer and model labels".to_string(),
            )
        }
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

struct RetainedReviewObservations {
    avionics: Vec<ParsedAvionics>,
    preserved_aspects: Vec<PendingReviewAspect>,
}

enum RetainedReviewObservationSource {
    Current(RetainedReviewObservations),
    RequiresFallback {
        preserved_aspects: Vec<PendingReviewAspect>,
    },
}

enum RetainedObservationSource {
    Review {
        avionics: Vec<ParsedAvionics>,
        preserved_aspects: Vec<PendingReviewAspect>,
    },
    Extraction {
        avionics: Vec<ParsedAvionics>,
        preserved_aspects: Vec<PendingReviewAspect>,
    },
    RequiresReextraction {
        reason: String,
        preserved_aspects: Vec<PendingReviewAspect>,
    },
}

async fn retained_observation_source(
    db: &AppDb,
    row: &ListingSourceRow,
    listing_context: &ListingEvidenceContext,
) -> Result<RetainedObservationSource, String> {
    let preserved_aspects = match retained_review_observations(db, row, listing_context).await? {
        RetainedReviewObservationSource::Current(replay) => {
            return Ok(RetainedObservationSource::Review {
                avionics: replay.avionics,
                preserved_aspects: replay.preserved_aspects,
            });
        }
        RetainedReviewObservationSource::RequiresFallback { preserved_aspects } => {
            preserved_aspects
        }
    };
    Ok(
        match retained_avionics_source(row.extracted_listing_json.as_deref(), listing_context) {
            RetainedAvionicsSource::Current(avionics) => RetainedObservationSource::Extraction {
                avionics,
                preserved_aspects,
            },
            RetainedAvionicsSource::RequiresReextraction { reason } => {
                RetainedObservationSource::RequiresReextraction {
                    reason,
                    preserved_aspects,
                }
            }
        },
    )
}

async fn retained_review_observations(
    db: &AppDb,
    row: &ListingSourceRow,
    listing_context: &ListingEvidenceContext,
) -> Result<RetainedReviewObservationSource, String> {
    let aspects = parse_current_pending_review_aspects(
        &row.review_payload_json,
        &row.review_payload_sha256,
        row.pending_aspect_count,
    )
    .map_err(|error| format!("pending review cannot be replayed safely: {error}"))?;
    let mut avionics = Vec::new();
    let preserved_aspects = aspects
        .iter()
        .filter(|aspect| aspect.kind != "avionics")
        .cloned()
        .collect::<Vec<_>>();
    for aspect in aspects.iter().filter(|aspect| aspect.kind == "avionics") {
        let evidence_is_current = aspect
            .source_evidence_text
            .as_deref()
            .is_some_and(|evidence| !evidence.trim().is_empty())
            && aspect
                .source_confidence
                .as_deref()
                .is_some_and(|confidence| matches!(confidence, "high" | "medium" | "low"));
        let independent_installation = aspect.configuration_action == "installed"
            && aspect.replaces_product_id.is_none()
            && aspect.replacement_aspect_id.is_none();
        if !evidence_is_current || !independent_installation {
            return Ok(RetainedReviewObservationSource::RequiresFallback { preserved_aspects });
        }
        let product = if let Some(product) = aspect.proposed_product.as_ref() {
            if product.capabilities.is_empty()
                || product
                    .capabilities
                    .iter()
                    .any(|capability| capability.trim().is_empty())
            {
                return Ok(RetainedReviewObservationSource::RequiresFallback { preserved_aspects });
            }
            product.clone()
        } else {
            let Some(product) =
                replay_current_verified_suggestion(db, row, listing_context, &aspect).await?
            else {
                return Ok(RetainedReviewObservationSource::RequiresFallback { preserved_aspects });
            };
            product
        };
        avionics.push(ParsedAvionics {
            manufacturer: product.manufacturer,
            model: product.model,
            avionics_types: product.capabilities,
            quantity: aspect.quantity,
            configuration_action: aspect.configuration_action.clone(),
            replaces: None,
            source_evidence_text: aspect.source_evidence_text.clone(),
            source_confidence: aspect.source_confidence.clone(),
        });
    }
    if (!avionics.is_empty()
        && validate_exact_listing_evidence(&avionics, listing_context).is_err())
        || (avionics.is_empty() && preserved_aspects.is_empty())
    {
        return Ok(RetainedReviewObservationSource::RequiresFallback { preserved_aspects });
    }
    Ok(RetainedReviewObservationSource::Current(
        RetainedReviewObservations {
            avionics,
            preserved_aspects,
        },
    ))
}

async fn prepare_current_preserved_associations(
    db: &AppDb,
    row: &ListingSourceRow,
    aspects: Vec<PendingReviewAspect>,
) -> Result<(Vec<PreparedLink>, Vec<PendingReviewAspect>), String> {
    let aspect_ids = aspects
        .iter()
        .filter(|aspect| aspect.kind == "avionics_reuse_attestation")
        .map(|aspect| aspect.id.clone())
        .collect::<Vec<_>>();
    if aspect_ids.is_empty() {
        return Ok((Vec::new(), aspects));
    }

    let catalog_revision_sha256 = approved_catalog_revision_sha256(db)
        .await
        .map_err(|error| error.to_string())?;
    let evaluations = evaluate_existing_product_associations(
        db,
        row.listing_owner_user_id,
        row.listing_id,
        &aspect_ids,
        &row.review_payload_sha256,
        &catalog_revision_sha256,
    )
    .await
    .map_err(|error| error.to_string())?;
    let mut evaluations = aspect_ids
        .into_iter()
        .zip(evaluations)
        .collect::<HashMap<_, _>>();
    let mut prepared = Vec::new();
    let mut residual = Vec::new();

    for aspect in aspects {
        let Some(evaluation) = evaluations.remove(&aspect.id) else {
            residual.push(aspect);
            continue;
        };
        let ExistingProductAssociationEvaluation::AutoVerifiable(target) = evaluation else {
            residual.push(aspect);
            continue;
        };
        let ExistingProductAssociationCommit::CorroboratePreserved { observation_sha256 } =
            target.commit
        else {
            residual.push(aspect);
            continue;
        };
        let Some(avionics_model_id) = target.product.id.filter(|id| *id > 0) else {
            residual.push(aspect);
            continue;
        };
        let identity_key = load_approved_graph_identity_key(db, avionics_model_id)
            .await
            .map_err(|error| error.to_string())?;
        let collision_closure_sha256 =
            active_collision_closure_revision_sha256(db, avionics_model_id)
                .await
                .map_err(|error| error.to_string())?;
        prepared.push(PreparedLink {
            identity_key,
            avionics_model_id,
            authorization: Some(AutomatedAssociationAuthorization::ManufacturerReuse),
            expected_collision_closure_sha256: Some(collision_closure_sha256),
            quantity: aspect.quantity,
            source_notes: Some(target.listing_evidence_text),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: Some(AutomatedPreservedAssociationGuard {
                listing_link_id: aspect.covered_associations[0].listing_link_id,
                association_role: ListingAssociationRole::Installed,
                expected_observation_sha256: observation_sha256,
            }),
        });
    }

    Ok((prepared, residual))
}

/// Replay a staged suggestion only when the current catalog independently
/// proves both claims needed for an automatic listing association.
///
/// The suggestion is a retrieval hint, never authority: its exact ID must
/// resolve from the listing excerpt to the current graph-approved product,
/// whose current-policy reuse attestation supplies the curated capabilities.
/// Any stale, ambiguous, weak-evidence, or unattested suggestion falls back to
/// the ordinary extraction/grounding workflow.
async fn replay_current_verified_suggestion(
    db: &AppDb,
    row: &ListingSourceRow,
    listing_context: &ListingEvidenceContext,
    aspect: &PendingReviewAspect,
) -> Result<Option<ReviewProduct>, String> {
    if aspect.source_confidence.as_deref() != Some("high")
        || !aspect
            .allowed_actions
            .contains(&ReviewAction::UseVerifiedProduct)
    {
        return Ok(None);
    }
    let Some(suggested) = aspect.suggested_product.as_ref() else {
        return Ok(None);
    };
    let Some(suggested_id) = suggested.id.filter(|id| *id > 0) else {
        return Ok(None);
    };
    if suggested.capabilities.is_empty()
        || suggested
            .capabilities
            .iter()
            .any(|capability| capability.trim().is_empty())
    {
        return Ok(None);
    }
    let identity = IdentityInput {
        manufacturer: &suggested.manufacturer,
        model: &suggested.model,
        avionics_types: &suggested.capabilities,
        quantity: aspect.quantity,
    };
    let request = identity_request(
        row,
        row.submission_source_url.as_deref(),
        listing_context,
        &identity,
        aspect.source_evidence_text.as_deref(),
    );
    let resolved = resolve_verified_local_avionics_identity(db, &request)
        .await
        .map_err(|error| format!("suggested product local replay failed: {error}"))?;
    let Some(resolved) = resolved.filter(|resolved| resolved.id == suggested_id) else {
        return Ok(None);
    };
    Ok(Some(review_product_from_approved(&resolved)))
}

fn retained_avionics_source(
    raw_json: Option<&str>,
    listing_context: &ListingEvidenceContext,
) -> RetainedAvionicsSource {
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
        Ok(avionics) => match validate_exact_listing_evidence(&avionics, listing_context) {
            Ok(()) => RetainedAvionicsSource::Current(avionics),
            Err(error) => RetainedAvionicsSource::RequiresReextraction {
                reason: format!(
                    "the retained plugin extraction has invalid listing evidence: {error}"
                ),
            },
        },
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
    listing_context: &ListingEvidenceContext,
) -> Result<Vec<ParsedAvionics>, String> {
    let extracted = extractor
        .extract(listing_text)
        .await
        .map_err(|error| format!("Gemini listing extraction request failed: {error}"))?;
    let avionics = parse_raw_avionics_value(&extracted).map_err(|error| {
        format!(
            "Gemini returned output incompatible with the current capability-array schema: {error}"
        )
    })?;
    validate_exact_listing_evidence(&avionics, listing_context).map_err(|error| {
        format!("Gemini returned listing evidence not present in the retained source: {error}")
    })?;
    Ok(avionics)
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
            validate_listing_evidence_fields(value, &format!("avionics[{index}]"))?;
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

fn validate_listing_evidence_fields(value: &Value, path: &str) -> Result<(), String> {
    let evidence = value
        .get("source_evidence_text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|evidence| !evidence.is_empty())
        .ok_or_else(|| {
            format!(
                "{path}.source_evidence_text must be one non-empty exact listing-source excerpt"
            )
        })?;
    if evidence.len() > crate::listing::evidence::MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES {
        return Err(format!(
            "{path}.source_evidence_text exceeds the bounded listing-evidence limit"
        ));
    }
    let confidence = value
        .get("source_confidence")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{path}.source_confidence must be high, medium, or low"))?;
    if !matches!(confidence, "high" | "medium" | "low") {
        return Err(format!(
            "{path}.source_confidence must be high, medium, or low"
        ));
    }
    Ok(())
}

fn validate_exact_listing_evidence(
    avionics: &[ParsedAvionics],
    listing_context: &ListingEvidenceContext,
) -> Result<(), String> {
    for (index, observation) in avionics.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("schema validation requires listing evidence")
            .trim();
        let bounded_context = listing_context.for_candidate(
            &observation.manufacturer,
            &observation.model,
            Some(evidence),
        );
        if !bounded_context.contains(evidence) {
            return Err(format!(
                "avionics[{index}].source_evidence_text is not one exact bounded excerpt containing the candidate identity"
            ));
        }
    }
    Ok(())
}

async fn load_listing_sources(
    db: &AppDb,
    scope: &AvionicsVerificationScope,
) -> VerificationResult<ListingSourcePage> {
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
          pending.id AS pending_review_id,
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

async fn load_listing_verification_state(
    db: &AppDb,
    listing_id: i64,
) -> VerificationResult<ListingVerificationStateRow> {
    let sql =
        db.sql("SELECT ingestion_state, is_verified FROM aircraft_sale_listings WHERE id = ?");
    let state = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ListingVerificationStateRow>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ListingVerificationStateRow>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await?
        }
    };
    state.ok_or_else(|| {
        AvionicsVerificationError::Validation(format!("listing {listing_id} not found"))
    })
}

async fn load_catalog_statuses(db: &AppDb) -> VerificationResult<HashMap<i64, String>> {
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

fn validate_prepared_links(links: &[PreparedLink]) -> VerificationResult<()> {
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
                return Err(AvionicsVerificationError::Validation(format!(
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
        .map_err(AvionicsVerificationError::Validation)?;
    Ok(())
}

fn summarize(listings: &[AvionicsVerificationListingReport]) -> AvionicsVerificationSummary {
    let mut summary = AvionicsVerificationSummary {
        listings_selected: listings.len(),
        ..AvionicsVerificationSummary::default()
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    use axum::{extract::State, routing::post, Json, Router};
    use serde_json::json;

    use super::*;
    use crate::avionics::manufacturer::{
        ensure_manufacturer_identity, ManufacturerIdentityEvidence,
    };
    use crate::avionics::reuse::refresh_reuse_attestation_sqlite;
    use crate::normalize::{
        normalize_avionics_identifier, normalize_avionics_manufacturer_name,
        normalize_avionics_model_name, normalize_name,
    };

    fn sqlite_pool(db: &AppDb) -> &sqlx::SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("verification tests require SQLite")
        };
        pool
    }

    fn revision_receipt(
        listing_id: i64,
        pending_review_id: i64,
        before_sha256: &str,
        after_sha256: &str,
    ) -> PendingReviewRevisionReceipt {
        PendingReviewRevisionReceipt {
            listing_id,
            pending_review_id,
            before_sha256: before_sha256.to_string(),
            after_sha256: after_sha256.to_string(),
        }
    }

    #[test]
    fn pending_review_revision_cursor_advances_only_a_contiguous_listing_chain() {
        let mut cursor = PendingReviewRevisionCursor {
            listing_id: 41,
            pending_review_id: 38,
            current_sha256: "h0".to_string(),
            stale_reason: None,
        };

        cursor
            .advance(&[
                revision_receipt(75, 91, "other-before", "other-after"),
                revision_receipt(41, 38, "h0", "h1"),
                revision_receipt(41, 38, "h1", "h2"),
            ])
            .unwrap();

        assert_eq!(cursor.current_sha256, "h2");
        assert_eq!(cursor.stale_reason, None);
    }

    #[test]
    fn pending_review_revision_cursor_does_not_adopt_an_unrelated_listing_revision() {
        let mut cursor = PendingReviewRevisionCursor {
            listing_id: 41,
            pending_review_id: 38,
            current_sha256: "h0".to_string(),
            stale_reason: None,
        };

        cursor
            .advance(&[revision_receipt(75, 91, "h0", "h1")])
            .unwrap();

        assert_eq!(cursor.current_sha256, "h0");
        assert_eq!(cursor.stale_reason, None);
    }

    #[test]
    fn pending_review_revision_cursor_rejects_a_broken_chain_and_stays_stale() {
        let mut cursor = PendingReviewRevisionCursor {
            listing_id: 41,
            pending_review_id: 38,
            current_sha256: "h0".to_string(),
            stale_reason: None,
        };

        let error = cursor
            .advance(&[revision_receipt(41, 38, "unexpected", "h1")])
            .unwrap_err();
        assert!(error.contains("chain is stale"));
        assert_eq!(cursor.current_sha256, "h0");
        assert_eq!(cursor.stale_reason.as_deref(), Some(error.as_str()));
        assert_eq!(
            cursor
                .advance(&[revision_receipt(41, 38, "h0", "h1")])
                .unwrap_err(),
            error
        );
        assert_eq!(cursor.current_sha256, "h0");
    }

    #[test]
    fn pending_review_revision_cursor_rejects_a_different_review_for_its_listing() {
        let mut cursor = PendingReviewRevisionCursor {
            listing_id: 41,
            pending_review_id: 38,
            current_sha256: "h0".to_string(),
            stale_reason: None,
        };

        let error = cursor
            .advance(&[revision_receipt(41, 39, "h0", "h1")])
            .unwrap_err();

        assert!(error.contains("pending review 39"));
        assert_eq!(cursor.current_sha256, "h0");
    }

    #[derive(Clone)]
    struct ClassifierEndpointState {
        confidence: &'static str,
        request_count: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<Value>>>,
    }

    async fn classifier_endpoint_response(
        State(state): State<ClassifierEndpointState>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        state.request_count.fetch_add(1, Ordering::SeqCst);
        state.requests.lock().unwrap().push(request);
        let content = json!({
            "classification": "generic",
            "manufacturer_is_avionics_maker": true,
            "model_identifies_single_unit": false,
            "confidence": state.confidence,
            "generic_indicators": ["GPS is an equipment capability rather than one model designation"],
            "notes": "The observation does not name one product."
        })
        .to_string();
        Json(json!({
            "candidates": [{
                "content": {"parts": [{"text": content}]}
            }]
        }))
    }

    async fn spawn_classifier_endpoint(
        confidence: &'static str,
    ) -> (
        String,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = ClassifierEndpointState {
            confidence,
            request_count: request_count.clone(),
            requests: requests.clone(),
        };
        let app = Router::new()
            .route("/", post(classifier_endpoint_response))
            .with_state(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            format!("http://{address}/"),
            request_count,
            requests,
            server,
        )
    }

    #[derive(Clone)]
    struct EmptyExtractionEndpointState {
        request_count: Arc<AtomicUsize>,
    }

    async fn empty_extraction_endpoint_response(
        State(state): State<EmptyExtractionEndpointState>,
        Json(_request): Json<Value>,
    ) -> Json<Value> {
        state.request_count.fetch_add(1, Ordering::SeqCst);
        let content = json!({
            "manufacturer": "Cessna",
            "model": "172",
            "variant": "S",
            "model_year": 2020,
            "asking_price_usd": 300000,
            "currency": "USD",
            "airframe_hours": 1000,
            "engine_hours": null,
            "engine_time_basis": "unknown",
            "engine_time_evidence": null,
            "engine_time_confidence": null,
            "propeller_hours": null,
            "propeller_time_basis": "unknown",
            "propeller_time_evidence": null,
            "propeller_time_confidence": null,
            "installed_engine": null,
            "installed_propeller": null,
            "registration_number": "N12345",
            "serial_number": null,
            "status": "active",
            "avionics": [],
            "valuation_facts": []
        })
        .to_string();
        Json(json!({
            "candidates": [{
                "content": {"parts": [{"text": content}]}
            }]
        }))
    }

    async fn spawn_empty_extraction_endpoint(
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let state = EmptyExtractionEndpointState {
            request_count: request_count.clone(),
        };
        let app = Router::new()
            .route("/", post(empty_extraction_endpoint_response))
            .with_state(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/"), request_count, server)
    }

    #[test]
    fn provider_request_plan_counts_logical_stages_not_transport_attempts() {
        let plan = provider_request_plan(&AvionicsVerificationPreflightSummary {
            listings_requiring_legacy_reextraction: 2,
            retained_identity_components: 8,
            verified_local_identity_components: 1,
            candidate_adjudication_identity_components: 2,
            candidate_adjudication_conditional_relationship_components: 1,
            candidate_triage_identity_components: 1,
            candidate_triage_conditional_relationship_components: 1,
            grounded_initial_identity_components: 3,
            grounded_conditional_relationship_components: 1,
            generic_invalid_identity_components: 2,
            ..AvionicsVerificationPreflightSummary::default()
        });

        assert_eq!(plan.listing_extraction_provider_requests_baseline, 2);
        assert_eq!(
            plan.listing_extraction_provider_requests_validation_envelope,
            4
        );
        assert_eq!(plan.candidate_adjudication_provider_requests_baseline, 2);
        assert_eq!(
            plan.candidate_grounded_fallback_provider_requests_baseline_maximum,
            12
        );
        assert_eq!(plan.candidate_triage_provider_requests_baseline, 1);
        assert_eq!(
            plan.candidate_triage_grounded_fallback_provider_requests_baseline_maximum,
            8
        );
        assert_eq!(plan.initial_grounded_provider_requests_baseline, 12);
        assert_eq!(
            plan.initial_grounded_provider_requests_nonpositive_validation_envelope,
            27
        );
        assert_eq!(plan.positive_identity_provider_requests_baseline, 28);
        assert_eq!(
            plan.positive_identity_provider_requests_validation_envelope,
            60
        );
        assert_eq!(plan.generic_invalid_identity_components, 2);
        assert_eq!(plan.generic_invalid_classifier_provider_requests, 2);
        assert_eq!(plan.known_total_provider_requests_minimum_baseline, 23);
        assert_eq!(plan.known_total_provider_requests_all_positive_baseline, 51);
        assert_eq!(
            plan.known_total_provider_requests_validation_envelope_maximum,
            146
        );
        assert!(plan.legacy_reextraction_identity_outputs_unknown);
        assert!(!plan.logical_provider_request_counts_include_transport_retries);
        assert_eq!(plan.default_max_transport_attempts_per_logical_request, 4);
        assert!(plan
            .transport_retry_note
            .contains("four transport attempts"));
        assert!(plan
            .grounded_pass_note
            .contains("exactly one tools-disabled"));
        assert!(plan
            .uncertainty_note
            .contains("does not run the concreteness classifier"));
    }

    #[test]
    fn verification_scope_rejects_ambiguous_cursor() {
        let error = AvionicsVerificationScope::new(10, Some(12), Some(11))
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

        let first =
            preflight_listing_avionics_page(&db, &AvionicsVerificationScope::new(1, None, None))
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

        let second = preflight_listing_avionics_page(
            &db,
            &AvionicsVerificationScope::new(1, None, Some(listing_ids[0])),
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
    async fn verification_rejects_before_gemini_and_link_replacement() {
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

        let report = verify_listing_avionics_page(
            &db,
            &extractor,
            AvionicsVerificationExecutionMode::Apply,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
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
                {
                  "manufacturer":"Garmin","model":"GTX 345R","types":["Transponder"],"quantity":1,
                  "source_evidence_text":"Garmin GTX 345R transponder",
                  "source_confidence":"high"
                },
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
        assert_eq!(
            parsed[0].source_evidence_text.as_deref(),
            Some("Garmin GTX 345R transponder")
        );
        assert_eq!(parsed[0].source_confidence.as_deref(), Some("high"));
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

        let aspects = input_error_aspects(
            3,
            &raw,
            raw_candidate_structure_issue(&raw).as_deref().unwrap(),
        );

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
    fn structurally_valid_generic_labels_are_eligible_for_the_discard_classifier() {
        for model in ["TAWS", "XM Weather & Radio", "Active Traffic", "AHRS"] {
            let raw = ParsedAvionics {
                manufacturer: "Garmin".to_string(),
                model: model.to_string(),
                avionics_types: vec!["Terrain Awareness".to_string()],
                quantity: 1,
                configuration_action: "installed".to_string(),
                replaces: None,
                source_evidence_text: Some(model.to_string()),
                source_confidence: Some("high".to_string()),
            };

            assert!(raw_candidate_structure_issue(&raw).is_none());
            let issue = generic_model_issue(&raw)
                .expect("a generic label must be eligible for classifier review");
            assert!(issue.contains("rather than a specific avionics product"));
        }
    }

    #[test]
    fn malformed_generic_labels_bypass_the_classifier_and_remain_for_review() {
        let raw = ParsedAvionics {
            manufacturer: "Garmin".to_string(),
            model: "GPS".to_string(),
            avionics_types: vec!["GPS".to_string()],
            quantity: 0,
            configuration_action: "replaces".to_string(),
            replaces: None,
            source_evidence_text: Some("Garmin GPS".to_string()),
            source_confidence: Some("high".to_string()),
        };

        assert!(generic_model_issue(&raw).is_some());
        assert_eq!(
            raw_candidate_structure_issue(&raw).as_deref(),
            Some("quantity must be at least 1")
        );

        let malformed_target = ParsedAvionics {
            quantity: 1,
            replaces: Some(crate::models::ParsedAvionicsReference {
                manufacturer: "".to_string(),
                model: "".to_string(),
                avionics_types: vec!["GPS".to_string()],
            }),
            ..raw
        };
        assert_eq!(
            raw_candidate_structure_issue(&malformed_target).as_deref(),
            Some("replacement identity requires non-empty manufacturer and model labels")
        );
    }

    #[tokio::test]
    async fn very_high_generic_current_observation_is_discarded_by_one_classifier_request() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_generic_listing(&db, "very-high-generic").await;
        let preflight = preflight_listing_avionics_page(
            &db,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();
        assert_eq!(preflight.listings[0].invalid_retained_observations, 1);
        assert_eq!(preflight.listings[0].generic_invalid_identity_components, 1);
        assert_eq!(
            preflight
                .provider_request_plan
                .generic_invalid_classifier_provider_requests,
            1
        );
        assert_eq!(
            preflight
                .provider_request_plan
                .known_total_provider_requests_minimum_baseline,
            1
        );

        let (endpoint, request_count, requests, server) =
            spawn_classifier_endpoint("very_high").await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let report = verify_listing_avionics_page(
            &db,
            &extractor,
            AvionicsVerificationExecutionMode::Preview,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();
        server.abort();

        let listing = &report.listings[0];
        assert_eq!(listing.status, "previewed");
        assert_eq!(listing.safely_discarded, 1);
        assert_eq!(listing.remaining_review_aspects, 0);
        assert_eq!(listing.candidates.len(), 1);
        assert_eq!(listing.candidates[0].status, "rejected");
        assert!(listing.candidates[0].resolution_attempted);
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].get("tools").is_none());
        let prompt = requests[0]["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(prompt.contains("GARMIN"));
        assert!(prompt.contains("GPS"));
    }

    #[tokio::test]
    async fn weaker_generic_classification_stays_in_review_without_grounding() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_generic_listing(&db, "high-generic").await;
        let (endpoint, request_count, _requests, server) = spawn_classifier_endpoint("high").await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let report = verify_listing_avionics_page(
            &db,
            &extractor,
            AvionicsVerificationExecutionMode::Preview,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();
        server.abort();

        let listing = &report.listings[0];
        assert_eq!(listing.status, "previewed");
        assert_eq!(listing.safely_discarded, 0);
        assert_eq!(listing.remaining_review_aspects, 1);
        assert_eq!(listing.candidates.len(), 1);
        assert_eq!(listing.candidates[0].status, "error");
        assert!(!listing.candidates[0].resolution_attempted);
        assert!(listing.candidates[0]
            .reason
            .contains("rather than a specific avionics product"));
        assert_eq!(
            request_count.load(Ordering::SeqCst),
            1,
            "a non-terminal classifier answer must remain pending without opening grounded calls"
        );
    }

    #[tokio::test]
    async fn failed_generic_classifier_request_stays_in_review() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_generic_listing(&db, "failed-generic-classifier").await;
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let report = verify_listing_avionics_page(
            &db,
            &extractor,
            AvionicsVerificationExecutionMode::Preview,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();

        let listing = &report.listings[0];
        assert_eq!(listing.status, "previewed");
        assert_eq!(listing.safely_discarded, 0);
        assert_eq!(listing.remaining_review_aspects, 1);
        assert_eq!(listing.candidates.len(), 1);
        assert_eq!(listing.candidates[0].status, "error");
        assert!(!listing.candidates[0].resolution_attempted);
    }

    #[test]
    fn legacy_scalar_type_requires_reextraction_without_mechanical_conversion() {
        let legacy = r#"{
          "avionics": [
            {"manufacturer":"Garmin","model":"GNX 375","type":"GPS","quantity":1}
          ]
        }"#;

        let source = retained_avionics_source(
            Some(legacy),
            &ListingEvidenceContext::from_cleaned_text("Garmin GNX 375"),
        );

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
              "quantity":1,
              "source_evidence_text":"Garmin GNX 375",
              "source_confidence":"high"
            }
          ]
        }"#;

        let source = retained_avionics_source(
            Some(current),
            &ListingEvidenceContext::from_cleaned_text("Garmin GNX 375"),
        );

        let RetainedAvionicsSource::Current(avionics) = source else {
            panic!("a current capability-array payload should be replayable")
        };
        assert_eq!(avionics.len(), 1);
        assert_eq!(avionics[0].avionics_types, vec!["GPS", "Transponder"]);
    }

    #[test]
    fn capability_array_without_exact_evidence_requires_reextraction() {
        let missing_evidence = r#"{
          "avionics": [{
            "manufacturer":"Garmin",
            "model":"GNX 375",
            "types":["GPS","Transponder"],
            "quantity":1
          }]
        }"#;

        let source = retained_avionics_source(
            Some(missing_evidence),
            &ListingEvidenceContext::from_cleaned_text("Garmin GNX 375"),
        );

        let RetainedAvionicsSource::RequiresReextraction { reason } = source else {
            panic!("an evidence-less extraction must never be replayed")
        };
        assert!(reason.contains("source_evidence_text"));
    }

    #[test]
    fn missing_or_invalid_capability_arrays_fail_closed_to_reextraction() {
        assert!(matches!(
            retained_avionics_source(None, &ListingEvidenceContext::default()),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert!(matches!(
            retained_avionics_source(
                Some(r#"{"avionics":[]}"#),
                &ListingEvidenceContext::default()
            ),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert!(matches!(
            retained_avionics_source(
                Some(r#"{"avionics":[{"manufacturer":"Garmin","model":"GTN 750Xi","types":[]}]}"#),
                &ListingEvidenceContext::default()
            ),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert!(matches!(
            retained_avionics_source(
                Some(
                    r#"{
                  "avionics":[{
                    "manufacturer":"Garmin","model":"GTN 750Xi","types":["GPS"],
                    "configuration_action":"replaces",
                    "replaces":{"manufacturer":"Garmin","model":"GNS 530W","type":"GPS"}
                  }]
                }"#
                ),
                &ListingEvidenceContext::default()
            ),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
    }

    #[test]
    fn fabricated_evidence_requires_reextraction() {
        let fabricated = r#"{
          "avionics": [{
            "manufacturer":"Garmin",
            "model":"GNX 375",
            "types":["GPS","Transponder"],
            "source_evidence_text":"Garmin GNX 375 with invented qualifier",
            "source_confidence":"high"
          }]
        }"#;

        let source = retained_avionics_source(
            Some(fabricated),
            &ListingEvidenceContext::from_cleaned_text("Garmin GNX 375 installed"),
        );

        let RetainedAvionicsSource::RequiresReextraction { reason } = source else {
            panic!("model-produced evidence absent from the source must never be replayed")
        };
        assert!(reason.contains("not one exact bounded excerpt"));
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
            authorization: None,
            expected_collision_closure_sha256: Some("a".repeat(64)),
            quantity: 1,
            source_notes: Some("GPS navigator".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: None,
        };
        let incoming = PreparedLink {
            identity_key: "catalog:42".to_string(),
            avionics_model_id: 42,
            authorization: None,
            expected_collision_closure_sha256: Some("a".repeat(64)),
            quantity: 2,
            source_notes: Some("Mode S transponder".to_string()),
            source_confidence: Some("medium".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: None,
        };

        merge_duplicate_link(&mut existing, &incoming).unwrap();

        assert_eq!(existing.quantity, 2);
        assert_eq!(existing.source_notes.as_deref(), Some("GPS navigator"));
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
            authorization: None,
            expected_collision_closure_sha256: Some("a".repeat(64)),
            quantity: 1,
            source_notes: Some("GNX 375 GPS navigator installed".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: None,
        }];
        let transponder_row = PreparedLink {
            identity_key: "catalog:375".to_string(),
            avionics_model_id: 375,
            authorization: None,
            expected_collision_closure_sha256: Some("a".repeat(64)),
            quantity: 1,
            source_notes: Some("GNX 375 transponder installed".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: None,
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
            Some("GNX 375 GPS navigator installed")
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
                authorization: None,
                expected_collision_closure_sha256: None,
                quantity: 1,
                source_notes: Some("GNX 375 GPS navigator".to_string()),
                source_confidence: Some("high".to_string()),
                configuration_action: "installed".to_string(),
                replaces_avionics_model_id: None,
                replacement_authorization: None,
                replacement_identity_key: None,
                expected_replacement_collision_closure_sha256: None,
                preserved_association_guard: None,
            },
        )
        .unwrap());
        assert!(merge_or_push_prepared_link(
            &mut prepared,
            PreparedLink {
                identity_key: transponder_key,
                avionics_model_id: 0,
                authorization: None,
                expected_collision_closure_sha256: None,
                quantity: 1,
                source_notes: Some("GNX 375 transponder".to_string()),
                source_confidence: Some("high".to_string()),
                configuration_action: "installed".to_string(),
                replaces_avionics_model_id: None,
                replacement_authorization: None,
                replacement_identity_key: None,
                expected_replacement_collision_closure_sha256: None,
                preserved_association_guard: None,
            },
        )
        .unwrap());
        assert_eq!(prepared.len(), 1);
        assert_eq!(
            prepared[0].source_notes.as_deref(),
            Some("GNX 375 GPS navigator")
        );
    }

    #[test]
    fn duplicate_capability_rows_with_conflicting_semantics_are_rejected() {
        let mut existing = PreparedLink {
            identity_key: "catalog:42".to_string(),
            avionics_model_id: 42,
            authorization: None,
            expected_collision_closure_sha256: Some("a".repeat(64)),
            quantity: 1,
            source_notes: None,
            source_confidence: None,
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: None,
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
            authorization: None,
            expected_collision_closure_sha256: (id > 0).then(|| "a".repeat(64)),
            quantity: 1,
            source_notes: None,
            source_confidence: Some("high".to_string()),
            configuration_action: action.to_string(),
            replaces_avionics_model_id: target_id,
            replacement_authorization: None,
            replacement_identity_key: target_key.map(ToString::to_string),
            expected_replacement_collision_closure_sha256: target_id.map(|_| "b".repeat(64)),
            preserved_association_guard: None,
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
                r#"{"avionics":[{"manufacturer":"Garmin","model":"Attached","types":["Transponder"],"source_evidence_text":"Attached GTX 345R installed","source_confidence":"high"}]}"#,
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
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
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
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
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
            retained_avionics_source(
                rows[0].extracted_listing_json.as_deref(),
                &ListingEvidenceContext::from_rendered_html(rows[0].rendered_html.as_deref())
            ),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert_eq!(
            validate_pending_source_binding(&rows[0]).unwrap().1,
            "canonical_listing_id"
        );
    }

    #[tokio::test]
    async fn current_exact_review_observations_bypass_stale_plugin_extraction() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_listing(&db, "https://example.test/listing/53").await;
        seed_submission_and_review(
            &db,
            listing_id,
            "https://example.test/listing/53",
            "<p>Garmin fixture installed</p>",
            Some(r#"{"avionics":[{"manufacturer":"Garmin","model":"Fixture","types":["GPS"]}]}"#),
            Some(listing_id),
            None,
        )
        .await;

        let row = load_listing_sources(
            &db,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap()
        .rows
        .pop()
        .unwrap();
        let listing_context =
            ListingEvidenceContext::from_rendered_html(row.rendered_html.as_deref());
        assert!(matches!(
            retained_avionics_source(row.extracted_listing_json.as_deref(), &listing_context),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));

        let RetainedReviewObservationSource::Current(replay) =
            retained_review_observations(&db, &row, &listing_context)
                .await
                .unwrap()
        else {
            panic!("the current exact pending review is the reusable work queue")
        };
        assert_eq!(replay.avionics.len(), 1);
        assert_eq!(replay.avionics[0].model, "Fixture");
        assert!(replay.preserved_aspects.is_empty());
    }

    #[tokio::test]
    async fn current_exact_suggested_product_is_provider_free_in_preflight_and_apply() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let listing_id = seed_suggestion_only_listing(
            &db,
            "eligible-suggested-product",
            product_id,
            product_id,
            "high",
        )
        .await;

        let preflight = preflight_listing_avionics(&db, listing_id)
            .await
            .expect("the exact current suggestion should preflight locally");
        let ListingAvionicsVerificationPreflight::PendingReview { report } = preflight else {
            panic!("the listing still has one pending review")
        };
        assert_eq!(report.status, "ready_retained_observations");
        assert!(!report.reextraction_required);
        assert_eq!(report.retained_identity_components, 1);
        assert_eq!(report.verified_local_identity_components, 1);

        let page_preflight = preflight_listing_avionics_page(
            &db,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();
        assert_eq!(
            page_preflight
                .provider_request_plan
                .known_total_provider_requests_minimum_baseline,
            0
        );
        assert_eq!(
            page_preflight
                .provider_request_plan
                .listing_extraction_provider_requests_baseline,
            0
        );

        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let applied = verify_listing_avionics(
            &db,
            &extractor,
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("the local replay must not contact the unavailable provider");
        let ListingAvionicsVerification::Processed { report } = applied else {
            panic!("the pending review should be processed")
        };
        assert_eq!(report.raw_avionics_source, "pending_review");
        assert!(!report.reextraction_required);
        assert!(!report.reextraction_attempted);
        assert_eq!(report.accepted, 1);
        assert_eq!(report.remaining_review_aspects, 0);

        let pool = sqlite_pool(&db);
        let link: (i64, String, i64) = sqlx::query_as(
            r#"
            SELECT avionics_model_id, source_confidence, quantity
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(link, (product_id, "high".to_string(), 1));
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let usage: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(pending, 0);
        assert_eq!(usage, 0);
    }

    #[tokio::test]
    async fn current_exact_preserved_association_is_consumed_without_provider_usage() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let (listing_id, link_id, _) = seed_preserved_association_listing(
            &db,
            "eligible-preserved-association",
            product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;

        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let applied = verify_listing_avionics(
            &db,
            &extractor,
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("an exact preserved association should use the local apply path");
        let ListingAvionicsVerification::Processed { report } = applied else {
            panic!("the pending review should be processed")
        };
        assert_eq!(report.status, "applied");
        assert_eq!(report.raw_avionics_source, "pending_review");
        assert!(!report.reextraction_required);
        assert!(!report.reextraction_attempted);
        assert_eq!(report.accepted, 1);
        assert_eq!(report.remaining_review_aspects, 0);

        let pool = sqlite_pool(&db);
        let persisted_link_id: i64 = sqlx::query_scalar(
            "SELECT id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let proof_counts: (i64, i64) = sqlx::query_as(
            r#"
            SELECT
              (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_corroborations
               WHERE listing_link_id = ?),
              (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_corroboration_scopes
               WHERE listing_link_id = ?)
            "#,
        )
        .bind(link_id)
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let pending_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let usage_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(persisted_link_id, link_id);
        assert_eq!(proof_counts, (1, 1));
        assert_eq!(pending_count, 0);
        assert_eq!(usage_count, 0);
    }

    #[tokio::test]
    async fn mixed_fallback_preserves_synthetic_cards_without_duplicating_ordinary_review() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let (listing_id, link_id, preserved_aspect_id) = seed_preserved_association_listing(
            &db,
            "mixed-preserved-and-nonreplayable",
            product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;
        let pool = sqlite_pool(&db);
        let (submission_id, review_json, review_sha256, pending_aspect_count, rendered_html): (
            i64,
            String,
            String,
            i64,
            String,
        ) = sqlx::query_as(
            r#"
            SELECT review.plugin_submission_id, review.review_payload_json,
                   review.review_payload_sha256, review.pending_aspect_count,
                   submission.rendered_html
            FROM aircraft_sale_listing_pending_reviews review
            JOIN plugin_submissions submission
              ON submission.id = review.plugin_submission_id
            WHERE review.listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut aspects = parse_current_pending_review_aspects(
            &review_json,
            &review_sha256,
            pending_aspect_count,
        )
        .unwrap();
        let ineligible_product_id = seed_approved_suggestion_product(&db, true).await;
        let (ineligible_manufacturer, ineligible_model): (String, String) = sqlx::query_as(
            r#"
            SELECT manufacturer.name, model.name
            FROM avionics_models model
            JOIN avionics_manufacturers manufacturer
              ON manufacturer.id = model.avionics_manufacturer_id
            WHERE model.id = ?
            "#,
        )
        .bind(ineligible_product_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let ineligible_evidence =
            format!("{ineligible_manufacturer} {ineligible_model} backup display");
        let ineligible_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', ?, 'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(ineligible_product_id)
        .bind(&ineligible_evidence)
        .fetch_one(pool)
        .await
        .unwrap();
        let ineligible_aspect_id =
            ReviewAspectId::from(format!("avionics:preserved:{ineligible_link_id}:installed"));
        aspects.push(
            PendingReviewAspect::avionics(
                ineligible_aspect_id.clone(),
                "avionics_reuse_attestation",
                format!("{ineligible_manufacturer} {ineligible_model}"),
                format!("{ineligible_manufacturer} {ineligible_model}"),
                "catalog_product_or_listing_corroboration_missing",
                1,
                "installed",
                None,
                None,
            )
            .with_covered_association(
                ineligible_link_id,
                ListingAssociationRole::Installed,
                ineligible_product_id,
            )
            .with_reuse_attestation_target(ineligible_product_id),
        );
        let ordinary_aspect_id = ReviewAspectId::from("fixture:mixed:replacement");
        let ordinary_evidence = "Unknown replacement package";
        aspects.push(
            PendingReviewAspect::avionics(
                ordinary_aspect_id.clone(),
                "avionics",
                "Unknown replacement package",
                "Unknown replacement package",
                "replacement relationship requires review",
                1,
                "replaces",
                Some(ordinary_evidence.to_string()),
                Some("high".to_string()),
            )
            .with_proposed_product(ReviewProduct::proposed(
                "Unknown",
                "Replacement Package",
                vec!["GPS".to_string()],
            ))
            .with_replacement_product(product_id),
        );
        let rendered_html =
            format!("{rendered_html}<p>{ineligible_evidence}</p><p>{ordinary_evidence}</p>");
        let extracted_listing_json = format!(
            r#"{{"avionics":[{{"manufacturer":"","model":"Replacement Package","types":["GPS"],"quantity":1,"configuration_action":"installed","source_evidence_text":"{ordinary_evidence}","source_confidence":"high"}}]}}"#
        );
        sqlx::query(
            r#"
            UPDATE plugin_submissions
            SET rendered_html = ?, rendered_html_sha256 = ?,
                extracted_listing_json = ?, extraction_error = NULL
            WHERE id = ?
            "#,
        )
        .bind(&rendered_html)
        .bind(sha256_hex(rendered_html.as_bytes()))
        .bind(extracted_listing_json)
        .bind(submission_id)
        .execute(pool)
        .await
        .unwrap();
        crate::listing::review::stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &aspects,
        )
        .await
        .unwrap();

        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let result = verify_listing_avionics(
            &db,
            &extractor,
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("mixed fallback should consume independent local work without a provider");
        let ListingAvionicsVerification::Processed { report } = result else {
            panic!("the mixed review should remain pending after partial local progress")
        };
        assert_eq!(report.status, "applied");
        assert_eq!(report.raw_avionics_source, "retained_extraction");
        assert!(!report.reextraction_attempted);
        assert_eq!(report.accepted, 1);
        assert_eq!(report.remaining_review_aspects, 2);

        let (retained_review_json, retained_review_sha256, retained_aspect_count): (
            String,
            String,
            i64,
        ) = sqlx::query_as(
            "SELECT review_payload_json, review_payload_sha256, pending_aspect_count FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let retained_aspects = parse_current_pending_review_aspects(
            &retained_review_json,
            &retained_review_sha256,
            retained_aspect_count,
        )
        .unwrap();
        let proof_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_corroborations WHERE listing_link_id = ?",
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let usage_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained_aspects.len(), 2);
        assert!(retained_aspects
            .iter()
            .any(|aspect| aspect.id == ineligible_aspect_id));
        assert!(!retained_aspects
            .iter()
            .any(|aspect| aspect.id == preserved_aspect_id));
        assert!(!retained_aspects
            .iter()
            .any(|aspect| aspect.id == ordinary_aspect_id));
        assert_eq!(proof_count, 1);
        assert_eq!(usage_count, 0);
    }

    #[tokio::test]
    async fn empty_reextraction_after_ordinary_review_fallback_preserves_all_prior_state() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let (listing_id, link_id, preserved_aspect_id) = seed_preserved_association_listing(
            &db,
            "empty-reextraction-preserves-prior-state",
            product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;
        let pool = sqlite_pool(&db);
        let (submission_id, review_json, review_sha256, pending_aspect_count, rendered_html): (
            i64,
            String,
            String,
            i64,
            String,
        ) = sqlx::query_as(
            r#"
            SELECT review.plugin_submission_id, review.review_payload_json,
                   review.review_payload_sha256, review.pending_aspect_count,
                   submission.rendered_html
            FROM aircraft_sale_listing_pending_reviews review
            JOIN plugin_submissions submission
              ON submission.id = review.plugin_submission_id
            WHERE review.listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut aspects = parse_current_pending_review_aspects(
            &review_json,
            &review_sha256,
            pending_aspect_count,
        )
        .unwrap();
        let ordinary_aspect_id = ReviewAspectId::from("fixture:empty-fallback:replacement");
        let ordinary_evidence = "Unknown replacement package";
        aspects.push(
            PendingReviewAspect::avionics(
                ordinary_aspect_id.clone(),
                "avionics",
                "Unknown replacement package",
                "Unknown replacement package",
                "replacement relationship requires review",
                1,
                "replaces",
                Some(ordinary_evidence.to_string()),
                Some("high".to_string()),
            )
            .with_proposed_product(ReviewProduct::proposed(
                "Unknown",
                "Replacement Package",
                vec!["GPS".to_string()],
            ))
            .with_replacement_product(product_id),
        );
        let rendered_html = format!("{rendered_html}<p>{ordinary_evidence}</p>");
        sqlx::query(
            r#"
            UPDATE plugin_submissions
            SET rendered_html = ?, rendered_html_sha256 = ?,
                extracted_listing_json = NULL,
                extraction_error = 'legacy extraction unavailable'
            WHERE id = ?
            "#,
        )
        .bind(&rendered_html)
        .bind(sha256_hex(rendered_html.as_bytes()))
        .bind(submission_id)
        .execute(pool)
        .await
        .unwrap();
        crate::listing::review::stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &aspects,
        )
        .await
        .unwrap();

        let review_before: (i64, String, String, i64) = sqlx::query_as(
            r#"
            SELECT id, review_payload_json, review_payload_sha256, pending_aspect_count
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let link_before: (
            i64,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<i64>,
        ) = sqlx::query_as(
            r#"
            SELECT id, avionics_model_id, quantity, source, source_notes,
                   source_confidence, configuration_action, replaces_avionics_model_id
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();

        let (endpoint, request_count, server) = spawn_empty_extraction_endpoint().await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let result = verify_listing_avionics(
            &db,
            &extractor,
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("a valid empty re-extraction should fail closed without mutating prior state");
        server.abort();
        let ListingAvionicsVerification::Processed { report } = result else {
            panic!("the complete prior review should remain pending")
        };
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert_eq!(report.status, "blocked");
        assert_eq!(report.raw_avionics_source, "gemini_reextraction");
        assert!(report.reextraction_required);
        assert!(report.reextraction_attempted);
        assert!(report.reextraction_succeeded);
        assert_eq!(report.accepted, 0);
        assert_eq!(report.remaining_review_aspects, 2);
        assert!(report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("complete prior review and links were retained")));

        let review_after: (i64, String, String, i64) = sqlx::query_as(
            r#"
            SELECT id, review_payload_json, review_payload_sha256, pending_aspect_count
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let link_after: (
            i64,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<i64>,
        ) = sqlx::query_as(
            r#"
            SELECT id, avionics_model_id, quantity, source, source_notes,
                   source_confidence, configuration_action, replaces_avionics_model_id
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let proof_counts: (i64, i64) = sqlx::query_as(
            r#"
            SELECT
              (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_corroborations
               WHERE listing_link_id = ?),
              (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_corroboration_scopes
               WHERE listing_link_id = ?)
            "#,
        )
        .bind(link_id)
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let retained_aspects =
            parse_current_pending_review_aspects(&review_after.1, &review_after.2, review_after.3)
                .unwrap();
        assert_eq!(review_after, review_before);
        assert_eq!(link_after, link_before);
        assert_eq!(proof_counts, (0, 0));
        assert!(retained_aspects
            .iter()
            .any(|aspect| aspect.id == preserved_aspect_id));
        assert!(retained_aspects
            .iter()
            .any(|aspect| aspect.id == ordinary_aspect_id));
    }

    #[tokio::test]
    async fn ineligible_preserved_associations_remain_pending_without_provider_usage() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let attested_product_id = seed_approved_suggestion_product(&db, true).await;
        let unattested_product_id = seed_approved_suggestion_product(&db, false).await;
        let fixtures = [
            (
                attested_product_id,
                PreservedAssociationFixture::MissingEvidence,
            ),
            (unattested_product_id, PreservedAssociationFixture::Exact),
            (
                attested_product_id,
                PreservedAssociationFixture::AmbiguousQualifier,
            ),
            (
                attested_product_id,
                PreservedAssociationFixture::Replacement,
            ),
        ];
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        for (index, (product_id, fixture)) in fixtures.into_iter().enumerate() {
            let suffix = format!("ineligible-preserved-association-{index}");
            let (listing_id, _, aspect_id) =
                seed_preserved_association_listing(&db, &suffix, product_id, fixture).await;
            let before_hash: String = sqlx::query_scalar(
                "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
            )
            .bind(listing_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();

            let result = verify_listing_avionics(
                &db,
                &extractor,
                AvionicsVerificationExecutionMode::Apply,
                listing_id,
            )
            .await
            .expect("ineligible preserved cards should remain pending without a provider call");
            let ListingAvionicsVerification::Processed { report } = result else {
                panic!("the listing should retain its pending review")
            };
            assert_eq!(report.status, "blocked");
            assert_eq!(report.raw_avionics_source, "pending_review");
            assert!(!report.reextraction_attempted);
            assert_eq!(report.accepted, 0);

            let retained: (String, String) = sqlx::query_as(
                "SELECT review_payload_sha256, review_payload_json FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
            )
            .bind(listing_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
            let usage_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
            )
            .bind(listing_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
            assert_eq!(retained.0, before_hash);
            assert!(retained.1.contains(&aspect_id.to_string()));
            assert_eq!(usage_count, 0);
        }
    }

    #[tokio::test]
    async fn weak_or_unattested_suggestions_do_not_bypass_reextraction() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let attested_product_id = seed_approved_suggestion_product(&db, true).await;
        let weak_listing_id = seed_suggestion_only_listing(
            &db,
            "weak-suggested-product",
            attested_product_id,
            attested_product_id,
            "medium",
        )
        .await;
        let unattested_product_id = seed_approved_suggestion_product(&db, false).await;
        let unattested_listing_id = seed_suggestion_only_listing(
            &db,
            "unattested-suggested-product",
            unattested_product_id,
            unattested_product_id,
            "high",
        )
        .await;
        let mismatched_id = seed_approved_suggestion_product(&db, true).await;
        let mismatched_listing_id = seed_suggestion_only_listing(
            &db,
            "mismatched-suggested-product",
            mismatched_id,
            attested_product_id,
            "high",
        )
        .await;

        for listing_id in [
            weak_listing_id,
            unattested_listing_id,
            mismatched_listing_id,
        ] {
            let preflight = preflight_listing_avionics(&db, listing_id).await.unwrap();
            let ListingAvionicsVerificationPreflight::PendingReview { report } = preflight else {
                panic!("the listing still has one pending review")
            };
            assert_eq!(report.status, "ready_legacy_reextraction");
            assert!(report.reextraction_required);
            assert_eq!(report.retained_identity_components, 0);
        }
    }

    #[tokio::test]
    async fn pending_review_with_non_source_evidence_falls_back_closed() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_listing(&db, "https://example.test/listing/54").await;
        seed_submission_and_review(
            &db,
            listing_id,
            "https://example.test/listing/54",
            "<p>No avionics identity appears here</p>",
            None,
            Some(listing_id),
            None,
        )
        .await;

        let row = load_listing_sources(
            &db,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap()
        .rows
        .pop()
        .unwrap();
        let listing_context =
            ListingEvidenceContext::from_rendered_html(row.rendered_html.as_deref());
        let RetainedReviewObservationSource::RequiresFallback { preserved_aspects } =
            retained_review_observations(&db, &row, &listing_context)
                .await
                .unwrap()
        else {
            panic!("non-source review evidence must require fallback")
        };
        assert!(preserved_aspects.is_empty());
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
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
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

    #[tokio::test]
    async fn per_listing_verification_is_a_provider_free_noop_without_pending_review() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_listing(&db, "https://example.test/listing/no-review").await;

        let preflight = preflight_listing_avionics(&db, listing_id)
            .await
            .expect("a listing without review should be a typed no-op");
        assert!(matches!(
            preflight,
            ListingAvionicsVerificationPreflight::NoPendingReview {
                listing_id: actual,
                ref ingestion_state,
                is_verified: false,
            } if actual == listing_id && ingestion_state == "incomplete"
        ));

        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let verification = verify_listing_avionics(
            &db,
            &extractor,
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("a repeated verifier must not require a provider");
        assert!(matches!(
            verification,
            ListingAvionicsVerification::NoPendingReview {
                listing_id: actual,
                ref ingestion_state,
                is_verified: false,
            } if actual == listing_id && ingestion_state == "incomplete"
        ));
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let usage_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(usage_count, 0);
    }

    async fn seed_generic_listing(db: &AppDb, suffix: &str) -> i64 {
        let source_url = format!("https://example.test/listing/{suffix}");
        let listing_id = seed_listing(db, &source_url).await;
        seed_faa_admission(db, listing_id).await;
        seed_submission_and_review(
            db,
            listing_id,
            &source_url,
            "<p>GARMIN GPS installed</p>",
            Some(
                r#"{"avionics":[{"manufacturer":"GARMIN","model":"GPS","types":["GPS"],"quantity":1,"configuration_action":"installed","source_evidence_text":"GARMIN GPS","source_confidence":"high"}]}"#,
            ),
            Some(listing_id),
            None,
        )
        .await;
        listing_id
    }

    async fn seed_faa_admission(db: &AppDb, listing_id: i64) {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query(
            "UPDATE aircraft_sale_listings SET registration_number = 'N123AB', serial_number = NULL WHERE id = ?",
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        let release_url =
            format!("https://www.faa.gov/aircraft-registry/test-release-{listing_id}.zip");
        let archive_sha256 = sha256_hex(release_url.as_bytes());
        let evidence_source_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO curation_evidence_sources (
              source_url, resolved_url, source_title, publisher, source_domain,
              source_tier, content_sha256, retrieved_at
            ) VALUES (
              ?, ?,
              'FAA registry fixture', 'Federal Aviation Administration',
              'faa.gov', 'regulator_primary', ?, CURRENT_TIMESTAMP
            )
            RETURNING id
            "#,
        )
        .bind(&release_url)
        .bind(&release_url)
        .bind(&archive_sha256)
        .fetch_one(pool)
        .await
        .unwrap();
        let snapshot_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO faa_registry_snapshots (
              evidence_source_id, snapshot_date, source_url, archive_sha256,
              source_manifest_sha256, target_set_sha256,
              master_member_name, master_member_sha256,
              aircraft_member_name, aircraft_member_sha256,
              engine_member_name, engine_member_sha256
            ) VALUES (
              ?, '2026-07-31',
              ?,
              ?, ?, ?, 'MASTER.txt', ?, 'ACFTREF.txt', ?, 'ENGINE.txt', ?
            )
            RETURNING id
            "#,
        )
        .bind(evidence_source_id)
        .bind(&release_url)
        .bind(&archive_sha256)
        .bind("b".repeat(64))
        .bind("c".repeat(64))
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .bind("f".repeat(64))
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO faa_registry_aircraft (
              snapshot_id, n_number, aircraft_code, year_manufactured,
              source_record_sha256
            ) VALUES (?, 'N123AB', 'TEST-1', 2020, ?)
            "#,
        )
        .bind(snapshot_id)
        .bind("0".repeat(64))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status) VALUES (?, 'N123AB', 'matched')",
        )
        .bind(snapshot_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_approved_suggestion_product(db: &AppDb, attest: bool) -> i64 {
        let pool = sqlite_pool(db);
        let manufacturer_key = normalize_avionics_manufacturer_name("Garmin");
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Garmin', ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(&manufacturer_key)
        .execute(pool)
        .await
        .unwrap();
        let manufacturer_id: i64 =
            sqlx::query_scalar("SELECT id FROM avionics_manufacturers WHERE normalized_name = ?")
                .bind(&manufacturer_key)
                .fetch_one(pool)
                .await
                .unwrap();
        ensure_manufacturer_identity(
            db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://www.garmin.com/en-US/aviation/".to_string(),
                source_title: "Garmin Aviation".to_string(),
                evidence_text: "Garmin identifies its aviation products.".to_string(),
            },
        )
        .await
        .unwrap();

        let sequence: i64 = sqlx::query_scalar("SELECT COUNT(*) + 1 FROM avionics_models")
            .fetch_one(pool)
            .await
            .unwrap();
        let model = format!("GI 275 TEST {sequence}");
        let identifier = format!("GI-275-TEST-{sequence}");
        let product_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_evidence_kind, identity_confidence, catalog_reviewed_at
            ) VALUES (?, ?, ?, 'manufacturer_model_number', ?, ?,
                      'https://www.garmin.com/aviation/product', 'Garmin product manual',
                      'Garmin identifies the exact test flight display.',
                      'authoritative_reference', 'very_high', CURRENT_TIMESTAMP)
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .bind(&model)
        .bind(normalize_avionics_model_name(&model))
        .bind(&identifier)
        .bind(normalize_avionics_identifier(&identifier))
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Flight Display', ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(normalize_name("Flight Display"))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
            SELECT ?, id FROM avionics_types WHERE normalized_name = ?
            "#,
        )
        .bind(product_id)
        .bind(normalize_name("Flight Display"))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
            .bind(product_id)
            .execute(pool)
            .await
            .unwrap();

        if attest {
            sqlx::query(
                r#"
                INSERT INTO avionics_authoritative_source_origins (
                  authority_kind, avionics_manufacturer_identity_id, https_origin,
                  evidence_source_url, evidence_source_title, evidence_text,
                  approval_basis, approval_reason
                )
                SELECT 'manufacturer_primary', avionics_manufacturer_identity_id,
                       'https://www.garmin.com',
                       'https://www.garmin.com/aviation/product',
                       'Garmin aviation product catalog',
                       'Garmin publishes the exact test product.',
                       'curated_bootstrap', 'verification test fixture'
                FROM avionics_approved_product_identities
                WHERE avionics_model_id = ?
                ON CONFLICT DO NOTHING
                "#,
            )
            .bind(product_id)
            .execute(pool)
            .await
            .unwrap();
            let mut transaction = pool.begin().await.unwrap();
            assert!(refresh_reuse_attestation_sqlite(
                db,
                &mut transaction,
                product_id,
                "https://www.garmin.com/aviation/product",
            )
            .await
            .unwrap());
            transaction.commit().await.unwrap();
        }
        product_id
    }

    async fn seed_suggestion_only_listing(
        db: &AppDb,
        suffix: &str,
        suggested_product_id: i64,
        evidence_product_id: i64,
        confidence: &str,
    ) -> i64 {
        let pool = sqlite_pool(db);
        let product: (String, String) = sqlx::query_as(
            r#"
            SELECT manufacturer.name, model.name
            FROM avionics_models model
            JOIN avionics_manufacturers manufacturer
              ON manufacturer.id = model.avionics_manufacturer_id
            WHERE model.id = ?
            "#,
        )
        .bind(evidence_product_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let source_url = format!("https://example.test/listing/{suffix}");
        let evidence = format!("{} {} standby instrument", product.0, product.1);
        let rendered_html = format!("<p>{evidence}</p>");
        let listing_id = seed_listing(db, &source_url).await;
        seed_faa_admission(db, listing_id).await;
        let submission_id = seed_submission_and_review(
            db,
            listing_id,
            &source_url,
            &rendered_html,
            None,
            Some(listing_id),
            Some("legacy extraction unavailable"),
        )
        .await;
        let aspect = PendingReviewAspect::avionics(
            "fixture:suggested:0",
            "avionics",
            product.1.clone(),
            evidence.clone(),
            "exact catalog suggestion requires replay",
            1,
            "installed",
            Some(evidence),
            Some(confidence.to_string()),
        )
        .with_suggested_product(ReviewProduct::verified(
            suggested_product_id,
            product.0,
            product.1,
            vec!["Flight Display".to_string()],
        ));
        crate::listing::review::stage_pending_review(
            db,
            listing_id,
            Some(submission_id),
            &[aspect],
        )
        .await
        .unwrap();
        listing_id
    }

    #[derive(Clone, Copy)]
    enum PreservedAssociationFixture {
        Exact,
        MissingEvidence,
        AmbiguousQualifier,
        Replacement,
    }

    async fn seed_preserved_association_listing(
        db: &AppDb,
        suffix: &str,
        product_id: i64,
        fixture: PreservedAssociationFixture,
    ) -> (i64, i64, ReviewAspectId) {
        let pool = sqlite_pool(db);
        let (manufacturer, model): (String, String) = sqlx::query_as(
            r#"
            SELECT manufacturer.name, model.name
            FROM avionics_models model
            JOIN avionics_manufacturers manufacturer
              ON manufacturer.id = model.avionics_manufacturer_id
            WHERE model.id = ?
            "#,
        )
        .bind(product_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let exact_evidence = format!("{manufacturer} {model} standby instrument");
        let evidence = match fixture {
            PreservedAssociationFixture::AmbiguousQualifier => {
                format!("{manufacturer} {model} WAAS upgraded")
            }
            _ => exact_evidence,
        };
        let source_url = format!("https://example.test/listing/{suffix}");
        let rendered_html = format!("<p>{evidence}</p>");
        let listing_id = seed_listing(db, &source_url).await;
        seed_faa_admission(db, listing_id).await;
        let submission_id = seed_submission_and_review(
            db,
            listing_id,
            &source_url,
            &rendered_html,
            None,
            Some(listing_id),
            Some("legacy extraction unavailable"),
        )
        .await;
        let replacement_id = if matches!(fixture, PreservedAssociationFixture::Replacement) {
            Some(seed_approved_suggestion_product(db, true).await)
        } else {
            None
        };
        let configuration_action = if replacement_id.is_some() {
            "replaces"
        } else {
            "installed"
        };
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action,
              replaces_avionics_model_id
            ) VALUES (?, ?, 1, 'listing', ?, 'high', ?, ?)
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .bind(&evidence)
        .bind(configuration_action)
        .bind(replacement_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let aspect_id = ReviewAspectId::from(format!("avionics:preserved:{link_id}:installed"));
        let mut aspect = PendingReviewAspect::avionics(
            aspect_id.clone(),
            "avionics_reuse_attestation",
            format!("{manufacturer} {model}"),
            format!("{manufacturer} {model}"),
            "catalog_product_or_listing_corroboration_missing",
            1,
            configuration_action,
            match fixture {
                PreservedAssociationFixture::MissingEvidence => None,
                _ => Some(evidence),
            },
            Some("high".to_string()),
        )
        .with_covered_association(
            link_id,
            crate::listing::review::ListingAssociationRole::Installed,
            product_id,
        )
        .with_reuse_attestation_target(product_id);
        if let Some(replacement_id) = replacement_id {
            aspect = aspect.with_replacement_product(replacement_id);
        }
        crate::listing::review::stage_pending_review(
            db,
            listing_id,
            Some(submission_id),
            &[aspect],
        )
        .await
        .unwrap();
        (listing_id, link_id, aspect_id)
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
