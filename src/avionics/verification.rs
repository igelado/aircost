use std::collections::{BTreeSet, HashMap};
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

use crate::aircraft::faa::require_listing_faa_admission;
use crate::avionics::catalog::{
    deterministic_generic_avionics_rejection_reason, plan_avionics_identity_verification_route,
    preview_avionics_identity, resolve_avionics_identity_for_automated_review,
    resolve_verified_local_avionics_identity, resolve_verified_local_avionics_model_observation,
    unique_exact_avionics_model_observation_review_candidate,
    unique_exact_avionics_review_candidate, ApprovedAvionicsIdentity, AvionicsExistingCatalogScope,
    AvionicsIdentityOutcome, AvionicsIdentityRequest, AvionicsIdentityVerificationPlan,
    AvionicsIdentityVerificationRoute, VerifiedLocalReuseProof,
};
use crate::avionics::consolidation::PendingReviewRevisionReceipt;
use crate::avionics::fingerprint::{
    active_collision_closure_revision_sha256, approved_catalog_revision_sha256,
    AvionicsFingerprintError,
};
use crate::avionics::reuse::{
    countable_unit_product_reuse_attestation_is_current, product_reuse_attestation_is_current,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::GeminiListingExtractor;
use crate::gemini::interactions::RetryPolicy;
use crate::gemini::usage::SourceCorrelation;
#[cfg(test)]
use crate::html::clean::clean_listing_html;
use crate::html::listing::source::listing_extraction_source;
use crate::listing::avionics::correction::validate_or_correct_listing_avionics;
use crate::listing::avionics::disposition::{AutomaticOccurrenceDisposition, OccurrenceRole};
#[cfg(test)]
use crate::listing::avionics::extraction::parse_current_avionics_extraction_json;
use crate::listing::avionics::extraction::{
    exact_controller_leading_dual_evidence_proof, parse_current_avionics_extraction_value,
    validate_current_avionics_observations, validate_unbound_current_avionics_extraction,
    ListingAvionicsEvidenceObservation,
};
use crate::listing::avionics::{
    approved_avionics_product_key, preview_avionics_product_key,
    validate_canonical_avionics_actions, CanonicalAvionicsAction,
};
use crate::listing::evidence::ListingEvidenceContext;
use crate::listing::review::automation::{
    apply_automated_avionics_review, unrelated_preserved_avionics_blocker,
    validate_automated_avionics_link, AutomatedAssociationAuthorization, AutomatedAvionicsLink,
    AutomatedLinkedOccurrenceGuard, AutomatedPreservedAssociationGuard,
    AutomatedReviewApplyRequest,
};
use crate::listing::review::{
    evaluate_existing_product_associations, parse_current_pending_review_aspects,
    ExistingProductAssociationCommit, ExistingProductAssociationEvaluation, ListingAssociationRole,
    PendingReviewAspect, ReviewAction, ReviewAspectId, ReviewProduct, StableIdentifier,
    POSTGRES_LISTING_CHILD_LOCK_SQL,
};
use crate::models::ParsedAvionics;
use crate::normalize::is_generic_avionics_model_name;
use crate::plugin::{
    canonical_current_checkpoint_payload, parse_current_checkpoint_payload, sha256_hex,
};

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

impl AvionicsProviderRequestPlan {
    /// Whether executing this exact preflight plan can require Gemini.
    ///
    /// The validation envelope includes every conditional identity route and
    /// legacy re-extraction. A zero envelope is therefore the only plan that
    /// may execute without a configured provider client.
    pub fn requires_provider(&self) -> bool {
        self.known_total_provider_requests_validation_envelope_maximum > 0
    }
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

#[derive(Debug)]
struct ValidatedListingReextraction {
    extracted_listing_json: String,
    avionics: Vec<ParsedAvionics>,
}

#[derive(Debug, FromRow)]
struct LockedReextractionStateRow {
    listing_id: i64,
    listing_owner_user_id: i64,
    listing_source_url: Option<String>,
    ingestion_state: String,
    is_verified: bool,
    pending_review_id: i64,
    pending_submission_id: i64,
    pending_aspect_count: i64,
    review_payload_json: String,
    review_payload_sha256: String,
    submission_id: i64,
    submission_owner_user_id: i64,
    submission_canonical_listing_id: Option<i64>,
    submission_source_url: String,
    rendered_html: String,
    rendered_html_sha256: String,
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

#[derive(Debug, FromRow)]
struct ApprovedGraphEnvelopeRow {
    avionics_model_id: i64,
    manufacturer_identity_id: i64,
    canonical_product_key: String,
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

fn literal_observation_manufacturer(manufacturer: &Option<String>) -> Option<&str> {
    manufacturer
        .as_deref()
        .map(str::trim)
        .filter(|manufacturer| !manufacturer.is_empty())
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
/// remains present in that submission. Otherwise, apply mode runs the current
/// Gemini listing extractor against the retained, hash-verified HTML and
/// durably replaces the obsolete derived extraction before identity work. The
/// guarded write is bound to the exact submission, owner, listing, source URL,
/// capture bytes/hash, pending-review revision, and prior extraction state.
/// Dry-run still makes paid preview calls without domain writes. Listing links
/// and residual review state retain their separate atomic apply boundary. An
/// absent extractor is valid only for a zero-request plan; any route that
/// requires a provider fails closed before the atomic apply boundary.
pub async fn verify_listing_avionics_page(
    db: &AppDb,
    extractor: Option<&GeminiListingExtractor>,
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
    let apply = mode.applies();
    let preflight = build_preflight_report(db, scope, &page, mode).await;
    let checkpoint = preflight.checkpoint.clone();
    let provider_request_plan = preflight.provider_request_plan;
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
            "Apply mode first reuses current hash-bound review observations whose exact excerpts remain in the retained submission. Otherwise it strictly validates the raw avionics occurrence array and durably replaces only the retained extraction's avionics member under exact source, owner, listing, capture-hash, review-revision, and prior-extraction guards before identity work; a later identity or listing block retains that extraction for provider-free replay. Listing links and residual review aspects keep their separate atomic apply boundary."
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
    mode: AvionicsVerificationExecutionMode,
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
            report: preflight_listing(db, row, mode.applies()).await,
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
/// and performs no catalog, link, review, or provider work. A zero-request
/// local plan may run without an extractor; provider-required work without one
/// remains blocked without mutating the pending review.
pub async fn verify_listing_avionics(
    db: &AppDb,
    extractor: Option<&GeminiListingExtractor>,
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
    Ok(build_preflight_report(db, scope, &page, AvionicsVerificationExecutionMode::Apply).await)
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
    mode: AvionicsVerificationExecutionMode,
) -> AvionicsVerificationPreflightReport {
    let mut listings = Vec::with_capacity(page.rows.len());
    for row in &page.rows {
        listings.push(preflight_listing(db, row, mode.applies()).await);
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
    require_commit_readiness: bool,
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
    let mut paid_candidate_scope = AvionicsExistingCatalogScope::default();
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
                "{reason}; one listing-extraction request is planned before identity work, but downstream identity calls are unknown until that validated extraction is durably available and preflighted again; the reported one-to-two extraction calls are not an end-to-end total"
            );
            return report;
        }
        Err(error) => {
            report.note = error;
            return report;
        }
    };
    for raw in &raw_avionics {
        if raw_candidate_structure_issue(raw).is_some() {
            report.invalid_retained_observations += 1;
            continue;
        }
        let primary_manufacturer = literal_observation_manufacturer(&raw.manufacturer);
        let primary_is_generic = is_generic_avionics_model_name(&raw.model);
        let replacement_is_generic = raw
            .replaces
            .as_ref()
            .is_some_and(|replacement| is_generic_avionics_model_name(&replacement.model));
        if primary_is_generic || replacement_is_generic {
            report.invalid_retained_observations += 1;
            report.generic_invalid_identity_components +=
                usize::from(primary_is_generic) + usize::from(replacement_is_generic);
        }
        let primary_plan = if primary_is_generic {
            None
        } else if let Some(primary_manufacturer) = primary_manufacturer {
            let primary_identity = IdentityInput {
                manufacturer: primary_manufacturer,
                model: &raw.model,
                avionics_types: &raw.avionics_types,
                quantity: raw.quantity,
            };
            match preflight_identity_component(
                db,
                row,
                row.submission_source_url.as_deref(),
                &listing_context,
                primary_identity,
                raw.source_evidence_text.as_deref(),
            )
            .await
            {
                Ok(plan) => {
                    report.retained_identity_components += 1;
                    add_preflight_identity_route(&mut report, plan.route, false);
                    if plan.route != AvionicsIdentityVerificationRoute::VerifiedLocal {
                        extend_existing_catalog_scope(
                            &mut paid_candidate_scope,
                            &plan.existing_catalog_scope,
                        );
                    }
                    Some(plan)
                }
                Err(error) => {
                    report.note = format!("verified-local identity preflight failed: {error}");
                    return report;
                }
            }
        } else {
            report.retained_identity_components += 1;
            match preflight_model_only_identity_component(
                db,
                &raw.model,
                &raw.avionics_types,
                raw.source_evidence_text.as_deref(),
            )
            .await
            {
                Ok(verified_local) => {
                    if verified_local {
                        add_preflight_identity_route(
                            &mut report,
                            AvionicsIdentityVerificationRoute::VerifiedLocal,
                            false,
                        );
                    }
                }
                Err(error) => {
                    report.note = format!("model-only identity preflight failed: {error}");
                    return report;
                }
            }
            None
        };

        let Some(replacement) = raw.replaces.as_ref() else {
            continue;
        };
        if replacement_is_generic {
            continue;
        }
        report.retained_identity_components += 1;
        let replacement_is_conditional = primary_plan
            .as_ref()
            .is_some_and(|plan| plan.route != AvionicsIdentityVerificationRoute::VerifiedLocal);
        if let Some(replacement_manufacturer) =
            literal_observation_manufacturer(&replacement.manufacturer)
        {
            let replacement_plan = match preflight_identity_component(
                db,
                row,
                row.submission_source_url.as_deref(),
                &listing_context,
                IdentityInput {
                    manufacturer: replacement_manufacturer,
                    model: &replacement.model,
                    avionics_types: &replacement.avionics_types,
                    quantity: 1,
                },
                raw.source_evidence_text.as_deref(),
            )
            .await
            {
                Ok(plan) => plan,
                Err(error) => {
                    report.note = format!("verified-local replacement preflight failed: {error}");
                    return report;
                }
            };
            if replacement_plan.route != AvionicsIdentityVerificationRoute::VerifiedLocal {
                extend_existing_catalog_scope(
                    &mut paid_candidate_scope,
                    &replacement_plan.existing_catalog_scope,
                );
            }
            add_preflight_identity_route(
                &mut report,
                replacement_plan.route,
                replacement_is_conditional,
            );
        } else {
            match preflight_model_only_identity_component(
                db,
                &replacement.model,
                &replacement.avionics_types,
                raw.source_evidence_text.as_deref(),
            )
            .await
            {
                Ok(verified_local) => {
                    if verified_local {
                        add_preflight_identity_route(
                            &mut report,
                            AvionicsIdentityVerificationRoute::VerifiedLocal,
                            replacement_is_conditional,
                        );
                    }
                }
                Err(error) => {
                    report.note = format!("model-only replacement preflight failed: {error}");
                    return report;
                }
            }
        }
    }
    if require_commit_readiness
        && (report.candidate_adjudication_identity_components > 0
            || report.candidate_adjudication_conditional_relationship_components > 0
            || report.candidate_triage_identity_components > 0
            || report.candidate_triage_conditional_relationship_components > 0
            || report.grounded_initial_identity_components > 0
            || report.grounded_conditional_relationship_components > 0)
    {
        let candidate_graph_keys = match candidate_graph_key_envelope(db, &paid_candidate_scope)
            .await
        {
            Ok(keys) => keys,
            Err(error) => {
                clear_paid_preflight_counts(&mut report);
                report.note = format!(
                    "automatic verification readiness could not be checked before paid avionics work: {error}"
                );
                return report;
            }
        };
        if let Some(candidate_graph_keys) = candidate_graph_keys {
            match unrelated_preserved_avionics_blocker(db, row.listing_id, &candidate_graph_keys)
                .await
            {
                Ok(Some(reason)) => {
                    clear_paid_preflight_counts(&mut report);
                    report.note = format!(
                        "automatic verification is unavailable before paid avionics work: {reason}"
                    );
                    return report;
                }
                Ok(None) => {}
                Err(error) => {
                    clear_paid_preflight_counts(&mut report);
                    report.note = format!(
                        "automatic verification readiness could not be checked before paid avionics work: {error}"
                    );
                    return report;
                }
            }
        }
    }
    report.status = "ready_retained_observations".to_string();
    report.note = if report.invalid_retained_observations == 0 {
        "retained current-schema observations are ready".to_string()
    } else if report.generic_invalid_identity_components > 0 {
        format!(
            "{} retained observation(s) are invalid; {} structurally valid exact generic-label observation(s) will be discarded deterministically without a provider request, while every otherwise invalid observation remains for review",
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

fn add_preflight_identity_route(
    report: &mut AvionicsVerificationPreflightListingReport,
    route: AvionicsIdentityVerificationRoute,
    conditional_relationship: bool,
) {
    match (route, conditional_relationship) {
        (AvionicsIdentityVerificationRoute::VerifiedLocal, _) => {
            report.verified_local_identity_components += 1;
        }
        (AvionicsIdentityVerificationRoute::CandidateAdjudication, false) => {
            report.candidate_adjudication_identity_components += 1;
        }
        (AvionicsIdentityVerificationRoute::CandidateAdjudication, true) => {
            report.candidate_adjudication_conditional_relationship_components += 1;
        }
        (AvionicsIdentityVerificationRoute::CandidateTriage, false) => {
            report.candidate_triage_identity_components += 1;
        }
        (AvionicsIdentityVerificationRoute::CandidateTriage, true) => {
            report.candidate_triage_conditional_relationship_components += 1;
        }
        (AvionicsIdentityVerificationRoute::GroundedCuration, false) => {
            report.grounded_initial_identity_components += 1;
        }
        (AvionicsIdentityVerificationRoute::GroundedCuration, true) => {
            report.grounded_conditional_relationship_components += 1;
        }
    }
}

async fn preflight_identity_component(
    db: &AppDb,
    row: &ListingSourceRow,
    source_url: Option<&str>,
    listing_context: &ListingEvidenceContext,
    identity: IdentityInput<'_>,
    source_evidence_text: Option<&str>,
) -> Result<AvionicsIdentityVerificationPlan, String> {
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

async fn preflight_model_only_identity_component(
    db: &AppDb,
    model: &str,
    avionics_types: &[String],
    source_evidence_text: Option<&str>,
) -> Result<bool, String> {
    resolve_verified_local_avionics_model_observation(
        db,
        model,
        avionics_types,
        source_evidence_text.unwrap_or_default(),
    )
    .await
    .map(|identity| identity.is_some())
    .map_err(|error| error.to_string())
}

fn extend_existing_catalog_scope(
    target: &mut AvionicsExistingCatalogScope,
    source: &AvionicsExistingCatalogScope,
) {
    target
        .catalog_ids
        .extend(source.catalog_ids.iter().copied());
    target
        .manufacturer_identity_ids
        .extend(source.manufacturer_identity_ids.iter().copied());
    target.unbounded |= source.unbounded;
}

async fn candidate_graph_key_envelope(
    db: &AppDb,
    scope: &AvionicsExistingCatalogScope,
) -> VerificationResult<Option<BTreeSet<String>>> {
    if scope.unbounded {
        return Ok(None);
    }
    if scope.catalog_ids.is_empty() && scope.manufacturer_identity_ids.is_empty() {
        return Ok(Some(BTreeSet::new()));
    }
    let sql = db.sql(
        r#"
        SELECT
          graph.avionics_model_id,
          graph.avionics_manufacturer_identity_id AS manufacturer_identity_id,
          graph.canonical_product_key
        FROM avionics_approved_product_graph_identities graph
        JOIN avionics_models model
          ON model.id = graph.avionics_model_id
        WHERE model.catalog_status = 'approved'
        ORDER BY model.id
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ApprovedGraphEnvelopeRow>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ApprovedGraphEnvelopeRow>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    rows.into_iter()
        .filter(|row| {
            scope.catalog_ids.contains(&row.avionics_model_id)
                || scope
                    .manufacturer_identity_ids
                    .contains(&row.manufacturer_identity_id)
        })
        .map(|row| {
            approved_avionics_product_key(row.manufacturer_identity_id, &row.canonical_product_key)
                .map_err(AvionicsVerificationError::Validation)
        })
        .collect::<VerificationResult<BTreeSet<_>>>()
        .map(Some)
}

fn clear_paid_preflight_counts(report: &mut AvionicsVerificationPreflightListingReport) {
    report.candidate_adjudication_identity_components = 0;
    report.candidate_adjudication_conditional_relationship_components = 0;
    report.candidate_triage_identity_components = 0;
    report.candidate_triage_conditional_relationship_components = 0;
    report.grounded_initial_identity_components = 0;
    report.grounded_conditional_relationship_components = 0;
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
        initial_grounded_provider_requests_baseline: grounded.saturating_mul(4),
        initial_grounded_provider_requests_nonpositive_validation_envelope: grounded
            .saturating_mul(9),
        positive_identity_provider_requests_baseline: all_grounded_components.saturating_mul(7),
        positive_identity_provider_requests_validation_envelope: all_grounded_components
            .saturating_mul(15),
        known_total_provider_requests_minimum_baseline: reextractions
            .saturating_add(candidate)
            .saturating_add(triage)
            .saturating_add(grounded.saturating_mul(4))
            .saturating_add(triage.saturating_mul(4)),
        known_total_provider_requests_all_positive_baseline: reextractions
            .saturating_add(all_candidate_components)
            .saturating_add(all_triage_components)
            .saturating_add(all_grounded_components.saturating_mul(7))
            .saturating_add(all_triage_components.saturating_mul(7)),
        known_total_provider_requests_validation_envelope_maximum: reextractions
            .saturating_mul(2)
            .saturating_add(all_candidate_components)
            .saturating_add(all_triage_components)
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
        grounded_pass_note: "Every identity that reaches the grounded route first uses exactly one tools-disabled concreteness-classifier request. A structurally valid observation whose complete normalized model label is in the closed generic-category vocabulary is discarded deterministically without entering the provider phase. Malformed observations remain for review. The fresh grounded identity pass has three logical provider requests at baseline (Search, URL Context, structure) and at most six after per-stage validation fallbacks. One reused-evidence identity correction can raise the grounded portion to eight. Including the classifier, a positive identity and its independent collision pass use seven requests at baseline and up to fifteen in the complete validation envelope. A nonpositive grounded identity does not run collision review."
            .to_string(),
        transport_retry_note: "Logical provider-request counts do not multiply transport retries. The default interactions retry policy may make up to four transport attempts for one logical request."
            .to_string(),
        uncertainty_note: "The minimum baseline assumes every listing extraction passes its current-schema validation on the primary request, every bounded approved-candidate adjudication succeeds without Search, every global candidate-triage call produces a usable hint, and every conditional relationship target is skipped. The listing-extraction validation envelope is exactly two logical requests per re-extraction: a malformed primary may use the existing JSON repair, while a parseable primary that fails deterministic avionics validation may instead use one fallback-model avionics-only correction. Those paths are mutually exclusive, and the correction is parsed once without JSON repair. Each candidate comparison is exactly one tools-disabled request. A successful approved-candidate decision does not run the concreteness classifier and still passes the unchanged local reuse gates. Because preflight cannot know the triage decision, it conservatively includes the ordinary classifier and grounded route for every triage component; a current approved singleton that passes reuse can be cheaper, while every unreviewed result must take that grounded route. An uncertain, negative, invalid, or stale answer falls through normally and then incurs exactly one classifier request before grounded research. Structurally valid exact generic-label observations use no provider request and never continue to grounding. The all-positive baseline includes every conditional target but assumes candidate comparison succeeds. The maximum validation envelope includes classifier plus grounded fallback for every candidate. Verified-local identities use neither request. All counts use the catalog as it exists at preflight time; earlier apply pages can approve identities that later pages resolve locally with zero Gemini requests. When legacy_reextraction_identity_outputs_unknown is true, every known-total field is only the calls-before-extraction floor: downstream identity calls are unknown until the validated extraction is durably persisted and preflighted again, so those fields must not be presented as an end-to-end total. Correction and fallback outcomes remain unknowable before execution, so no dollar estimate is inferred."
            .to_string(),
    }
}

async fn process_listing(
    db: &AppDb,
    extractor: Option<&GeminiListingExtractor>,
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
    let scoped_extractor = extractor.map(|extractor| {
        extractor.clone().with_usage_scope(
            format!("auto-review-listing-{}", row.listing_id),
            Some(row.listing_id),
            Some(SourceCorrelation {
                kind: "plugin_submission".to_string(),
                id: submission_id.to_string(),
            }),
        )
    });

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
    let retained_extraction_source = source_url
        .as_deref()
        .zip(row.rendered_html.as_deref())
        .and_then(|(source_url, rendered_html)| {
            listing_extraction_source(source_url, rendered_html).ok()
        });
    let mut ordinary_review_forced_fallback = false;
    let (raw_avionics, mut residual_aspects, current_checkpoint_sha256) =
        match retained_observation_source(db, row, &listing_context).await {
            Ok(RetainedObservationSource::Review {
                avionics,
                preserved_aspects,
            }) => {
                listing_report.raw_avionics_source = "pending_review".to_string();
                (
                    avionics,
                    preserved_aspects,
                    row.extracted_listing_json
                        .as_deref()
                        .map(|checkpoint| sha256_hex(checkpoint.as_bytes()))
                        .unwrap_or_default(),
                )
            }
            Ok(RetainedObservationSource::Extraction {
                avionics,
                preserved_aspects,
            }) => {
                ordinary_review_forced_fallback = true;
                listing_report.raw_avionics_source = "retained_extraction".to_string();
                (
                    avionics,
                    preserved_aspects,
                    row.extracted_listing_json
                        .as_deref()
                        .map(|checkpoint| sha256_hex(checkpoint.as_bytes()))
                        .unwrap_or_default(),
                )
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
                let Some(scoped_extractor) = scoped_extractor.as_ref() else {
                    let error = "current-schema re-extraction requires configured Gemini services"
                        .to_string();
                    listing_report.status = "blocked".to_string();
                    listing_report.reextraction_error = Some(error.clone());
                    listing_report.error = Some(error);
                    return listing_report;
                };
                listing_report.reextraction_attempted = true;
                match reextract_avionics(
                    scoped_extractor,
                    &listing_text,
                    &listing_context,
                    source_url,
                    rendered_html,
                    row.extracted_listing_json.as_deref(),
                )
                .await
                {
                    Ok(reextraction) => {
                        listing_report.raw_avionics_source = "gemini_reextraction".to_string();
                        if apply && !reextraction.avionics.is_empty() {
                            if let Err(error) = persist_validated_listing_reextraction(
                                db,
                                row,
                                submission_id,
                                &reextraction.extracted_listing_json,
                            )
                            .await
                            {
                                let error = error.to_string();
                                listing_report.status = "blocked".to_string();
                                listing_report.reextraction_error = Some(error.clone());
                                listing_report.error = Some(format!(
                                "validated current-schema re-extraction was not persisted because its retained source binding changed: {error}"
                            ));
                                return listing_report;
                            }
                            listing_report.source_extraction_error = None;
                        }
                        listing_report.reextraction_succeeded = true;
                        (
                            reextraction.avionics,
                            preserved_aspects,
                            sha256_hex(reextraction.extracted_listing_json.as_bytes()),
                        )
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
                if let Err(error) = reconcile_or_push_prepared_link(&mut prepared, link) {
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
    // Durable disposition and association receipts remain bound to the exact
    // retained source-array coordinates. Post-validation processing must not
    // change occurrence cardinality or infer physical-unit quantity.
    let mut occurrence_dispositions = Vec::new();
    let mut linked_occurrence_guards = Vec::new();
    for (occurrence_index, raw) in raw_avionics.iter().enumerate() {
        if raw_candidate_structure_issue(raw).is_some() {
            continue;
        }
        if let Some(primary_manufacturer) = literal_observation_manufacturer(&raw.manufacturer) {
            let identity = IdentityInput {
                manufacturer: primary_manufacturer,
                model: &raw.model,
                avionics_types: &raw.avionics_types,
                quantity: raw.quantity,
            };
            let primary_request = identity_request(
                row,
                source_url.as_deref(),
                &listing_context,
                &identity,
                raw.source_evidence_text.as_deref(),
            );
            if deterministic_generic_avionics_rejection_reason(&primary_request).is_some() {
                occurrence_dispositions.push(AutomaticOccurrenceDisposition::discarded(
                    occurrence_index,
                    OccurrenceRole::Primary,
                ));
            }
        }
        if let Some(replacement) = raw.replaces.as_ref() {
            let Some(replacement_manufacturer) =
                literal_observation_manufacturer(&replacement.manufacturer)
            else {
                continue;
            };
            let replacement_identity = IdentityInput {
                manufacturer: replacement_manufacturer,
                model: &replacement.model,
                avionics_types: &replacement.avionics_types,
                quantity: 1,
            };
            let replacement_request = identity_request(
                row,
                source_url.as_deref(),
                &listing_context,
                &replacement_identity,
                raw.source_evidence_text.as_deref(),
            );
            if deterministic_generic_avionics_rejection_reason(&replacement_request).is_some() {
                occurrence_dispositions.push(AutomaticOccurrenceDisposition::discarded(
                    occurrence_index,
                    OccurrenceRole::Replacement,
                ));
            }
        }
    }
    // Route every retained observation before executing identity resolution.
    // Structurally invalid and exact generic-category observations remain
    // provider-free; concrete observations still let exact local identities
    // and their prepared-link semantics fail closed before a later candidate
    // can spend provider requests or curate catalog state for a listing whose
    // transaction cannot commit.
    let mut provider_free_candidate_indices = Vec::new();
    let mut provider_candidate_indices = Vec::new();
    let mut candidate_scope = AvionicsExistingCatalogScope::default();
    for (candidate_index, raw) in raw_avionics.iter().enumerate() {
        if raw_candidate_structure_issue(raw).is_some() {
            provider_free_candidate_indices.push(candidate_index);
            continue;
        }
        if generic_model_issue(raw).is_some() {
            let Some(replacement) = raw.replaces.as_ref() else {
                provider_free_candidate_indices.push(candidate_index);
                continue;
            };
            let Some(replacement_manufacturer) =
                literal_observation_manufacturer(&replacement.manufacturer)
            else {
                provider_free_candidate_indices.push(candidate_index);
                continue;
            };
            let replacement_identity = IdentityInput {
                manufacturer: replacement_manufacturer,
                model: &replacement.model,
                avionics_types: &replacement.avionics_types,
                quantity: 1,
            };
            let replacement_request = identity_request(
                row,
                source_url.as_deref(),
                &listing_context,
                &replacement_identity,
                raw.source_evidence_text.as_deref(),
            );
            if deterministic_generic_avionics_rejection_reason(&replacement_request).is_some() {
                provider_free_candidate_indices.push(candidate_index);
                continue;
            }
            let replacement_plan = match preflight_identity_component(
                db,
                row,
                source_url.as_deref(),
                &listing_context,
                replacement_identity,
                raw.source_evidence_text.as_deref(),
            )
            .await
            {
                Ok(plan) => plan,
                Err(error) => {
                    blocking_reasons.push(format!(
                        "candidate {candidate_index}: provider-free replacement identity preflight failed: {error}"
                    ));
                    continue;
                }
            };
            extend_existing_catalog_scope(
                &mut candidate_scope,
                &replacement_plan.existing_catalog_scope,
            );
            if replacement_plan.route == AvionicsIdentityVerificationRoute::VerifiedLocal {
                provider_free_candidate_indices.push(candidate_index);
            } else {
                provider_candidate_indices.push(candidate_index);
            }
            continue;
        }
        let primary_plan = if let Some(primary_manufacturer) =
            literal_observation_manufacturer(&raw.manufacturer)
        {
            match preflight_identity_component(
                db,
                row,
                source_url.as_deref(),
                &listing_context,
                IdentityInput {
                    manufacturer: primary_manufacturer,
                    model: &raw.model,
                    avionics_types: &raw.avionics_types,
                    quantity: raw.quantity,
                },
                raw.source_evidence_text.as_deref(),
            )
            .await
            {
                Ok(plan) => {
                    extend_existing_catalog_scope(
                        &mut candidate_scope,
                        &plan.existing_catalog_scope,
                    );
                    Some(plan)
                }
                Err(error) => {
                    blocking_reasons.push(format!(
                        "candidate {candidate_index}: provider-free primary identity preflight failed: {error}"
                    ));
                    continue;
                }
            }
        } else {
            None
        };
        let replacement_plan = if let Some(replacement) = raw.replaces.as_ref() {
            let Some(replacement_manufacturer) =
                literal_observation_manufacturer(&replacement.manufacturer)
            else {
                let fully_local = primary_plan.as_ref().is_none_or(|plan| {
                    plan.route == AvionicsIdentityVerificationRoute::VerifiedLocal
                });
                if fully_local {
                    provider_free_candidate_indices.push(candidate_index);
                } else {
                    provider_candidate_indices.push(candidate_index);
                }
                continue;
            };
            let replacement_identity = IdentityInput {
                manufacturer: replacement_manufacturer,
                model: &replacement.model,
                avionics_types: &replacement.avionics_types,
                quantity: 1,
            };
            let replacement_request = identity_request(
                row,
                source_url.as_deref(),
                &listing_context,
                &replacement_identity,
                raw.source_evidence_text.as_deref(),
            );
            if deterministic_generic_avionics_rejection_reason(&replacement_request).is_some() {
                None
            } else {
                match preflight_identity_component(
                    db,
                    row,
                    source_url.as_deref(),
                    &listing_context,
                    replacement_identity,
                    raw.source_evidence_text.as_deref(),
                )
                .await
                {
                    Ok(plan) => Some(plan),
                    Err(error) => {
                        blocking_reasons.push(format!(
                        "candidate {candidate_index}: provider-free replacement identity preflight failed: {error}"
                    ));
                        continue;
                    }
                }
            }
        } else {
            None
        };
        if let Some(replacement_plan) = replacement_plan.as_ref() {
            extend_existing_catalog_scope(
                &mut candidate_scope,
                &replacement_plan.existing_catalog_scope,
            );
        }
        let fully_local = primary_plan
            .as_ref()
            .is_none_or(|plan| plan.route == AvionicsIdentityVerificationRoute::VerifiedLocal)
            && replacement_plan
                .as_ref()
                .is_none_or(|plan| plan.route == AvionicsIdentityVerificationRoute::VerifiedLocal);
        if fully_local {
            provider_free_candidate_indices.push(candidate_index);
        } else {
            provider_candidate_indices.push(candidate_index);
        }
    }

    let (mut paid_candidate_graph_keys, paid_candidate_graph_key_error) = if apply {
        match candidate_graph_key_envelope(db, &candidate_scope).await {
            Ok(keys) => (keys, None),
            Err(error) => (Some(BTreeSet::new()), Some(error.to_string())),
        }
    } else {
        (None, None)
    };

    let mut execution_order = provider_free_candidate_indices
        .into_iter()
        .map(|candidate_index| (candidate_index, false))
        .chain(
            provider_candidate_indices
                .into_iter()
                .map(|candidate_index| (candidate_index, true)),
        )
        .collect::<Vec<_>>();
    let mut execution_cursor = 0;
    let mut provider_phase_started = false;
    let mut provider_phase_blocked_at_barrier = false;

    while execution_cursor < execution_order.len() {
        let (candidate_index, requires_provider) = execution_order[execution_cursor];
        execution_cursor += 1;
        if requires_provider && !provider_phase_started {
            provider_phase_started = true;
            if scoped_extractor.is_none() {
                blocking_reasons
                    .push("identity resolution requires configured Gemini services".to_string());
            }
            if let Some(reason) = review_revision.stale_reason.as_ref() {
                blocking_reasons.push(format!(
                    "pending review revision could not be advanced safely: {reason}"
                ));
            }
            if let Err(error) = validate_prepared_links(&prepared) {
                blocking_reasons.push(format!("listing avionics action graph is invalid: {error}"));
            }
            if apply {
                listing_report.prepared_link_count = prepared.len();
                if let Some(candidate_graph_keys) = paid_candidate_graph_keys.as_mut() {
                    candidate_graph_keys.extend(prepared.iter().flat_map(|link| {
                        std::iter::once(link.identity_key.clone())
                            .chain(link.replacement_identity_key.iter().cloned())
                    }));
                }
                if let Some(error) = paid_candidate_graph_key_error.as_ref() {
                    blocking_reasons.push(format!(
                        "paid candidate graph-key envelope could not be checked before identity work: {error}"
                    ));
                } else if let Some(candidate_graph_keys) = paid_candidate_graph_keys.as_ref() {
                    match unrelated_preserved_avionics_blocker(
                        db,
                        row.listing_id,
                        candidate_graph_keys,
                    )
                    .await
                    {
                        Ok(Some(reason)) => blocking_reasons.push(format!(
                            "unrelated existing listing avionics cannot commit before paid identity work: {reason}"
                        )),
                        Ok(None) => {}
                        Err(error) => blocking_reasons.push(format!(
                            "unrelated existing listing avionics readiness could not be checked before paid identity work: {error}"
                        )),
                    }
                }
                if let Err(error) = validate_prepared_link_commit_readiness(&prepared) {
                    blocking_reasons.push(format!(
                        "prepared listing avionics cannot commit before paid identity work: {error}"
                    ));
                }
            }
            if !blocking_reasons.is_empty() {
                provider_phase_blocked_at_barrier = true;
                break;
            }
        }
        if requires_provider && !blocking_reasons.is_empty() {
            break;
        }
        let raw = &raw_avionics[candidate_index];
        if let Some(issue) = raw_candidate_structure_issue(raw) {
            listing_report.candidates.push(input_error_report(
                candidate_index,
                "primary",
                literal_observation_manufacturer(&raw.manufacturer).unwrap_or_default(),
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
                manufacturer: literal_observation_manufacturer(&raw.manufacturer)
                    .unwrap_or_default(),
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
            if let Some(reason) = deterministic_generic_avionics_rejection_reason(&request) {
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
                let Some(replacement) = raw.replaces.as_ref() else {
                    continue;
                };
                let replacement_identity = IdentityInput {
                    manufacturer: literal_observation_manufacturer(&replacement.manufacturer)
                        .unwrap_or_default(),
                    model: &replacement.model,
                    avionics_types: &replacement.avionics_types,
                    quantity: 1,
                };
                let replacement_request = identity_request(
                    row,
                    source_url.as_deref(),
                    &listing_context,
                    &replacement_identity,
                    raw.source_evidence_text.as_deref(),
                );
                if let Some(replacement_reason) =
                    deterministic_generic_avionics_rejection_reason(&replacement_request)
                {
                    listing_report.candidates.push(outcome_report(
                        candidate_index,
                        "replacement",
                        &replacement_identity,
                        &raw.configuration_action,
                        raw.source_evidence_text.as_deref(),
                        raw.source_confidence.as_deref(),
                        "rejected",
                        None,
                        None,
                        None,
                        Vec::new(),
                        replacement_reason,
                    ));
                    listing_report.safely_discarded += 1;
                    continue;
                }
                let mut replacement_attempt = if replacement_identity.manufacturer.is_empty() {
                    match resolve_model_only_identity_attempt(
                        db,
                        apply,
                        candidate_index,
                        "replacement",
                        replacement_identity,
                        &raw.configuration_action,
                        raw.source_evidence_text.as_deref(),
                        raw.source_confidence.as_deref(),
                        catalog_statuses,
                    )
                    .await
                    {
                        Ok(attempt) => attempt,
                        Err(error) => {
                            blocking_reasons.push(format!(
                                "candidate {candidate_index}: model-only replacement identity resolution failed: {error}"
                            ));
                            continue;
                        }
                    }
                } else if requires_provider {
                    resolve_identity_attempt(
                        db,
                        scoped_extractor
                            .as_ref()
                            .expect("the provider barrier requires a configured extractor"),
                        apply,
                        row,
                        source_url.as_deref(),
                        &listing_context,
                        candidate_index,
                        "replacement",
                        replacement_identity,
                        &raw.configuration_action,
                        raw.source_evidence_text.as_deref(),
                        raw.source_confidence.as_deref(),
                        catalog_statuses,
                        &mut review_revision,
                    )
                    .await
                } else {
                    match resolve_local_only_identity_attempt(
                        db,
                        apply,
                        row,
                        source_url.as_deref(),
                        &listing_context,
                        candidate_index,
                        "replacement",
                        replacement_identity,
                        &raw.configuration_action,
                        raw.source_evidence_text.as_deref(),
                        raw.source_confidence.as_deref(),
                        catalog_statuses,
                    )
                    .await
                    {
                        Ok(Some(attempt)) => attempt,
                        Ok(None) => {
                            execution_order.push((candidate_index, true));
                            continue;
                        }
                        Err(error) => {
                            blocking_reasons.push(format!(
                                "candidate {candidate_index}: provider-free replacement identity resolution failed: {error}"
                            ));
                            continue;
                        }
                    }
                };
                if can_automatically_accept(&replacement_attempt, raw.source_confidence.as_deref())
                {
                    let avionics_model_id = replacement_attempt
                        .approved_id
                        .expect("approved replacement has a catalog id");
                    occurrence_dispositions.push(AutomaticOccurrenceDisposition::linked(
                        candidate_index,
                        OccurrenceRole::Replacement,
                        avionics_model_id,
                    ));
                    linked_occurrence_guards.push(AutomatedLinkedOccurrenceGuard {
                        occurrence_index: candidate_index,
                        occurrence_role: OccurrenceRole::Replacement,
                        avionics_model_id,
                        authorization: replacement_attempt
                            .authorization
                            .clone()
                            .expect("approved replacement has an authorization"),
                        expected_collision_closure_sha256: replacement_attempt
                            .collision_closure_sha256
                            .clone()
                            .expect("approved replacement has a collision revision"),
                    });
                } else {
                    if identity_is_approved(&replacement_attempt)
                        && raw.source_confidence.as_deref() != Some("high")
                    {
                        mark_weak_listing_evidence(&mut replacement_attempt.report);
                    }
                    let aspect = replacement_residual_aspect(
                        candidate_index,
                        raw,
                        replacement_attempt.report.reason.clone(),
                        replacement_attempt.suggested_product.clone(),
                    );
                    match attach_unique_exact_review_candidate(
                        db,
                        aspect,
                        literal_observation_manufacturer(&replacement.manufacturer),
                        &replacement.model,
                        &replacement.avionics_types,
                        raw.source_evidence_text.as_deref(),
                    )
                    .await
                    {
                        Ok(aspect) => residual_aspects.push(aspect),
                        Err(error) => blocking_reasons.push(format!(
                            "candidate {candidate_index}: exact replacement catalog review retrieval failed: {error}"
                        )),
                    }
                }
                listing_report.candidates.push(replacement_attempt.report);
                continue;
            }
            listing_report.candidates.push(input_error_report(
                candidate_index,
                "primary",
                literal_observation_manufacturer(&raw.manufacturer).unwrap_or_default(),
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

        let exact_leading_dual_proof = source_url
            .as_deref()
            .zip(retained_extraction_source.as_deref())
            .and_then(|(source_url, extraction_source)| {
                exact_controller_leading_dual_evidence_proof(
                    source_url,
                    extraction_source,
                    submission_id,
                    row.rendered_html_sha256
                        .as_deref()
                        .expect("pending source binding validated the retained capture hash"),
                    &current_checkpoint_sha256,
                    &ListingAvionicsEvidenceObservation {
                        manufacturer: raw.manufacturer.as_deref(),
                        model: &raw.model,
                        avionics_types: &raw.avionics_types,
                        quantity: raw.quantity,
                        configuration_action: &raw.configuration_action,
                        source_confidence: raw.source_confidence.as_deref(),
                        source_evidence_text: raw.source_evidence_text.as_deref(),
                    },
                )
            });
        let primary_identity = IdentityInput {
            manufacturer: literal_observation_manufacturer(&raw.manufacturer).unwrap_or_default(),
            model: &raw.model,
            avionics_types: &raw.avionics_types,
            quantity: raw.quantity,
        };
        let primary_resolved_local_only = !requires_provider;
        let mut primary = if primary_identity.manufacturer.is_empty() {
            match resolve_model_only_identity_attempt(
                db,
                apply,
                candidate_index,
                "primary",
                primary_identity,
                &raw.configuration_action,
                raw.source_evidence_text.as_deref(),
                raw.source_confidence.as_deref(),
                catalog_statuses,
            )
            .await
            {
                Ok(attempt) => attempt,
                Err(error) => {
                    blocking_reasons.push(format!(
                        "candidate {candidate_index}: model-only primary identity resolution failed: {error}"
                    ));
                    continue;
                }
            }
        } else if requires_provider {
            resolve_identity_attempt(
                db,
                scoped_extractor
                    .as_ref()
                    .expect("the provider barrier requires a configured extractor"),
                apply,
                row,
                source_url.as_deref(),
                &listing_context,
                candidate_index,
                "primary",
                primary_identity,
                &raw.configuration_action,
                raw.source_evidence_text.as_deref(),
                raw.source_confidence.as_deref(),
                catalog_statuses,
                &mut review_revision,
            )
            .await
        } else {
            match resolve_local_only_identity_attempt(
                db,
                apply,
                row,
                source_url.as_deref(),
                &listing_context,
                candidate_index,
                "primary",
                primary_identity,
                &raw.configuration_action,
                raw.source_evidence_text.as_deref(),
                raw.source_confidence.as_deref(),
                catalog_statuses,
            )
            .await
            {
                Ok(Some(attempt)) => attempt,
                Ok(None) => {
                    execution_order.push((candidate_index, true));
                    continue;
                }
                Err(error) => {
                    blocking_reasons.push(format!(
                        "candidate {candidate_index}: provider-free primary identity resolution failed: {error}"
                    ));
                    continue;
                }
            }
        };
        if primary.report.status == "rejected" {
            listing_report.candidates.push(primary.report);
            listing_report.safely_discarded += 1;
            continue;
        }

        let qualified_explicit_count = primary_resolved_local_only
            && exact_leading_dual_proof.is_some()
            && primary.approved_id.is_some()
            && countable_unit_product_reuse_attestation_is_current(
                db,
                primary.approved_id.unwrap_or_default(),
            )
            .await
            .unwrap_or(false);
        if qualified_explicit_count {
            primary.authorization =
                Some(AutomatedAssociationAuthorization::CountableUnitManufacturerReuse);
        }
        let effective_source_confidence = if qualified_explicit_count {
            Some("high")
        } else {
            raw.source_confidence.as_deref()
        };
        let primary_is_approved = identity_is_approved(&primary);
        let primary_has_high_evidence = effective_source_confidence == Some("high");
        if primary_is_approved && !primary_has_high_evidence {
            mark_weak_listing_evidence(&mut primary.report);
        }

        if raw.configuration_action == "installed" {
            if can_automatically_accept(&primary, effective_source_confidence) {
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
                    literal_observation_manufacturer(&raw.manufacturer),
                    &raw.model,
                    &raw.avionics_types,
                    raw.source_evidence_text.as_deref(),
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
        let replacement_identity = IdentityInput {
            manufacturer: literal_observation_manufacturer(&replacement.manufacturer)
                .unwrap_or_default(),
            model: &replacement.model,
            avionics_types: &replacement.avionics_types,
            quantity: 1,
        };
        let replacement_request = identity_request(
            row,
            source_url.as_deref(),
            &listing_context,
            &replacement_identity,
            raw.source_evidence_text.as_deref(),
        );
        if let Some(reason) = deterministic_generic_avionics_rejection_reason(&replacement_request)
        {
            let mut independent_raw = raw.clone();
            independent_raw.configuration_action = "installed".to_string();
            independent_raw.replaces = None;
            if can_automatically_accept(&primary, raw.source_confidence.as_deref()) {
                let primary_id = primary
                    .approved_id
                    .expect("approved primary has a catalog id");
                let primary_identity_key = primary
                    .identity_key
                    .clone()
                    .expect("approved primary has a product key");
                let is_removal = raw.configuration_action == "removes";
                let incoming_link = PreparedLink {
                    identity_key: primary_identity_key.clone(),
                    avionics_model_id: primary_id,
                    authorization: primary.authorization.clone(),
                    expected_collision_closure_sha256: primary.collision_closure_sha256.clone(),
                    quantity: raw.quantity,
                    source_notes: raw.source_evidence_text.clone(),
                    source_confidence: Some("high".to_string()),
                    configuration_action: if is_removal {
                        "removes".to_string()
                    } else {
                        "installed".to_string()
                    },
                    replaces_avionics_model_id: is_removal.then_some(primary_id),
                    replacement_authorization: is_removal
                        .then(|| primary.authorization.clone())
                        .flatten(),
                    replacement_identity_key: is_removal.then_some(primary_identity_key),
                    expected_replacement_collision_closure_sha256: is_removal
                        .then(|| primary.collision_closure_sha256.clone())
                        .flatten(),
                    preserved_association_guard: None,
                };
                if let Err(error) =
                    merge_prepared_link_for_candidate(&mut prepared, incoming_link, &mut primary)
                {
                    blocking_reasons.push(format!("candidate {candidate_index}: {error}"));
                }
            } else {
                let suggested_id = primary
                    .suggested_product
                    .as_ref()
                    .and_then(|product| product.id);
                let self_removal_aspect_id = (raw.configuration_action == "removes"
                    && suggested_id.is_none())
                .then(|| {
                    ReviewAspectId::String(format!("avionics:{candidate_index}:removal-product"))
                });
                let mut aspect = primary_residual_aspect(
                    candidate_index,
                    if raw.configuration_action == "removes" {
                        raw
                    } else {
                        &independent_raw
                    },
                    primary.report.reason.clone(),
                    primary.suggested_product.clone(),
                    self_removal_aspect_id.clone(),
                );
                if raw.configuration_action == "removes" {
                    aspect.replaces_product_id = suggested_id;
                }
                match attach_unique_exact_review_candidate(
                    db,
                    aspect,
                    literal_observation_manufacturer(&raw.manufacturer),
                    &raw.model,
                    &raw.avionics_types,
                    raw.source_evidence_text.as_deref(),
                )
                .await
                {
                    Ok(aspect) => {
                        let target_suggestion = aspect.suggested_product.clone();
                        residual_aspects.push(aspect);
                        if let Some(self_removal_aspect_id) = self_removal_aspect_id {
                            let mut target = primary_residual_aspect(
                                candidate_index,
                                &independent_raw,
                                primary.report.reason.clone(),
                                target_suggestion,
                                None,
                            );
                            target.id = self_removal_aspect_id;
                            target.quantity = 1;
                            residual_aspects.push(target);
                        }
                    }
                    Err(error) => blocking_reasons.push(format!(
                        "candidate {candidate_index}: exact primary catalog review retrieval failed: {error}"
                    )),
                }
            }
            listing_report.candidates.push(primary.report);
            listing_report.candidates.push(outcome_report(
                candidate_index,
                "replacement",
                &replacement_identity,
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
        let mut replacement_attempt = if replacement_identity.manufacturer.is_empty() {
            match resolve_model_only_identity_attempt(
                db,
                apply,
                candidate_index,
                "replacement",
                replacement_identity,
                &raw.configuration_action,
                raw.source_evidence_text.as_deref(),
                raw.source_confidence.as_deref(),
                catalog_statuses,
            )
            .await
            {
                Ok(attempt) => attempt,
                Err(error) => {
                    blocking_reasons.push(format!(
                        "candidate {candidate_index}: model-only replacement identity resolution failed: {error}"
                    ));
                    continue;
                }
            }
        } else if requires_provider {
            resolve_identity_attempt(
                db,
                scoped_extractor
                    .as_ref()
                    .expect("the provider barrier requires a configured extractor"),
                apply,
                row,
                source_url.as_deref(),
                &listing_context,
                candidate_index,
                "replacement",
                replacement_identity,
                &raw.configuration_action,
                raw.source_evidence_text.as_deref(),
                raw.source_confidence.as_deref(),
                catalog_statuses,
                &mut review_revision,
            )
            .await
        } else {
            match resolve_local_only_identity_attempt(
                db,
                apply,
                row,
                source_url.as_deref(),
                &listing_context,
                candidate_index,
                "replacement",
                replacement_identity,
                &raw.configuration_action,
                raw.source_evidence_text.as_deref(),
                raw.source_confidence.as_deref(),
                catalog_statuses,
            )
            .await
            {
                Ok(Some(attempt)) => attempt,
                Ok(None) => {
                    execution_order.push((candidate_index, true));
                    continue;
                }
                Err(error) => {
                    blocking_reasons.push(format!(
                        "candidate {candidate_index}: provider-free replacement identity resolution failed: {error}"
                    ));
                    continue;
                }
            }
        };
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
                literal_observation_manufacturer(&raw.manufacturer),
                &raw.model,
                &raw.avionics_types,
                raw.source_evidence_text.as_deref(),
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
                literal_observation_manufacturer(&replacement.manufacturer),
                &replacement.model,
                &replacement.avionics_types,
                raw.source_evidence_text.as_deref(),
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

    listing_report
        .candidates
        .sort_by_key(|candidate| (candidate.candidate_index, candidate.role != "primary"));
    if !provider_phase_blocked_at_barrier {
        if let Some(reason) = review_revision.stale_reason.as_ref() {
            blocking_reasons.push(format!(
                "pending review revision could not be advanced safely: {reason}"
            ));
        }
        if let Err(error) = validate_prepared_links(&prepared) {
            blocking_reasons.push(format!("listing avionics action graph is invalid: {error}"));
        }
        if apply {
            if let Err(error) = validate_prepared_link_commit_readiness(&prepared) {
                blocking_reasons.push(format!("prepared listing avionics cannot commit: {error}"));
            }
        }
    }
    if !provider_phase_blocked_at_barrier {
        listing_report.prepared_link_count = prepared.len();
    }
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

    let accepted_links = match prepared
        .iter()
        .map(automated_link_from_prepared)
        .collect::<VerificationResult<Vec<_>>>()
    {
        Ok(links) => links,
        Err(error) => {
            listing_report.status = "blocked".to_string();
            listing_report.error =
                Some(format!("prepared listing avionics cannot commit: {error}"));
            return listing_report;
        }
    };
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
        occurrence_dispositions,
        linked_occurrence_guards,
        residual_aspects,
    };
    match apply_automated_avionics_review(db, &request).await {
        Ok(result) => {
            listing_report.status = "applied".to_string();
            listing_report.applied = true;
            listing_report.accepted = result.accepted_link_count.max(0) as usize;
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
    match reconcile_or_push_prepared_link(prepared, incoming) {
        Ok(true) => {
            attempt.report.reason.push_str(
                "; reconciled the exact retained checkpoint occurrence with its preserved listing-link guard",
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
    manufacturer: Option<&str>,
    model: &str,
    avionics_types: &[String],
    listing_evidence_text: Option<&str>,
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
    let candidate = match manufacturer {
        Some(manufacturer) => {
            unique_exact_avionics_review_candidate(db, manufacturer, model, avionics_types).await
        }
        None => {
            unique_exact_avionics_model_observation_review_candidate(
                db,
                model,
                avionics_types,
                listing_evidence_text.unwrap_or_default(),
            )
            .await
        }
    }
    .map_err(|error| error.to_string())?;
    let Some(candidate) = candidate else {
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
            manufacturer: None,
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
    let manufacturer = literal_observation_manufacturer(&raw.manufacturer);
    PendingReviewAspect {
        id: ReviewAspectId::String(format!("avionics:{candidate_index}:primary")),
        kind: "avionics".to_string(),
        label: observation_label(manufacturer, &raw.model),
        observed_text: observation_text(
            manufacturer,
            &raw.model,
            &raw.avionics_types,
            raw.quantity,
            &raw.configuration_action,
        ),
        required: true,
        reason,
        suggested_product,
        proposed_product: manufacturer.map(|manufacturer| {
            ReviewProduct::proposed(
                observation_component(manufacturer, "Unknown manufacturer"),
                observation_component(&raw.model, "Unknown model"),
                raw.avionics_types.clone(),
            )
        }),
        allowed_actions: residual_review_actions(manufacturer.is_some()),
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
    let manufacturer = literal_observation_manufacturer(&replacement.manufacturer);
    PendingReviewAspect {
        id: ReviewAspectId::String(format!("avionics:{candidate_index}:replacement")),
        kind: "avionics".to_string(),
        label: observation_label(manufacturer, &replacement.model),
        observed_text: observation_text(
            manufacturer,
            &replacement.model,
            &replacement.avionics_types,
            1,
            "installed",
        ),
        required: true,
        reason,
        suggested_product,
        proposed_product: manufacturer.map(|manufacturer| {
            ReviewProduct::proposed(
                observation_component(manufacturer, "Unknown manufacturer"),
                observation_component(&replacement.model, "Unknown model"),
                replacement.avionics_types.clone(),
            )
        }),
        allowed_actions: residual_review_actions(manufacturer.is_some()),
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

fn residual_review_actions(has_literal_manufacturer: bool) -> Vec<ReviewAction> {
    let mut actions = vec![ReviewAction::UseVerifiedProduct];
    if has_literal_manufacturer {
        actions.push(ReviewAction::CreateVerifiedProduct);
    }
    actions.push(ReviewAction::Discard);
    actions
}

fn observation_label(manufacturer: Option<&str>, model: &str) -> String {
    let label = format!(
        "{} {}",
        manufacturer.unwrap_or_default().trim(),
        model.trim()
    )
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
    manufacturer: Option<&str>,
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

fn reconcile_or_push_prepared_link(
    prepared: &mut Vec<PreparedLink>,
    incoming: PreparedLink,
) -> Result<bool, String> {
    if let Some(index) = prepared
        .iter()
        .position(|link| link.identity_key == incoming.identity_key)
    {
        reconcile_preserved_link_or_reject_duplicate(&mut prepared[index], &incoming)?;
        return Ok(true);
    }
    prepared.push(incoming);
    Ok(false)
}

fn reconcile_preserved_link_or_reject_duplicate(
    existing: &mut PreparedLink,
    incoming: &PreparedLink,
) -> Result<(), String> {
    let exact_source_notes = existing
        .source_notes
        .as_deref()
        .filter(|evidence| !evidence.trim().is_empty());
    let exact_preserved_checkpoint_duplicate = existing.identity_key == incoming.identity_key
        && existing.avionics_model_id == incoming.avionics_model_id
        && existing.authorization == incoming.authorization
        && existing.expected_collision_closure_sha256 == incoming.expected_collision_closure_sha256
        && existing.quantity == incoming.quantity
        && exact_source_notes.is_some()
        && exact_source_notes
            == incoming
                .source_notes
                .as_deref()
                .filter(|evidence| !evidence.trim().is_empty())
        && existing.source_confidence == incoming.source_confidence
        && existing.configuration_action == incoming.configuration_action
        && existing.replaces_avionics_model_id == incoming.replaces_avionics_model_id
        && existing.replacement_authorization == incoming.replacement_authorization
        && existing.replacement_identity_key == incoming.replacement_identity_key
        && existing.expected_replacement_collision_closure_sha256
            == incoming.expected_replacement_collision_closure_sha256
        && existing.preserved_association_guard.is_some()
            != incoming.preserved_association_guard.is_some();
    if exact_preserved_checkpoint_duplicate {
        if existing.preserved_association_guard.is_none() {
            existing.preserved_association_guard = incoming.preserved_association_guard.clone();
        }
        return Ok(());
    }

    Err(format!(
        "catalog id {} resolved from multiple retained avionics occurrences; extraction must contain exactly one occurrence per canonical product with explicit physical-unit quantity",
        existing.avionics_model_id
    ))
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

fn verified_local_reuse_authorization(
    approved: &ApprovedAvionicsIdentity,
) -> AutomatedAssociationAuthorization {
    match approved.verified_local_reuse_proof {
        Some(VerifiedLocalReuseProof::GlobalExactModel) => {
            AutomatedAssociationAuthorization::GlobalExactModelReuse
        }
        Some(VerifiedLocalReuseProof::ManufacturerScoped) | None => {
            AutomatedAssociationAuthorization::ManufacturerReuse
        }
    }
}

fn current_manufacturer_reuse_authorization(
    approved: &ApprovedAvionicsIdentity,
    reuse_is_current: bool,
) -> Option<AutomatedAssociationAuthorization> {
    reuse_is_current.then(|| verified_local_reuse_authorization(approved))
}

#[allow(clippy::too_many_arguments)]
async fn resolve_model_only_identity_attempt(
    db: &AppDb,
    apply: bool,
    candidate_index: usize,
    role: &str,
    identity: IdentityInput<'_>,
    configuration_action: &str,
    source_evidence_text: Option<&str>,
    source_confidence: Option<&str>,
    catalog_statuses: &mut HashMap<i64, String>,
) -> Result<IdentityAttempt, String> {
    let listing_evidence_text = source_evidence_text.unwrap_or_default();
    let approved = resolve_verified_local_avionics_model_observation(
        db,
        identity.model,
        identity.avionics_types,
        listing_evidence_text,
    )
    .await
    .map_err(|error| error.to_string())?;
    if let Some(approved) = approved {
        let reuse_is_current = !apply
            || product_reuse_attestation_is_current(db, approved.id)
                .await
                .map_err(|error| error.to_string())?;
        if reuse_is_current {
            let identity_key = load_approved_graph_identity_key(db, approved.id)
                .await
                .map_err(|error| error.to_string())?;
            let approved_id = approved.id;
            let authorization = verified_local_reuse_authorization(&approved);
            let mut attempt = approved_attempt(
                apply,
                candidate_index,
                role,
                &identity,
                configuration_action,
                source_evidence_text,
                source_confidence,
                approved,
                Some(identity_key),
                catalog_statuses,
            );
            if apply {
                attempt.authorization = Some(authorization);
                attempt.collision_closure_sha256 = Some(
                    active_collision_closure_revision_sha256(db, approved_id)
                        .await
                        .map_err(|error| error.to_string())?,
                );
            }
            return Ok(attempt);
        }
    }

    let candidate = unique_exact_avionics_model_observation_review_candidate(
        db,
        identity.model,
        identity.avionics_types,
        listing_evidence_text,
    )
    .await
    .map_err(|error| error.to_string())?;
    let suggested_product = candidate.as_ref().map(|candidate| {
        let mut product = ReviewProduct::verified(
            candidate.id,
            candidate.manufacturer.clone(),
            candidate.model.clone(),
            candidate.avionics_types.clone(),
        );
        if !candidate.manufacturer_identifier_kind.trim().is_empty()
            && !candidate.manufacturer_identifier.trim().is_empty()
        {
            product = product.with_stable_identifier(
                candidate.manufacturer_identifier_kind.clone(),
                candidate.manufacturer_identifier.clone(),
            );
        }
        product
    });
    let reason = if candidate.is_some() {
        "the listing names an exact collision-safe approved avionics model, but that catalog product has no current reuse attestation; corroborate the suggested product or discard the observation"
    } else {
        "the listing names an avionics model without its manufacturer and no collision-safe approved local product can be authorized; select an existing verified product or discard the observation"
    };
    Ok(IdentityAttempt {
        report: outcome_report(
            candidate_index,
            role,
            &identity,
            configuration_action,
            source_evidence_text,
            source_confidence,
            "unresolved",
            candidate.as_ref().map(|candidate| candidate.id),
            candidate
                .as_ref()
                .map(|candidate| candidate.manufacturer.clone()),
            candidate.as_ref().map(|candidate| candidate.model.clone()),
            candidate
                .as_ref()
                .map(|candidate| candidate.avionics_types.clone())
                .unwrap_or_default(),
            reason.to_string(),
        ),
        approved_id: None,
        identity_key: None,
        collision_closure_sha256: None,
        authorization: None,
        suggested_product,
    })
}

/// Resolve one preflight-local identity without any provider or catalog write.
///
/// `None` means the exact local reuse proof changed after route planning. The
/// caller must move the complete observation behind the paid-work barrier and
/// let the ordinary resolver reconsider it there; it must never fall through
/// to that resolver while deterministic local work is still being processed.
#[allow(clippy::too_many_arguments)]
async fn resolve_local_only_identity_attempt(
    db: &AppDb,
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
) -> Result<Option<IdentityAttempt>, String> {
    let request = identity_request(
        row,
        source_url,
        listing_context,
        &identity,
        source_evidence_text,
    );
    let Some(approved) = resolve_verified_local_avionics_identity(db, &request)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if approved.id <= 0 {
        return Err(
            "verified-local resolution returned a non-persisted catalog identity".to_string(),
        );
    }
    if apply
        && !product_reuse_attestation_is_current(db, approved.id)
            .await
            .map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    let identity_key = load_approved_graph_identity_key(db, approved.id)
        .await
        .map_err(|error| error.to_string())?;
    let approved_id = approved.id;
    let reuse_authorization = verified_local_reuse_authorization(&approved);
    let mut attempt = approved_attempt(
        apply,
        candidate_index,
        role,
        &identity,
        configuration_action,
        source_evidence_text,
        source_confidence,
        approved,
        Some(identity_key),
        catalog_statuses,
    );
    if apply {
        attempt.authorization = Some(reuse_authorization);
        attempt.collision_closure_sha256 = Some(
            active_collision_closure_revision_sha256(db, approved_id)
                .await
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(Some(attempt))
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
    let outcome = if apply {
        match resolve_avionics_identity_for_automated_review(db, extractor, &request).await {
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
                if let Some(current_authorization) =
                    current_manufacturer_reuse_authorization(&approved, reuse_is_current)
                {
                    authorization = Some(current_authorization);
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
                            "the product identity is approved, but it has no current manufacturer-primary reuse attestation"
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
                        Some(
                            AutomatedAssociationAuthorization::ManufacturerReuse
                            | AutomatedAssociationAuthorization::CountableUnitManufacturerReuse
                            | AutomatedAssociationAuthorization::GlobalExactModelReuse,
                        ) => active_collision_closure_revision_sha256(db, model_id).await,
                        None => Err(AvionicsFingerprintError::Conflict(format!(
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
    if raw.model.trim().is_empty()
        || raw
            .manufacturer
            .as_deref()
            .is_some_and(|manufacturer| manufacturer.trim().is_empty())
    {
        return Some(
            "model must be non-empty and a present manufacturer must be non-empty".to_string(),
        );
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
                replacement.model.trim().is_empty()
                    || replacement
                        .manufacturer
                        .as_deref()
                        .is_some_and(|manufacturer| manufacturer.trim().is_empty())
            }) =>
        {
            Some(
                "replacement identity requires a non-empty model and any present manufacturer must be non-empty"
                    .to_string(),
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
    let review_source = retained_review_observations(db, row, listing_context).await?;
    let extraction_source = retained_avionics_source(
        row.extracted_listing_json.as_deref(),
        row.submission_source_url.as_deref(),
        row.rendered_html.as_deref(),
    );
    Ok(match (review_source, extraction_source) {
        (RetainedReviewObservationSource::Current(replay), RetainedAvionicsSource::Current(_)) => {
            RetainedObservationSource::Review {
                avionics: replay.avionics,
                preserved_aspects: replay.preserved_aspects,
            }
        }
        (
            RetainedReviewObservationSource::Current(replay),
            RetainedAvionicsSource::RequiresReextraction { reason },
        ) => RetainedObservationSource::RequiresReextraction {
            reason,
            preserved_aspects: replay.preserved_aspects,
        },
        (
            RetainedReviewObservationSource::RequiresFallback { preserved_aspects },
            RetainedAvionicsSource::Current(avionics),
        ) => RetainedObservationSource::Extraction {
            avionics,
            preserved_aspects,
        },
        (
            RetainedReviewObservationSource::RequiresFallback { preserved_aspects },
            RetainedAvionicsSource::RequiresReextraction { reason },
        ) => RetainedObservationSource::RequiresReextraction {
            reason,
            preserved_aspects,
        },
    })
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
            manufacturer: Some(product.manufacturer),
            model: product.model,
            avionics_types: product.capabilities,
            quantity: aspect.quantity,
            configuration_action: aspect.configuration_action.clone(),
            replaces: None,
            source_evidence_text: aspect.source_evidence_text.clone(),
            source_confidence: aspect.source_confidence.clone(),
        });
    }
    let Some(source_url) = row.submission_source_url.as_deref() else {
        return Ok(RetainedReviewObservationSource::RequiresFallback { preserved_aspects });
    };
    if (!avionics.is_empty()
        && validate_current_avionics_observations(
            &avionics,
            listing_context,
            source_url,
            row.rendered_html.as_deref().unwrap_or_default(),
        )
        .is_err())
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
    source_url: Option<&str>,
    rendered_html: Option<&str>,
) -> RetainedAvionicsSource {
    let Some(raw_json) = raw_json.filter(|raw_json| !raw_json.trim().is_empty()) else {
        return RetainedAvionicsSource::RequiresReextraction {
            reason: "the retained plugin submission has no extracted listing JSON".to_string(),
        };
    };
    let Some(source_url) = source_url.filter(|source_url| !source_url.trim().is_empty()) else {
        return RetainedAvionicsSource::RequiresReextraction {
            reason: "the retained plugin submission has no source URL".to_string(),
        };
    };
    if let Err(error) = parse_current_checkpoint_payload(raw_json) {
        return RetainedAvionicsSource::RequiresReextraction {
            reason: format!(
                "the retained plugin extraction is not an exact current checkpoint: {error}"
            ),
        };
    }
    match validate_unbound_current_avionics_extraction(
        raw_json,
        source_url,
        rendered_html.unwrap_or_default(),
    ) {
        Ok(avionics) if avionics.is_empty() => {
            RetainedAvionicsSource::RequiresReextraction {
                reason: "the retained plugin extraction contains no avionics capability arrays"
                    .to_string(),
            }
        }
        Ok(avionics) => RetainedAvionicsSource::Current(avionics),
        Err(error) => RetainedAvionicsSource::RequiresReextraction {
            reason: format!(
                "the retained plugin extraction does not satisfy the complete current avionics contract: {error}"
            ),
        },
    }
}

fn prepare_stored_listing_text(source_url: &str, rendered_html: &str) -> Result<String, String> {
    crate::extract::validate_source_url(source_url)
        .map_err(|error| format!("retained source URL is invalid: {error}"))?;
    let listing_text = listing_extraction_source(source_url, rendered_html)
        .map_err(|error| format!("retained listing source is invalid: {error}"))?;
    if listing_text.trim().is_empty() {
        return Err("retained rendered_html contains no usable listing text".to_string());
    }
    Ok(listing_text)
}

/// Commit one retained extraction with a validated current-schema avionics
/// member before identity resolution.
///
/// The extraction is derived data, not part of the signed source capture. The
/// single guarded update is nevertheless tied to every source and ownership
/// value used to produce it, the exact pending-review revision that selected
/// the submission, and the exact prior extraction/error state. Repeating the
/// same write is idempotent; a different concurrent extraction or any capture,
/// binding, owner, listing, or review change fails closed.
async fn persist_validated_listing_reextraction(
    db: &AppDb,
    row: &ListingSourceRow,
    submission_id: i64,
    extracted_listing_json: &str,
) -> VerificationResult<()> {
    let submission_source_url = row.submission_source_url.as_deref().ok_or_else(|| {
        AvionicsVerificationError::Validation(
            "validated re-extraction lost its submission source URL".to_string(),
        )
    })?;
    let rendered_html = row.rendered_html.as_deref().ok_or_else(|| {
        AvionicsVerificationError::Validation(
            "validated re-extraction lost its retained rendered HTML".to_string(),
        )
    })?;
    let rendered_html_sha256 = row.rendered_html_sha256.as_deref().ok_or_else(|| {
        AvionicsVerificationError::Validation(
            "validated re-extraction lost its rendered HTML SHA-256".to_string(),
        )
    })?;
    if extracted_listing_json.trim().is_empty() {
        return Err(AvionicsVerificationError::Validation(
            "validated re-extraction cannot persist an empty JSON document".to_string(),
        ));
    }

    let update_sql = db.sql(
        r#"
        UPDATE plugin_submissions AS submission
        SET extracted_listing_json = ?, extraction_error = NULL
        WHERE submission.id = ?
          AND submission.user_id = ?
          AND submission.source_url = ?
          AND submission.rendered_html = ?
          AND submission.rendered_html_sha256 = ?
          AND (
            submission.canonical_listing_id = ?
            OR (
              submission.canonical_listing_id IS NULL
              AND ? IS NULL
            )
          )
          AND (
            (
              (
                submission.extracted_listing_json = ?
                OR (
                  submission.extracted_listing_json IS NULL
                  AND ? IS NULL
                )
              )
              AND (
                submission.extraction_error = ?
                OR (
                  submission.extraction_error IS NULL
                  AND ? IS NULL
                )
              )
            )
            OR (
              submission.extracted_listing_json = ?
              AND submission.extraction_error IS NULL
            )
          )
          AND EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_reviews pending
            JOIN aircraft_sale_listings listing
              ON listing.id = pending.listing_id
            WHERE pending.id = ?
              AND pending.listing_id = ?
              AND pending.plugin_submission_id = submission.id
              AND pending.review_payload_sha256 = ?
              AND listing.created_by_user_id = ?
              AND listing.ingestion_state = 'pending_review'
              AND listing.is_verified = FALSE
              AND (
                listing.source_url = ?
                OR (
                  listing.source_url IS NULL
                  AND ? IS NULL
                )
              )
              AND (
                submission.canonical_listing_id = listing.id
                OR (
                  submission.canonical_listing_id IS NULL
                  AND listing.source_url = submission.source_url
                )
              )
          )
        "#,
    );

    macro_rules! execute_guarded_update {
        ($transaction:expr) => {{
            sqlx::query(&update_sql)
                .bind(extracted_listing_json)
                .bind(submission_id)
                .bind(row.listing_owner_user_id)
                .bind(submission_source_url)
                .bind(rendered_html)
                .bind(rendered_html_sha256)
                .bind(row.submission_canonical_listing_id)
                .bind(row.submission_canonical_listing_id)
                .bind(row.extracted_listing_json.as_deref())
                .bind(row.extracted_listing_json.as_deref())
                .bind(row.submission_extraction_error.as_deref())
                .bind(row.submission_extraction_error.as_deref())
                .bind(extracted_listing_json)
                .bind(row.pending_review_id)
                .bind(row.listing_id)
                .bind(&row.review_payload_sha256)
                .bind(row.listing_owner_user_id)
                .bind(row.listing_source_url.as_deref())
                .bind(row.listing_source_url.as_deref())
                .execute($transaction)
                .await?
                .rows_affected()
        }};
    }

    macro_rules! persist_sqlite_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let changed = execute_guarded_update!(&mut *transaction);
            if changed != 1 {
                return Err(AvionicsVerificationError::Validation(format!(
                    "plugin submission {submission_id} or its exact owner, listing, source capture, pending-review revision, or prior extraction state changed while re-extraction was running"
                )));
            }
            transaction.commit().await?;
            Ok(())
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => persist_sqlite_transaction!(pool),
        DatabaseBackend::Postgres(pool) => {
            let lock_listing_children = db.sql(POSTGRES_LISTING_CHILD_LOCK_SQL);
            let locked_state_sql = db.sql(
                r#"
                SELECT
                  listing.id AS listing_id,
                  listing.created_by_user_id AS listing_owner_user_id,
                  listing.source_url AS listing_source_url,
                  listing.ingestion_state,
                  listing.is_verified,
                  pending.id AS pending_review_id,
                  pending.plugin_submission_id AS pending_submission_id,
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
                JOIN plugin_submissions submission
                  ON submission.id = pending.plugin_submission_id
                WHERE listing.id = ?
                  AND pending.id = ?
                  AND submission.id = ?
                FOR UPDATE OF listing, pending, submission
                "#,
            );
            let mut transaction = pool.begin().await?;
            // Keep the same PostgreSQL ordering as every listing writer:
            // mutable listing-child tables first, then the exact parent,
            // pending-review, and retained-submission rows.
            sqlx::query(&lock_listing_children)
                .execute(&mut *transaction)
                .await?;
            let locked = sqlx::query_as::<_, LockedReextractionStateRow>(&locked_state_sql)
                .bind(row.listing_id)
                .bind(row.pending_review_id)
                .bind(submission_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    AvionicsVerificationError::Validation(format!(
                        "plugin submission {submission_id} or its exact listing and pending review changed while re-extraction was running"
                    ))
                })?;
            validate_locked_reextraction_state(
                row,
                submission_id,
                extracted_listing_json,
                &locked,
            )?;
            let changed = execute_guarded_update!(&mut *transaction);
            if changed != 1 {
                return Err(AvionicsVerificationError::Validation(format!(
                    "plugin submission {submission_id} or its exact owner, listing, source capture, pending-review revision, or prior extraction state changed while re-extraction was running"
                )));
            }
            transaction.commit().await?;
            Ok(())
        }
    }
}

fn validate_locked_reextraction_state(
    expected: &ListingSourceRow,
    submission_id: i64,
    extracted_listing_json: &str,
    locked: &LockedReextractionStateRow,
) -> VerificationResult<()> {
    let source_binding_is_current = locked.submission_canonical_listing_id
        == Some(locked.listing_id)
        || (locked.submission_canonical_listing_id.is_none()
            && locked.listing_source_url.as_deref() == Some(locked.submission_source_url.as_str()));
    let exact_state_is_current = locked.listing_id == expected.listing_id
        && locked.listing_owner_user_id == expected.listing_owner_user_id
        && locked.listing_source_url == expected.listing_source_url
        && locked.ingestion_state == "pending_review"
        && !locked.is_verified
        && locked.pending_review_id == expected.pending_review_id
        && locked.pending_submission_id == submission_id
        && locked.pending_aspect_count == expected.pending_aspect_count
        && locked.review_payload_json == expected.review_payload_json
        && locked.review_payload_sha256 == expected.review_payload_sha256
        && locked.submission_id == submission_id
        && expected.submission_id == Some(submission_id)
        && locked.submission_owner_user_id == expected.listing_owner_user_id
        && expected.submission_owner_user_id == Some(locked.submission_owner_user_id)
        && locked.submission_canonical_listing_id == expected.submission_canonical_listing_id
        && expected.submission_source_url.as_deref() == Some(locked.submission_source_url.as_str())
        && !locked.submission_source_url.trim().is_empty()
        && expected.rendered_html.as_deref() == Some(locked.rendered_html.as_str())
        && !locked.rendered_html.trim().is_empty()
        && expected.rendered_html_sha256.as_deref() == Some(locked.rendered_html_sha256.as_str())
        && valid_sha256(&locked.rendered_html_sha256)
        && sha256_hex(locked.rendered_html.as_bytes()) == locked.rendered_html_sha256
        && valid_sha256(&locked.review_payload_sha256)
        && sha256_hex(locked.review_payload_json.as_bytes()) == locked.review_payload_sha256
        && source_binding_is_current;
    if !exact_state_is_current {
        return Err(AvionicsVerificationError::Validation(format!(
            "plugin submission {submission_id} or its exact owner, listing, source capture, or pending-review revision changed while re-extraction was running"
        )));
    }
    let prior_is_current = locked.extracted_listing_json == expected.extracted_listing_json
        && locked.submission_extraction_error == expected.submission_extraction_error;
    let identical_retry = locked.extracted_listing_json.as_deref() == Some(extracted_listing_json)
        && locked.submission_extraction_error.is_none();
    if !prior_is_current && !identical_retry {
        return Err(AvionicsVerificationError::Validation(format!(
            "plugin submission {submission_id} prior extraction state changed while re-extraction was running"
        )));
    }
    Ok(())
}

async fn reextract_avionics(
    extractor: &GeminiListingExtractor,
    listing_text: &str,
    listing_context: &ListingEvidenceContext,
    source_url: &str,
    rendered_html: &str,
    prior_extracted_listing_json: Option<&str>,
) -> Result<ValidatedListingReextraction, String> {
    let extraction = extractor
        .extract_for_avionics_validation(listing_text)
        .await
        .map_err(|error| format!("Gemini listing extraction request failed: {error}"))?;
    let mut extracted = extraction.value;
    let avionics = validate_or_correct_listing_avionics(
        extractor,
        extraction.correction_token,
        listing_text,
        source_url,
        rendered_html,
        &mut extracted,
    )
    .await
    .map_err(|error| format!("Gemini returned invalid listing avionics: {error}"))?;
    for (index, observation) in avionics.iter().enumerate() {
        if let Some(issue) = raw_candidate_structure_issue(observation) {
            return Err(format!(
                "Gemini returned invalid current-schema avionics[{index}]: {issue}"
            ));
        }
    }
    validate_current_avionics_observations(&avionics, listing_context, source_url, rendered_html)
        .map_err(|error| format!("Gemini returned invalid current avionics: {error}"))?;
    let extracted_listing_json = if avionics.is_empty() {
        String::new()
    } else {
        let extracted_listing_json = merge_validated_avionics_into_listing_extraction(
            prior_extracted_listing_json,
            extracted,
            &avionics,
        )?;
        let persisted_avionics = validate_unbound_current_avionics_extraction(
            &extracted_listing_json,
            source_url,
            rendered_html,
        )
        .map_err(|error| {
            format!("repaired listing extraction failed final current validation: {error}")
        })?;
        if persisted_avionics != avionics {
            return Err(
                "repaired listing extraction changed validated avionics during persistence"
                    .to_string(),
            );
        }
        extracted_listing_json
    };
    Ok(ValidatedListingReextraction {
        extracted_listing_json,
        avionics,
    })
}

/// Build the complete current extraction that can durably replace the retained
/// checkpoint.
///
/// A structurally current retained listing keeps its non-avionics values. An
/// absent or structurally unusable checkpoint cannot supply values to
/// preserve, so the newly extracted complete listing becomes the repair base.
/// In both cases the validated avionics are injected and the final object must
/// satisfy both the complete listing and strict current avionics schemas.
fn merge_validated_avionics_into_listing_extraction(
    prior_extracted_listing_json: Option<&str>,
    mut newly_extracted_listing: Value,
    avionics: &[ParsedAvionics],
) -> Result<String, String> {
    let avionics_value = serde_json::to_value(avionics)
        .map_err(|error| format!("could not serialize validated avionics extraction: {error}"))?;

    newly_extracted_listing
        .as_object_mut()
        .ok_or_else(|| {
            "cannot durably persist avionics re-extraction because the newly extracted listing is not a top-level object"
                .to_string()
        })?
        .insert("avionics".to_string(), avionics_value.clone());
    let newly_extracted_listing =
        canonical_complete_current_listing_value(&newly_extracted_listing, "newly extracted")?;

    let mut repaired_listing = prior_extracted_listing_json
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| canonical_complete_current_listing_value(&value, "retained").ok())
        .unwrap_or(newly_extracted_listing);
    repaired_listing
        .as_object_mut()
        .expect("a validated current listing extraction is a top-level object")
        .insert("avionics".to_string(), avionics_value);
    let repaired_listing = canonical_complete_current_listing_value(&repaired_listing, "repaired")?;

    serde_json::to_string(&repaired_listing)
        .map_err(|error| format!("could not serialize validated listing extraction: {error}"))
}

fn canonical_complete_current_listing_value(value: &Value, label: &str) -> Result<Value, String> {
    if !value.is_object() {
        return Err(format!(
            "cannot durably persist avionics re-extraction because the {label} listing extraction is not a top-level object"
        ));
    }
    let serialized = serde_json::to_string(value).map_err(|error| {
        format!("could not serialize the {label} listing extraction for validation: {error}")
    })?;
    let (parsed_listing, identity_recovery) = parse_current_checkpoint_payload(&serialized)
        .map_err(|error| {
            format!(
                "cannot durably persist avionics re-extraction because the {label} listing extraction is not an exact current checkpoint: {error}"
            )
        })?;
    parse_current_avionics_extraction_value(value).map_err(|error| {
        format!(
            "cannot durably persist avionics re-extraction because the {label} listing extraction does not use the current avionics schema: {error}"
        )
    })?;
    Ok(canonical_current_checkpoint_payload(
        &parsed_listing,
        identity_recovery.as_ref(),
    ))
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

fn automated_link_from_prepared(link: &PreparedLink) -> VerificationResult<AutomatedAvionicsLink> {
    let authorization = link.authorization.clone().ok_or_else(|| {
        AvionicsVerificationError::Validation(format!(
            "catalog id {} is not commit-ready: automatic association authorization is missing",
            link.avionics_model_id
        ))
    })?;
    let expected_collision_closure_sha256 = link
        .expected_collision_closure_sha256
        .clone()
        .ok_or_else(|| {
            AvionicsVerificationError::Validation(format!(
                "catalog id {} is not commit-ready: resolution-time collision-closure revision is missing",
                link.avionics_model_id
            ))
        })?;
    if matches!(link.configuration_action.as_str(), "replaces" | "removes") {
        if link.replaces_avionics_model_id.is_none() {
            return Err(AvionicsVerificationError::Validation(format!(
                "catalog id {} is not commit-ready for {}: replacement catalog id is missing",
                link.avionics_model_id, link.configuration_action
            )));
        }
        if link.replacement_authorization.is_none() {
            return Err(AvionicsVerificationError::Validation(format!(
                "catalog id {} is not commit-ready for {}: replacement automatic association authorization is missing",
                link.avionics_model_id, link.configuration_action
            )));
        }
        if link.expected_replacement_collision_closure_sha256.is_none() {
            return Err(AvionicsVerificationError::Validation(format!(
                "catalog id {} is not commit-ready for {}: replacement collision-closure revision is missing",
                link.avionics_model_id, link.configuration_action
            )));
        }
    }
    let accepted = AutomatedAvionicsLink {
        avionics_model_id: link.avionics_model_id,
        authorization,
        expected_collision_closure_sha256,
        quantity: link.quantity,
        source_notes: link.source_notes.clone(),
        source_confidence: link.source_confidence.clone(),
        configuration_action: link.configuration_action.clone(),
        replaces_avionics_model_id: link.replaces_avionics_model_id,
        replacement_authorization: link.replacement_authorization.clone(),
        expected_replacement_collision_closure_sha256: link
            .expected_replacement_collision_closure_sha256
            .clone(),
        preserved_association_guard: link.preserved_association_guard.clone(),
    };
    validate_automated_avionics_link(&accepted)
        .map_err(|error| AvionicsVerificationError::Validation(error.to_string()))?;
    Ok(accepted)
}

fn validate_prepared_link_commit_readiness(links: &[PreparedLink]) -> VerificationResult<()> {
    for link in links {
        automated_link_from_prepared(link)?;
    }
    Ok(())
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
    use crate::models::ParsedListing;
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

    #[derive(Clone)]
    struct ListingExtractionEndpointState {
        request_count: Arc<AtomicUsize>,
        extractions: Arc<Vec<Value>>,
    }

    async fn listing_extraction_endpoint_response(
        State(state): State<ListingExtractionEndpointState>,
        Json(_request): Json<Value>,
    ) -> Json<Value> {
        let index = state.request_count.fetch_add(1, Ordering::SeqCst);
        let extraction = state
            .extractions
            .get(index)
            .or_else(|| state.extractions.last())
            .expect("the test endpoint requires at least one extraction");
        Json(json!({
            "candidates": [{
                "content": {"parts": [{"text": extraction.to_string()}]}
            }]
        }))
    }

    async fn spawn_listing_extraction_endpoint(
        avionics: Value,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        spawn_listing_extraction_sequence_endpoint(vec![avionics]).await
    }

    async fn spawn_listing_extraction_sequence_endpoint(
        avionics: Vec<Value>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let state = ListingExtractionEndpointState {
            request_count: request_count.clone(),
            extractions: Arc::new(
                avionics
                    .into_iter()
                    .enumerate()
                    .map(|(index, avionics)| {
                        if index == 0 {
                            json!({
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
                                "avionics": avionics,
                                "valuation_facts": []
                            })
                        } else {
                            json!({"avionics": avionics})
                        }
                    })
                    .collect(),
            ),
        };
        let app = Router::new()
            .route("/", post(listing_extraction_endpoint_response))
            .with_state(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/"), request_count, server)
    }

    const CONTROLLER_ROLE_LISTING_URL: &str =
        "https://www.controller.com/listing/for-sale/252742967/example";

    fn trusted_controller_role_html(field: &str) -> String {
        format!(
            r#"<html><body>
              <p>Currently hangared at KSAR</p>
              <main id="main-content" class="detail__main-content">
                <h1 class="detail__title">Test Aircraft</h1>
                <div class="listing-prices__retail-price">$100,000</div>
                <div class="detail__specs">
                  <h3 class="detail__specs-heading">Avionics</h3>
                  <div class="detail__specs-wrapper">
                    <div class="detail__specs-label">ADS-B Equipped</div>
                    <div class="detail__specs-value">Yes</div>
                    <div class="detail__specs-label">Avionics/Radios</div>
                    <div class="detail__specs-value">{field}</div>
                  </div>
                </div>
              </main>
            </body></html>"#
        )
    }

    fn retained_legacy_listing_extraction() -> Value {
        json!({
            "manufacturer": "Retained Aircraft Maker",
            "model": "Retained Aircraft Model",
            "variant": "Retained Aircraft Variant",
            "model_year": 1997,
            "asking_price_usd": 123456,
            "currency": "USD",
            "airframe_hours": 4321.25,
            "engine_hours": 987.5,
            "engine_time_basis": "SMOH",
            "engine_time_evidence": "987.5 SMOH",
            "engine_time_confidence": "high",
            "propeller_hours": null,
            "propeller_time_basis": "unknown",
            "propeller_time_evidence": null,
            "propeller_time_confidence": null,
            "installed_engine": null,
            "installed_propeller": null,
            "registration_number": "N98765",
            "serial_number": "RETAINED-SERIAL",
            "status": "retained-status",
            "avionics": [{
                "manufacturer": "Legacy",
                "model": "Scalar payload",
                "type": "Flight Display"
            }],
            "valuation_facts": [{
                "kind": "paint_condition",
                "value": "retained value",
                "evidence_text": "retained evidence",
                "source_url": null,
                "confidence": "low"
            }]
        })
    }

    fn current_listing_extraction_with_avionics(avionics: Value) -> String {
        let mut payload = retained_legacy_listing_extraction();
        payload["avionics"] = avionics;
        payload.to_string()
    }

    async fn store_retained_legacy_listing_extraction(db: &AppDb, submission_id: i64) {
        sqlx::query(
            r#"
            UPDATE plugin_submissions
            SET extracted_listing_json = ?,
                extraction_error = 'legacy extraction unavailable'
            WHERE id = ?
            "#,
        )
        .bind(retained_legacy_listing_extraction().to_string())
        .bind(submission_id)
        .execute(sqlite_pool(db))
        .await
        .unwrap();
    }

    fn locked_reextraction_state(
        row: &ListingSourceRow,
        submission_id: i64,
    ) -> LockedReextractionStateRow {
        LockedReextractionStateRow {
            listing_id: row.listing_id,
            listing_owner_user_id: row.listing_owner_user_id,
            listing_source_url: row.listing_source_url.clone(),
            ingestion_state: "pending_review".to_string(),
            is_verified: false,
            pending_review_id: row.pending_review_id,
            pending_submission_id: submission_id,
            pending_aspect_count: row.pending_aspect_count,
            review_payload_json: row.review_payload_json.clone(),
            review_payload_sha256: row.review_payload_sha256.clone(),
            submission_id,
            submission_owner_user_id: row.submission_owner_user_id.unwrap(),
            submission_canonical_listing_id: row.submission_canonical_listing_id,
            submission_source_url: row.submission_source_url.clone().unwrap(),
            rendered_html: row.rendered_html.clone().unwrap(),
            rendered_html_sha256: row.rendered_html_sha256.clone().unwrap(),
            extracted_listing_json: row.extracted_listing_json.clone(),
            submission_extraction_error: row.submission_extraction_error.clone(),
        }
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
        assert_eq!(plan.known_total_provider_requests_minimum_baseline, 21);
        assert_eq!(plan.known_total_provider_requests_all_positive_baseline, 49);
        assert_eq!(
            plan.known_total_provider_requests_validation_envelope_maximum,
            144
        );
        assert!(plan.legacy_reextraction_identity_outputs_unknown);
        assert!(!plan.logical_provider_request_counts_include_transport_retries);
        assert_eq!(plan.default_max_transport_attempts_per_logical_request, 4);
        assert!(plan
            .transport_retry_note
            .contains("four transport attempts"));
        assert!(plan
            .uncertainty_note
            .contains("exactly two logical requests per re-extraction"));
        assert!(plan
            .uncertainty_note
            .contains("Those paths are mutually exclusive"));
        assert!(plan
            .uncertainty_note
            .contains("parsed once without JSON repair"));
        assert!(plan
            .grounded_pass_note
            .contains("exactly one tools-disabled"));
        assert!(plan.requires_provider());

        let local_plan = provider_request_plan(&AvionicsVerificationPreflightSummary {
            retained_identity_components: 3,
            verified_local_identity_components: 3,
            generic_invalid_identity_components: 1,
            ..AvionicsVerificationPreflightSummary::default()
        });
        assert_eq!(
            local_plan.known_total_provider_requests_validation_envelope_maximum,
            0
        );
        assert!(!local_plan.requires_provider());
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
            Some(&extractor),
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
    fn raw_parser_preserves_capability_arrays_and_explicit_actions() {
        let parsed = parse_current_avionics_extraction_json(
            r#"{
              "avionics": [
                {
                  "manufacturer":"Garmin","model":"GTX 345R","types":["Transponder"],"quantity":1,
                  "configuration_action":"installed","replaces":null,
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
                manufacturer: Some("Garmin".to_string()),
                model: "GNS 530W".to_string(),
                avionics_types: vec!["GPS".to_string(), "NAV".to_string(), "COM".to_string()],
            })
        );
        assert_eq!(parsed[1].source_confidence.as_deref(), Some("medium"));
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
            verified_local_reuse_proof: None,
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
    fn grounded_approval_requires_current_reuse_before_automatic_association() {
        let approved = ApprovedAvionicsIdentity {
            id: 42,
            manufacturer: "Garmin".to_string(),
            model: "GNX 375".to_string(),
            avionics_types: vec!["GPS".to_string(), "Transponder".to_string()],
            manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
            manufacturer_identifier: "GNX-375".to_string(),
            evidence_url: "https://www.garmin.com/gnx375".to_string(),
            evidence_title: "GNX 375".to_string(),
            evidence: "Grounded manufacturer evidence identifies the product.".to_string(),
            reason: "freshly curated exact product".to_string(),
            grounded_claim_source_urls: vec!["https://www.garmin.com/gnx375".to_string()],
            verified_local_reuse_proof: None,
        };

        assert_eq!(
            current_manufacturer_reuse_authorization(&approved, false),
            None
        );
        assert_eq!(
            current_manufacturer_reuse_authorization(&approved, true),
            Some(AutomatedAssociationAuthorization::ManufacturerReuse)
        );
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
            manufacturer: Some("Garmin".to_string()),
            model: "GTN 750Xi".to_string(),
            avionics_types: vec!["GPS".to_string(), "NAV".to_string(), "COM".to_string()],
            quantity: 1,
            configuration_action: "replaces".to_string(),
            replaces: Some(crate::models::ParsedAvionicsReference {
                manufacturer: Some("Garmin".to_string()),
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
    fn model_only_observation_is_reviewable_without_a_creation_proposal() {
        let raw = ParsedAvionics {
            manufacturer: None,
            model: "WX500".to_string(),
            avionics_types: vec!["Lightning Detection".to_string()],
            quantity: 1,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some("WX500 Stormscope".to_string()),
            source_confidence: Some("high".to_string()),
        };

        assert!(raw_candidate_structure_issue(&raw).is_none());
        assert!(literal_observation_manufacturer(&raw.manufacturer).is_none());
        let aspect = primary_residual_aspect(
            8,
            &raw,
            "manufacturer is absent from the listing".to_string(),
            None,
            None,
        );

        assert_eq!(aspect.label, "WX500");
        assert!(aspect
            .observed_text
            .starts_with("WX500 · Lightning Detection"));
        assert!(aspect.proposed_product.is_none());
        assert_eq!(
            aspect.allowed_actions,
            vec![ReviewAction::UseVerifiedProduct, ReviewAction::Discard]
        );
    }

    #[tokio::test]
    async fn model_only_attempt_authorizes_only_a_current_attested_local_product() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_named_product_for_manufacturer(
            &db,
            true,
            "Garmin",
            "https://www.garmin.com",
            "G5 MODEL ONLY TEST",
            "G5-MODEL-ONLY-TEST",
            "Flight Display",
        )
        .await;
        let mut catalog_statuses = HashMap::from([(product_id, "approved".to_string())]);

        let attempt = resolve_model_only_identity_attempt(
            &db,
            true,
            3,
            "primary",
            IdentityInput {
                manufacturer: "",
                model: "G5 MODEL ONLY TEST",
                avionics_types: &["Flight Display".to_string()],
                quantity: 1,
            },
            "installed",
            Some("G5 MODEL ONLY TEST installed"),
            Some("high"),
            &mut catalog_statuses,
        )
        .await
        .unwrap();

        assert_eq!(attempt.approved_id, Some(product_id));
        assert_eq!(
            attempt.authorization,
            Some(AutomatedAssociationAuthorization::GlobalExactModelReuse)
        );
        assert!(attempt.collision_closure_sha256.is_some());
        assert_eq!(
            attempt.report.canonical_manufacturer.as_deref(),
            Some("Garmin")
        );
    }

    #[tokio::test]
    async fn model_only_attempt_suggests_but_never_authorizes_an_unattested_product() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_named_product_for_manufacturer(
            &db,
            false,
            "L3",
            "https://www.l3harris.com",
            "WX-500 MODEL ONLY TEST",
            "WX-500-MODEL-ONLY-TEST",
            "Lightning Detection",
        )
        .await;
        let mut catalog_statuses = HashMap::from([(product_id, "approved".to_string())]);

        let attempt = resolve_model_only_identity_attempt(
            &db,
            true,
            4,
            "replacement",
            IdentityInput {
                manufacturer: "",
                model: "WX500 MODEL ONLY TEST",
                avionics_types: &["Lightning Detection".to_string()],
                quantity: 1,
            },
            "replaces",
            Some("WX500 MODEL ONLY TEST Stormscope"),
            Some("high"),
            &mut catalog_statuses,
        )
        .await
        .unwrap();

        assert_eq!(attempt.approved_id, None);
        assert_eq!(attempt.authorization, None);
        assert_eq!(attempt.collision_closure_sha256, None);
        assert_eq!(attempt.report.status, "unresolved");
        assert_eq!(
            attempt
                .suggested_product
                .as_ref()
                .and_then(|product| product.id),
            Some(product_id)
        );
    }

    #[test]
    fn malformed_candidate_becomes_residual_review_instead_of_blocking_listing() {
        let raw = ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
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
    fn only_exact_structurally_valid_generic_labels_are_deterministic_discards() {
        for model in ["TAWS", "XM Weather & Radio", "Active Traffic", "AHRS"] {
            let raw = ParsedAvionics {
                manufacturer: Some("Garmin".to_string()),
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
                .expect("an exact generic label must be eligible for deterministic discard");
            assert!(issue.contains("rather than a specific avionics product"));
        }

        for model in [
            "GPS 150",
            "WX 500",
            "GDL 69A XM Receiver",
            "AHRS-200",
            "TAWS-B",
        ] {
            let raw = ParsedAvionics {
                manufacturer: Some("Garmin".to_string()),
                model: model.to_string(),
                avionics_types: vec!["Navigation".to_string()],
                quantity: 1,
                configuration_action: "installed".to_string(),
                replaces: None,
                source_evidence_text: Some(model.to_string()),
                source_confidence: Some("high".to_string()),
            };

            assert!(raw_candidate_structure_issue(&raw).is_none());
            assert!(
                generic_model_issue(&raw).is_none(),
                "concrete product {model} must continue through normal identity resolution"
            );
        }
    }

    #[test]
    fn malformed_generic_labels_bypass_deterministic_discard_and_remain_for_review() {
        let raw = ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
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
                manufacturer: Some("".to_string()),
                model: "".to_string(),
                avionics_types: vec!["GPS".to_string()],
            }),
            ..raw
        };
        assert_eq!(
            raw_candidate_structure_issue(&malformed_target).as_deref(),
            Some(
                "replacement identity requires a non-empty model and any present manufacturer must be non-empty"
            )
        );
    }

    #[tokio::test]
    async fn exact_generic_current_observation_is_discarded_without_provider_requests() {
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
                .known_total_provider_requests_minimum_baseline,
            0
        );

        let report = verify_listing_avionics_page(
            &db,
            None,
            AvionicsVerificationExecutionMode::Preview,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();

        let listing = &report.listings[0];
        assert_eq!(listing.status, "previewed");
        assert_eq!(listing.safely_discarded, 1);
        assert_eq!(listing.remaining_review_aspects, 0);
        assert_eq!(listing.candidates.len(), 1);
        assert_eq!(listing.candidates[0].status, "rejected");
        assert!(listing.candidates[0].resolution_attempted);

        let applied = verify_listing_avionics_page(
            &db,
            None,
            AvionicsVerificationExecutionMode::Apply,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();
        assert_eq!(applied.listings[0].status, "applied");
        let disposition: (String, Option<i64>) = sqlx::query_as(
            r#"
            SELECT outcome, avionics_model_id
            FROM aircraft_sale_listing_avionics_dispositions
            WHERE aircraft_sale_listing_id = ?
              AND occurrence_index = 0
              AND occurrence_role = 'primary'
            "#,
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(disposition, ("discarded".to_string(), None));
        let usage: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(usage, 0);
    }

    #[tokio::test]
    async fn generic_primary_keeps_concrete_replacement_on_its_independent_local_path() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let replacement_id = seed_approved_named_product_for_manufacturer(
            &db,
            true,
            "Garmin",
            "https://www.garmin.com",
            "GNS 430W",
            "GNS-430W",
            "GPS",
        )
        .await;
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        for action in ["replaces", "removes"] {
            let listing_id = seed_generic_primary_with_concrete_replacement_listing(
                &db,
                &format!("generic-primary-concrete-replacement-{action}"),
                action,
                "GNS 430W",
            )
            .await;
            let preflight = preflight_listing_avionics_page(
                &db,
                &AvionicsVerificationScope::new(1, Some(listing_id), None),
            )
            .await
            .unwrap();
            let preflight_listing = &preflight.listings[0];
            assert_eq!(preflight_listing.generic_invalid_identity_components, 1);
            assert_eq!(preflight_listing.retained_identity_components, 1);
            assert_eq!(preflight_listing.verified_local_identity_components, 1);
            assert_eq!(
                preflight
                    .provider_request_plan
                    .known_total_provider_requests_validation_envelope_maximum,
                0
            );

            let applied = verify_listing_avionics_page(
                &db,
                Some(&extractor),
                AvionicsVerificationExecutionMode::Apply,
                &AvionicsVerificationScope::new(1, Some(listing_id), None),
            )
            .await
            .unwrap();

            let listing = &applied.listings[0];
            assert_eq!(listing.status, "applied");
            assert_eq!(listing.safely_discarded, 1);
            assert_eq!(listing.remaining_review_aspects, 0);
            assert_eq!(listing.candidates.len(), 2);
            assert_eq!(listing.candidates[0].role, "primary");
            assert_eq!(listing.candidates[0].status, "rejected");
            assert_eq!(listing.candidates[1].role, "replacement");
            assert_eq!(listing.candidates[1].status, "existing");
            let dispositions: Vec<(String, String, Option<i64>)> = sqlx::query_as(
                r#"
            SELECT occurrence_role, outcome, avionics_model_id
            FROM aircraft_sale_listing_avionics_dispositions
            WHERE aircraft_sale_listing_id = ?
            ORDER BY occurrence_role
            "#,
            )
            .bind(listing_id)
            .fetch_all(sqlite_pool(&db))
            .await
            .unwrap();
            assert_eq!(
                dispositions,
                vec![
                    ("primary".to_string(), "discarded".to_string(), None),
                    (
                        "replacement".to_string(),
                        "linked".to_string(),
                        Some(replacement_id),
                    ),
                ]
            );
            let usage: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
            )
            .bind(listing_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
            assert_eq!(usage, 0);
        }
    }

    #[tokio::test]
    async fn concrete_primary_keeps_local_link_when_replacement_is_generic() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let primary_id = seed_approved_named_product_for_manufacturer(
            &db,
            true,
            "Garmin",
            "https://www.garmin.com",
            "GNS 430W",
            "GNS-430W",
            "GPS",
        )
        .await;
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        for action in ["replaces", "removes"] {
            let listing_id = seed_concrete_primary_with_generic_replacement_listing(
                &db,
                &format!("concrete-primary-generic-replacement-{action}"),
                action,
            )
            .await;
            let preflight = preflight_listing_avionics_page(
                &db,
                &AvionicsVerificationScope::new(1, Some(listing_id), None),
            )
            .await
            .unwrap();
            let preflight_listing = &preflight.listings[0];
            assert_eq!(preflight_listing.generic_invalid_identity_components, 1);
            assert_eq!(preflight_listing.retained_identity_components, 1);
            assert_eq!(preflight_listing.verified_local_identity_components, 1);
            assert_eq!(
                preflight
                    .provider_request_plan
                    .known_total_provider_requests_validation_envelope_maximum,
                0
            );
            let applied = verify_listing_avionics_page(
                &db,
                Some(&extractor),
                AvionicsVerificationExecutionMode::Apply,
                &AvionicsVerificationScope::new(1, Some(listing_id), None),
            )
            .await
            .unwrap();

            let listing = &applied.listings[0];
            assert_eq!(listing.status, "applied");
            assert_eq!(listing.accepted, 1);
            assert_eq!(listing.safely_discarded, 1);
            assert_eq!(listing.remaining_review_aspects, 0);
            assert_eq!(listing.candidates.len(), 2);
            assert_eq!(listing.candidates[0].role, "primary");
            assert_eq!(listing.candidates[0].status, "existing");
            assert_eq!(listing.candidates[1].role, "replacement");
            assert_eq!(listing.candidates[1].status, "rejected");
            let link: (i64, String, Option<i64>) = sqlx::query_as(
                r#"
            SELECT avionics_model_id, configuration_action,
                   replaces_avionics_model_id
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
            )
            .bind(listing_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
            assert_eq!(
                link,
                (
                    primary_id,
                    if action == "removes" {
                        "removes".to_string()
                    } else {
                        "installed".to_string()
                    },
                    (action == "removes").then_some(primary_id),
                )
            );
            let replacement_disposition: (String, Option<i64>) = sqlx::query_as(
                r#"
            SELECT outcome, avionics_model_id
            FROM aircraft_sale_listing_avionics_dispositions
            WHERE aircraft_sale_listing_id = ?
              AND occurrence_index = 0
              AND occurrence_role = 'replacement'
            "#,
            )
            .bind(listing_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
            assert_eq!(replacement_disposition, ("discarded".to_string(), None));
            let usage: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
            )
            .bind(listing_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
            assert_eq!(usage, 0);
        }
    }

    #[tokio::test]
    async fn generic_relationship_routes_only_the_concrete_nonlocal_role_unconditionally() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let (endpoint, request_count, requests, server) =
            spawn_classifier_endpoint("very_high").await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let mut expected_requests = 0;

        for action in ["replaces", "removes"] {
            for generic_role in [OccurrenceRole::Primary, OccurrenceRole::Replacement] {
                let suffix = format!("nonlocal-{}-generic-{}", generic_role.as_str(), action);
                let listing_id = match generic_role {
                    OccurrenceRole::Primary => {
                        seed_generic_primary_with_concrete_replacement_listing(
                            &db, &suffix, action, "GNS 530W",
                        )
                        .await
                    }
                    OccurrenceRole::Replacement => {
                        seed_concrete_primary_with_generic_replacement_listing(&db, &suffix, action)
                            .await
                    }
                };

                let preflight = preflight_listing_avionics_page(
                    &db,
                    &AvionicsVerificationScope::new(1, Some(listing_id), None),
                )
                .await
                .unwrap();
                let listing = &preflight.listings[0];
                assert_eq!(listing.status, "ready_retained_observations");
                assert_eq!(listing.generic_invalid_identity_components, 1);
                assert_eq!(listing.invalid_retained_observations, 1);
                assert_eq!(listing.retained_identity_components, 1);
                assert_eq!(listing.verified_local_identity_components, 0);
                assert_eq!(listing.grounded_initial_identity_components, 1);
                assert_eq!(listing.grounded_conditional_relationship_components, 0);
                assert_eq!(
                    preflight
                        .provider_request_plan
                        .initial_grounded_provider_requests_baseline,
                    4
                );
                assert_eq!(
                    preflight
                        .provider_request_plan
                        .known_total_provider_requests_minimum_baseline,
                    4
                );
                assert_eq!(
                    preflight
                        .provider_request_plan
                        .known_total_provider_requests_all_positive_baseline,
                    7
                );

                let preview = verify_listing_avionics_page(
                    &db,
                    Some(&extractor),
                    AvionicsVerificationExecutionMode::Preview,
                    &AvionicsVerificationScope::new(1, Some(listing_id), None),
                )
                .await
                .unwrap();
                expected_requests += 1;
                assert_eq!(request_count.load(Ordering::SeqCst), expected_requests);
                let expected_reported_attempts =
                    usize::from(generic_role == OccurrenceRole::Primary) + 1;
                assert_eq!(
                    preview.summary.identity_resolution_attempts,
                    expected_reported_attempts
                );
                assert_eq!(preview.summary.rejected, expected_reported_attempts);
            }
        }
        server.abort();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), expected_requests);
        assert!(requests
            .iter()
            .all(|request| request.get("tools").is_none()));
    }

    #[tokio::test]
    async fn generic_relationship_paid_readiness_scope_contains_only_the_nonlocal_concrete_role() {
        for action in ["replaces", "removes"] {
            for generic_role in [OccurrenceRole::Primary, OccurrenceRole::Replacement] {
                let db = AppDb::connect("sqlite::memory:").await.unwrap();
                let (listing_id, blocker_id) = match generic_role {
                    OccurrenceRole::Primary => {
                        seed_manufacturer_identity_only(
                            &db,
                            "BendixKing",
                            "https://www.bendixking.com",
                        )
                        .await;
                        let blocker_id = seed_approved_named_product_for_manufacturer(
                            &db,
                            false,
                            "Garmin",
                            "https://www.garmin.com",
                            "GNS 430W",
                            "GNS-430W",
                            "GPS",
                        )
                        .await;
                        let listing_id = seed_relationship_listing(
                            &db,
                            &format!("scope-generic-primary-{action}"),
                            action,
                            "Garmin",
                            "GPS",
                            "BendixKing",
                            "KX 999",
                        )
                        .await;
                        (listing_id, blocker_id)
                    }
                    OccurrenceRole::Replacement => {
                        seed_manufacturer_identity_only(&db, "Garmin", "https://www.garmin.com")
                            .await;
                        let blocker_id = seed_approved_named_product_for_manufacturer(
                            &db,
                            false,
                            "BendixKing",
                            "https://www.bendixking.com",
                            "KX 155",
                            "KX-155",
                            "GPS",
                        )
                        .await;
                        let listing_id = seed_relationship_listing(
                            &db,
                            &format!("scope-generic-replacement-{action}"),
                            action,
                            "Garmin",
                            "GNS 999",
                            "BendixKing",
                            "GPS",
                        )
                        .await;
                        (listing_id, blocker_id)
                    }
                };
                sqlx::query(
                    r#"
                    INSERT INTO aircraft_sale_listing_avionics (
                      aircraft_sale_listing_id, avionics_model_id, quantity,
                      source, source_notes, source_confidence,
                      configuration_action
                    ) VALUES (?, ?, 1, 'listing', 'unrelated preserved blocker',
                              'high', 'installed')
                    "#,
                )
                .bind(listing_id)
                .bind(blocker_id)
                .execute(sqlite_pool(&db))
                .await
                .unwrap();

                let preflight = preflight_listing_avionics(
                    &db,
                    listing_id,
                    AvionicsVerificationExecutionMode::Apply,
                )
                .await
                .unwrap();
                let ListingAvionicsVerificationPreflight::PendingReview { report } = preflight
                else {
                    panic!("the listing should remain in review")
                };
                assert_eq!(report.status, "blocked");
                assert_eq!(report.generic_invalid_identity_components, 1);
                assert_eq!(report.grounded_initial_identity_components, 0);
                assert_eq!(report.grounded_conditional_relationship_components, 0);
                assert!(report.note.contains(&format!(
                    "preserved avionics catalog id {blocker_id} has neither current manufacturer-reuse nor exact same-case authorization"
                )));
            }
        }
    }

    #[test]
    fn legacy_scalar_type_requires_reextraction_without_mechanical_conversion() {
        let legacy = current_listing_extraction_with_avionics(json!([
            {"manufacturer":"Garmin","model":"GNX 375","type":"GPS","quantity":1}
        ]));

        let source = retained_avionics_source(
            Some(&legacy),
            Some("https://example.test/listing/legacy"),
            Some("Garmin GNX 375"),
        );

        let RetainedAvionicsSource::RequiresReextraction { reason } = source else {
            panic!("a scalar legacy type must never be replayed or converted locally")
        };
        assert!(reason.contains("scalar type payloads are intentionally unsupported"));
    }

    #[test]
    fn durable_avionics_repair_preserves_every_current_prior_non_avionics_value() {
        let avionics = vec![ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
            model: "G5".to_string(),
            avionics_types: vec!["Flight Display".to_string()],
            quantity: 2,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some("Dual Garmin G5 displays".to_string()),
            source_confidence: Some("high".to_string()),
        }];
        let mut prior = retained_legacy_listing_extraction();
        prior["avionics"] = serde_json::to_value(&avionics).unwrap();
        let mut fresh = prior.clone();
        fresh["manufacturer"] = json!("Fresh extraction maker");
        fresh["airframe_hours"] = json!(111.0);

        let merged = merge_validated_avionics_into_listing_extraction(
            Some(&prior.to_string()),
            fresh.clone(),
            &avionics,
        )
        .unwrap();
        let merged: Value = serde_json::from_str(&merged).unwrap();
        let canonical_prior =
            canonical_complete_current_listing_value(&prior, "test prior extraction").unwrap();
        for (field, value) in canonical_prior.as_object().unwrap() {
            if field != "avionics" {
                assert_eq!(&merged[field], value, "field {field} changed");
            }
        }
        assert_eq!(
            serde_json::from_value::<Vec<ParsedAvionics>>(merged["avionics"].clone()).unwrap(),
            avionics
        );

        let repaired_from_missing =
            merge_validated_avionics_into_listing_extraction(None, fresh.clone(), &avionics)
                .unwrap();
        let repaired_from_missing: Value = serde_json::from_str(&repaired_from_missing).unwrap();
        let canonical_fresh =
            canonical_complete_current_listing_value(&fresh, "test fresh extraction").unwrap();
        assert_eq!(repaired_from_missing, canonical_fresh);

        let repaired_from_malformed = merge_validated_avionics_into_listing_extraction(
            Some("not json"),
            fresh.clone(),
            &avionics,
        )
        .unwrap();
        let repaired_from_malformed: Value =
            serde_json::from_str(&repaired_from_malformed).unwrap();
        assert_eq!(repaired_from_malformed, canonical_fresh);

        let mut unsupported_prior = prior;
        unsupported_prior["candidates"] = json!([{"content": "provider envelope"}]);
        let repaired_from_unsupported_prior = merge_validated_avionics_into_listing_extraction(
            Some(&unsupported_prior.to_string()),
            fresh.clone(),
            &avionics,
        )
        .unwrap();
        let repaired_from_unsupported_prior: Value =
            serde_json::from_str(&repaired_from_unsupported_prior).unwrap();
        assert_eq!(repaired_from_unsupported_prior, canonical_fresh);
        assert!(repaired_from_unsupported_prior.get("candidates").is_none());

        let mut unsupported_fresh = fresh;
        unsupported_fresh["grounding"] = json!({"dossier": "must not persist"});
        let error =
            merge_validated_avionics_into_listing_extraction(None, unsupported_fresh, &avionics)
                .unwrap_err();
        assert!(error.contains("unsupported field"), "{error}");
    }

    #[test]
    fn durable_occurrence_validation_rejects_missing_defaults_and_relationship_repair() {
        let missing_quantity = json!({"avionics": [{
            "manufacturer": "Garmin", "model": "G5", "types": ["Flight Display"],
            "configuration_action": "installed", "replaces": null
        }]});
        assert!(parse_current_avionics_extraction_value(&missing_quantity)
            .unwrap_err()
            .contains("quantity must be an explicit integer"));

        let missing_action = json!({"avionics": [{
            "manufacturer": "Garmin", "model": "G5", "types": ["Flight Display"],
            "quantity": 1, "replaces": null
        }]});
        assert!(parse_current_avionics_extraction_value(&missing_action)
            .unwrap_err()
            .contains("configuration_action must be an explicit"));

        let missing_replacement = json!({"avionics": [{
            "manufacturer": "Garmin", "model": "G5", "types": ["Flight Display"],
            "quantity": 1, "configuration_action": "replaces", "replaces": null
        }]});
        assert!(
            parse_current_avionics_extraction_value(&missing_replacement)
                .unwrap_err()
                .contains("requires one replacement object")
        );

        let unexpected_replacement = json!({"avionics": [{
            "manufacturer": "Garmin", "model": "G5", "types": ["Flight Display"],
            "quantity": 1, "configuration_action": "installed",
            "replaces": {"manufacturer": "Garmin", "model": "G3X", "types": ["Flight Display"]}
        }]});
        assert!(
            parse_current_avionics_extraction_value(&unexpected_replacement)
                .unwrap_err()
                .contains("must use replaces=null")
        );
    }

    #[tokio::test]
    async fn durable_reextraction_rejects_invalid_occurrence_after_one_bounded_correction() {
        let evidence = "Garmin G5 installed";
        let (endpoint, request_count, server) = spawn_listing_extraction_endpoint(json!([{
            "manufacturer": "Garmin",
            "model": "G5",
            "types": ["Flight Display"],
            "quantity": 0,
            "configuration_action": "installed",
            "replaces": {
                "manufacturer": "Garmin",
                "model": "G3X",
                "types": ["Flight Display"]
            },
            "source_evidence_text": evidence,
            "source_confidence": "high"
        }]))
        .await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let prior = retained_legacy_listing_extraction().to_string();

        let error = reextract_avionics(
            &extractor,
            evidence,
            &ListingEvidenceContext::from_cleaned_text(evidence),
            "https://example.test/listing/1",
            evidence,
            Some(&prior),
        )
        .await
        .unwrap_err();
        server.abort();

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert!(error.contains("quantity must be at least 1"));
    }

    #[tokio::test]
    async fn durable_reextraction_checkpoints_ambiguous_quantity_below_high_confidence() {
        let html = trusted_controller_role_html(
            "KSAR Garmin G5 attitude, Garmin G5 HSI, Garmin GFC500 auto pilot",
        );
        let primary = json!([{
            "manufacturer": "Garmin",
            "model": "G5",
            "types": ["Flight Display", "AHRS"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": "Garmin G5 attitude",
            "source_confidence": "high"
        }]);
        let corrected = json!([{
            "manufacturer": "Garmin",
            "model": "G5",
            "types": ["Flight Display", "AHRS"],
            "quantity": 2,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": "Garmin G5 attitude, Garmin G5 HSI",
            "source_confidence": "medium"
        }]);
        let (endpoint, request_count, server) =
            spawn_listing_extraction_sequence_endpoint(vec![primary, corrected]).await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let prior = retained_legacy_listing_extraction().to_string();

        let reextracted = reextract_avionics(
            &extractor,
            &clean_listing_html(&html),
            &ListingEvidenceContext::from_listing_capture(
                Some(CONTROLLER_ROLE_LISTING_URL),
                Some(&html),
            ),
            CONTROLLER_ROLE_LISTING_URL,
            &html,
            Some(&prior),
        )
        .await
        .unwrap();
        server.abort();

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert_eq!(reextracted.avionics.len(), 1);
        assert_eq!(reextracted.avionics[0].quantity, 2);
        assert_eq!(
            reextracted.avionics[0].source_confidence.as_deref(),
            Some("medium")
        );
        assert_eq!(
            reextracted.avionics[0].source_evidence_text.as_deref(),
            Some("Garmin G5 attitude, Garmin G5 HSI")
        );
        let merged: Value = serde_json::from_str(&reextracted.extracted_listing_json).unwrap();
        assert_eq!(merged["avionics"][0]["quantity"], 2);
    }

    #[tokio::test]
    async fn durable_reextraction_uses_one_avionics_only_correction() {
        let html = trusted_controller_role_html("GARMIN G1000 NXI");
        let primary = json!([{
            "manufacturer": "Garmin",
            "model": "G1000 NXi",
            "types": ["Integrated Flight Deck", "Autopilot"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": "GARMIN G1000 NXI unrelated text",
            "source_confidence": "high"
        }]);
        let corrected = json!([{
            "manufacturer": "Garmin",
            "model": "G1000 NXi",
            "types": ["Integrated Flight Deck"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": "GARMIN G1000 NXI",
            "source_confidence": "high"
        }]);
        let (endpoint, request_count, server) =
            spawn_listing_extraction_sequence_endpoint(vec![primary, corrected.clone()]).await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let mut prior = retained_legacy_listing_extraction();
        prior["avionics"] = corrected;
        let prior = prior.to_string();

        let reextracted = reextract_avionics(
            &extractor,
            &clean_listing_html(&html),
            &ListingEvidenceContext::from_listing_capture(
                Some(CONTROLLER_ROLE_LISTING_URL),
                Some(&html),
            ),
            CONTROLLER_ROLE_LISTING_URL,
            &html,
            Some(&prior),
        )
        .await
        .unwrap();
        server.abort();

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert_eq!(reextracted.avionics.len(), 1);
        assert_eq!(reextracted.avionics[0].model, "G1000 NXi");
        assert_eq!(
            reextracted.avionics[0].avionics_types,
            vec!["Integrated Flight Deck"]
        );
        let merged: Value = serde_json::from_str(&reextracted.extracted_listing_json).unwrap();
        let prior: Value = serde_json::from_str(&prior).unwrap();
        let prior =
            canonical_complete_current_listing_value(&prior, "test prior extraction").unwrap();
        for field in [
            "manufacturer",
            "model",
            "variant",
            "model_year",
            "asking_price_usd",
            "airframe_hours",
            "valuation_facts",
        ] {
            assert_eq!(merged[field], prior[field], "{field} changed");
        }
    }

    #[tokio::test]
    async fn durable_reextraction_rejects_an_unresolved_repeated_identity_after_one_correction() {
        let html = trusted_controller_role_html(
            "Garmin G5 attitude, Garmin G5 HSI, Garmin GTX 345 transponder, Garmin G5 standby display",
        );
        let (endpoint, request_count, server) = spawn_listing_extraction_endpoint(json!([{
            "manufacturer": "Garmin",
            "model": "G5",
            "types": ["Flight Display", "AHRS"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": "Garmin G5 attitude",
            "source_confidence": "high"
        }]))
        .await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let prior = retained_legacy_listing_extraction().to_string();

        let error = reextract_avionics(
            &extractor,
            &clean_listing_html(&html),
            &ListingEvidenceContext::from_listing_capture(
                Some(CONTROLLER_ROLE_LISTING_URL),
                Some(&html),
            ),
            CONTROLLER_ROLE_LISTING_URL,
            &html,
            Some(&prior),
        )
        .await
        .unwrap_err();
        server.abort();

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert!(error.contains("ambiguous Controller quantity evidence"));
    }

    #[test]
    fn current_multi_capability_payload_is_replayed_without_reextraction() {
        let current = current_listing_extraction_with_avionics(json!([
            {
              "manufacturer":"Garmin",
              "model":"GNX 375",
              "types":["GPS","Transponder"],
              "quantity":1,
              "configuration_action":"installed",
              "replaces":null,
              "source_evidence_text":"Garmin GNX 375",
              "source_confidence":"high"
            }
        ]));

        let source = retained_avionics_source(
            Some(&current),
            Some("https://example.test/listing/current"),
            Some("<p>Garmin GNX 375</p>"),
        );

        let RetainedAvionicsSource::Current(avionics) = source else {
            panic!("a current capability-array payload should be replayable")
        };
        assert_eq!(avionics.len(), 1);
        assert_eq!(avionics[0].avionics_types, vec!["GPS", "Transponder"]);
    }

    #[test]
    fn retained_extraction_rejects_exact_evidence_with_inflated_suite_types() {
        let current = current_listing_extraction_with_avionics(json!([{
          "manufacturer":"Garmin",
          "model":"G1000 NXi",
          "types":["Integrated Flight Deck","Autopilot"],
          "quantity":1,
          "configuration_action":"installed",
          "replaces":null,
          "source_evidence_text":"Garmin G1000 NXi",
          "source_confidence":"high"
        }]));

        let source = retained_avionics_source(
            Some(&current),
            Some("https://example.test/listing/inflated-suite"),
            Some("<p>Garmin G1000 NXi</p>"),
        );

        let RetainedAvionicsSource::RequiresReextraction { reason } = source else {
            panic!("inflated retained suite capabilities must require re-extraction")
        };
        assert!(reason.contains("integrated suite"), "{reason}");
    }

    #[test]
    fn capability_array_without_exact_evidence_requires_reextraction() {
        let missing_evidence = current_listing_extraction_with_avionics(json!([{
          "manufacturer":"Garmin",
          "model":"GNX 375",
          "types":["GPS","Transponder"],
          "quantity":1,
          "configuration_action":"installed",
          "replaces":null
        }]));

        let source = retained_avionics_source(
            Some(&missing_evidence),
            Some("https://example.test/listing/missing-evidence"),
            Some("Garmin GNX 375"),
        );

        let RetainedAvionicsSource::RequiresReextraction { reason } = source else {
            panic!("an evidence-less extraction must never be replayed")
        };
        assert!(reason.contains("source_evidence_text"));
    }

    #[test]
    fn missing_or_invalid_capability_arrays_fail_closed_to_reextraction() {
        assert!(matches!(
            retained_avionics_source(None, None, None),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert!(matches!(
            retained_avionics_source(Some(r#"{"avionics":[]}"#), None, None,),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert!(matches!(
            retained_avionics_source(
                Some(r#"{"avionics":[{"manufacturer":"Garmin","model":"GTN 750Xi","types":[]}]}"#),
                None,
                None,
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
                None,
                None,
            ),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
    }

    #[test]
    fn fabricated_evidence_requires_reextraction() {
        let fabricated = current_listing_extraction_with_avionics(json!([{
          "manufacturer":"Garmin",
          "model":"GNX 375",
          "types":["GPS","Transponder"],
          "quantity":1,
          "configuration_action":"installed",
          "replaces":null,
          "source_evidence_text":"Garmin GNX 375 with invented qualifier",
          "source_confidence":"high"
        }]));

        let source = retained_avionics_source(
            Some(&fabricated),
            Some("https://example.test/listing/fabricated"),
            Some("Garmin GNX 375 installed"),
        );

        let RetainedAvionicsSource::RequiresReextraction { reason } = source else {
            panic!("model-produced evidence absent from the source must never be replayed")
        };
        assert!(reason.contains("not one exact structurally visible source unit"));
    }

    #[test]
    fn retained_extraction_rejects_hidden_and_metadata_only_evidence() {
        for (html, evidence) in [
            (
                "<html><body><p hidden>Garmin G1000 avionics system</p></body></html>",
                "Garmin G1000 avionics system",
            ),
            (
                "<html><head><meta content=\"Garmin G1000 metadata\"></head><body><p>Aircraft listing</p></body></html>",
                "Garmin G1000 metadata",
            ),
        ] {
            let payload = current_listing_extraction_with_avionics(json!([{
                "manufacturer": "Garmin",
                "model": "G1000",
                "types": ["Flight Display"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": evidence,
                "source_confidence": "high"
            }]));
            let RetainedAvionicsSource::RequiresReextraction { reason } =
                retained_avionics_source(
                    Some(&payload.to_string()),
                    Some("https://example.test/listing/hidden"),
                    Some(html),
                )
            else {
                panic!("non-visible evidence must never be reused from a retained extraction")
            };
            assert!(reason.contains("structurally visible"));
        }
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
    fn duplicate_product_rows_fail_closed_without_mutating_the_first_occurrence() {
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

        let error =
            reconcile_preserved_link_or_reject_duplicate(&mut existing, &incoming).unwrap_err();

        assert!(error.contains("multiple retained avionics occurrences"));
        assert!(error.contains("explicit physical-unit quantity"));
        assert_eq!(existing.quantity, 1);
        assert_eq!(existing.source_notes.as_deref(), Some("GPS navigator"));
        assert_eq!(existing.source_confidence.as_deref(), Some("high"));
    }

    fn preserved_guard(
        listing_link_id: i64,
        expected_observation_sha256: &str,
    ) -> AutomatedPreservedAssociationGuard {
        AutomatedPreservedAssociationGuard {
            listing_link_id,
            association_role: ListingAssociationRole::Installed,
            expected_observation_sha256: expected_observation_sha256.to_string(),
        }
    }

    #[test]
    fn unbound_fresh_duplicate_cannot_consume_a_preserved_occurrence() {
        let guard = preserved_guard(416, &"a".repeat(64));
        let mut preserved = PreparedLink {
            identity_key: "catalog:94".to_string(),
            avionics_model_id: 94,
            authorization: Some(AutomatedAssociationAuthorization::ManufacturerReuse),
            expected_collision_closure_sha256: Some("b".repeat(64)),
            quantity: 1,
            source_notes: Some("Garmin Flight Stream 210".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: Some(guard.clone()),
        };
        let fresh = PreparedLink {
            source_notes: Some("Garmin FlightStream 210".to_string()),
            preserved_association_guard: None,
            ..preserved.clone()
        };

        assert!(reconcile_preserved_link_or_reject_duplicate(&mut preserved, &fresh).is_err());

        assert_eq!(preserved.preserved_association_guard, Some(guard));
        assert_eq!(preserved.quantity, 1);
        assert_eq!(
            preserved.source_notes.as_deref(),
            Some("Garmin Flight Stream 210")
        );

        let mut evidence_missing = PreparedLink {
            source_notes: None,
            preserved_association_guard: Some(preserved_guard(416, &"a".repeat(64))),
            ..preserved.clone()
        };
        let unbound_evidence_missing = PreparedLink {
            preserved_association_guard: None,
            ..evidence_missing.clone()
        };
        assert!(reconcile_preserved_link_or_reject_duplicate(
            &mut evidence_missing,
            &unbound_evidence_missing
        )
        .is_err());
    }

    #[test]
    fn exact_checkpoint_occurrence_reconciles_with_its_preserved_guard() {
        let guard = preserved_guard(416, &"a".repeat(64));
        let mut preserved = PreparedLink {
            identity_key: "catalog:94".to_string(),
            avionics_model_id: 94,
            authorization: Some(AutomatedAssociationAuthorization::ManufacturerReuse),
            expected_collision_closure_sha256: Some("b".repeat(64)),
            quantity: 1,
            source_notes: Some("Garmin Flight Stream 210".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: Some(guard.clone()),
        };
        let exact_checkpoint_occurrence = PreparedLink {
            preserved_association_guard: None,
            ..preserved.clone()
        };

        reconcile_preserved_link_or_reject_duplicate(&mut preserved, &exact_checkpoint_occurrence)
            .unwrap();

        assert_eq!(preserved.preserved_association_guard, Some(guard));
        assert_eq!(preserved.quantity, 1);
        assert_eq!(
            preserved.source_notes.as_deref(),
            Some("Garmin Flight Stream 210")
        );
    }

    #[test]
    fn preserved_occurrence_guard_cannot_be_adopted_by_identity_match() {
        let guard = preserved_guard(416, &"a".repeat(64));
        let mut fresh = PreparedLink {
            identity_key: "catalog:94".to_string(),
            avionics_model_id: 94,
            authorization: Some(AutomatedAssociationAuthorization::ManufacturerReuse),
            expected_collision_closure_sha256: Some("b".repeat(64)),
            quantity: 1,
            source_notes: Some("Garmin FlightStream 210".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: None,
        };
        let preserved = PreparedLink {
            source_notes: Some("Garmin Flight Stream 210".to_string()),
            preserved_association_guard: Some(guard.clone()),
            ..fresh.clone()
        };

        assert!(reconcile_preserved_link_or_reject_duplicate(&mut fresh, &preserved).is_err());

        assert_eq!(fresh.preserved_association_guard, None);
        assert_eq!(fresh.quantity, 1);
        assert_eq!(
            fresh.source_notes.as_deref(),
            Some("Garmin FlightStream 210")
        );
    }

    #[test]
    fn duplicate_guard_authorization_and_collision_mismatches_fail_closed() {
        let original_guard = preserved_guard(416, &"a".repeat(64));
        let original = PreparedLink {
            identity_key: "catalog:94".to_string(),
            avionics_model_id: 94,
            authorization: Some(AutomatedAssociationAuthorization::ManufacturerReuse),
            expected_collision_closure_sha256: Some("b".repeat(64)),
            quantity: 1,
            source_notes: Some("Garmin Flight Stream 210".to_string()),
            source_confidence: Some("high".to_string()),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            replacement_authorization: None,
            replacement_identity_key: None,
            expected_replacement_collision_closure_sha256: None,
            preserved_association_guard: Some(original_guard.clone()),
        };
        let mismatches = [
            PreparedLink {
                preserved_association_guard: Some(preserved_guard(417, &"a".repeat(64))),
                ..original.clone()
            },
            PreparedLink {
                authorization: None,
                preserved_association_guard: None,
                ..original.clone()
            },
            PreparedLink {
                expected_collision_closure_sha256: Some("c".repeat(64)),
                preserved_association_guard: None,
                ..original.clone()
            },
        ];

        for mismatch in mismatches {
            let mut existing = original.clone();
            assert!(
                reconcile_preserved_link_or_reject_duplicate(&mut existing, &mismatch).is_err()
            );
            assert_eq!(
                existing.preserved_association_guard,
                Some(original_guard.clone())
            );
            assert_eq!(existing.authorization, original.authorization);
            assert_eq!(
                existing.expected_collision_closure_sha256,
                original.expected_collision_closure_sha256
            );
        }
    }

    #[test]
    fn gnx_375_gps_and_transponder_rows_require_one_explicit_product_occurrence() {
        let mut existing = PreparedLink {
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
        };
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

        let error = reconcile_preserved_link_or_reject_duplicate(&mut existing, &transponder_row)
            .unwrap_err();

        assert!(error.contains("multiple retained avionics occurrences"));
        assert_eq!(existing.quantity, 1);
        assert_eq!(
            existing.source_notes.as_deref(),
            Some("GNX 375 GPS navigator installed")
        );
    }

    #[test]
    fn dry_run_duplicate_preview_product_key_fails_closed() {
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
            verified_local_reuse_proof: None,
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
        assert!(!reconcile_or_push_prepared_link(
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
        let error = reconcile_or_push_prepared_link(
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
        .unwrap_err();
        assert!(error.contains("multiple retained avionics occurrences"));
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

        assert!(reconcile_preserved_link_or_reject_duplicate(&mut existing, &conflicting).is_err());
        assert_eq!(existing.configuration_action, "installed");
        assert_eq!(existing.replaces_avionics_model_id, None);

        let mut unresolved_replacement = PreparedLink {
            configuration_action: "replaces".to_string(),
            replaces_avionics_model_id: Some(0),
            ..existing.clone()
        };
        let same_unresolved_replacement = unresolved_replacement.clone();
        assert!(reconcile_preserved_link_or_reject_duplicate(
            &mut unresolved_replacement,
            &same_unresolved_replacement,
        )
        .is_err());
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
                r#"{"avionics":[{"manufacturer":"Garmin","model":"Attached","types":["Transponder"],"quantity":1,"configuration_action":"installed","replaces":null,"source_evidence_text":"Attached GTX 345R installed","source_confidence":"high"}]}"#,
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
        let raw = parse_current_avionics_extraction_json(
            rows[0].extracted_listing_json.as_deref().unwrap(),
        )
        .unwrap();
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
                rows[0].submission_source_url.as_deref(),
                rows[0].rendered_html.as_deref(),
            ),
            RetainedAvionicsSource::RequiresReextraction { .. }
        ));
        assert_eq!(
            validate_pending_source_binding(&rows[0]).unwrap().1,
            "canonical_listing_id"
        );
    }

    #[tokio::test]
    async fn current_review_observations_require_and_can_replace_a_stale_plugin_extraction() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_named_product_for_manufacturer(
            &db,
            true,
            "Garmin",
            "https://www.garmin.com",
            "Fixture",
            "FIXTURE-TEST",
            "GPS",
        )
        .await;
        let listing_id = seed_listing(&db, "https://example.test/listing/53").await;
        seed_faa_admission(&db, listing_id).await;
        let stale_extraction = retained_legacy_listing_extraction().to_string();
        seed_submission_and_review(
            &db,
            listing_id,
            "https://example.test/listing/53",
            "<p>Garmin fixture installed</p>",
            Some(&stale_extraction),
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
            retained_avionics_source(
                row.extracted_listing_json.as_deref(),
                row.submission_source_url.as_deref(),
                row.rendered_html.as_deref(),
            ),
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

        let RetainedObservationSource::RequiresReextraction {
            reason,
            preserved_aspects,
        } = retained_observation_source(&db, &row, &listing_context)
            .await
            .unwrap()
        else {
            panic!("a stale attached extraction must not select review observations")
        };
        assert!(reason.contains("not an exact current checkpoint"));
        assert!(preserved_aspects.is_empty());

        let (endpoint, request_count, server) = spawn_listing_extraction_endpoint(json!([{
            "manufacturer": "Garmin",
            "model": "Fixture",
            "types": ["GPS"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": "Garmin fixture installed",
            "source_confidence": "high"
        }]))
        .await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let result = verify_listing_avionics(
            &db,
            Some(&extractor),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("a validated re-extraction should replace the stale checkpoint and advance");
        server.abort();
        let ListingAvionicsVerification::Processed { report } = result else {
            panic!("the listing should be processed")
        };
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert_eq!(report.status, "applied", "{report:#?}");
        assert_eq!(report.raw_avionics_source, "gemini_reextraction");
        assert!(report.reextraction_required);
        assert!(report.reextraction_attempted);
        assert!(report.reextraction_succeeded);
        assert_eq!(report.accepted, 1);

        let stored_product_id: i64 = sqlx::query_scalar(
            "SELECT avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(stored_product_id, product_id);
    }

    #[tokio::test]
    async fn provider_required_reextraction_without_a_client_fails_closed() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        seed_approved_named_product_for_manufacturer(
            &db,
            true,
            "Garmin",
            "https://www.garmin.com",
            "Fixture",
            "FIXTURE-TEST",
            "GPS",
        )
        .await;
        let source_url = "https://example.test/listing/keyless-reextraction";
        let listing_id = seed_listing(&db, source_url).await;
        seed_faa_admission(&db, listing_id).await;
        let stale_extraction = retained_legacy_listing_extraction().to_string();
        let submission_id = seed_submission_and_review(
            &db,
            listing_id,
            source_url,
            "<p>Garmin fixture installed</p>",
            Some(&stale_extraction),
            Some(listing_id),
            None,
        )
        .await;
        let preflight = preflight_listing_avionics_page(
            &db,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();
        assert!(preflight.provider_request_plan.requires_provider());

        let before: (String, i64, i64, i64, String) = sqlx::query_as(
            r#"
            SELECT submission.extracted_listing_json,
                   (SELECT COUNT(*) FROM avionics_models),
                   (SELECT COUNT(*) FROM aircraft_sale_listing_avionics
                     WHERE aircraft_sale_listing_id = ?),
                   (SELECT COUNT(*) FROM gemini_api_usage
                     WHERE aircraft_sale_listing_id = ?),
                   review.review_payload_sha256
            FROM plugin_submissions submission
            JOIN aircraft_sale_listing_pending_reviews review
              ON review.plugin_submission_id = submission.id
            WHERE submission.id = ?
            "#,
        )
        .bind(listing_id)
        .bind(listing_id)
        .bind(submission_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();

        let result = verify_listing_avionics(
            &db,
            None,
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("missing optional provider capability is a guarded listing outcome");
        let ListingAvionicsVerification::Processed { report } = result else {
            panic!("the pending review should remain present")
        };
        assert_eq!(report.status, "blocked");
        assert!(report.reextraction_required);
        assert!(!report.reextraction_attempted);
        assert!(report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("configured Gemini services")));

        let after: (String, i64, i64, i64, String) = sqlx::query_as(
            r#"
            SELECT submission.extracted_listing_json,
                   (SELECT COUNT(*) FROM avionics_models),
                   (SELECT COUNT(*) FROM aircraft_sale_listing_avionics
                     WHERE aircraft_sale_listing_id = ?),
                   (SELECT COUNT(*) FROM gemini_api_usage
                     WHERE aircraft_sale_listing_id = ?),
                   review.review_payload_sha256
            FROM plugin_submissions submission
            JOIN aircraft_sale_listing_pending_reviews review
              ON review.plugin_submission_id = submission.id
            WHERE submission.id = ?
            "#,
        )
        .bind(listing_id)
        .bind(listing_id)
        .bind(submission_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn missing_malformed_and_non_object_checkpoints_reextract_once_and_apply() {
        for case in ["missing", "malformed", "non-object", "unsupported-prior"] {
            let db = AppDb::connect("sqlite::memory:").await.unwrap();
            let product_id = seed_approved_named_product_for_manufacturer(
                &db,
                true,
                "Garmin",
                "https://www.garmin.com",
                "Fixture",
                "FIXTURE-TEST",
                "GPS",
            )
            .await;
            let source_url = format!("https://example.test/listing/repair-{case}");
            let listing_id = seed_listing(&db, &source_url).await;
            seed_faa_admission(&db, listing_id).await;
            let retained_extraction = match case {
                "missing" => None,
                "malformed" => Some("{not-json".to_string()),
                "non-object" => Some("[]".to_string()),
                "unsupported-prior" => {
                    let mut checkpoint = retained_legacy_listing_extraction();
                    checkpoint["avionics"] = json!([{
                        "manufacturer": "Garmin",
                        "model": "Fixture",
                        "types": ["GPS"],
                        "quantity": 1,
                        "configuration_action": "installed",
                        "replaces": null,
                        "source_evidence_text": "Garmin fixture installed",
                        "source_confidence": "high"
                    }]);
                    checkpoint["candidates"] =
                        json!([{"content": "provider envelope must not persist"}]);
                    Some(checkpoint.to_string())
                }
                _ => unreachable!(),
            };
            let submission_id = seed_submission_and_review(
                &db,
                listing_id,
                &source_url,
                "<p>Garmin fixture installed</p>",
                retained_extraction.as_deref(),
                Some(listing_id),
                None,
            )
            .await;

            let (endpoint, request_count, server) = spawn_listing_extraction_endpoint(json!([{
                "manufacturer": "Garmin",
                "model": "Fixture",
                "types": ["GPS"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": "Garmin fixture installed",
                "source_confidence": "high"
            }]))
            .await;
            let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
            let result = verify_listing_avionics(
                &db,
                Some(&extractor),
                AvionicsVerificationExecutionMode::Apply,
                listing_id,
            )
            .await
            .unwrap_or_else(|error| panic!("{case} checkpoint repair failed: {error}"));
            server.abort();
            let ListingAvionicsVerification::Processed { report } = result else {
                panic!("{case} checkpoint listing was not processed")
            };

            assert_eq!(
                request_count.load(Ordering::SeqCst),
                1,
                "{case} checkpoint should use one extraction request"
            );
            assert_eq!(report.status, "applied", "{case}: {report:#?}");
            assert_eq!(report.raw_avionics_source, "gemini_reextraction");
            assert!(report.reextraction_succeeded);
            assert_eq!(report.accepted, 1);

            let (stored_json, stored_error): (String, Option<String>) = sqlx::query_as(
                "SELECT extracted_listing_json, extraction_error FROM plugin_submissions WHERE id = ?",
            )
            .bind(submission_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
            let stored_value: Value = serde_json::from_str(&stored_json).unwrap();
            serde_json::from_value::<ParsedListing>(stored_value.clone()).unwrap();
            let stored_avionics = validate_unbound_current_avionics_extraction(
                &stored_json,
                &source_url,
                "<p>Garmin fixture installed</p>",
            )
            .unwrap();
            assert_eq!(stored_avionics.len(), 1);
            assert_eq!(stored_avionics[0].model, "Fixture");
            assert_eq!(stored_error, None);
            assert!(stored_value.get("candidates").is_none());
            assert!(stored_value.get("content").is_none());

            let stored_product_id: i64 = sqlx::query_scalar(
                "SELECT avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
            )
            .bind(listing_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
            assert_eq!(stored_product_id, product_id);
        }
    }

    #[tokio::test]
    async fn retained_review_observations_reject_inflated_suite_types() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let source_url = "https://example.test/listing/review-inflated-suite";
        let listing_id = seed_listing(&db, source_url).await;
        let submission_id = seed_submission_and_review(
            &db,
            listing_id,
            source_url,
            "<p>Garmin G1000 NXi</p>",
            None,
            Some(listing_id),
            Some("legacy extraction unavailable"),
        )
        .await;
        let aspect = PendingReviewAspect::avionics(
            "fixture:review-inflated-suite:0",
            "avionics",
            "Garmin G1000 NXi",
            "Garmin G1000 NXi",
            "exact retained observation",
            1,
            "installed",
            Some("Garmin G1000 NXi".to_string()),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "G1000 NXi",
            vec![
                "Integrated Flight Deck".to_string(),
                "Autopilot".to_string(),
            ],
        ));
        crate::listing::review::stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[aspect],
        )
        .await
        .unwrap();

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
            retained_review_observations(&db, &row, &listing_context)
                .await
                .unwrap(),
            RetainedReviewObservationSource::RequiresFallback { .. }
        ));
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

        let preflight =
            preflight_listing_avionics(&db, listing_id, AvionicsVerificationExecutionMode::Apply)
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

        let applied = verify_listing_avionics(
            &db,
            None,
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
    async fn exact_controller_leading_dual_pending_unit_is_verified_provider_free() {
        const EVIDENCE: &str = "Dual Garmin GIA63W GPS/NAV/WAAS";
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_named_product_for_manufacturer_with_identifier_kind(
            &db,
            true,
            "Garmin",
            "https://www.garmin.com",
            "GIA63W",
            "manufacturer_part_number",
            "011-01105-00",
            "GPS",
        )
        .await;
        let pool = sqlite_pool(&db);
        sqlx::query(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('NAV', 'nav') ON CONFLICT (normalized_name) DO NOTHING",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
            SELECT ?, id FROM avionics_types WHERE normalized_name = 'nav'
            "#,
        )
        .bind(product_id)
        .execute(pool)
        .await
        .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        assert!(refresh_reuse_attestation_sqlite(
            &db,
            &mut transaction,
            product_id,
            "https://www.garmin.com/aviation/product",
        )
        .await
        .unwrap());
        transaction.commit().await.unwrap();

        let listing_id = seed_listing(&db, CONTROLLER_ROLE_LISTING_URL).await;
        seed_faa_admission(&db, listing_id).await;
        let rendered_html = trusted_controller_role_html(EVIDENCE);
        let extracted_listing_json = json!({"avionics": [{
            "manufacturer": "Garmin",
            "model": "GIA63W",
            "types": ["GPS", "NAV"],
            "quantity": 2,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": EVIDENCE,
            "source_confidence": "medium"
        }]})
        .to_string();
        let submission_id = seed_submission_and_review(
            &db,
            listing_id,
            CONTROLLER_ROLE_LISTING_URL,
            &rendered_html,
            Some(&extracted_listing_json),
            Some(listing_id),
            None,
        )
        .await;
        let aspect = PendingReviewAspect::avionics(
            "fixture:leading-dual:0",
            "avionics",
            "Garmin GIA63W",
            EVIDENCE,
            "verified identity needs installation corroboration",
            2,
            "installed",
            Some(EVIDENCE.to_string()),
            Some("medium".to_string()),
        )
        .with_suggested_product(ReviewProduct::verified(
            product_id,
            "Garmin",
            "GIA63W",
            vec!["GPS".to_string(), "NAV".to_string()],
        ));
        crate::listing::review::stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[aspect],
        )
        .await
        .unwrap();

        let preflight =
            preflight_listing_avionics(&db, listing_id, AvionicsVerificationExecutionMode::Apply)
                .await
                .expect("the exact counted unit should preflight locally");
        let ListingAvionicsVerificationPreflight::PendingReview { report } = preflight else {
            panic!("the listing still has one pending review")
        };
        assert_eq!(report.verified_local_identity_components, 1);

        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let applied = verify_listing_avionics(
            &db,
            Some(&extractor),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("the counted unit must not contact the unavailable provider");
        let ListingAvionicsVerification::Processed { report } = applied else {
            panic!("the pending review should be processed")
        };
        assert_eq!(report.accepted, 1);
        assert_eq!(report.remaining_review_aspects, 0);
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
        assert_eq!(link, (product_id, "high".to_string(), 2));
        let authorization: (String, String, i64) = sqlx::query_as(
            r#"
            SELECT authorization.authorization_kind,
                   authorization.association_role,
                   authorization.avionics_model_id
            FROM aircraft_sale_listing_avionics_link_authorizations authorization
            JOIN aircraft_sale_listing_avionics link
              ON link.id = authorization.listing_link_id
            WHERE link.aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            authorization,
            (
                "manufacturer_reuse".to_string(),
                "installed".to_string(),
                product_id,
            )
        );
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
    async fn exact_controller_model_only_dual_display_is_verified_provider_free() {
        const EVIDENCE: &str = "Dual GDU-1040 PFD/MFD";
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_named_product_for_manufacturer_with_identifier_kind(
            &db,
            true,
            "Garmin",
            "https://www.garmin.com",
            "GDU 1040",
            "manufacturer_part_number",
            "011-00972-00",
            "Flight Display",
        )
        .await;
        let pool = sqlite_pool(&db);
        let mut transaction = pool.begin().await.unwrap();
        assert!(refresh_reuse_attestation_sqlite(
            &db,
            &mut transaction,
            product_id,
            "https://www.garmin.com/aviation/product",
        )
        .await
        .unwrap());
        transaction.commit().await.unwrap();

        let listing_id = seed_listing(&db, CONTROLLER_ROLE_LISTING_URL).await;
        seed_faa_admission(&db, listing_id).await;
        let rendered_html = trusted_controller_role_html(EVIDENCE);
        let extracted_listing_json = json!({"avionics": [{
            "manufacturer": null,
            "model": "GDU-1040",
            "types": ["Flight Display"],
            "quantity": 2,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": EVIDENCE,
            "source_confidence": "medium"
        }]})
        .to_string();
        let submission_id = seed_submission_and_review(
            &db,
            listing_id,
            CONTROLLER_ROLE_LISTING_URL,
            &rendered_html,
            Some(&extracted_listing_json),
            Some(listing_id),
            None,
        )
        .await;
        let aspect = PendingReviewAspect::avionics(
            "fixture:model-only-leading-dual:0",
            "avionics",
            "GDU-1040",
            EVIDENCE,
            "verified identity needs installation corroboration",
            2,
            "installed",
            Some(EVIDENCE.to_string()),
            Some("medium".to_string()),
        )
        .with_suggested_product(ReviewProduct::verified(
            product_id,
            "Garmin",
            "GDU 1040",
            vec!["Flight Display".to_string()],
        ));
        crate::listing::review::stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[aspect],
        )
        .await
        .unwrap();

        let preflight =
            preflight_listing_avionics(&db, listing_id, AvionicsVerificationExecutionMode::Apply)
                .await
                .expect("the exact model-only counted unit should preflight locally");
        let ListingAvionicsVerificationPreflight::PendingReview { report } = preflight else {
            panic!("the listing still has one pending review")
        };
        assert_eq!(report.verified_local_identity_components, 1);

        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let applied = verify_listing_avionics(
            &db,
            Some(&extractor),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("the model-only counted unit must not contact the unavailable provider");
        let ListingAvionicsVerification::Processed { report } = applied else {
            panic!("the pending review should be processed")
        };
        assert_eq!(report.accepted, 1);
        assert_eq!(report.remaining_review_aspects, 0);
        let link: (i64, String, String, i64) = sqlx::query_as(
            r#"
            SELECT avionics_model_id, source, source_confidence, quantity
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            link,
            (
                product_id,
                "listing_explicit_count".to_string(),
                "high".to_string(),
                2,
            )
        );
        let authorization: String = sqlx::query_scalar(
            r#"
            SELECT authorization.authorization_kind
            FROM aircraft_sale_listing_avionics_link_authorizations authorization
            JOIN aircraft_sale_listing_avionics link
              ON link.id = authorization.listing_link_id
            WHERE link.aircraft_sale_listing_id = ?
              AND authorization.association_role = 'installed'
              AND authorization.avionics_model_id = ?
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(authorization, "manufacturer_reuse");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
            )
            .bind(listing_id)
            .fetch_one(pool)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
            )
            .bind(listing_id)
            .fetch_one(pool)
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn exact_controller_leading_dual_rejects_non_countable_catalog_products_provider_free() {
        const EVIDENCE: &str = "Dual Garmin GIA63W GPS/NAV/WAAS";
        for invalid_product in ["model_number", "integrated_suite", "suite_membership"] {
            let db = AppDb::connect("sqlite::memory:").await.unwrap();
            let identifier_kind = if invalid_product == "model_number" {
                "manufacturer_model_number"
            } else {
                "manufacturer_part_number"
            };
            let product_id = seed_approved_named_product_for_manufacturer_with_identifier_kind(
                &db,
                true,
                "Garmin",
                "https://www.garmin.com",
                "GIA63W",
                identifier_kind,
                "011-01105-00",
                "GPS",
            )
            .await;
            let pool = sqlite_pool(&db);
            sqlx::query(
                "INSERT INTO avionics_types (name, normalized_name) VALUES ('NAV', 'nav') ON CONFLICT (normalized_name) DO NOTHING",
            )
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                r#"
                INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
                SELECT ?, id FROM avionics_types WHERE normalized_name = 'nav'
                "#,
            )
            .bind(product_id)
            .execute(pool)
            .await
            .unwrap();
            if invalid_product == "integrated_suite" {
                sqlx::query(
                    "UPDATE avionics_models SET valuation_scope = 'integrated_suite' WHERE id = ?",
                )
                .bind(product_id)
                .execute(pool)
                .await
                .unwrap();
            } else if invalid_product == "suite_membership" {
                let component_id =
                    seed_approved_named_product_for_manufacturer_with_identifier_kind(
                        &db,
                        true,
                        "Garmin",
                        "https://www.garmin.com",
                        "GDU 1040",
                        "manufacturer_part_number",
                        "011-00820-10",
                        "Flight Display",
                    )
                    .await;
                sqlx::query(
                    "INSERT INTO avionics_suite_components (suite_model_id, component_model_id, quantity) VALUES (?, ?, 1)",
                )
                .bind(product_id)
                .bind(component_id)
                .execute(pool)
                .await
                .unwrap();
            }
            let mut transaction = pool.begin().await.unwrap();
            assert!(refresh_reuse_attestation_sqlite(
                &db,
                &mut transaction,
                product_id,
                "https://www.garmin.com/aviation/product",
            )
            .await
            .unwrap());
            transaction.commit().await.unwrap();

            let listing_id = seed_listing(&db, CONTROLLER_ROLE_LISTING_URL).await;
            seed_faa_admission(&db, listing_id).await;
            let rendered_html = trusted_controller_role_html(EVIDENCE);
            let extracted_listing_json = json!({"avionics": [{
                "manufacturer": "Garmin",
                "model": "GIA63W",
                "types": ["GPS", "NAV"],
                "quantity": 2,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": EVIDENCE,
                "source_confidence": "medium"
            }]})
            .to_string();
            let submission_id = seed_submission_and_review(
                &db,
                listing_id,
                CONTROLLER_ROLE_LISTING_URL,
                &rendered_html,
                Some(&extracted_listing_json),
                Some(listing_id),
                None,
            )
            .await;
            let aspect = PendingReviewAspect::avionics(
                format!("fixture:leading-dual:{invalid_product}"),
                "avionics",
                "Garmin GIA63W",
                EVIDENCE,
                "verified identity needs installation corroboration",
                2,
                "installed",
                Some(EVIDENCE.to_string()),
                Some("medium".to_string()),
            )
            .with_suggested_product(ReviewProduct::verified(
                product_id,
                "Garmin",
                "GIA63W",
                vec!["GPS".to_string(), "NAV".to_string()],
            ));
            crate::listing::review::stage_pending_review(
                &db,
                listing_id,
                Some(submission_id),
                &[aspect],
            )
            .await
            .unwrap();

            let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
            let applied = verify_listing_avionics(
                &db,
                Some(&extractor),
                AvionicsVerificationExecutionMode::Apply,
                listing_id,
            )
            .await
            .expect("a non-countable local product should remain pending without Gemini");
            let ListingAvionicsVerification::Processed { report } = applied else {
                panic!("the pending review should be processed")
            };
            assert_eq!(report.accepted, 0, "{invalid_product}");
            assert_eq!(report.remaining_review_aspects, 1, "{invalid_product}");
            let state: (i64, i64, i64) = sqlx::query_as(
                r#"
                SELECT
                  (SELECT COUNT(*) FROM aircraft_sale_listing_avionics
                   WHERE aircraft_sale_listing_id = ?),
                  (SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews
                   WHERE listing_id = ?),
                  (SELECT COUNT(*) FROM gemini_api_usage
                   WHERE aircraft_sale_listing_id = ?)
                "#,
            )
            .bind(listing_id)
            .bind(listing_id)
            .bind(listing_id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(state, (0, 1, 0), "{invalid_product}");
        }
    }

    #[tokio::test]
    async fn final_apply_rejection_reports_prepared_but_zero_accepted_links() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let listing_id = seed_suggestion_only_listing(
            &db,
            "stale-final-apply-accepted-count",
            product_id,
            product_id,
            "high",
        )
        .await;
        let stale_row = load_listing_sources(
            &db,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap()
        .rows
        .pop()
        .unwrap();
        let mut current_aspects = parse_current_pending_review_aspects(
            &stale_row.review_payload_json,
            &stale_row.review_payload_sha256,
            stale_row.pending_aspect_count,
        )
        .unwrap();
        current_aspects.push(
            PendingReviewAspect::avionics(
                "fixture:concurrent-review:0",
                "avionics",
                "Concurrent unresolved observation",
                "Concurrent unresolved observation",
                "concurrent review revision",
                1,
                "installed",
                None,
                None,
            )
            .with_proposed_product(ReviewProduct::proposed(
                "Unknown",
                "Concurrent unresolved observation",
                vec!["Unknown".to_string()],
            )),
        );
        crate::listing::review::stage_pending_review(
            &db,
            listing_id,
            stale_row.submission_id,
            &current_aspects,
        )
        .await
        .unwrap();

        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let mut catalog_statuses = load_catalog_statuses(&db).await.unwrap();
        let report = process_listing(
            &db,
            Some(&extractor),
            true,
            &stale_row,
            &mut catalog_statuses,
        )
        .await;

        assert_eq!(report.status, "blocked");
        assert_eq!(report.prepared_link_count, 1);
        assert_eq!(report.accepted, 0);
        assert!(report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("automated review apply was rejected")));
        let stored_links: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(stored_links, 0);
    }

    #[tokio::test]
    async fn stale_verified_local_route_never_falls_through_to_a_provider() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let listing_id = seed_suggestion_only_listing(
            &db,
            "stale-verified-local-route",
            product_id,
            product_id,
            "high",
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
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let evidence = format!("{manufacturer} {model} standby instrument");
        let listing_context =
            ListingEvidenceContext::from_rendered_html(row.rendered_html.as_deref());
        let avionics_types = vec!["Flight Display".to_string()];
        let identity = || IdentityInput {
            manufacturer: &manufacturer,
            model: &model,
            avionics_types: &avionics_types,
            quantity: 1,
        };
        assert_eq!(
            preflight_identity_component(
                &db,
                &row,
                row.submission_source_url.as_deref(),
                &listing_context,
                identity(),
                Some(&evidence),
            )
            .await
            .unwrap()
            .route,
            AvionicsIdentityVerificationRoute::VerifiedLocal
        );

        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(product_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();
        // The deletion trigger may restage the pending review, but the local
        // resolver deliberately receives the route-planning snapshot here to
        // model eligibility changing between the two phases.
        let mut catalog_statuses = load_catalog_statuses(&db).await.unwrap();
        let attempt = resolve_local_only_identity_attempt(
            &db,
            true,
            &row,
            row.submission_source_url.as_deref(),
            &listing_context,
            0,
            "primary",
            identity(),
            "installed",
            Some(&evidence),
            Some("high"),
            &mut catalog_statuses,
        )
        .await
        .unwrap();

        assert!(attempt.is_none(), "stale local work must be reclassified");
        let usage_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(usage_count, 0);
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
            Some(&extractor),
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
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_link_authorizations WHERE listing_link_id = ?",
        )
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
        assert_eq!(authorization_count, 1);
        assert_eq!(pending_count, 0);
        assert_eq!(usage_count, 0);
    }

    #[tokio::test]
    async fn unbound_fresh_observation_cannot_consume_a_preserved_listing_occurrence() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let (listing_id, link_id, _) = seed_preserved_association_listing(
            &db,
            "fresh-and-preserved-same-product",
            product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;
        let pool = sqlite_pool(&db);
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
        let compact_model = model.replace(' ', "");
        let fresh_evidence = format!("{manufacturer} {compact_model} installed");
        let (submission_id, rendered_html, review_json, review_sha256, aspect_count): (
            i64,
            String,
            String,
            String,
            i64,
        ) = sqlx::query_as(
            r#"
            SELECT review.plugin_submission_id, submission.rendered_html,
                   review.review_payload_json, review.review_payload_sha256,
                   review.pending_aspect_count
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
        let mut aspects =
            parse_current_pending_review_aspects(&review_json, &review_sha256, aspect_count)
                .unwrap();
        aspects.push(
            PendingReviewAspect::avionics(
                "fixture:fresh-duplicate:0",
                "avionics",
                compact_model.clone(),
                fresh_evidence.clone(),
                "fresh exact observation requires catalog resolution",
                1,
                "installed",
                Some(fresh_evidence.clone()),
                Some("high".to_string()),
            )
            .with_proposed_product(ReviewProduct::proposed(
                manufacturer,
                compact_model,
                vec!["Flight Display".to_string()],
            )),
        );
        let rendered_html = format!("{rendered_html}<p>{fresh_evidence}</p>");
        sqlx::query(
            r#"
            UPDATE plugin_submissions
            SET rendered_html = ?, rendered_html_sha256 = ?
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

        let (endpoint, request_count, _requests, server) =
            spawn_classifier_endpoint("very_high").await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let result = verify_listing_avionics(
            &db,
            Some(&extractor),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("the unbound duplicate should fail closed without provider work");
        server.abort();
        let ListingAvionicsVerification::Processed { report } = result else {
            panic!("the listing should still have been processed")
        };

        assert_eq!(request_count.load(Ordering::SeqCst), 0);
        assert_eq!(report.status, "blocked");
        assert_eq!(report.prepared_link_count, 1);
        assert_eq!(report.accepted, 0);
        assert_eq!(report.remaining_review_aspects, 2);
        assert!(report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("multiple retained avionics occurrences")));
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].candidate_index, 0);
        assert_eq!(report.candidates[0].catalog_id, Some(product_id));

        let stored_links: Vec<(i64, i64)> = sqlx::query_as(
            r#"
            SELECT id, avionics_model_id
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_all(pool)
        .await
        .unwrap();
        let usage_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let pending_count: i64 = sqlx::query_scalar(
            "SELECT pending_aspect_count FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored_links, vec![(link_id, product_id)]);
        assert_eq!(pending_count, 2);
        assert_eq!(usage_count, 0);
    }

    #[tokio::test]
    async fn local_prepared_link_conflict_blocks_earlier_paid_candidate() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let replacement_id = seed_approved_suggestion_product(&db, true).await;
        let (listing_id, link_id, _) = seed_preserved_association_listing(
            &db,
            "local-conflict-before-paid-candidate",
            product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;
        let pool = sqlite_pool(&db);
        let products: Vec<(i64, String, String)> = sqlx::query_as(
            r#"
            SELECT model.id, manufacturer.name, model.name
            FROM avionics_models model
            JOIN avionics_manufacturers manufacturer
              ON manufacturer.id = model.avionics_manufacturer_id
            WHERE model.id IN (?, ?)
            ORDER BY model.id
            "#,
        )
        .bind(product_id)
        .bind(replacement_id)
        .fetch_all(pool)
        .await
        .unwrap();
        let product = products
            .iter()
            .find(|product| product.0 == product_id)
            .unwrap();
        let replacement = products
            .iter()
            .find(|product| product.0 == replacement_id)
            .unwrap();
        let product_model = product.2.replace(' ', "");
        let replacement_model = replacement.2.replace(' ', "");
        let generic_evidence = "Garmin GPS capability";
        let relationship_evidence = format!(
            "{} {} replaces {} {}",
            product.1, product_model, replacement.1, replacement_model
        );
        let (
            submission_id,
            rendered_html,
            retained_checkpoint,
            review_json,
            review_sha256,
            aspect_count,
        ): (i64, String, String, String, String, i64) = sqlx::query_as(
            r#"
            SELECT review.plugin_submission_id, submission.rendered_html,
                   submission.extracted_listing_json,
                   review.review_payload_json, review.review_payload_sha256,
                   review.pending_aspect_count
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
        let mut aspects =
            parse_current_pending_review_aspects(&review_json, &review_sha256, aspect_count)
                .unwrap();
        aspects.push(
            PendingReviewAspect::avionics(
                "fixture:relationship-fallback:1",
                "avionics",
                product_model.clone(),
                relationship_evidence.clone(),
                "replacement relationship requires replay",
                1,
                "replaces",
                Some(relationship_evidence.clone()),
                Some("high".to_string()),
            )
            .with_proposed_product(ReviewProduct::proposed(
                product.1.clone(),
                product_model.clone(),
                vec!["Flight Display".to_string()],
            ))
            .with_replacement_product(replacement_id),
        );
        let rendered_html =
            format!("{rendered_html}<p>{generic_evidence}</p><p>{relationship_evidence}</p>");
        let mut extracted_listing: Value = serde_json::from_str(&retained_checkpoint).unwrap();
        let checkpoint_occurrences = extracted_listing["avionics"]
            .as_array_mut()
            .expect("the preserved fixture has a current avionics checkpoint");
        let preserved_evidence = checkpoint_occurrences[0]["source_evidence_text"]
            .as_str()
            .expect("the preserved fixture has exact evidence")
            .to_string();
        let evidence_carrier_model = format!("{} standby instrument", product.2);
        checkpoint_occurrences.clear();
        checkpoint_occurrences.extend([
            json!({
            "manufacturer": "Garmin",
            "model": "GPS",
            "types": ["GPS"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": generic_evidence,
            "source_confidence": "high"
            }),
            json!({
            "manufacturer": product.1,
            "model": product_model,
            "types": ["Flight Display"],
            "quantity": 1,
            "configuration_action": "replaces",
            "replaces": {
                "manufacturer": replacement.1,
                "model": replacement_model,
                "types": ["Flight Display"]
            },
            "source_evidence_text": relationship_evidence,
            "source_confidence": "high"
            }),
            json!({
            "manufacturer": null,
            "model": evidence_carrier_model,
            "types": ["Flight Display"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": preserved_evidence,
            "source_confidence": "high"
            }),
        ]);
        let extracted_listing_json = extracted_listing.to_string();
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

        let (endpoint, request_count, _requests, server) =
            spawn_classifier_endpoint("very_high").await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let result = verify_listing_avionics(
            &db,
            Some(&extractor),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("a deterministic prepared-link conflict should block before paid work");
        server.abort();
        let ListingAvionicsVerification::Processed { report } = result else {
            panic!("the listing should still have been inspected")
        };

        assert_eq!(request_count.load(Ordering::SeqCst), 0, "{report:#?}");
        assert_eq!(report.status, "blocked");
        assert!(report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("multiple retained avionics occurrences")));
        assert_eq!(report.candidates.len(), 4);
        assert!(report
            .candidates
            .iter()
            .any(|candidate| { candidate.candidate_index == 0 && candidate.status == "rejected" }));
        assert_eq!(
            report
                .candidates
                .iter()
                .filter(|candidate| candidate.candidate_index == 1)
                .count(),
            2
        );
        let stored_link_id: i64 = sqlx::query_scalar(
            "SELECT id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
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
        assert_eq!(stored_link_id, link_id);
        assert_eq!(usage_count, 0);
    }

    #[tokio::test]
    async fn provider_free_generic_skips_paid_preflight_before_preserved_link_apply_guard() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let (listing_id, link_id, preserved_aspect_id) = seed_preserved_association_listing(
            &db,
            "unauthorized-preserved-before-paid-candidate",
            product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;
        let pool = sqlite_pool(&db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(product_id)
            .execute(pool)
            .await
            .unwrap();

        let (submission_id, rendered_html, review_json, review_sha256, aspect_count): (
            i64,
            String,
            String,
            String,
            i64,
        ) = sqlx::query_as(
            r#"
            SELECT review.plugin_submission_id, submission.rendered_html,
                   review.review_payload_json, review.review_payload_sha256,
                   review.pending_aspect_count
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
        let mut aspects =
            parse_current_pending_review_aspects(&review_json, &review_sha256, aspect_count)
                .unwrap();
        assert!(aspects
            .iter()
            .any(|aspect| aspect.id == preserved_aspect_id));
        aspects.push(
            PendingReviewAspect::avionics(
                "fixture:paid-generic:0",
                "avionics",
                "Garmin GPS",
                "Garmin GPS capability",
                "generic observation requires classification",
                1,
                "installed",
                None,
                None,
            )
            .with_proposed_product(ReviewProduct::proposed(
                "Garmin",
                "GPS",
                vec!["GPS".to_string()],
            )),
        );
        let paid_evidence = "Garmin GPS capability";
        let rendered_html = format!("{rendered_html}<p>{paid_evidence}</p>");
        let extracted_listing_json = current_listing_extraction_with_avionics(json!([{
            "manufacturer": "Garmin",
            "model": "GPS",
            "types": ["GPS"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": paid_evidence,
            "source_confidence": "high"
        }]));
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

        let apply_preflight =
            preflight_listing_avionics(&db, listing_id, AvionicsVerificationExecutionMode::Apply)
                .await
                .unwrap();
        let ListingAvionicsVerificationPreflight::PendingReview { report } = apply_preflight else {
            panic!("the listing should remain in review")
        };
        assert_eq!(report.status, "ready_retained_observations");
        assert_eq!(report.generic_invalid_identity_components, 1);
        assert!(report.note.contains("discarded deterministically"));
        let page_preflight = preflight_listing_avionics_page(
            &db,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap();
        assert_eq!(
            page_preflight
                .provider_request_plan
                .known_total_provider_requests_validation_envelope_maximum,
            0
        );

        let preview_preflight =
            preflight_listing_avionics(&db, listing_id, AvionicsVerificationExecutionMode::Preview)
                .await
                .unwrap();
        let ListingAvionicsVerificationPreflight::PendingReview { report } = preview_preflight
        else {
            panic!("preview should still inspect the pending review")
        };
        assert_eq!(report.status, "ready_retained_observations");
        assert_eq!(report.generic_invalid_identity_components, 1);

        let catalog_before: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, catalog_status FROM avionics_models ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        let review_before: (String, i64) = sqlx::query_as(
            "SELECT review_payload_sha256, pending_aspect_count FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let link_before: (i64, i64, String) = sqlx::query_as(
            "SELECT id, avionics_model_id, configuration_action FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let (endpoint, request_count, _requests, server) =
            spawn_classifier_endpoint("very_high").await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let result = verify_listing_avionics(
            &db,
            Some(&extractor),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("the deterministic preserved-link blocker should be reported");
        server.abort();
        let ListingAvionicsVerification::Processed { report } = result else {
            panic!("the pending review should be processed")
        };
        assert_eq!(request_count.load(Ordering::SeqCst), 0);
        assert_eq!(report.status, "blocked");
        assert_eq!(report.prepared_link_count, 0);
        assert_eq!(report.accepted, 0);
        assert!(report.error.as_deref().is_some_and(|error| error.contains(
            &format!("preserved avionics catalog id {product_id} has neither current manufacturer-reuse nor exact same-case authorization")
        )));

        let catalog_after: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, catalog_status FROM avionics_models ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        let review_after: (String, i64) = sqlx::query_as(
            "SELECT review_payload_sha256, pending_aspect_count FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let link_after: (i64, i64, String) = sqlx::query_as(
            "SELECT id, avionics_model_id, configuration_action FROM aircraft_sale_listing_avionics WHERE id = ?",
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
        assert_eq!(catalog_after, catalog_before);
        assert_eq!(review_after, review_before);
        assert_eq!(link_after, link_before);
        assert_eq!(usage_count, 0);
    }

    #[tokio::test]
    async fn unauthorized_same_key_link_remains_inside_paid_candidate_envelope() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let (listing_id, _link_id, _) = seed_preserved_association_listing(
            &db,
            "unauthorized-same-key-paid-candidate",
            product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;
        let pool = sqlite_pool(&db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(product_id)
            .execute(pool)
            .await
            .unwrap();
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
        let (submission_id, rendered_html, review_json, review_sha256, aspect_count): (
            i64,
            String,
            String,
            String,
            i64,
        ) = sqlx::query_as(
            r#"
            SELECT review.plugin_submission_id, submission.rendered_html,
                   review.review_payload_json, review.review_payload_sha256,
                   review.pending_aspect_count
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
        let mut aspects =
            parse_current_pending_review_aspects(&review_json, &review_sha256, aspect_count)
                .unwrap();
        let evidence = format!("{manufacturer} {model} second installed occurrence");
        aspects.push(
            PendingReviewAspect::avionics(
                "fixture:same-key-paid:0",
                "avionics",
                model.clone(),
                evidence.clone(),
                "same catalog identity requires renewed grounding",
                1,
                "installed",
                Some(evidence.clone()),
                Some("high".to_string()),
            )
            .with_proposed_product(ReviewProduct::proposed(
                manufacturer.clone(),
                model.clone(),
                vec!["Flight Display".to_string()],
            )),
        );
        let rendered_html = format!("{rendered_html}<p>{evidence}</p>");
        sqlx::query(
            r#"
            UPDATE plugin_submissions
            SET rendered_html = ?, rendered_html_sha256 = ?
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

        let preflight =
            preflight_listing_avionics(&db, listing_id, AvionicsVerificationExecutionMode::Apply)
                .await
                .unwrap();
        let ListingAvionicsVerificationPreflight::PendingReview { report } = preflight else {
            panic!("the listing should remain in review")
        };
        assert_eq!(report.status, "ready_retained_observations");
        assert_eq!(report.grounded_initial_identity_components, 1);

        let (endpoint, request_count, _requests, server) =
            spawn_classifier_endpoint("very_high").await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let result = verify_listing_avionics(
            &db,
            Some(&extractor),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .unwrap();
        server.abort();
        let ListingAvionicsVerification::Processed { report } = result else {
            panic!("the listing should have been processed")
        };
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert_eq!(report.status, "blocked");
        assert_eq!(report.accepted, 0);
        assert!(report.error.as_deref().is_some_and(|error| {
            error.contains("automated review apply was rejected")
                && !error.contains("unrelated existing listing avionics cannot commit")
        }));
    }

    #[tokio::test]
    async fn cross_maker_adjudication_fallback_does_not_false_block_apply_preflight() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let garmin_product_id = seed_approved_suggestion_product(&db, true).await;
        let cessna_product_id = seed_approved_suggestion_product_for_manufacturer(
            &db,
            true,
            "Cessna",
            "https://www.cessna.com",
        )
        .await;
        let (listing_id, _link_id, _) = seed_preserved_association_listing(
            &db,
            "cross-maker-adjudication-fallback",
            cessna_product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;
        let pool = sqlite_pool(&db);
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(cessna_product_id)
            .execute(pool)
            .await
            .unwrap();
        let (garmin_manufacturer, garmin_model): (String, String) = sqlx::query_as(
            r#"
            SELECT manufacturer.name, model.name
            FROM avionics_models model
            JOIN avionics_manufacturers manufacturer
              ON manufacturer.id = model.avionics_manufacturer_id
            WHERE model.id = ?
            "#,
        )
        .bind(garmin_product_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let (submission_id, rendered_html, review_json, review_sha256, aspect_count): (
            i64,
            String,
            String,
            String,
            i64,
        ) = sqlx::query_as(
            r#"
            SELECT review.plugin_submission_id, submission.rendered_html,
                   review.review_payload_json, review.review_payload_sha256,
                   review.pending_aspect_count
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
        let mut aspects =
            parse_current_pending_review_aspects(&review_json, &review_sha256, aspect_count)
                .unwrap();
        let observed_model = format!("{garmin_model} Flight Display");
        let evidence = format!("{garmin_manufacturer} {observed_model} installed");
        aspects.push(
            PendingReviewAspect::avionics(
                "fixture:cross-maker-adjudication:0",
                "avionics",
                observed_model.clone(),
                evidence.clone(),
                "descriptive expansion requires candidate adjudication",
                1,
                "installed",
                Some(evidence.clone()),
                Some("high".to_string()),
            )
            .with_proposed_product(ReviewProduct::proposed(
                garmin_manufacturer,
                observed_model,
                vec!["Flight Display".to_string()],
            )),
        );
        let rendered_html = format!("{rendered_html}<p>{evidence}</p>");
        sqlx::query(
            r#"
            UPDATE plugin_submissions
            SET rendered_html = ?, rendered_html_sha256 = ?
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

        let preflight =
            preflight_listing_avionics(&db, listing_id, AvionicsVerificationExecutionMode::Apply)
                .await
                .unwrap();
        let ListingAvionicsVerificationPreflight::PendingReview { report } = preflight else {
            panic!("the listing should remain in review")
        };

        assert_eq!(report.status, "ready_retained_observations");
        assert_eq!(report.candidate_adjudication_identity_components, 1);
        assert!(!report
            .note
            .contains("automatic verification is unavailable"));
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
        let (
            submission_id,
            review_json,
            review_sha256,
            pending_aspect_count,
            rendered_html,
            retained_checkpoint,
        ): (i64, String, String, i64, String, String) = sqlx::query_as(
            r#"
            SELECT review.plugin_submission_id, review.review_payload_json,
                   review.review_payload_sha256, review.pending_aspect_count,
                   submission.rendered_html, submission.extracted_listing_json
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
        let mut extracted_listing: Value = serde_json::from_str(&retained_checkpoint).unwrap();
        extracted_listing["avionics"]
            .as_array_mut()
            .expect("the preserved fixture has a current avionics checkpoint")
            .push(json!({
            "manufacturer": null,
            "model": "Replacement Package",
            "types": ["GPS"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": ordinary_evidence,
            "source_confidence": "high"
            }));
        let extracted_listing_json = extracted_listing.to_string();
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
            Some(&extractor),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("mixed fallback should consume independent local work without a provider");
        let ListingAvionicsVerification::Processed { report } = result else {
            panic!("the mixed review should remain pending after partial local progress")
        };
        assert_eq!(report.status, "applied", "{report:#?}");
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
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_link_authorizations WHERE listing_link_id = ?",
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
            Some(&extractor),
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
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_link_authorizations WHERE listing_link_id = ?",
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let retained_aspects =
            parse_current_pending_review_aspects(&review_after.1, &review_after.2, review_after.3)
                .unwrap();
        assert_eq!(review_after, review_before);
        assert_eq!(link_after, link_before);
        assert_eq!(authorization_count, 0);
        assert!(retained_aspects
            .iter()
            .any(|aspect| aspect.id == preserved_aspect_id));
        assert!(retained_aspects
            .iter()
            .any(|aspect| aspect.id == ordinary_aspect_id));
    }

    #[tokio::test]
    async fn blocked_apply_retains_validated_reextraction_and_rerun_skips_provider() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let replacement_id = seed_approved_suggestion_product(&db, true).await;
        let (listing_id, link_id, _) = seed_preserved_association_listing(
            &db,
            "durable-reextraction-after-block",
            product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;
        let submission_id = append_nonreplayable_review_aspect(&db, listing_id).await;
        let pool = sqlite_pool(&db);
        let (manufacturer, model, evidence): (String, String, String) = sqlx::query_as(
            r#"
            SELECT manufacturer.name, model.name, link.source_notes
            FROM avionics_models model
            JOIN avionics_manufacturers manufacturer
              ON manufacturer.id = model.avionics_manufacturer_id
            JOIN aircraft_sale_listing_avionics link
              ON link.avionics_model_id = model.id
            WHERE model.id = ? AND link.id = ?
            "#,
        )
        .bind(product_id)
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let (replacement_manufacturer, replacement_model): (String, String) = sqlx::query_as(
            r#"
            SELECT manufacturer.name, model.name
            FROM avionics_models model
            JOIN avionics_manufacturers manufacturer
              ON manufacturer.id = model.avionics_manufacturer_id
            WHERE model.id = ?
            "#,
        )
        .bind(replacement_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let evidence_carrier_model = format!("{model} standby instrument");
        let relationship_evidence = format!(
            "{manufacturer} {} replaces {replacement_manufacturer} {}",
            model.replace(' ', ""),
            replacement_model.replace(' ', "")
        );
        let (rendered_html,): (String,) =
            sqlx::query_as("SELECT rendered_html FROM plugin_submissions WHERE id = ?")
                .bind(submission_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let rendered_html = format!("{rendered_html}<p>{relationship_evidence}</p>");
        sqlx::query(
            "UPDATE plugin_submissions SET rendered_html = ?, rendered_html_sha256 = ? WHERE id = ?",
        )
        .bind(&rendered_html)
        .bind(sha256_hex(rendered_html.as_bytes()))
        .bind(submission_id)
        .execute(pool)
        .await
        .unwrap();
        let mut prior = retained_legacy_listing_extraction();
        prior["avionics"] = json!([{
            "manufacturer": manufacturer,
            "model": model,
            "types": ["Flight Display"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": format!("{evidence} fabricated"),
            "source_confidence": "high"
        }]);
        let canonical_prior =
            canonical_complete_current_listing_value(&prior, "test prior extraction").unwrap();
        sqlx::query(
            "UPDATE plugin_submissions SET extracted_listing_json = ?, extraction_error = NULL WHERE id = ?",
        )
        .bind(prior.to_string())
        .bind(submission_id)
        .execute(pool)
        .await
        .unwrap();
        let (endpoint, request_count, server) = spawn_listing_extraction_endpoint(json!([
        {
            "manufacturer": null,
            "model": evidence_carrier_model,
            "types": ["Flight Display"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": evidence,
            "source_confidence": "high"
        },
        {
            "manufacturer": manufacturer,
            "model": model,
            "types": ["Flight Display"],
            "quantity": 1,
            "configuration_action": "replaces",
            "replaces": {
                "manufacturer": replacement_manufacturer,
                "model": replacement_model,
                "types": ["Flight Display"]
            },
            "source_evidence_text": relationship_evidence,
            "source_confidence": "high"
        }
        ]))
        .await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let first = verify_listing_avionics(
            &db,
            Some(&extractor),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("the downstream duplicate conflict should be a reported block");
        server.abort();
        let ListingAvionicsVerification::Processed { report } = first else {
            panic!("the listing should remain in review")
        };
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert_eq!(report.status, "blocked");
        assert!(report.reextraction_required);
        assert!(report.reextraction_attempted);
        assert!(report.reextraction_succeeded);
        assert!(report.reextraction_error.is_none());
        assert!(report.source_extraction_error.is_none());
        assert!(report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("multiple retained avionics occurrences")));

        let (persisted_json, extraction_error): (String, Option<String>) = sqlx::query_as(
            "SELECT extracted_listing_json, extraction_error FROM plugin_submissions WHERE id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(extraction_error.is_none());
        let persisted: Value = serde_json::from_str(&persisted_json).unwrap();
        for field in [
            "manufacturer",
            "model",
            "variant",
            "model_year",
            "asking_price_usd",
            "airframe_hours",
            "engine_hours",
            "registration_number",
            "serial_number",
            "status",
            "valuation_facts",
        ] {
            assert_eq!(
                persisted[field], canonical_prior[field],
                "field {field} changed"
            );
        }
        assert_eq!(persisted["avionics"].as_array().unwrap().len(), 2);
        assert_eq!(
            persisted["avionics"][0]["source_confidence"],
            Value::String("high".to_string())
        );

        let preflight =
            preflight_listing_avionics(&db, listing_id, AvionicsVerificationExecutionMode::Apply)
                .await
                .unwrap();
        let ListingAvionicsVerificationPreflight::PendingReview { report } = preflight else {
            panic!("the duplicate conflict should leave a pending review")
        };
        assert_eq!(report.status, "ready_retained_observations");
        assert!(!report.reextraction_required);
        assert_eq!(report.retained_identity_components, 3);
        assert_eq!(report.verified_local_identity_components, 2);

        let unavailable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let second = verify_listing_avionics(
            &db,
            Some(&unavailable),
            AvionicsVerificationExecutionMode::Apply,
            listing_id,
        )
        .await
        .expect("the durable extraction must make the blocked rerun provider-free");
        let ListingAvionicsVerification::Processed { report } = second else {
            panic!("the duplicate conflict should remain pending")
        };
        assert_eq!(report.status, "blocked");
        assert_eq!(report.raw_avionics_source, "retained_extraction");
        assert!(!report.reextraction_required);
        assert!(!report.reextraction_attempted);
    }

    #[tokio::test]
    async fn stale_capture_rejects_validated_reextraction_without_writing_it() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let product_id = seed_approved_suggestion_product(&db, true).await;
        let (listing_id, link_id, _) = seed_preserved_association_listing(
            &db,
            "stale-capture-reextraction",
            product_id,
            PreservedAssociationFixture::Exact,
        )
        .await;
        let submission_id = append_nonreplayable_review_aspect(&db, listing_id).await;
        store_retained_legacy_listing_extraction(&db, submission_id).await;
        let pool = sqlite_pool(&db);
        let (manufacturer, model, evidence): (String, String, String) = sqlx::query_as(
            r#"
            SELECT manufacturer.name, model.name, link.source_notes
            FROM avionics_models model
            JOIN avionics_manufacturers manufacturer
              ON manufacturer.id = model.avionics_manufacturer_id
            JOIN aircraft_sale_listing_avionics link
              ON link.avionics_model_id = model.id
            WHERE model.id = ? AND link.id = ?
            "#,
        )
        .bind(product_id)
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let stale_row = load_listing_sources(
            &db,
            &AvionicsVerificationScope::new(1, Some(listing_id), None),
        )
        .await
        .unwrap()
        .rows
        .pop()
        .unwrap();
        let changed_html = "<p>The retained capture changed while Gemini was running</p>";
        sqlx::query(
            "UPDATE plugin_submissions SET rendered_html = ?, rendered_html_sha256 = ? WHERE id = ?",
        )
        .bind(changed_html)
        .bind(sha256_hex(changed_html.as_bytes()))
        .bind(submission_id)
        .execute(pool)
        .await
        .unwrap();

        let (endpoint, request_count, server) = spawn_listing_extraction_endpoint(json!([{
            "manufacturer": manufacturer,
            "model": model,
            "types": ["Flight Display"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": evidence,
            "source_confidence": "high"
        }]))
        .await;
        let extractor = GeminiListingExtractor::with_test_endpoint(endpoint);
        let mut catalog_statuses = load_catalog_statuses(&db).await.unwrap();
        let report = process_listing(
            &db,
            Some(&extractor),
            true,
            &stale_row,
            &mut catalog_statuses,
        )
        .await;
        server.abort();
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert_eq!(report.status, "blocked");
        assert!(report.reextraction_attempted);
        assert!(!report.reextraction_succeeded);
        assert!(report
            .reextraction_error
            .as_deref()
            .is_some_and(|error| error.contains("changed while re-extraction was running")));

        let (stored_html, stored_extraction, stored_error): (
            String,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT rendered_html, extracted_listing_json, extraction_error FROM plugin_submissions WHERE id = ?",
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored_html, changed_html);
        let expected_prior_extraction = retained_legacy_listing_extraction().to_string();
        assert_eq!(
            stored_extraction.as_deref(),
            Some(expected_prior_extraction.as_str())
        );
        assert_eq!(
            stored_error.as_deref(),
            Some("legacy extraction unavailable")
        );
    }

    #[tokio::test]
    async fn validated_reextraction_write_is_idempotent_and_rejects_a_different_prior_state() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id =
            seed_listing(&db, "https://example.test/listing/reextraction-idempotency").await;
        let submission_id = seed_submission_and_review(
            &db,
            listing_id,
            "https://example.test/listing/reextraction-idempotency",
            "<p>Garmin fixture installed</p>",
            Some(r#"{"avionics":[{"manufacturer":"Garmin","model":"Fixture","types":["GPS"]}]}"#),
            Some(listing_id),
            Some("legacy extraction warning"),
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
        let validated = json!({
            "manufacturer": "Cessna",
            "model": "172",
            "variant": "S",
            "model_year": 2020,
            "asking_price_usd": 300000,
            "currency": "USD",
            "airframe_hours": 1000,
            "avionics": [{
                "manufacturer": "Garmin",
                "model": "Fixture",
                "types": ["GPS"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": "Garmin fixture installed",
                "source_confidence": "high"
            }],
            "valuation_facts": []
        })
        .to_string();
        persist_validated_listing_reextraction(&db, &row, submission_id, &validated)
            .await
            .unwrap();
        persist_validated_listing_reextraction(&db, &row, submission_id, &validated)
            .await
            .expect("the same guarded extraction write must be idempotent");

        let concurrent =
            r#"{"avionics":[{"manufacturer":"Garmin","model":"Concurrent","types":["GPS"]}]}"#;
        sqlx::query(
            "UPDATE plugin_submissions SET extracted_listing_json = ?, extraction_error = NULL WHERE id = ?",
        )
        .bind(concurrent)
        .bind(submission_id)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();
        let error = persist_validated_listing_reextraction(&db, &row, submission_id, &validated)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("prior extraction state changed"));
        let stored: String = sqlx::query_scalar(
            "SELECT extracted_listing_json FROM plugin_submissions WHERE id = ?",
        )
        .bind(submission_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(stored, concurrent);
    }

    #[tokio::test]
    async fn locked_reextraction_state_revalidates_every_postgres_write_dependency() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id =
            seed_listing(&db, "https://example.test/listing/reextraction-lock-state").await;
        let prior_extraction = retained_legacy_listing_extraction().to_string();
        let submission_id = seed_submission_and_review(
            &db,
            listing_id,
            "https://example.test/listing/reextraction-lock-state",
            "<p>Garmin G5 installed</p>",
            Some(&prior_extraction),
            Some(listing_id),
            Some("legacy extraction warning"),
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
        let target = r#"{"avionics":[]}"#;
        let mut locked = locked_reextraction_state(&row, submission_id);

        validate_locked_reextraction_state(&row, submission_id, target, &locked).unwrap();

        locked.extracted_listing_json = Some(target.to_string());
        locked.submission_extraction_error = None;
        validate_locked_reextraction_state(&row, submission_id, target, &locked)
            .expect("an identical retry remains valid after locking");

        locked.extracted_listing_json = Some(r#"{"avionics":[1]}"#.to_string());
        let error =
            validate_locked_reextraction_state(&row, submission_id, target, &locked).unwrap_err();
        assert!(error.to_string().contains("prior extraction state changed"));

        locked = locked_reextraction_state(&row, submission_id);
        locked.review_payload_json.push(' ');
        let error =
            validate_locked_reextraction_state(&row, submission_id, target, &locked).unwrap_err();
        assert!(error
            .to_string()
            .contains("pending-review revision changed"));

        locked = locked_reextraction_state(&row, submission_id);
        locked.rendered_html.push_str(" changed");
        let error =
            validate_locked_reextraction_state(&row, submission_id, target, &locked).unwrap_err();
        assert!(error.to_string().contains("source capture"));
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
                Some(&extractor),
                AvionicsVerificationExecutionMode::Apply,
                listing_id,
            )
            .await
            .expect("ineligible preserved cards should remain pending without a provider call");
            let ListingAvionicsVerification::Processed { report } = result else {
                panic!("the listing should retain its pending review")
            };
            assert_eq!(report.status, "blocked", "fixture {index}: {report:#?}");
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
            let submission_id: i64 = sqlx::query_scalar(
                "SELECT plugin_submission_id FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
            )
            .bind(listing_id)
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
            store_retained_legacy_listing_extraction(&db, submission_id).await;

            let preflight = preflight_listing_avionics(
                &db,
                listing_id,
                AvionicsVerificationExecutionMode::Apply,
            )
            .await
            .unwrap();
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

        let preflight =
            preflight_listing_avionics(&db, listing_id, AvionicsVerificationExecutionMode::Apply)
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

        let verification = verify_listing_avionics(
            &db,
            None,
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
                r#"{"avionics":[{"manufacturer":"GARMIN","model":"GPS","types":["GPS"],"quantity":1,"configuration_action":"installed","replaces":null,"source_evidence_text":"GARMIN GPS","source_confidence":"high"}]}"#,
            ),
            Some(listing_id),
            None,
        )
        .await;
        listing_id
    }

    async fn seed_generic_primary_with_concrete_replacement_listing(
        db: &AppDb,
        suffix: &str,
        action: &str,
        replacement_model: &str,
    ) -> i64 {
        seed_relationship_listing(
            db,
            suffix,
            action,
            "GARMIN",
            "GPS",
            "GARMIN",
            replacement_model,
        )
        .await
    }

    async fn seed_concrete_primary_with_generic_replacement_listing(
        db: &AppDb,
        suffix: &str,
        action: &str,
    ) -> i64 {
        seed_relationship_listing(db, suffix, action, "GARMIN", "GNS 430W", "GARMIN", "GPS").await
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_relationship_listing(
        db: &AppDb,
        suffix: &str,
        action: &str,
        primary_manufacturer: &str,
        primary_model: &str,
        replacement_manufacturer: &str,
        replacement_model: &str,
    ) -> i64 {
        let source_url = format!("https://example.test/listing/{suffix}");
        let listing_id = seed_listing(db, &source_url).await;
        seed_faa_admission(db, listing_id).await;
        let evidence = format!(
            "{primary_manufacturer} {primary_model} {action} {replacement_manufacturer} {replacement_model}"
        );
        let rendered_html = format!("<p>{evidence}</p>");
        let extracted = serde_json::json!({
            "avionics": [{
                "manufacturer": primary_manufacturer,
                "model": primary_model,
                "types": ["GPS"],
                "quantity": 1,
                "configuration_action": action,
                "replaces": {
                    "manufacturer": replacement_manufacturer,
                    "model": replacement_model,
                    "types": ["GPS"]
                },
                "source_evidence_text": evidence,
                "source_confidence": "high"
            }]
        })
        .to_string();
        seed_submission_and_review(
            db,
            listing_id,
            &source_url,
            &rendered_html,
            Some(&extracted),
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
              engine_member_name, engine_member_sha256, record_hash_domain
            ) VALUES (
              ?, '2026-07-31',
              ?,
              ?, ?, ?, 'MASTER.txt', ?, 'ACFTREF.txt', ?, 'ENGINE.txt', ?,
              'aircost-faa-master-retained-aircraft-projection-v1'
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
        seed_approved_suggestion_product_for_manufacturer(
            db,
            attest,
            "Garmin",
            "https://www.garmin.com",
        )
        .await
    }

    async fn seed_approved_suggestion_product_for_manufacturer(
        db: &AppDb,
        attest: bool,
        manufacturer: &str,
        source_origin: &str,
    ) -> i64 {
        let sequence: i64 = sqlx::query_scalar("SELECT COUNT(*) + 1 FROM avionics_models")
            .fetch_one(sqlite_pool(db))
            .await
            .unwrap();
        let model = if manufacturer == "Garmin" {
            format!("GI 275 TEST {sequence}")
        } else {
            format!("{manufacturer} DISPLAY TEST {sequence}")
        };
        let identifier = if manufacturer == "Garmin" {
            format!("GI-275-TEST-{sequence}")
        } else {
            format!(
                "{}-DISPLAY-TEST-{sequence}",
                normalize_avionics_manufacturer_name(manufacturer).to_uppercase()
            )
        };
        seed_approved_named_product_for_manufacturer(
            db,
            attest,
            manufacturer,
            source_origin,
            &model,
            &identifier,
            "Flight Display",
        )
        .await
    }

    async fn seed_manufacturer_identity_only(db: &AppDb, manufacturer: &str, source_origin: &str) {
        let pool = sqlite_pool(db);
        let manufacturer_key = normalize_avionics_manufacturer_name(manufacturer);
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(manufacturer)
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
                source_url: format!("{source_origin}/aviation/"),
                source_title: format!("{manufacturer} Aviation"),
                evidence_text: format!("{manufacturer} identifies its aviation products."),
            },
        )
        .await
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_approved_named_product_for_manufacturer(
        db: &AppDb,
        attest: bool,
        manufacturer: &str,
        source_origin: &str,
        model: &str,
        identifier: &str,
        avionics_type: &str,
    ) -> i64 {
        seed_approved_named_product_for_manufacturer_with_identifier_kind(
            db,
            attest,
            manufacturer,
            source_origin,
            model,
            "manufacturer_model_number",
            identifier,
            avionics_type,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_approved_named_product_for_manufacturer_with_identifier_kind(
        db: &AppDb,
        attest: bool,
        manufacturer: &str,
        source_origin: &str,
        model: &str,
        identifier_kind: &str,
        identifier: &str,
        avionics_type: &str,
    ) -> i64 {
        let pool = sqlite_pool(db);
        let manufacturer_key = normalize_avionics_manufacturer_name(manufacturer);
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(manufacturer)
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
                source_url: format!("{source_origin}/aviation/"),
                source_title: format!("{manufacturer} Aviation"),
                evidence_text: format!("{manufacturer} identifies its aviation products."),
            },
        )
        .await
        .unwrap();

        let product_source_url = format!("{source_origin}/aviation/product");
        let product_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_evidence_kind, identity_confidence, catalog_reviewed_at
            ) VALUES (?, ?, ?, ?, ?, ?,
                      ?, ?, ?,
                      'authoritative_reference', 'very_high', CURRENT_TIMESTAMP)
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .bind(model)
        .bind(normalize_avionics_model_name(model))
        .bind(identifier_kind)
        .bind(identifier)
        .bind(normalize_avionics_identifier(identifier))
        .bind(&product_source_url)
        .bind(format!("{manufacturer} product manual"))
        .bind(format!("{manufacturer} identifies the exact test product."))
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(avionics_type)
        .bind(normalize_name(avionics_type))
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
        .bind(normalize_name(avionics_type))
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
                       ?, ?, ?, ?,
                       'curated_bootstrap', 'verification test fixture'
                FROM avionics_approved_product_identities
                WHERE avionics_model_id = ?
                ON CONFLICT DO NOTHING
            "#,
            )
            .bind(source_origin)
            .bind(&product_source_url)
            .bind(format!("{manufacturer} aviation product catalog"))
            .bind(format!("{manufacturer} publishes the exact test product."))
            .bind(product_id)
            .execute(pool)
            .await
            .unwrap();
            let mut transaction = pool.begin().await.unwrap();
            assert!(refresh_reuse_attestation_sqlite(
                db,
                &mut transaction,
                product_id,
                &product_source_url,
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
        let extracted_listing_json = json!({"avionics": [{
            "manufacturer": product.0,
            "model": product.1,
            "types": ["Flight Display"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": evidence,
            "source_confidence": "high"
        }]})
        .to_string();
        let listing_id = seed_listing(db, &source_url).await;
        seed_faa_admission(db, listing_id).await;
        let submission_id = seed_submission_and_review(
            db,
            listing_id,
            &source_url,
            &rendered_html,
            Some(&extracted_listing_json),
            Some(listing_id),
            None,
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
        let review_evidence = match fixture {
            PreservedAssociationFixture::AmbiguousQualifier => {
                format!("{manufacturer} {model} WAAS upgraded")
            }
            _ => exact_evidence.clone(),
        };
        let source_url = format!("https://example.test/listing/{suffix}");
        let rendered_html = if review_evidence == exact_evidence {
            format!("<p>{exact_evidence}</p>")
        } else {
            format!("<p>{exact_evidence}</p><p>{review_evidence}</p>")
        };
        let extracted_listing_json = json!({"avionics": [{
            "manufacturer": manufacturer,
            "model": model,
            "types": ["Flight Display"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": exact_evidence,
            "source_confidence": "high"
        }]})
        .to_string();
        let listing_id = seed_listing(db, &source_url).await;
        seed_faa_admission(db, listing_id).await;
        let submission_id = seed_submission_and_review(
            db,
            listing_id,
            &source_url,
            &rendered_html,
            Some(&extracted_listing_json),
            Some(listing_id),
            None,
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
        .bind(&review_evidence)
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
                _ => Some(review_evidence),
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

    async fn append_nonreplayable_review_aspect(db: &AppDb, listing_id: i64) -> i64 {
        let pool = sqlite_pool(db);
        let (submission_id, review_json, review_sha256, pending_aspect_count): (
            i64,
            String,
            String,
            i64,
        ) = sqlx::query_as(
            r#"
            SELECT plugin_submission_id, review_payload_json,
                   review_payload_sha256, pending_aspect_count
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
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
        aspects.push(
            PendingReviewAspect::avionics(
                format!("fixture:nonreplayable:{listing_id}"),
                "avionics",
                "Legacy unresolved observation",
                "Legacy unresolved observation",
                "legacy observation has no exact current-schema source evidence",
                1,
                "installed",
                None,
                None,
            )
            .with_proposed_product(ReviewProduct::proposed(
                "Unknown",
                "Legacy unresolved observation",
                vec!["Unknown".to_string()],
            )),
        );
        crate::listing::review::stage_pending_review(db, listing_id, Some(submission_id), &aspects)
            .await
            .unwrap();
        submission_id
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
        let extracted_listing_json = extracted_listing_json.map(|raw| {
            let Ok(value) = serde_json::from_str::<Value>(raw) else {
                return raw.to_string();
            };
            if value
                .as_object()
                .is_some_and(|object| object.len() == 1 && object.contains_key("avionics"))
            {
                return current_listing_extraction_with_avionics(value["avionics"].clone());
            }
            raw.to_string()
        });
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
        .bind(extracted_listing_json.as_deref())
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
