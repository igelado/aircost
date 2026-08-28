//! Permanent, idempotent automatic listing verification.
//!
//! This module composes the existing aircraft, avionics, and listing
//! finalization boundaries. It owns no parallel verification state: aircraft
//! assignments, avionics links, pending reviews, listing ingestion state, and
//! Gemini usage accounting remain the durable checkpoints.

use std::fmt;

use serde::Serialize;
use sqlx::FromRow;

use crate::aircraft::reference::persistence::{
    listing_reference_status, ListingReferenceStatus, ReferenceGap,
};
use crate::aircraft::verification::{
    apply_listing_aircraft_verification, preflight_listing_aircraft_verification,
    preview_listing_aircraft_verification, AircraftVerificationMethod, AircraftVerificationOutcome,
    AircraftVerificationServices,
};
use crate::avionics::verification::{
    preflight_listing_avionics, provider_request_plan_for_listing_preflights,
    verify_listing_avionics, AvionicsProviderRequestPlan, AvionicsVerificationCheckpoint,
    AvionicsVerificationExecutionMode, ListingAvionicsVerification,
    ListingAvionicsVerificationPreflight,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::GeminiListingExtractor;
use crate::listings::{finalize_reviewed_listing_ingestion, ListingFinalizationOutcome};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListingVerificationMode {
    Preflight,
    Preview,
    Apply,
}

impl ListingVerificationMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Preview => "preview",
            Self::Apply => "apply",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ListingVerificationScope {
    pub limit: i64,
    pub listing_id: Option<i64>,
    pub after_listing_id: Option<i64>,
}

pub const REVIEWER_PREFLIGHT_DEFAULT_LIMIT: i64 = 100;
pub const REVIEWER_PREFLIGHT_MAX_LIMIT: i64 = 100;

/// Reviewer-facing preflight scope whose owner is supplied separately by the
/// authenticated server boundary. Unlike the administrative scope above, this
/// type has no execution mode or provider services and cannot request writes.
#[derive(Clone, Debug)]
pub struct ReviewerListingPreflightScope {
    pub limit: i64,
    pub listing_id: Option<i64>,
    pub after_listing_id: Option<i64>,
}

impl ReviewerListingPreflightScope {
    pub fn new(limit: i64, listing_id: Option<i64>, after_listing_id: Option<i64>) -> Self {
        Self {
            limit,
            listing_id,
            after_listing_id,
        }
    }

    fn validate(&self) -> Result<(), ListingVerificationError> {
        ListingVerificationScope::new(self.limit, self.listing_id, self.after_listing_id)
            .validate()?;
        if self.limit > REVIEWER_PREFLIGHT_MAX_LIMIT {
            return Err(ListingVerificationError::Validation(format!(
                "limit must not exceed {REVIEWER_PREFLIGHT_MAX_LIMIT}"
            )));
        }
        Ok(())
    }
}

impl ListingVerificationScope {
    pub fn new(limit: i64, listing_id: Option<i64>, after_listing_id: Option<i64>) -> Self {
        Self {
            limit,
            listing_id,
            after_listing_id,
        }
    }

    fn validate(&self) -> Result<(), ListingVerificationError> {
        if self.limit < 1 {
            return Err(ListingVerificationError::Validation(
                "limit must be at least 1".to_string(),
            ));
        }
        if self.listing_id.is_some_and(|listing_id| listing_id < 1) {
            return Err(ListingVerificationError::Validation(
                "listing_id must be a positive integer".to_string(),
            ));
        }
        if self
            .after_listing_id
            .is_some_and(|listing_id| listing_id < 1)
        {
            return Err(ListingVerificationError::Validation(
                "after_listing_id must be a positive integer".to_string(),
            ));
        }
        if self.listing_id.is_some() && self.after_listing_id.is_some() {
            return Err(ListingVerificationError::Validation(
                "listing_id and after_listing_id are mutually exclusive".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub struct ListingVerificationServices<'a> {
    pub extractor: Option<&'a GeminiListingExtractor>,
    pub aircraft: Option<AircraftVerificationServices<'a>>,
}

impl<'a> ListingVerificationServices<'a> {
    pub fn unavailable() -> Self {
        Self {
            extractor: None,
            aircraft: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListingVerificationStage {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub gemini_used: bool,
    pub catalog_writes: usize,
}

impl ListingVerificationStage {
    fn new(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            reason_code: None,
            reason: None,
            gemini_used: false,
            catalog_writes: 0,
        }
    }

    fn blocked(
        status: impl Into<String>,
        reason_code: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            status: status.into(),
            reason_code: Some(reason_code.into()),
            reason: Some(reason.into()),
            gemini_used: false,
            catalog_writes: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListingAvionicsVerificationStage {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub accepted: usize,
    pub safely_discarded: usize,
    pub remaining_review_aspects: usize,
    pub gemini_used: bool,
}

impl ListingAvionicsVerificationStage {
    fn no_pending_review() -> Self {
        Self {
            status: "already_complete".to_string(),
            reason_code: None,
            reason: None,
            accepted: 0,
            safely_discarded: 0,
            remaining_review_aspects: 0,
            gemini_used: false,
        }
    }

    fn skipped(
        reason_code: impl Into<String>,
        reason: impl Into<String>,
        remaining: usize,
    ) -> Self {
        Self {
            status: "skipped".to_string(),
            reason_code: Some(reason_code.into()),
            reason: Some(reason.into()),
            accepted: 0,
            safely_discarded: 0,
            remaining_review_aspects: remaining,
            gemini_used: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListingVerificationOutcome {
    pub listing_id: i64,
    pub status: String,
    pub initial_ingestion_state: String,
    pub final_ingestion_state: String,
    pub aircraft: ListingVerificationStage,
    pub avionics: ListingAvionicsVerificationStage,
    pub reference: ListingReferenceVerificationStage,
    pub finalization: ListingVerificationStage,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListingReferenceVerificationStage {
    pub status: String,
    pub configuration_version_id: Option<i64>,
    pub configuration_name: Option<String>,
    pub building_version_count: i64,
    pub gaps: Vec<ReferenceGap>,
}

fn reference_stage(status: &ListingReferenceStatus) -> ListingReferenceVerificationStage {
    ListingReferenceVerificationStage {
        status: if status.ready {
            "ready"
        } else {
            "pending_reference"
        }
        .to_string(),
        configuration_version_id: status.published.as_ref().map(|value| value.version_id),
        configuration_name: status
            .published
            .as_ref()
            .map(|value| value.display_name.clone()),
        building_version_count: status.building_version_count,
        gaps: status.gaps.clone(),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ListingVerificationSummary {
    pub listings_selected: usize,
    pub already_verified: usize,
    pub verified: usize,
    pub pending_reference: usize,
    pub pending_review: usize,
    pub blocked: usize,
    pub stale: usize,
    pub failed: usize,
    pub aircraft_verified: usize,
    pub aircraft_pending: usize,
    pub aircraft_rejected: usize,
    pub avionics_accepted: usize,
    pub avionics_safely_discarded: usize,
    pub avionics_remaining_review_aspects: usize,
    pub finalized: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ListingVerificationProviderPlan {
    pub aircraft_grounding_candidates: usize,
    pub avionics: AvionicsProviderRequestPlan,
    pub finalization_enrichment_requests_included: bool,
    pub finalization_note: String,
}

impl ListingVerificationProviderPlan {
    /// Whether executing this exact page can require a provider-backed stage.
    pub fn requires_provider(&self) -> bool {
        self.aircraft_grounding_candidates > 0 || self.avionics.requires_provider()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ListingVerificationReport {
    pub mode: String,
    pub requested_limit: i64,
    pub requested_listing_id: Option<i64>,
    pub requested_after_listing_id: Option<i64>,
    pub checkpoint: AvionicsVerificationCheckpoint,
    pub provider_request_plan: ListingVerificationProviderPlan,
    pub summary: ListingVerificationSummary,
    pub listings: Vec<ListingVerificationOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReviewerListingPreflightContext {
    pub listing_id: i64,
    pub label: String,
    pub registration_number: Option<String>,
    pub model_year: i64,
    pub has_pending_review: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReviewerListingPreflightReport {
    pub verification: ListingVerificationReport,
    pub listing_contexts: Vec<ReviewerListingPreflightContext>,
}

#[derive(Debug)]
pub enum ListingVerificationError {
    Validation(String),
    NotFound(i64),
    Unavailable(String),
    Database(String),
    Aircraft(String),
    Avionics(String),
}

impl fmt::Display for ListingVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message)
            | Self::Unavailable(message)
            | Self::Database(message)
            | Self::Aircraft(message)
            | Self::Avionics(message) => formatter.write_str(message),
            Self::NotFound(listing_id) => write!(formatter, "listing {listing_id} was not found"),
        }
    }
}

impl std::error::Error for ListingVerificationError {}

#[derive(Debug, FromRow)]
struct ListingVerificationState {
    ingestion_state: String,
    is_verified: bool,
    pending_aspect_count: Option<i64>,
}

/// Verify one listing through aircraft, avionics, then publication readiness.
///
/// The apply order is intentionally serial. Earlier catalog decisions may
/// make later work local, and every underlying mutation rechecks its own
/// source/FAA/catalog optimistic guards after network work.
pub async fn verify_listing(
    db: &AppDb,
    listing_id: i64,
    mode: ListingVerificationMode,
    services: ListingVerificationServices<'_>,
) -> Result<ListingVerificationOutcome, ListingVerificationError> {
    if listing_id < 1 {
        return Err(ListingVerificationError::NotFound(listing_id));
    }
    let initial = load_listing_state(db, listing_id).await?;
    let initial_reference = listing_reference_status(db, listing_id)
        .await
        .map_err(|error| ListingVerificationError::Database(error.to_string()))?;
    if initial.ingestion_state == "ready" && initial.is_verified {
        return Ok(ListingVerificationOutcome {
            listing_id,
            status: "already_verified".to_string(),
            initial_ingestion_state: initial.ingestion_state.clone(),
            final_ingestion_state: initial.ingestion_state,
            aircraft: ListingVerificationStage::new("already_verified"),
            avionics: ListingAvionicsVerificationStage::no_pending_review(),
            reference: reference_stage(&initial_reference),
            finalization: ListingVerificationStage::new("ready"),
        });
    }

    let aircraft_preflight = preflight_listing_aircraft_verification(db, listing_id)
        .await
        .map_err(|error| ListingVerificationError::Aircraft(error.to_string()))?;
    let aircraft_paid_attempted = mode != ListingVerificationMode::Preflight
        && matches!(
            &aircraft_preflight,
            AircraftVerificationOutcome::Pending { .. }
        )
        && services.aircraft.is_some();
    let aircraft_outcome = match mode {
        ListingVerificationMode::Preflight => aircraft_preflight,
        ListingVerificationMode::Preview => {
            preview_listing_aircraft_verification(db, listing_id, services.aircraft)
                .await
                .map_err(|error| ListingVerificationError::Aircraft(error.to_string()))?
        }
        ListingVerificationMode::Apply => {
            apply_listing_aircraft_verification(db, listing_id, services.aircraft)
                .await
                .map_err(|error| ListingVerificationError::Aircraft(error.to_string()))?
        }
    };
    let aircraft_rejected = matches!(
        &aircraft_outcome,
        AircraftVerificationOutcome::Rejected { .. }
    );
    let aircraft_verified = aircraft_outcome.is_verified();
    let aircraft = aircraft_stage(&aircraft_outcome, aircraft_paid_attempted);

    let pending_before_avionics = load_listing_state(db, listing_id)
        .await?
        .pending_aspect_count
        .unwrap_or_default()
        .max(0) as usize;
    let avionics = if aircraft_rejected {
        ListingAvionicsVerificationStage::skipped(
            "faa_rejected",
            "Avionics verification is unavailable until the aircraft passes mandatory FAA admission.",
            pending_before_avionics,
        )
    } else {
        let avionics_mode = if mode == ListingVerificationMode::Preview {
            AvionicsVerificationExecutionMode::Preview
        } else {
            AvionicsVerificationExecutionMode::Apply
        };
        let preflight = preflight_listing_avionics(db, listing_id, avionics_mode)
            .await
            .map_err(|error| ListingVerificationError::Avionics(error.to_string()))?;
        let avionics_requires_provider = match &preflight {
            ListingAvionicsVerificationPreflight::NoPendingReview { .. } => false,
            ListingAvionicsVerificationPreflight::PendingReview { report } => {
                provider_request_plan_for_listing_preflights(std::slice::from_ref(report))
                    .requires_provider()
            }
        };
        match mode {
            ListingVerificationMode::Preflight => {
                avionics_preflight_stage(preflight, pending_before_avionics)
            }
            ListingVerificationMode::Preview | ListingVerificationMode::Apply
                if matches!(
                    &preflight,
                    ListingAvionicsVerificationPreflight::NoPendingReview { .. }
                ) =>
            {
                ListingAvionicsVerificationStage::no_pending_review()
            }
            ListingVerificationMode::Apply if !avionics_preflight_is_runnable(&preflight) => {
                avionics_preflight_stage(preflight, pending_before_avionics)
            }
            ListingVerificationMode::Preview | ListingVerificationMode::Apply => {
                if avionics_requires_provider && services.extractor.is_none() {
                    return Err(ListingVerificationError::Unavailable(
                        "automatic avionics verification requires configured Gemini services"
                            .to_string(),
                    ));
                }
                let execution_mode = if mode == ListingVerificationMode::Apply {
                    AvionicsVerificationExecutionMode::Apply
                } else {
                    AvionicsVerificationExecutionMode::Preview
                };
                let usage_before = listing_gemini_usage_count(db, listing_id).await.ok();
                let outcome =
                    verify_listing_avionics(db, services.extractor, execution_mode, listing_id)
                        .await
                        .map_err(|error| ListingVerificationError::Avionics(error.to_string()))?;
                let usage_after = listing_gemini_usage_count(db, listing_id).await.ok();
                avionics_stage(
                    outcome,
                    gemini_used_from_usage_delta(usage_before, usage_after),
                )
            }
        }
    };

    let state_after_avionics = load_listing_state(db, listing_id).await?;
    let review_work_complete =
        aircraft_verified && state_after_avionics.pending_aspect_count.is_none();
    let mut finalization = ListingVerificationStage::new("not_attempted");
    if mode == ListingVerificationMode::Apply && review_work_complete {
        match finalize_reviewed_listing_ingestion(db, listing_id).await {
            Ok(ListingFinalizationOutcome::Ready) => {
                finalization = ListingVerificationStage::new("ready");
            }
            Err(error) => {
                finalization = ListingVerificationStage::blocked(
                    "failed",
                    "listing_finalization_failed",
                    error.to_string(),
                );
            }
        }
    } else if mode != ListingVerificationMode::Apply && review_work_complete {
        finalization = ListingVerificationStage::new("ready");
    } else if mode == ListingVerificationMode::Apply {
        finalization = ListingVerificationStage::blocked(
            "pending",
            if aircraft_verified {
                "avionics_review_remaining"
            } else {
                "aircraft_verification_remaining"
            },
            if aircraft_verified {
                "The listing still has unresolved avionics observations."
            } else {
                "The listing still lacks a verified FAA-backed aircraft identity."
            },
        );
    }

    let final_state = load_listing_state(db, listing_id).await?;
    let final_reference = listing_reference_status(db, listing_id)
        .await
        .map_err(|error| ListingVerificationError::Database(error.to_string()))?;
    let status = listing_outcome_status(&final_state, &finalization, aircraft_rejected);
    Ok(ListingVerificationOutcome {
        listing_id,
        status: status.to_string(),
        initial_ingestion_state: initial.ingestion_state,
        final_ingestion_state: final_state.ingestion_state,
        aircraft,
        avionics,
        reference: reference_stage(&final_reference),
        finalization,
    })
}

fn listing_outcome_status(
    final_state: &ListingVerificationState,
    finalization: &ListingVerificationStage,
    aircraft_rejected: bool,
) -> &'static str {
    if final_state.ingestion_state == "ready" && final_state.is_verified {
        "verified"
    } else if finalization.status == "failed" {
        "failed"
    } else if aircraft_rejected {
        "blocked"
    } else {
        "pending_review"
    }
}

/// Verify a keyset page sequentially. Preflight is provider- and write-free;
/// preview permits paid calls but no domain writes; apply permits guarded
/// domain writes and listing finalization.
pub async fn verify_listings(
    db: &AppDb,
    mode: ListingVerificationMode,
    scope: &ListingVerificationScope,
    services: ListingVerificationServices<'_>,
) -> Result<ListingVerificationReport, ListingVerificationError> {
    scope.validate()?;
    let page = load_listing_id_page(db, scope).await?;
    if scope.listing_id.is_some() && page.listing_ids.is_empty() {
        return Err(ListingVerificationError::NotFound(
            scope.listing_id.unwrap_or_default(),
        ));
    }

    verify_listing_page(
        db,
        mode,
        scope.limit,
        scope.listing_id,
        scope.after_listing_id,
        page,
        services,
    )
    .await
}

/// Run a provider- and write-free verification preflight over listings owned
/// by one authenticated reviewer.
///
/// Ownership is a required server-supplied argument rather than client query
/// state. The function deliberately exposes neither a mode nor provider
/// services, so web callers cannot accidentally widen this into the
/// administrative preview/apply workflow.
pub async fn preflight_reviewer_listing_verifications(
    db: &AppDb,
    owner_user_id: i64,
    scope: &ReviewerListingPreflightScope,
) -> Result<ReviewerListingPreflightReport, ListingVerificationError> {
    if owner_user_id < 1 {
        return Err(ListingVerificationError::Validation(
            "owner_user_id must be a positive integer".to_string(),
        ));
    }
    scope.validate()?;
    let owner_page = load_reviewer_listing_page(db, owner_user_id, scope).await?;
    if scope.listing_id.is_some() && owner_page.contexts.is_empty() {
        return Err(ListingVerificationError::NotFound(
            scope.listing_id.unwrap_or_default(),
        ));
    }
    let contexts = owner_page.contexts;
    let page = ListingIdPage {
        listing_ids: contexts.iter().map(|context| context.listing_id).collect(),
        has_more: owner_page.has_more,
    };
    let verification = verify_listing_page(
        db,
        ListingVerificationMode::Preflight,
        scope.limit,
        scope.listing_id,
        scope.after_listing_id,
        page,
        ListingVerificationServices::unavailable(),
    )
    .await?;
    Ok(ReviewerListingPreflightReport {
        verification,
        listing_contexts: contexts,
    })
}

async fn verify_listing_page(
    db: &AppDb,
    mode: ListingVerificationMode,
    requested_limit: i64,
    requested_listing_id: Option<i64>,
    requested_after_listing_id: Option<i64>,
    page: ListingIdPage,
    services: ListingVerificationServices<'_>,
) -> Result<ListingVerificationReport, ListingVerificationError> {
    let mut aircraft_grounding_candidates = 0;
    let mut avionics_preflights = Vec::new();
    for &listing_id in &page.listing_ids {
        if let Ok(AircraftVerificationOutcome::Pending { reason_code, .. }) =
            preflight_listing_aircraft_verification(db, listing_id).await
        {
            aircraft_grounding_candidates += usize::from(reason_code == "grounding_required");
        }
        let avionics_mode = if mode == ListingVerificationMode::Preview {
            AvionicsVerificationExecutionMode::Preview
        } else {
            AvionicsVerificationExecutionMode::Apply
        };
        if let Ok(ListingAvionicsVerificationPreflight::PendingReview { report }) =
            preflight_listing_avionics(db, listing_id, avionics_mode).await
        {
            avionics_preflights.push(report);
        }
    }
    let avionics_provider_plan = provider_request_plan_for_listing_preflights(&avionics_preflights);

    let mut listings = Vec::with_capacity(page.listing_ids.len());
    for listing_id in &page.listing_ids {
        let initial = load_listing_state(db, *listing_id).await?;
        match verify_listing(db, *listing_id, mode, services).await {
            Ok(outcome) => listings.push(outcome),
            Err(error) => {
                let final_state = load_listing_state(db, *listing_id).await?;
                listings.push(failed_listing_outcome(
                    *listing_id,
                    initial.ingestion_state,
                    final_state.ingestion_state,
                    error,
                ));
            }
        }
    }
    let summary = summarize(&listings);
    Ok(ListingVerificationReport {
        mode: mode.label().to_string(),
        requested_limit,
        requested_listing_id,
        requested_after_listing_id,
        checkpoint: page.checkpoint(requested_after_listing_id),
        provider_request_plan: ListingVerificationProviderPlan {
            aircraft_grounding_candidates,
            avionics: avionics_provider_plan,
            finalization_enrichment_requests_included: false,
            finalization_note: "Listing finalization is local and depends only on current FAA identity plus the approved avionics product graph. Published aircraft references are reported separately as valuation availability and never curated implicitly during finalization."
                .to_string(),
        },
        summary,
        listings,
    })
}

#[derive(Debug, FromRow)]
struct ReviewerListingPreflightContextRow {
    listing_id: i64,
    registration_number: Option<String>,
    model_year: i64,
    has_pending_review: bool,
    canonical_make: Option<String>,
    canonical_designation: Option<String>,
    canonical_generation: Option<String>,
    canonical_package: Option<String>,
    legacy_manufacturer: String,
    legacy_model: String,
    legacy_variant: String,
}

impl ReviewerListingPreflightContextRow {
    fn into_context(self) -> ReviewerListingPreflightContext {
        let canonical = [
            self.canonical_make.as_deref(),
            self.canonical_designation.as_deref(),
            self.canonical_generation.as_deref(),
            self.canonical_package.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
        let legacy = [
            self.legacy_manufacturer.trim(),
            self.legacy_model.trim(),
            self.legacy_variant.trim(),
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
        let label = if canonical.len() >= 2 {
            canonical.join(" ")
        } else if !legacy.is_empty() {
            legacy.join(" ")
        } else {
            format!("Listing {}", self.listing_id)
        };
        ReviewerListingPreflightContext {
            listing_id: self.listing_id,
            label,
            registration_number: self.registration_number,
            model_year: self.model_year,
            has_pending_review: self.has_pending_review,
        }
    }
}

struct ReviewerListingPage {
    contexts: Vec<ReviewerListingPreflightContext>,
    has_more: bool,
}

async fn load_reviewer_listing_page(
    db: &AppDb,
    owner_user_id: i64,
    scope: &ReviewerListingPreflightScope,
) -> Result<ReviewerListingPage, ListingVerificationError> {
    let keyset_predicate = if scope.listing_id.is_some() {
        "AND listing.id = ?"
    } else if scope.after_listing_id.is_some() {
        "AND listing.id > ?"
    } else {
        ""
    };
    let work_predicate = if scope.listing_id.is_some() {
        ""
    } else {
        r#"
        AND (
          listing.ingestion_state <> 'ready'
          OR listing.is_verified = FALSE
          OR EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_reviews pending
            WHERE pending.listing_id = listing.id
          )
        )
        "#
    };
    let statement = format!(
        r#"
        SELECT
          listing.id AS listing_id,
          listing.registration_number,
          listing.model_year,
          EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_reviews pending
            WHERE pending.listing_id = listing.id
          ) AS has_pending_review,
          canonical_make.name AS canonical_make,
          designation.official_designation AS canonical_designation,
          generation.name AS canonical_generation,
          package.name AS canonical_package,
          manufacturer.name AS legacy_manufacturer,
          model.name AS legacy_model,
          variant.name AS legacy_variant
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = model.aircraft_manufacturer_id
        LEFT JOIN aircraft_sale_listing_current_identity_assignments current_assignment
          ON current_assignment.aircraft_sale_listing_id = listing.id
        LEFT JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = current_assignment.identity_assignment_id
         AND assignment.aircraft_sale_listing_id = listing.id
        LEFT JOIN aircraft_makes canonical_make
          ON canonical_make.id = assignment.aircraft_make_id
        LEFT JOIN aircraft_designations designation
          ON designation.id = assignment.aircraft_designation_id
        LEFT JOIN aircraft_generations generation
          ON generation.id = assignment.aircraft_generation_id
        LEFT JOIN aircraft_factory_packages package
          ON package.id = assignment.aircraft_factory_package_id
        WHERE listing.created_by_user_id = ?
          {keyset_predicate}
          {work_predicate}
        ORDER BY listing.id
        LIMIT ?
        "#
    );
    let sql = db.sql(&statement);
    let fetch_limit = scope.limit.saturating_add(1);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let query =
                sqlx::query_as::<_, ReviewerListingPreflightContextRow>(&sql).bind(owner_user_id);
            if let Some(listing_id) = scope.listing_id {
                query
                    .bind(listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await
            } else if let Some(after_listing_id) = scope.after_listing_id {
                query
                    .bind(after_listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await
            } else {
                query.bind(fetch_limit).fetch_all(pool).await
            }
        }
        DatabaseBackend::Postgres(pool) => {
            let query =
                sqlx::query_as::<_, ReviewerListingPreflightContextRow>(&sql).bind(owner_user_id);
            if let Some(listing_id) = scope.listing_id {
                query
                    .bind(listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await
            } else if let Some(after_listing_id) = scope.after_listing_id {
                query
                    .bind(after_listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await
            } else {
                query.bind(fetch_limit).fetch_all(pool).await
            }
        }
    }
    .map_err(|error| ListingVerificationError::Database(error.to_string()))?;
    let mut contexts = rows
        .into_iter()
        .map(ReviewerListingPreflightContextRow::into_context)
        .collect::<Vec<_>>();
    let has_more = contexts.len() as i64 > scope.limit;
    if has_more {
        contexts.truncate(scope.limit as usize);
    }
    Ok(ReviewerListingPage { contexts, has_more })
}

struct ListingIdPage {
    listing_ids: Vec<i64>,
    has_more: bool,
}

impl ListingIdPage {
    fn checkpoint(
        &self,
        requested_after_listing_id: Option<i64>,
    ) -> AvionicsVerificationCheckpoint {
        let page_first_listing_id = self.listing_ids.first().copied();
        let page_last_listing_id = self.listing_ids.last().copied();
        AvionicsVerificationCheckpoint {
            requested_after_listing_id,
            page_first_listing_id,
            page_last_listing_id,
            resume_after_listing_id: page_last_listing_id.or(requested_after_listing_id),
            has_more: self.has_more,
        }
    }
}

async fn load_listing_id_page(
    db: &AppDb,
    scope: &ListingVerificationScope,
) -> Result<ListingIdPage, ListingVerificationError> {
    let predicate = if scope.listing_id.is_some() {
        "WHERE listing.id = ?"
    } else if scope.after_listing_id.is_some() {
        r#"
        WHERE listing.id > ?
          AND (
            listing.ingestion_state <> 'ready'
            OR listing.is_verified = FALSE
            OR EXISTS (
              SELECT 1
              FROM aircraft_sale_listing_pending_reviews review
              WHERE review.listing_id = listing.id
            )
          )
        "#
    } else {
        r#"
        WHERE listing.ingestion_state <> 'ready'
           OR listing.is_verified = FALSE
           OR EXISTS (
             SELECT 1
             FROM aircraft_sale_listing_pending_reviews review
             WHERE review.listing_id = listing.id
           )
        "#
    };
    let statement = format!(
        r#"
        SELECT listing.id
        FROM aircraft_sale_listings listing
        {predicate}
        ORDER BY listing.id
        LIMIT ?
        "#
    );
    let sql = db.sql(&statement);
    let fetch_limit = scope.limit.saturating_add(1);
    let mut listing_ids = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let query = sqlx::query_scalar::<_, i64>(&sql);
            if let Some(listing_id) = scope.listing_id {
                query
                    .bind(listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await
            } else if let Some(after_listing_id) = scope.after_listing_id {
                query
                    .bind(after_listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await
            } else {
                query.bind(fetch_limit).fetch_all(pool).await
            }
        }
        DatabaseBackend::Postgres(pool) => {
            let query = sqlx::query_scalar::<_, i64>(&sql);
            if let Some(listing_id) = scope.listing_id {
                query
                    .bind(listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await
            } else if let Some(after_listing_id) = scope.after_listing_id {
                query
                    .bind(after_listing_id)
                    .bind(fetch_limit)
                    .fetch_all(pool)
                    .await
            } else {
                query.bind(fetch_limit).fetch_all(pool).await
            }
        }
    }
    .map_err(|error| ListingVerificationError::Database(error.to_string()))?;
    let has_more = listing_ids.len() as i64 > scope.limit;
    if has_more {
        listing_ids.truncate(scope.limit as usize);
    }
    Ok(ListingIdPage {
        listing_ids,
        has_more,
    })
}

fn failed_listing_outcome(
    listing_id: i64,
    initial_ingestion_state: String,
    final_ingestion_state: String,
    error: ListingVerificationError,
) -> ListingVerificationOutcome {
    let reason = error.to_string();
    ListingVerificationOutcome {
        listing_id,
        status: "failed".to_string(),
        initial_ingestion_state,
        final_ingestion_state,
        aircraft: ListingVerificationStage::blocked(
            "unknown",
            "automatic_verification_failed",
            reason.clone(),
        ),
        avionics: ListingAvionicsVerificationStage {
            status: "unknown".to_string(),
            reason_code: Some("automatic_verification_failed".to_string()),
            reason: Some(reason.clone()),
            accepted: 0,
            safely_discarded: 0,
            remaining_review_aspects: 0,
            gemini_used: false,
        },
        reference: ListingReferenceVerificationStage {
            status: "unknown".to_string(),
            configuration_version_id: None,
            configuration_name: None,
            building_version_count: 0,
            gaps: Vec::new(),
        },
        finalization: ListingVerificationStage::blocked(
            "failed",
            "automatic_verification_failed",
            reason,
        ),
    }
}

fn aircraft_stage(
    outcome: &AircraftVerificationOutcome,
    paid_attempted: bool,
) -> ListingVerificationStage {
    match outcome {
        AircraftVerificationOutcome::Verified {
            method,
            catalog_writes,
            ..
        } => ListingVerificationStage {
            status: match method {
                AircraftVerificationMethod::CurrentAssignment => "current",
                AircraftVerificationMethod::ApprovedCatalog => "assigned",
                AircraftVerificationMethod::GroundedCuration => "curated",
            }
            .to_string(),
            reason_code: None,
            reason: None,
            gemini_used: *method == AircraftVerificationMethod::GroundedCuration,
            catalog_writes: *catalog_writes,
        },
        AircraftVerificationOutcome::LocallyAssignable { .. } => {
            ListingVerificationStage::blocked(
                "locally_assignable",
                "aircraft_assignment_ready",
                "The aircraft matches one approved catalog identity and can be assigned without Gemini.",
            )
        }
        AircraftVerificationOutcome::GroundingPreview {
            ready_to_apply,
            provider_request_count,
            validation_errors,
            ..
        } => ListingVerificationStage {
            status: if *ready_to_apply {
                "preview_ready".to_string()
            } else {
                "pending".to_string()
            },
            reason_code: (!*ready_to_apply).then(|| "curation_not_reviewable".to_string()),
            reason: (!validation_errors.is_empty()).then(|| validation_errors.join("; ")),
            gemini_used: *provider_request_count > 0,
            catalog_writes: 0,
        },
        AircraftVerificationOutcome::Pending {
            reason_code,
            detail,
            ..
        } => ListingVerificationStage {
            status: "pending".to_string(),
            reason_code: Some((*reason_code).to_string()),
            reason: Some(detail.clone()),
            gemini_used: paid_attempted,
            catalog_writes: 0,
        },
        AircraftVerificationOutcome::Rejected { reason_code, .. } => ListingVerificationStage {
            status: "rejected".to_string(),
            reason_code: Some(reason_code.clone()),
            reason: Some("The aircraft did not pass mandatory FAA admission.".to_string()),
            gemini_used: false,
            catalog_writes: 0,
        },
    }
}

fn avionics_preflight_stage(
    outcome: ListingAvionicsVerificationPreflight,
    pending_aspects: usize,
) -> ListingAvionicsVerificationStage {
    match outcome {
        ListingAvionicsVerificationPreflight::NoPendingReview { .. } => {
            ListingAvionicsVerificationStage::no_pending_review()
        }
        ListingAvionicsVerificationPreflight::PendingReview { report } => {
            let reason_code = match report.status.as_str() {
                "faa_rejected" => "faa_rejected",
                "ready_retained_observations" | "ready_legacy_reextraction" => {
                    "automatic_verification_available"
                }
                _ => "automatic_verification_blocked",
            };
            ListingAvionicsVerificationStage {
                status: report.status,
                reason_code: Some(reason_code.to_string()),
                reason: Some(report.note),
                accepted: 0,
                safely_discarded: 0,
                remaining_review_aspects: pending_aspects,
                gemini_used: false,
            }
        }
    }
}

fn avionics_preflight_is_runnable(outcome: &ListingAvionicsVerificationPreflight) -> bool {
    match outcome {
        ListingAvionicsVerificationPreflight::NoPendingReview { .. } => false,
        ListingAvionicsVerificationPreflight::PendingReview { report } => matches!(
            report.status.as_str(),
            "ready_retained_observations" | "ready_legacy_reextraction"
        ),
    }
}

fn avionics_stage(
    outcome: ListingAvionicsVerification,
    gemini_used: bool,
) -> ListingAvionicsVerificationStage {
    match outcome {
        ListingAvionicsVerification::NoPendingReview { .. } => {
            ListingAvionicsVerificationStage::no_pending_review()
        }
        ListingAvionicsVerification::Processed { report } => {
            let reason_code = match report.status.as_str() {
                "faa_rejected" => Some("faa_rejected".to_string()),
                "missing_source" => Some("source_unavailable".to_string()),
                "blocked" => Some("manual_review_required".to_string()),
                "error" => Some("automatic_verification_failed".to_string()),
                _ if report.remaining_review_aspects > 0 => {
                    Some("manual_review_required".to_string())
                }
                _ => None,
            };
            ListingAvionicsVerificationStage {
                status: report.status,
                reason_code,
                reason: report.error,
                accepted: report.accepted,
                safely_discarded: report.safely_discarded,
                remaining_review_aspects: report.remaining_review_aspects,
                gemini_used,
            }
        }
    }
}

fn gemini_used_from_usage_delta(usage_before: Option<i64>, usage_after: Option<i64>) -> bool {
    match (usage_before, usage_after) {
        (Some(before), Some(after)) if after >= before => after > before,
        _ => true,
    }
}

fn summarize(listings: &[ListingVerificationOutcome]) -> ListingVerificationSummary {
    let mut summary = ListingVerificationSummary {
        listings_selected: listings.len(),
        ..ListingVerificationSummary::default()
    };
    for listing in listings {
        match listing.status.as_str() {
            "already_verified" => summary.already_verified += 1,
            "verified" => summary.verified += 1,
            "pending_review" => summary.pending_review += 1,
            "blocked" => summary.blocked += 1,
            "stale" => summary.stale += 1,
            _ => summary.failed += 1,
        }
        if listing.reference.status == "pending_reference" {
            summary.pending_reference += 1;
        }
        match listing.aircraft.status.as_str() {
            "current" | "assigned" | "curated" | "already_verified" => {
                summary.aircraft_verified += 1;
            }
            "rejected" => summary.aircraft_rejected += 1,
            _ => summary.aircraft_pending += 1,
        }
        summary.avionics_accepted += listing.avionics.accepted;
        summary.avionics_safely_discarded += listing.avionics.safely_discarded;
        summary.avionics_remaining_review_aspects += listing.avionics.remaining_review_aspects;
        if listing.finalization.status == "ready" {
            summary.finalized += 1;
        }
    }
    summary
}

async fn load_listing_state(
    db: &AppDb,
    listing_id: i64,
) -> Result<ListingVerificationState, ListingVerificationError> {
    let sql = db.sql(
        r#"
        SELECT
          listing.ingestion_state,
          listing.is_verified,
          review.pending_aspect_count
        FROM aircraft_sale_listings listing
        LEFT JOIN aircraft_sale_listing_pending_reviews review
          ON review.listing_id = listing.id
        WHERE listing.id = ?
        "#,
    );
    let state = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ListingVerificationState>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ListingVerificationState>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|error| ListingVerificationError::Database(error.to_string()))?;
    state.ok_or(ListingVerificationError::NotFound(listing_id))
}

async fn listing_gemini_usage_count(
    db: &AppDb,
    listing_id: i64,
) -> Result<i64, ListingVerificationError> {
    let sql = db.sql("SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?");
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar(&sql)
                .bind(listing_id)
                .fetch_one(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar(&sql)
                .bind(listing_id)
                .fetch_one(pool)
                .await
        }
    }
    .map_err(|error| ListingVerificationError::Database(error.to_string()))
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::DEVELOPER_EMAIL;

    fn sqlite_pool(db: &AppDb) -> &SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("listing verification test database is not SQLite");
        };
        pool
    }

    async fn insert_pending_listing(db: &AppDb) -> i64 {
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
              user.id,
              'https://example.test/pending-listing',
              2024,
              500000,
              'pending_review',
              'N4242T',
              250
            FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
            JOIN users user ON user.email = ?
            WHERE placeholder.singleton_id = 1
            RETURNING id
            "#,
        )
        .bind(DEVELOPER_EMAIL)
        .fetch_one(sqlite_pool(db))
        .await
        .unwrap()
    }

    async fn developer_user_id(db: &AppDb) -> i64 {
        sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
            .bind(DEVELOPER_EMAIL)
            .fetch_one(sqlite_pool(db))
            .await
            .unwrap()
    }

    async fn insert_test_user(db: &AppDb, suffix: &str) -> i64 {
        sqlx::query_scalar(
            r#"
            INSERT INTO users (email, display_name, auth_provider, auth_subject)
            VALUES (?, ?, 'local', ?)
            RETURNING id
            "#,
        )
        .bind(format!("{suffix}@example.test"))
        .bind(format!("Test {suffix}"))
        .bind(format!("test:{suffix}"))
        .fetch_one(sqlite_pool(db))
        .await
        .unwrap()
    }

    async fn insert_owner_listing(
        db: &AppDb,
        owner_user_id: i64,
        ingestion_state: &str,
        suffix: &str,
    ) -> i64 {
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
              ?,
              ?,
              250
            FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
            WHERE placeholder.singleton_id = 1
            RETURNING id
            "#,
        )
        .bind(owner_user_id)
        .bind(format!("https://example.test/{suffix}"))
        .bind(ingestion_state)
        .bind(format!("N{suffix}"))
        .fetch_one(sqlite_pool(db))
        .await
        .unwrap()
    }

    async fn verification_write_counts(db: &AppDb) -> Vec<i64> {
        let mut counts = Vec::new();
        for table in [
            "aircraft_sale_listings",
            "aircraft_sale_listing_current_identity_assignments",
            "aircraft_sale_listing_avionics",
            "aircraft_sale_listing_pending_reviews",
            "aircraft_identity_decisions",
            "avionics_models",
            "gemini_api_usage",
        ] {
            counts.push(
                sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(sqlite_pool(db))
                    .await
                    .unwrap(),
            );
        }
        counts
    }

    #[test]
    fn scope_rejects_invalid_or_ambiguous_keysets() {
        assert!(matches!(
            ListingVerificationScope::new(0, None, None).validate(),
            Err(ListingVerificationError::Validation(_))
        ));
        assert!(matches!(
            ListingVerificationScope::new(1, Some(1), Some(2)).validate(),
            Err(ListingVerificationError::Validation(_))
        ));
    }

    #[test]
    fn provider_plan_requires_services_for_aircraft_or_avionics_work() {
        let mut plan = ListingVerificationProviderPlan {
            aircraft_grounding_candidates: 0,
            avionics: AvionicsProviderRequestPlan::default(),
            finalization_enrichment_requests_included: false,
            finalization_note: String::new(),
        };
        assert!(!plan.requires_provider());

        plan.aircraft_grounding_candidates = 1;
        assert!(plan.requires_provider());

        plan.aircraft_grounding_candidates = 0;
        plan.avionics
            .known_total_provider_requests_validation_envelope_maximum = 1;
        assert!(plan.requires_provider());
    }

    #[tokio::test]
    async fn batch_preflight_is_provider_free_and_reports_faa_blockers() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = insert_pending_listing(&db).await;

        let report = verify_listings(
            &db,
            ListingVerificationMode::Preflight,
            &ListingVerificationScope::new(10, None, None),
            ListingVerificationServices::unavailable(),
        )
        .await
        .unwrap();

        assert_eq!(report.listings.len(), 1);
        assert_eq!(report.listings[0].listing_id, listing_id);
        assert_eq!(report.listings[0].status, "blocked");
        assert_eq!(report.listings[0].aircraft.status, "rejected");
        assert!(!report.listings[0].aircraft.gemini_used);
        assert!(!report.listings[0].avionics.gemini_used);
        assert_eq!(
            report
                .provider_request_plan
                .avionics
                .known_total_provider_requests_minimum_baseline,
            0
        );
    }

    #[tokio::test]
    async fn keyset_checkpoint_advances_past_a_blocked_listing() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = insert_pending_listing(&db).await;

        let page = verify_listings(
            &db,
            ListingVerificationMode::Preflight,
            &ListingVerificationScope::new(10, None, None),
            ListingVerificationServices::unavailable(),
        )
        .await
        .unwrap();
        assert_eq!(page.checkpoint.resume_after_listing_id, Some(listing_id));

        let next = verify_listings(
            &db,
            ListingVerificationMode::Preflight,
            &ListingVerificationScope::new(10, None, Some(listing_id)),
            ListingVerificationServices::unavailable(),
        )
        .await
        .unwrap();
        assert!(next.listings.is_empty());
    }

    #[tokio::test]
    async fn reviewer_preflight_is_owner_scoped_and_includes_pending_reference_rows() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner_user_id = developer_user_id(&db).await;
        let foreign_user_id = insert_test_user(&db, "foreign-owner").await;
        let pending_reference_id =
            insert_owner_listing(&db, owner_user_id, "incomplete", "4242T").await;
        insert_owner_listing(&db, foreign_user_id, "pending_review", "4243T").await;

        let report = preflight_reviewer_listing_verifications(
            &db,
            owner_user_id,
            &ReviewerListingPreflightScope::new(100, None, None),
        )
        .await
        .unwrap();

        assert_eq!(report.verification.mode, "preflight");
        assert_eq!(report.verification.listings.len(), 1);
        assert_eq!(
            report.verification.listings[0].listing_id,
            pending_reference_id
        );
        assert_eq!(report.listing_contexts.len(), 1);
        let context = &report.listing_contexts[0];
        assert_eq!(context.listing_id, pending_reference_id);
        assert!(!context.label.trim().is_empty());
        assert_eq!(context.registration_number.as_deref(), Some("N4242T"));
        assert_eq!(context.model_year, 2024);
        assert!(!context.has_pending_review);
    }

    #[tokio::test]
    async fn reviewer_exact_foreign_listing_is_not_found() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner_user_id = developer_user_id(&db).await;
        let foreign_user_id = insert_test_user(&db, "foreign-exact").await;
        let foreign_listing_id =
            insert_owner_listing(&db, foreign_user_id, "incomplete", "4343T").await;

        let error = preflight_reviewer_listing_verifications(
            &db,
            owner_user_id,
            &ReviewerListingPreflightScope::new(100, Some(foreign_listing_id), None),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            ListingVerificationError::NotFound(id) if id == foreign_listing_id
        ));
    }

    #[tokio::test]
    async fn reviewer_preflight_is_provider_and_domain_write_free() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner_user_id = developer_user_id(&db).await;
        insert_owner_listing(&db, owner_user_id, "incomplete", "4444T").await;
        let before = verification_write_counts(&db).await;

        let report = preflight_reviewer_listing_verifications(
            &db,
            owner_user_id,
            &ReviewerListingPreflightScope::new(100, None, None),
        )
        .await
        .unwrap();

        assert!(report
            .verification
            .listings
            .iter()
            .all(|listing| !listing.aircraft.gemini_used && !listing.avionics.gemini_used));
        assert_eq!(verification_write_counts(&db).await, before);
    }

    #[tokio::test]
    async fn reviewer_keyset_checkpoint_skips_foreign_rows() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let owner_user_id = developer_user_id(&db).await;
        let foreign_user_id = insert_test_user(&db, "foreign-keyset").await;
        let first = insert_owner_listing(&db, owner_user_id, "incomplete", "4545T").await;
        let foreign = insert_owner_listing(&db, foreign_user_id, "incomplete", "4546T").await;
        let second = insert_owner_listing(&db, owner_user_id, "incomplete", "4547T").await;
        assert!(first < foreign && foreign < second);

        let first_page = preflight_reviewer_listing_verifications(
            &db,
            owner_user_id,
            &ReviewerListingPreflightScope::new(1, None, None),
        )
        .await
        .unwrap();
        assert_eq!(first_page.verification.listings[0].listing_id, first);
        assert_eq!(
            first_page.verification.checkpoint.resume_after_listing_id,
            Some(first)
        );
        assert!(first_page.verification.checkpoint.has_more);

        let second_page = preflight_reviewer_listing_verifications(
            &db,
            owner_user_id,
            &ReviewerListingPreflightScope::new(1, None, Some(first)),
        )
        .await
        .unwrap();
        assert_eq!(second_page.verification.listings[0].listing_id, second);
        assert_eq!(
            second_page
                .verification
                .checkpoint
                .requested_after_listing_id,
            Some(first)
        );
        assert!(!second_page.verification.checkpoint.has_more);
    }

    #[test]
    fn reviewer_scope_enforces_the_web_page_limit() {
        assert!(
            ReviewerListingPreflightScope::new(REVIEWER_PREFLIGHT_MAX_LIMIT, None, None)
                .validate()
                .is_ok()
        );
        assert!(matches!(
            ReviewerListingPreflightScope::new(REVIEWER_PREFLIGHT_MAX_LIMIT + 1, None, None)
                .validate(),
            Err(ListingVerificationError::Validation(_))
        ));
    }

    #[test]
    fn pending_reference_is_reported_without_blocking_listing_verification() {
        let finalization = ListingVerificationStage::new("ready");
        let listings = [ListingVerificationOutcome {
            listing_id: 42,
            status: "verified".to_string(),
            initial_ingestion_state: "incomplete".to_string(),
            final_ingestion_state: "ready".to_string(),
            aircraft: ListingVerificationStage::new("current"),
            avionics: ListingAvionicsVerificationStage::no_pending_review(),
            reference: ListingReferenceVerificationStage {
                status: "pending_reference".to_string(),
                configuration_version_id: None,
                configuration_name: None,
                building_version_count: 0,
                gaps: Vec::new(),
            },
            finalization,
        }];

        let summary = summarize(&listings);
        assert_eq!(summary.listings_selected, 1);
        assert_eq!(summary.verified, 1);
        assert_eq!(summary.pending_reference, 1);
        assert_eq!(summary.pending_review, 0);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.finalized, 1);
    }

    #[test]
    fn pending_review_still_controls_listing_status() {
        let final_state = ListingVerificationState {
            ingestion_state: "pending_review".to_string(),
            is_verified: false,
            pending_aspect_count: Some(1),
        };
        let finalization = ListingVerificationStage::blocked(
            "pending",
            "avionics_review_remaining",
            "A pending avionics review remains.",
        );
        assert_eq!(
            listing_outcome_status(&final_state, &finalization, false),
            "pending_review"
        );
    }

    #[test]
    fn avionics_gemini_usage_requires_a_new_accounting_row() {
        assert!(!gemini_used_from_usage_delta(Some(4), Some(4)));
        assert!(gemini_used_from_usage_delta(Some(4), Some(5)));
    }

    #[test]
    fn avionics_gemini_usage_fails_safe_when_accounting_is_unavailable_or_regresses() {
        assert!(gemini_used_from_usage_delta(None, Some(5)));
        assert!(gemini_used_from_usage_delta(Some(4), None));
        assert!(gemini_used_from_usage_delta(None, None));
        assert!(gemini_used_from_usage_delta(Some(5), Some(4)));
    }
}
