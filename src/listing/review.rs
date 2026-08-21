//! Human review boundary for listing avionics that could not be admitted
//! automatically.
//!
//! Publication is intentionally separate from extraction. A pending bundle is
//! an immutable, hash-addressed view of the observed avionics and the approved
//! catalog revision against which it was prepared. The server additionally
//! restricts these operations to configured reviewers; this store layer always
//! scopes reads and resolution to the listing owner supplied by the server.

pub(crate) mod automation;
pub(crate) mod replacement;
pub(crate) mod staging;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::aircraft::faa::{block_reason_code, require_listing_admission, AircraftAdmissionError};
use crate::avionics::catalog::{
    exact_product_identity_signal_is_present, ApprovedProductAssociationRequest,
    ApprovedProductAssociationResolver, PendingProductAttestationCommitGuard,
};
pub(super) use crate::avionics::catalog::{
    ActiveCollisionCatalogFingerprintRow, ACTIVE_COLLISION_CATALOG_ROWS_SQL,
};
use crate::avionics::manufacturer::{
    admit_manufacturer_product_scope_postgres, admit_manufacturer_product_scope_sqlite,
    stage_batch_manufacturer_alias_collision_postgres,
    stage_batch_manufacturer_alias_collision_sqlite, ManufacturerIdentityError,
    ManufacturerIdentityEvidence, ManufacturerProductAdmission,
    ManufacturerProductAdmissionOutcome,
};
use crate::avionics::model::avionics_identities_are_typography_exact;
use crate::avionics::reuse::{
    current_reuse_attested_product_ids, refresh_reuse_attestation_postgres,
    refresh_reuse_attestation_sqlite, reuse_attestation_is_current_postgres,
    reuse_attestation_is_current_sqlite, reuse_source_origin_is_authorized,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::CURATED_AVIONICS_TYPES;
use crate::gemini::curation::workflow::MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS;
use crate::html::clean::listing_body_contains_exact_structurally_visible_text_span;
use crate::listing::avionics::disposition::{
    bounded_decision_reason, coordinates_from_aspect_id, extraction_sha256, occurrence_fingerprint,
    DISPOSITION_POLICY_VERSION, INSERT_DISPOSITION_SQL,
};
use crate::listing::avionics::extraction::{
    parse_current_avionics_extraction_json, validate_current_avionics_extraction,
    CurrentAvionicsExtraction,
};
use crate::listing::avionics::{
    approved_avionics_product_key, validate_canonical_avionics_actions, CanonicalAvionicsAction,
};
use crate::listing::evidence::{
    identity_span_has_boundaries, ListingEvidenceContext, MAX_LISTING_EVIDENCE_CONTEXT_BYTES,
};
use crate::listings::get_listing;
use crate::models::SaleListing;
use crate::normalize::{
    is_usable_avionics_label, normalize_avionics_identifier, normalize_avionics_manufacturer_name,
    normalize_avionics_model_name, normalize_name,
};

use self::staging::{
    project_pending_review, reset_requires_reextraction, CatalogProjectionProduct,
    PendingReviewProjection,
};

const DEFAULT_PAGE_LIMIT: i64 = 25;
const MAX_PAGE_LIMIT: i64 = 100;
const REVIEW_PAYLOAD_VERSION: u32 = 1;
const APPROVED_CATALOG_FINGERPRINT_DOMAIN: &[u8] = b"aircost:approved-avionics-catalog:v1";
const APPROVED_CATALOG_PRODUCT_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:approved-avionics-catalog-product:v1";
const ACTIVE_COLLISION_CLOSURE_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:active-avionics-collision-closure:v1";
const GROUNDED_COLLISION_CLOSURE_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:grounded-avionics-collision-closure:v1";
const EXTRACTION_FINGERPRINT_DOMAIN: &[u8] = b"aircost:listing-avionics-observation:v1";
const ASSOCIATION_AUTHORIZATION_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:listing-avionics-association-authorization:v1";
pub(super) const ASSOCIATION_AUTHORIZATION_POLICY_VERSION: &str =
    "listing_avionics_authorization_v1";
const MAX_REVIEW_PRODUCT_IDENTITY_LABEL_CHARACTERS: usize = 128;
const MAX_REVIEW_PRODUCT_SOURCE_TITLE_CHARACTERS: usize = 200;
const MAX_REVIEW_PRODUCT_SOURCE_URL_CHARACTERS: usize = 2_048;
const PRESERVED_ASSOCIATION_REVIEW_REASON: &str =
    "catalog_product_or_listing_corroboration_missing";
const REVIEWER_CORRECTED_AVIONICS_KIND: &str = "avionics_reviewer_correction";
pub(crate) const POSTGRES_LISTING_CHILD_LOCK_SQL: &str = r#"
    LOCK TABLE aircraft_sale_listing_avionics,
               aircraft_sale_listing_avionics_authorizations,
               aircraft_sale_listing_avionics_dispositions,
               aircraft_sale_listing_pending_reviews
    IN SHARE ROW EXCLUSIVE MODE
"#;
pub(crate) const POSTGRES_RESTAGE_CATALOG_LOCK_SQL: &str = r#"
    LOCK TABLE avionics_models,
               avionics_model_types,
               avionics_types,
               avionics_manufacturers,
               avionics_manufacturer_identities,
               avionics_manufacturer_identity_memberships,
               avionics_manufacturer_identity_merges,
               avionics_approved_product_identities,
               avionics_product_reuse_attestations,
               avionics_authoritative_source_origins,
               avionics_authoritative_source_origin_revocations
    IN SHARE ROW EXCLUSIVE MODE
"#;

#[derive(Debug)]
pub enum ReviewError {
    Validation(String),
    Stale(String),
    Conflict(String),
    NotFound(String),
    Permission(String),
    Database(String),
}

impl fmt::Display for ReviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message)
            | Self::Stale(message)
            | Self::Conflict(message)
            | Self::NotFound(message)
            | Self::Permission(message)
            | Self::Database(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for ReviewError {}

impl From<sqlx::Error> for ReviewError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

fn manufacturer_identity_review_error(error: ManufacturerIdentityError) -> ReviewError {
    match error {
        ManufacturerIdentityError::Validation(message) => ReviewError::Validation(message),
        ManufacturerIdentityError::Conflict(message) => ReviewError::Conflict(message),
        ManufacturerIdentityError::Database(message) => ReviewError::Database(message),
    }
}

pub type ReviewResult<T> = Result<T, ReviewError>;

/// Opaque aspect IDs remain stable across JSON/UI round trips. Integer IDs are
/// accepted for older/generated bundles, but strings are preferred by new
/// producers.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ReviewAspectId {
    String(String),
    Integer(i64),
}

impl fmt::Display for ReviewAspectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(value) => formatter.write_str(value),
            Self::Integer(value) => write!(formatter, "{value}"),
        }
    }
}

impl ReviewAspectId {
    fn validate(&self) -> ReviewResult<()> {
        if matches!(self, Self::String(value) if value.trim().is_empty()) {
            return Err(ReviewError::Validation(
                "review aspect IDs cannot be blank".to_string(),
            ));
        }
        Ok(())
    }
}

impl From<String> for ReviewAspectId {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for ReviewAspectId {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<i64> for ReviewAspectId {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    UseVerifiedProduct,
    CreateVerifiedProduct,
    Discard,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StableIdentifier {
    pub kind: String,
    pub value: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingAssociationRole {
    Installed,
    Replacement,
}

/// Exact existing listing-link component adjudicated by an aspect. The link
/// ID prevents a product shared by multiple associations from being removed
/// merely because one occurrence was reviewed.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CoveredListingAssociation {
    pub listing_link_id: i64,
    pub role: ListingAssociationRole,
    pub avionics_model_id: i64,
}

/// Exact listing-link state against which a reviewer correction was made.
/// The corrected quantity and identity live on the aspect; this snapshot keeps
/// concurrent listing mutations detectable until ordinary review resolution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReviewerCorrectionAssociationBinding {
    pub listing_link_id: i64,
    pub avionics_model_id: i64,
    pub quantity: i64,
    pub configuration_action: String,
    pub replaces_avionics_model_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReviewProduct {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub manufacturer: String,
    pub model: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_identifier: Option<StableIdentifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_source_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_evidence_text: Option<String>,
}

impl ReviewProduct {
    pub fn verified(
        id: i64,
        manufacturer: impl Into<String>,
        model: impl Into<String>,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            id: Some(id),
            manufacturer: manufacturer.into(),
            model: model.into(),
            capabilities,
            stable_identifier: None,
            identity_source_url: None,
            identity_source_title: None,
            identity_evidence_text: None,
        }
    }

    pub fn proposed(
        manufacturer: impl Into<String>,
        model: impl Into<String>,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            id: None,
            manufacturer: manufacturer.into(),
            model: model.into(),
            capabilities,
            stable_identifier: None,
            identity_source_url: None,
            identity_source_title: None,
            identity_evidence_text: None,
        }
    }

    /// A specific existing unreviewed catalog row whose identity matched the
    /// staged observation uniquely. The ID is only a promotion candidate: it
    /// is never treated as a verified-product suggestion, and resolution
    /// revalidates the row under the catalog lock before changing it.
    pub fn unreviewed_catalog_candidate(
        id: i64,
        manufacturer: impl Into<String>,
        model: impl Into<String>,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            id: Some(id),
            manufacturer: manufacturer.into(),
            model: model.into(),
            capabilities,
            stable_identifier: None,
            identity_source_url: None,
            identity_source_title: None,
            identity_evidence_text: None,
        }
    }

    pub fn with_stable_identifier(
        mut self,
        kind: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.stable_identifier = Some(StableIdentifier {
            kind: kind.into(),
            value: value.into(),
        });
        self
    }

    pub fn with_identity_evidence(
        mut self,
        source_url: impl Into<String>,
        source_title: impl Into<String>,
        evidence_text: impl Into<String>,
    ) -> Self {
        self.identity_source_url = Some(source_url.into());
        self.identity_source_title = Some(source_title.into());
        self.identity_evidence_text = Some(evidence_text.into());
        self
    }
}

/// Complete persisted aspect. Fields after `allowed_actions` are deliberately
/// omitted from the public detail DTO but retained so review cannot alter the
/// listing evidence or installation semantics.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PendingReviewAspect {
    pub id: ReviewAspectId,
    pub kind: String,
    pub label: String,
    pub observed_text: String,
    pub required: bool,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_product: Option<ReviewProduct>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_product: Option<ReviewProduct>,
    #[serde(default)]
    pub allowed_actions: Vec<ReviewAction>,
    pub quantity: i64,
    pub configuration_action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_evidence_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces_product_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_aspect_id: Option<ReviewAspectId>,
    /// Exact existing listing-link components this aspect adjudicates.
    /// Resolution preserves every association component not covered here,
    /// including associations to the same product on a different link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered_associations: Vec<CoveredListingAssociation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_correction_association_binding: Option<ReviewerCorrectionAssociationBinding>,
    /// Approved catalog product whose hash-bound listing aspect must be
    /// freshly verified before its occurrence is eligible for reuse.
    ///
    /// This is deliberately separate from `proposed_product.id`: that field
    /// identifies only a legacy *unreviewed* promotion candidate. Treating an
    /// approved row as such would route it through the wrong mutation
    /// contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse_attestation_target_id: Option<i64>,
}

impl PendingReviewAspect {
    #[allow(clippy::too_many_arguments)]
    pub fn avionics(
        id: impl Into<ReviewAspectId>,
        kind: impl Into<String>,
        label: impl Into<String>,
        observed_text: impl Into<String>,
        reason: impl Into<String>,
        quantity: i64,
        configuration_action: impl Into<String>,
        source_evidence_text: Option<String>,
        source_confidence: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            label: label.into(),
            observed_text: observed_text.into(),
            required: true,
            reason: reason.into(),
            suggested_product: None,
            proposed_product: None,
            allowed_actions: Vec::new(),
            quantity,
            configuration_action: configuration_action.into(),
            source_evidence_text,
            source_confidence,
            replaces_product_id: None,
            replacement_aspect_id: None,
            covered_associations: Vec::new(),
            reviewer_correction_association_binding: None,
            reuse_attestation_target_id: None,
        }
    }

    pub fn with_suggested_product(mut self, product: ReviewProduct) -> Self {
        self.suggested_product = Some(product);
        self
    }

    pub fn with_proposed_product(mut self, product: ReviewProduct) -> Self {
        self.proposed_product = Some(product);
        self
    }

    pub fn with_replacement_product(mut self, avionics_model_id: i64) -> Self {
        self.replaces_product_id = Some(avionics_model_id);
        self.replacement_aspect_id = None;
        self
    }

    pub fn with_replacement_aspect(mut self, aspect_id: impl Into<ReviewAspectId>) -> Self {
        self.replacement_aspect_id = Some(aspect_id.into());
        self.replaces_product_id = None;
        self
    }

    pub fn with_covered_association(
        mut self,
        listing_link_id: i64,
        role: ListingAssociationRole,
        avionics_model_id: i64,
    ) -> Self {
        let association = CoveredListingAssociation {
            listing_link_id,
            role,
            avionics_model_id,
        };
        if !self.covered_associations.contains(&association) {
            self.covered_associations.push(association);
        }
        self
    }

    pub fn with_reuse_attestation_target(mut self, avionics_model_id: i64) -> Self {
        self.reuse_attestation_target_id = Some(avionics_model_id);
        self
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct PendingReviewPayload {
    version: u32,
    aspects: Vec<PendingReviewAspect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SerializedReviewPayload {
    pub review_payload_json: String,
    pub review_payload_sha256: String,
    pub extraction_sha256: String,
    pub pending_aspect_count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StagedPendingReview {
    pub listing_id: i64,
    pub review_payload_sha256: String,
    pub catalog_revision_sha256: String,
    pub pending_aspect_count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RebuildPendingAvionicsReview {
    Rebuilt {
        #[serde(skip_serializing_if = "Option::is_none")]
        review: Option<StagedPendingReview>,
    },
    Blocked {
        listing_id: i64,
        reason_code: RebuildPendingAvionicsReviewBlockReason,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RebuildPendingAvionicsReviewBlockReason {
    RetainedSourceMissing,
    ExtractionNotCurrent,
    OccurrenceDispositionUnknown,
    UnsupportedReviewState,
}

impl RebuildPendingAvionicsReviewBlockReason {
    pub const fn message(self) -> &'static str {
        match self {
            Self::RetainedSourceMissing => {
                "The retained listing source is unavailable. Capture the listing again before rebuilding its avionics review."
            }
            Self::ExtractionNotCurrent => {
                "The retained extraction does not satisfy the current avionics schema. Run a validated re-extraction before rebuilding its review."
            }
            Self::OccurrenceDispositionUnknown => {
                "At least one retained avionics occurrence has no current review or listing-link disposition. Run a validated re-extraction before rebuilding its review."
            }
            Self::UnsupportedReviewState => {
                "This review includes state outside the avionics workflow. No review state was changed."
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ReviewQueueQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReviewAircraftSummary {
    pub manufacturer: String,
    pub model: String,
    pub variant: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAircraftIdentityState {
    Verified,
    CurationRequired,
}

/// Read-only strict FAA/canonical-identity status at the instant a review is
/// loaded. This never attempts to create or repair an assignment: reviewers
/// must be able to see the aircraft blocker before submitting avionics
/// decisions that the server cannot publish.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReviewAircraftIdentityStatus {
    pub status: ReviewAircraftIdentityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faa_n_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faa_snapshot_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<crate::aircraft::repair::AircraftRepairPreflight>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ListingReviewQueueItem {
    pub listing_id: i64,
    pub label: String,
    pub aircraft: ReviewAircraftSummary,
    pub registration_number: Option<String>,
    pub model_year: i64,
    pub pending_aspect_count: i64,
    pub reason_codes: Vec<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ListingReviewQueue {
    pub reviews: Vec<ListingReviewQueueItem>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductReviewPageQuery {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductAttestationStatus {
    Current,
    Required,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ProductAssociationEligibilityCounts {
    pub ready_local: i64,
    pub source_evidence_missing: i64,
    pub product_attestation_required: i64,
    pub manual_review_required: i64,
}

impl ProductAssociationEligibilityCounts {
    fn record(&mut self, eligibility: &ProductAssociationVerificationEligibility) {
        match eligibility.status {
            ProductAssociationEligibilityStatus::AutoVerifiable => self.ready_local += 1,
            ProductAssociationEligibilityStatus::ProductAttestationRequired => {
                self.product_attestation_required += 1;
            }
            ProductAssociationEligibilityStatus::ManualReviewRequired
                if eligibility.reason_code.as_deref() == Some("source_evidence_missing") =>
            {
                self.source_evidence_missing += 1;
            }
            ProductAssociationEligibilityStatus::ManualReviewRequired => {
                self.manual_review_required += 1;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PendingProductReviewGroup {
    pub product: ReviewProduct,
    pub attestation_status: ProductAttestationStatus,
    pub pending_association_count: i64,
    pub pending_listing_count: i64,
    pub eligibility_counts: ProductAssociationEligibilityCounts,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PendingProductReviewPage {
    pub catalog_revision_sha256: String,
    pub items: Vec<PendingProductReviewGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PreparedPendingProductReviews {
    pub inspected_listing_count: i64,
    pub restaged_listing_count: i64,
    pub catalog_revision_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductAssociationEligibilityStatus {
    AutoVerifiable,
    ProductAttestationRequired,
    ManualReviewRequired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProductAssociationVerificationEligibility {
    pub status: ProductAssociationEligibilityStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PendingProductAssociation {
    pub listing_id: i64,
    pub listing_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub aspect_id: ReviewAspectId,
    pub review_payload_sha256: String,
    pub observed_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_evidence_text: Option<String>,
    pub quantity: i64,
    pub configuration_action: String,
    pub verification_eligibility: ProductAssociationVerificationEligibility,
}

#[derive(Clone, Debug)]
struct PendingProductAssociationSource {
    listing_id: i64,
    listing_label: String,
    source_url: Option<String>,
    aspect_id: ReviewAspectId,
    review_payload_sha256: String,
    observed_text: String,
    source_evidence_text: Option<String>,
    quantity: i64,
    configuration_action: String,
}

impl PendingProductAssociationSource {
    fn project(
        self,
        verification_eligibility: ProductAssociationVerificationEligibility,
    ) -> PendingProductAssociation {
        PendingProductAssociation {
            listing_id: self.listing_id,
            listing_label: self.listing_label,
            source_url: self.source_url,
            aspect_id: self.aspect_id,
            review_payload_sha256: self.review_payload_sha256,
            observed_text: self.observed_text,
            source_evidence_text: self.source_evidence_text,
            quantity: self.quantity,
            configuration_action: self.configuration_action,
            verification_eligibility,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PendingProductAssociationPage {
    pub product: ReviewProduct,
    pub attestation_status: ProductAttestationStatus,
    pub catalog_revision_sha256: String,
    pub associations: Vec<PendingProductAssociation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProductGroupCursor {
    product_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProductAssociationCursor {
    product_id: i64,
    listing_id: i64,
    aspect_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReviewAspectView {
    pub id: ReviewAspectId,
    pub kind: String,
    pub label: String,
    pub observed_text: String,
    pub required: bool,
    pub reason: String,
    pub quantity: i64,
    pub configuration_action: String,
    /// True when a reviewer has saved corrected occurrence values while the
    /// publisher observation and its evidence remain immutable below.
    pub reviewer_corrected: bool,
    /// Existing listing-link graphs stay bound to their staged action and
    /// target. Fresh, unlinked observations may correct the complete action
    /// graph before ordinary product review.
    pub configuration_action_editable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_evidence_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement_aspect_id: Option<ReviewAspectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement_product: Option<ReviewProduct>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_product: Option<ReviewProduct>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_product: Option<ReviewProduct>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reuse_attestation_target: Option<ReviewProduct>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reuse_attestation_status: Option<ProductAttestationStatus>,
    pub allowed_actions: Vec<ReviewAction>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ListingReview {
    pub listing_id: i64,
    pub source_url: Option<String>,
    pub label: String,
    pub aircraft: ReviewAircraftSummary,
    pub aircraft_identity: ReviewAircraftIdentityStatus,
    pub registration_number: Option<String>,
    pub model_year: i64,
    pub review_payload_sha256: String,
    pub catalog_revision_sha256: String,
    pub allowed_capabilities: Vec<String>,
    pub aspects: Vec<ReviewAspectView>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ListingReviewDetail {
    pub review: ListingReview,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewReplacementTarget {
    CatalogProduct { avionics_model_id: i64 },
    ReviewAspect { aspect_id: ReviewAspectId },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviseAvionicsObservationRequest {
    #[serde(rename = "review_payload_sha256")]
    pub expected_review_payload_sha256: String,
    #[serde(rename = "catalog_revision_sha256")]
    pub expected_catalog_revision_sha256: String,
    pub aspect_id: ReviewAspectId,
    pub manufacturer: String,
    pub model: String,
    pub capabilities: Vec<String>,
    pub quantity: i64,
    pub configuration_action: String,
    #[serde(default)]
    pub replacement_target: Option<ReviewReplacementTarget>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResolveReviewRequest {
    #[serde(rename = "review_payload_sha256")]
    pub expected_review_payload_sha256: String,
    #[serde(rename = "catalog_revision_sha256")]
    pub expected_catalog_revision_sha256: String,
    /// Opt in to network-capable enrichment after the review transaction
    /// commits. Omitted requests only save decisions and leave the listing
    /// incomplete and private.
    #[serde(default)]
    pub finalize_listing: bool,
    pub decisions: Vec<ReviewDecision>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ReviewDecision {
    UseVerifiedProduct {
        aspect_id: ReviewAspectId,
        avionics_model_id: i64,
    },
    CreateVerifiedProduct {
        aspect_id: ReviewAspectId,
        /// Existing unreviewed row to promote instead of inserting a duplicate.
        /// The preflight and resolve transaction both require it to match the
        /// submitted and independently grounded identity.
        #[serde(default)]
        unreviewed_avionics_model_id: Option<i64>,
        manufacturer: String,
        model: String,
        capabilities: Vec<String>,
        manufacturer_identifier_kind: String,
        manufacturer_identifier: String,
        identity_source_url: String,
        identity_source_title: String,
        identity_evidence_text: String,
        /// Populated only by the server after grounded adjudication. Client
        /// input is ignored so these URLs remain a trusted write sidecar.
        #[serde(skip)]
        grounded_claim_source_urls: Vec<String>,
    },
    Discard {
        aspect_id: ReviewAspectId,
        reason: String,
    },
}

impl ReviewDecision {
    fn aspect_id(&self) -> &ReviewAspectId {
        match self {
            Self::UseVerifiedProduct { aspect_id, .. }
            | Self::CreateVerifiedProduct { aspect_id, .. }
            | Self::Discard { aspect_id, .. } => aspect_id,
        }
    }

    fn action(&self) -> ReviewAction {
        match self {
            Self::UseVerifiedProduct { .. } => ReviewAction::UseVerifiedProduct,
            Self::CreateVerifiedProduct { .. } => ReviewAction::CreateVerifiedProduct,
            Self::Discard { .. } => ReviewAction::Discard,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolveReviewResponse {
    pub listing: SaleListing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedReview {
    pub listing_id: i64,
}

#[derive(Clone, Debug, FromRow)]
pub(crate) struct CatalogFingerprintRow {
    pub(crate) id: i64,
    pub(crate) manufacturer: String,
    pub(crate) model: String,
    pub(crate) capability: String,
    pub(crate) manufacturer_identifier_kind: Option<String>,
    pub(crate) manufacturer_identifier: Option<String>,
    pub(crate) avionics_manufacturer_identity_id: i64,
    pub(crate) canonical_product_key: String,
    pub(crate) graph_manufacturer_identifier_kind: String,
    pub(crate) canonical_identifier_key: String,
    pub(crate) identity_source_url: Option<String>,
    pub(crate) identity_source_title: Option<String>,
    pub(crate) identity_evidence_text: Option<String>,
}

#[derive(Clone, Debug)]
struct CatalogFingerprintProduct {
    id: i64,
    manufacturer: String,
    model: String,
    capabilities: Vec<String>,
    manufacturer_identifier_kind: String,
    manufacturer_identifier: String,
    avionics_manufacturer_identity_id: i64,
    canonical_product_key: String,
    graph_manufacturer_identifier_kind: String,
    canonical_identifier_key: String,
    identity_source_url: String,
    identity_source_title: String,
    identity_evidence_text: String,
}

#[derive(Debug, FromRow)]
struct QueueRow {
    listing_id: i64,
    model_year: i64,
    registration_number: Option<String>,
    pending_aspect_count: i64,
    review_payload_json: String,
    updated_at: String,
    manufacturer: String,
    model: String,
    variant: String,
}

#[derive(Debug, FromRow)]
struct ProductReviewSourceRow {
    listing_id: i64,
    source_url: Option<String>,
    model_year: i64,
    pending_aspect_count: i64,
    review_payload_json: String,
    review_payload_sha256: String,
    manufacturer: String,
    model: String,
    variant: String,
}

#[derive(Debug, FromRow)]
struct ReviewRow {
    listing_id: i64,
    owner_user_id: i64,
    plugin_submission_id: Option<i64>,
    extracted_listing_json: Option<String>,
    source_url: Option<String>,
    model_year: i64,
    registration_number: Option<String>,
    ingestion_state: String,
    is_verified: bool,
    pending_aspect_count: i64,
    review_payload_json: String,
    review_payload_sha256: String,
    catalog_revision_sha256: String,
    manufacturer: String,
    model: String,
    variant: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ListingEvidenceProvenance {
    plugin_submission_id: i64,
    source_url: String,
    rendered_html_sha256: String,
}

#[derive(Debug, FromRow)]
struct ListingEvidenceCaptureRow {
    plugin_submission_id: i64,
    user_id: i64,
    canonical_listing_id: Option<i64>,
    source_url: String,
    rendered_html: String,
    rendered_html_sha256: String,
}

#[derive(Debug, FromRow)]
struct RebuildSubmissionRow {
    submission_id: i64,
    user_id: i64,
    canonical_listing_id: Option<i64>,
    source_url: String,
    rendered_html: String,
    rendered_html_sha256: String,
    extracted_listing_json: Option<String>,
    extraction_error: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct CatalogProjectionRow {
    id: i64,
    manufacturer: String,
    model: String,
    capability: Option<String>,
    catalog_status: String,
}

#[derive(Clone, Debug, FromRow)]
struct ApprovedProductRow {
    id: i64,
    manufacturer: String,
    model: String,
    capability: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    identity_source_url: Option<String>,
    identity_source_title: Option<String>,
    identity_evidence_text: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct RecoverableCatalogIdentityRow {
    id: i64,
    model: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct ExistingAssignmentRow {
    listing_link_id: i64,
    avionics_model_id: i64,
    installed_manufacturer: Option<String>,
    installed_model: Option<String>,
    replacement_manufacturer: Option<String>,
    replacement_model: Option<String>,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    installed_catalog_status: Option<String>,
    replacement_catalog_status: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct AssociationAuthorizationRow {
    listing_link_id: i64,
    association_role: String,
    avionics_model_id: i64,
    authorization_kind: String,
    observation_sha256: String,
    product_fingerprint: String,
    current_reuse_product_fingerprint: Option<String>,
    grounded_resolution_sha256: Option<String>,
    evidence_capture_is_current: bool,
    policy_version: String,
    collision_closure_sha256: String,
}

#[derive(Debug, FromRow)]
struct ExternalCoveredReferenceRow {
    listing_link_id: i64,
    listing_id: i64,
    association_role: String,
    pending_aspect_count: Option<i64>,
    review_payload_json: Option<String>,
    review_payload_sha256: Option<String>,
}

#[derive(Debug, FromRow)]
struct GlobalCatalogReferenceRow {
    reference_kind: String,
    reference_count: i64,
}

const SELECT_EXTERNAL_LEGACY_REFERENCES_SQL: &str = r#"
    SELECT
      link.id AS listing_link_id,
      link.aircraft_sale_listing_id AS listing_id,
      'installed' AS association_role,
      review.pending_aspect_count,
      review.review_payload_json,
      review.review_payload_sha256
    FROM aircraft_sale_listing_avionics link
    LEFT JOIN aircraft_sale_listing_pending_reviews review
      ON review.listing_id = link.aircraft_sale_listing_id
    WHERE link.avionics_model_id = ?
      AND link.aircraft_sale_listing_id <> ?
    UNION ALL
    SELECT
      link.id AS listing_link_id,
      link.aircraft_sale_listing_id AS listing_id,
      'replacement' AS association_role,
      review.pending_aspect_count,
      review.review_payload_json,
      review.review_payload_sha256
    FROM aircraft_sale_listing_avionics link
    LEFT JOIN aircraft_sale_listing_pending_reviews review
      ON review.listing_id = link.aircraft_sale_listing_id
    WHERE link.replaces_avionics_model_id = ?
      AND link.aircraft_sale_listing_id <> ?
    ORDER BY listing_id, listing_link_id, association_role
"#;

// Listing associations are checked separately, with exact link/role
// coverage. Type memberships are also separate: the create decision
// explicitly replaces them with its reviewed capabilities. These are all
// remaining foreign-key roles whose meaning a listing review cannot
// adjudicate, so none may be inherited by an in-place promotion.
const SELECT_GLOBAL_LEGACY_REFERENCES_SQL: &str = r#"
    SELECT reference_kind, reference_count
    FROM (
      SELECT
        'aircraft_reference_avionics.avionics_model_id' AS reference_kind,
        COUNT(*) AS reference_count
      FROM aircraft_reference_avionics
      WHERE avionics_model_id = ?
      UNION ALL
      SELECT
        'avionics_suite_components.suite_model_id' AS reference_kind,
        COUNT(*) AS reference_count
      FROM avionics_suite_components
      WHERE suite_model_id = ?
      UNION ALL
      SELECT
        'avionics_suite_components.component_model_id' AS reference_kind,
        COUNT(*) AS reference_count
      FROM avionics_suite_components
      WHERE component_model_id = ?
    ) global_reference
    WHERE reference_count > 0
    ORDER BY reference_kind
"#;

#[derive(Debug, FromRow)]
struct CatalogIdentityRow {
    id: i64,
    catalog_status: String,
    manufacturer: String,
    model: String,
    manufacturer_identifier_kind: Option<String>,
    normalized_manufacturer_identifier: Option<String>,
    avionics_manufacturer_identity_id: Option<i64>,
}

const APPROVED_CATALOG_ROWS_SQL: &str = r#"
    SELECT
      model.id,
      manufacturer.name AS manufacturer,
      model.name AS model,
      capability.name AS capability,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier,
      graph.avionics_manufacturer_identity_id,
      graph.canonical_product_key,
      graph.manufacturer_identifier_kind AS graph_manufacturer_identifier_kind,
      graph.canonical_identifier_key,
      model.identity_source_url,
      model.identity_source_title,
      model.identity_evidence_text
    FROM avionics_models model
    JOIN avionics_manufacturers manufacturer
      ON manufacturer.id = model.avionics_manufacturer_id
    JOIN avionics_model_types membership
      ON membership.avionics_model_id = model.id
    JOIN avionics_types capability
      ON capability.id = membership.avionics_type_id
    JOIN avionics_approved_product_graph_identities graph
      ON graph.avionics_model_id = model.id
    WHERE model.catalog_status = 'approved'
    ORDER BY model.id, capability.normalized_name, capability.id
"#;

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn feed_fingerprint(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn catalog_products(rows: Vec<CatalogFingerprintRow>) -> Vec<CatalogFingerprintProduct> {
    let mut products = BTreeMap::<i64, CatalogFingerprintProduct>::new();
    for row in rows {
        let product = products
            .entry(row.id)
            .or_insert_with(|| CatalogFingerprintProduct {
                id: row.id,
                manufacturer: row.manufacturer,
                model: row.model,
                capabilities: Vec::new(),
                manufacturer_identifier_kind: row.manufacturer_identifier_kind.unwrap_or_default(),
                manufacturer_identifier: row.manufacturer_identifier.unwrap_or_default(),
                avionics_manufacturer_identity_id: row.avionics_manufacturer_identity_id,
                canonical_product_key: row.canonical_product_key,
                graph_manufacturer_identifier_kind: row.graph_manufacturer_identifier_kind,
                canonical_identifier_key: row.canonical_identifier_key,
                identity_source_url: row.identity_source_url.unwrap_or_default(),
                identity_source_title: row.identity_source_title.unwrap_or_default(),
                identity_evidence_text: row.identity_evidence_text.unwrap_or_default(),
            });
        product.capabilities.push(row.capability);
    }
    products.into_values().collect()
}

fn fingerprint_catalog_products(products: &[CatalogFingerprintProduct]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(APPROVED_CATALOG_FINGERPRINT_DOMAIN);
    for product in products {
        for value in [
            product.id.to_string(),
            product.manufacturer.clone(),
            product.model.clone(),
            product.capabilities.join("\u{1f}"),
            product.manufacturer_identifier_kind.clone(),
            product.manufacturer_identifier.clone(),
            product.avionics_manufacturer_identity_id.to_string(),
            product.canonical_product_key.clone(),
            product.graph_manufacturer_identifier_kind.clone(),
            product.canonical_identifier_key.clone(),
            product.identity_source_url.clone(),
            product.identity_source_title.clone(),
            product.identity_evidence_text.clone(),
        ] {
            feed_fingerprint(&mut hasher, &value);
        }
    }
    format!("{:x}", hasher.finalize())
}

fn fingerprint_catalog_product(product: &CatalogFingerprintProduct) -> String {
    let mut hasher = Sha256::new();
    hasher.update(APPROVED_CATALOG_PRODUCT_FINGERPRINT_DOMAIN);
    for value in [
        product.id.to_string(),
        product.manufacturer.clone(),
        product.model.clone(),
        product.capabilities.join("\u{1f}"),
        product.manufacturer_identifier_kind.clone(),
        product.manufacturer_identifier.clone(),
        product.avionics_manufacturer_identity_id.to_string(),
        product.canonical_product_key.clone(),
        product.graph_manufacturer_identifier_kind.clone(),
        product.canonical_identifier_key.clone(),
        product.identity_source_url.clone(),
        product.identity_source_title.clone(),
        product.identity_evidence_text.clone(),
    ] {
        feed_fingerprint(&mut hasher, &value);
    }
    format!("{:x}", hasher.finalize())
}

fn catalog_product_fingerprints(products: &[CatalogFingerprintProduct]) -> HashMap<i64, String> {
    products
        .iter()
        .map(|product| (product.id, fingerprint_catalog_product(product)))
        .collect()
}

pub(crate) fn fingerprint_approved_catalog_rows(rows: Vec<CatalogFingerprintRow>) -> String {
    fingerprint_catalog_products(&catalog_products(rows))
}

async fn load_catalog_product_fingerprint_map(db: &AppDb) -> ReviewResult<HashMap<i64, String>> {
    let sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, CatalogFingerprintRow>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, CatalogFingerprintRow>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(catalog_product_fingerprints(&catalog_products(rows)))
}

fn active_collision_closure_rows(
    rows: &[ActiveCollisionCatalogFingerprintRow],
    target_id: i64,
) -> Option<(
    &ActiveCollisionCatalogFingerprintRow,
    Vec<&ActiveCollisionCatalogFingerprintRow>,
)> {
    let target_rows = rows
        .iter()
        .filter(|row| row.id == target_id)
        .collect::<Vec<_>>();
    let [target] = target_rows.as_slice() else {
        return None;
    };
    let target_model_key = normalize_avionics_identifier(&target.model);
    let target_identifier_key = normalize_avionics_identifier(
        target
            .manufacturer_identifier
            .as_deref()
            .unwrap_or_default(),
    );
    if target_model_key.is_empty() || target_identifier_key.is_empty() {
        return None;
    }
    let members = rows
        .iter()
        .filter(|row| {
            let model_key = normalize_avionics_identifier(&row.model);
            let identifier_key = normalize_avionics_identifier(
                row.manufacturer_identifier.as_deref().unwrap_or_default(),
            );
            let exact_identity_collision = [model_key.as_str(), identifier_key.as_str()]
                .into_iter()
                .filter(|key| !key.is_empty())
                .any(|key| key == target_model_key || key == target_identifier_key);
            exact_identity_collision
                || (!model_key.is_empty()
                    && (model_key.starts_with(&target_model_key)
                        || target_model_key.starts_with(&model_key)))
        })
        .collect();
    Some((target, members))
}

pub(super) fn active_collision_closure_member_ids(
    rows: &[ActiveCollisionCatalogFingerprintRow],
    target_id: i64,
) -> Option<Vec<i64>> {
    let (_, members) = active_collision_closure_rows(rows, target_id)?;
    Some(members.into_iter().map(|row| row.id).collect())
}

pub(super) fn fingerprint_active_collision_closure(
    rows: &[ActiveCollisionCatalogFingerprintRow],
    current_reuse_eligible_ids: &HashSet<i64>,
    target_id: i64,
) -> Option<String> {
    let (_, members) = active_collision_closure_rows(rows, target_id)?;
    let mut keys = members
        .into_iter()
        .map(|row| {
            [
                row.id.to_string(),
                row.catalog_status.clone(),
                row.effective_manufacturer_identity_id
                    .map(|identity_id| identity_id.to_string())
                    .unwrap_or_default(),
                normalize_avionics_identifier(&row.model),
                row.manufacturer_identifier_kind.clone().unwrap_or_default(),
                normalize_avionics_identifier(
                    row.manufacturer_identifier.as_deref().unwrap_or_default(),
                ),
                current_reuse_eligible_ids.contains(&row.id).to_string(),
            ]
        })
        .collect::<Vec<_>>();
    keys.sort();

    let mut hasher = Sha256::new();
    hasher.update(ACTIVE_COLLISION_CLOSURE_FINGERPRINT_DOMAIN);
    feed_fingerprint(&mut hasher, &target_id.to_string());
    for key in keys {
        for value in key {
            feed_fingerprint(&mut hasher, &value);
        }
    }
    Some(format!("{:x}", hasher.finalize()))
}

pub(super) fn fingerprint_grounded_collision_closure(
    rows: &[ActiveCollisionCatalogFingerprintRow],
    target_id: i64,
) -> Option<String> {
    let (_, members) = active_collision_closure_rows(rows, target_id)?;
    let mut keys = members
        .into_iter()
        .map(|row| {
            [
                row.id.to_string(),
                row.catalog_status.clone(),
                row.effective_manufacturer_identity_id
                    .map(|identity_id| identity_id.to_string())
                    .unwrap_or_default(),
                normalize_avionics_identifier(&row.model),
                row.manufacturer_identifier_kind.clone().unwrap_or_default(),
                normalize_avionics_identifier(
                    row.manufacturer_identifier.as_deref().unwrap_or_default(),
                ),
            ]
        })
        .collect::<Vec<_>>();
    keys.sort();

    let mut hasher = Sha256::new();
    hasher.update(GROUNDED_COLLISION_CLOSURE_FINGERPRINT_DOMAIN);
    feed_fingerprint(&mut hasher, &target_id.to_string());
    for key in keys {
        for value in key {
            feed_fingerprint(&mut hasher, &value);
        }
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Optimistic token for every catalog fact that can change the zero-Gemini
/// listing-association resolver's identity decision.
///
/// This is intentionally separate from the approved catalog revision exposed
/// in the review payload: unreviewed rows are collision-only state, while
/// current reuse eligibility determines which approved graph identities can
/// participate in the local decision.
pub(crate) async fn active_collision_closure_revision_sha256(
    db: &AppDb,
    target_id: i64,
) -> ReviewResult<String> {
    let rows = load_active_collision_catalog_rows(db).await?;
    let current_reuse_eligible_ids = current_reuse_attested_product_ids(db).await?;
    fingerprint_active_collision_closure(&rows, &current_reuse_eligible_ids, target_id).ok_or_else(
        || {
            ReviewError::Conflict(format!(
                "catalog id {target_id} has no unique active collision-closure identity"
            ))
        },
    )
}

pub(crate) async fn grounded_collision_closure_revision_sha256(
    db: &AppDb,
    target_id: i64,
) -> ReviewResult<String> {
    let rows = load_active_collision_catalog_rows(db).await?;
    fingerprint_grounded_collision_closure(&rows, target_id).ok_or_else(|| {
        ReviewError::Conflict(format!(
            "catalog id {target_id} has no unique grounded collision-closure identity"
        ))
    })
}

async fn load_active_collision_catalog_rows(
    db: &AppDb,
) -> ReviewResult<Vec<ActiveCollisionCatalogFingerprintRow>> {
    let sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as::<
            _,
            ActiveCollisionCatalogFingerprintRow,
        >(&sql)
        .fetch_all(pool)
        .await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as::<
            _,
            ActiveCollisionCatalogFingerprintRow,
        >(&sql)
        .fetch_all(pool)
        .await?),
    }
}

/// Current approved-only catalog revision used by both ingestion and review.
/// Unreviewed/rejected legacy rows intentionally cannot invalidate a review.
pub async fn approved_catalog_revision_sha256(db: &AppDb) -> ReviewResult<String> {
    let sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, CatalogFingerprintRow>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, CatalogFingerprintRow>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(fingerprint_approved_catalog_rows(rows))
}

/// Deterministically serializes a complete review bundle. Producers should use
/// this rather than hand-building JSON so all optimistic-lock hashes agree.
pub fn serialize_review_payload(
    aspects: &[PendingReviewAspect],
) -> ReviewResult<SerializedReviewPayload> {
    let aspects = validated_aspects(aspects)?;
    let payload = PendingReviewPayload {
        version: REVIEW_PAYLOAD_VERSION,
        aspects: aspects.clone(),
    };
    let review_payload_json = serde_json::to_string(&payload).map_err(|error| {
        ReviewError::Validation(format!(
            "could not serialize pending review payload: {error}"
        ))
    })?;
    let extraction_json = serde_json::to_vec(
        &aspects
            .iter()
            .map(ExtractionFingerprintAspect::from)
            .collect::<Vec<_>>(),
    )
    .map_err(|error| {
        ReviewError::Validation(format!(
            "could not fingerprint listing observations: {error}"
        ))
    })?;
    let mut extraction_hasher = Sha256::new();
    extraction_hasher.update(EXTRACTION_FINGERPRINT_DOMAIN);
    extraction_hasher.update(extraction_json);
    Ok(SerializedReviewPayload {
        review_payload_sha256: sha256_hex(review_payload_json.as_bytes()),
        extraction_sha256: format!("{:x}", extraction_hasher.finalize()),
        pending_aspect_count: aspects.len() as i64,
        review_payload_json,
    })
}

#[derive(Serialize)]
struct ExtractionFingerprintAspect<'a> {
    id: &'a ReviewAspectId,
    kind: &'a str,
    observed_text: &'a str,
    quantity: i64,
    configuration_action: &'a str,
    source_evidence_text: Option<&'a str>,
    source_confidence: Option<&'a str>,
    replaces_product_id: Option<i64>,
    replacement_aspect_id: Option<&'a ReviewAspectId>,
    covered_associations: &'a [CoveredListingAssociation],
    reviewer_correction_association_binding: Option<&'a ReviewerCorrectionAssociationBinding>,
    reuse_attestation_target_id: Option<i64>,
}

impl<'a> From<&'a PendingReviewAspect> for ExtractionFingerprintAspect<'a> {
    fn from(aspect: &'a PendingReviewAspect) -> Self {
        Self {
            id: &aspect.id,
            kind: aspect.kind.as_str(),
            observed_text: aspect.observed_text.as_str(),
            quantity: aspect.quantity,
            configuration_action: aspect.configuration_action.as_str(),
            source_evidence_text: aspect.source_evidence_text.as_deref(),
            source_confidence: aspect.source_confidence.as_deref(),
            replaces_product_id: aspect.replaces_product_id,
            replacement_aspect_id: aspect.replacement_aspect_id.as_ref(),
            covered_associations: &aspect.covered_associations,
            reviewer_correction_association_binding: aspect
                .reviewer_correction_association_binding
                .as_ref(),
            reuse_attestation_target_id: aspect.reuse_attestation_target_id,
        }
    }
}

fn validated_aspects(aspects: &[PendingReviewAspect]) -> ReviewResult<Vec<PendingReviewAspect>> {
    if aspects.is_empty() {
        return Err(ReviewError::Validation(
            "a pending review must contain at least one aspect".to_string(),
        ));
    }
    let mut ids = HashSet::new();
    let mut covered_associations = HashSet::new();
    let mut validated = Vec::with_capacity(aspects.len());
    for original in aspects {
        let mut aspect = original.clone();
        aspect.id.validate()?;
        if !ids.insert(aspect.id.clone()) {
            return Err(ReviewError::Validation(format!(
                "duplicate review aspect ID {}",
                aspect.id
            )));
        }
        for (field, value) in [
            ("kind", aspect.kind.as_str()),
            ("label", aspect.label.as_str()),
            ("observed_text", aspect.observed_text.as_str()),
            ("reason", aspect.reason.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ReviewError::Validation(format!(
                    "review aspect {} has blank {field}",
                    aspect.id
                )));
            }
        }
        if aspect.quantity <= 0 {
            return Err(ReviewError::Validation(format!(
                "review aspect {} quantity must be positive",
                aspect.id
            )));
        }
        if aspect.covered_associations.iter().any(|association| {
            association.listing_link_id <= 0 || association.avionics_model_id <= 0
        }) {
            return Err(ReviewError::Validation(format!(
                "review aspect {} contains an invalid covered listing association",
                aspect.id
            )));
        }
        match aspect.reviewer_correction_association_binding.as_ref() {
            Some(binding)
                if aspect.kind != REVIEWER_CORRECTED_AVIONICS_KIND
                    || binding.listing_link_id <= 0
                    || binding.avionics_model_id <= 0
                    || binding.quantity <= 0
                    || !matches!(
                        binding.configuration_action.as_str(),
                        "installed" | "replaces" | "removes"
                    )
                    || ((binding.configuration_action == "installed")
                        != binding.replaces_avionics_model_id.is_none())
                    || aspect.covered_associations.len() != 1
                    || aspect.covered_associations[0].listing_link_id
                        != binding.listing_link_id
                    || match aspect.covered_associations[0].role {
                        ListingAssociationRole::Installed => {
                            aspect.covered_associations[0].avionics_model_id
                                != binding.avionics_model_id
                        }
                        ListingAssociationRole::Replacement => {
                            binding.replaces_avionics_model_id
                                != Some(aspect.covered_associations[0].avionics_model_id)
                        }
                    } =>
            {
                return Err(ReviewError::Validation(format!(
                    "review aspect {} contains an invalid reviewer-correction association binding",
                    aspect.id
                )));
            }
            None if aspect.kind == REVIEWER_CORRECTED_AVIONICS_KIND
                && !aspect.covered_associations.is_empty() =>
            {
                return Err(ReviewError::Validation(format!(
                    "covered reviewer correction {} is missing its exact association binding",
                    aspect.id
                )));
            }
            _ => {}
        }
        if aspect
            .proposed_product
            .as_ref()
            .and_then(|product| product.id)
            .is_some_and(|id| id <= 0)
        {
            return Err(ReviewError::Validation(format!(
                "review aspect {} contains an invalid unreviewed catalog candidate ID",
                aspect.id
            )));
        }
        if aspect.reuse_attestation_target_id.is_some_and(|id| id <= 0) {
            return Err(ReviewError::Validation(format!(
                "review aspect {} contains an invalid reuse-attestation target ID",
                aspect.id
            )));
        }
        aspect.covered_associations.sort();
        aspect.covered_associations.dedup();
        for association in &aspect.covered_associations {
            let key = (association.listing_link_id, association.role);
            if !covered_associations.insert(key) {
                return Err(ReviewError::Validation(format!(
                    "listing link {} {:?} component is covered by more than one review aspect",
                    association.listing_link_id, association.role
                )));
            }
        }
        if !matches!(
            aspect.configuration_action.as_str(),
            "installed" | "replaces" | "removes"
        ) {
            return Err(ReviewError::Validation(format!(
                "review aspect {} has invalid configuration_action {:?}",
                aspect.id, aspect.configuration_action
            )));
        }
        if !matches!(
            aspect.source_confidence.as_deref(),
            None | Some("high" | "medium" | "low")
        ) {
            return Err(ReviewError::Validation(format!(
                "review aspect {} has invalid source_confidence",
                aspect.id
            )));
        }
        aspect.source_evidence_text = aspect
            .source_evidence_text
            .as_deref()
            .map(str::trim)
            .filter(|evidence| !evidence.is_empty())
            .map(str::to_string);
        if aspect.source_evidence_text.is_none() || aspect.source_confidence.is_none() {
            aspect.source_evidence_text = None;
            aspect.source_confidence = None;
        }
        let has_replacement =
            aspect.replaces_product_id.is_some() || aspect.replacement_aspect_id.is_some();
        if aspect.replaces_product_id.is_some() && aspect.replacement_aspect_id.is_some() {
            return Err(ReviewError::Validation(format!(
                "review aspect {} cannot have two replacement targets",
                aspect.id
            )));
        }
        if aspect.configuration_action == "installed" && has_replacement {
            return Err(ReviewError::Validation(format!(
                "installed review aspect {} cannot have a replacement target",
                aspect.id
            )));
        }
        if matches!(aspect.configuration_action.as_str(), "replaces" | "removes")
            && !has_replacement
        {
            return Err(ReviewError::Validation(format!(
                "review aspect {} requires a replacement target",
                aspect.id
            )));
        }
        if aspect.reuse_attestation_target_id.is_some() {
            let target_id = aspect
                .reuse_attestation_target_id
                .expect("checked as present");
            if aspect.covered_associations.len() > 1
                || aspect
                    .covered_associations
                    .first()
                    .is_some_and(|association| association.avionics_model_id != target_id)
            {
                return Err(ReviewError::Validation(format!(
                    "review aspect {} may cover at most one association and it must identify its reuse-attestation target catalog id {target_id}",
                    aspect.id
                )));
            }
            if aspect.covered_associations.is_empty()
                && (!aspect.kind.starts_with("avionics")
                    || aspect.configuration_action != "installed"
                    || has_replacement)
            {
                return Err(ReviewError::Validation(format!(
                    "unlinked reuse-target aspect {} must be an ordinary installed avionics observation",
                    aspect.id
                )));
            }
            if aspect
                .proposed_product
                .as_ref()
                .and_then(|product| product.id)
                .is_some()
            {
                return Err(ReviewError::Validation(format!(
                    "review aspect {} cannot treat an approved reuse-attestation target as an unreviewed promotion candidate",
                    aspect.id
                )));
            }
            aspect.allowed_actions = vec![ReviewAction::Discard];
        } else if aspect.kind.starts_with("avionics") {
            // These are reviewer capabilities, not predictions made while the
            // bundle was staged. Product selection searches the live approved
            // catalog, while create still requires authoritative evidence.
            // Re-deriving the complete action set on every parse means a
            // review staged before another candidate was approved can select
            // that now-verified product instead of becoming stranded.
            aspect.allowed_actions = vec![
                ReviewAction::UseVerifiedProduct,
                ReviewAction::CreateVerifiedProduct,
                ReviewAction::Discard,
            ];
        } else {
            if aspect.allowed_actions.is_empty() {
                if aspect.proposed_product.is_some() {
                    aspect
                        .allowed_actions
                        .push(ReviewAction::CreateVerifiedProduct);
                }
                aspect.allowed_actions.push(ReviewAction::Discard);
            }
            let mut unique_actions = Vec::new();
            for action in aspect.allowed_actions {
                if !unique_actions.contains(&action) {
                    unique_actions.push(action);
                }
            }
            aspect.allowed_actions = unique_actions;
        }
        validated.push(aspect);
    }
    for aspect in &validated {
        if let Some(replacement_id) = &aspect.replacement_aspect_id {
            replacement_id.validate()?;
            if replacement_id == &aspect.id || !ids.contains(replacement_id) {
                return Err(ReviewError::Validation(format!(
                    "review aspect {} references unknown replacement aspect {}",
                    aspect.id, replacement_id
                )));
            }
        }
    }
    let coverage_owners = validated
        .iter()
        .flat_map(|aspect| {
            aspect.covered_associations.iter().map(move |association| {
                ((association.listing_link_id, association.role), &aspect.id)
            })
        })
        .collect::<HashMap<_, _>>();
    for child in &validated {
        for replacement in child
            .covered_associations
            .iter()
            .filter(|association| association.role == ListingAssociationRole::Replacement)
        {
            let installed_key = (
                replacement.listing_link_id,
                ListingAssociationRole::Installed,
            );
            let Some(parent_id) = coverage_owners.get(&installed_key) else {
                return Err(ReviewError::Validation(format!(
                    "covered replacement on listing link {} requires a covered installed parent",
                    replacement.listing_link_id
                )));
            };
            let parent = validated
                .iter()
                .find(|aspect| &aspect.id == *parent_id)
                .expect("coverage owner came from validated aspects");
            if parent.replacement_aspect_id.as_ref() != Some(&child.id) {
                return Err(ReviewError::Validation(format!(
                    "covered replacement on listing link {} is not referenced by its installed parent aspect",
                    replacement.listing_link_id
                )));
            }
        }
    }
    Ok(validated)
}

/// Atomically creates or replaces the single pending review for a listing.
/// Passing an empty aspect slice clears any stale bundle and returns `None`.
pub async fn replace_pending_review(
    db: &AppDb,
    listing_id: i64,
    plugin_submission_id: Option<i64>,
    aspects: &[PendingReviewAspect],
) -> ReviewResult<Option<StagedPendingReview>> {
    if aspects.is_empty() {
        clear_pending_review(db, listing_id).await?;
        return Ok(None);
    }
    stage_pending_review(db, listing_id, plugin_submission_id, aspects)
        .await
        .map(Some)
}

/// Stages a hash-addressed review bundle and moves the listing into the
/// `pending_review` ingestion state. `plugin_submission_id` is optional so the
/// same workflow can handle manual/update ingestion while still retaining the
/// plugin source when one exists.
pub async fn stage_pending_review(
    db: &AppDb,
    listing_id: i64,
    plugin_submission_id: Option<i64>,
    aspects: &[PendingReviewAspect],
) -> ReviewResult<StagedPendingReview> {
    if listing_id <= 0 {
        return Err(ReviewError::Validation(
            "listing_id must be positive".to_string(),
        ));
    }
    let serialized = serialize_review_payload(aspects)?;
    let lock_catalog = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models WHERE catalog_status = 'approved' ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE avionics_models, avionics_model_types, avionics_types, avionics_manufacturers IN SHARE ROW EXCLUSIVE MODE",
        ),
    };
    let select_owner = match db.backend() {
        DatabaseBackend::Sqlite(_) => {
            db.sql("SELECT created_by_user_id FROM aircraft_sale_listings WHERE id = ?")
        }
        DatabaseBackend::Postgres(_) => {
            db.sql("SELECT created_by_user_id FROM aircraft_sale_listings WHERE id = ? FOR UPDATE")
        }
    };
    let select_submission_owner = db.sql("SELECT user_id FROM plugin_submissions WHERE id = ?");
    let catalog_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let upsert_review = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_pending_reviews (
          listing_id,
          plugin_submission_id,
          extraction_sha256,
          catalog_revision_sha256,
          pending_aspect_count,
          review_payload_json,
          review_payload_sha256
        ) VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (listing_id) DO UPDATE SET
          plugin_submission_id = excluded.plugin_submission_id,
          extraction_sha256 = excluded.extraction_sha256,
          catalog_revision_sha256 = excluded.catalog_revision_sha256,
          pending_aspect_count = excluded.pending_aspect_count,
          review_payload_json = excluded.review_payload_json,
          review_payload_sha256 = excluded.review_payload_sha256,
          updated_at = CURRENT_TIMESTAMP
        "#,
    );
    let mark_pending = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'pending_review',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
    );

    macro_rules! stage_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let owner_user_id: Option<i64> = sqlx::query_scalar(&select_owner)
                .bind(listing_id)
                .fetch_optional(&mut *transaction)
                .await?;
            let owner_user_id = owner_user_id.ok_or_else(|| {
                ReviewError::NotFound(format!("listing {listing_id} was not found"))
            })?;
            if let Some(submission_id) = plugin_submission_id {
                let submission_owner: Option<i64> = sqlx::query_scalar(&select_submission_owner)
                    .bind(submission_id)
                    .fetch_optional(&mut *transaction)
                    .await?;
                match submission_owner {
                    Some(submission_owner) if submission_owner == owner_user_id => {}
                    Some(_) => {
                        return Err(ReviewError::Permission(format!(
                            "plugin submission {submission_id} does not belong to listing owner"
                        )));
                    }
                    None => {
                        return Err(ReviewError::NotFound(format!(
                            "plugin submission {submission_id} was not found"
                        )));
                    }
                }
            }

            sqlx::query(&lock_catalog)
                .execute(&mut *transaction)
                .await?;
            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let catalog_revision_sha256 =
                fingerprint_catalog_products(&catalog_products(catalog_rows));
            sqlx::query(&upsert_review)
                .bind(listing_id)
                .bind(plugin_submission_id)
                .bind(serialized.extraction_sha256.as_str())
                .bind(catalog_revision_sha256.as_str())
                .bind(serialized.pending_aspect_count)
                .bind(serialized.review_payload_json.as_str())
                .bind(serialized.review_payload_sha256.as_str())
                .execute(&mut *transaction)
                .await?;
            let changed = sqlx::query(&mark_pending)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if changed != 1 {
                return Err(ReviewError::Conflict(format!(
                    "listing {listing_id} changed while its review was being staged"
                )));
            }
            transaction.commit().await?;
            Ok::<String, ReviewError>(catalog_revision_sha256)
        }};
    }

    let catalog_revision_sha256 = match db.backend() {
        DatabaseBackend::Sqlite(pool) => stage_in_transaction!(pool)?,
        DatabaseBackend::Postgres(pool) => stage_in_transaction!(pool)?,
    };
    Ok(StagedPendingReview {
        listing_id,
        review_payload_sha256: serialized.review_payload_sha256,
        catalog_revision_sha256,
        pending_aspect_count: serialized.pending_aspect_count,
    })
}

#[derive(Clone, Debug)]
struct AssociationCorroborationCommit {
    aspect_id: ReviewAspectId,
    avionics_model_id: i64,
    observation_sha256: String,
    expected_catalog_revision_sha256: String,
    expected_collision_closure_sha256: String,
    evidence_provenance: ListingEvidenceProvenance,
}

#[derive(Clone, Debug)]
struct OrdinaryAspectUseExistingCommit {
    aspect_id: ReviewAspectId,
    avionics_model_id: i64,
    expected_catalog_revision_sha256: String,
    authorization: OrdinaryAspectUseExistingAuthorization,
}

#[derive(Clone, Debug)]
enum OrdinaryAspectUseExistingAuthorization {
    ReviewerSelection,
    HashBoundReuseTarget {
        expected_collision_closure_sha256: String,
        evidence_provenance: ListingEvidenceProvenance,
    },
}

#[derive(Clone, Debug)]
enum ReviewMaintenanceCommit {
    CorroborateAssociation(AssociationCorroborationCommit),
    UseExistingForOrdinaryAspect(OrdinaryAspectUseExistingCommit),
}

async fn restage_pending_review_if_current_with_commit(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    expected_review_payload_sha256: &str,
    maintenance_commit: Option<&ReviewMaintenanceCommit>,
) -> ReviewResult<Option<StagedPendingReview>> {
    if !valid_sha256(expected_review_payload_sha256)
        || maintenance_commit.is_some_and(|commit| match commit {
            ReviewMaintenanceCommit::CorroborateAssociation(commit) => {
                !valid_sha256(&commit.observation_sha256)
                    || !valid_sha256(&commit.expected_catalog_revision_sha256)
                    || !valid_sha256(&commit.expected_collision_closure_sha256)
                    || commit.evidence_provenance.plugin_submission_id <= 0
                    || commit.evidence_provenance.source_url.trim().is_empty()
                    || !valid_sha256(&commit.evidence_provenance.rendered_html_sha256)
            }
            ReviewMaintenanceCommit::UseExistingForOrdinaryAspect(commit) => {
                !valid_sha256(&commit.expected_catalog_revision_sha256)
                    || matches!(
                        &commit.authorization,
                        OrdinaryAspectUseExistingAuthorization::HashBoundReuseTarget {
                            expected_collision_closure_sha256,
                            evidence_provenance,
                        } if !valid_sha256(expected_collision_closure_sha256)
                            || evidence_provenance.plugin_submission_id <= 0
                            || evidence_provenance.source_url.trim().is_empty()
                            || !valid_sha256(&evidence_provenance.rendered_html_sha256)
                    )
            }
        })
    {
        return Err(ReviewError::Validation(
            "review maintenance revisions or listing evidence provenance are invalid".to_string(),
        ));
    }
    let lock_catalog = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models WHERE catalog_status = 'approved' ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_RESTAGE_CATALOG_LOCK_SQL),
    };
    let lock_listing_children = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql("SELECT 1"),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_LISTING_CHILD_LOCK_SQL),
    };
    let postgres_review_select = format!("{REVIEW_SELECT_SQL} FOR UPDATE OF listing, review");
    let select_review = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(REVIEW_SELECT_SQL),
        DatabaseBackend::Postgres(_) => db.sql(&postgres_review_select),
    };
    let catalog_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let active_collision_catalog_sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    let approved_products_sql = db.sql(APPROVED_PRODUCT_ROWS_SQL);
    let catalog_identity_sql = db.sql(
        r#"
        SELECT id, name AS model, manufacturer_identifier_kind,
               manufacturer_identifier
        FROM avionics_models
        ORDER BY id
        "#,
    );
    let assignments_sql = db.sql(EXISTING_ASSIGNMENT_ROWS_SQL);
    let corroborations_sql = db.sql(association_authorization_rows_sql(db));
    let attested_product_ids_sql = db.sql(
        r#"
        SELECT avionics_model_id
        FROM avionics_product_reuse_attestations
        ORDER BY avionics_model_id
        "#,
    );
    let evidence_capture_select = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            r#"
            SELECT
              id AS plugin_submission_id,
              user_id,
              canonical_listing_id,
              source_url,
              rendered_html,
              rendered_html_sha256
            FROM plugin_submissions
            WHERE id = ?
            "#,
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            r#"
            SELECT
              id AS plugin_submission_id,
              user_id,
              canonical_listing_id,
              source_url,
              rendered_html,
              rendered_html_sha256
            FROM plugin_submissions
            WHERE id = ?
            FOR SHARE
            "#,
        ),
    };
    let invalid_action_graph_sql = db.sql(
        r#"
        SELECT COUNT(*)
        FROM avionics_semantic_invalid_listing_action_graphs
        WHERE listing_id = ?
        "#,
    );
    let set_association_evidence = db.sql(
        r#"
        UPDATE aircraft_sale_listing_avionics
        SET source_notes = ?, source_confidence = 'high'
        WHERE id = ?
          AND aircraft_sale_listing_id = ?
          AND avionics_model_id = ?
        "#,
    );
    let clear_association_evidence = db.sql(
        r#"
        UPDATE aircraft_sale_listing_avionics
        SET source_notes = NULL, source_confidence = NULL
        WHERE id = ?
          AND aircraft_sale_listing_id = ?
          AND avionics_model_id = ?
        "#,
    );
    let update_review = db.sql(
        r#"
        UPDATE aircraft_sale_listing_pending_reviews
        SET extraction_sha256 = ?,
            catalog_revision_sha256 = ?,
            pending_aspect_count = ?,
            review_payload_json = ?,
            review_payload_sha256 = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE listing_id = ?
          AND review_payload_sha256 = ?
        "#,
    );
    let insert_reuse_authorization = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics_authorizations (
          listing_link_id,
          association_role,
          avionics_model_id,
          authorization_kind,
          observation_sha256,
          product_fingerprint,
          grounded_resolution_sha256,
          evidence_capture_sha256,
          collision_closure_sha256,
          policy_version
        )
        SELECT ?, ?, ?, 'manufacturer_reuse', ?, attestation.product_fingerprint,
               NULL, ?, ?, ?
        FROM avionics_product_reuse_attestations attestation
        WHERE attestation.avionics_model_id = ?
        ON CONFLICT (listing_link_id, association_role) DO NOTHING
        "#,
    );
    let delete_existing_authorization = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_avionics_authorizations
        WHERE listing_link_id = ?
          AND association_role = ?
        "#,
    );
    let select_approved_product_identity = db.sql(
        r#"
        SELECT avionics_manufacturer_identity_id, canonical_product_key
        FROM avionics_approved_product_graph_identities
        WHERE avionics_model_id = ?
        "#,
    );
    let select_colliding_product_relationship = db.sql(
        r#"
        SELECT existing.id
        FROM aircraft_sale_listing_avionics existing
        LEFT JOIN avionics_approved_product_graph_identities subject
          ON subject.avionics_model_id = existing.avionics_model_id
        LEFT JOIN avionics_approved_product_graph_identities displaced
          ON displaced.avionics_model_id = existing.replaces_avionics_model_id
        WHERE existing.aircraft_sale_listing_id = ?
          AND existing.id <> COALESCE(?, -1)
          AND (
            (
              existing.configuration_action IN ('installed', 'replaces')
              AND subject.avionics_manufacturer_identity_id = ?
              AND subject.canonical_product_key = ?
            )
            OR (
              existing.configuration_action IN ('replaces', 'removes')
              AND displaced.avionics_manufacturer_identity_id = ?
              AND displaced.canonical_product_key = ?
            )
          )
        ORDER BY existing.id
        LIMIT 1
        "#,
    );
    let insert_reviewed_link = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics (
          aircraft_sale_listing_id,
          avionics_model_id,
          quantity,
          source,
          source_notes,
          source_confidence,
          configuration_action,
          replaces_avionics_model_id
        ) VALUES (?, ?, ?, 'listing_review', ?, 'high', 'installed', NULL)
        RETURNING id
        "#,
    );
    let update_reviewed_link = db.sql(
        r#"
        UPDATE aircraft_sale_listing_avionics
        SET avionics_model_id = ?,
            quantity = ?,
            source = 'listing_review',
            source_notes = ?,
            source_confidence = 'high',
            configuration_action = 'installed',
            replaces_avionics_model_id = NULL
        WHERE id = ?
          AND aircraft_sale_listing_id = ?
          AND avionics_model_id = ?
          AND quantity = ?
          AND configuration_action = 'installed'
          AND replaces_avionics_model_id IS NULL
        "#,
    );
    let delete_review = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_pending_reviews
        WHERE listing_id = ?
          AND review_payload_sha256 = ?
        "#,
    );
    let mark_incomplete = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'incomplete',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND created_by_user_id = ?
          AND ingestion_state = 'pending_review'
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_reviews review
            WHERE review.listing_id = aircraft_sale_listings.id
          )
        "#,
    );

    macro_rules! restage_in_transaction {
        ($pool:expr, $reuse_attestation_is_current:ident) => {{
            let mut transaction = $pool.begin().await?;
            // Match the global writer order used by resolve: catalog first,
            // listing child tables second, then the listing/review rows.
            sqlx::query(&lock_catalog)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&lock_listing_children)
                .execute(&mut *transaction)
                .await?;
            let row: Option<ReviewRow> = sqlx::query_as(&select_review)
                .bind(listing_id)
                .fetch_optional(&mut *transaction)
                .await?;
            let row = row.ok_or_else(|| {
                ReviewError::Stale(format!(
                    "pending review for listing {listing_id} changed or was resolved"
                ))
            })?;
            if row.owner_user_id != owner_user_id {
                return Err(ReviewError::Permission(
                    "reviewers may only restage reviews for listings they own".to_string(),
                ));
            }
            if row.ingestion_state != "pending_review"
                || row.is_verified
                || row.review_payload_sha256 != expected_review_payload_sha256
            {
                return Err(ReviewError::Stale(
                    "pending review changed while preserved avionics were being restaged; reload"
                        .to_string(),
                ));
            }

            // Every value that determines the replacement graph and synthetic
            // reuse aspects is read after the same catalog/listing locks that
            // protect the write below. In particular, never carry an
            // out-of-transaction attestation decision into the persisted hash.
            let mut payload = parse_payload(
                &row.review_payload_json,
                Some(&row.review_payload_sha256),
                row.pending_aspect_count,
            )?;
            let normalized_payload = serialize_review_payload(&payload.aspects)?;
            let mut repaired_evidence =
                normalized_payload.review_payload_json != row.review_payload_json;
            let local_evidence_guard = match maintenance_commit {
                Some(ReviewMaintenanceCommit::CorroborateAssociation(commit)) => {
                    Some((&commit.aspect_id, &commit.evidence_provenance))
                }
                Some(ReviewMaintenanceCommit::UseExistingForOrdinaryAspect(commit)) => {
                    match &commit.authorization {
                        OrdinaryAspectUseExistingAuthorization::ReviewerSelection => None,
                        OrdinaryAspectUseExistingAuthorization::HashBoundReuseTarget {
                            evidence_provenance,
                            ..
                        } => Some((&commit.aspect_id, evidence_provenance)),
                    }
                }
                None => None,
            };
            if let Some((aspect_id, expected_provenance)) = local_evidence_guard {
                let evidence_text = payload
                    .aspects
                    .iter()
                    .find(|aspect| &aspect.id == aspect_id)
                    .and_then(|aspect| aspect.source_evidence_text.as_deref())
                    .filter(|evidence| !evidence.is_empty())
                    .ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "review aspect {aspect_id} no longer retains source_evidence_text"
                        ))
                    })?;
                if row.plugin_submission_id != Some(expected_provenance.plugin_submission_id) {
                    return Err(ReviewError::Stale(
                        "pending review changed its exact listing source capture after evidence verification"
                            .to_string(),
                    ));
                }
                let capture =
                    sqlx::query_as::<_, ListingEvidenceCaptureRow>(&evidence_capture_select)
                        .bind(expected_provenance.plugin_submission_id)
                        .fetch_optional(&mut *transaction)
                        .await?
                        .ok_or_else(|| {
                            ReviewError::Stale(
                                "pending review lost its exact retained listing source capture"
                                    .to_string(),
                            )
                        })?;
                validate_listing_evidence_capture(
                    &row,
                    &capture,
                    evidence_text,
                    Some(expected_provenance),
                )
                .map_err(ReviewError::Stale)?;
            }
            let mut assignments =
                sqlx::query_as::<_, ExistingAssignmentRow>(&assignments_sql)
                    .bind(listing_id)
                    .fetch_all(&mut *transaction)
                    .await?;
            let approved_rows = sqlx::query_as::<_, ApprovedProductRow>(&approved_products_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let approved = approved_product_map(approved_rows);
            let catalog_identity_rows =
                sqlx::query_as::<_, RecoverableCatalogIdentityRow>(&catalog_identity_sql)
                    .fetch_all(&mut *transaction)
                    .await?;
            let attested_product_ids =
                sqlx::query_scalar::<_, i64>(&attested_product_ids_sql)
                    .fetch_all(&mut *transaction)
                    .await?;
            let mut reuse_attested_ids = HashSet::new();
            for avionics_model_id in attested_product_ids {
                if $reuse_attestation_is_current(db, &mut transaction, avionics_model_id).await? {
                    reuse_attested_ids.insert(avionics_model_id);
                }
            }
            let active_collision_catalog_rows =
                sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(
                    &active_collision_catalog_sql,
                )
                .fetch_all(&mut *transaction)
                .await?;
            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let catalog_products = catalog_products(catalog_rows);
            let catalog_product_fingerprints =
                catalog_product_fingerprints(&catalog_products);
            let catalog_revision_sha256 = fingerprint_catalog_products(&catalog_products);
            let corroboration_rows_before_repair =
                sqlx::query_as::<_, AssociationAuthorizationRow>(&corroborations_sql)
                    .bind(listing_id)
                    .fetch_all(&mut *transaction)
                    .await?;
            let corroborated_before_evidence_repair =
                current_row_backed_authorized_associations(
                    listing_id,
                    &assignments,
                    &corroboration_rows_before_repair,
                    &reuse_attested_ids,
                    &active_collision_catalog_rows,
                    &catalog_product_fingerprints,
                );
            if maintenance_commit.is_none() {
                repaired_evidence |= remove_stale_covered_relationships(
                    &mut payload.aspects,
                    &assignments,
                );
            }
            validate_current_covered_associations(&payload.aspects, &assignments)?;
            let mut exact_association_evidence = HashMap::new();
            let mut evidence_repaired_link_ids = HashSet::new();
            let mut repair_evidence_capture_sha256 = None;
            if maintenance_commit.is_none() {
                let action_graph_issues: i64 =
                    sqlx::query_scalar(&invalid_action_graph_sql)
                        .bind(listing_id)
                        .fetch_one(&mut *transaction)
                        .await?;
                let rendered_capture = match row.plugin_submission_id {
                    Some(plugin_submission_id) if action_graph_issues == 0 => {
                        let capture = sqlx::query_as::<_, ListingEvidenceCaptureRow>(
                            &evidence_capture_select,
                        )
                            .bind(plugin_submission_id)
                            .fetch_optional(&mut *transaction)
                            .await?
                            .ok_or_else(|| {
                                ReviewError::Stale(
                                    "pending review lost its exact retained listing source capture"
                                        .to_string(),
                                )
                            })?;
                        let provenance = validate_listing_evidence_capture_provenance(
                            &row, &capture, None,
                        )
                        .map_err(ReviewError::Stale)?;
                        Some((capture, provenance))
                    }
                    _ => None,
                };
                let source = rendered_capture
                    .as_ref()
                    .map(|(capture, _)| {
                        ListingEvidenceContext::from_listing_capture(
                            Some(capture.source_url.as_str()),
                            Some(capture.rendered_html.as_str()),
                        )
                    });
                repair_evidence_capture_sha256 = rendered_capture
                    .as_ref()
                    .map(|(_, provenance)| provenance.rendered_html_sha256.clone());
                let repairs = plan_pending_association_evidence_repair(
                    &payload,
                    &assignments,
                    &approved,
                    &catalog_identity_rows,
                    source.as_ref(),
                    rendered_capture
                        .as_ref()
                        .map(|(capture, _)| capture.rendered_html.as_str()),
                );
                for repair in repairs {
                    let assignment = &mut assignments[repair.assignment_index];
                    let expected_evidence = repair.link_evidence_text.as_deref();
                    let expected_confidence = expected_evidence.map(|_| "high");
                    let link_changed = repair.update_link
                        && (assignment.source_notes.as_deref() != expected_evidence
                            || assignment.source_confidence.as_deref() != expected_confidence);
                    if link_changed {
                        let changed = match expected_evidence {
                            Some(evidence) => sqlx::query(&set_association_evidence)
                                .bind(evidence)
                                .bind(assignment.listing_link_id)
                                .bind(listing_id)
                                .bind(assignment.avionics_model_id)
                                .execute(&mut *transaction)
                                .await?
                                .rows_affected(),
                            None => sqlx::query(&clear_association_evidence)
                                .bind(assignment.listing_link_id)
                                .bind(listing_id)
                                .bind(assignment.avionics_model_id)
                                .execute(&mut *transaction)
                                .await?
                                .rows_affected(),
                        };
                        if changed != 1 {
                            return Err(ReviewError::Stale(format!(
                                "listing link {} changed while occurrence evidence was being repaired",
                                assignment.listing_link_id
                            )));
                        }
                        assignment.source_notes = repair.link_evidence_text.clone();
                        assignment.source_confidence =
                            expected_confidence.map(str::to_string);
                        evidence_repaired_link_ids.insert(assignment.listing_link_id);
                        repaired_evidence = true;
                    }
                    let association = CoveredListingAssociation {
                        listing_link_id: assignment.listing_link_id,
                        role: ListingAssociationRole::Installed,
                        avionics_model_id: assignment.avionics_model_id,
                    };
                    if let Some(evidence) = repair.evidence_text.as_ref() {
                        exact_association_evidence.insert(association, evidence.clone());
                    }
                    if let Some(aspect_index) = repair.aspect_index {
                        let before = payload.aspects[aspect_index].clone();
                        if repair.replace_redundant_primary
                            || is_synthetic_preserved_attestation_aspect(
                                &payload.aspects[aspect_index],
                            )
                        {
                            let product = approved
                                .get(&assignment.avionics_model_id)
                                .expect("exact evidence repairs only approved products");
                            payload.aspects[aspect_index] = preserved_product_aspect(
                                assignment,
                                ListingAssociationRole::Installed,
                                product,
                                None,
                            );
                        }
                        payload.aspects[aspect_index].source_evidence_text =
                            repair.aspect_evidence_text.clone();
                        payload.aspects[aspect_index].source_confidence = repair
                            .aspect_evidence_text
                            .as_ref()
                            .map(|_| "high".to_string());
                        repaired_evidence |= payload.aspects[aspect_index] != before;
                    }
                    let replacement_association = assignment
                        .replaces_avionics_model_id
                        .map(|avionics_model_id| CoveredListingAssociation {
                            listing_link_id: assignment.listing_link_id,
                            role: ListingAssociationRole::Replacement,
                            avionics_model_id,
                        });
                    if let (Some(association), Some(evidence)) = (
                        replacement_association,
                        repair.replacement_evidence_text.as_ref(),
                    ) {
                        exact_association_evidence.insert(association, evidence.clone());
                    }
                    if let Some(aspect_index) = repair.replacement_aspect_index {
                        let aspect = &mut payload.aspects[aspect_index];
                        let before = (
                            aspect.source_evidence_text.clone(),
                            aspect.source_confidence.clone(),
                        );
                        aspect.source_evidence_text =
                            repair.replacement_aspect_evidence_text.clone();
                        aspect.source_confidence = repair
                            .replacement_aspect_evidence_text
                            .as_ref()
                            .map(|_| "high".to_string());
                        repaired_evidence |= before
                            != (
                                aspect.source_evidence_text.clone(),
                                aspect.source_confidence.clone(),
                            );
                    }
                }
                if repaired_evidence && !payload.aspects.is_empty() {
                    payload.aspects = validated_aspects(&payload.aspects)?;
                    validate_current_covered_associations(&payload.aspects, &assignments)?;
                }
            }

            let mut ordinary_aspect_used = false;
            if let Some(ReviewMaintenanceCommit::UseExistingForOrdinaryAspect(commit)) =
                maintenance_commit
            {
                if commit.expected_catalog_revision_sha256 != catalog_revision_sha256 {
                    return Err(ReviewError::Stale(
                        "approved avionics catalog changed during aspect-scoped review; reload and re-evaluate"
                            .to_string(),
                    ));
                }
                let aspect_index = payload
                    .aspects
                    .iter()
                    .position(|aspect| aspect.id == commit.aspect_id)
                    .ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "review aspect {} changed before aspect-scoped approval",
                            commit.aspect_id
                        ))
                    })?;
                let aspect = payload.aspects[aspect_index].clone();
                match &commit.authorization {
                    OrdinaryAspectUseExistingAuthorization::ReviewerSelection => {
                        if aspect.reuse_attestation_target_id.is_some()
                            || !aspect
                                .allowed_actions
                                .contains(&ReviewAction::UseVerifiedProduct)
                        {
                            return Err(ReviewError::Validation(format!(
                                "review aspect {} is not an ordinary avionics identity aspect",
                                aspect.id
                            )));
                        }
                    }
                    OrdinaryAspectUseExistingAuthorization::HashBoundReuseTarget { .. } => {
                        if aspect.reuse_attestation_target_id != Some(commit.avionics_model_id) {
                            return Err(ReviewError::Validation(format!(
                                "review aspect {} is not authorized to select catalog id {}",
                                aspect.id, commit.avionics_model_id
                            )));
                        }
                    }
                }
                validate_independent_ordinary_aspect(&aspect, &payload.aspects)?;
                if !approved.contains_key(&commit.avionics_model_id)
                    || !reuse_attested_ids.contains(&commit.avionics_model_id)
                {
                    return Err(ReviewError::Conflict(format!(
                        "avionics catalog id {} is not an approved current-policy reusable product",
                        commit.avionics_model_id
                    )));
                }
                if let OrdinaryAspectUseExistingAuthorization::HashBoundReuseTarget {
                    expected_collision_closure_sha256,
                    ..
                } = &commit.authorization
                {
                    let current_collision_closure_sha256 =
                        fingerprint_active_collision_closure(
                            &active_collision_catalog_rows,
                            &reuse_attested_ids,
                            commit.avionics_model_id,
                        )
                        .ok_or_else(|| {
                            ReviewError::Stale(format!(
                                "catalog id {} lost its unique active collision closure",
                                commit.avionics_model_id
                            ))
                        })?;
                    if expected_collision_closure_sha256 != &current_collision_closure_sha256 {
                        return Err(ReviewError::Stale(
                            "active avionics collision catalog changed after local association matching; reload and re-evaluate"
                                .to_string(),
                        ));
                    }
                }

                let existing_action_graph_issues: i64 =
                    sqlx::query_scalar(&invalid_action_graph_sql)
                        .bind(listing_id)
                        .fetch_one(&mut *transaction)
                        .await?;
                if existing_action_graph_issues != 0 {
                    return Err(ReviewError::Conflict(format!(
                        "listing {listing_id} already has an invalid avionics action graph; aspect-scoped approval refuses to modify it"
                    )));
                }
                let covered_link_id = aspect
                    .covered_associations
                    .first()
                    .map(|association| association.listing_link_id);
                let product_identity: Option<(i64, String)> =
                    sqlx::query_as(&select_approved_product_identity)
                        .bind(commit.avionics_model_id)
                        .fetch_optional(&mut *transaction)
                        .await?;
                let (manufacturer_identity_id, canonical_product_key) =
                    product_identity.ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "approved catalog id {} has no canonical product identity",
                            commit.avionics_model_id
                        ))
                    })?;
                approved_avionics_product_key(
                    manufacturer_identity_id,
                    &canonical_product_key,
                )
                .map_err(ReviewError::Stale)?;
                let colliding_relationship: Option<i64> =
                    sqlx::query_scalar(&select_colliding_product_relationship)
                        .bind(listing_id)
                        .bind(covered_link_id)
                        .bind(manufacturer_identity_id)
                        .bind(canonical_product_key.as_str())
                        .bind(manufacturer_identity_id)
                        .bind(canonical_product_key.as_str())
                        .fetch_optional(&mut *transaction)
                        .await?;
                if let Some(colliding_link_id) = colliding_relationship {
                    return Err(ReviewError::Conflict(format!(
                        "listing {listing_id} link {colliding_link_id} already installs or displaces the canonical identity of catalog id {}; aspect-scoped approval refuses to merge or contradict independent relationships",
                        commit.avionics_model_id,
                    )));
                }

                let source_notes = aspect.source_evidence_text.as_deref();
                let selected_product = approved
                    .get(&commit.avionics_model_id)
                    .expect("approved aspect-scoped product was loaded under lock");
                if let Some(association) = aspect.covered_associations.first() {
                    let assignment = assignments
                        .iter_mut()
                        .find(|assignment| {
                            assignment.listing_link_id == association.listing_link_id
                        })
                        .expect("covered associations were validated against assignments");
                    let original_avionics_model_id = assignment.avionics_model_id;
                    let original_quantity = assignment.quantity;
                    let changed = sqlx::query(&update_reviewed_link)
                        .bind(commit.avionics_model_id)
                        .bind(aspect.quantity)
                        .bind(source_notes)
                        .bind(assignment.listing_link_id)
                        .bind(listing_id)
                        .bind(original_avionics_model_id)
                        .bind(original_quantity)
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if changed != 1 {
                        return Err(ReviewError::Stale(format!(
                            "listing link {} changed before aspect-scoped approval",
                            assignment.listing_link_id
                        )));
                    }
                    assignment.avionics_model_id = commit.avionics_model_id;
                    assignment.installed_manufacturer =
                        Some(selected_product.manufacturer.clone());
                    assignment.installed_model = Some(selected_product.model.clone());
                    assignment.replacement_manufacturer = None;
                    assignment.replacement_model = None;
                    assignment.quantity = aspect.quantity;
                    assignment.source = "listing_review".to_string();
                    assignment.source_notes = aspect.source_evidence_text.clone();
                    assignment.source_confidence = Some("high".to_string());
                    assignment.configuration_action = "installed".to_string();
                    assignment.replaces_avionics_model_id = None;
                    assignment.installed_catalog_status = Some("approved".to_string());
                    assignment.replacement_catalog_status = None;
                } else {
                    let listing_link_id: i64 = sqlx::query_scalar(&insert_reviewed_link)
                        .bind(listing_id)
                        .bind(commit.avionics_model_id)
                        .bind(aspect.quantity)
                        .bind(source_notes)
                        .fetch_one(&mut *transaction)
                        .await?;
                    assignments.push(ExistingAssignmentRow {
                        listing_link_id,
                        avionics_model_id: commit.avionics_model_id,
                        installed_manufacturer: Some(selected_product.manufacturer.clone()),
                        installed_model: Some(selected_product.model.clone()),
                        replacement_manufacturer: None,
                        replacement_model: None,
                        quantity: aspect.quantity,
                        source: "listing_review".to_string(),
                        source_notes: aspect.source_evidence_text.clone(),
                        source_confidence: Some("high".to_string()),
                        configuration_action: "installed".to_string(),
                        replaces_avionics_model_id: None,
                        installed_catalog_status: Some("approved".to_string()),
                        replacement_catalog_status: None,
                    });
                }
                payload.aspects.remove(aspect_index);
                ordinary_aspect_used = true;
            }

            // Repairing a link's exact source note deliberately invalidates
            // every hash-bound corroboration on that row. Preserve only
            // conclusions that were current before the repair and whose
            // repaired note is itself the exact retained evidence for the
            // same unchanged association role. This cannot mint a new
            // conclusion from restage text alone.
            for association in corroborated_before_evidence_repair.iter().filter(|association| {
                evidence_repaired_link_ids.contains(&association.listing_link_id)
            }) {
                let Some(evidence_text) = exact_association_evidence.get(association) else {
                    continue;
                };
                let Some(assignment) = assignments.iter().find(|assignment| {
                    assignment.listing_link_id == association.listing_link_id
                }) else {
                    continue;
                };
                if assignment.source_notes.as_deref() != Some(evidence_text.as_str()) {
                    continue;
                }
                let current_target_id = match association.role {
                    ListingAssociationRole::Installed => assignment.avionics_model_id,
                    ListingAssociationRole::Replacement => {
                        let Some(target_id) = assignment.replaces_avionics_model_id else {
                            continue;
                        };
                        target_id
                    }
                };
                if current_target_id != association.avionics_model_id {
                    continue;
                }
                if !corroboration_rows_before_repair.iter().any(|row| {
                    row.listing_link_id == association.listing_link_id
                        && row.association_role == association_role_label(association.role)
                        && row.authorization_kind == "manufacturer_reuse"
                }) {
                    // A same-case grounded receipt is bound to the exact
                    // original observation. Evidence repair must reopen it
                    // rather than silently mint a new grounded conclusion.
                    continue;
                }
                let Some(evidence_capture_sha256) =
                    repair_evidence_capture_sha256.as_deref()
                else {
                    continue;
                };
                let collision_closure_sha256 = fingerprint_active_collision_closure(
                    &active_collision_catalog_rows,
                    &reuse_attested_ids,
                    association.avionics_model_id,
                )
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "catalog id {} lost its unique active collision closure during occurrence-evidence repair",
                        association.avionics_model_id
                    ))
                })?;
                let observation_sha256 = association_observation_sha256(
                    listing_id,
                    assignment,
                    association.role,
                    evidence_text,
                );
                let role_label = association_role_label(association.role);
                sqlx::query(&delete_existing_authorization)
                    .bind(association.listing_link_id)
                    .bind(role_label)
                    .execute(&mut *transaction)
                    .await?;
                let inserted = sqlx::query(&insert_reuse_authorization)
                    .bind(association.listing_link_id)
                    .bind(role_label)
                    .bind(association.avionics_model_id)
                    .bind(observation_sha256)
                    .bind(evidence_capture_sha256)
                    .bind(collision_closure_sha256)
                    .bind(ASSOCIATION_AUTHORIZATION_POLICY_VERSION)
                    .bind(association.avionics_model_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if inserted != 1 {
                    return Err(ReviewError::Conflict(format!(
                        "current product attestation for catalog id {} disappeared during occurrence-evidence repair",
                        association.avionics_model_id
                    )));
                }
            }

            let mut corroboration_rows =
                sqlx::query_as::<_, AssociationAuthorizationRow>(&corroborations_sql)
                    .bind(listing_id)
                    .fetch_all(&mut *transaction)
                    .await?;
            let mut authorized_associations = current_authorized_associations(
                listing_id,
                &assignments,
                &corroboration_rows,
                &reuse_attested_ids,
                &active_collision_catalog_rows,
                &catalog_product_fingerprints,
            );

            if let Some(ReviewMaintenanceCommit::CorroborateAssociation(commit)) =
                maintenance_commit
            {
                if commit.expected_catalog_revision_sha256 != catalog_revision_sha256 {
                    return Err(ReviewError::Stale(
                        "approved avionics catalog changed during association corroboration; reload and re-evaluate"
                            .to_string(),
                    ));
                }
                let current_collision_closure_sha256 =
                    fingerprint_active_collision_closure(
                        &active_collision_catalog_rows,
                        &reuse_attested_ids,
                        commit.avionics_model_id,
                    )
                    .ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "catalog id {} lost its unique active collision closure",
                            commit.avionics_model_id
                        ))
                    })?;
                if commit.expected_collision_closure_sha256
                    != current_collision_closure_sha256
                {
                    return Err(ReviewError::Stale(
                        "active avionics collision catalog changed after local association matching; reload and re-evaluate"
                            .to_string(),
                    ));
                }
                let aspect = payload
                    .aspects
                    .iter()
                    .find(|aspect| aspect.id == commit.aspect_id)
                    .ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "review aspect {} changed before association corroboration",
                            commit.aspect_id
                        ))
                    })?;
                if !is_synthetic_preserved_attestation_aspect(aspect)
                    || aspect.reuse_attestation_target_id != Some(commit.avionics_model_id)
                {
                    return Err(ReviewError::Stale(format!(
                        "review aspect {} no longer identifies catalog id {} as an isolated preserved association",
                        commit.aspect_id, commit.avionics_model_id
                    )));
                }
                let association = aspect
                    .covered_associations
                    .first()
                    .expect("synthetic preserved aspects have one covered association");
                if association.role != ListingAssociationRole::Installed
                    || association.avionics_model_id != commit.avionics_model_id
                {
                    return Err(ReviewError::Validation(format!(
                        "review aspect {} is outside the supported one-by-one installed-association contract",
                        commit.aspect_id
                    )));
                }
                let assignment = assignments
                    .iter()
                    .find(|assignment| {
                        assignment.listing_link_id == association.listing_link_id
                    })
                    .ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "listing link {} changed before association corroboration",
                            association.listing_link_id
                        ))
                    })?;
                let evidence_text = aspect
                    .source_evidence_text
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "review aspect {} no longer retains listing evidence",
                            commit.aspect_id
                        ))
                    })?;
                if assignment.avionics_model_id != commit.avionics_model_id
                    || assignment.installed_catalog_status.as_deref() != Some("approved")
                    || assignment.configuration_action != "installed"
                    || assignment.replaces_avionics_model_id.is_some()
                    || assignment.quantity <= 0
                    || aspect.quantity != assignment.quantity
                    || assignment.source_notes.as_deref() != Some(evidence_text)
                {
                    return Err(ReviewError::Stale(format!(
                        "listing association for review aspect {} changed before corroboration",
                        commit.aspect_id
                    )));
                }
                let expected_observation_sha256 = association_observation_sha256(
                    listing_id,
                    assignment,
                    ListingAssociationRole::Installed,
                    evidence_text,
                );
                if expected_observation_sha256 != commit.observation_sha256 {
                    return Err(ReviewError::Stale(format!(
                        "listing evidence for review aspect {} changed before corroboration",
                        commit.aspect_id
                    )));
                }
                if !reuse_attested_ids.contains(&commit.avionics_model_id) {
                    return Err(ReviewError::Conflict(format!(
                        "catalog id {} lacks a current product reuse attestation",
                        commit.avionics_model_id
                    )));
                }

                if !authorized_associations.contains(association) {
                    // A stale row is not useful history: it refers to an
                    // observation or product fingerprint that no longer
                    // authorizes this exact association. Replace it inside
                    // the same hash-bound transaction.
                    sqlx::query(&delete_existing_authorization)
                        .bind(association.listing_link_id)
                        .bind("installed")
                        .execute(&mut *transaction)
                        .await?;
                    sqlx::query(&insert_reuse_authorization)
                        .bind(association.listing_link_id)
                        .bind("installed")
                        .bind(commit.avionics_model_id)
                        .bind(commit.observation_sha256.as_str())
                        .bind(commit.evidence_provenance.rendered_html_sha256.as_str())
                        .bind(commit.expected_collision_closure_sha256.as_str())
                        .bind(ASSOCIATION_AUTHORIZATION_POLICY_VERSION)
                        .bind(commit.avionics_model_id)
                        .execute(&mut *transaction)
                        .await?;
                    corroboration_rows =
                        sqlx::query_as::<_, AssociationAuthorizationRow>(&corroborations_sql)
                            .bind(listing_id)
                            .fetch_all(&mut *transaction)
                            .await?;
                    authorized_associations = current_authorized_associations(
                        listing_id,
                        &assignments,
                        &corroboration_rows,
                        &reuse_attested_ids,
                        &active_collision_catalog_rows,
                        &catalog_product_fingerprints,
                    );
                    if !authorized_associations.contains(association) {
                        return Err(ReviewError::Conflict(format!(
                            "exact listing corroboration for catalog id {} could not be persisted",
                            commit.avionics_model_id
                        )));
                    }
                }
            }

            let removed_authorized = remove_authorized_preserved_aspects(
                &mut payload.aspects,
                &authorized_associations,
            )?;
            let added_unauthorized = add_unauthorized_preserved_aspects(
                &mut payload.aspects,
                &assignments,
                &approved,
                &authorized_associations,
            )?;
            repaired_evidence |= apply_exact_association_evidence(
                &mut payload.aspects,
                &exact_association_evidence,
            );
            validate_current_covered_associations(&payload.aspects, &assignments)?;
            let hidden_blockers = hidden_preserved_blockers(
                &payload.aspects,
                &assignments,
                &authorized_associations,
            );
            if !hidden_blockers.is_empty() {
                return Err(ReviewError::Conflict(format!(
                    "preserved avionics cannot be represented by the current review: {}",
                    hidden_blockers.join("; ")
                )));
            }
            if payload.aspects.is_empty() {
                let deleted = sqlx::query(&delete_review)
                    .bind(listing_id)
                    .bind(expected_review_payload_sha256)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if deleted != 1 {
                    return Err(ReviewError::Stale(
                        "pending review changed while preserved avionics were being restaged; reload"
                            .to_string(),
                    ));
                }
                let changed = sqlx::query(&mark_incomplete)
                    .bind(listing_id)
                    .bind(owner_user_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if changed != 1 {
                    return Err(ReviewError::Stale(
                        "listing state changed while its satisfied review was being cleared"
                            .to_string(),
                    ));
                }
                transaction.commit().await?;
                return Ok::<Option<StagedPendingReview>, ReviewError>(None);
            }
            let (review_payload_sha256, pending_aspect_count) =
                if removed_authorized
                    || added_unauthorized
                    || repaired_evidence
                    || ordinary_aspect_used
                {
                    let serialized = serialize_review_payload(&payload.aspects)?;
                    let changed = sqlx::query(&update_review)
                        .bind(serialized.extraction_sha256.as_str())
                        .bind(catalog_revision_sha256.as_str())
                        .bind(serialized.pending_aspect_count)
                        .bind(serialized.review_payload_json.as_str())
                        .bind(serialized.review_payload_sha256.as_str())
                        .bind(listing_id)
                        .bind(expected_review_payload_sha256)
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if changed != 1 {
                        return Err(ReviewError::Stale(
                            "pending review changed while preserved avionics were being restaged; reload"
                                .to_string(),
                        ));
                    }
                    (
                        serialized.review_payload_sha256,
                        serialized.pending_aspect_count,
                    )
                } else {
                    (row.review_payload_sha256, row.pending_aspect_count)
                };
            transaction.commit().await?;
            Ok::<Option<StagedPendingReview>, ReviewError>(Some(StagedPendingReview {
                listing_id,
                review_payload_sha256,
                catalog_revision_sha256,
                pending_aspect_count,
            }))
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            restage_in_transaction!(pool, reuse_attestation_is_current_sqlite)
        }
        DatabaseBackend::Postgres(pool) => {
            restage_in_transaction!(pool, reuse_attestation_is_current_postgres)
        }
    }
}

async fn restage_pending_review_if_current(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    expected_review_payload_sha256: &str,
) -> ReviewResult<Option<StagedPendingReview>> {
    restage_pending_review_if_current_with_commit(
        db,
        owner_user_id,
        listing_id,
        expected_review_payload_sha256,
        None,
    )
    .await
}

fn configuration_action_is_editable(
    aspect: &PendingReviewAspect,
    aspects: &[PendingReviewAspect],
) -> bool {
    aspect.covered_associations.is_empty()
        && !aspects
            .iter()
            .any(|candidate| candidate.replacement_aspect_id.as_ref() == Some(&aspect.id))
}

fn replacement_target_matches_aspect(
    target: Option<&ReviewReplacementTarget>,
    aspect: &PendingReviewAspect,
) -> bool {
    match target {
        None => aspect.replaces_product_id.is_none() && aspect.replacement_aspect_id.is_none(),
        Some(ReviewReplacementTarget::CatalogProduct { avionics_model_id }) => {
            aspect.replaces_product_id == Some(*avionics_model_id)
                && aspect.replacement_aspect_id.is_none()
        }
        Some(ReviewReplacementTarget::ReviewAspect { aspect_id }) => {
            aspect.replaces_product_id.is_none()
                && aspect.replacement_aspect_id.as_ref() == Some(aspect_id)
        }
    }
}

fn reviewer_correction_association_binding(
    aspect: &PendingReviewAspect,
    assignments: &[ExistingAssignmentRow],
) -> ReviewResult<Option<ReviewerCorrectionAssociationBinding>> {
    if let Some(binding) = aspect.reviewer_correction_association_binding.as_ref() {
        return Ok(Some(binding.clone()));
    }
    let Some(association) = aspect.covered_associations.first() else {
        return Ok(None);
    };
    if aspect.covered_associations.len() != 1 {
        return Err(ReviewError::Validation(format!(
            "review aspect {} covers multiple listing associations and cannot be corrected as one occurrence",
            aspect.id
        )));
    }
    let assignment = assignments
        .iter()
        .find(|assignment| assignment.listing_link_id == association.listing_link_id)
        .ok_or_else(|| {
            ReviewError::Stale(format!(
                "listing link {} covered by review aspect {} no longer exists; reload",
                association.listing_link_id, aspect.id
            ))
        })?;
    Ok(Some(ReviewerCorrectionAssociationBinding {
        listing_link_id: assignment.listing_link_id,
        avionics_model_id: assignment.avionics_model_id,
        quantity: assignment.quantity,
        configuration_action: assignment.configuration_action.clone(),
        replaces_avionics_model_id: assignment.replaces_avionics_model_id,
    }))
}

fn prepare_observation_revision(
    request: &ReviseAvionicsObservationRequest,
) -> ReviewResult<(String, String, Vec<String>, String)> {
    request.aspect_id.validate()?;
    if !valid_sha256(&request.expected_review_payload_sha256)
        || !valid_sha256(&request.expected_catalog_revision_sha256)
    {
        return Err(ReviewError::Validation(
            "review and catalog revisions must be lowercase SHA-256 hex values".to_string(),
        ));
    }
    let manufacturer = request.manufacturer.trim();
    let model = request.model.trim();
    if manufacturer.chars().count() > MAX_REVIEW_PRODUCT_IDENTITY_LABEL_CHARACTERS
        || model.chars().count() > MAX_REVIEW_PRODUCT_IDENTITY_LABEL_CHARACTERS
    {
        return Err(ReviewError::Validation(format!(
            "corrected avionics manufacturer and model must each contain at most {MAX_REVIEW_PRODUCT_IDENTITY_LABEL_CHARACTERS} characters"
        )));
    }
    if !is_usable_avionics_label(manufacturer, model) {
        return Err(ReviewError::Validation(format!(
            "corrected avionics requires a concrete manufacturer and model: {manufacturer} {model}"
        )));
    }
    if request.quantity <= 0 {
        return Err(ReviewError::Validation(
            "corrected avionics quantity must be positive".to_string(),
        ));
    }
    let configuration_action = request.configuration_action.trim();
    if !matches!(configuration_action, "installed" | "replaces" | "removes") {
        return Err(ReviewError::Validation(format!(
            "unsupported corrected configuration_action {configuration_action:?}"
        )));
    }
    if (configuration_action == "installed") != request.replacement_target.is_none() {
        return Err(ReviewError::Validation(
            "installed avionics cannot have a replacement target, while replaces/removes requires exactly one target"
                .to_string(),
        ));
    }
    if let Some(ReviewReplacementTarget::CatalogProduct { avionics_model_id }) =
        request.replacement_target.as_ref()
    {
        if *avionics_model_id <= 0 {
            return Err(ReviewError::Validation(
                "replacement avionics_model_id must be positive".to_string(),
            ));
        }
    }
    if let Some(ReviewReplacementTarget::ReviewAspect { aspect_id }) =
        request.replacement_target.as_ref()
    {
        aspect_id.validate()?;
        if aspect_id == &request.aspect_id {
            return Err(ReviewError::Validation(
                "an avionics observation cannot use itself as a replacement target".to_string(),
            ));
        }
    }
    Ok((
        manufacturer.to_string(),
        model.to_string(),
        canonical_capabilities(&request.capabilities)?,
        configuration_action.to_string(),
    ))
}

/// Saves corrected reviewer values into the hash-addressed pending bundle.
///
/// Publisher text and evidence are intentionally immutable. The correction
/// only changes the product proposal and occurrence semantics consumed by the
/// existing guarded resolve workflow; it never mutates a catalog row or a
/// listing association directly.
pub async fn revise_avionics_observation_and_restage(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    request: &ReviseAvionicsObservationRequest,
) -> ReviewResult<StagedPendingReview> {
    if listing_id <= 0 || owner_user_id <= 0 {
        return Err(ReviewError::Validation(
            "listing and owner IDs must be positive".to_string(),
        ));
    }
    let (manufacturer, model, capabilities, configuration_action) =
        prepare_observation_revision(request)?;
    let lock_catalog = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models WHERE catalog_status = 'approved' ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_RESTAGE_CATALOG_LOCK_SQL),
    };
    let lock_listing_children = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql("SELECT 1"),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_LISTING_CHILD_LOCK_SQL),
    };
    let postgres_review_select = format!("{REVIEW_SELECT_SQL} FOR UPDATE OF listing, review");
    let select_review = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(REVIEW_SELECT_SQL),
        DatabaseBackend::Postgres(_) => db.sql(&postgres_review_select),
    };
    let catalog_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let approved_products_sql = db.sql(APPROVED_PRODUCT_ROWS_SQL);
    let assignments_sql = db.sql(EXISTING_ASSIGNMENT_ROWS_SQL);
    let update_review = db.sql(
        r#"
        UPDATE aircraft_sale_listing_pending_reviews
        SET extraction_sha256 = ?,
            catalog_revision_sha256 = ?,
            pending_aspect_count = ?,
            review_payload_json = ?,
            review_payload_sha256 = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE listing_id = ?
          AND review_payload_sha256 = ?
        "#,
    );

    macro_rules! revise_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&lock_catalog)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&lock_listing_children)
                .execute(&mut *transaction)
                .await?;
            let row = sqlx::query_as::<_, ReviewRow>(&select_review)
                .bind(listing_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "pending review for listing {listing_id} changed or was resolved"
                    ))
                })?;
            if row.owner_user_id != owner_user_id {
                return Err(ReviewError::Permission(
                    "reviewers may only revise reviews for listings they own".to_string(),
                ));
            }
            if row.ingestion_state != "pending_review"
                || row.is_verified
                || row.review_payload_sha256 != request.expected_review_payload_sha256
            {
                return Err(ReviewError::Stale(
                    "review payload is stale; reload before saving corrected avionics".to_string(),
                ));
            }
            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let catalog_revision_sha256 =
                fingerprint_catalog_products(&catalog_products(catalog_rows));
            if catalog_revision_sha256 != request.expected_catalog_revision_sha256 {
                return Err(ReviewError::Stale(
                    "approved avionics catalog changed; reload before saving corrected avionics"
                        .to_string(),
                ));
            }
            let approved_rows = sqlx::query_as::<_, ApprovedProductRow>(&approved_products_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let approved = approved_product_map(approved_rows);
            if let Some(ReviewReplacementTarget::CatalogProduct { avionics_model_id }) =
                request.replacement_target.as_ref()
            {
                if !approved.contains_key(avionics_model_id) {
                    return Err(ReviewError::Stale(format!(
                        "replacement catalog id {avionics_model_id} is missing or no longer approved"
                    )));
                }
            }
            let assignments = sqlx::query_as::<_, ExistingAssignmentRow>(&assignments_sql)
                .bind(listing_id)
                .fetch_all(&mut *transaction)
                .await?;
            let mut payload = parse_payload(
                &row.review_payload_json,
                Some(&row.review_payload_sha256),
                row.pending_aspect_count,
            )?;
            validate_current_covered_associations(&payload.aspects, &assignments)?;
            let aspect_index = payload
                .aspects
                .iter()
                .position(|aspect| aspect.id == request.aspect_id)
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "review aspect {} changed before its correction was saved",
                        request.aspect_id
                    ))
                })?;
            let original = payload.aspects[aspect_index].clone();
            if !original.kind.starts_with("avionics") {
                return Err(ReviewError::Validation(format!(
                    "review aspect {} is not an avionics observation",
                    request.aspect_id
                )));
            }
            let configuration_editable =
                configuration_action_is_editable(&original, &payload.aspects);
            let association_binding =
                reviewer_correction_association_binding(&original, &assignments)?;
            if !configuration_editable
                && (configuration_action != original.configuration_action
                    || !replacement_target_matches_aspect(
                        request.replacement_target.as_ref(),
                        &original,
                    ))
            {
                return Err(ReviewError::Validation(format!(
                    "review aspect {} is bound to an existing listing relationship; correct its identity, capabilities, or quantity, but keep the staged action and replacement target",
                    request.aspect_id
                )));
            }
            if payload.aspects.iter().any(|candidate| {
                candidate.replacement_aspect_id.as_ref() == Some(&original.id)
            }) && request.quantity != 1
            {
                return Err(ReviewError::Validation(format!(
                    "replacement target aspect {} must retain quantity 1",
                    request.aspect_id
                )));
            }
            if let Some(ReviewReplacementTarget::ReviewAspect { aspect_id }) =
                request.replacement_target.as_ref()
            {
                let target = payload
                    .aspects
                    .iter()
                    .find(|candidate| &candidate.id == aspect_id)
                    .ok_or_else(|| {
                        ReviewError::Validation(format!(
                            "replacement review aspect {aspect_id} does not exist"
                        ))
                    })?;
                if target.configuration_action != "installed"
                    || target.replaces_product_id.is_some()
                    || target.replacement_aspect_id.is_some()
                {
                    return Err(ReviewError::Validation(format!(
                        "replacement review aspect {aspect_id} is not a standalone product identity"
                    )));
                }
                if payload.aspects.iter().any(|candidate| {
                    candidate.id != original.id
                        && candidate.replacement_aspect_id.as_ref() == Some(aspect_id)
                }) {
                    return Err(ReviewError::Validation(format!(
                        "replacement review aspect {aspect_id} is already used by another observation"
                    )));
                }
            }

            let old_replacement_aspect_id = original.replacement_aspect_id.clone();
            let aspect = &mut payload.aspects[aspect_index];
            aspect.kind = REVIEWER_CORRECTED_AVIONICS_KIND.to_string();
            aspect.proposed_product = Some(ReviewProduct::proposed(
                manufacturer.clone(),
                model.clone(),
                capabilities.clone(),
            ));
            // A reviewer correction invalidates staged candidate IDs and
            // automatic suggestions. The ordinary live catalog search or the
            // grounded create path must prove the corrected identity again.
            aspect.suggested_product = None;
            aspect.reuse_attestation_target_id = None;
            aspect.reviewer_correction_association_binding = association_binding;
            aspect.quantity = request.quantity;
            aspect.configuration_action = configuration_action.clone();
            match request.replacement_target.as_ref() {
                None => {
                    aspect.replaces_product_id = None;
                    aspect.replacement_aspect_id = None;
                }
                Some(ReviewReplacementTarget::CatalogProduct { avionics_model_id }) => {
                    aspect.replaces_product_id = Some(*avionics_model_id);
                    aspect.replacement_aspect_id = None;
                }
                Some(ReviewReplacementTarget::ReviewAspect { aspect_id }) => {
                    aspect.replaces_product_id = None;
                    aspect.replacement_aspect_id = Some(aspect_id.clone());
                }
            }
            if configuration_editable {
                if let Some(old_target_id) = old_replacement_aspect_id {
                    let still_referenced = payload.aspects.iter().any(|candidate| {
                        candidate.replacement_aspect_id.as_ref() == Some(&old_target_id)
                    });
                    if !still_referenced {
                        if let Some(old_target_index) = payload
                            .aspects
                            .iter()
                            .position(|candidate| candidate.id == old_target_id)
                        {
                            if payload.aspects[old_target_index].covered_associations.is_empty() {
                                payload.aspects.remove(old_target_index);
                            }
                        }
                    }
                }
            }
            payload.aspects = validated_aspects(&payload.aspects)?;
            let serialized = serialize_review_payload(&payload.aspects)?;
            let changed = sqlx::query(&update_review)
                .bind(serialized.extraction_sha256.as_str())
                .bind(catalog_revision_sha256.as_str())
                .bind(serialized.pending_aspect_count)
                .bind(serialized.review_payload_json.as_str())
                .bind(serialized.review_payload_sha256.as_str())
                .bind(listing_id)
                .bind(request.expected_review_payload_sha256.as_str())
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if changed != 1 {
                return Err(ReviewError::Stale(
                    "pending review changed while corrected avionics were being saved; reload"
                        .to_string(),
                ));
            }
            transaction.commit().await?;
            Ok::<StagedPendingReview, ReviewError>(StagedPendingReview {
                listing_id,
                review_payload_sha256: serialized.review_payload_sha256,
                catalog_revision_sha256,
                pending_aspect_count: serialized.pending_aspect_count,
            })
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => revise_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => revise_in_transaction!(pool),
    }
}

/// Atomically records one exact installed-association corroboration and
/// removes only review maintenance aspects satisfied by the resulting
/// product-plus-association gate.
pub(crate) async fn corroborate_existing_product_association_and_restage(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    aspect_id: &ReviewAspectId,
    expected_review_payload_sha256: &str,
    expected_catalog_revision_sha256: &str,
    expected_collision_closure_sha256: &str,
    avionics_model_id: i64,
    observation_sha256: &str,
    evidence_provenance: &ListingEvidenceProvenance,
) -> ReviewResult<Option<StagedPendingReview>> {
    if avionics_model_id <= 0 {
        return Err(ReviewError::Validation(
            "avionics_model_id must be positive".to_string(),
        ));
    }
    aspect_id.validate()?;
    let commit = ReviewMaintenanceCommit::CorroborateAssociation(AssociationCorroborationCommit {
        aspect_id: aspect_id.clone(),
        avionics_model_id,
        observation_sha256: observation_sha256.to_string(),
        expected_catalog_revision_sha256: expected_catalog_revision_sha256.to_string(),
        expected_collision_closure_sha256: expected_collision_closure_sha256.to_string(),
        evidence_provenance: evidence_provenance.clone(),
    });
    restage_pending_review_if_current_with_commit(
        db,
        owner_user_id,
        listing_id,
        expected_review_payload_sha256,
        Some(&commit),
    )
    .await
}

/// Atomically applies one `use_verified_product` decision for an independent
/// ordinary installed aspect and re-hashes only the residual review work.
///
/// Unlike whole-review resolution, this boundary preserves every unrelated
/// listing link ID. It never merges an already-associated copy of the selected
/// product because doing so would make sequential aspect quantities ambiguous.
pub(crate) async fn use_existing_product_for_aspect_and_restage(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    aspect_id: &ReviewAspectId,
    expected_review_payload_sha256: &str,
    expected_catalog_revision_sha256: &str,
    avionics_model_id: i64,
) -> ReviewResult<Option<StagedPendingReview>> {
    if avionics_model_id <= 0 {
        return Err(ReviewError::Validation(
            "avionics_model_id must be positive".to_string(),
        ));
    }
    aspect_id.validate()?;
    let commit =
        ReviewMaintenanceCommit::UseExistingForOrdinaryAspect(OrdinaryAspectUseExistingCommit {
            aspect_id: aspect_id.clone(),
            avionics_model_id,
            expected_catalog_revision_sha256: expected_catalog_revision_sha256.to_string(),
            authorization: OrdinaryAspectUseExistingAuthorization::ReviewerSelection,
        });
    restage_pending_review_if_current_with_commit(
        db,
        owner_user_id,
        listing_id,
        expected_review_payload_sha256,
        Some(&commit),
    )
    .await
}

/// Atomically accepts one locally verified, hash-bound reuse target on an
/// independent ordinary extraction aspect.
///
/// This authorization is intentionally distinct from reviewer product
/// selection: only the source-free verification workflow can supply the
/// collision-closure token produced by the preceding exact local match.
pub(crate) async fn approve_locally_verified_ordinary_aspect_and_restage(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    aspect_id: &ReviewAspectId,
    expected_review_payload_sha256: &str,
    expected_catalog_revision_sha256: &str,
    expected_collision_closure_sha256: &str,
    avionics_model_id: i64,
    evidence_provenance: &ListingEvidenceProvenance,
) -> ReviewResult<Option<StagedPendingReview>> {
    if avionics_model_id <= 0 {
        return Err(ReviewError::Validation(
            "avionics_model_id must be positive".to_string(),
        ));
    }
    aspect_id.validate()?;
    let commit =
        ReviewMaintenanceCommit::UseExistingForOrdinaryAspect(OrdinaryAspectUseExistingCommit {
            aspect_id: aspect_id.clone(),
            avionics_model_id,
            expected_catalog_revision_sha256: expected_catalog_revision_sha256.to_string(),
            authorization: OrdinaryAspectUseExistingAuthorization::HashBoundReuseTarget {
                expected_collision_closure_sha256: expected_collision_closure_sha256.to_string(),
                evidence_provenance: evidence_provenance.clone(),
            },
        });
    restage_pending_review_if_current_with_commit(
        db,
        owner_user_id,
        listing_id,
        expected_review_payload_sha256,
        Some(&commit),
    )
    .await
}

/// Deletes a stale review bundle and returns the listing to incomplete. The
/// ordinary listing finalizer decides whether it can subsequently become
/// ready; clearing alone never publishes it.
pub async fn clear_pending_review(db: &AppDb, listing_id: i64) -> ReviewResult<()> {
    if listing_id <= 0 {
        return Err(ReviewError::Validation(
            "listing_id must be positive".to_string(),
        ));
    }
    let delete = db.sql("DELETE FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?");
    let mark_incomplete = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'incomplete',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND ingestion_state = 'pending_review'
        "#,
    );
    macro_rules! clear_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&delete)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&mark_incomplete)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Ok::<(), ReviewError>(())
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => clear_in_transaction!(pool)?,
        DatabaseBackend::Postgres(pool) => clear_in_transaction!(pool)?,
    }
    Ok(())
}

/// Attaches the retained plugin submission after plugin ingestion has linked
/// it to the newly created listing. Neither payload hash changes because the
/// submission reference is provenance metadata, not a reviewer decision.
pub async fn attach_pending_review_submission(
    db: &AppDb,
    listing_id: i64,
    submission_id: i64,
    owner_user_id: i64,
) -> ReviewResult<()> {
    if listing_id <= 0 || submission_id <= 0 || owner_user_id <= 0 {
        return Err(ReviewError::Validation(
            "listing, submission, and owner IDs must be positive".to_string(),
        ));
    }
    let sql = db.sql(
        r#"
        UPDATE aircraft_sale_listing_pending_reviews
        SET plugin_submission_id = ?, updated_at = CURRENT_TIMESTAMP
        WHERE listing_id = ?
          AND EXISTS (
            SELECT 1
            FROM aircraft_sale_listings listing
            JOIN plugin_submissions submission
              ON submission.canonical_listing_id = listing.id
            WHERE listing.id = aircraft_sale_listing_pending_reviews.listing_id
              AND listing.created_by_user_id = ?
              AND submission.id = ?
              AND submission.user_id = ?
          )
        "#,
    );
    let changed = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query(&sql)
            .bind(submission_id)
            .bind(listing_id)
            .bind(owner_user_id)
            .bind(submission_id)
            .bind(owner_user_id)
            .execute(pool)
            .await?
            .rows_affected(),
        DatabaseBackend::Postgres(pool) => sqlx::query(&sql)
            .bind(submission_id)
            .bind(listing_id)
            .bind(owner_user_id)
            .bind(submission_id)
            .bind(owner_user_id)
            .execute(pool)
            .await?
            .rows_affected(),
    };
    if changed != 1 {
        return Err(ReviewError::Conflict(format!(
            "could not attach plugin submission {submission_id} to pending review for listing {listing_id}; verify ownership and canonical listing linkage"
        )));
    }
    Ok(())
}

fn pagination(query: ReviewQueueQuery) -> ReviewResult<(i64, i64)> {
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    let offset = query.offset.unwrap_or(0);
    if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
        return Err(ReviewError::Validation(format!(
            "limit must be between 1 and {MAX_PAGE_LIMIT}"
        )));
    }
    if offset < 0 {
        return Err(ReviewError::Validation(
            "offset must be non-negative".to_string(),
        ));
    }
    Ok((limit, offset))
}

fn listing_label(model_year: i64, manufacturer: &str, model: &str, variant: &str) -> String {
    let aircraft = [manufacturer.trim(), model.trim(), variant.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    format!("{model_year} {aircraft}").trim().to_string()
}

fn parse_payload(
    review_payload_json: &str,
    review_payload_sha256: Option<&str>,
    pending_aspect_count: i64,
) -> ReviewResult<PendingReviewPayload> {
    if let Some(expected) = review_payload_sha256 {
        let actual = sha256_hex(review_payload_json.as_bytes());
        if actual != expected {
            return Err(ReviewError::Conflict(
                "stored review payload does not match its SHA-256; restage the listing".to_string(),
            ));
        }
    }
    let payload: PendingReviewPayload =
        serde_json::from_str(review_payload_json).map_err(|error| {
            ReviewError::Conflict(format!(
                "stored review payload is invalid; restage the listing: {error}"
            ))
        })?;
    if payload.version != REVIEW_PAYLOAD_VERSION {
        return Err(ReviewError::Conflict(format!(
            "unsupported stored review payload version {}; restage the listing",
            payload.version
        )));
    }
    let aspects = validated_aspects(&payload.aspects).map_err(|error| {
        ReviewError::Conflict(format!(
            "stored review payload failed validation; restage the listing: {error}"
        ))
    })?;
    if aspects.len() as i64 != pending_aspect_count {
        return Err(ReviewError::Conflict(
            "stored review aspect count is inconsistent; restage the listing".to_string(),
        ));
    }
    Ok(PendingReviewPayload {
        version: payload.version,
        aspects,
    })
}

pub(crate) fn parse_current_pending_review_aspects(
    review_payload_json: &str,
    review_payload_sha256: &str,
    pending_aspect_count: i64,
) -> ReviewResult<Vec<PendingReviewAspect>> {
    Ok(parse_payload(
        review_payload_json,
        Some(review_payload_sha256),
        pending_aspect_count,
    )?
    .aspects)
}

pub async fn list_listing_reviews(
    db: &AppDb,
    owner_user_id: i64,
    query: ReviewQueueQuery,
) -> ReviewResult<ListingReviewQueue> {
    let (limit, offset) = pagination(query)?;
    let count_sql = db.sql(
        r#"
        SELECT COUNT(*)
        FROM aircraft_sale_listing_pending_reviews review
        JOIN aircraft_sale_listings listing ON listing.id = review.listing_id
        WHERE listing.created_by_user_id = ?
        "#,
    );
    let page_sql = db.sql(
        r#"
        SELECT
          listing.id AS listing_id,
          listing.model_year,
          listing.registration_number,
          review.pending_aspect_count,
          review.review_payload_json,
          review.updated_at,
          manufacturer.name AS manufacturer,
          model.name AS model,
          variant.name AS variant
        FROM aircraft_sale_listing_pending_reviews review
        JOIN aircraft_sale_listings listing ON listing.id = review.listing_id
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = model.aircraft_manufacturer_id
        WHERE listing.created_by_user_id = ?
        ORDER BY review.updated_at DESC, listing.id DESC
        LIMIT ? OFFSET ?
        "#,
    );
    let (total, rows) = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let total: i64 = sqlx::query_scalar(&count_sql)
                .bind(owner_user_id)
                .fetch_one(pool)
                .await?;
            let rows = sqlx::query_as::<_, QueueRow>(&page_sql)
                .bind(owner_user_id)
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await?;
            (total, rows)
        }
        DatabaseBackend::Postgres(pool) => {
            let total: i64 = sqlx::query_scalar(&count_sql)
                .bind(owner_user_id)
                .fetch_one(pool)
                .await?;
            let rows = sqlx::query_as::<_, QueueRow>(&page_sql)
                .bind(owner_user_id)
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await?;
            (total, rows)
        }
    };
    let mut reviews = Vec::with_capacity(rows.len());
    for row in rows {
        let payload = parse_payload(&row.review_payload_json, None, row.pending_aspect_count)?;
        let mut reason_codes = Vec::new();
        for aspect in &payload.aspects {
            for reason in queue_reason_codes(&aspect.reason) {
                if !reason_codes.contains(&reason) {
                    reason_codes.push(reason);
                }
            }
        }
        reviews.push(ListingReviewQueueItem {
            listing_id: row.listing_id,
            label: listing_label(row.model_year, &row.manufacturer, &row.model, &row.variant),
            aircraft: ReviewAircraftSummary {
                manufacturer: row.manufacturer,
                model: row.model,
                variant: row.variant,
            },
            registration_number: row.registration_number,
            model_year: row.model_year,
            pending_aspect_count: row.pending_aspect_count,
            reason_codes,
            updated_at: row.updated_at,
        });
    }
    Ok(ListingReviewQueue {
        reviews,
        total,
        limit,
        offset,
    })
}

fn queue_reason_codes(reason: &str) -> Vec<String> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Vec::new();
    }
    let parts = reason.split(',').map(str::trim).collect::<Vec<_>>();
    let is_code_list = parts.len() > 1
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.chars().all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
                })
        });
    if is_code_list {
        parts.into_iter().map(str::to_string).collect()
    } else {
        vec![reason.to_string()]
    }
}

fn product_review_page_limit(query: &ProductReviewPageQuery) -> ReviewResult<usize> {
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
        return Err(ReviewError::Validation(format!(
            "limit must be between 1 and {MAX_PAGE_LIMIT}"
        )));
    }
    Ok(limit as usize)
}

fn encode_product_review_cursor<T: Serialize>(cursor: &T) -> ReviewResult<String> {
    let bytes = serde_json::to_vec(cursor).map_err(|error| {
        ReviewError::Database(format!("could not encode review cursor: {error}"))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_product_review_cursor<T>(
    cursor: Option<&str>,
    cursor_kind: &str,
) -> ReviewResult<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| {
        ReviewError::Validation(format!(
            "{cursor_kind} cursor is not valid opaque base64url"
        ))
    })?;
    serde_json::from_slice(&bytes).map(Some).map_err(|_| {
        ReviewError::Validation(format!("{cursor_kind} cursor has an invalid payload"))
    })
}

async fn load_product_review_source_rows(
    db: &AppDb,
    owner_user_id: i64,
) -> ReviewResult<Vec<ProductReviewSourceRow>> {
    let sql = db.sql(
        r#"
        SELECT
          listing.id AS listing_id,
          listing.source_url,
          listing.model_year,
          review.pending_aspect_count,
          review.review_payload_json,
          review.review_payload_sha256,
          manufacturer.name AS manufacturer,
          model.name AS model,
          variant.name AS variant
        FROM aircraft_sale_listing_pending_reviews review
        JOIN aircraft_sale_listings listing ON listing.id = review.listing_id
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = model.aircraft_manufacturer_id
        WHERE listing.created_by_user_id = ?
        ORDER BY listing.id
        "#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as::<_, ProductReviewSourceRow>(&sql)
            .bind(owner_user_id)
            .fetch_all(pool)
            .await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as::<_, ProductReviewSourceRow>(&sql)
            .bind(owner_user_id)
            .fetch_all(pool)
            .await?),
    }
}

fn aspect_cursor_key(aspect_id: &ReviewAspectId) -> ReviewResult<String> {
    serde_json::to_string(aspect_id)
        .map_err(|error| ReviewError::Database(format!("could not encode aspect cursor: {error}")))
}

fn pending_product_associations_from_rows(
    rows: Vec<ProductReviewSourceRow>,
) -> ReviewResult<Vec<(i64, String, PendingProductAssociationSource)>> {
    let mut associations = Vec::new();
    for row in rows {
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )?;
        let listing_label =
            listing_label(row.model_year, &row.manufacturer, &row.model, &row.variant);
        for aspect in payload.aspects {
            let Some(product_id) = aspect.reuse_attestation_target_id else {
                continue;
            };
            let aspect_key = aspect_cursor_key(&aspect.id)?;
            associations.push((
                product_id,
                aspect_key,
                PendingProductAssociationSource {
                    listing_id: row.listing_id,
                    listing_label: listing_label.clone(),
                    source_url: row.source_url.clone(),
                    aspect_id: aspect.id,
                    review_payload_sha256: row.review_payload_sha256.clone(),
                    observed_text: aspect.observed_text,
                    source_evidence_text: aspect.source_evidence_text,
                    quantity: aspect.quantity,
                    configuration_action: aspect.configuration_action,
                },
            ));
        }
    }
    associations.sort_by(|left, right| {
        (left.0, left.2.listing_id, left.1.as_str()).cmp(&(
            right.0,
            right.2.listing_id,
            right.1.as_str(),
        ))
    });
    Ok(associations)
}

pub async fn list_pending_product_reviews(
    db: &AppDb,
    owner_user_id: i64,
    query: ProductReviewPageQuery,
) -> ReviewResult<PendingProductReviewPage> {
    let limit = product_review_page_limit(&query)?;
    let cursor = decode_product_review_cursor::<ProductGroupCursor>(
        query.cursor.as_deref(),
        "product review",
    )?;
    let associations = pending_product_associations_from_rows(
        load_product_review_source_rows(db, owner_user_id).await?,
    )?;
    let snapshot = load_existing_product_association_global_snapshot(db).await?;
    let mut counts =
        BTreeMap::<i64, (i64, HashSet<i64>, Vec<PendingProductAssociationSource>)>::new();
    for (product_id, _, association) in associations {
        let count = counts
            .entry(product_id)
            .or_insert_with(|| (0, HashSet::new(), Vec::new()));
        count.0 += 1;
        count.1.insert(association.listing_id);
        count.2.push(association);
    }

    let mut groups = Vec::new();
    for (product_id, (pending_association_count, listing_ids, associations)) in counts {
        let product = snapshot.products.get(&product_id).cloned().ok_or_else(|| {
            ReviewError::Conflict(format!(
                "pending review targets catalog id {product_id}, which is not a current approved product"
            ))
        })?;
        let status = if snapshot.reuse_attested_ids.contains(&product_id) {
            ProductAttestationStatus::Current
        } else {
            ProductAttestationStatus::Required
        };
        if cursor
            .as_ref()
            .is_some_and(|cursor| product_id <= cursor.product_id)
        {
            continue;
        }
        let mut eligibility_counts = ProductAssociationEligibilityCounts::default();
        for association in associations {
            let eligibility = evaluate_existing_product_association_with_snapshot(
                db,
                &snapshot,
                owner_user_id,
                association.listing_id,
                &association.aspect_id,
                &association.review_payload_sha256,
                &snapshot.catalog_revision_sha256,
            )
            .await?
            .eligibility();
            eligibility_counts.record(&eligibility);
        }
        groups.push((
            product_id,
            PendingProductReviewGroup {
                product,
                attestation_status: status,
                pending_association_count,
                pending_listing_count: listing_ids.len() as i64,
                eligibility_counts,
            },
        ));
    }
    groups.sort_by_key(|(product_id, _)| *product_id);
    let has_more = groups.len() > limit;
    groups.truncate(limit);
    let next_cursor = if has_more {
        groups
            .last()
            .map(|(product_id, _)| ProductGroupCursor {
                product_id: *product_id,
            })
            .map(|cursor| encode_product_review_cursor(&cursor))
            .transpose()?
    } else {
        None
    };
    Ok(PendingProductReviewPage {
        catalog_revision_sha256: snapshot.catalog_revision_sha256,
        items: groups.into_iter().map(|(_, group)| group).collect(),
        next_cursor,
    })
}

pub async fn list_pending_product_associations(
    db: &AppDb,
    owner_user_id: i64,
    product_id: i64,
    query: ProductReviewPageQuery,
) -> ReviewResult<PendingProductAssociationPage> {
    if product_id <= 0 {
        return Err(ReviewError::Validation(
            "product_id must be positive".to_string(),
        ));
    }
    let limit = product_review_page_limit(&query)?;
    let cursor = decode_product_review_cursor::<ProductAssociationCursor>(
        query.cursor.as_deref(),
        "product association",
    )?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.product_id != product_id)
    {
        return Err(ReviewError::Validation(
            "product association cursor belongs to a different product".to_string(),
        ));
    }
    let mut associations = pending_product_associations_from_rows(
        load_product_review_source_rows(db, owner_user_id).await?,
    )?
    .into_iter()
    .filter(|(candidate_id, _, _)| *candidate_id == product_id)
    .collect::<Vec<_>>();
    if associations.is_empty() {
        return Err(ReviewError::NotFound(format!(
            "no pending review associations target approved catalog id {product_id}"
        )));
    }
    let snapshot = load_existing_product_association_global_snapshot(db).await?;
    let product = snapshot
        .products
        .get(&product_id)
        .cloned()
        .ok_or_else(|| {
            ReviewError::Conflict(format!(
                "pending review targets catalog id {product_id}, which is not a current approved product"
            ))
        })?;
    associations.retain(|(_, aspect_key, association)| {
        cursor.as_ref().is_none_or(|cursor| {
            (association.listing_id, aspect_key.as_str())
                > (cursor.listing_id, cursor.aspect_key.as_str())
        })
    });
    let has_more = associations.len() > limit;
    associations.truncate(limit);
    let next_cursor = if has_more {
        associations
            .last()
            .map(|(_, aspect_key, association)| ProductAssociationCursor {
                product_id,
                listing_id: association.listing_id,
                aspect_key: aspect_key.clone(),
            })
            .map(|cursor| encode_product_review_cursor(&cursor))
            .transpose()?
    } else {
        None
    };
    let mut projected_associations = Vec::with_capacity(associations.len());
    for (_, _, association) in associations {
        let eligibility = evaluate_existing_product_association_with_snapshot(
            db,
            &snapshot,
            owner_user_id,
            association.listing_id,
            &association.aspect_id,
            &association.review_payload_sha256,
            &snapshot.catalog_revision_sha256,
        )
        .await?
        .eligibility();
        projected_associations.push(association.project(eligibility));
    }
    Ok(PendingProductAssociationPage {
        product,
        attestation_status: if snapshot.reuse_attested_ids.contains(&product_id) {
            ProductAttestationStatus::Current
        } else {
            ProductAttestationStatus::Required
        },
        catalog_revision_sha256: snapshot.catalog_revision_sha256,
        associations: projected_associations,
        next_cursor,
    })
}

const REVIEW_SELECT_SQL: &str = r#"
    SELECT
      listing.id AS listing_id,
      listing.created_by_user_id AS owner_user_id,
      review.plugin_submission_id,
      capture.extracted_listing_json,
      listing.source_url,
      listing.model_year,
      listing.registration_number,
      listing.ingestion_state,
      listing.is_verified,
      review.pending_aspect_count,
      review.review_payload_json,
      review.review_payload_sha256,
      review.catalog_revision_sha256,
      manufacturer.name AS manufacturer,
      model.name AS model,
      variant.name AS variant
    FROM aircraft_sale_listings listing
    JOIN aircraft_sale_listing_pending_reviews review
      ON review.listing_id = listing.id
    LEFT JOIN plugin_submissions capture ON capture.id = review.plugin_submission_id
    JOIN aircraft_model_variants variant
      ON variant.id = listing.aircraft_model_variant_id
    JOIN aircraft_models model ON model.id = variant.aircraft_model_id
    JOIN aircraft_manufacturers manufacturer
      ON manufacturer.id = model.aircraft_manufacturer_id
    WHERE listing.id = ?
"#;

const APPROVED_PRODUCT_ROWS_SQL: &str = r#"
    SELECT
      model.id,
      manufacturer.name AS manufacturer,
      model.name AS model,
      capability.name AS capability,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier,
      model.identity_source_url,
      model.identity_source_title,
      model.identity_evidence_text
    FROM avionics_models model
    JOIN avionics_manufacturers manufacturer
      ON manufacturer.id = model.avionics_manufacturer_id
    JOIN avionics_model_types membership
      ON membership.avionics_model_id = model.id
    JOIN avionics_types capability
      ON capability.id = membership.avionics_type_id
    WHERE model.catalog_status = 'approved'
    ORDER BY model.id, capability.normalized_name, capability.id
"#;

const EXISTING_ASSIGNMENT_ROWS_SQL: &str = r#"
    SELECT
      link.id AS listing_link_id,
      link.avionics_model_id,
      installed_manufacturer.name AS installed_manufacturer,
      installed.name AS installed_model,
      replacement_manufacturer.name AS replacement_manufacturer,
      replaced.name AS replacement_model,
      link.quantity,
      link.source,
      link.source_notes,
      link.source_confidence,
      link.configuration_action,
      link.replaces_avionics_model_id,
      installed.catalog_status AS installed_catalog_status,
      replaced.catalog_status AS replacement_catalog_status
    FROM aircraft_sale_listing_avionics link
    LEFT JOIN avionics_models installed ON installed.id = link.avionics_model_id
    LEFT JOIN avionics_manufacturers installed_manufacturer
      ON installed_manufacturer.id = installed.avionics_manufacturer_id
    LEFT JOIN avionics_models replaced ON replaced.id = link.replaces_avionics_model_id
    LEFT JOIN avionics_manufacturers replacement_manufacturer
      ON replacement_manufacturer.id = replaced.avionics_manufacturer_id
    WHERE link.aircraft_sale_listing_id = ?
    ORDER BY link.id
"#;

const ASSOCIATION_AUTHORIZATION_ROWS_SQLITE: &str = r#"
    SELECT
      authorization.listing_link_id,
      authorization.association_role,
      authorization.avionics_model_id,
      authorization.authorization_kind,
      authorization.observation_sha256,
      authorization.product_fingerprint,
      attestation.product_fingerprint AS current_reuse_product_fingerprint,
      authorization.grounded_resolution_sha256,
      EXISTS (
        SELECT 1
        FROM plugin_submissions capture
        WHERE capture.canonical_listing_id = link.aircraft_sale_listing_id
          AND capture.rendered_html_sha256 = authorization.evidence_capture_sha256
          AND length(trim(COALESCE(link.source_notes, ''))) > 0
          AND instr(capture.rendered_html, link.source_notes) > 0
      ) AS evidence_capture_is_current,
      authorization.policy_version,
      authorization.collision_closure_sha256
    FROM aircraft_sale_listing_avionics_authorizations authorization
    JOIN aircraft_sale_listing_avionics link
      ON link.id = authorization.listing_link_id
    LEFT JOIN avionics_product_reuse_attestations attestation
      ON authorization.authorization_kind = 'manufacturer_reuse'
     AND attestation.avionics_model_id = authorization.avionics_model_id
    WHERE link.aircraft_sale_listing_id = ?
    ORDER BY authorization.listing_link_id, authorization.association_role
"#;

const ASSOCIATION_AUTHORIZATION_ROWS_POSTGRES: &str = r#"
    SELECT
      authorization.listing_link_id,
      authorization.association_role,
      authorization.avionics_model_id,
      authorization.authorization_kind,
      authorization.observation_sha256,
      authorization.product_fingerprint,
      attestation.product_fingerprint AS current_reuse_product_fingerprint,
      authorization.grounded_resolution_sha256,
      EXISTS (
        SELECT 1
        FROM plugin_submissions capture
        WHERE capture.canonical_listing_id = link.aircraft_sale_listing_id
          AND capture.rendered_html_sha256 = authorization.evidence_capture_sha256
          AND length(BTRIM(COALESCE(link.source_notes, ''))) > 0
          AND position(link.source_notes IN capture.rendered_html) > 0
      ) AS evidence_capture_is_current,
      authorization.policy_version,
      authorization.collision_closure_sha256
    FROM aircraft_sale_listing_avionics_authorizations authorization
    JOIN aircraft_sale_listing_avionics link
      ON link.id = authorization.listing_link_id
    LEFT JOIN avionics_product_reuse_attestations attestation
      ON authorization.authorization_kind = 'manufacturer_reuse'
     AND attestation.avionics_model_id = authorization.avionics_model_id
    WHERE link.aircraft_sale_listing_id = ?
    ORDER BY authorization.listing_link_id, authorization.association_role
"#;

fn association_authorization_rows_sql(db: &AppDb) -> &'static str {
    match db.backend() {
        DatabaseBackend::Sqlite(_) => ASSOCIATION_AUTHORIZATION_ROWS_SQLITE,
        DatabaseBackend::Postgres(_) => ASSOCIATION_AUTHORIZATION_ROWS_POSTGRES,
    }
}

fn approved_product_map(rows: Vec<ApprovedProductRow>) -> HashMap<i64, ReviewProduct> {
    let mut products = BTreeMap::<i64, ReviewProduct>::new();
    for row in rows {
        let product = products.entry(row.id).or_insert_with(|| ReviewProduct {
            id: Some(row.id),
            manufacturer: row.manufacturer,
            model: row.model,
            capabilities: Vec::new(),
            stable_identifier: match (
                row.manufacturer_identifier_kind,
                row.manufacturer_identifier,
            ) {
                (Some(kind), Some(value)) => Some(StableIdentifier { kind, value }),
                _ => None,
            },
            identity_source_url: row.identity_source_url,
            identity_source_title: row.identity_source_title,
            identity_evidence_text: row.identity_evidence_text,
        });
        product.capabilities.push(row.capability);
    }
    products.into_iter().collect()
}

fn catalog_projection(rows: Vec<CatalogProjectionRow>) -> Vec<CatalogProjectionProduct> {
    let mut products = BTreeMap::<i64, CatalogProjectionProduct>::new();
    for row in rows {
        let product = products
            .entry(row.id)
            .or_insert_with(|| CatalogProjectionProduct {
                id: row.id,
                manufacturer: row.manufacturer,
                model: row.model,
                capabilities: Vec::new(),
                catalog_status: row.catalog_status,
            });
        if let Some(capability) = row.capability.filter(|value| !value.trim().is_empty()) {
            product.capabilities.push(capability);
        }
    }
    products.into_values().collect()
}

async fn load_all_approved_product_map(db: &AppDb) -> ReviewResult<HashMap<i64, ReviewProduct>> {
    let sql = db.sql(APPROVED_PRODUCT_ROWS_SQL);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ApprovedProductRow>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ApprovedProductRow>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(approved_product_map(rows))
}

async fn load_approved_product_map(db: &AppDb) -> ReviewResult<HashMap<i64, ReviewProduct>> {
    let reuse_attested_ids = current_reuse_attested_product_ids(db).await?;
    let mut products = load_all_approved_product_map(db).await?;
    products.retain(|id, _| reuse_attested_ids.contains(id));
    Ok(products)
}

async fn load_existing_assignments(
    db: &AppDb,
    listing_id: i64,
) -> ReviewResult<Vec<ExistingAssignmentRow>> {
    let sql = db.sql(EXISTING_ASSIGNMENT_ROWS_SQL);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ExistingAssignmentRow>(&sql)
                .bind(listing_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ExistingAssignmentRow>(&sql)
                .bind(listing_id)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows)
}

async fn load_association_authorizations(
    db: &AppDb,
    listing_id: i64,
) -> ReviewResult<Vec<AssociationAuthorizationRow>> {
    let sql = db.sql(association_authorization_rows_sql(db));
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, AssociationAuthorizationRow>(&sql)
                .bind(listing_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, AssociationAuthorizationRow>(&sql)
                .bind(listing_id)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows)
}

async fn load_review_row(db: &AppDb, listing_id: i64) -> ReviewResult<ReviewRow> {
    let sql = db.sql(REVIEW_SELECT_SQL);
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ReviewRow>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ReviewRow>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await?
        }
    };
    row.ok_or_else(|| {
        ReviewError::NotFound(format!(
            "pending review for listing {listing_id} was not found"
        ))
    })
}

fn validate_listing_evidence_capture(
    review: &ReviewRow,
    capture: &ListingEvidenceCaptureRow,
    evidence_text: &str,
    expected: Option<&ListingEvidenceProvenance>,
) -> Result<ListingEvidenceProvenance, String> {
    let provenance = validate_listing_evidence_capture_provenance(review, capture, expected)?;
    validate_exact_listing_evidence_span(&capture.rendered_html, evidence_text)?;
    Ok(provenance)
}

fn validate_listing_evidence_capture_provenance(
    review: &ReviewRow,
    capture: &ListingEvidenceCaptureRow,
    expected: Option<&ListingEvidenceProvenance>,
) -> Result<ListingEvidenceProvenance, String> {
    if review.plugin_submission_id != Some(capture.plugin_submission_id)
        || capture.user_id != review.owner_user_id
        || capture.canonical_listing_id != Some(review.listing_id)
    {
        return Err(
            "pending review is not bound to the exact owner and canonical listing source capture"
                .to_string(),
        );
    }
    let listing_source_url = review
        .source_url
        .as_deref()
        .filter(|source_url| !source_url.trim().is_empty())
        .ok_or_else(|| {
            "pending review does not retain the listing source URL for its source capture"
                .to_string()
        })?;
    if capture.plugin_submission_id <= 0 || capture.source_url.trim().is_empty() {
        return Err("retained listing source capture has invalid provenance".to_string());
    }
    if capture.source_url != listing_source_url {
        return Err(
            "retained listing source capture changed: its URL does not match the listing source URL"
                .to_string(),
        );
    }
    if !valid_sha256(&capture.rendered_html_sha256)
        || sha256_hex(capture.rendered_html.as_bytes()) != capture.rendered_html_sha256
    {
        return Err("retained listing source capture failed its content hash".to_string());
    }
    if let Some(expected) = expected {
        if expected.plugin_submission_id != capture.plugin_submission_id
            || expected.source_url != capture.source_url
            || expected.rendered_html_sha256 != capture.rendered_html_sha256
        {
            return Err(
                "retained listing source capture changed after evidence verification".to_string(),
            );
        }
    }
    Ok(ListingEvidenceProvenance {
        plugin_submission_id: capture.plugin_submission_id,
        source_url: capture.source_url.clone(),
        rendered_html_sha256: capture.rendered_html_sha256.clone(),
    })
}

pub(super) fn validate_exact_listing_evidence_span(
    rendered_html: &str,
    evidence_text: &str,
) -> Result<(), String> {
    if evidence_text.is_empty()
        || evidence_text.len() > MAX_LISTING_EVIDENCE_CONTEXT_BYTES
        || !listing_body_contains_exact_structurally_visible_text_span(rendered_html, evidence_text)
    {
        return Err(
            "source_evidence_text is not one bounded exact structurally visible-body span from the retained listing capture"
                .to_string(),
        );
    }
    Ok(())
}

pub(super) fn validate_exact_listing_product_evidence(
    rendered_html: &str,
    evidence_text: &str,
    manufacturer: &str,
    model: &str,
) -> Result<(), String> {
    validate_exact_listing_evidence_span(rendered_html, evidence_text)?;
    let evidence = ListingEvidenceContext::from_cleaned_text(evidence_text);
    let exact_product = evidence
        .unique_exact_product_slice(manufacturer, model)
        .is_some();
    let exact_model = evidence.unique_exact_model_slice(model).is_some();
    if !exact_product && !exact_model {
        return Err(
            "source_evidence_text does not itself contain the exact unique catalog product model"
                .to_string(),
        );
    }
    Ok(())
}

async fn load_listing_evidence_provenance(
    db: &AppDb,
    review: &ReviewRow,
    evidence_text: &str,
) -> ReviewResult<ListingEvidenceProvenance> {
    let plugin_submission_id = review.plugin_submission_id.ok_or_else(|| {
        ReviewError::Validation(
            "automated existing-product verification requires the exact plugin submission attached to the pending review"
                .to_string(),
        )
    })?;
    let sql = db.sql(
        r#"
        SELECT
          id AS plugin_submission_id,
          user_id,
          canonical_listing_id,
          source_url,
          rendered_html,
          rendered_html_sha256
        FROM plugin_submissions
        WHERE id = ?
        "#,
    );
    let capture = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ListingEvidenceCaptureRow>(&sql)
                .bind(plugin_submission_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ListingEvidenceCaptureRow>(&sql)
                .bind(plugin_submission_id)
                .fetch_optional(pool)
                .await?
        }
    }
    .ok_or_else(|| {
        ReviewError::Validation(
            "pending review no longer has its exact retained listing source capture".to_string(),
        )
    })?;
    validate_listing_evidence_capture(review, &capture, evidence_text, None)
        .map_err(ReviewError::Validation)
}

async fn load_aircraft_identity_status(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
) -> ReviewResult<ReviewAircraftIdentityStatus> {
    let mut status = match require_listing_admission(db, listing_id).await {
        Ok(grounding) => ReviewAircraftIdentityStatus {
            status: ReviewAircraftIdentityState::Verified,
            reason_code: None,
            faa_n_number: Some(grounding.n_number),
            faa_snapshot_id: Some(grounding.snapshot.id),
            repair: None,
        },
        Err(error) => aircraft_identity_status_from_error(listing_id, error)?,
    };
    let repair = crate::aircraft::repair::preflight_aircraft_repair(db, owner_user_id, listing_id)
        .await
        .map_err(|error| ReviewError::Database(error.to_string()))?;
    if matches!(
        repair,
        crate::aircraft::repair::AircraftRepairPreflight::Available { .. }
    ) {
        status.repair = Some(repair);
    }
    Ok(status)
}

fn aircraft_identity_status_from_error(
    listing_id: i64,
    error: AircraftAdmissionError,
) -> ReviewResult<ReviewAircraftIdentityStatus> {
    match error {
        AircraftAdmissionError::Rejected {
            reason,
            n_number,
            snapshot_id,
            ..
        } => Ok(ReviewAircraftIdentityStatus {
            status: ReviewAircraftIdentityState::CurationRequired,
            reason_code: Some(block_reason_code(&reason).to_string()),
            faa_n_number: n_number,
            faa_snapshot_id: snapshot_id,
            repair: None,
        }),
        AircraftAdmissionError::LookupFailed { message, .. } => Err(ReviewError::Database(
            format!("could not check FAA aircraft identity for listing {listing_id}: {message}"),
        )),
        AircraftAdmissionError::ListingNotFound { .. } => Err(ReviewError::NotFound(format!(
            "listing {listing_id} was not found while checking its aircraft identity"
        ))),
    }
}

fn staged_approved_suggestion_id(
    aspect: &PendingReviewAspect,
    approved: &HashMap<i64, ReviewProduct>,
) -> Option<i64> {
    aspect
        .suggested_product
        .as_ref()
        .and_then(|product| product.id)
        .filter(|id| approved.contains_key(id))
        .or_else(|| {
            aspect
                .proposed_product
                .as_ref()
                .and_then(|product| product.id)
                .filter(|id| approved.contains_key(id))
        })
}

fn exact_single_covered_association(
    aspect: &PendingReviewAspect,
    role: ListingAssociationRole,
) -> Option<&CoveredListingAssociation> {
    let [association] = aspect.covered_associations.as_slice() else {
        return None;
    };
    (association.role == role).then_some(association)
}

fn exact_replacement_child_matches_assignment(
    child: &PendingReviewAspect,
    assignment: &ExistingAssignmentRow,
    expected_replacement_id: i64,
) -> bool {
    child.configuration_action == "installed"
        && child.quantity == 1
        && child.replaces_product_id.is_none()
        && child.replacement_aspect_id.is_none()
        && exact_single_covered_association(child, ListingAssociationRole::Replacement).is_some_and(
            |association| {
                association.listing_link_id == assignment.listing_link_id
                    && association.avionics_model_id == expected_replacement_id
            },
        )
}

fn exact_installed_aspect_matches_assignment(
    aspect: &PendingReviewAspect,
    aspects_by_id: &HashMap<ReviewAspectId, &PendingReviewAspect>,
    association: &CoveredListingAssociation,
    assignment: &ExistingAssignmentRow,
) -> bool {
    if association.listing_link_id != assignment.listing_link_id
        || association.avionics_model_id != assignment.avionics_model_id
        || aspect.quantity != assignment.quantity
        || aspect.configuration_action != assignment.configuration_action
    {
        return false;
    }
    match assignment.configuration_action.as_str() {
        "installed" => {
            assignment.replaces_avionics_model_id.is_none()
                && aspect.replaces_product_id.is_none()
                && aspect.replacement_aspect_id.is_none()
        }
        "replaces" | "removes" => {
            let Some(replacement_id) = assignment.replaces_avionics_model_id else {
                return false;
            };
            match (
                aspect.replaces_product_id,
                aspect.replacement_aspect_id.as_ref(),
            ) {
                (Some(staged_id), None) => staged_id == replacement_id,
                (None, Some(child_id)) => aspects_by_id.get(child_id).is_some_and(|child| {
                    exact_replacement_child_matches_assignment(child, assignment, replacement_id)
                }),
                _ => false,
            }
        }
        _ => false,
    }
}

fn exact_replacement_aspect_matches_assignment(
    aspect: &PendingReviewAspect,
    aspects: &[PendingReviewAspect],
    association: &CoveredListingAssociation,
    assignment: &ExistingAssignmentRow,
) -> bool {
    if association.listing_link_id != assignment.listing_link_id
        || assignment.replaces_avionics_model_id != Some(association.avionics_model_id)
        || !matches!(
            assignment.configuration_action.as_str(),
            "replaces" | "removes"
        )
        || aspect.configuration_action != "installed"
        || aspect.quantity != 1
        || aspect.replaces_product_id.is_some()
        || aspect.replacement_aspect_id.is_some()
    {
        return false;
    }
    let mut parents = aspects
        .iter()
        .filter(|candidate| candidate.replacement_aspect_id.as_ref() == Some(&aspect.id));
    let Some(parent) = parents.next() else {
        return false;
    };
    if parents.next().is_some()
        || parent.replaces_product_id.is_some()
        || parent.quantity != assignment.quantity
        || parent.configuration_action != assignment.configuration_action
    {
        return false;
    }
    exact_single_covered_association(parent, ListingAssociationRole::Installed).is_some_and(
        |parent_association| {
            parent_association.listing_link_id == assignment.listing_link_id
                && parent_association.avionics_model_id == assignment.avionics_model_id
        },
    )
}

fn exact_existing_approved_association_id(
    aspect: &PendingReviewAspect,
    aspects: &[PendingReviewAspect],
    aspects_by_id: &HashMap<ReviewAspectId, &PendingReviewAspect>,
    assignments_by_link: &HashMap<i64, &ExistingAssignmentRow>,
    approved: &HashMap<i64, ReviewProduct>,
) -> Option<i64> {
    if !aspect.kind.starts_with("avionics")
        || !aspect
            .allowed_actions
            .contains(&ReviewAction::UseVerifiedProduct)
    {
        return None;
    }
    let [association] = aspect.covered_associations.as_slice() else {
        return None;
    };
    let assignment = assignments_by_link.get(&association.listing_link_id)?;
    let (matches, current_product_id, current_catalog_status) = match association.role {
        ListingAssociationRole::Installed => (
            exact_installed_aspect_matches_assignment(
                aspect,
                aspects_by_id,
                association,
                assignment,
            ),
            assignment.avionics_model_id,
            assignment.installed_catalog_status.as_deref(),
        ),
        ListingAssociationRole::Replacement => (
            exact_replacement_aspect_matches_assignment(aspect, aspects, association, assignment),
            assignment.replaces_avionics_model_id?,
            assignment.replacement_catalog_status.as_deref(),
        ),
    };
    (matches
        && current_product_id == association.avionics_model_id
        && current_catalog_status == Some("approved")
        && approved.contains_key(&current_product_id))
    .then_some(current_product_id)
}

fn proposed_observation_matches_approved_product(
    aspect: &PendingReviewAspect,
    approved: &ReviewProduct,
) -> bool {
    let Some(proposed) = aspect.proposed_product.as_ref() else {
        return false;
    };
    let proposed_manufacturer = normalize_avionics_manufacturer_name(&proposed.manufacturer);
    let approved_manufacturer = normalize_avionics_manufacturer_name(&approved.manufacturer);
    let proposed_model = normalize_avionics_model_name(&proposed.model);
    let approved_model = normalize_avionics_model_name(&approved.model);
    if proposed_manufacturer.is_empty()
        || approved_manufacturer.is_empty()
        || proposed_manufacturer != approved_manufacturer
        || proposed_model.is_empty()
        || approved_model.is_empty()
        || proposed_model != approved_model
        || proposed.capabilities.is_empty()
    {
        return false;
    }
    let approved_capabilities = approved
        .capabilities
        .iter()
        .map(|capability| normalize_name(capability))
        .filter(|capability| !capability.is_empty())
        .collect::<HashSet<_>>();
    !approved_capabilities.is_empty()
        && proposed.capabilities.iter().all(|capability| {
            let capability = normalize_name(capability);
            !capability.is_empty() && approved_capabilities.contains(&capability)
        })
}

fn projected_suggested_product_id(
    aspect: &PendingReviewAspect,
    aspects: &[PendingReviewAspect],
    aspects_by_id: &HashMap<ReviewAspectId, &PendingReviewAspect>,
    assignments_by_link: &HashMap<i64, &ExistingAssignmentRow>,
    approved: &HashMap<i64, ReviewProduct>,
) -> Option<i64> {
    let staged = staged_approved_suggestion_id(aspect, approved);
    if aspect.covered_associations.is_empty() {
        return staged;
    }
    let exact = exact_existing_approved_association_id(
        aspect,
        aspects,
        aspects_by_id,
        assignments_by_link,
        approved,
    )?;
    if staged.is_some_and(|staged| staged != exact) {
        return None;
    }
    if aspect.reuse_attestation_target_id == Some(exact) {
        return Some(exact);
    }
    if !approved
        .get(&exact)
        .is_some_and(|product| proposed_observation_matches_approved_product(aspect, product))
    {
        return None;
    }
    Some(exact)
}

fn preserved_review_aspect_id(
    listing_link_id: i64,
    role: ListingAssociationRole,
) -> ReviewAspectId {
    let role = match role {
        ListingAssociationRole::Installed => "installed",
        ListingAssociationRole::Replacement => "replacement",
    };
    ReviewAspectId::String(format!("avionics:preserved:{listing_link_id}:{role}"))
}

fn preserved_product_aspect(
    assignment: &ExistingAssignmentRow,
    role: ListingAssociationRole,
    product: &ReviewProduct,
    exact_evidence: Option<&str>,
) -> PendingReviewAspect {
    let (product_id, quantity, configuration_action) = match role {
        ListingAssociationRole::Installed => (
            assignment.avionics_model_id,
            assignment.quantity,
            assignment.configuration_action.as_str(),
        ),
        ListingAssociationRole::Replacement => (
            assignment
                .replaces_avionics_model_id
                .expect("replacement aspect requires a replacement product"),
            1,
            "installed",
        ),
    };
    let label = [product.manufacturer.trim(), product.model.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    PendingReviewAspect::avionics(
        preserved_review_aspect_id(assignment.listing_link_id, role),
        "avionics_reuse_attestation",
        label.clone(),
        label,
        PRESERVED_ASSOCIATION_REVIEW_REASON,
        quantity,
        configuration_action,
        exact_evidence.map(str::to_string),
        exact_evidence.map(|_| "high".to_string()),
    )
    .with_covered_association(assignment.listing_link_id, role, product_id)
    .with_reuse_attestation_target(product_id)
}

#[derive(Clone, Debug)]
struct AssociationEvidenceRepair {
    assignment_index: usize,
    aspect_index: Option<usize>,
    replacement_aspect_index: Option<usize>,
    replace_redundant_primary: bool,
    update_link: bool,
    link_evidence_text: Option<String>,
    evidence_text: Option<String>,
    aspect_evidence_text: Option<String>,
    replacement_evidence_text: Option<String>,
    replacement_aspect_evidence_text: Option<String>,
}

fn catalog_identity_is_unique(
    product: &ReviewProduct,
    catalog: &[RecoverableCatalogIdentityRow],
) -> bool {
    let Some(product_id) = product.id else {
        return false;
    };
    let model_key = normalize_avionics_identifier(&product.model);
    let Some(identifier) = product.stable_identifier.as_ref() else {
        return false;
    };
    let identifier_key = normalize_avionics_identifier(&identifier.value);
    !model_key.is_empty()
        && !identifier_key.is_empty()
        && catalog.iter().all(|candidate| {
            let candidate_model_key = normalize_avionics_identifier(&candidate.model);
            candidate.id == product_id
                || (!candidate_model_key.starts_with(&model_key)
                    && (candidate.manufacturer_identifier_kind.as_deref()
                        != Some(identifier.kind.as_str())
                        || candidate
                            .manufacturer_identifier
                            .as_deref()
                            .map(normalize_avionics_identifier)
                            .is_none_or(|candidate| candidate != identifier_key)))
        })
}

fn redundant_historical_association_aspect(
    aspect: &PendingReviewAspect,
    assignment: &ExistingAssignmentRow,
    product: &ReviewProduct,
    aspects: &[PendingReviewAspect],
) -> bool {
    let reason_codes = aspect
        .reason
        .split(',')
        .map(str::trim)
        .collect::<BTreeSet<_>>();
    let expected_reasons = BTreeSet::from([
        "listing_action_graph_invalid",
        "catalog_product_unverified",
        "listing_link_confidence_not_high",
    ]);
    aspect.kind == "avionics"
        && reason_codes == expected_reasons
        && aspect.quantity == 1
        && aspect.configuration_action == "installed"
        && aspect.replaces_product_id.is_none()
        && aspect.replacement_aspect_id.is_none()
        && aspect
            .covered_associations
            .as_slice()
            .first()
            .is_some_and(|association| {
                aspect.covered_associations.len() == 1
                    && association.listing_link_id == assignment.listing_link_id
                    && association.role == ListingAssociationRole::Installed
                    && association.avionics_model_id == assignment.avionics_model_id
            })
        && aspect
            .reuse_attestation_target_id
            .is_none_or(|id| id == assignment.avionics_model_id)
        && proposed_observation_matches_approved_product(aspect, product)
        && aspect.suggested_product.as_ref().is_none_or(|suggested| {
            suggested.id == Some(assignment.avionics_model_id)
                && normalize_avionics_manufacturer_name(&suggested.manufacturer)
                    == normalize_avionics_manufacturer_name(&product.manufacturer)
                && normalize_avionics_model_name(&suggested.model)
                    == normalize_avionics_model_name(&product.model)
        })
        && !aspects
            .iter()
            .any(|candidate| candidate.replacement_aspect_id.as_ref() == Some(&aspect.id))
}

fn plan_pending_association_evidence_repair(
    payload: &PendingReviewPayload,
    assignments: &[ExistingAssignmentRow],
    approved: &HashMap<i64, ReviewProduct>,
    catalog: &[RecoverableCatalogIdentityRow],
    source: Option<&ListingEvidenceContext>,
    rendered_html: Option<&str>,
) -> Vec<AssociationEvidenceRepair> {
    let owners = covered_association_owners(&payload.aspects);
    assignments
        .iter()
        .enumerate()
        .map(|(assignment_index, assignment)| {
            let aspect_index = owners
                .get(&(
                    assignment.listing_link_id,
                    ListingAssociationRole::Installed,
                ))
                .copied();
            let replacement_aspect_index = owners
                .get(&(
                    assignment.listing_link_id,
                    ListingAssociationRole::Replacement,
                ))
                .copied();
            let aspect_shape_is_unique = aspect_index.is_none_or(|index| {
                let aspect = &payload.aspects[index];
                aspect.covered_associations.len() == 1
                    && aspect.quantity == assignment.quantity
                    && aspect.configuration_action == assignment.configuration_action
                    && aspect.replaces_product_id.is_none()
                    && aspect.replacement_aspect_id.is_none()
            });
            let approved_product = approved
                .get(&assignment.avionics_model_id)
                .filter(|product| catalog_identity_is_unique(product, catalog));
            let installed_identity = assignment
                .installed_manufacturer
                .as_deref()
                .zip(assignment.installed_model.as_deref());
            let exact_retained_evidence = installed_identity.and_then(|(manufacturer, model)| {
                assignment
                    .source_notes
                    .as_deref()
                    .filter(|_| assignment.source_confidence.is_some())
                    .filter(|evidence| {
                        source.is_some_and(|source| {
                            source.contains_exact_product_evidence(evidence, manufacturer, model)
                        }) && rendered_html.is_some_and(|html| {
                            listing_body_contains_exact_structurally_visible_text_span(
                                html, evidence,
                            )
                        })
                    })
                    .map(str::trim)
                    .map(str::to_string)
            });
            let replacement_identity = assignment
                .replacement_manufacturer
                .as_deref()
                .zip(assignment.replacement_model.as_deref());
            let exact_retained_replacement_evidence =
                replacement_identity.and_then(|(manufacturer, model)| {
                    assignment
                        .source_notes
                        .as_deref()
                        .filter(|_| assignment.source_confidence.is_some())
                        .filter(|evidence| {
                            source.is_some_and(|source| {
                                source.contains_exact_product_evidence(
                                    evidence,
                                    manufacturer,
                                    model,
                                )
                            }) && rendered_html.is_some_and(|html| {
                                listing_body_contains_exact_structurally_visible_text_span(
                                    html, evidence,
                                )
                            })
                        })
                        .map(str::trim)
                        .map(str::to_string)
                });
            let automatic_repair_shape = assignment.quantity > 0
                && assignment.configuration_action == "installed"
                && assignment.replaces_avionics_model_id.is_none()
                && assignment.installed_catalog_status.as_deref() == Some("approved")
                && aspect_shape_is_unique
                && approved_product.is_some();
            let retained_evidence_is_exact = exact_retained_evidence.is_some();
            let retained_replacement_evidence_is_exact =
                exact_retained_replacement_evidence.is_some();
            let evidence_text = if retained_evidence_is_exact {
                exact_retained_evidence
            } else if !automatic_repair_shape {
                None
            } else {
                approved_product.and_then(|product| {
                    source.and_then(|source| {
                        source
                            .unique_exact_model_slice(&product.model)
                            .and_then(|model_evidence| {
                                source
                                    .unique_exact_product_slice(
                                        &product.manufacturer,
                                        &product.model,
                                    )
                                    .or(Some(model_evidence))
                            })
                            .filter(|evidence| {
                                rendered_html.is_some_and(|html| {
                                    listing_body_contains_exact_structurally_visible_text_span(
                                        html, evidence,
                                    )
                                })
                            })
                    })
                })
            };
            let update_link = retained_evidence_is_exact
                || retained_replacement_evidence_is_exact
                || automatic_repair_shape;
            let link_evidence_text = evidence_text
                .clone()
                .or_else(|| exact_retained_replacement_evidence.clone());
            let aspect_evidence_text =
                aspect_index.map_or_else(
                    || evidence_text.clone(),
                    |index| {
                        let aspect = &payload.aspects[index];
                        if aspect.kind == REVIEWER_CORRECTED_AVIONICS_KIND {
                            paired_aspect_evidence(aspect)
                        } else if approved.get(&assignment.avionics_model_id).is_some_and(
                            |product| proposed_identity_conflicts_with_product(aspect, product),
                        ) {
                            exact_aspect_identity_evidence(aspect, rendered_html)
                        } else {
                            evidence_text.clone()
                        }
                    },
                );
            let replacement_product = assignment
                .replaces_avionics_model_id
                .and_then(|id| approved.get(&id));
            let replacement_aspect_evidence_text = replacement_aspect_index.map_or_else(
                || exact_retained_replacement_evidence.clone(),
                |index| {
                    let aspect = &payload.aspects[index];
                    if aspect.kind == REVIEWER_CORRECTED_AVIONICS_KIND {
                        paired_aspect_evidence(aspect)
                    } else if replacement_product.is_some_and(|product| {
                        proposed_identity_conflicts_with_product(aspect, product)
                    }) {
                        exact_aspect_identity_evidence(aspect, rendered_html)
                    } else {
                        exact_retained_replacement_evidence.clone()
                    }
                },
            );
            let replace_redundant_primary = automatic_repair_shape
                && evidence_text.is_some()
                && aspect_index.is_some_and(|index| {
                    let product = approved
                        .get(&assignment.avionics_model_id)
                        .expect("exact evidence requires an approved product");
                    redundant_historical_association_aspect(
                        &payload.aspects[index],
                        assignment,
                        product,
                        &payload.aspects,
                    )
                });
            AssociationEvidenceRepair {
                assignment_index,
                aspect_index,
                replacement_aspect_index,
                replace_redundant_primary,
                update_link,
                link_evidence_text,
                evidence_text,
                aspect_evidence_text,
                replacement_evidence_text: exact_retained_replacement_evidence,
                replacement_aspect_evidence_text,
            }
        })
        .collect()
}

fn apply_exact_association_evidence(
    aspects: &mut [PendingReviewAspect],
    exact_evidence: &HashMap<CoveredListingAssociation, String>,
) -> bool {
    let owners = covered_association_owners(aspects);
    let mut changed = false;
    for (association, evidence) in exact_evidence {
        let Some(index) = owners.get(&(association.listing_link_id, association.role)) else {
            continue;
        };
        let aspect = &mut aspects[*index];
        if aspect.source_evidence_text.as_deref() != Some(evidence.as_str())
            || aspect.source_confidence.as_deref() != Some("high")
        {
            aspect.source_evidence_text = Some(evidence.clone());
            aspect.source_confidence = Some("high".to_string());
            changed = true;
        }
    }
    changed
}

fn is_synthetic_preserved_attestation_aspect(aspect: &PendingReviewAspect) -> bool {
    let [association] = aspect.covered_associations.as_slice() else {
        return false;
    };
    aspect.kind == "avionics_reuse_attestation"
        && aspect.reuse_attestation_target_id == Some(association.avionics_model_id)
        && aspect.id == preserved_review_aspect_id(association.listing_link_id, association.role)
}

fn validate_independent_ordinary_aspect(
    aspect: &PendingReviewAspect,
    aspects: &[PendingReviewAspect],
) -> ReviewResult<()> {
    if !aspect.kind.starts_with("avionics") || is_synthetic_preserved_attestation_aspect(aspect) {
        return Err(ReviewError::Validation(format!(
            "review aspect {} is not an ordinary avionics identity aspect",
            aspect.id
        )));
    }
    let referenced_as_replacement = aspects
        .iter()
        .any(|candidate| candidate.replacement_aspect_id.as_ref() == Some(&aspect.id));
    if aspect.configuration_action != "installed"
        || aspect.replaces_product_id.is_some()
        || aspect.replacement_aspect_id.is_some()
        || referenced_as_replacement
    {
        return Err(ReviewError::Validation(format!(
            "review aspect {} is coupled to a replacement action and requires complete review",
            aspect.id
        )));
    }
    if aspect.quantity <= 0 {
        return Err(ReviewError::Validation(format!(
            "review aspect {} has an invalid quantity",
            aspect.id
        )));
    }
    if aspect.covered_associations.len() > 1
        || aspect
            .covered_associations
            .first()
            .is_some_and(|association| association.role != ListingAssociationRole::Installed)
    {
        return Err(ReviewError::Validation(format!(
            "review aspect {} does not identify an independent installed association",
            aspect.id
        )));
    }
    Ok(())
}

fn association_role_label(role: ListingAssociationRole) -> &'static str {
    match role {
        ListingAssociationRole::Installed => "installed",
        ListingAssociationRole::Replacement => "replacement",
    }
}

fn association_observation_sha256(
    listing_id: i64,
    assignment: &ExistingAssignmentRow,
    role: ListingAssociationRole,
    evidence_text: &str,
) -> String {
    let target_id = match role {
        ListingAssociationRole::Installed => assignment.avionics_model_id,
        ListingAssociationRole::Replacement => {
            assignment.replaces_avionics_model_id.unwrap_or_default()
        }
    };
    association_observation_sha256_from_values(
        listing_id,
        assignment.listing_link_id,
        role,
        target_id,
        assignment.avionics_model_id,
        assignment.replaces_avionics_model_id,
        assignment.quantity,
        &assignment.configuration_action,
        evidence_text,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn association_observation_sha256_from_values(
    listing_id: i64,
    listing_link_id: i64,
    role: ListingAssociationRole,
    target_id: i64,
    avionics_model_id: i64,
    replaces_avionics_model_id: Option<i64>,
    quantity: i64,
    configuration_action: &str,
    evidence_text: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ASSOCIATION_AUTHORIZATION_FINGERPRINT_DOMAIN);
    for value in [
        ASSOCIATION_AUTHORIZATION_POLICY_VERSION.to_string(),
        listing_id.to_string(),
        listing_link_id.to_string(),
        association_role_label(role).to_string(),
        target_id.to_string(),
        avionics_model_id.to_string(),
        replaces_avionics_model_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        quantity.to_string(),
        configuration_action.to_string(),
        evidence_text.to_string(),
    ] {
        feed_fingerprint(&mut hasher, &value);
    }
    format!("{:x}", hasher.finalize())
}

fn current_authorized_associations(
    listing_id: i64,
    assignments: &[ExistingAssignmentRow],
    rows: &[AssociationAuthorizationRow],
    reuse_attested_ids: &HashSet<i64>,
    active_collision_catalog_rows: &[ActiveCollisionCatalogFingerprintRow],
    catalog_product_fingerprints: &HashMap<i64, String>,
) -> HashSet<CoveredListingAssociation> {
    let mut authorized = current_row_backed_authorized_associations(
        listing_id,
        assignments,
        rows,
        reuse_attested_ids,
        active_collision_catalog_rows,
        catalog_product_fingerprints,
    );

    // A completed whole-listing review remains a legacy authorization only
    // while each exact product is globally reusable. Unlike a hash-bound
    // authorization row, source/confidence alone carries no current product
    // or collision fingerprint and must never bypass global eligibility.
    for assignment in assignments.iter().filter(|assignment| {
        assignment.source == "listing_review"
            && assignment.source_confidence.as_deref() == Some("high")
    }) {
        if reuse_attested_ids.contains(&assignment.avionics_model_id) {
            authorized.insert(CoveredListingAssociation {
                listing_link_id: assignment.listing_link_id,
                role: ListingAssociationRole::Installed,
                avionics_model_id: assignment.avionics_model_id,
            });
        }
        if let Some(avionics_model_id) = assignment
            .replaces_avionics_model_id
            .filter(|id| reuse_attested_ids.contains(id))
        {
            authorized.insert(CoveredListingAssociation {
                listing_link_id: assignment.listing_link_id,
                role: ListingAssociationRole::Replacement,
                avionics_model_id,
            });
        }
    }
    authorized
}

fn current_row_backed_authorized_associations(
    listing_id: i64,
    assignments: &[ExistingAssignmentRow],
    rows: &[AssociationAuthorizationRow],
    reuse_attested_ids: &HashSet<i64>,
    active_collision_catalog_rows: &[ActiveCollisionCatalogFingerprintRow],
    catalog_product_fingerprints: &HashMap<i64, String>,
) -> HashSet<CoveredListingAssociation> {
    let assignments_by_link = assignments
        .iter()
        .map(|assignment| (assignment.listing_link_id, assignment))
        .collect::<HashMap<_, _>>();
    let mut authorized = HashSet::new();

    for row in rows {
        if row.policy_version != ASSOCIATION_AUTHORIZATION_POLICY_VERSION
            || !row.evidence_capture_is_current
        {
            continue;
        }
        let current_collision_closure_sha256 = match row.authorization_kind.as_str() {
            "manufacturer_reuse"
                if row.grounded_resolution_sha256.is_none()
                    && row.current_reuse_product_fingerprint.as_deref()
                        == Some(row.product_fingerprint.as_str())
                    && reuse_attested_ids.contains(&row.avionics_model_id) =>
            {
                fingerprint_active_collision_closure(
                    active_collision_catalog_rows,
                    reuse_attested_ids,
                    row.avionics_model_id,
                )
            }
            "same_case_grounded"
                if row
                    .grounded_resolution_sha256
                    .as_deref()
                    .is_some_and(valid_sha256)
                    && catalog_product_fingerprints.get(&row.avionics_model_id)
                        == Some(&row.product_fingerprint) =>
            {
                fingerprint_grounded_collision_closure(
                    active_collision_catalog_rows,
                    row.avionics_model_id,
                )
            }
            _ => None,
        };
        let Some(current_collision_closure_sha256) = current_collision_closure_sha256 else {
            continue;
        };
        if row.collision_closure_sha256 != current_collision_closure_sha256 {
            continue;
        }
        let Some(assignment) = assignments_by_link.get(&row.listing_link_id) else {
            continue;
        };
        let role = match row.association_role.as_str() {
            "installed" => ListingAssociationRole::Installed,
            "replacement" => ListingAssociationRole::Replacement,
            _ => continue,
        };
        let target_id = match role {
            ListingAssociationRole::Installed => assignment.avionics_model_id,
            ListingAssociationRole::Replacement => {
                assignment.replaces_avionics_model_id.unwrap_or_default()
            }
        };
        let evidence_text = assignment.source_notes.as_deref().unwrap_or_default();
        if target_id != row.avionics_model_id
            || row.observation_sha256
                != association_observation_sha256(listing_id, assignment, role, evidence_text)
        {
            continue;
        }
        authorized.insert(CoveredListingAssociation {
            listing_link_id: row.listing_link_id,
            role,
            avionics_model_id: row.avionics_model_id,
        });
    }
    authorized
}

/// Remove hash-bound maintenance aspects after their exact association has a
/// current authorization.
///
/// A real extraction aspect can also be annotated with a reuse target while
/// that target is unselectable. Its identity and reviewer decision remain
/// relevant after authorization, so only the exact synthetic preserved-
/// association shape created by `preserved_product_aspect` is retired here.
fn remove_authorized_preserved_aspects(
    aspects: &mut Vec<PendingReviewAspect>,
    authorized_associations: &HashSet<CoveredListingAssociation>,
) -> ReviewResult<bool> {
    let candidates = aspects
        .iter()
        .filter(|aspect| {
            is_synthetic_preserved_attestation_aspect(aspect)
                && aspect
                    .covered_associations
                    .first()
                    .is_some_and(|association| authorized_associations.contains(association))
        })
        .filter_map(|aspect| {
            aspect
                .reuse_attestation_target_id
                .map(|target_id| (aspect.id.clone(), target_id))
        })
        .collect::<HashMap<_, _>>();
    if candidates.is_empty() {
        return Ok(false);
    }

    // A replacement observation requires its installed parent in the review
    // graph. Keep an otherwise-obsolete synthetic parent while an unattested
    // replacement child still needs it.
    let removable = candidates
        .iter()
        .filter(|(aspect_id, _)| {
            aspects
                .iter()
                .find(|aspect| &aspect.id == *aspect_id)
                .and_then(|aspect| aspect.replacement_aspect_id.as_ref())
                .is_none_or(|child_id| candidates.contains_key(child_id))
        })
        .map(|(aspect_id, target_id)| (aspect_id.clone(), *target_id))
        .collect::<HashMap<_, _>>();
    if removable.is_empty() {
        return Ok(false);
    }

    // If the attested component was represented as a replacement child,
    // retain the parent's exact replacement identity as a direct immutable
    // target before dropping the now-obsolete child aspect.
    for aspect in aspects.iter_mut() {
        let Some(child_id) = aspect.replacement_aspect_id.as_ref() else {
            continue;
        };
        let Some(target_id) = removable.get(child_id).copied() else {
            continue;
        };
        aspect.replacement_aspect_id = None;
        aspect.replaces_product_id = Some(target_id);
    }
    aspects.retain(|aspect| !removable.contains_key(&aspect.id));

    if !aspects.is_empty() {
        *aspects = validated_aspects(aspects)?;
    }
    Ok(true)
}

fn covered_association_owners(
    aspects: &[PendingReviewAspect],
) -> HashMap<(i64, ListingAssociationRole), usize> {
    aspects
        .iter()
        .enumerate()
        .flat_map(|(index, aspect)| {
            aspect
                .covered_associations
                .iter()
                .map(move |association| ((association.listing_link_id, association.role), index))
        })
        .collect()
}

fn hidden_preserved_blockers(
    aspects: &[PendingReviewAspect],
    assignments: &[ExistingAssignmentRow],
    authorized_associations: &HashSet<CoveredListingAssociation>,
) -> Vec<String> {
    let covered = covered_association_owners(aspects);
    let mut blockers = Vec::new();
    for assignment in assignments {
        let installed_key = (
            assignment.listing_link_id,
            ListingAssociationRole::Installed,
        );
        if !covered.contains_key(&installed_key) {
            if assignment.installed_catalog_status.as_deref() != Some("approved") {
                blockers.push(format!(
                    "link {} installed catalog id {} is not approved",
                    assignment.listing_link_id, assignment.avionics_model_id
                ));
            } else if !authorized_associations.contains(&CoveredListingAssociation {
                listing_link_id: assignment.listing_link_id,
                role: ListingAssociationRole::Installed,
                avionics_model_id: assignment.avionics_model_id,
            }) {
                blockers.push(format!(
                    "link {} installed catalog id {} lacks current association authorization",
                    assignment.listing_link_id, assignment.avionics_model_id
                ));
            }
        }
        if let Some(replacement_id) = assignment.replaces_avionics_model_id {
            let replacement_key = (
                assignment.listing_link_id,
                ListingAssociationRole::Replacement,
            );
            if !covered.contains_key(&replacement_key) {
                if assignment.replacement_catalog_status.as_deref() != Some("approved") {
                    blockers.push(format!(
                        "link {} replacement catalog id {replacement_id} is not approved",
                        assignment.listing_link_id
                    ));
                } else if !authorized_associations.contains(&CoveredListingAssociation {
                    listing_link_id: assignment.listing_link_id,
                    role: ListingAssociationRole::Replacement,
                    avionics_model_id: replacement_id,
                }) {
                    blockers.push(format!(
                        "link {} replacement catalog id {replacement_id} lacks current association authorization",
                        assignment.listing_link_id
                    ));
                }
            }
        }
    }
    blockers
}

fn reviewer_correction_binding_matches_assignment(
    aspect: &PendingReviewAspect,
    association: &CoveredListingAssociation,
    assignment: &ExistingAssignmentRow,
) -> bool {
    let Some(binding) = aspect.reviewer_correction_association_binding.as_ref() else {
        return false;
    };
    binding.listing_link_id == assignment.listing_link_id
        && binding.avionics_model_id == assignment.avionics_model_id
        && binding.quantity == assignment.quantity
        && binding.configuration_action == assignment.configuration_action
        && binding.replaces_avionics_model_id == assignment.replaces_avionics_model_id
        && association.listing_link_id == binding.listing_link_id
        && match association.role {
            ListingAssociationRole::Installed => {
                association.avionics_model_id == binding.avionics_model_id
            }
            ListingAssociationRole::Replacement => {
                binding.replaces_avionics_model_id == Some(association.avionics_model_id)
            }
        }
}

fn validate_current_covered_associations(
    aspects: &[PendingReviewAspect],
    assignments: &[ExistingAssignmentRow],
) -> ReviewResult<()> {
    let aspects_by_id = aspects
        .iter()
        .map(|aspect| (aspect.id.clone(), aspect))
        .collect::<HashMap<_, _>>();
    let assignments_by_link = assignments
        .iter()
        .map(|assignment| (assignment.listing_link_id, assignment))
        .collect::<HashMap<_, _>>();
    for aspect in aspects {
        for association in &aspect.covered_associations {
            let Some(assignment) = assignments_by_link.get(&association.listing_link_id) else {
                return Err(ReviewError::Stale(format!(
                    "listing link {} covered by review aspect {} no longer exists; reload",
                    association.listing_link_id, aspect.id
                )));
            };
            let reviewer_correction_binding_matches = aspect.kind
                == REVIEWER_CORRECTED_AVIONICS_KIND
                && reviewer_correction_binding_matches_assignment(aspect, association, assignment);
            let matches = reviewer_correction_binding_matches
                || match association.role {
                    ListingAssociationRole::Installed => exact_installed_aspect_matches_assignment(
                        aspect,
                        &aspects_by_id,
                        association,
                        assignment,
                    ),
                    ListingAssociationRole::Replacement => {
                        exact_replacement_aspect_matches_assignment(
                            aspect,
                            aspects,
                            association,
                            assignment,
                        )
                    }
                };
            if !matches {
                return Err(ReviewError::Stale(format!(
                    "listing link {} covered by review aspect {} changed; reload",
                    association.listing_link_id, aspect.id
                )));
            }
        }
    }
    Ok(())
}

/// The explicit restage operation is the sole recovery boundary for stale
/// covered-link IDs. It discards the complete stale relationship component;
/// the normal preserved-association projection then recreates current aspects
/// from the locked listing links. Independent raw extraction aspects remain
/// untouched. Revision and resolution never call this helper and therefore
/// continue to fail closed on stale coverage.
fn remove_stale_covered_relationships(
    aspects: &mut Vec<PendingReviewAspect>,
    assignments: &[ExistingAssignmentRow],
) -> bool {
    let aspects_by_id = aspects
        .iter()
        .map(|aspect| (aspect.id.clone(), aspect))
        .collect::<HashMap<_, _>>();
    let assignments_by_link = assignments
        .iter()
        .map(|assignment| (assignment.listing_link_id, assignment))
        .collect::<HashMap<_, _>>();
    let mut stale = aspects
        .iter()
        .filter(|aspect| {
            if !is_synthetic_preserved_attestation_aspect(aspect) {
                return false;
            }
            aspect.covered_associations.iter().any(|association| {
                let Some(assignment) = assignments_by_link.get(&association.listing_link_id) else {
                    return true;
                };
                let exact = match association.role {
                    ListingAssociationRole::Installed => exact_installed_aspect_matches_assignment(
                        aspect,
                        &aspects_by_id,
                        association,
                        assignment,
                    ),
                    ListingAssociationRole::Replacement => {
                        exact_replacement_aspect_matches_assignment(
                            aspect,
                            aspects,
                            association,
                            assignment,
                        )
                    }
                };
                !exact
            })
        })
        .map(|aspect| aspect.id.clone())
        .collect::<HashSet<_>>();
    if stale.is_empty() {
        return false;
    }
    loop {
        let before = stale.len();
        for aspect in aspects.iter() {
            let aspect_is_stale = stale.contains(&aspect.id);
            let targets_stale = aspect
                .replacement_aspect_id
                .as_ref()
                .is_some_and(|target| stale.contains(target));
            if !aspect_is_stale && !targets_stale {
                continue;
            }
            if !is_synthetic_preserved_attestation_aspect(aspect) {
                return false;
            }
            if let Some(target) = aspect.replacement_aspect_id.as_ref() {
                let Some(target_aspect) = aspects.iter().find(|candidate| &candidate.id == target)
                else {
                    return false;
                };
                if !is_synthetic_preserved_attestation_aspect(target_aspect) {
                    return false;
                }
                stale.insert(target.clone());
            }
            stale.insert(aspect.id.clone());
        }
        if stale.len() == before {
            break;
        }
    }
    aspects.retain(|aspect| !stale.contains(&aspect.id));
    true
}

fn proposed_identity_conflicts_with_product(
    aspect: &PendingReviewAspect,
    product: &ReviewProduct,
) -> bool {
    let Some(proposed) = aspect.proposed_product.as_ref() else {
        return false;
    };
    !avionics_identities_are_typography_exact(
        &proposed.manufacturer,
        &proposed.model,
        &product.manufacturer,
        &product.model,
    )
}

fn paired_aspect_evidence(aspect: &PendingReviewAspect) -> Option<String> {
    aspect
        .source_evidence_text
        .as_ref()
        .filter(|_| aspect.source_confidence.is_some())
        .cloned()
}

fn exact_aspect_identity_evidence(
    aspect: &PendingReviewAspect,
    rendered_html: Option<&str>,
) -> Option<String> {
    let proposed = aspect.proposed_product.as_ref()?;
    paired_aspect_evidence(aspect).filter(|evidence| {
        rendered_html.is_some_and(|html| {
            validate_exact_listing_product_evidence(
                html,
                evidence,
                &proposed.manufacturer,
                &proposed.model,
            )
            .is_ok()
        })
    })
}

/// Add an explicit, hash-bound aspect for every preserved approved listing
/// association that lacks current authorization.
///
/// A replacement component can only be reviewed with its installed parent,
/// so an otherwise reusable parent is also materialized when necessary.
fn add_unauthorized_preserved_aspects(
    aspects: &mut Vec<PendingReviewAspect>,
    assignments: &[ExistingAssignmentRow],
    approved: &HashMap<i64, ReviewProduct>,
    authorized_associations: &HashSet<CoveredListingAssociation>,
) -> ReviewResult<bool> {
    fn annotate_existing_aspect(
        aspect: &mut PendingReviewAspect,
        target: &ReviewProduct,
    ) -> ReviewResult<bool> {
        let target_id = target.id.ok_or_else(|| {
            ReviewError::Conflict(
                "approved preserved product is missing its catalog id".to_string(),
            )
        })?;
        // An occurrence that explicitly disagrees with the covered catalog
        // identity already represents the required disposition of that link.
        // Keep normal use/create/discard actions so review can replace or
        // remove the stale association; do not collapse it into a
        // reuse-attestation card for the product it contradicts.
        if proposed_identity_conflicts_with_product(aspect, target) {
            return Ok(false);
        }
        let reason_changed = is_synthetic_preserved_attestation_aspect(aspect)
            && aspect.reason != PRESERVED_ASSOCIATION_REVIEW_REASON;
        if reason_changed {
            aspect.reason = PRESERVED_ASSOCIATION_REVIEW_REASON.to_string();
        }
        if aspect.reuse_attestation_target_id == Some(target_id) {
            return Ok(reason_changed);
        }
        if aspect.reuse_attestation_target_id.is_some()
            || aspect.covered_associations.len() != 1
            || aspect.covered_associations[0].avionics_model_id != target_id
        {
            return Err(ReviewError::Conflict(format!(
                "review aspect {} does not uniquely represent approved catalog id {target_id}; restage the complete listing",
                aspect.id
            )));
        }
        if let Some(proposed) = &mut aspect.proposed_product {
            if proposed.id.is_some_and(|id| id != target_id) {
                return Err(ReviewError::Conflict(format!(
                    "review aspect {} has a different explicit catalog candidate; restage the complete listing",
                    aspect.id
                )));
            }
            // An approved target is never an unreviewed promotion candidate.
            // Retain the observed identity/capability subset as grounding
            // context, but remove the overloaded candidate ID.
            proposed.id = None;
        }
        aspect.reuse_attestation_target_id = Some(target_id);
        if is_synthetic_preserved_attestation_aspect(aspect) {
            aspect.reason = PRESERVED_ASSOCIATION_REVIEW_REASON.to_string();
        }
        Ok(true)
    }

    let mut changed = false;
    let mut owners = covered_association_owners(aspects);
    for assignment in assignments {
        let installed_key = (
            assignment.listing_link_id,
            ListingAssociationRole::Installed,
        );
        let replacement_key = (
            assignment.listing_link_id,
            ListingAssociationRole::Replacement,
        );
        let installed_product = approved.get(&assignment.avionics_model_id);
        let replacement_product = assignment
            .replaces_avionics_model_id
            .and_then(|id| approved.get(&id));
        let installed_association = CoveredListingAssociation {
            listing_link_id: assignment.listing_link_id,
            role: ListingAssociationRole::Installed,
            avionics_model_id: assignment.avionics_model_id,
        };
        let installed_needs_attestation = installed_product.is_some()
            && !authorized_associations.contains(&installed_association);
        let replacement_needs_attestation =
            assignment.replaces_avionics_model_id.is_some_and(|id| {
                replacement_product.is_some()
                    && !authorized_associations.contains(&CoveredListingAssociation {
                        listing_link_id: assignment.listing_link_id,
                        role: ListingAssociationRole::Replacement,
                        avionics_model_id: id,
                    })
            });
        let installed_is_covered = owners.contains_key(&installed_key);
        let replacement_is_covered = owners.contains_key(&replacement_key);

        if installed_needs_attestation && installed_is_covered {
            let index = owners[&installed_key];
            changed |= annotate_existing_aspect(
                &mut aspects[index],
                installed_product.expect("attestation requires an approved installed product"),
            )?;
        }
        if replacement_needs_attestation && replacement_is_covered {
            let index = owners[&replacement_key];
            changed |= annotate_existing_aspect(
                &mut aspects[index],
                replacement_product.expect("attestation requires an approved replacement product"),
            )?;
        }

        if (!installed_needs_attestation || installed_is_covered)
            && (!replacement_needs_attestation || replacement_is_covered)
        {
            continue;
        }

        let parent_index = if let Some(index) = owners.get(&installed_key).copied() {
            index
        } else {
            let product = installed_product.ok_or_else(|| {
                ReviewError::Conflict(format!(
                    "preserved listing link {} has catalog id {} that is not an approved product; restage it through full avionics curation",
                    assignment.listing_link_id, assignment.avionics_model_id
                ))
            })?;
            let index = aspects.len();
            aspects.push(preserved_product_aspect(
                assignment,
                ListingAssociationRole::Installed,
                product,
                None,
            ));
            owners.insert(installed_key, index);
            changed = true;
            index
        };

        let Some(replacement_id) = assignment.replaces_avionics_model_id else {
            continue;
        };
        if replacement_needs_attestation && !replacement_is_covered {
            let product = replacement_product.ok_or_else(|| {
                ReviewError::Conflict(format!(
                    "preserved listing link {} has replacement catalog id {replacement_id} that is not an approved product; restage it through full avionics curation",
                    assignment.listing_link_id
                ))
            })?;
            let child_id = preserved_review_aspect_id(
                assignment.listing_link_id,
                ListingAssociationRole::Replacement,
            );
            {
                let parent = &mut aspects[parent_index];
                if parent.replacement_aspect_id.is_some()
                    && parent.replacement_aspect_id.as_ref() != Some(&child_id)
                {
                    return Err(ReviewError::Conflict(format!(
                        "review aspect {} already has a different replacement observation; restage the complete listing",
                        parent.id
                    )));
                }
                if parent
                    .replaces_product_id
                    .is_some_and(|target| target != replacement_id)
                {
                    return Err(ReviewError::Conflict(format!(
                        "review aspect {} replacement target changed; restage the complete listing",
                        parent.id
                    )));
                }
                parent.replaces_product_id = None;
                parent.replacement_aspect_id = Some(child_id.clone());
            }
            let child_index = aspects.len();
            aspects.push(preserved_product_aspect(
                assignment,
                ListingAssociationRole::Replacement,
                product,
                None,
            ));
            owners.insert(replacement_key, child_index);
            changed = true;
        } else if !replacement_is_covered {
            let parent = &mut aspects[parent_index];
            if parent.replacement_aspect_id.is_none() {
                parent.replaces_product_id = Some(replacement_id);
            }
        }
    }
    if changed {
        *aspects = validated_aspects(aspects)?;
    }
    Ok(changed)
}

/// Explicit, idempotent review-maintenance boundary used before a reviewer
/// loads a listing. This keeps GET read-only while ensuring every future paid
/// grounding call is represented by the persisted review hash.
pub async fn restage_unattested_preserved_products(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
) -> ReviewResult<Option<StagedPendingReview>> {
    // This initial read supplies only the optimistic-lock token and preserves
    // the public not-found/permission behavior. The payload and every mutable
    // dependency are re-read after acquiring the write transaction's locks.
    let row = load_review_row(db, listing_id).await?;
    if row.owner_user_id != owner_user_id {
        return Err(ReviewError::Permission(
            "reviewers may only restage reviews for listings they own".to_string(),
        ));
    }
    restage_pending_review_if_current(db, owner_user_id, listing_id, &row.review_payload_sha256)
        .await
}

/// Explicitly reset one pending avionics review from its complete, strict
/// retained extraction without contacting a provider.
///
/// This is intentionally separate from ordinary restage maintenance. The
/// reset first proves that every extracted occurrence already has a one-to-one
/// durable representation, so it cannot resurrect an observation that an
/// earlier run may have discarded without a disposition receipt.
pub async fn rebuild_pending_avionics_review_if_current(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    expected_review_payload_sha256: &str,
) -> ReviewResult<RebuildPendingAvionicsReview> {
    if owner_user_id <= 0 || listing_id <= 0 || !valid_sha256(expected_review_payload_sha256) {
        return Err(ReviewError::Validation(
            "listing, owner, and expected review revision are required for avionics reset"
                .to_string(),
        ));
    }
    let lock_catalog = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models WHERE catalog_status = 'approved' ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_RESTAGE_CATALOG_LOCK_SQL),
    };
    let lock_listing_children = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql("SELECT 1"),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_LISTING_CHILD_LOCK_SQL),
    };
    let postgres_review_select = format!("{REVIEW_SELECT_SQL} FOR UPDATE OF listing, review");
    let select_review = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(REVIEW_SELECT_SQL),
        DatabaseBackend::Postgres(_) => db.sql(&postgres_review_select),
    };
    let submission_select = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            r#"
            SELECT id AS submission_id, user_id, canonical_listing_id, source_url,
                   rendered_html, rendered_html_sha256, extracted_listing_json,
                   extraction_error
            FROM plugin_submissions
            WHERE id = ?
            "#,
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            r#"
            SELECT id AS submission_id, user_id, canonical_listing_id, source_url,
                   rendered_html, rendered_html_sha256, extracted_listing_json,
                   extraction_error
            FROM plugin_submissions
            WHERE id = ?
            FOR UPDATE
            "#,
        ),
    };
    let assignments_sql = db.sql(EXISTING_ASSIGNMENT_ROWS_SQL);
    let catalog_projection_sql = db.sql(
        r#"
        SELECT model.id, manufacturer.name AS manufacturer, model.name AS model,
               capability.name AS capability, model.catalog_status
        FROM avionics_models model
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        LEFT JOIN avionics_model_types membership
          ON membership.avionics_model_id = model.id
        LEFT JOIN avionics_types capability
          ON capability.id = membership.avionics_type_id
        ORDER BY model.id, capability.normalized_name, capability.id
        "#,
    );
    let approved_products_sql = db.sql(APPROVED_PRODUCT_ROWS_SQL);
    let active_collision_catalog_sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    let catalog_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let corroborations_sql = db.sql(association_authorization_rows_sql(db));
    let attested_product_ids_sql = db.sql(
        r#"
        SELECT avionics_model_id
        FROM avionics_product_reuse_attestations
        ORDER BY avionics_model_id
        "#,
    );
    let update_review = db.sql(
        r#"
        UPDATE aircraft_sale_listing_pending_reviews
        SET extraction_sha256 = ?,
            catalog_revision_sha256 = ?,
            pending_aspect_count = ?,
            review_payload_json = ?,
            review_payload_sha256 = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE listing_id = ?
          AND review_payload_sha256 = ?
        "#,
    );
    let delete_review = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_pending_reviews
        WHERE listing_id = ? AND review_payload_sha256 = ?
        "#,
    );
    let mark_incomplete = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'incomplete', ingestion_error = NULL,
            ingestion_completed_at = NULL, is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND created_by_user_id = ?
          AND ingestion_state = 'pending_review'
          AND NOT EXISTS (
            SELECT 1 FROM aircraft_sale_listing_pending_reviews review
            WHERE review.listing_id = aircraft_sale_listings.id
          )
        "#,
    );

    macro_rules! rebuild_in_transaction {
        ($pool:expr, $reuse_attestation_is_current:ident) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&lock_catalog)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&lock_listing_children)
                .execute(&mut *transaction)
                .await?;
            let row = sqlx::query_as::<_, ReviewRow>(&select_review)
                .bind(listing_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "pending review for listing {listing_id} changed or was resolved"
                    ))
                })?;
            if row.owner_user_id != owner_user_id {
                return Err(ReviewError::Permission(
                    "reviewers may only reset reviews for listings they own".to_string(),
                ));
            }
            if row.ingestion_state != "pending_review"
                || row.is_verified
                || row.review_payload_sha256 != expected_review_payload_sha256
            {
                return Err(ReviewError::Stale(
                    "pending review changed before avionics reset; reload".to_string(),
                ));
            }
            let payload = parse_payload(
                &row.review_payload_json,
                Some(&row.review_payload_sha256),
                row.pending_aspect_count,
            )?;
            if payload
                .aspects
                .iter()
                .any(|aspect| !aspect.kind.starts_with("avionics"))
            {
                transaction.rollback().await?;
                return Ok(RebuildPendingAvionicsReview::Blocked {
                    listing_id,
                    reason_code: RebuildPendingAvionicsReviewBlockReason::UnsupportedReviewState,
                });
            }
            let Some(submission_id) = row.plugin_submission_id else {
                transaction.rollback().await?;
                return Ok(RebuildPendingAvionicsReview::Blocked {
                    listing_id,
                    reason_code: RebuildPendingAvionicsReviewBlockReason::RetainedSourceMissing,
                });
            };
            let submission = sqlx::query_as::<_, RebuildSubmissionRow>(&submission_select)
                .bind(submission_id)
                .fetch_optional(&mut *transaction)
                .await?;
            let Some(submission) = submission else {
                transaction.rollback().await?;
                return Ok(RebuildPendingAvionicsReview::Blocked {
                    listing_id,
                    reason_code: RebuildPendingAvionicsReviewBlockReason::RetainedSourceMissing,
                });
            };
            if submission
                .extraction_error
                .as_deref()
                .filter(|error| !error.trim().is_empty())
                .is_some()
            {
                transaction.rollback().await?;
                return Ok(RebuildPendingAvionicsReview::Blocked {
                    listing_id,
                    reason_code: RebuildPendingAvionicsReviewBlockReason::ExtractionNotCurrent,
                });
            }
            let Some(extracted_listing_json) = submission.extracted_listing_json.as_deref() else {
                transaction.rollback().await?;
                return Ok(RebuildPendingAvionicsReview::Blocked {
                    listing_id,
                    reason_code: RebuildPendingAvionicsReviewBlockReason::RetainedSourceMissing,
                });
            };
            let occurrences =
                match validate_current_avionics_extraction(CurrentAvionicsExtraction {
                    listing_id,
                    listing_owner_user_id: row.owner_user_id,
                    listing_source_url: row.source_url.as_deref(),
                    submission_id: submission.submission_id,
                    submission_owner_user_id: submission.user_id,
                    submission_canonical_listing_id: submission.canonical_listing_id,
                    submission_source_url: &submission.source_url,
                    rendered_html: &submission.rendered_html,
                    rendered_html_sha256: &submission.rendered_html_sha256,
                    extracted_listing_json,
                }) {
                    Ok(occurrences) if !occurrences.is_empty() => occurrences,
                    Ok(_) => {
                        transaction.rollback().await?;
                        return Ok(RebuildPendingAvionicsReview::Blocked {
                            listing_id,
                            reason_code:
                                RebuildPendingAvionicsReviewBlockReason::ExtractionNotCurrent,
                        });
                    }
                    Err(_) => {
                        transaction.rollback().await?;
                        return Ok(RebuildPendingAvionicsReview::Blocked {
                            listing_id,
                            reason_code:
                                RebuildPendingAvionicsReviewBlockReason::ExtractionNotCurrent,
                        });
                    }
                };
            let assignments = sqlx::query_as::<_, ExistingAssignmentRow>(&assignments_sql)
                .bind(listing_id)
                .fetch_all(&mut *transaction)
                .await?;
            if reset_requires_reextraction(&occurrences, &assignments, &payload.aspects)?.is_some()
            {
                transaction.rollback().await?;
                return Ok(RebuildPendingAvionicsReview::Blocked {
                    listing_id,
                    reason_code:
                        RebuildPendingAvionicsReviewBlockReason::OccurrenceDispositionUnknown,
                });
            }
            validate_current_covered_associations(&payload.aspects, &assignments)?;

            let projection_rows =
                sqlx::query_as::<_, CatalogProjectionRow>(&catalog_projection_sql)
                    .fetch_all(&mut *transaction)
                    .await?;
            let catalog = catalog_projection(projection_rows);
            let approved_rows = sqlx::query_as::<_, ApprovedProductRow>(&approved_products_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let approved = approved_product_map(approved_rows);
            let attested_product_ids = sqlx::query_scalar::<_, i64>(&attested_product_ids_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let mut reuse_attested_ids = HashSet::new();
            for avionics_model_id in attested_product_ids {
                if $reuse_attestation_is_current(db, &mut transaction, avionics_model_id).await? {
                    reuse_attested_ids.insert(avionics_model_id);
                }
            }
            let active_collision_catalog_rows = sqlx::query_as::<
                _,
                ActiveCollisionCatalogFingerprintRow,
            >(&active_collision_catalog_sql)
            .fetch_all(&mut *transaction)
            .await?;
            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let catalog_products = catalog_products(catalog_rows);
            let catalog_revision_sha256 = fingerprint_catalog_products(&catalog_products);
            let catalog_product_fingerprints = catalog_product_fingerprints(&catalog_products);
            let corroboration_rows =
                sqlx::query_as::<_, AssociationAuthorizationRow>(&corroborations_sql)
                    .bind(listing_id)
                    .fetch_all(&mut *transaction)
                    .await?;
            let authorized_associations = current_authorized_associations(
                listing_id,
                &assignments,
                &corroboration_rows,
                &reuse_attested_ids,
                &active_collision_catalog_rows,
                &catalog_product_fingerprints,
            );
            let mut aspects = project_pending_review(PendingReviewProjection {
                occurrences: &occurrences,
                assignments: &assignments,
                catalog: &catalog,
                authorized_associations: &authorized_associations,
                prior_aspects: &payload.aspects,
            })?;
            remove_authorized_preserved_aspects(&mut aspects, &authorized_associations)?;
            add_unauthorized_preserved_aspects(
                &mut aspects,
                &assignments,
                &approved,
                &authorized_associations,
            )?;
            validate_current_covered_associations(&aspects, &assignments)?;
            let blockers =
                hidden_preserved_blockers(&aspects, &assignments, &authorized_associations);
            if !blockers.is_empty() {
                return Err(ReviewError::Conflict(format!(
                    "current listing avionics cannot be represented by the rebuilt review: {}",
                    blockers.join("; ")
                )));
            }
            if aspects.is_empty() {
                let deleted = sqlx::query(&delete_review)
                    .bind(listing_id)
                    .bind(expected_review_payload_sha256)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if deleted != 1 {
                    return Err(ReviewError::Stale(
                        "pending review changed while avionics reset was being committed"
                            .to_string(),
                    ));
                }
                let changed = sqlx::query(&mark_incomplete)
                    .bind(listing_id)
                    .bind(owner_user_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if changed != 1 {
                    return Err(ReviewError::Stale(
                        "listing changed while its empty rebuilt review was being cleared"
                            .to_string(),
                    ));
                }
                transaction.commit().await?;
                return Ok(RebuildPendingAvionicsReview::Rebuilt { review: None });
            }
            let serialized = serialize_review_payload(&aspects)?;
            if serialized.review_payload_sha256 != row.review_payload_sha256
                || serialized.review_payload_json != row.review_payload_json
                || serialized.pending_aspect_count != row.pending_aspect_count
                || catalog_revision_sha256 != row.catalog_revision_sha256
            {
                let changed = sqlx::query(&update_review)
                    .bind(serialized.extraction_sha256.as_str())
                    .bind(catalog_revision_sha256.as_str())
                    .bind(serialized.pending_aspect_count)
                    .bind(serialized.review_payload_json.as_str())
                    .bind(serialized.review_payload_sha256.as_str())
                    .bind(listing_id)
                    .bind(expected_review_payload_sha256)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if changed != 1 {
                    return Err(ReviewError::Stale(
                        "pending review changed while avionics reset was being committed"
                            .to_string(),
                    ));
                }
            }
            transaction.commit().await?;
            Ok(RebuildPendingAvionicsReview::Rebuilt {
                review: Some(StagedPendingReview {
                    listing_id,
                    review_payload_sha256: serialized.review_payload_sha256,
                    catalog_revision_sha256,
                    pending_aspect_count: serialized.pending_aspect_count,
                }),
            })
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            rebuild_in_transaction!(pool, reuse_attestation_is_current_sqlite)
        }
        DatabaseBackend::Postgres(pool) => {
            rebuild_in_transaction!(pool, reuse_attestation_is_current_postgres)
        }
    }
}

/// Make hidden approved-product references visible to the product review
/// queue without re-running extraction or contacting a provider.
///
/// Only owner-scoped pending listings that retain an approved product without
/// a current reuse attestation cross the hash-guarded restage boundary. A
/// second call observes the already-staged hash and performs no review
/// mutation.
pub async fn prepare_pending_product_reviews(
    db: &AppDb,
    owner_user_id: i64,
) -> ReviewResult<PreparedPendingProductReviews> {
    let rows = load_product_review_source_rows(db, owner_user_id).await?;
    let inspected_listing_count = rows.len() as i64;
    let approved = load_all_approved_product_map(db).await?;
    let reuse_attested_ids = current_reuse_attested_product_ids(db).await?;
    let mut restaged_listing_count = 0;

    for row in rows {
        let assignments = load_existing_assignments(db, row.listing_id).await?;
        let needs_product_source = assignments.iter().any(|assignment| {
            let installed_needs_source = approved.contains_key(&assignment.avionics_model_id)
                && !reuse_attested_ids.contains(&assignment.avionics_model_id);
            let replacement_needs_source =
                assignment
                    .replaces_avionics_model_id
                    .is_some_and(|product_id| {
                        approved.contains_key(&product_id)
                            && !reuse_attested_ids.contains(&product_id)
                    });
            installed_needs_source || replacement_needs_source
        });
        if !needs_product_source {
            continue;
        }

        let previous_sha256 = row.review_payload_sha256;
        let restaged =
            restage_pending_review_if_current(db, owner_user_id, row.listing_id, &previous_sha256)
                .await?;
        if restaged
            .as_ref()
            .is_none_or(|review| review.review_payload_sha256 != previous_sha256)
        {
            restaged_listing_count += 1;
        }
    }

    Ok(PreparedPendingProductReviews {
        inspected_listing_count,
        restaged_listing_count,
        catalog_revision_sha256: approved_catalog_revision_sha256(db).await?,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct PendingProductAttestationTarget {
    pub product: ReviewProduct,
    pub already_reuse_attested: bool,
    pub commit_guard: PendingProductAttestationCommitGuard,
}

#[derive(Clone, Debug)]
pub(crate) struct ExistingProductAssociationTarget {
    pub product: ReviewProduct,
    pub listing_evidence_text: String,
    pub listing_evidence_provenance: ListingEvidenceProvenance,
    pub commit: ExistingProductAssociationCommit,
}

#[derive(Clone, Debug)]
pub(crate) enum ExistingProductAssociationCommit {
    CorroboratePreserved { observation_sha256: String },
    ApproveOrdinary,
}

struct ExistingProductAssociationGlobalSnapshot {
    catalog_revision_sha256: String,
    catalog_product_fingerprints: HashMap<i64, String>,
    products: HashMap<i64, ReviewProduct>,
    reuse_attested_ids: HashSet<i64>,
    active_collision_catalog_rows: Vec<ActiveCollisionCatalogFingerprintRow>,
    resolver: ApprovedProductAssociationResolver,
}

enum ExistingProductAssociationPreflight {
    Ready(ExistingProductAssociationTarget),
    ProductAttestationRequired {
        product_id: i64,
    },
    ManualReviewRequired {
        eligibility: ProductAssociationVerificationEligibility,
        error: ReviewError,
    },
}

pub(crate) enum ExistingProductAssociationEvaluation {
    AutoVerifiable(ExistingProductAssociationTarget),
    ProductAttestationRequired {
        eligibility: ProductAssociationVerificationEligibility,
    },
    ManualReviewRequired {
        eligibility: ProductAssociationVerificationEligibility,
        error: ReviewError,
    },
}

impl ExistingProductAssociationEvaluation {
    pub(crate) fn eligibility(&self) -> ProductAssociationVerificationEligibility {
        match self {
            Self::AutoVerifiable(_) => ProductAssociationVerificationEligibility {
                status: ProductAssociationEligibilityStatus::AutoVerifiable,
                reason_code: None,
                reason: None,
            },
            Self::ProductAttestationRequired { eligibility }
            | Self::ManualReviewRequired { eligibility, .. } => eligibility.clone(),
        }
    }
}

fn manual_product_association_eligibility(
    reason_code: impl Into<String>,
    reason: impl Into<String>,
) -> ProductAssociationVerificationEligibility {
    ProductAssociationVerificationEligibility {
        status: ProductAssociationEligibilityStatus::ManualReviewRequired,
        reason_code: Some(reason_code.into()),
        reason: Some(reason.into()),
    }
}

fn product_attestation_required_eligibility(
    product_id: i64,
) -> ProductAssociationVerificationEligibility {
    ProductAssociationVerificationEligibility {
        status: ProductAssociationEligibilityStatus::ProductAttestationRequired,
        reason_code: Some("product_attestation_required".to_string()),
        reason: Some(format!(
            "Catalog product {product_id} needs one current OEM attestation before its listing associations can be verified locally."
        )),
    }
}

async fn load_existing_product_association_global_snapshot(
    db: &AppDb,
) -> ReviewResult<ExistingProductAssociationGlobalSnapshot> {
    let catalog_revision_sha256 = approved_catalog_revision_sha256(db).await?;
    let catalog_product_fingerprints = load_catalog_product_fingerprint_map(db).await?;
    let products = load_all_approved_product_map(db).await?;
    let reuse_attested_ids = current_reuse_attested_product_ids(db).await?;
    let active_collision_catalog_rows = load_active_collision_catalog_rows(db).await?;
    let resolver = ApprovedProductAssociationResolver::load_with_reuse_attested_product_ids(
        db,
        &reuse_attested_ids,
    )
    .await
    .map_err(|error| {
        ReviewError::Database(format!(
            "could not load the local approved-product association resolver: {error}"
        ))
    })?;
    Ok(ExistingProductAssociationGlobalSnapshot {
        catalog_revision_sha256,
        catalog_product_fingerprints,
        products,
        reuse_attested_ids,
        active_collision_catalog_rows,
        resolver,
    })
}

fn compact_listing_identity_spans(source: &str, model: &str) -> Vec<(usize, usize)> {
    let target = normalize_avionics_identifier(model);
    if target.len() < 3 {
        return Vec::new();
    }
    let mut normalized = String::new();
    let mut offsets = Vec::new();
    for (offset, character) in source.char_indices() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            offsets.push(offset);
        }
    }
    normalized
        .match_indices(&target)
        .filter_map(|(start, _)| {
            let Some(source_start) = offsets.get(start).copied() else {
                return None;
            };
            let Some(last) = start
                .checked_add(target.len())
                .and_then(|end| end.checked_sub(1))
                .and_then(|end| offsets.get(end))
                .copied()
            else {
                return None;
            };
            let source_end = last
                + source[last..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or_default();
            identity_span_has_boundaries(source, source_start, source_end)
                .then_some((source_start, source_end))
        })
        .collect()
}

fn exact_compact_listing_identity_is_present(source: &str, model: &str) -> bool {
    !compact_listing_identity_spans(source, model).is_empty()
}

fn listing_evidence_has_distinct_variant_suffix(
    evidence_text: &str,
    canonical_model: &str,
) -> bool {
    compact_listing_identity_spans(evidence_text, canonical_model)
        .into_iter()
        .any(|(_, source_end)| {
            let tail = &evidence_text[source_end..];
            let trimmed_tail = tail.trim_start_matches(|character: char| {
                character.is_ascii_whitespace() || matches!(character, '-' | '/')
            });
            let normalized_tail = trimmed_tail.to_ascii_lowercase();
            if normalized_tail.starts_with("p/n")
                || normalized_tail.starts_with("pn ")
                || normalized_tail.starts_with("part number")
            {
                return false;
            }
            let mut characters = tail.char_indices().peekable();
            while characters.peek().is_some_and(|(_, character)| {
                character.is_ascii_whitespace() || matches!(character, '-' | '/')
            }) {
                characters.next();
            }
            let mut suffix = String::new();
            while characters
                .peek()
                .is_some_and(|(_, character)| character.is_ascii_alphanumeric())
            {
                suffix.push(characters.next().expect("peeked suffix character exists").1);
            }
            let normalized_suffix = suffix.to_ascii_lowercase();
            matches!(
                normalized_suffix.as_str(),
                "es" | "nxi" | "xi" | "plus" | "touch" | "waas" | "sxm"
            ) || (suffix.len() == 1
                && suffix
                    .chars()
                    .all(|character| character.is_ascii_uppercase()))
        })
}

fn listing_evidence_has_ambiguous_semantic_qualifier(
    evidence_text: &str,
    canonical_model: &str,
) -> bool {
    let evidence = evidence_text.to_ascii_lowercase();
    let canonical = canonical_model.to_ascii_lowercase();
    [
        "ads-b compliant",
        "adsb compliant",
        "waas upgraded",
        "waas upgrade",
        "field upgraded",
        "modified",
        "compatible",
        "capable",
        "compliant",
    ]
    .iter()
    .any(|qualifier| evidence.contains(qualifier) && !canonical.contains(qualifier))
}

fn validate_review_product_identity_source_fields(
    identity_source_title: &str,
    identity_evidence_text: &str,
) -> ReviewResult<()> {
    let identity_source_title = identity_source_title.trim();
    if identity_source_title.is_empty() {
        return Err(ReviewError::Validation(
            "identity_source_title is required".to_string(),
        ));
    }
    if identity_source_title.chars().count() > MAX_REVIEW_PRODUCT_SOURCE_TITLE_CHARACTERS {
        return Err(ReviewError::Validation(format!(
            "identity_source_title must contain at most {MAX_REVIEW_PRODUCT_SOURCE_TITLE_CHARACTERS} characters"
        )));
    }

    let identity_evidence_text = identity_evidence_text.trim();
    if identity_evidence_text.is_empty() {
        return Err(ReviewError::Validation(
            "identity_evidence_text is required".to_string(),
        ));
    }
    if identity_evidence_text.chars().count() > MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS {
        return Err(ReviewError::Validation(format!(
            "identity_evidence_text must be an exact publisher excerpt of at most {MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS} characters"
        )));
    }
    Ok(())
}

fn validate_existing_product_verification_evidence(
    aspect_id: &ReviewAspectId,
    target: &ReviewProduct,
    identity_source_url: &str,
    identity_source_title: &str,
    identity_evidence_text: &str,
) -> ReviewResult<()> {
    let target_id = target.id.unwrap_or_default();
    let identifier = target.stable_identifier.as_ref().ok_or_else(|| {
        ReviewError::Conflict(format!(
            "approved catalog id {target_id} has no stable manufacturer identifier and must be curated before reuse"
        ))
    })?;
    authoritative_source_url(identity_source_url)?;
    validate_review_product_identity_source_fields(identity_source_title, identity_evidence_text)?;
    if !exact_product_identity_signal_is_present(
        identity_evidence_text,
        &target.model,
        &identifier.value,
    ) {
        return Err(ReviewError::Validation(format!(
            "review aspect {aspect_id} identity evidence must itself contain the complete model and manufacturer identifier at alphanumeric boundaries"
        )));
    }
    Ok(())
}

/// Read-only, cost-avoidance gate for one global product attestation.
///
/// One exact hash-bound pending association is the authorization boundary. A
/// current attestation is returned before source validation or network access
/// so retries remain both idempotent and free.
pub(crate) async fn preflight_pending_product_attestation(
    db: &AppDb,
    owner_user_id: i64,
    product_id: i64,
    listing_id: i64,
    expected_review_payload_sha256: &str,
    aspect_id: &ReviewAspectId,
    expected_catalog_revision_sha256: &str,
    identity_source_url: &str,
    identity_source_title: &str,
    identity_evidence_text: &str,
) -> ReviewResult<PendingProductAttestationTarget> {
    if product_id <= 0
        || listing_id <= 0
        || !valid_sha256(expected_review_payload_sha256)
        || !valid_sha256(expected_catalog_revision_sha256)
    {
        return Err(ReviewError::Validation(
            "product and listing IDs must be positive and review and catalog revisions must be lowercase SHA-256 hex values"
                .to_string(),
        ));
    }
    let row = load_review_row(db, listing_id).await?;
    if row.owner_user_id != owner_user_id {
        return Err(ReviewError::Permission(
            "reviewers may only attest products through listings they own".to_string(),
        ));
    }
    if row.ingestion_state != "pending_review" || row.is_verified {
        return Err(ReviewError::Stale(format!(
            "listing {listing_id} is no longer in its expected pending-review state"
        )));
    }
    if row.review_payload_sha256 != expected_review_payload_sha256 {
        return Err(ReviewError::Stale(format!(
            "listing {listing_id} review payload changed during product attestation; reload and re-evaluate"
        )));
    }
    let payload = parse_payload(
        &row.review_payload_json,
        Some(&row.review_payload_sha256),
        row.pending_aspect_count,
    )?;
    let aspect = payload
        .aspects
        .into_iter()
        .find(|aspect| &aspect.id == aspect_id)
        .ok_or_else(|| {
            ReviewError::NotFound(format!(
                "pending review aspect {aspect_id} was not found for listing {listing_id}"
            ))
        })?;
    if aspect.reuse_attestation_target_id != Some(product_id) {
        return Err(ReviewError::Conflict(format!(
            "pending review aspect {aspect_id} for listing {listing_id} does not target approved catalog id {product_id}"
        )));
    }
    let commit_guard = PendingProductAttestationCommitGuard {
        owner_user_id,
        listing_id,
        review_payload_sha256: row.review_payload_sha256,
        aspect_id: serde_json::to_value(aspect.id).map_err(|error| {
            ReviewError::Database(format!(
                "could not bind pending product aspect authorization: {error}"
            ))
        })?,
    };

    let current_catalog_revision = approved_catalog_revision_sha256(db).await?;
    if current_catalog_revision != expected_catalog_revision_sha256 {
        return Err(ReviewError::Stale(
            "approved avionics catalog changed during review; reload and re-evaluate".to_string(),
        ));
    }
    let product = load_all_approved_product_map(db)
        .await?
        .remove(&product_id)
        .ok_or_else(|| {
            ReviewError::Conflict(format!(
                "pending review targets catalog id {product_id}, which is not a current approved product"
            ))
        })?;
    let already_reuse_attested = current_reuse_attested_product_ids(db)
        .await?
        .contains(&product_id);
    if already_reuse_attested {
        return Ok(PendingProductAttestationTarget {
            product,
            already_reuse_attested,
            commit_guard,
        });
    }

    let aspect_id = ReviewAspectId::from(format!("product:{product_id}"));
    validate_existing_product_verification_evidence(
        &aspect_id,
        &product,
        identity_source_url,
        identity_source_title,
        identity_evidence_text,
    )?;
    if !reuse_source_origin_is_authorized(db, product_id, identity_source_url).await? {
        return Err(ReviewError::Conflict(format!(
            "approved catalog id {product_id} cannot be attested from this source origin; curate an active exact manufacturer source origin or correct the catalog identity before grounding"
        )));
    }
    Ok(PendingProductAttestationTarget {
        product,
        already_reuse_attested,
        commit_guard,
    })
}

/// Read-only gate for one retained-source, association-only local verification.
///
/// Product identity is a global prerequisite and must already have a current
/// reuse attestation. This function validates only the hash-bound listing
/// occurrence and never accepts or replays a product dossier.
#[cfg(test)]
async fn preflight_existing_product_association(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    aspect_id: &ReviewAspectId,
    expected_review_payload_sha256: &str,
    expected_catalog_revision_sha256: &str,
) -> ReviewResult<ExistingProductAssociationTarget> {
    let snapshot = load_existing_product_association_global_snapshot(db).await?;
    match preflight_existing_product_association_with_snapshot(
        db,
        &snapshot,
        owner_user_id,
        listing_id,
        aspect_id,
        expected_review_payload_sha256,
        expected_catalog_revision_sha256,
    )
    .await?
    {
        ExistingProductAssociationPreflight::Ready(target) => Ok(target),
        ExistingProductAssociationPreflight::ProductAttestationRequired { product_id } => {
            Err(ReviewError::Conflict(format!(
                "approved catalog id {product_id} requires global product attestation before listing associations can be verified"
            )))
        }
        ExistingProductAssociationPreflight::ManualReviewRequired { error, .. } => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn preflight_existing_product_association_with_snapshot(
    db: &AppDb,
    snapshot: &ExistingProductAssociationGlobalSnapshot,
    owner_user_id: i64,
    listing_id: i64,
    aspect_id: &ReviewAspectId,
    expected_review_payload_sha256: &str,
    expected_catalog_revision_sha256: &str,
) -> ReviewResult<ExistingProductAssociationPreflight> {
    if !valid_sha256(expected_review_payload_sha256)
        || !valid_sha256(expected_catalog_revision_sha256)
    {
        return Err(ReviewError::Validation(
            "review and catalog revisions must be lowercase SHA-256 hex values".to_string(),
        ));
    }
    let row = load_review_row(db, listing_id).await?;
    if row.owner_user_id != owner_user_id {
        return Err(ReviewError::Permission(
            "reviewers may only verify products for listings they own".to_string(),
        ));
    }
    if row.ingestion_state != "pending_review" || row.is_verified {
        return Err(ReviewError::Stale(format!(
            "listing {listing_id} is no longer in its expected pending-review state"
        )));
    }
    if row.review_payload_sha256 != expected_review_payload_sha256 {
        return Err(ReviewError::Stale(
            "review payload is stale; reload the review".to_string(),
        ));
    }
    let payload = parse_payload(
        &row.review_payload_json,
        Some(&row.review_payload_sha256),
        row.pending_aspect_count,
    )?;
    if snapshot.catalog_revision_sha256 != expected_catalog_revision_sha256 {
        return Err(ReviewError::Stale(
            "approved avionics catalog changed during review; reload and re-evaluate".to_string(),
        ));
    }
    let assignments = load_existing_assignments(db, listing_id).await?;
    validate_current_covered_associations(&payload.aspects, &assignments)?;
    let corroboration_rows = load_association_authorizations(db, listing_id).await?;
    let authorized_associations = current_authorized_associations(
        listing_id,
        &assignments,
        &corroboration_rows,
        &snapshot.reuse_attested_ids,
        &snapshot.active_collision_catalog_rows,
        &snapshot.catalog_product_fingerprints,
    );
    let hidden =
        hidden_preserved_blockers(&payload.aspects, &assignments, &authorized_associations);
    if !hidden.is_empty() {
        return Err(ReviewError::Stale(format!(
            "the pending review omits preserved avionics; restage before grounding: {}",
            hidden.join("; ")
        )));
    }
    let aspect = payload
        .aspects
        .iter()
        .find(|aspect| &aspect.id == aspect_id)
        .ok_or_else(|| ReviewError::Validation(format!("unknown review aspect {aspect_id}")))?;
    let target_id = aspect.reuse_attestation_target_id.ok_or_else(|| {
        ReviewError::Validation(format!(
            "review aspect {aspect_id} is not an existing-product verification aspect"
        ))
    })?;
    let product = snapshot.products.get(&target_id).cloned().ok_or_else(|| {
        ReviewError::Stale(format!(
            "approved catalog id {target_id} is no longer available for review aspect {aspect_id}"
        ))
    })?;
    let Some(listing_evidence_text) = aspect
        .source_evidence_text
        .as_ref()
        .filter(|text| !text.trim().is_empty())
        .cloned()
    else {
        let error = ReviewError::Validation(format!(
            "review aspect {aspect_id} has no retained raw listing evidence and cannot be corroborated automatically"
        ));
        return Ok(ExistingProductAssociationPreflight::ManualReviewRequired {
            eligibility: manual_product_association_eligibility(
                "source_evidence_missing",
                "No exact visible listing-source excerpt is retained for this association. Recover source evidence from the listing before retrying local validation.",
            ),
            error,
        });
    };
    if listing_evidence_text.len() > MAX_LISTING_EVIDENCE_CONTEXT_BYTES
        || !exact_compact_listing_identity_is_present(&listing_evidence_text, &product.model)
    {
        return Err(ReviewError::Validation(format!(
            "review aspect {aspect_id} raw listing evidence is stale, oversized, or does not contain the complete target model at alphanumeric boundaries"
        )));
    }
    if listing_evidence_has_ambiguous_semantic_qualifier(&listing_evidence_text, &product.model)
        || listing_evidence_has_distinct_variant_suffix(&listing_evidence_text, &product.model)
    {
        let error = ReviewError::Validation(format!(
            "review aspect {aspect_id} contains an unresolved identity or capability qualifier and cannot use the exact local/one-by-one fast path"
        ));
        return Ok(ExistingProductAssociationPreflight::ManualReviewRequired {
            eligibility: manual_product_association_eligibility(
                "identity_or_capability_qualifier_unresolved",
                "The listing evidence includes a model variant or capability qualifier that may identify a different product.",
            ),
            error,
        });
    }
    let listing_evidence_provenance =
        load_listing_evidence_provenance(db, &row, &listing_evidence_text).await?;
    let commit = if is_synthetic_preserved_attestation_aspect(aspect) {
        let association = aspect
            .covered_associations
            .first()
            .expect("validated synthetic aspect has exactly one association");
        let assignment = assignments
            .iter()
            .find(|assignment| assignment.listing_link_id == association.listing_link_id)
            .ok_or_else(|| {
                ReviewError::Stale(format!(
                    "listing link {} covered by review aspect {aspect_id} no longer exists",
                    association.listing_link_id
                ))
            })?;
        if association.role != ListingAssociationRole::Installed
            || assignment.configuration_action != "installed"
            || assignment.replaces_avionics_model_id.is_some()
            || assignment.quantity <= 0
            || aspect.quantity != assignment.quantity
            || assignment.avionics_model_id != target_id
            || assignment.installed_catalog_status.as_deref() != Some("approved")
            || assignment.source_notes.as_deref() != Some(listing_evidence_text.as_str())
        {
            return Err(ReviewError::Validation(format!(
                "review aspect {aspect_id} requires complete manual review: preserved-product corroboration supports only one unchanged ordinary installed association with an exact positive quantity"
            )));
        }
        ExistingProductAssociationCommit::CorroboratePreserved {
            observation_sha256: association_observation_sha256(
                listing_id,
                assignment,
                association.role,
                &listing_evidence_text,
            ),
        }
    } else {
        validate_independent_ordinary_aspect(aspect, &payload.aspects)?;
        ExistingProductAssociationCommit::ApproveOrdinary
    };
    if !snapshot.reuse_attested_ids.contains(&target_id) {
        return Ok(
            ExistingProductAssociationPreflight::ProductAttestationRequired {
                product_id: target_id,
            },
        );
    }
    Ok(ExistingProductAssociationPreflight::Ready(
        ExistingProductAssociationTarget {
            product,
            listing_evidence_text,
            listing_evidence_provenance,
            commit,
        },
    ))
}

fn manual_eligibility_from_preflight_error(
    error: &ReviewError,
) -> ProductAssociationVerificationEligibility {
    let (reason_code, reason) = match error {
        ReviewError::Stale(_) => (
            "listing_restage_required",
            "The listing review or one of its covered avionics links changed. Restage the listing before retrying.",
        ),
        ReviewError::Conflict(_) => (
            "catalog_preflight_conflict",
            "The current avionics catalog conflicts with this staged association. Review the complete listing.",
        ),
        ReviewError::Validation(_) => (
            "association_preflight_rejected",
            "The retained listing proof or association shape does not meet automatic verification requirements.",
        ),
        ReviewError::NotFound(_) => (
            "pending_association_missing",
            "This pending association is no longer available. Reload the product review.",
        ),
        ReviewError::Permission(_) => (
            "association_access_denied",
            "This pending association is not available to the current reviewer.",
        ),
        ReviewError::Database(_) => (
            "association_preflight_failed",
            "The automatic verification preflight could not be completed.",
        ),
    };
    manual_product_association_eligibility(reason_code, reason)
}

#[allow(clippy::too_many_arguments)]
async fn evaluate_existing_product_association_with_snapshot(
    db: &AppDb,
    snapshot: &ExistingProductAssociationGlobalSnapshot,
    owner_user_id: i64,
    listing_id: i64,
    aspect_id: &ReviewAspectId,
    expected_review_payload_sha256: &str,
    expected_catalog_revision_sha256: &str,
) -> ReviewResult<ExistingProductAssociationEvaluation> {
    let preflight = match preflight_existing_product_association_with_snapshot(
        db,
        snapshot,
        owner_user_id,
        listing_id,
        aspect_id,
        expected_review_payload_sha256,
        expected_catalog_revision_sha256,
    )
    .await
    {
        Ok(preflight) => preflight,
        Err(error @ ReviewError::Permission(_))
        | Err(error @ ReviewError::NotFound(_))
        | Err(error @ ReviewError::Database(_)) => return Err(error),
        Err(error) => {
            return Ok(ExistingProductAssociationEvaluation::ManualReviewRequired {
                eligibility: manual_eligibility_from_preflight_error(&error),
                error,
            });
        }
    };
    let target = match preflight {
        ExistingProductAssociationPreflight::Ready(target) => target,
        ExistingProductAssociationPreflight::ProductAttestationRequired { product_id } => {
            return Ok(
                ExistingProductAssociationEvaluation::ProductAttestationRequired {
                    eligibility: product_attestation_required_eligibility(product_id),
                },
            );
        }
        ExistingProductAssociationPreflight::ManualReviewRequired { eligibility, error } => {
            return Ok(ExistingProductAssociationEvaluation::ManualReviewRequired {
                eligibility,
                error,
            });
        }
    };
    let target_id = target
        .product
        .id
        .expect("existing-product targets come from the approved catalog snapshot");
    let request = ApprovedProductAssociationRequest {
        listing_evidence_text: target.listing_evidence_text.clone(),
        manufacturer: target.product.manufacturer.clone(),
        model: target.product.model.clone(),
        avionics_types: target.product.capabilities.clone(),
    };
    match snapshot.resolver.resolve(&request) {
        Some(approved) if approved.id == target_id => {
            Ok(ExistingProductAssociationEvaluation::AutoVerifiable(target))
        }
        Some(approved) => {
            let reason = format!(
                "Local matching selected catalog id {} instead of the hash-bound catalog id {target_id}.",
                approved.id
            );
            Ok(ExistingProductAssociationEvaluation::ManualReviewRequired {
                eligibility: manual_product_association_eligibility(
                    "different_product_detected",
                    &reason,
                ),
                error: ReviewError::Conflict(reason),
            })
        }
        None => {
            let reason = format!(
                "Retained listing evidence does not unambiguously identify current catalog id {target_id}; this association requires manual or full-listing review."
            );
            Ok(ExistingProductAssociationEvaluation::ManualReviewRequired {
                eligibility: manual_product_association_eligibility(
                    "catalog_identity_ambiguous",
                    &reason,
                ),
                error: ReviewError::Conflict(reason),
            })
        }
    }
}

pub(crate) async fn evaluate_existing_product_association(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    aspect_id: &ReviewAspectId,
    expected_review_payload_sha256: &str,
    expected_catalog_revision_sha256: &str,
) -> ReviewResult<ExistingProductAssociationEvaluation> {
    let snapshot = load_existing_product_association_global_snapshot(db).await?;
    evaluate_existing_product_association_with_snapshot(
        db,
        &snapshot,
        owner_user_id,
        listing_id,
        aspect_id,
        expected_review_payload_sha256,
        expected_catalog_revision_sha256,
    )
    .await
}

/// Evaluate several existing-product association cards against one immutable
/// catalog snapshot.
///
/// The automatic listing verifier uses this provider-free boundary so its
/// batch path applies the same exact-evidence, current-attestation, collision,
/// and association-shape rules as the one-by-one review endpoint without
/// repeatedly rebuilding the catalog resolver for every aspect.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn evaluate_existing_product_associations(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    aspect_ids: &[ReviewAspectId],
    expected_review_payload_sha256: &str,
    expected_catalog_revision_sha256: &str,
) -> ReviewResult<Vec<ExistingProductAssociationEvaluation>> {
    let snapshot = load_existing_product_association_global_snapshot(db).await?;
    let mut evaluations = Vec::with_capacity(aspect_ids.len());
    for aspect_id in aspect_ids {
        evaluations.push(
            evaluate_existing_product_association_with_snapshot(
                db,
                &snapshot,
                owner_user_id,
                listing_id,
                aspect_id,
                expected_review_payload_sha256,
                expected_catalog_revision_sha256,
            )
            .await?,
        );
    }
    Ok(evaluations)
}

pub async fn get_listing_review(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
) -> ReviewResult<ListingReviewDetail> {
    let row = load_review_row(db, listing_id).await?;
    if row.owner_user_id != owner_user_id {
        return Err(ReviewError::Permission(
            "reviewers may only access reviews for listings they own".to_string(),
        ));
    }
    // A verified product created while resolving another listing changes the
    // approved-catalog fingerprint for every staged bundle. Loading a review
    // is the explicit re-evaluation boundary: expose the current revision
    // without mutating the staged provenance row. Resolution accepts only a
    // request that still matches this current revision.
    let current_catalog_revision = approved_catalog_revision_sha256(db).await?;
    let aircraft_identity = load_aircraft_identity_status(db, owner_user_id, listing_id).await?;
    let payload = parse_payload(
        &row.review_payload_json,
        Some(&row.review_payload_sha256),
        row.pending_aspect_count,
    )?;
    let approved = load_approved_product_map(db).await?;
    let all_approved = load_all_approved_product_map(db).await?;
    let existing_assignments = load_existing_assignments(db, listing_id).await?;
    let aspects_by_id = payload
        .aspects
        .iter()
        .map(|aspect| (aspect.id.clone(), aspect))
        .collect::<HashMap<_, _>>();
    let assignments_by_link = existing_assignments
        .iter()
        .map(|assignment| (assignment.listing_link_id, assignment))
        .collect::<HashMap<_, _>>();
    let projected_suggestions = payload
        .aspects
        .iter()
        .filter_map(|aspect| {
            projected_suggested_product_id(
                aspect,
                &payload.aspects,
                &aspects_by_id,
                &assignments_by_link,
                &approved,
            )
            .map(|product_id| (aspect.id.clone(), product_id))
        })
        .collect::<HashMap<_, _>>();
    let configuration_action_editability = payload
        .aspects
        .iter()
        .map(|aspect| {
            (
                aspect.id.clone(),
                configuration_action_is_editable(aspect, &payload.aspects),
            )
        })
        .collect::<HashMap<_, _>>();
    let aspects = payload
        .aspects
        .into_iter()
        .map(|aspect| {
            let reviewer_corrected = aspect.kind == REVIEWER_CORRECTED_AVIONICS_KIND;
            let configuration_action_editable = configuration_action_editability
                .get(&aspect.id)
                .copied()
                .unwrap_or(false);
            let suggested_product = projected_suggestions
                .get(&aspect.id)
                .and_then(|id| approved.get(id))
                .cloned();
            let replacement_product = aspect
                .replaces_product_id
                .and_then(|id| approved.get(&id).cloned());
            let reuse_attestation_target = aspect
                .reuse_attestation_target_id
                .and_then(|id| all_approved.get(&id).cloned());
            let reuse_attestation_status = aspect.reuse_attestation_target_id.map(|id| {
                if approved.contains_key(&id) {
                    ProductAttestationStatus::Current
                } else {
                    ProductAttestationStatus::Required
                }
            });
            ReviewAspectView {
                id: aspect.id,
                kind: aspect.kind,
                label: aspect.label,
                observed_text: aspect.observed_text,
                required: aspect.required,
                reason: aspect.reason,
                quantity: aspect.quantity,
                configuration_action: aspect.configuration_action,
                reviewer_corrected,
                configuration_action_editable,
                source_evidence_text: aspect.source_evidence_text,
                source_confidence: aspect.source_confidence,
                replacement_aspect_id: aspect.replacement_aspect_id,
                replacement_product,
                suggested_product,
                proposed_product: aspect.proposed_product,
                reuse_attestation_target,
                reuse_attestation_status,
                // `use_verified_product` is also a catalog-search action; it
                // remains valid without a preselected suggestion.
                allowed_actions: aspect.allowed_actions,
            }
        })
        .collect();
    Ok(ListingReviewDetail {
        review: ListingReview {
            listing_id: row.listing_id,
            source_url: row.source_url,
            label: listing_label(row.model_year, &row.manufacturer, &row.model, &row.variant),
            aircraft: ReviewAircraftSummary {
                manufacturer: row.manufacturer,
                model: row.model,
                variant: row.variant,
            },
            aircraft_identity,
            registration_number: row.registration_number,
            model_year: row.model_year,
            review_payload_sha256: row.review_payload_sha256,
            catalog_revision_sha256: current_catalog_revision,
            allowed_capabilities: CURATED_AVIONICS_TYPES
                .iter()
                .map(|capability| (*capability).to_string())
                .collect(),
            aspects,
        },
    })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn index_decisions<'a>(
    payload: &PendingReviewPayload,
    decisions: &'a [ReviewDecision],
) -> ReviewResult<HashMap<ReviewAspectId, &'a ReviewDecision>> {
    let aspects = payload
        .aspects
        .iter()
        .map(|aspect| (aspect.id.clone(), aspect))
        .collect::<HashMap<_, _>>();
    let mut indexed = HashMap::new();
    for decision in decisions {
        decision.aspect_id().validate()?;
        let aspect = aspects.get(decision.aspect_id()).ok_or_else(|| {
            ReviewError::Validation(format!(
                "decision references unknown review aspect {}",
                decision.aspect_id()
            ))
        })?;
        if indexed
            .insert(decision.aspect_id().clone(), decision)
            .is_some()
        {
            return Err(ReviewError::Validation(format!(
                "review aspect {} has more than one decision",
                decision.aspect_id()
            )));
        }
        if !aspect.allowed_actions.contains(&decision.action()) {
            return Err(ReviewError::Validation(format!(
                "action {:?} is not allowed for review aspect {}",
                decision.action(),
                decision.aspect_id()
            )));
        }
        match decision {
            ReviewDecision::UseVerifiedProduct {
                avionics_model_id, ..
            } if *avionics_model_id <= 0 => {
                return Err(ReviewError::Validation(format!(
                    "review aspect {} has an invalid avionics_model_id",
                    decision.aspect_id()
                )));
            }
            ReviewDecision::Discard { reason, .. } if reason.trim().is_empty() => {
                return Err(ReviewError::Validation(format!(
                    "discard decision for review aspect {} requires a reason",
                    decision.aspect_id()
                )));
            }
            _ => {}
        }
    }
    if indexed.len() != aspects.len() {
        let missing = payload
            .aspects
            .iter()
            .filter(|aspect| !indexed.contains_key(&aspect.id))
            .map(|aspect| aspect.id.to_string())
            .collect::<Vec<_>>();
        return Err(ReviewError::Validation(format!(
            "exactly one decision is required for every review aspect; missing: {}",
            missing.join(", ")
        )));
    }
    for aspect in &payload.aspects {
        let Some(replacement_aspect_id) = &aspect.replacement_aspect_id else {
            continue;
        };
        let primary_discarded = matches!(
            indexed.get(&aspect.id).copied(),
            Some(ReviewDecision::Discard { .. })
        );
        let replacement_discarded = matches!(
            indexed.get(replacement_aspect_id).copied(),
            Some(ReviewDecision::Discard { .. })
        );
        if primary_discarded != replacement_discarded {
            return Err(ReviewError::Validation(format!(
                "review aspect {} and its replacement aspect {} must either both be accepted or both be discarded",
                aspect.id, replacement_aspect_id
            )));
        }
    }
    Ok(indexed)
}

#[derive(Clone, Debug)]
struct CreateProductSpec {
    manufacturer: String,
    model: String,
    normalized_model: String,
    capabilities: Vec<String>,
    manufacturer_identifier_kind: String,
    manufacturer_identifier: String,
    normalized_manufacturer_identifier: String,
    identity_source_url: String,
    identity_source_title: String,
    identity_evidence_text: String,
    grounded_claim_source_urls: Vec<String>,
}

#[derive(Clone, Debug)]
struct PreflightCreateProduct {
    aspect_id: ReviewAspectId,
    unreviewed_avionics_model_id: Option<i64>,
    product: CreateProductSpec,
}

fn canonical_capabilities(capabilities: &[String]) -> ReviewResult<Vec<String>> {
    if capabilities.is_empty() {
        return Err(ReviewError::Validation(
            "a verified avionics product requires at least one capability".to_string(),
        ));
    }
    let mut present = HashSet::new();
    for capability in capabilities {
        let capability = capability.trim();
        if !CURATED_AVIONICS_TYPES.contains(&capability) {
            return Err(ReviewError::Validation(format!(
                "unsupported canonical avionics capability {capability:?}"
            )));
        }
        present.insert(capability);
    }
    Ok(CURATED_AVIONICS_TYPES
        .iter()
        .copied()
        .filter(|capability| present.contains(capability))
        .map(str::to_string)
        .collect())
}

fn authoritative_source_url(value: &str) -> ReviewResult<String> {
    let value = value.trim();
    if value.chars().count() > MAX_REVIEW_PRODUCT_SOURCE_URL_CHARACTERS {
        return Err(ReviewError::Validation(format!(
            "identity_source_url must contain at most {MAX_REVIEW_PRODUCT_SOURCE_URL_CHARACTERS} characters"
        )));
    }
    let parsed = url::Url::parse(value).map_err(|_| {
        ReviewError::Validation(
            "identity_source_url must be a valid authoritative HTTPS URL".to_string(),
        )
    })?;
    if parsed.scheme() != "https" {
        return Err(ReviewError::Validation(
            "identity_source_url must use HTTPS".to_string(),
        ));
    }
    let lower = value.to_ascii_lowercase();
    if [
        "/listing/",
        "/listings/",
        "/aircraft-for-sale/",
        "/classifieds/",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Err(ReviewError::Validation(
            "identity_source_url must cite authoritative product evidence, not a sale listing"
                .to_string(),
        ));
    }
    Ok(value.to_string())
}

fn prepare_create_product(decision: &ReviewDecision) -> ReviewResult<CreateProductSpec> {
    let ReviewDecision::CreateVerifiedProduct {
        manufacturer,
        model,
        capabilities,
        manufacturer_identifier_kind,
        manufacturer_identifier,
        identity_source_url,
        identity_source_title,
        identity_evidence_text,
        grounded_claim_source_urls,
        ..
    } = decision
    else {
        return Err(ReviewError::Validation(
            "internal review decision is not a create action".to_string(),
        ));
    };
    let manufacturer = manufacturer.trim();
    let model = model.trim();
    if manufacturer.chars().count() > MAX_REVIEW_PRODUCT_IDENTITY_LABEL_CHARACTERS
        || model.chars().count() > MAX_REVIEW_PRODUCT_IDENTITY_LABEL_CHARACTERS
    {
        return Err(ReviewError::Validation(format!(
            "verified avionics manufacturer and model must each contain at most {MAX_REVIEW_PRODUCT_IDENTITY_LABEL_CHARACTERS} characters"
        )));
    }
    if !is_usable_avionics_label(manufacturer, model) {
        return Err(ReviewError::Validation(format!(
            "generic avionics labels cannot become a verified product: {manufacturer} {model}"
        )));
    }
    let normalized_manufacturer = normalize_avionics_manufacturer_name(manufacturer);
    let normalized_model = normalize_avionics_model_name(model);
    if normalized_manufacturer.is_empty() || normalized_model.is_empty() {
        return Err(ReviewError::Validation(
            "verified avionics manufacturer and model must be nonblank".to_string(),
        ));
    }
    let manufacturer_identifier_kind = manufacturer_identifier_kind.trim();
    if !matches!(
        manufacturer_identifier_kind,
        "manufacturer_part_number" | "manufacturer_model_number" | "sku"
    ) {
        return Err(ReviewError::Validation(format!(
            "unsupported manufacturer_identifier_kind {manufacturer_identifier_kind:?}"
        )));
    }
    let manufacturer_identifier = manufacturer_identifier.trim();
    if manufacturer_identifier.chars().count() > MAX_REVIEW_PRODUCT_IDENTITY_LABEL_CHARACTERS {
        return Err(ReviewError::Validation(format!(
            "manufacturer_identifier must contain at most {MAX_REVIEW_PRODUCT_IDENTITY_LABEL_CHARACTERS} characters"
        )));
    }
    let normalized_manufacturer_identifier = normalize_avionics_identifier(manufacturer_identifier);
    if manufacturer_identifier.is_empty() || normalized_manufacturer_identifier.is_empty() {
        return Err(ReviewError::Validation(
            "manufacturer_identifier must be a concrete manufacturer identifier".to_string(),
        ));
    }
    let identity_source_title = identity_source_title.trim();
    let identity_evidence_text = identity_evidence_text.trim();
    validate_review_product_identity_source_fields(identity_source_title, identity_evidence_text)?;
    if !exact_product_identity_signal_is_present(
        identity_evidence_text,
        model,
        manufacturer_identifier,
    ) {
        return Err(ReviewError::Validation(
            "identity_evidence_text must itself contain the complete model and manufacturer identifier at alphanumeric boundaries"
            .to_string(),
        ));
    }
    let mut grounded_claim_source_urls = grounded_claim_source_urls
        .iter()
        .map(|source_url| authoritative_source_url(source_url))
        .collect::<ReviewResult<Vec<_>>>()?;
    grounded_claim_source_urls.sort();
    grounded_claim_source_urls.dedup();
    Ok(CreateProductSpec {
        manufacturer: manufacturer.to_string(),
        model: model.to_string(),
        normalized_model,
        capabilities: canonical_capabilities(capabilities)?,
        manufacturer_identifier_kind: manufacturer_identifier_kind.to_string(),
        manufacturer_identifier: manufacturer_identifier.to_string(),
        normalized_manufacturer_identifier,
        identity_source_url: authoritative_source_url(identity_source_url)?,
        identity_source_title: identity_source_title.to_string(),
        identity_evidence_text: identity_evidence_text.to_string(),
        grounded_claim_source_urls,
    })
}

fn preflight_decisions(
    review: &ListingReview,
    decisions: &[ReviewDecision],
) -> ReviewResult<(Vec<(ReviewAspectId, i64)>, Vec<PreflightCreateProduct>)> {
    let aspects = review
        .aspects
        .iter()
        .map(|aspect| (aspect.id.clone(), aspect))
        .collect::<HashMap<_, _>>();
    let mut indexed = HashMap::new();
    let mut referenced_products = Vec::new();
    let mut create_products = Vec::new();

    for decision in decisions {
        decision.aspect_id().validate()?;
        let aspect = aspects.get(decision.aspect_id()).ok_or_else(|| {
            ReviewError::Validation(format!(
                "decision references unknown review aspect {}",
                decision.aspect_id()
            ))
        })?;
        if indexed
            .insert(decision.aspect_id().clone(), decision)
            .is_some()
        {
            return Err(ReviewError::Validation(format!(
                "review aspect {} has more than one decision",
                decision.aspect_id()
            )));
        }
        if !aspect.allowed_actions.contains(&decision.action()) {
            return Err(ReviewError::Validation(format!(
                "action {:?} is not allowed for review aspect {}",
                decision.action(),
                decision.aspect_id()
            )));
        }

        match decision {
            ReviewDecision::UseVerifiedProduct {
                aspect_id,
                avionics_model_id,
            } => {
                if *avionics_model_id <= 0 {
                    return Err(ReviewError::Validation(format!(
                        "review aspect {aspect_id} has an invalid avionics_model_id"
                    )));
                }
                if aspect
                    .reuse_attestation_target
                    .as_ref()
                    .and_then(|product| product.id)
                    .is_some_and(|target_id| target_id != *avionics_model_id)
                {
                    return Err(ReviewError::Validation(format!(
                        "review aspect {aspect_id} must keep its existing catalog product association"
                    )));
                }
                referenced_products.push((aspect_id.clone(), *avionics_model_id));
            }
            ReviewDecision::CreateVerifiedProduct {
                aspect_id,
                unreviewed_avionics_model_id,
                ..
            } => {
                if unreviewed_avionics_model_id.is_some_and(|id| id <= 0) {
                    return Err(ReviewError::Validation(format!(
                        "review aspect {aspect_id} has an invalid unreviewed_avionics_model_id"
                    )));
                }
                create_products.push(PreflightCreateProduct {
                    aspect_id: aspect_id.clone(),
                    unreviewed_avionics_model_id: *unreviewed_avionics_model_id,
                    product: prepare_create_product(decision)?,
                });
            }
            ReviewDecision::Discard { aspect_id, reason } => {
                if reason.trim().is_empty() {
                    return Err(ReviewError::Validation(format!(
                        "discard decision for review aspect {aspect_id} requires a reason"
                    )));
                }
            }
        }
    }

    if indexed.len() != aspects.len() {
        let missing = review
            .aspects
            .iter()
            .filter(|aspect| !indexed.contains_key(&aspect.id))
            .map(|aspect| aspect.id.to_string())
            .collect::<Vec<_>>();
        return Err(ReviewError::Validation(format!(
            "exactly one decision is required for every review aspect; missing: {}",
            missing.join(", ")
        )));
    }

    for aspect in &review.aspects {
        let Some(replacement_aspect_id) = &aspect.replacement_aspect_id else {
            continue;
        };
        let primary_discarded = matches!(
            indexed.get(&aspect.id).copied(),
            Some(ReviewDecision::Discard { .. })
        );
        let replacement_discarded = matches!(
            indexed.get(replacement_aspect_id).copied(),
            Some(ReviewDecision::Discard { .. })
        );
        if primary_discarded != replacement_discarded {
            return Err(ReviewError::Validation(format!(
                "review aspect {} and its replacement aspect {} must either both be accepted or both be discarded",
                aspect.id, replacement_aspect_id
            )));
        }
    }

    Ok((referenced_products, create_products))
}

pub(crate) async fn approved_product_is_selectable(
    db: &AppDb,
    avionics_model_id: i64,
) -> ReviewResult<bool> {
    if !current_reuse_attested_product_ids(db)
        .await?
        .contains(&avionics_model_id)
    {
        return Ok(false);
    }
    let sql = db.sql(
        r#"
        SELECT model.id
        FROM avionics_models model
        JOIN avionics_approved_product_graph_identities identity
          ON identity.avionics_model_id = model.id
        WHERE model.id = ?
          AND model.catalog_status = 'approved'
          AND EXISTS (
            SELECT 1
            FROM avionics_model_types membership
            WHERE membership.avionics_model_id = model.id
          )
        "#,
    );
    let selected = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(avionics_model_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(avionics_model_id)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(selected.is_some())
}

async fn load_catalog_identity(
    db: &AppDb,
    avionics_model_id: i64,
) -> ReviewResult<Option<CatalogIdentityRow>> {
    let sql = db.sql(
        r#"
        SELECT
          model.id,
          model.catalog_status,
          manufacturer.name AS manufacturer,
          model.name AS model,
          model.manufacturer_identifier_kind,
          model.normalized_manufacturer_identifier,
          manufacturer_scope.avionics_manufacturer_identity_id
        FROM avionics_models model
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        LEFT JOIN avionics_manufacturer_effective_memberships manufacturer_scope
          ON manufacturer_scope.avionics_manufacturer_id =
             model.avionics_manufacturer_id
        WHERE model.id = ?
        "#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as::<_, CatalogIdentityRow>(&sql)
            .bind(avionics_model_id)
            .fetch_optional(pool)
            .await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as::<_, CatalogIdentityRow>(&sql)
            .bind(avionics_model_id)
            .fetch_optional(pool)
            .await?),
    }
}

fn catalog_candidate_matches_product(
    candidate: &CatalogIdentityRow,
    product: &CreateProductSpec,
) -> bool {
    let has_concrete_identifier = candidate
        .manufacturer_identifier_kind
        .as_deref()
        .is_some_and(|kind| !kind.trim().is_empty())
        && candidate
            .normalized_manufacturer_identifier
            .as_deref()
            .is_some_and(|identifier| !normalize_avionics_identifier(identifier).is_empty());
    let exact_identifier = candidate
        .manufacturer_identifier_kind
        .as_deref()
        .is_some_and(|kind| kind.trim() == product.manufacturer_identifier_kind)
        && candidate
            .normalized_manufacturer_identifier
            .as_deref()
            .is_some_and(|identifier| {
                normalize_avionics_identifier(identifier)
                    == product.normalized_manufacturer_identifier
            });
    let exact_name = normalize_avionics_manufacturer_name(&candidate.manufacturer)
        == normalize_avionics_manufacturer_name(&product.manufacturer)
        && normalize_avionics_model_name(&candidate.model) == product.normalized_model;
    if has_concrete_identifier {
        exact_identifier
    } else {
        exact_name
    }
}

async fn approved_product_collision(
    db: &AppDb,
    product: &CreateProductSpec,
) -> ReviewResult<Option<i64>> {
    let sql = db.sql(
        r#"
        WITH submitted_manufacturer_scope AS (
          SELECT membership.avionics_manufacturer_identity_id
          FROM avionics_manufacturers manufacturer
          JOIN avionics_manufacturer_effective_memberships membership
            ON membership.avionics_manufacturer_id = manufacturer.id
          WHERE manufacturer.normalized_name = ?
          UNION
          SELECT effective.avionics_manufacturer_identity_id
          FROM avionics_manufacturer_identities identity
          JOIN avionics_manufacturer_effective_identities effective
            ON effective.identity_id = identity.id
          WHERE identity.normalized_identity_key = ?
        )
        SELECT identity.avionics_model_id
        FROM avionics_approved_product_graph_identities identity
        JOIN submitted_manufacturer_scope manufacturer_scope
          ON manufacturer_scope.avionics_manufacturer_identity_id =
             identity.avionics_manufacturer_identity_id
        JOIN avionics_models model
          ON model.id = identity.avionics_model_id
        WHERE model.catalog_status = 'approved'
          AND (
            identity.canonical_product_key = ?
            OR (
              identity.manufacturer_identifier_kind = ?
              AND identity.canonical_identifier_key = ?
            )
          )
        ORDER BY identity.avionics_model_id
        LIMIT 1
        "#,
    );
    let manufacturer_key = normalize_avionics_manufacturer_name(&product.manufacturer);
    let product_key = normalize_avionics_identifier(&product.normalized_model);
    let collision = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(manufacturer_key.as_str())
                .bind(manufacturer_key.as_str())
                .bind(product_key.as_str())
                .bind(product.manufacturer_identifier_kind.as_str())
                .bind(product.normalized_manufacturer_identifier.as_str())
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(manufacturer_key.as_str())
                .bind(manufacturer_key.as_str())
                .bind(product_key.as_str())
                .bind(product.manufacturer_identifier_kind.as_str())
                .bind(product.normalized_manufacturer_identifier.as_str())
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(collision)
}

async fn unreviewed_product_collision(
    db: &AppDb,
    product: &CreateProductSpec,
    promotion_candidate_id: Option<i64>,
) -> ReviewResult<Option<i64>> {
    let sql = db.sql(
        r#"
        WITH submitted_manufacturer_scope AS (
          SELECT membership.avionics_manufacturer_identity_id
          FROM avionics_manufacturers manufacturer
          JOIN avionics_manufacturer_effective_memberships membership
            ON membership.avionics_manufacturer_id = manufacturer.id
          WHERE manufacturer.normalized_name = ?
          UNION
          SELECT effective.avionics_manufacturer_identity_id
          FROM avionics_manufacturer_identities identity
          JOIN avionics_manufacturer_effective_identities effective
            ON effective.identity_id = identity.id
          WHERE identity.normalized_identity_key = ?
        )
        SELECT model.id
        FROM avionics_models model
        JOIN avionics_manufacturer_effective_memberships manufacturer_scope
          ON manufacturer_scope.avionics_manufacturer_id =
             model.avionics_manufacturer_id
        JOIN submitted_manufacturer_scope submitted_scope
          ON submitted_scope.avionics_manufacturer_identity_id =
             manufacturer_scope.avionics_manufacturer_identity_id
        WHERE model.catalog_status = 'unreviewed'
          AND model.manufacturer_identifier_kind = ?
          AND lower(replace(replace(replace(replace(replace(
            trim(model.normalized_manufacturer_identifier),
            ' ', ''), '-', ''), '/', ''), '.', ''), '_', '')) = ?
          AND model.id <> COALESCE(?, CAST(-1 AS BIGINT))
        ORDER BY model.id
        LIMIT 1
        "#,
    );
    let manufacturer_key = normalize_avionics_manufacturer_name(&product.manufacturer);
    let collision = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(manufacturer_key.as_str())
                .bind(manufacturer_key.as_str())
                .bind(product.manufacturer_identifier_kind.as_str())
                .bind(product.normalized_manufacturer_identifier.as_str())
                .bind(promotion_candidate_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(manufacturer_key.as_str())
                .bind(manufacturer_key.as_str())
                .bind(product.manufacturer_identifier_kind.as_str())
                .bind(product.normalized_manufacturer_identifier.as_str())
                .bind(promotion_candidate_id)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(collision)
}

async fn legacy_global_references(
    db: &AppDb,
    target_id: i64,
) -> ReviewResult<Vec<GlobalCatalogReferenceRow>> {
    let sql = db.sql(SELECT_GLOBAL_LEGACY_REFERENCES_SQL);
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as::<_, GlobalCatalogReferenceRow>(&sql)
            .bind(target_id)
            .bind(target_id)
            .bind(target_id)
            .fetch_all(pool)
            .await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as::<_, GlobalCatalogReferenceRow>(&sql)
            .bind(target_id)
            .bind(target_id)
            .bind(target_id)
            .fetch_all(pool)
            .await?),
    }
}

async fn external_legacy_references(
    db: &AppDb,
    listing_id: i64,
    target_id: i64,
) -> ReviewResult<Vec<ExternalCoveredReferenceRow>> {
    let sql = db.sql(SELECT_EXTERNAL_LEGACY_REFERENCES_SQL);
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as::<_, ExternalCoveredReferenceRow>(&sql)
            .bind(target_id)
            .bind(listing_id)
            .bind(target_id)
            .bind(listing_id)
            .fetch_all(pool)
            .await?),
        DatabaseBackend::Postgres(pool) => {
            Ok(sqlx::query_as::<_, ExternalCoveredReferenceRow>(&sql)
                .bind(target_id)
                .bind(listing_id)
                .bind(target_id)
                .bind(listing_id)
                .fetch_all(pool)
                .await?)
        }
    }
}

fn require_external_legacy_reference_coverage(
    target_id: i64,
    external_references: Vec<ExternalCoveredReferenceRow>,
) -> ReviewResult<()> {
    for reference in external_references {
        let role = match reference.association_role.as_str() {
            "installed" => ListingAssociationRole::Installed,
            "replacement" => ListingAssociationRole::Replacement,
            _ => unreachable!("legacy reference query uses fixed roles"),
        };
        let (Some(count), Some(payload_json), Some(payload_hash)) = (
            reference.pending_aspect_count,
            reference.review_payload_json.as_deref(),
            reference.review_payload_sha256.as_deref(),
        ) else {
            return Err(ReviewError::Conflict(format!(
                "cannot promote legacy catalog id {target_id}: listing {} also references it but has no complete pending review",
                reference.listing_id
            )));
        };
        let external_payload = parse_payload(payload_json, Some(payload_hash), count)?;
        let covered = external_payload.aspects.iter().any(|aspect| {
            aspect.covered_associations.iter().any(|association| {
                association.listing_link_id == reference.listing_link_id
                    && association.role == role
                    && association.avionics_model_id == target_id
            })
        });
        if !covered {
            return Err(ReviewError::Conflict(format!(
                "cannot promote legacy catalog id {target_id}: listing {} link {} is not explicitly covered by its pending review",
                reference.listing_id, reference.listing_link_id
            )));
        }
    }
    Ok(())
}

async fn preflight_legacy_promotion_references(
    db: &AppDb,
    listing_id: i64,
    target_id: i64,
) -> ReviewResult<()> {
    let global_references = legacy_global_references(db, target_id).await?;
    if !global_references.is_empty() {
        let references = global_references
            .iter()
            .map(|reference| {
                format!(
                    "{} ({})",
                    reference.reference_kind, reference.reference_count
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ReviewError::Conflict(format!(
            "cannot promote legacy catalog id {target_id}: it has non-listing catalog references ({references}); curate the global avionics catalog/reference data before resolving this listing"
        )));
    }
    require_external_legacy_reference_coverage(
        target_id,
        external_legacy_references(db, listing_id, target_id).await?,
    )
}

/// Performs the complete, read-only resolve gate before FAA preparation or
/// paid avionics grounding. The resolve transaction deliberately repeats the
/// revision, catalog, decision, and collision checks under its database locks;
/// this preflight only prevents invalid or already-stale requests from paying
/// for work that can never commit.
pub async fn preflight_listing_review_resolution(
    db: &AppDb,
    review: &ListingReview,
    request: &ResolveReviewRequest,
) -> ReviewResult<()> {
    if !valid_sha256(&request.expected_review_payload_sha256)
        || !valid_sha256(&request.expected_catalog_revision_sha256)
    {
        return Err(ReviewError::Validation(
            "review and catalog revisions must be lowercase SHA-256 hex values".to_string(),
        ));
    }

    let current_review =
        load_review_row(db, review.listing_id)
            .await
            .map_err(|error| match error {
                ReviewError::NotFound(_) => ReviewError::Stale(
                    "pending review changed or was resolved; reload the review queue".to_string(),
                ),
                other => other,
            })?;
    if current_review.review_payload_sha256 != review.review_payload_sha256
        || request.expected_review_payload_sha256 != current_review.review_payload_sha256
    {
        return Err(ReviewError::Stale(
            "review payload is stale; reload the review".to_string(),
        ));
    }
    let current_payload = parse_payload(
        &current_review.review_payload_json,
        Some(&current_review.review_payload_sha256),
        current_review.pending_aspect_count,
    )?;
    let existing_assignments = load_existing_assignments(db, review.listing_id).await?;
    let reuse_attested_ids = current_reuse_attested_product_ids(db).await?;
    let corroboration_rows = load_association_authorizations(db, review.listing_id).await?;
    let active_collision_catalog_rows = load_active_collision_catalog_rows(db).await?;
    let catalog_product_fingerprints = load_catalog_product_fingerprint_map(db).await?;
    let authorized_associations = current_authorized_associations(
        review.listing_id,
        &existing_assignments,
        &corroboration_rows,
        &reuse_attested_ids,
        &active_collision_catalog_rows,
        &catalog_product_fingerprints,
    );
    let hidden_blockers = hidden_preserved_blockers(
        &current_payload.aspects,
        &existing_assignments,
        &authorized_associations,
    );
    if !hidden_blockers.is_empty() {
        return Err(ReviewError::Stale(format!(
            "the pending review omits preserved avionics without current association authorization; restage before grounding: {}",
            hidden_blockers.join("; ")
        )));
    }

    let current_catalog_revision = approved_catalog_revision_sha256(db).await?;
    if current_catalog_revision != review.catalog_revision_sha256
        || request.expected_catalog_revision_sha256 != current_catalog_revision
    {
        return Err(ReviewError::Stale(
            "approved avionics catalog changed during review; reload and re-evaluate".to_string(),
        ));
    }

    let (referenced_products, create_products) = preflight_decisions(review, &request.decisions)?;
    let mut checked_product_ids = HashSet::new();
    for (aspect_id, avionics_model_id) in referenced_products {
        if checked_product_ids.insert(avionics_model_id)
            && !approved_product_is_selectable(db, avionics_model_id).await?
        {
            return Err(ReviewError::Validation(format!(
                "review aspect {aspect_id} references avionics catalog id {avionics_model_id}, which is not an approved verified product"
            )));
        }
    }
    let mut batch_product_keys = HashMap::<(String, String), ReviewAspectId>::new();
    let mut batch_identifier_keys = HashMap::<(String, String, String), ReviewAspectId>::new();
    for creation in &create_products {
        let pending_aspect = current_payload
            .aspects
            .iter()
            .find(|aspect| aspect.id == creation.aspect_id)
            .expect("review view came from the current immutable pending payload");
        let staged_candidate_id = pending_aspect
            .proposed_product
            .as_ref()
            .and_then(|product| product.id);
        if let (Some(staged_id), Some(candidate_id)) =
            (staged_candidate_id, creation.unreviewed_avionics_model_id)
        {
            if staged_id != candidate_id {
                return Err(ReviewError::Conflict(format!(
                    "review aspect {} was staged with unreviewed catalog candidate id {}, but the decision selected id {candidate_id}; reload or restage instead of changing the candidate implicitly",
                    creation.aspect_id,
                    staged_id
                )));
            }
        }
        let covered_target_ids = pending_aspect
            .covered_associations
            .iter()
            .map(|association| association.avionics_model_id)
            .collect::<HashSet<_>>();
        if covered_target_ids.len() > 1 {
            return Err(ReviewError::Validation(format!(
                "review aspect {} covers multiple existing products and cannot promote them as one verified identity",
                creation.aspect_id
            )));
        }
        let covered_target_id = covered_target_ids.into_iter().next();
        let promotion_candidate_id = creation
            .unreviewed_avionics_model_id
            .or(staged_candidate_id);
        if let (Some(covered_id), Some(candidate_id)) = (covered_target_id, promotion_candidate_id)
        {
            if covered_id != candidate_id {
                return Err(ReviewError::Conflict(format!(
                    "review aspect {} covers catalog id {covered_id}, but its explicit unreviewed candidate is catalog id {candidate_id}; restage the listing instead of choosing either identity implicitly",
                    creation.aspect_id
                )));
            }
        }
        if let Some(candidate_id) = promotion_candidate_id {
            let candidate = load_catalog_identity(db, candidate_id)
                .await?
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "unreviewed catalog candidate id {candidate_id} for review aspect {} no longer exists",
                        creation.aspect_id
                    ))
                })?;
            if candidate.catalog_status != "unreviewed" {
                return Err(ReviewError::Stale(format!(
                    "unreviewed catalog candidate id {candidate_id} for review aspect {} is now {}; reload the review",
                    creation.aspect_id, candidate.catalog_status
                )));
            }
            if !catalog_candidate_matches_product(&candidate, &creation.product) {
                return Err(ReviewError::Conflict(format!(
                    "unreviewed catalog candidate id {candidate_id} does not match the submitted manufacturer, model, or stable identifier for review aspect {}",
                    creation.aspect_id
                )));
            }
            preflight_legacy_promotion_references(db, review.listing_id, candidate_id).await?;
        }
        if let Some(collision) =
            unreviewed_product_collision(db, &creation.product, promotion_candidate_id).await?
        {
            return Err(ReviewError::Conflict(format!(
                "new verified product for review aspect {} collides with unreviewed catalog id {collision}; explicitly curate or consolidate the existing product",
                creation.aspect_id
            )));
        }

        let manufacturer_key = normalize_avionics_manufacturer_name(&creation.product.manufacturer);
        let product_key = normalize_avionics_identifier(&creation.product.normalized_model);
        if let Some(other_aspect_id) = batch_product_keys.insert(
            (manufacturer_key.clone(), product_key),
            creation.aspect_id.clone(),
        ) {
            return Err(ReviewError::Conflict(format!(
                "create decisions for review aspects {other_aspect_id} and {} describe the same manufacturer product; consolidate the observation before resolving",
                creation.aspect_id
            )));
        }
        if let Some(other_aspect_id) = batch_identifier_keys.insert(
            (
                manufacturer_key,
                creation.product.manufacturer_identifier_kind.clone(),
                creation.product.normalized_manufacturer_identifier.clone(),
            ),
            creation.aspect_id.clone(),
        ) {
            return Err(ReviewError::Conflict(format!(
                "create decisions for review aspects {other_aspect_id} and {} reuse the same manufacturer identifier; consolidate the observation before resolving",
                creation.aspect_id
            )));
        }
    }

    for creation in create_products {
        if let Some(avionics_model_id) = approved_product_collision(db, &creation.product).await? {
            if approved_product_is_selectable(db, avionics_model_id).await? {
                return Err(ReviewError::Conflict(format!(
                    "new verified product for review aspect {} collides with current-policy reusable catalog id {avionics_model_id}; select the existing product",
                    creation.aspect_id
                )));
            }
            // The historical approved row remains a mandatory collision
            // candidate, but cannot yet be selected. Let the server's fresh
            // grounded preview adjudicate it; an exact existing match is
            // persisted through the dedicated reuse-attestation transaction
            // and then requires a reload/use action.
        }
    }

    Ok(())
}

#[derive(Clone, Debug)]
struct PreparedAssignment {
    avionics_model_id: i64,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
}

fn merged_notes(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left.map(str::trim), right.map(str::trim)) {
        (None | Some(""), None | Some("")) => None,
        (Some(left), None | Some("")) => Some(left.to_string()),
        (None | Some(""), Some(right)) => Some(right.to_string()),
        (Some(left), Some(right)) if left == right => Some(left.to_string()),
        (Some(left), Some(right)) => Some(format!("{left}\n{right}")),
    }
}

fn conservative_confidence(left: Option<&str>, right: Option<&str>) -> Option<String> {
    let rank = |value: &str| match value {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 3,
    };
    match (left, right) {
        (Some(left), Some(right)) => Some(
            if rank(left) >= rank(right) {
                left
            } else {
                right
            }
            .to_string(),
        ),
        _ => None,
    }
}

fn merge_assignment(
    existing: &mut PreparedAssignment,
    incoming: PreparedAssignment,
) -> ReviewResult<()> {
    if existing.configuration_action != incoming.configuration_action
        || existing.replaces_avionics_model_id != incoming.replaces_avionics_model_id
    {
        return Err(ReviewError::Validation(format!(
            "verified avionics product {} has conflicting installation actions or replacement targets",
            existing.avionics_model_id
        )));
    }
    existing.quantity = existing.quantity.max(incoming.quantity);
    let reviewer_confirmed =
        existing.source == "listing_review" || incoming.source == "listing_review";
    if existing.source != incoming.source {
        existing.source = "listing_review".to_string();
    }
    existing.source_notes = merged_notes(
        existing.source_notes.as_deref(),
        incoming.source_notes.as_deref(),
    );
    existing.source_confidence = if reviewer_confirmed {
        Some("high".to_string())
    } else {
        conservative_confidence(
            existing.source_confidence.as_deref(),
            incoming.source_confidence.as_deref(),
        )
    };
    Ok(())
}

/// Applies a complete set of reviewer decisions atomically. This transaction
/// deliberately leaves the listing `incomplete` and unverified. A caller may
/// explicitly run `listings::finalize_reviewed_listing_ingestion` afterward;
/// its full enrichment checks are the only path to `ready` + `is_verified`.
pub async fn resolve_listing_review(
    db: &AppDb,
    owner_user_id: i64,
    listing_id: i64,
    request: &ResolveReviewRequest,
) -> ReviewResult<ResolvedReview> {
    if !valid_sha256(&request.expected_review_payload_sha256)
        || !valid_sha256(&request.expected_catalog_revision_sha256)
    {
        return Err(ReviewError::Validation(
            "review and catalog revisions must be lowercase SHA-256 hex values".to_string(),
        ));
    }
    let postgres_review_select = format!("{REVIEW_SELECT_SQL} FOR UPDATE");
    let review_select = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(REVIEW_SELECT_SQL),
        DatabaseBackend::Postgres(_) => db.sql(&postgres_review_select),
    };
    let lock_catalog = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models WHERE catalog_status = 'approved' ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE avionics_models, avionics_model_types, avionics_types, avionics_manufacturers, avionics_manufacturer_canonical_keys, avionics_manufacturer_identities, avionics_manufacturer_identity_memberships, avionics_manufacturer_identity_merges, avionics_manufacturer_alias_candidates, avionics_approved_product_identities, avionics_product_reuse_attestations, avionics_authoritative_source_origins, avionics_authoritative_source_origin_revocations IN SHARE ROW EXCLUSIVE MODE",
        ),
    };
    let lock_listing_children = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql("SELECT 1"),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_LISTING_CHILD_LOCK_SQL),
    };
    let catalog_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let active_collision_catalog_sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    let attested_product_ids_sql = db.sql(
        r#"
        SELECT avionics_model_id
        FROM avionics_product_reuse_attestations
        ORDER BY avionics_model_id
        "#,
    );
    let select_approved =
        db.sql("SELECT id FROM avionics_models WHERE id = ? AND catalog_status = 'approved'");
    let insert_occurrence_disposition = db.sql(INSERT_DISPOSITION_SQL);
    let select_approved_identity = db.sql(
        r#"
        SELECT avionics_manufacturer_identity_id, canonical_product_key
        FROM avionics_approved_product_graph_identities
        WHERE avionics_model_id = ?
        "#,
    );
    let select_approved_collisions = db.sql(
        r#"
        SELECT avionics_model_id
        FROM avionics_approved_product_identities
        WHERE avionics_manufacturer_identity_id = ?
          AND (
            canonical_product_key = ?
            OR (
              manufacturer_identifier_kind = ?
              AND canonical_identifier_key = ?
            )
          )
        ORDER BY avionics_model_id
        "#,
    );
    let select_unreviewed_collisions = db.sql(
        r#"
        SELECT model.id
        FROM avionics_models model
        JOIN avionics_manufacturer_effective_memberships manufacturer_scope
          ON manufacturer_scope.avionics_manufacturer_id =
             model.avionics_manufacturer_id
        WHERE manufacturer_scope.avionics_manufacturer_identity_id = ?
          AND model.catalog_status = 'unreviewed'
          AND model.manufacturer_identifier_kind = ?
          AND lower(replace(replace(replace(replace(replace(
            trim(model.normalized_manufacturer_identifier),
            ' ', ''), '-', ''), '/', ''), '.', ''), '_', '')) = ?
        ORDER BY model.id
        "#,
    );
    let select_catalog_identity = db.sql(
        r#"
        SELECT
          model.id,
          model.catalog_status,
          manufacturer.name AS manufacturer,
          model.name AS model,
          model.manufacturer_identifier_kind,
          model.normalized_manufacturer_identifier,
          manufacturer_scope.avionics_manufacturer_identity_id
        FROM avionics_models model
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        LEFT JOIN avionics_manufacturer_effective_memberships manufacturer_scope
          ON manufacturer_scope.avionics_manufacturer_id =
             model.avionics_manufacturer_id
        WHERE model.id = ?
        "#,
    );
    let select_external_legacy_references = db.sql(SELECT_EXTERNAL_LEGACY_REFERENCES_SQL);
    let select_global_legacy_references = db.sql(SELECT_GLOBAL_LEGACY_REFERENCES_SQL);
    let insert_type = db.sql(
        "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
    );
    let select_type = db.sql("SELECT id FROM avionics_types WHERE normalized_name = ?");
    let insert_model = db.sql(
        r#"
        INSERT INTO avionics_models (
          avionics_manufacturer_id,
          name,
          normalized_name,
          catalog_status,
          manufacturer_identifier_kind,
          manufacturer_identifier,
          normalized_manufacturer_identifier,
          identity_source_url,
          identity_source_title,
          identity_evidence_text,
          identity_evidence_kind,
          identity_confidence,
          catalog_reviewed_at
        ) VALUES (?, ?, ?, 'unreviewed', ?, ?, ?, ?, ?, ?,
                  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP)
        RETURNING id
        "#,
    );
    let delete_legacy_types =
        db.sql("DELETE FROM avionics_model_types WHERE avionics_model_id = ?");
    let rewrite_legacy_model = db.sql(
        r#"
        UPDATE avionics_models
        SET avionics_manufacturer_id = ?,
            name = ?,
            normalized_name = ?,
            manufacturer_identifier_kind = ?,
            manufacturer_identifier = ?,
            normalized_manufacturer_identifier = ?,
            identity_source_url = ?,
            identity_source_title = ?,
            identity_evidence_text = ?,
            identity_evidence_kind = 'authoritative_reference',
            identity_confidence = 'very_high',
            catalog_reviewed_at = CURRENT_TIMESTAMP,
            introduced_year = NULL,
            discontinued_year = NULL,
            estimated_unit_value_usd = NULL,
            value_basis = 'unreviewed',
            replacement_cost_usd = NULL,
            value_reference_year = NULL,
            value_source = NULL,
            valuation_scope = 'unit',
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND catalog_status = 'unreviewed'
        "#,
    );
    let insert_membership = db.sql(
        "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?) ON CONFLICT (avionics_model_id, avionics_type_id) DO NOTHING",
    );
    let approve_model = db.sql(
        r#"
        UPDATE avionics_models
        SET catalog_status = 'approved',
            catalog_reviewed_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND catalog_status = 'unreviewed'
          AND EXISTS (
            SELECT 1 FROM avionics_model_types membership
            WHERE membership.avionics_model_id = avionics_models.id
          )
        "#,
    );
    let select_existing_links = db.sql(EXISTING_ASSIGNMENT_ROWS_SQL);
    let select_association_corroborations = db.sql(association_authorization_rows_sql(db));
    let delete_links =
        db.sql("DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?");
    let insert_link = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics (
          aircraft_sale_listing_id,
          avionics_model_id,
          quantity,
          source,
          source_notes,
          source_confidence,
          configuration_action,
          replaces_avionics_model_id
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    );
    let delete_review = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_pending_reviews
        WHERE listing_id = ?
          AND review_payload_sha256 = ?
          AND catalog_revision_sha256 = ?
        "#,
    );
    let count_pending =
        db.sql("SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?");
    let count_invalid_links = db.sql(
        r#"
        SELECT COUNT(*)
        FROM aircraft_sale_listing_avionics link
        LEFT JOIN avionics_models installed ON installed.id = link.avionics_model_id
        LEFT JOIN avionics_models replaced ON replaced.id = link.replaces_avionics_model_id
        WHERE link.aircraft_sale_listing_id = ?
          AND (
            installed.id IS NULL
            OR installed.catalog_status <> 'approved'
            OR (
              link.replaces_avionics_model_id IS NOT NULL
              AND (replaced.id IS NULL OR replaced.catalog_status <> 'approved')
            )
          )
        "#,
    );
    let mark_incomplete = db.sql(
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'incomplete',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND created_by_user_id = ?
          AND ingestion_state = 'pending_review'
          AND NOT EXISTS (
            SELECT 1 FROM aircraft_sale_listing_pending_reviews review
            WHERE review.listing_id = aircraft_sale_listings.id
          )
        "#,
    );

    macro_rules! resolve_in_transaction {
        (
            $pool:expr,
            $admit_manufacturer:path,
            $stage_batch_alias:path,
            $refresh_reuse:path,
            $reuse_is_current:path
        ) => {{
            let mut transaction = $pool.begin().await?;
            if matches!(db.backend(), DatabaseBackend::Postgres(_)) {
                // Global PostgreSQL order: catalog/source state, child link
                // tables, then the listing/review rows selected below.
                sqlx::query(&lock_catalog)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(&lock_listing_children)
                    .execute(&mut *transaction)
                    .await?;
            }
            let row = sqlx::query_as::<_, ReviewRow>(&review_select)
                .bind(listing_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "pending review for listing {listing_id} is missing or was already resolved"
                    ))
                })?;
            if row.owner_user_id != owner_user_id {
                return Err(ReviewError::Permission(
                    "reviewers may only resolve reviews for listings they own".to_string(),
                ));
            }
            if row.ingestion_state != "pending_review" || row.is_verified {
                return Err(ReviewError::Stale(format!(
                    "listing {listing_id} is no longer in its expected pending-review state"
                )));
            }
            if row.review_payload_sha256 != request.expected_review_payload_sha256 {
                return Err(ReviewError::Stale(
                    "review payload is stale; reload the review".to_string(),
                ));
            }
            let payload = parse_payload(
                &row.review_payload_json,
                Some(&row.review_payload_sha256),
                row.pending_aspect_count,
            )?;
            let decisions = index_decisions(&payload, &request.decisions)?;

            if matches!(db.backend(), DatabaseBackend::Sqlite(_)) {
                sqlx::query(&lock_catalog)
                    .execute(&mut *transaction)
                    .await?;
            }
            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(&catalog_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let catalog_products = catalog_products(catalog_rows);
            let catalog_product_fingerprints =
                catalog_product_fingerprints(&catalog_products);
            let current_catalog_revision = fingerprint_catalog_products(&catalog_products);
            if current_catalog_revision != request.expected_catalog_revision_sha256 {
                return Err(ReviewError::Stale(
                    "approved avionics catalog changed during review; reload and re-evaluate"
                        .to_string(),
                ));
            }

            // Link replacement is a whole-listing operation. Serialize all
            // concurrent link writers before reading the exact component IDs
            // covered by this review; otherwise a late insert could be swept
            // up by the delete-and-rebuild below.
            if matches!(db.backend(), DatabaseBackend::Sqlite(_)) {
                sqlx::query(&lock_listing_children)
                    .execute(&mut *transaction)
                    .await?;
            }

            // Automatically resolved links were admitted before the ambiguous
            // subset was staged. Retain them and merge reviewer decisions into
            // this complete current assignment set; rebuilding from pending
            // aspects alone would silently drop verified equipment.
            let existing_links =
                sqlx::query_as::<_, ExistingAssignmentRow>(&select_existing_links)
                    .bind(listing_id)
                    .fetch_all(&mut *transaction)
                    .await?;
            validate_current_covered_associations(&payload.aspects, &existing_links)?;
            let mut current_reuse_attested_ids = HashSet::new();
            let attested_product_ids =
                sqlx::query_scalar::<_, i64>(&attested_product_ids_sql)
                    .fetch_all(&mut *transaction)
                    .await?;
            for avionics_model_id in attested_product_ids {
                if $reuse_is_current(db, &mut transaction, avionics_model_id).await? {
                    current_reuse_attested_ids.insert(avionics_model_id);
                }
            }
            let active_collision_catalog_rows =
                sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(
                    &active_collision_catalog_sql,
                )
                .fetch_all(&mut *transaction)
                .await?;
            let association_corroboration_rows =
                sqlx::query_as::<_, AssociationAuthorizationRow>(
                    &select_association_corroborations,
                )
                .bind(listing_id)
                .fetch_all(&mut *transaction)
                .await?;
            let authorized_associations = current_authorized_associations(
                listing_id,
                &existing_links,
                &association_corroboration_rows,
                &current_reuse_attested_ids,
                &active_collision_catalog_rows,
                &catalog_product_fingerprints,
            );
            let covered_associations = payload
                .aspects
                .iter()
                .flat_map(|aspect| aspect.covered_associations.iter())
                .map(|association| {
                    (
                        (association.listing_link_id, association.role),
                        association.avionics_model_id,
                    )
                })
                .collect::<HashMap<_, _>>();
            let mut represented_covered_associations = HashSet::new();
            let mut assignments = BTreeMap::<i64, PreparedAssignment>::new();
            for existing in existing_links {
                let installed_key = (
                    existing.listing_link_id,
                    ListingAssociationRole::Installed,
                );
                let installed_is_covered =
                    if let Some(expected_product_id) = covered_associations.get(&installed_key) {
                        if *expected_product_id != existing.avionics_model_id {
                            return Err(ReviewError::Stale(format!(
                                "covered listing link {} installed product changed from {} to {}; restage the listing",
                                existing.listing_link_id,
                                expected_product_id,
                                existing.avionics_model_id
                            )));
                        }
                        represented_covered_associations.insert(installed_key);
                        true
                    } else {
                        false
                    };
                if existing.installed_catalog_status.as_deref() != Some("approved")
                    && !installed_is_covered
                {
                    return Err(ReviewError::Stale(format!(
                        "existing unapproved catalog id {} on listing link {} is not explicitly covered by this review payload; restage the complete listing",
                        existing.avionics_model_id, existing.listing_link_id
                    )));
                }

                let mut replacement_is_covered = false;
                if let Some(replacement_id) = existing.replaces_avionics_model_id {
                    let replacement_key = (
                        existing.listing_link_id,
                        ListingAssociationRole::Replacement,
                    );
                    if let Some(expected_product_id) = covered_associations.get(&replacement_key) {
                        if *expected_product_id != replacement_id {
                            return Err(ReviewError::Stale(format!(
                                "covered listing link {} replacement product changed from {} to {}; restage the listing",
                                existing.listing_link_id, expected_product_id, replacement_id
                            )));
                        }
                        represented_covered_associations.insert(replacement_key);
                        replacement_is_covered = true;
                    }
                    if existing.replacement_catalog_status.as_deref() != Some("approved")
                        && !replacement_is_covered
                    {
                        return Err(ReviewError::Stale(format!(
                            "existing unapproved replacement catalog id {replacement_id} on listing link {} is not explicitly covered by this review payload; restage the complete listing",
                            existing.listing_link_id
                        )));
                    }
                }
                if installed_is_covered || replacement_is_covered {
                    // A covered association is intentionally rebuilt (or
                    // discarded) from the complete decision set below. This
                    // is distinct from changing an approved catalog identity.
                    continue;
                }
                let installed_association = CoveredListingAssociation {
                    listing_link_id: existing.listing_link_id,
                    role: ListingAssociationRole::Installed,
                    avionics_model_id: existing.avionics_model_id,
                };
                if !authorized_associations.contains(&installed_association) {
                    return Err(ReviewError::Stale(format!(
                        "preserved avionics catalog id {} on listing link {} lacks current association authorization; restage and review it before resolving this review",
                        existing.avionics_model_id, existing.listing_link_id
                    )));
                }
                if let Some(replacement_id) = existing.replaces_avionics_model_id {
                    if !authorized_associations.contains(&CoveredListingAssociation {
                        listing_link_id: existing.listing_link_id,
                        role: ListingAssociationRole::Replacement,
                        avionics_model_id: replacement_id,
                    }) {
                        return Err(ReviewError::Stale(format!(
                            "preserved replacement catalog id {replacement_id} on listing link {} lacks current association authorization; restage and review it before resolving this review",
                            existing.listing_link_id
                        )));
                    }
                }
                assignments.insert(
                    existing.avionics_model_id,
                    PreparedAssignment {
                        avionics_model_id: existing.avionics_model_id,
                        quantity: existing.quantity,
                        source: existing.source,
                        source_notes: existing.source_notes,
                        source_confidence: existing.source_confidence,
                        configuration_action: existing.configuration_action,
                        replaces_avionics_model_id: existing.replaces_avionics_model_id,
                    },
                );
            }
            let covered_keys = covered_associations.keys().copied().collect::<HashSet<_>>();
            if represented_covered_associations != covered_keys {
                let stale = covered_keys
                    .difference(&represented_covered_associations)
                    .map(|(link_id, role)| format!("{link_id}:{role:?}"))
                    .collect::<Vec<_>>();
                return Err(ReviewError::Stale(format!(
                    "review payload claims listing associations that are no longer current: {stale:?}; restage the listing"
                )));
            }

            // Admit every proposed manufacturer before mutating a catalog
            // product, listing link, or the pending review. If any admission
            // discovers a possible alias, commit only those append-only
            // curation records and leave the listing transaction otherwise
            // untouched so a reviewer has an actionable pending candidate.
            let mut preflighted_creates = Vec::new();
            let mut pending_alias_aspects = Vec::new();
            for aspect in &payload.aspects {
                let decision = decisions
                    .get(&aspect.id)
                    .expect("decision coverage was validated");
                if !matches!(decision, ReviewDecision::CreateVerifiedProduct { .. }) {
                    continue;
                }
                let product = prepare_create_product(decision)?;
                let manufacturer_admission = ManufacturerProductAdmission {
                    manufacturer: product.manufacturer.as_str(),
                    model: product.model.as_str(),
                    manufacturer_identifier_kind: product.manufacturer_identifier_kind.as_str(),
                    manufacturer_identifier: product.manufacturer_identifier.as_str(),
                    evidence: ManufacturerIdentityEvidence {
                        source_url: product.identity_source_url.clone(),
                        source_title: product.identity_source_title.clone(),
                        evidence_text: product.identity_evidence_text.clone(),
                    },
                    additional_evidence_source_urls: &product.grounded_claim_source_urls,
                };
                match $admit_manufacturer(db, &mut transaction, &manufacturer_admission)
                    .await
                    .map_err(manufacturer_identity_review_error)?
                {
                    ManufacturerProductAdmissionOutcome::Admitted(scope) => {
                        preflighted_creates.push((aspect.id.clone(), product, scope));
                    }
                    ManufacturerProductAdmissionOutcome::PendingAliasReview => {
                        pending_alias_aspects.push(aspect.id.to_string());
                    }
                }
            }
            if !pending_alias_aspects.is_empty() {
                transaction.commit().await?;
                return Err(ReviewError::Conflict(format!(
                    "manufacturer identity requires human alias review before product approval for review aspect(s): {}",
                    pending_alias_aspects.join(", ")
                )));
            }

            let mut representative_manufacturers = BTreeMap::<i64, i64>::new();
            for (_, _, scope) in &preflighted_creates {
                representative_manufacturers
                    .entry(scope.avionics_manufacturer_identity_id)
                    .and_modify(|manufacturer_id| {
                        *manufacturer_id =
                            (*manufacturer_id).min(scope.avionics_manufacturer_id);
                    })
                    .or_insert(scope.avionics_manufacturer_id);
            }
            let mut same_identity_duplicates = Vec::new();
            let mut cross_identity_collisions =
                BTreeMap::<(i64, i64), (String, Vec<String>)>::new();
            for left_index in 0..preflighted_creates.len() {
                for right_index in (left_index + 1)..preflighted_creates.len() {
                    let (left_aspect_id, left_product, left_scope) =
                        &preflighted_creates[left_index];
                    let (right_aspect_id, right_product, right_scope) =
                        &preflighted_creates[right_index];
                    let exact_stable_identifier =
                        left_product.manufacturer_identifier_kind
                            == right_product.manufacturer_identifier_kind
                            && left_scope.canonical_identifier_key
                                == right_scope.canonical_identifier_key;
                    let exact_product_name = left_scope.canonical_product_key
                        == right_scope.canonical_product_key;
                    if !exact_stable_identifier && !exact_product_name {
                        continue;
                    }
                    let basis = if exact_stable_identifier {
                        "exact_stable_identifier"
                    } else {
                        "exact_product_name"
                    };
                    let aspect_pair = format!("{left_aspect_id} / {right_aspect_id}");
                    if left_scope.avionics_manufacturer_identity_id
                        == right_scope.avionics_manufacturer_identity_id
                    {
                        same_identity_duplicates.push(format!(
                            "{aspect_pair} ({basis}, manufacturer identity {})",
                            left_scope.avionics_manufacturer_identity_id
                        ));
                        continue;
                    }
                    let source_identity_id = left_scope
                        .avionics_manufacturer_identity_id
                        .max(right_scope.avionics_manufacturer_identity_id);
                    let target_identity_id = left_scope
                        .avionics_manufacturer_identity_id
                        .min(right_scope.avionics_manufacturer_identity_id);
                    let source_manufacturer_id = *representative_manufacturers
                        .get(&source_identity_id)
                        .expect("every admitted identity has a representative manufacturer");
                    let entry = cross_identity_collisions
                        .entry((source_manufacturer_id, target_identity_id))
                        .or_insert_with(|| (basis.to_string(), Vec::new()));
                    if exact_stable_identifier {
                        entry.0 = "exact_stable_identifier".to_string();
                    }
                    entry.1.push(aspect_pair);
                }
            }
            if !cross_identity_collisions.is_empty() {
                let mut descriptions = Vec::new();
                for (
                    (source_manufacturer_id, target_identity_id),
                    (basis, aspect_pairs),
                ) in &cross_identity_collisions
                {
                    $stage_batch_alias(
                        db,
                        &mut transaction,
                        *source_manufacturer_id,
                        *target_identity_id,
                        basis.as_str(),
                    )
                    .await
                    .map_err(manufacturer_identity_review_error)?;
                    descriptions.push(format!(
                        "{} ({basis}, manufacturer {} -> identity {})",
                        aspect_pairs.join(", "),
                        source_manufacturer_id,
                        target_identity_id
                    ));
                }
                if !same_identity_duplicates.is_empty() {
                    descriptions.push(format!(
                        "same-identity duplicates: {}",
                        same_identity_duplicates.join(", ")
                    ));
                }
                transaction.commit().await?;
                return Err(ReviewError::Conflict(format!(
                    "review batch contains cross-manufacturer product collisions requiring human alias review: {}",
                    descriptions.join("; ")
                )));
            }
            if !same_identity_duplicates.is_empty() {
                return Err(ReviewError::Conflict(format!(
                    "review batch proposes duplicate products within the same evidence-backed manufacturer identity: {}",
                    same_identity_duplicates.join("; ")
                )));
            }
            let manufacturer_scopes = preflighted_creates
                .iter()
                .map(|(aspect_id, _, scope)| (aspect_id.clone(), scope.clone()))
                .collect::<HashMap<_, _>>();

            let mut resolved = HashMap::<ReviewAspectId, Option<i64>>::new();
            for aspect in &payload.aspects {
                let decision = decisions
                    .get(&aspect.id)
                    .expect("decision coverage was validated");
                let product_id = match decision {
                    ReviewDecision::UseVerifiedProduct {
                        avionics_model_id, ..
                    } => {
                        let approved: Option<i64> = sqlx::query_scalar(&select_approved)
                            .bind(*avionics_model_id)
                            .fetch_optional(&mut *transaction)
                            .await?;
                        let approved = approved.ok_or_else(|| {
                            ReviewError::Stale(format!(
                                "avionics catalog id {avionics_model_id} is missing or no longer approved"
                            ))
                        })?;
                        if !$reuse_is_current(db, &mut transaction, approved).await? {
                            return Err(ReviewError::Stale(format!(
                                "avionics catalog id {avionics_model_id} is not eligible for current-policy reuse; ground and re-attest it before selection"
                            )));
                        }
                        Some(approved)
                    }
                    ReviewDecision::CreateVerifiedProduct { .. } => {
                        let product = prepare_create_product(decision)?;
                        let decision_candidate_id = match decision {
                            ReviewDecision::CreateVerifiedProduct {
                                unreviewed_avionics_model_id,
                                ..
                            } => *unreviewed_avionics_model_id,
                            _ => unreachable!("matched create decision"),
                        };
                        let covered_target_ids = aspect
                            .covered_associations
                            .iter()
                            .map(|association| association.avionics_model_id)
                            .collect::<HashSet<_>>();
                        if covered_target_ids.len() > 1 {
                            return Err(ReviewError::Validation(format!(
                                "review aspect {} covers multiple existing products and cannot promote them as one verified identity",
                                aspect.id
                            )));
                        }
                        let covered_target_id = covered_target_ids.into_iter().next();
                        let staged_candidate_id = aspect
                            .proposed_product
                            .as_ref()
                            .and_then(|candidate| candidate.id);
                        if let (Some(staged_id), Some(decision_id)) =
                            (staged_candidate_id, decision_candidate_id)
                        {
                            if staged_id != decision_id {
                                return Err(ReviewError::Conflict(format!(
                                    "review aspect {} was staged with unreviewed catalog candidate id {staged_id}, but the decision selected id {decision_id}; reload or restage instead of changing the candidate implicitly",
                                    aspect.id
                                )));
                            }
                        }
                        let candidate_target_id =
                            decision_candidate_id.or(staged_candidate_id);
                        if let (Some(covered_id), Some(candidate_id)) =
                            (covered_target_id, candidate_target_id)
                        {
                            if covered_id != candidate_id {
                                return Err(ReviewError::Conflict(format!(
                                    "review aspect {} covers catalog id {covered_id}, but its explicit unreviewed candidate is catalog id {candidate_id}; restage the listing instead of choosing either identity implicitly",
                                    aspect.id
                                )));
                            }
                        }
                        let manufacturer_scope = manufacturer_scopes
                            .get(&aspect.id)
                            .expect("create decisions were preflighted");
                        let possible_target_id = candidate_target_id.or(covered_target_id);
                        let promotion_target = if let Some(target_id) = possible_target_id {
                            let target =
                                sqlx::query_as::<_, CatalogIdentityRow>(&select_catalog_identity)
                                    .bind(target_id)
                                    .fetch_optional(&mut *transaction)
                                    .await?
                                    .ok_or_else(|| {
                                        ReviewError::Stale(format!(
                                            "catalog id {target_id} disappeared while review aspect {} was open",
                                            aspect.id
                                        ))
                                    })?;
                            debug_assert_eq!(target.id, target_id);
                            if candidate_target_id.is_some()
                                && target.catalog_status != "unreviewed"
                            {
                                return Err(ReviewError::Stale(format!(
                                    "unreviewed catalog candidate id {target_id} is now {}; reload or restage the review",
                                    target.catalog_status
                                )));
                            }
                            if staged_candidate_id.is_some() {
                                let staged = aspect
                                    .proposed_product
                                    .as_ref()
                                    .expect("candidate ID came from proposed product");
                                let target_still_matches_staged =
                                    if let Some(stable_identifier) =
                                        staged.stable_identifier.as_ref()
                                    {
                                        target
                                            .manufacturer_identifier_kind
                                            .as_deref()
                                            .is_some_and(|kind| {
                                                kind.trim()
                                                    == stable_identifier.kind.trim()
                                            })
                                            && target
                                                .normalized_manufacturer_identifier
                                                .as_deref()
                                                .is_some_and(|identifier| {
                                                    normalize_avionics_identifier(identifier)
                                                        == normalize_avionics_identifier(
                                                            &stable_identifier.value,
                                                        )
                                                })
                                    } else {
                                        normalize_avionics_manufacturer_name(
                                            &target.manufacturer,
                                        ) == normalize_avionics_manufacturer_name(
                                            &staged.manufacturer,
                                        ) && normalize_avionics_model_name(&target.model)
                                            == normalize_avionics_model_name(&staged.model)
                                    };
                                if !target_still_matches_staged {
                                    return Err(ReviewError::Stale(format!(
                                        "unreviewed catalog candidate id {target_id} changed after review aspect {} was staged; reload or restage the review",
                                        aspect.id
                                    )));
                                }
                            }
                            let same_manufacturer_identity = target
                                .avionics_manufacturer_identity_id
                                .is_some_and(|identity_id| {
                                    identity_id
                                        == manufacturer_scope
                                            .avionics_manufacturer_identity_id
                                });
                            let has_concrete_identifier = target
                                .manufacturer_identifier_kind
                                .as_deref()
                                .is_some_and(|kind| !kind.trim().is_empty())
                                && target
                                    .normalized_manufacturer_identifier
                                    .as_deref()
                                    .is_some_and(|identifier| {
                                        !normalize_avionics_identifier(identifier).is_empty()
                                    });
                            let exact_stable_identifier = target
                                    .manufacturer_identifier_kind
                                    .as_deref()
                                    .is_some_and(|kind| {
                                        kind.trim()
                                            == product.manufacturer_identifier_kind
                                    })
                                && target
                                    .normalized_manufacturer_identifier
                                    .as_deref()
                                    .is_some_and(|identifier| {
                                        normalize_avionics_identifier(identifier)
                                            == product
                                                .normalized_manufacturer_identifier
                                    });
                            let exact_product_name =
                                normalize_avionics_model_name(&target.model)
                                    == product.normalized_model;
                            let exact_target_identity = same_manufacturer_identity
                                && (exact_stable_identifier
                                    || (decision_candidate_id.is_some()
                                        && !has_concrete_identifier
                                        && exact_product_name));
                            if decision_candidate_id.is_some() && !exact_target_identity {
                                return Err(ReviewError::Conflict(format!(
                                    "explicit unreviewed catalog candidate id {target_id} no longer matches the independently grounded manufacturer identity and product identity for review aspect {}",
                                    aspect.id
                                )));
                            }
                            exact_target_identity.then(|| (target_id, target.catalog_status))
                        } else {
                            None
                        };
                        let legacy_target_id = if let Some((target_id, status)) = promotion_target {
                            match status.as_str() {
                                "unreviewed" => {
                                    let global_references = sqlx::query_as::<
                                        _,
                                        GlobalCatalogReferenceRow,
                                    >(&select_global_legacy_references)
                                    .bind(target_id)
                                    .bind(target_id)
                                    .bind(target_id)
                                    .fetch_all(&mut *transaction)
                                    .await?;
                                    if !global_references.is_empty() {
                                        let references = global_references
                                            .iter()
                                            .map(|reference| {
                                                format!(
                                                    "{} ({})",
                                                    reference.reference_kind,
                                                    reference.reference_count
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        return Err(ReviewError::Conflict(format!(
                                            "cannot promote legacy catalog id {target_id}: it has non-listing catalog references ({references}); curate the global avionics catalog/reference data before resolving this listing"
                                        )));
                                    }
                                    // Promoting a legacy row changes its
                                    // identity globally. Every other listing
                                    // occurrence must therefore already be an
                                    // exact covered component in its own
                                    // pending bundle; otherwise this listing's
                                    // review could silently bless unrelated
                                    // imported associations.
                                    let external_references = sqlx::query_as::<
                                        _,
                                        ExternalCoveredReferenceRow,
                                    >(&select_external_legacy_references)
                                    .bind(target_id)
                                    .bind(listing_id)
                                    .bind(target_id)
                                    .bind(listing_id)
                                    .fetch_all(&mut *transaction)
                                    .await?;
                                    for reference in external_references {
                                        let role = match reference.association_role.as_str() {
                                            "installed" => ListingAssociationRole::Installed,
                                            "replacement" => ListingAssociationRole::Replacement,
                                            _ => unreachable!("legacy reference query uses fixed roles"),
                                        };
                                        let (Some(count), Some(payload_json), Some(payload_hash)) = (
                                            reference.pending_aspect_count,
                                            reference.review_payload_json.as_deref(),
                                            reference.review_payload_sha256.as_deref(),
                                        ) else {
                                            return Err(ReviewError::Conflict(format!(
                                                "cannot promote legacy catalog id {target_id}: listing {} also references it but has no complete pending review",
                                                reference.listing_id
                                            )));
                                        };
                                        let external_payload =
                                            parse_payload(payload_json, Some(payload_hash), count)?;
                                        let covered = external_payload.aspects.iter().any(|aspect| {
                                            aspect.covered_associations.iter().any(|association| {
                                                association.listing_link_id
                                                    == reference.listing_link_id
                                                    && association.role == role
                                                    && association.avionics_model_id == target_id
                                            })
                                        });
                                        if !covered {
                                            return Err(ReviewError::Conflict(format!(
                                                "cannot promote legacy catalog id {target_id}: listing {} link {} is not explicitly covered by its pending review",
                                                reference.listing_id,
                                                reference.listing_link_id
                                            )));
                                        }
                                    }
                                    Some(target_id)
                                }
                                "approved" | "rejected" if candidate_target_id.is_some() => {
                                    return Err(ReviewError::Stale(format!(
                                        "unreviewed catalog candidate id {target_id} is now {}; reload or restage the review",
                                        status
                                    )));
                                }
                                // Covering an approved or rejected product
                                // adjudicates only this listing association;
                                // it must never rewrite that catalog identity.
                                "approved" | "rejected" => None,
                                _ => {
                                    return Err(ReviewError::Stale(format!(
                                        "covered catalog id {target_id} is missing or changed while the review was open"
                                    )));
                                }
                            }
                        } else {
                            None
                        };
                        let approved_collisions: Vec<i64> =
                            sqlx::query_scalar(&select_approved_collisions)
                                .bind(
                                    manufacturer_scope
                                        .avionics_manufacturer_identity_id,
                                )
                                .bind(
                                    manufacturer_scope
                                        .canonical_product_key
                                        .as_str(),
                                )
                                .bind(product.manufacturer_identifier_kind.as_str())
                                .bind(
                                    manufacturer_scope
                                        .canonical_identifier_key
                                        .as_str(),
                                )
                                .fetch_all(&mut *transaction)
                                .await?;
                        if let Some(collision) = approved_collisions.into_iter().next() {
                            return Err(ReviewError::Conflict(format!(
                                "new verified product collides with approved catalog id {collision} under the same manufacturer identity; select the existing product"
                            )));
                        }
                        let unreviewed_collisions: Vec<i64> =
                            sqlx::query_scalar(&select_unreviewed_collisions)
                                .bind(manufacturer_scope.avionics_manufacturer_identity_id)
                                .bind(product.manufacturer_identifier_kind.as_str())
                                .bind(manufacturer_scope.canonical_identifier_key.as_str())
                                .fetch_all(&mut *transaction)
                                .await?;
                        if let Some(collision) = unreviewed_collisions
                            .into_iter()
                            .find(|collision| Some(*collision) != legacy_target_id)
                        {
                            return Err(ReviewError::Conflict(format!(
                                "new verified product collides with unreviewed catalog id {collision} under the same manufacturer identity; explicitly curate or consolidate the existing product"
                            )));
                        }
                        let manufacturer_id =
                            manufacturer_scope.avionics_manufacturer_id;
                        let model_id: i64 = if let Some(target_id) = legacy_target_id {
                            // Legacy value and capability assumptions must not
                            // inherit the authority of the new identity
                            // evidence. Global references were rejected above;
                            // capabilities are explicitly replaced by this
                            // review decision.
                            sqlx::query(&delete_legacy_types)
                                .bind(target_id)
                                .execute(&mut *transaction)
                                .await?;
                            let rewritten = sqlx::query(&rewrite_legacy_model)
                                .bind(manufacturer_id)
                                .bind(product.model.as_str())
                                .bind(product.normalized_model.as_str())
                                .bind(product.manufacturer_identifier_kind.as_str())
                                .bind(product.manufacturer_identifier.as_str())
                                .bind(product.normalized_manufacturer_identifier.as_str())
                                .bind(product.identity_source_url.as_str())
                                .bind(product.identity_source_title.as_str())
                                .bind(product.identity_evidence_text.as_str())
                                .bind(target_id)
                                .execute(&mut *transaction)
                                .await?
                                .rows_affected();
                            if rewritten != 1 {
                                return Err(ReviewError::Stale(format!(
                                    "legacy catalog id {target_id} changed while it was being promoted"
                                )));
                            }
                            target_id
                        } else {
                            sqlx::query_scalar(&insert_model)
                                .bind(manufacturer_id)
                                .bind(product.model.as_str())
                                .bind(product.normalized_model.as_str())
                                .bind(product.manufacturer_identifier_kind.as_str())
                                .bind(product.manufacturer_identifier.as_str())
                                .bind(product.normalized_manufacturer_identifier.as_str())
                                .bind(product.identity_source_url.as_str())
                                .bind(product.identity_source_title.as_str())
                                .bind(product.identity_evidence_text.as_str())
                                .fetch_one(&mut *transaction)
                                .await?
                        };
                        for capability in &product.capabilities {
                            let normalized_capability = normalize_name(capability);
                            sqlx::query(&insert_type)
                                .bind(capability.as_str())
                                .bind(normalized_capability.as_str())
                                .execute(&mut *transaction)
                                .await?;
                            let type_id: i64 = sqlx::query_scalar(&select_type)
                                .bind(normalized_capability.as_str())
                                .fetch_one(&mut *transaction)
                                .await?;
                            sqlx::query(&insert_membership)
                                .bind(model_id)
                                .bind(type_id)
                                .execute(&mut *transaction)
                                .await?;
                        }
                        let approved = sqlx::query(&approve_model)
                            .bind(model_id)
                            .execute(&mut *transaction)
                            .await?
                            .rows_affected();
                        if approved != 1 {
                            return Err(ReviewError::Conflict(
                                "new catalog product could not be atomically approved".to_string(),
                            ));
                        }
                        let reuse_attested = $refresh_reuse(
                            db,
                            &mut transaction,
                            model_id,
                            product.identity_source_url.as_str(),
                        )
                        .await?;
                        if !reuse_attested {
                            return Err(ReviewError::Conflict(format!(
                                "new verified catalog id {model_id} could not be bound to a current active exact manufacturer source origin"
                            )));
                        }
                        Some(model_id)
                    }
                    ReviewDecision::Discard { .. } => None,
                };
                resolved.insert(aspect.id.clone(), product_id);
            }

            // Fresh current-schema review aspects have a deterministic source
            // slot. Record their terminal result in the same transaction as
            // the link/review mutation. Covered associations already have a
            // prior terminal receipt and are not rewritten.
            if let (Some(plugin_submission_id), Some(extracted_listing_json)) =
                (row.plugin_submission_id, row.extracted_listing_json.as_deref())
            {
                let extracted_occurrences =
                    parse_current_avionics_extraction_json(extracted_listing_json)
                        .map_err(ReviewError::Stale)?;
                let extraction_sha256 = extraction_sha256(extracted_listing_json);
                for aspect in payload
                    .aspects
                    .iter()
                    .filter(|aspect| aspect.covered_associations.is_empty())
                {
                    let Some((occurrence_index, occurrence_role)) =
                        coordinates_from_aspect_id(&aspect.id)
                    else {
                        continue;
                    };
                    let occurrence = extracted_occurrences.get(occurrence_index).ok_or_else(|| {
                        ReviewError::Stale(format!(
                            "review aspect {} no longer identifies a retained extraction occurrence",
                            aspect.id
                        ))
                    })?;
                    if occurrence_role.as_str() == "replacement" && occurrence.replaces.is_none() {
                        return Err(ReviewError::Stale(format!(
                            "review aspect {} identifies a missing replacement occurrence",
                            aspect.id
                        )));
                    }
                    let decision = decisions
                        .get(&aspect.id)
                        .expect("decision coverage was validated");
                    let (outcome, avionics_model_id, reason_code, decision_reason) = match decision {
                        ReviewDecision::Discard { reason, .. } => (
                            "discarded",
                            None,
                            "reviewer_discarded",
                            bounded_decision_reason(reason).map_err(ReviewError::Validation)?,
                        ),
                        _ => (
                            "linked",
                            resolved.get(&aspect.id).copied().flatten(),
                            "reviewer_verified_product",
                            "Reviewer linked this occurrence to a verified catalog product.",
                        ),
                    };
                    let avionics_model_id = if outcome == "linked" {
                        Some(avionics_model_id.ok_or_else(|| {
                            ReviewError::Stale(format!(
                                "review aspect {} has no resolved catalog product",
                                aspect.id
                            ))
                        })?)
                    } else {
                        None
                    };
                    let fingerprint = occurrence_fingerprint(
                        &extraction_sha256,
                        occurrence_index,
                        occurrence_role,
                    )
                    .map_err(ReviewError::Validation)?;
                    let inserted = sqlx::query(&insert_occurrence_disposition)
                        .bind(listing_id)
                        .bind(plugin_submission_id)
                        .bind(&extraction_sha256)
                        .bind(occurrence_index as i64)
                        .bind(occurrence_role.as_str())
                        .bind(fingerprint)
                        .bind(outcome)
                        .bind(avionics_model_id)
                        .bind(reason_code)
                        .bind(decision_reason)
                        .bind("manual")
                        .bind(owner_user_id)
                        .bind(DISPOSITION_POLICY_VERSION)
                        .execute(&mut *transaction)
                        .await?
                        .rows_affected();
                    if inserted != 1 {
                        return Err(ReviewError::Stale(format!(
                            "review aspect {} already has a terminal occurrence disposition",
                            aspect.id
                        )));
                    }
                }
            }

            let replacement_aspects = payload
                .aspects
                .iter()
                .filter_map(|aspect| aspect.replacement_aspect_id.clone())
                .collect::<HashSet<_>>();
            for aspect in &payload.aspects {
                if replacement_aspects.contains(&aspect.id) {
                    continue;
                }
                let Some(avionics_model_id) = resolved.get(&aspect.id).copied().flatten() else {
                    continue;
                };
                let replaces_avionics_model_id = match aspect.configuration_action.as_str() {
                    "installed" => None,
                    "replaces" | "removes" => {
                        if let Some(replaced_id) = aspect.replaces_product_id {
                            let approved: Option<i64> = sqlx::query_scalar(&select_approved)
                                .bind(replaced_id)
                                .fetch_optional(&mut *transaction)
                                .await?;
                            let approved = approved.ok_or_else(|| {
                                ReviewError::Stale(format!(
                                    "replacement catalog id {replaced_id} is missing or no longer approved"
                                ))
                            })?;
                            if !$reuse_is_current(db, &mut transaction, approved).await? {
                                return Err(ReviewError::Stale(format!(
                                    "replacement catalog id {replaced_id} is not eligible for current-policy reuse; ground and re-attest it before selection"
                                )));
                            }
                            Some(approved)
                        } else if let Some(replacement_aspect_id) = &aspect.replacement_aspect_id {
                            Some(
                                resolved
                                    .get(replacement_aspect_id)
                                    .copied()
                                    .flatten()
                                    .ok_or_else(|| {
                                        ReviewError::Validation(format!(
                                            "accepted aspect {} requires an accepted replacement aspect {}",
                                            aspect.id, replacement_aspect_id
                                        ))
                                    })?,
                            )
                        } else {
                            return Err(ReviewError::Conflict(format!(
                                "stored aspect {} lost its replacement target; restage the listing",
                                aspect.id
                            )));
                        }
                    }
                    _ => unreachable!("stored configuration action was validated"),
                };
                if aspect.configuration_action == "replaces"
                    && replaces_avionics_model_id == Some(avionics_model_id)
                {
                    return Err(ReviewError::Validation(format!(
                        "catalog id {avionics_model_id} cannot replace itself"
                    )));
                }
                if aspect.configuration_action == "removes"
                    && replaces_avionics_model_id != Some(avionics_model_id)
                {
                    return Err(ReviewError::Validation(format!(
                        "removal action must select catalog id {avionics_model_id} as both subject and displaced product"
                    )));
                }
                let incoming = PreparedAssignment {
                    avionics_model_id,
                    quantity: aspect.quantity,
                    source: "listing_review".to_string(),
                    source_notes: aspect.source_evidence_text.clone(),
                    // An explicit reviewer decision is the corroboration
                    // boundary for this listing association. Keep the source
                    // observation as notes, but make the accepted association
                    // valuation-eligible.
                    source_confidence: Some("high".to_string()),
                    configuration_action: aspect.configuration_action.clone(),
                    replaces_avionics_model_id,
                };
                if let Some(existing) = assignments.get_mut(&avionics_model_id) {
                    merge_assignment(existing, incoming)?;
                } else {
                    assignments.insert(avionics_model_id, incoming);
                }
            }

            let mut identity_keys = HashMap::<i64, String>::new();
            let mut action_model_ids = BTreeSet::new();
            for assignment in assignments.values() {
                action_model_ids.insert(assignment.avionics_model_id);
                if let Some(target) = assignment.replaces_avionics_model_id {
                    action_model_ids.insert(target);
                }
            }
            for model_id in action_model_ids {
                let identity: Option<(i64, String)> =
                    sqlx::query_as(&select_approved_identity)
                        .bind(model_id)
                        .fetch_optional(&mut *transaction)
                        .await?;
                let (manufacturer_identity_id, product_key) = identity.ok_or_else(|| {
                    ReviewError::Stale(format!(
                        "approved catalog id {model_id} has no canonical product identity"
                    ))
                })?;
                identity_keys.insert(
                    model_id,
                    approved_avionics_product_key(manufacturer_identity_id, &product_key)
                        .map_err(ReviewError::Stale)?,
                );
            }
            let canonical_actions = assignments
                .values()
                .map(|assignment| {
                    CanonicalAvionicsAction::new(
                        identity_keys
                            .get(&assignment.avionics_model_id)
                            .expect("subject identity was loaded")
                            .clone(),
                        assignment.configuration_action.clone(),
                        assignment.replaces_avionics_model_id.map(|target| {
                            identity_keys
                                .get(&target)
                                .expect("replacement identity was loaded")
                                .clone()
                        }),
                    )
                })
                .collect::<Vec<_>>();
            validate_canonical_avionics_actions(&canonical_actions)
                .map_err(ReviewError::Validation)?;

            sqlx::query(&delete_links)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            for assignment in assignments.values() {
                sqlx::query(&insert_link)
                    .bind(listing_id)
                    .bind(assignment.avionics_model_id)
                    .bind(assignment.quantity)
                    .bind(assignment.source.as_str())
                    .bind(assignment.source_notes.as_deref())
                    .bind(assignment.source_confidence.as_deref())
                    .bind(assignment.configuration_action.as_str())
                    .bind(assignment.replaces_avionics_model_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            // Candidate IDs can also occur in another listing's immutable
            // pending-review JSON, which has no foreign key. Catalog deletion
            // is therefore a separate global cleanup operation; resolving one
            // listing must never invalidate another review bundle.
            let deleted = sqlx::query(&delete_review)
                .bind(listing_id)
                .bind(row.review_payload_sha256.as_str())
                .bind(row.catalog_revision_sha256.as_str())
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if deleted != 1 {
                return Err(ReviewError::Stale(
                    "pending review changed while decisions were being applied".to_string(),
                ));
            }
            let pending: i64 = sqlx::query_scalar(&count_pending)
                .bind(listing_id)
                .fetch_one(&mut *transaction)
                .await?;
            let invalid_links: i64 = sqlx::query_scalar(&count_invalid_links)
                .bind(listing_id)
                .fetch_one(&mut *transaction)
                .await?;
            if pending != 0 || invalid_links != 0 {
                return Err(ReviewError::Stale(
                    "review resolution left unresolved publication blockers".to_string(),
                ));
            }
            let changed = sqlx::query(&mark_incomplete)
                .bind(listing_id)
                .bind(owner_user_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if changed != 1 {
                return Err(ReviewError::Stale(
                    "listing state changed while review decisions were being applied".to_string(),
                ));
            }
            transaction.commit().await?;
            Ok::<ResolvedReview, ReviewError>(ResolvedReview { listing_id })
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => resolve_in_transaction!(
            pool,
            admit_manufacturer_product_scope_sqlite,
            stage_batch_manufacturer_alias_collision_sqlite,
            refresh_reuse_attestation_sqlite,
            reuse_attestation_is_current_sqlite
        ),
        DatabaseBackend::Postgres(pool) => resolve_in_transaction!(
            pool,
            admit_manufacturer_product_scope_postgres,
            stage_batch_manufacturer_alias_collision_postgres,
            refresh_reuse_attestation_postgres,
            reuse_attestation_is_current_postgres
        ),
    }
}

/// Loads the listing state after review resolution or final enrichment.
pub async fn resolved_review_response(
    db: &AppDb,
    owner_user_id: i64,
    resolved: ResolvedReview,
) -> ReviewResult<ResolveReviewResponse> {
    let listing = get_listing(db, owner_user_id, resolved.listing_id)
        .await
        .map_err(|error| ReviewError::Database(error.to_string()))?;
    Ok(ResolveReviewResponse { listing })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use sqlx::SqlitePool;

    use crate::aircraft::faa::{
        require_listing_faa_admission, store_release, ReleaseFixtureBuilder, ReleaseMetadata,
    };
    use crate::avionics::manufacturer::{
        ensure_manufacturer_identity, ManufacturerIdentityEvidence,
    };
    use crate::avionics::reuse::{
        refresh_reuse_attestation_sqlite, reuse_attestation_is_current_sqlite,
    };

    use super::*;

    #[test]
    fn exact_association_evidence_is_scoped_to_the_bound_source_span() {
        let rendered_html = r#"
            <html><body>
              <p>Dual GDU-1044B PFD/MFD</p>
              <p>Garmin GI 275 Standby Instrument</p>
              <p>A separate GDU-1044B NXi package appears in comparison text.</p>
            </body></html>
        "#;
        validate_exact_listing_product_evidence(
            rendered_html,
            "Dual GDU-1044B PFD/MFD",
            "Garmin",
            "GDU-1044B",
        )
        .expect("an exact bound listing span is not poisoned by another page occurrence");
        validate_exact_listing_product_evidence(
            rendered_html,
            "Garmin GI 275 Standby Instrument",
            "Garmin",
            "GI 275",
        )
        .expect("the complete product spelling is exact listing evidence");
        assert!(validate_exact_listing_product_evidence(
            rendered_html,
            "A separate GDU-1044B NXi package appears in comparison text.",
            "Garmin",
            "GDU-1044B",
        )
        .is_err());
    }

    fn pending_aspect(id: &str, suggested_id: i64) -> PendingReviewAspect {
        PendingReviewAspect::avionics(
            id,
            "avionics_identity",
            "Garmin GTX 345",
            "Garmin GTX 345 transponder",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("GTX 345 shown in listing equipment".to_string()),
            Some("high".to_string()),
        )
        .with_suggested_product(ReviewProduct::verified(
            suggested_id,
            "Garmin",
            "GTX 345",
            vec!["Transponder".to_string()],
        ))
    }

    fn existing_assignment(
        listing_link_id: i64,
        avionics_model_id: i64,
        quantity: i64,
        configuration_action: &str,
        replaces_avionics_model_id: Option<i64>,
    ) -> ExistingAssignmentRow {
        ExistingAssignmentRow {
            listing_link_id,
            avionics_model_id,
            installed_manufacturer: Some("Garmin".to_string()),
            installed_model: Some(format!("Product {avionics_model_id}")),
            replacement_manufacturer: replaces_avionics_model_id.map(|_| "Garmin".to_string()),
            replacement_model: replaces_avionics_model_id.map(|id| format!("Product {id}")),
            quantity,
            source: "listing".to_string(),
            source_notes: Some("retained listing evidence".to_string()),
            source_confidence: Some("medium".to_string()),
            configuration_action: configuration_action.to_string(),
            replaces_avionics_model_id,
            installed_catalog_status: Some("approved".to_string()),
            replacement_catalog_status: replaces_avionics_model_id.map(|_| "approved".to_string()),
        }
    }

    fn approved_review_products(ids: &[i64]) -> HashMap<i64, ReviewProduct> {
        ids.iter()
            .map(|id| {
                (
                    *id,
                    ReviewProduct::verified(
                        *id,
                        "Garmin",
                        format!("Product {id}"),
                        vec!["GPS".to_string()],
                    ),
                )
            })
            .collect()
    }

    fn collision_catalog_row(id: i64) -> ActiveCollisionCatalogFingerprintRow {
        ActiveCollisionCatalogFingerprintRow {
            id,
            catalog_status: "approved".to_string(),
            effective_manufacturer_identity_id: Some(1),
            model: format!("Product {id}"),
            manufacturer_identifier_kind: Some("manufacturer_model_number".to_string()),
            manufacturer_identifier: Some(format!("PRODUCT{id}")),
        }
    }

    fn same_case_authorization_row(
        listing_id: i64,
        assignment: &ExistingAssignmentRow,
        role: ListingAssociationRole,
        product_fingerprint: &str,
        collision_rows: &[ActiveCollisionCatalogFingerprintRow],
    ) -> AssociationAuthorizationRow {
        let avionics_model_id = match role {
            ListingAssociationRole::Installed => assignment.avionics_model_id,
            ListingAssociationRole::Replacement => assignment
                .replaces_avionics_model_id
                .expect("replacement authorization requires a replacement target"),
        };
        AssociationAuthorizationRow {
            listing_link_id: assignment.listing_link_id,
            association_role: association_role_label(role).to_string(),
            avionics_model_id,
            authorization_kind: "same_case_grounded".to_string(),
            observation_sha256: association_observation_sha256(
                listing_id,
                assignment,
                role,
                assignment.source_notes.as_deref().unwrap_or_default(),
            ),
            product_fingerprint: product_fingerprint.to_string(),
            current_reuse_product_fingerprint: None,
            grounded_resolution_sha256: Some("a".repeat(64)),
            evidence_capture_is_current: true,
            policy_version: ASSOCIATION_AUTHORIZATION_POLICY_VERSION.to_string(),
            collision_closure_sha256: fingerprint_grounded_collision_closure(
                collision_rows,
                avionics_model_id,
            )
            .expect("test target must have a collision closure"),
        }
    }

    fn projected_suggestion_for_test(
        aspect_id: &str,
        aspects: &[PendingReviewAspect],
        assignments: &[ExistingAssignmentRow],
        approved: &HashMap<i64, ReviewProduct>,
    ) -> Option<i64> {
        let aspect_id = ReviewAspectId::from(aspect_id);
        let aspect = aspects
            .iter()
            .find(|aspect| aspect.id == aspect_id)
            .expect("test aspect must exist");
        let aspects_by_id = aspects
            .iter()
            .map(|aspect| (aspect.id.clone(), aspect))
            .collect::<HashMap<_, _>>();
        let assignments_by_link = assignments
            .iter()
            .map(|assignment| (assignment.listing_link_id, assignment))
            .collect::<HashMap<_, _>>();
        projected_suggested_product_id(
            aspect,
            aspects,
            &aspects_by_id,
            &assignments_by_link,
            approved,
        )
    }

    fn candidate_aspect(aspect_id: &str, candidate_id: i64, model: &str) -> PendingReviewAspect {
        PendingReviewAspect::avionics(
            aspect_id,
            "avionics_identity",
            format!("Garmin {model}"),
            format!("Garmin {model} shown in the listing"),
            "unreviewed_catalog_candidate",
            1,
            "installed",
            Some(format!("Listing identifies Garmin {model}")),
            Some("medium".to_string()),
        )
        .with_proposed_product(ReviewProduct::unreviewed_catalog_candidate(
            candidate_id,
            "Garmin",
            model,
            vec!["GPS".to_string()],
        ))
    }

    fn create_candidate_decision(aspect_id: &str, model: &str, identifier: &str) -> ReviewDecision {
        ReviewDecision::CreateVerifiedProduct {
            aspect_id: aspect_id.into(),
            unreviewed_avionics_model_id: None,
            manufacturer: "Garmin".to_string(),
            model: model.to_string(),
            capabilities: vec!["GPS".to_string()],
            manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
            manufacturer_identifier: identifier.to_string(),
            identity_source_url: "https://www.garmin.com/aviation/product".to_string(),
            identity_source_title: format!("Garmin {model} product page"),
            identity_evidence_text: format!(
                "Garmin identifies {model} by manufacturer model number {identifier}."
            ),
            grounded_claim_source_urls: Vec::new(),
        }
    }

    fn create_candidate_decision_with_catalog_id(
        aspect_id: &str,
        model: &str,
        identifier: &str,
        unreviewed_avionics_model_id: i64,
    ) -> ReviewDecision {
        let mut decision = create_candidate_decision(aspect_id, model, identifier);
        let ReviewDecision::CreateVerifiedProduct {
            unreviewed_avionics_model_id: candidate_id,
            ..
        } = &mut decision
        else {
            unreachable!("helper creates a product decision");
        };
        *candidate_id = Some(unreviewed_avionics_model_id);
        decision
    }

    fn resolve_request(
        review: &ListingReview,
        decisions: Vec<ReviewDecision>,
    ) -> ResolveReviewRequest {
        ResolveReviewRequest {
            expected_review_payload_sha256: review.review_payload_sha256.clone(),
            expected_catalog_revision_sha256: review.catalog_revision_sha256.clone(),
            finalize_listing: false,
            decisions,
        }
    }

    #[test]
    fn postgres_listing_child_lock_order_matches_finalization_contract() {
        let avionics = POSTGRES_LISTING_CHILD_LOCK_SQL
            .find("aircraft_sale_listing_avionics")
            .expect("child lock must include listing avionics");
        let pending_review = POSTGRES_LISTING_CHILD_LOCK_SQL
            .find("aircraft_sale_listing_pending_reviews")
            .expect("child lock must include pending reviews");
        assert!(
            avionics < pending_review,
            "all PostgreSQL listing writers must lock avionics before pending reviews"
        );
    }

    #[test]
    fn resolve_request_defaults_network_finalization_off() {
        let omitted: ResolveReviewRequest = serde_json::from_value(serde_json::json!({
            "review_payload_sha256": "a".repeat(64),
            "catalog_revision_sha256": "b".repeat(64),
            "decisions": []
        }))
        .expect("request without an opt-in flag should deserialize");
        assert!(!omitted.finalize_listing);

        let opted_in: ResolveReviewRequest = serde_json::from_value(serde_json::json!({
            "review_payload_sha256": "a".repeat(64),
            "catalog_revision_sha256": "b".repeat(64),
            "finalize_listing": true,
            "decisions": []
        }))
        .expect("explicit finalization opt-in should deserialize");
        assert!(opted_in.finalize_listing);
    }

    #[test]
    fn product_identity_source_fields_report_precise_character_limits() {
        assert!(validate_review_product_identity_source_fields(
            &"é".repeat(MAX_REVIEW_PRODUCT_SOURCE_TITLE_CHARACTERS),
            &"é".repeat(MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS),
        )
        .is_ok());

        for (title, evidence, expected_message) in [
            (" ", "identity", "identity_source_title is required"),
            (
                &"x".repeat(MAX_REVIEW_PRODUCT_SOURCE_TITLE_CHARACTERS + 1),
                "identity",
                "identity_source_title must contain at most 200 characters",
            ),
            ("identity", " ", "identity_evidence_text is required"),
            (
                "identity",
                &"x".repeat(MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS + 1),
                "identity_evidence_text must be an exact publisher excerpt of at most 128 characters",
            ),
        ] {
            let error =
                validate_review_product_identity_source_fields(title, evidence).unwrap_err();
            assert_eq!(error.to_string(), expected_message);
        }
    }

    #[test]
    fn verified_product_requires_one_bounded_exact_publisher_excerpt() {
        let mut missing = create_candidate_decision("identity", "GIA 63W", "GIA 63W");
        let ReviewDecision::CreateVerifiedProduct {
            identity_evidence_text,
            ..
        } = &mut missing
        else {
            unreachable!("helper creates a product decision");
        };
        identity_evidence_text.clear();
        let error = prepare_create_product(&missing).unwrap_err();
        assert_eq!(error.to_string(), "identity_evidence_text is required");

        let mut oversized = create_candidate_decision("identity", "GIA 63W", "GIA 63W");
        let ReviewDecision::CreateVerifiedProduct {
            identity_evidence_text,
            ..
        } = &mut oversized
        else {
            unreachable!("helper creates a product decision");
        };
        *identity_evidence_text = "x".repeat(MAX_DIRECT_SOURCE_RELEVANCE_ANCHOR_CHARACTERS + 1);
        let error = prepare_create_product(&oversized).unwrap_err();
        assert_eq!(
            error.to_string(),
            "identity_evidence_text must be an exact publisher excerpt of at most 128 characters"
        );

        let mut unrelated = create_candidate_decision("identity", "GIA 63W", "GIA 63W");
        let ReviewDecision::CreateVerifiedProduct {
            identity_evidence_text,
            ..
        } = &mut unrelated
        else {
            unreachable!("helper creates a product decision");
        };
        *identity_evidence_text =
            "This page also lists GIA 64W by manufacturer model number GIA 64W.".to_string();
        let error = prepare_create_product(&unrelated).unwrap_err();
        assert!(error
            .to_string()
            .contains("must itself contain the complete model"));

        let mut oversized_title = create_candidate_decision("identity", "GIA 63W", "GIA 63W");
        let ReviewDecision::CreateVerifiedProduct {
            identity_source_title,
            ..
        } = &mut oversized_title
        else {
            unreachable!("helper creates a product decision");
        };
        *identity_source_title = "x".repeat(MAX_REVIEW_PRODUCT_SOURCE_TITLE_CHARACTERS + 1);
        let error = prepare_create_product(&oversized_title).unwrap_err();
        assert_eq!(
            error.to_string(),
            "identity_source_title must contain at most 200 characters"
        );

        let client_attempt: ReviewDecision = serde_json::from_value(serde_json::json!({
            "action": "create_verified_product",
            "aspect_id": "identity",
            "manufacturer": "Garmin",
            "model": "GIA 63W",
            "capabilities": ["GPS"],
            "manufacturer_identifier_kind": "manufacturer_model_number",
            "manufacturer_identifier": "GIA 63W",
            "identity_source_url": "https://www.garmin.com/aviation/product",
            "identity_source_title": "Garmin GIA 63W product page",
            "identity_evidence_text":
                "Garmin identifies GIA 63W by manufacturer model number GIA 63W.",
            "grounded_claim_source_urls": ["https://attacker.example/fake"]
        }))
        .expect("client decision should deserialize");
        let ReviewDecision::CreateVerifiedProduct {
            grounded_claim_source_urls,
            ..
        } = client_attempt
        else {
            unreachable!("JSON creates a product decision");
        };
        assert!(
            grounded_claim_source_urls.is_empty(),
            "client input cannot populate the trusted grounding sidecar"
        );

        let mut server_grounded = create_candidate_decision("identity", "GIA 63W", "GIA 63W");
        let ReviewDecision::CreateVerifiedProduct {
            grounded_claim_source_urls,
            ..
        } = &mut server_grounded
        else {
            unreachable!("helper creates a product decision");
        };
        grounded_claim_source_urls.push(
            "https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf".to_string(),
        );
        let prepared = prepare_create_product(&server_grounded)
            .expect("server-owned grounding claims should survive preflight");
        assert_eq!(
            prepared.grounded_claim_source_urls,
            vec!["https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf".to_string()]
        );
    }

    fn batch_create_aspect(
        aspect_id: &str,
        manufacturer: &str,
        model: &str,
    ) -> PendingReviewAspect {
        PendingReviewAspect::avionics(
            aspect_id,
            "avionics_identity",
            format!("{manufacturer} {model}"),
            format!("{manufacturer} {model} shown in the listing"),
            "new_verified_product",
            1,
            "installed",
            Some(format!(
                "Listing identifies the {manufacturer} {model} unit."
            )),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            manufacturer,
            model,
            vec!["GPS".to_string()],
        ))
    }

    fn batch_create_decision(
        aspect_id: &str,
        manufacturer: &str,
        model: &str,
        identifier: &str,
    ) -> ReviewDecision {
        ReviewDecision::CreateVerifiedProduct {
            aspect_id: aspect_id.into(),
            unreviewed_avionics_model_id: None,
            manufacturer: manufacturer.to_string(),
            model: model.to_string(),
            capabilities: vec!["GPS".to_string()],
            manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
            manufacturer_identifier: identifier.to_string(),
            identity_source_url: "https://example.org/avionics/product-reference".to_string(),
            identity_source_title: format!("{manufacturer} {model} product reference"),
            identity_evidence_text: format!(
                "The authoritative product reference identifies {model} as manufacturer model number {identifier}."
            ),
            grounded_claim_source_urls: Vec::new(),
        }
    }

    async fn insert_evidence_backed_manufacturer(
        db: &AppDb,
        name: &str,
        normalized_name: &str,
    ) -> (i64, i64) {
        let pool = sqlite_pool(db);
        let manufacturer_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES (?, ?) RETURNING id",
        )
        .bind(name)
        .bind(normalized_name)
        .fetch_one(pool)
        .await
        .unwrap();
        ensure_manufacturer_identity(
            db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: format!("https://example.org/manufacturers/{normalized_name}"),
                source_title: format!("{name} manufacturer record"),
                evidence_text: format!(
                    "The authoritative manufacturer record identifies {name} as the manufacturer."
                ),
            },
        )
        .await
        .unwrap();
        let effective_identity_id: i64 = sqlx::query_scalar(
            r#"
            SELECT avionics_manufacturer_identity_id
            FROM avionics_manufacturer_effective_memberships
            WHERE avionics_manufacturer_id = ?
            "#,
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        (manufacturer_id, effective_identity_id)
    }

    #[test]
    fn serialization_is_deterministic_and_separates_observation_hash() {
        let aspect = pending_aspect("avionics:0:primary", 12);
        let first = serialize_review_payload(&[aspect.clone()]).unwrap();
        let second = serialize_review_payload(&[aspect]).unwrap();
        assert_eq!(first, second);
        assert_ne!(first.review_payload_sha256, first.extraction_sha256);
        assert_eq!(first.review_payload_sha256.len(), 64);
        assert_eq!(first.pending_aspect_count, 1);
    }

    #[test]
    fn approved_existing_assignment_projection_is_exact_and_fail_closed() {
        let exact = PendingReviewAspect::avionics(
            "installed",
            "avionics",
            "Garmin Product 11",
            "Garmin Product 11 shown in the listing",
            "listing_link_confidence_not_high",
            2,
            "installed",
            Some("retained listing evidence".to_string()),
            Some("medium".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GIA63W",
            vec!["GPS".to_string()],
        ))
        .with_covered_association(7, ListingAssociationRole::Installed, 11);
        let aspects = validated_aspects(&[exact]).unwrap();
        let assignment = existing_assignment(7, 11, 2, "installed", None);
        let mut approved = approved_review_products(&[11, 12]);
        // Compact model-code comparison accepts the staged `GIA63W` spelling,
        // and approved-only NAV remains valid because observed GPS is a subset.
        approved.get_mut(&11).unwrap().model = "GIA 63W".to_string();
        approved
            .get_mut(&11)
            .unwrap()
            .capabilities
            .push("NAV".to_string());
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &aspects,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            Some(11)
        );

        let mut missing_observation_identity = aspects.clone();
        missing_observation_identity[0].proposed_product = None;
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &missing_observation_identity,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            None
        );

        let mut wrong_quantity = aspects.clone();
        wrong_quantity[0].quantity = 1;
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &wrong_quantity,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            None
        );

        let mut wrong_action = aspects.clone();
        wrong_action[0].configuration_action = "removes".to_string();
        wrong_action[0].replaces_product_id = Some(11);
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &wrong_action,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            None
        );

        let mut wrong_product = aspects.clone();
        wrong_product[0].covered_associations[0].avionics_model_id = 12;
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &wrong_product,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            None
        );

        let mut ambiguous = aspects.clone();
        ambiguous[0]
            .covered_associations
            .push(CoveredListingAssociation {
                listing_link_id: 8,
                role: ListingAssociationRole::Installed,
                avionics_model_id: 11,
            });
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &ambiguous,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            None
        );

        let mut unapproved_assignment = assignment.clone();
        unapproved_assignment.installed_catalog_status = Some("unreviewed".to_string());
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &aspects,
                &[unapproved_assignment],
                &approved,
            ),
            None
        );

        let mut conflicting_staged_suggestion = aspects.clone();
        conflicting_staged_suggestion[0].suggested_product = approved.get(&12).cloned();
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &conflicting_staged_suggestion,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            None
        );

        let mut wrong_model = aspects.clone();
        wrong_model[0].proposed_product.as_mut().unwrap().model = "WX 500".to_string();
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &wrong_model,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            None
        );

        let mut unsupported_capability = aspects.clone();
        unsupported_capability[0]
            .proposed_product
            .as_mut()
            .unwrap()
            .capabilities = vec!["Traffic".to_string()];
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &unsupported_capability,
                &[assignment],
                &approved,
            ),
            None
        );
    }

    #[test]
    fn replacement_projection_requires_the_exact_parent_child_action_graph() {
        let parent = PendingReviewAspect::avionics(
            "installed",
            "avionics",
            "Garmin Product 11",
            "Garmin Product 11 replaces Product 12",
            "replacement_association_requires_review",
            2,
            "replaces",
            Some("retained replacement evidence".to_string()),
            Some("medium".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "Product 11",
            vec!["GPS".to_string()],
        ))
        .with_replacement_aspect("replacement")
        .with_covered_association(9, ListingAssociationRole::Installed, 11);
        let child = PendingReviewAspect::avionics(
            "replacement",
            "avionics",
            "Garmin Product 12",
            "Garmin Product 12 is the displaced unit",
            "replacement_association_confidence_not_high",
            1,
            "installed",
            Some("retained replacement evidence".to_string()),
            Some("medium".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "Product 12",
            vec!["GPS".to_string()],
        ))
        .with_covered_association(9, ListingAssociationRole::Replacement, 12);
        let aspects = validated_aspects(&[parent, child]).unwrap();
        let assignment = existing_assignment(9, 11, 2, "replaces", Some(12));
        let approved = approved_review_products(&[11, 12]);
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &aspects,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            Some(11)
        );
        assert_eq!(
            projected_suggestion_for_test(
                "replacement",
                &aspects,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            Some(12)
        );

        let mut wrong_child_quantity = aspects.clone();
        wrong_child_quantity[1].quantity = 2;
        assert_eq!(
            projected_suggestion_for_test(
                "installed",
                &wrong_child_quantity,
                std::slice::from_ref(&assignment),
                &approved,
            ),
            None
        );
        assert_eq!(
            projected_suggestion_for_test(
                "replacement",
                &wrong_child_quantity,
                &[assignment],
                &approved,
            ),
            None
        );
    }

    #[test]
    fn avionics_actions_are_rederived_as_the_complete_review_capability_set() {
        let mut aspect = pending_aspect("avionics:0:primary", 12);
        aspect.suggested_product = None;
        aspect.proposed_product = None;
        aspect.allowed_actions = vec![ReviewAction::Discard];
        let validated = validated_aspects(&[aspect]).unwrap();
        assert_eq!(
            validated[0].allowed_actions,
            vec![
                ReviewAction::UseVerifiedProduct,
                ReviewAction::CreateVerifiedProduct,
                ReviewAction::Discard,
            ]
        );
    }

    #[test]
    fn queue_splits_backfill_reason_codes_but_preserves_explanatory_text() {
        assert_eq!(
            queue_reason_codes("catalog_product_unverified,listing_link_confidence_not_high"),
            vec![
                "catalog_product_unverified".to_string(),
                "listing_link_confidence_not_high".to_string(),
            ]
        );
        assert_eq!(
            queue_reason_codes(
                "the product identity is verified, but its replacement target is unresolved"
            ),
            vec![
                "the product identity is verified, but its replacement target is unresolved"
                    .to_string()
            ]
        );
    }

    #[test]
    fn canonical_assignment_rejection_is_a_structured_curation_blocker() {
        let status = aircraft_identity_status_from_error(
            23,
            AircraftAdmissionError::Rejected {
                listing_id: Some(23),
                reason: crate::aircraft::faa::BlockReason::CanonicalIdentityAssignmentMissing,
                n_number: Some("N89225".to_string()),
                snapshot_id: Some(2),
            },
        )
        .unwrap();

        assert_eq!(
            status,
            ReviewAircraftIdentityStatus {
                status: ReviewAircraftIdentityState::CurationRequired,
                reason_code: Some("canonical_identity_assignment_missing".to_string()),
                faa_n_number: Some("N89225".to_string()),
                faa_snapshot_id: Some(2),
                repair: None,
            }
        );
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "status": "curation_required",
                "reason_code": "canonical_identity_assignment_missing",
                "faa_n_number": "N89225",
                "faa_snapshot_id": 2
            })
        );
    }

    #[test]
    fn optimistic_revisions_use_the_database_lowercase_hash_contract() {
        assert!(valid_sha256(&"0123456789abcdef".repeat(4)));
        assert!(!valid_sha256(&"0123456789ABCDEF".repeat(4)));
        assert!(!valid_sha256("short"));
    }

    #[test]
    fn decision_coverage_rejects_missing_duplicate_and_unknown_aspects() {
        let aspects = validated_aspects(&[pending_aspect("a", 1), pending_aspect("b", 2)]).unwrap();
        let payload = PendingReviewPayload {
            version: REVIEW_PAYLOAD_VERSION,
            aspects,
        };
        let missing = vec![ReviewDecision::Discard {
            aspect_id: "a".into(),
            reason: "not installed".to_string(),
        }];
        assert!(matches!(
            index_decisions(&payload, &missing),
            Err(ReviewError::Validation(message)) if message.contains("missing: b")
        ));

        let duplicate = vec![
            ReviewDecision::Discard {
                aspect_id: "a".into(),
                reason: "not installed".to_string(),
            },
            ReviewDecision::Discard {
                aspect_id: "a".into(),
                reason: "duplicate".to_string(),
            },
            ReviewDecision::Discard {
                aspect_id: "b".into(),
                reason: "not installed".to_string(),
            },
        ];
        assert!(matches!(
            index_decisions(&payload, &duplicate),
            Err(ReviewError::Validation(message)) if message.contains("more than one")
        ));

        let unknown = vec![
            ReviewDecision::Discard {
                aspect_id: "a".into(),
                reason: "not installed".to_string(),
            },
            ReviewDecision::Discard {
                aspect_id: "b".into(),
                reason: "not installed".to_string(),
            },
            ReviewDecision::Discard {
                aspect_id: "c".into(),
                reason: "unknown".to_string(),
            },
        ];
        assert!(matches!(
            index_decisions(&payload, &unknown),
            Err(ReviewError::Validation(message)) if message.contains("unknown review aspect c")
        ));
    }

    #[test]
    fn preserved_projection_exposes_every_unattested_product_and_no_hidden_blocker() {
        let mut aspects = vec![PendingReviewAspect::avionics(
            "new-observation",
            "avionics",
            "Garmin Product 99",
            "Garmin Product 99",
            "catalog_match_requires_review",
            1,
            "installed",
            None,
            Some("high".to_string()),
        )];
        let assignments = vec![
            existing_assignment(7, 11, 2, "installed", None),
            existing_assignment(8, 12, 1, "replaces", Some(13)),
        ];
        let approved = approved_review_products(&[11, 12, 13]);
        assert!(add_unauthorized_preserved_aspects(
            &mut aspects,
            &assignments,
            &approved,
            &HashSet::new(),
        )
        .unwrap());
        let targets = aspects
            .iter()
            .filter_map(|aspect| aspect.reuse_attestation_target_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(targets, BTreeSet::from([11, 12, 13]));
        assert_eq!(
            aspects
                .iter()
                .find(|aspect| aspect.reuse_attestation_target_id == Some(11))
                .map(|aspect| aspect.quantity),
            Some(2),
            "a preserved aspect must carry the exact installed association quantity"
        );
        assert!(
            hidden_preserved_blockers(&aspects, &assignments, &HashSet::new()).is_empty(),
            "every association that would fail the transaction is now hash-bound to an aspect"
        );

        let parent = aspects
            .iter()
            .find(|aspect| aspect.reuse_attestation_target_id == Some(12))
            .unwrap();
        let child = aspects
            .iter()
            .find(|aspect| aspect.reuse_attestation_target_id == Some(13))
            .unwrap();
        assert_eq!(parent.replacement_aspect_id.as_ref(), Some(&child.id));
        let payload = PendingReviewPayload {
            version: REVIEW_PAYLOAD_VERSION,
            aspects: aspects.clone(),
        };
        let inconsistent = aspects
            .iter()
            .map(|aspect| {
                if aspect.id == parent.id {
                    ReviewDecision::Discard {
                        aspect_id: aspect.id.clone(),
                        reason: "not installed".to_string(),
                    }
                } else {
                    ReviewDecision::UseVerifiedProduct {
                        aspect_id: aspect.id.clone(),
                        avionics_model_id: aspect.reuse_attestation_target_id.unwrap_or(99),
                    }
                }
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            index_decisions(&payload, &inconsistent),
            Err(ReviewError::Validation(message))
                if message.contains("action UseVerifiedProduct is not allowed")
        ));
    }

    #[test]
    fn same_case_authorization_is_the_complete_preserved_association_gate_without_global_reuse() {
        let listing_id = 1;
        let assignment = existing_assignment(7, 11, 1, "installed", None);
        let collision_rows = vec![collision_catalog_row(11)];
        let product_fingerprint = "b".repeat(64);
        let authorization = same_case_authorization_row(
            listing_id,
            &assignment,
            ListingAssociationRole::Installed,
            &product_fingerprint,
            &collision_rows,
        );
        let authorized = current_authorized_associations(
            listing_id,
            std::slice::from_ref(&assignment),
            &[authorization],
            &HashSet::new(),
            &collision_rows,
            &HashMap::from([(11, product_fingerprint)]),
        );
        let association = CoveredListingAssociation {
            listing_link_id: 7,
            role: ListingAssociationRole::Installed,
            avionics_model_id: 11,
        };
        assert_eq!(authorized, HashSet::from([association]));

        let approved = approved_review_products(&[11]);
        let mut aspects = vec![preserved_product_aspect(
            &assignment,
            ListingAssociationRole::Installed,
            approved.get(&11).unwrap(),
            None,
        )];
        assert!(remove_authorized_preserved_aspects(&mut aspects, &authorized).unwrap());
        assert!(aspects.is_empty());
        assert!(!add_unauthorized_preserved_aspects(
            &mut aspects,
            std::slice::from_ref(&assignment),
            &approved,
            &authorized,
        )
        .unwrap());
        assert!(hidden_preserved_blockers(
            &aspects,
            std::slice::from_ref(&assignment),
            &authorized,
        )
        .is_empty());
    }

    #[test]
    fn stale_same_case_capture_product_or_collision_proof_is_not_authorized() {
        let listing_id = 1;
        let assignment = existing_assignment(7, 11, 1, "installed", None);
        let collision_rows = vec![collision_catalog_row(11)];
        let product_fingerprint = "b".repeat(64);
        let current = same_case_authorization_row(
            listing_id,
            &assignment,
            ListingAssociationRole::Installed,
            &product_fingerprint,
            &collision_rows,
        );
        let product_fingerprints = HashMap::from([(11, product_fingerprint)]);

        let mut stale_capture = current.clone();
        stale_capture.evidence_capture_is_current = false;
        let mut stale_product = current.clone();
        stale_product.product_fingerprint = "c".repeat(64);
        let mut stale_collision = current;
        stale_collision.collision_closure_sha256 = "d".repeat(64);
        for stale in [stale_capture, stale_product, stale_collision] {
            assert!(current_authorized_associations(
                listing_id,
                std::slice::from_ref(&assignment),
                &[stale],
                &HashSet::new(),
                &collision_rows,
                &product_fingerprints,
            )
            .is_empty());
        }
    }

    #[test]
    fn legacy_high_confidence_review_requires_current_reuse_for_each_role() {
        let listing_id = 1;
        let mut assignment = existing_assignment(8, 12, 1, "replaces", Some(13));
        assignment.source = "listing_review".to_string();
        assignment.source_confidence = Some("high".to_string());
        assert!(current_authorized_associations(
            listing_id,
            std::slice::from_ref(&assignment),
            &[],
            &HashSet::new(),
            &[],
            &HashMap::new(),
        )
        .is_empty());

        let installed_only = current_authorized_associations(
            listing_id,
            std::slice::from_ref(&assignment),
            &[],
            &HashSet::from([12]),
            &[],
            &HashMap::new(),
        );
        assert_eq!(
            installed_only,
            HashSet::from([CoveredListingAssociation {
                listing_link_id: 8,
                role: ListingAssociationRole::Installed,
                avionics_model_id: 12,
            }])
        );

        let both_roles = current_authorized_associations(
            listing_id,
            &[assignment],
            &[],
            &HashSet::from([12, 13]),
            &[],
            &HashMap::new(),
        );
        assert_eq!(both_roles.len(), 2);
        assert!(both_roles.contains(&CoveredListingAssociation {
            listing_link_id: 8,
            role: ListingAssociationRole::Replacement,
            avionics_model_id: 13,
        }));
    }

    #[test]
    fn same_case_authorization_covers_the_replacement_role_without_global_reuse() {
        let listing_id = 1;
        let assignment = existing_assignment(8, 12, 1, "replaces", Some(13));
        let collision_rows = vec![collision_catalog_row(12), collision_catalog_row(13)];
        let installed_fingerprint = "b".repeat(64);
        let replacement_fingerprint = "c".repeat(64);
        let rows = [
            same_case_authorization_row(
                listing_id,
                &assignment,
                ListingAssociationRole::Installed,
                &installed_fingerprint,
                &collision_rows,
            ),
            same_case_authorization_row(
                listing_id,
                &assignment,
                ListingAssociationRole::Replacement,
                &replacement_fingerprint,
                &collision_rows,
            ),
        ];
        let authorized = current_authorized_associations(
            listing_id,
            &[assignment],
            &rows,
            &HashSet::new(),
            &collision_rows,
            &HashMap::from([(12, installed_fingerprint), (13, replacement_fingerprint)]),
        );
        assert_eq!(authorized.len(), 2);
        assert!(authorized.contains(&CoveredListingAssociation {
            listing_link_id: 8,
            role: ListingAssociationRole::Replacement,
            avionics_model_id: 13,
        }));
    }

    #[test]
    fn covered_unattested_approved_target_is_annotated_without_unreviewed_contract() {
        let mut aspects = vec![candidate_aspect("covered", 11, "Product 11")
            .with_covered_association(7, ListingAssociationRole::Installed, 11)];
        let assignments = vec![existing_assignment(7, 11, 1, "installed", None)];
        let approved = approved_review_products(&[11]);
        assert!(add_unauthorized_preserved_aspects(
            &mut aspects,
            &assignments,
            &approved,
            &HashSet::new(),
        )
        .unwrap());
        assert_eq!(aspects.len(), 1);
        assert_eq!(aspects[0].reuse_attestation_target_id, Some(11));
        assert_eq!(
            aspects[0]
                .proposed_product
                .as_ref()
                .and_then(|product| product.id),
            None,
            "approved targets must never use the unreviewed promotion ID"
        );
        assert_eq!(aspects[0].allowed_actions, vec![ReviewAction::Discard]);
    }

    #[test]
    fn restaging_existing_synthetic_aspect_normalizes_legacy_reason() {
        let assignment = existing_assignment(7, 11, 1, "installed", None);
        let approved = approved_review_products(&[11]);
        let mut aspect = preserved_product_aspect(
            &assignment,
            ListingAssociationRole::Installed,
            approved.get(&11).unwrap(),
            None,
        );
        aspect.reason = "catalog_product_reuse_attestation_missing".to_string();
        let mut aspects = vec![aspect];

        assert!(add_unauthorized_preserved_aspects(
            &mut aspects,
            &[assignment],
            &approved,
            &HashSet::new(),
        )
        .unwrap());
        assert_eq!(aspects[0].reason, PRESERVED_ASSOCIATION_REVIEW_REASON);
    }

    #[test]
    fn retiring_attested_replacement_child_preserves_parent_replacement_identity() {
        let assignments = vec![existing_assignment(8, 12, 1, "replaces", Some(13))];
        let mut aspects = Vec::new();
        assert!(add_unauthorized_preserved_aspects(
            &mut aspects,
            &assignments,
            &approved_review_products(&[12, 13]),
            &HashSet::new(),
        )
        .unwrap());
        assert_eq!(aspects.len(), 2);

        let replacement = CoveredListingAssociation {
            listing_link_id: 8,
            role: ListingAssociationRole::Replacement,
            avionics_model_id: 13,
        };
        assert!(remove_authorized_preserved_aspects(
            &mut aspects,
            &HashSet::from([replacement.clone()]),
        )
        .unwrap());
        assert_eq!(aspects.len(), 1);
        let parent = &aspects[0];
        assert_eq!(parent.reuse_attestation_target_id, Some(12));
        assert_eq!(parent.replacement_aspect_id, None);
        assert_eq!(parent.replaces_product_id, Some(13));
        assert!(
            hidden_preserved_blockers(&aspects, &assignments, &HashSet::from([replacement]),)
                .is_empty()
        );
    }

    #[test]
    fn attested_preserved_product_stays_implicit() {
        let original = PendingReviewAspect::avionics(
            "new-observation",
            "avionics",
            "Garmin Product 99",
            "Garmin Product 99",
            "catalog_match_requires_review",
            1,
            "installed",
            None,
            Some("high".to_string()),
        );
        let mut aspects = vec![original.clone()];
        let assignments = vec![existing_assignment(7, 11, 1, "installed", None)];
        let corroborated = CoveredListingAssociation {
            listing_link_id: 7,
            role: ListingAssociationRole::Installed,
            avionics_model_id: 11,
        };
        assert!(!add_unauthorized_preserved_aspects(
            &mut aspects,
            &assignments,
            &approved_review_products(&[11]),
            &HashSet::from([corroborated]),
        )
        .unwrap());
        assert_eq!(aspects, vec![original]);
    }

    #[test]
    fn existing_product_verification_rejects_model_only_proof_for_current_part_number() {
        let target = ReviewProduct::verified(
            43,
            "Garmin",
            "Flight Stream 510",
            vec!["Connectivity".to_string()],
        )
        .with_stable_identifier("manufacturer_part_number", "010-01322-11");
        let error = validate_existing_product_verification_evidence(
            &ReviewAspectId::from("flight-stream"),
            &target,
            "https://static.garmin.com/pumac/flight_stream_510.pdf",
            "Garmin Flight Stream 510 installation manual",
            "The Garmin Flight Stream 510 is the supported wireless gateway.",
        )
        .expect_err("model-only proof cannot attest a catalog row keyed by a part number");
        assert!(matches!(
            error,
            ReviewError::Validation(message)
                if message.contains("complete model and manufacturer identifier")
        ));
    }

    #[test]
    fn listing_association_fast_path_rejects_capability_and_variant_qualified_base_models() {
        let ads_b = "Garmin GTX 33 Mode S Transponder - ADS-B Compliant";
        assert!(exact_compact_listing_identity_is_present(ads_b, "GTX 33"));
        assert!(listing_evidence_has_ambiguous_semantic_qualifier(
            ads_b, "GTX 33"
        ));

        for (evidence, base_model) in [
            ("Garmin GTX 33 ES", "GTX 33"),
            ("Garmin G1000 NXi integrated flight deck", "G1000"),
            ("Garmin GTX 345 R transponder", "GTX 345"),
            ("Garmin GDL 69A SXM datalink", "GDL 69A"),
        ] {
            assert!(
                exact_compact_listing_identity_is_present(evidence, base_model),
                "the compact label alone demonstrates why a second gate is required"
            );
            assert!(
                listing_evidence_has_distinct_variant_suffix(evidence, base_model),
                "{evidence:?} must not corroborate base product {base_model:?}"
            );
        }

        assert!(!listing_evidence_has_distinct_variant_suffix(
            "Garmin GTX 345 transponder",
            "GTX 345"
        ));
        assert!(!listing_evidence_has_distinct_variant_suffix(
            "Garmin GNS 430W P/N 011-01064-40",
            "GNS 430W"
        ));
        assert!(!listing_evidence_has_ambiguous_semantic_qualifier(
            "Garmin GTX 345 transponder",
            "GTX 345"
        ));
    }

    #[test]
    fn recovered_association_rejects_longer_catalog_neighbor_but_allows_shorter_one() {
        let product =
            ReviewProduct::verified(10, "Garmin", "GDL 69A", vec!["Datalink".to_string()])
                .with_stable_identifier("manufacturer_part_number", "011-00987-00");
        let own = RecoverableCatalogIdentityRow {
            id: 10,
            model: "GDL 69A".to_string(),
            manufacturer_identifier_kind: Some("manufacturer_part_number".to_string()),
            manufacturer_identifier: Some("011-00987-00".to_string()),
        };
        let shorter = RecoverableCatalogIdentityRow {
            id: 11,
            model: "GDL 69".to_string(),
            manufacturer_identifier_kind: Some("manufacturer_part_number".to_string()),
            manufacturer_identifier: Some("011-00987-01".to_string()),
        };
        assert!(catalog_identity_is_unique(
            &product,
            &[own.clone(), shorter]
        ));

        let longer = RecoverableCatalogIdentityRow {
            id: 12,
            model: "GDL 69A D".to_string(),
            manufacturer_identifier_kind: Some("manufacturer_part_number".to_string()),
            manufacturer_identifier: Some("011-00987-02".to_string()),
        };
        assert!(!catalog_identity_is_unique(&product, &[own, longer]));
    }

    #[test]
    fn pending_association_repair_derives_exact_evidence_or_clears_it() {
        let product =
            ReviewProduct::verified(10, "Garmin", "GMA 1347", vec!["Audio Panel".to_string()])
                .with_stable_identifier("manufacturer_part_number", "011-00809-00");
        let approved = HashMap::from([(10, product)]);
        let catalog = vec![RecoverableCatalogIdentityRow {
            id: 10,
            model: "GMA 1347".to_string(),
            manufacturer_identifier_kind: Some("manufacturer_part_number".to_string()),
            manufacturer_identifier: Some("011-00809-00".to_string()),
        }];
        let aspect = PendingReviewAspect::avionics(
            "legacy",
            "avionics",
            "Garmin GMA 1347",
            "Garmin GMA 1347 audio panel",
            "listing_action_graph_invalid,catalog_product_unverified,listing_link_confidence_not_high",
            1,
            "installed",
            None,
            None,
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GMA 1347",
            vec!["Audio Panel".to_string()],
        ))
        .with_covered_association(7, ListingAssociationRole::Installed, 10);
        let mut payload = PendingReviewPayload {
            version: REVIEW_PAYLOAD_VERSION,
            aspects: vec![aspect],
        };
        let source_html =
            "<body>Garmin integrated avionics. The listing identifies a GMA-1347 audio panel.</body>";
        let source = ListingEvidenceContext::from_listing_capture(None, Some(source_html));
        let mut assignment = existing_assignment(7, 10, 1, "installed", None);
        assignment.installed_manufacturer = Some("Garmin".to_string());
        assignment.installed_model = Some("GMA 1347".to_string());
        assignment.source_notes = Some("generated resolver explanation".to_string());
        let repairs = plan_pending_association_evidence_repair(
            &payload,
            std::slice::from_ref(&assignment),
            &approved,
            &catalog,
            Some(&source),
            Some(source_html),
        );
        assert_eq!(repairs[0].evidence_text.as_deref(), Some("GMA-1347"));

        assignment.quantity = 2;
        payload.aspects[0].quantity = 2;
        assert_eq!(
            plan_pending_association_evidence_repair(
                &payload,
                std::slice::from_ref(&assignment),
                &approved,
                &catalog,
                Some(&source),
                Some(source_html),
            )[0]
            .evidence_text
            .as_deref(),
            Some("GMA-1347")
        );
        assignment.quantity = 1;
        payload.aspects[0].quantity = 1;
        assignment.configuration_action = "replaces".to_string();
        assignment.replaces_avionics_model_id = Some(11);
        let repair = plan_pending_association_evidence_repair(
            &payload,
            std::slice::from_ref(&assignment),
            &approved,
            &catalog,
            Some(&source),
            Some(source_html),
        );
        assert_eq!(repair[0].evidence_text, None);
        assert!(!repair[0].update_link);
        assignment.configuration_action = "installed".to_string();
        assignment.replaces_avionics_model_id = None;
        assert_eq!(
            plan_pending_association_evidence_repair(
                &payload,
                std::slice::from_ref(&assignment),
                &approved,
                &catalog,
                None,
                None,
            )[0]
            .evidence_text,
            None
        );

        let ambiguous_html = "<body>Garmin GMA-1347 and a spare GMA-1347 NXi</body>";
        let ambiguous = ListingEvidenceContext::from_listing_capture(None, Some(ambiguous_html));
        assert_eq!(
            plan_pending_association_evidence_repair(
                &payload,
                std::slice::from_ref(&assignment),
                &approved,
                &catalog,
                Some(&ambiguous),
                Some(ambiguous_html),
            )[0]
            .evidence_text,
            None
        );

        let hidden_html =
            "<style>.hidden { display: none }</style><body><div class=hidden>GMA-1347</div></body>";
        let hidden = ListingEvidenceContext::from_listing_capture(None, Some(hidden_html));
        assert_eq!(
            plan_pending_association_evidence_repair(
                &payload,
                std::slice::from_ref(&assignment),
                &approved,
                &catalog,
                Some(&hidden),
                Some(hidden_html),
            )[0]
            .evidence_text,
            None
        );

        let script_html = "<body><script>const equipment = 'GMA-1347';</script></body>";
        let script = ListingEvidenceContext::from_listing_capture(None, Some(script_html));
        assert_eq!(
            plan_pending_association_evidence_repair(
                &payload,
                std::slice::from_ref(&assignment),
                &approved,
                &catalog,
                Some(&script),
                Some(script_html),
            )[0]
            .evidence_text,
            None
        );

        let competing_catalog = [
            catalog[0].clone(),
            RecoverableCatalogIdentityRow {
                id: 11,
                model: "GMA 1347B".to_string(),
                manufacturer_identifier_kind: Some("manufacturer_part_number".to_string()),
                manufacturer_identifier: Some("011-00809-01".to_string()),
            },
        ];
        assert_eq!(
            plan_pending_association_evidence_repair(
                &payload,
                std::slice::from_ref(&assignment),
                &approved,
                &competing_catalog,
                Some(&source),
                Some(source_html),
            )[0]
            .evidence_text,
            None
        );
    }

    #[test]
    fn pending_association_repair_preserves_only_exact_visible_correction_evidence() {
        let linked_product =
            ReviewProduct::verified(10, "Garmin", "G500", vec!["Flight Display".to_string()])
                .with_stable_identifier("manufacturer_model_number", "G500");
        let approved = HashMap::from([(10, linked_product)]);
        let catalog = vec![RecoverableCatalogIdentityRow {
            id: 10,
            model: "G500".to_string(),
            manufacturer_identifier_kind: Some("manufacturer_model_number".to_string()),
            manufacturer_identifier: Some("G500".to_string()),
        }];
        let mut assignment = existing_assignment(7, 10, 1, "installed", None);
        assignment.installed_manufacturer = Some("Garmin".to_string());
        assignment.installed_model = Some("G500".to_string());
        assignment.source_notes = Some("generated resolver prose".to_string());
        assignment.source_confidence = Some("high".to_string());

        let mut aspect = PendingReviewAspect::avionics(
            "changed-identity",
            "avionics",
            "Garmin G5",
            "Garmin G5",
            "catalog_product_unverified",
            1,
            "installed",
            Some("generated resolver prose".to_string()),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "G5",
            vec!["Flight Display".to_string()],
        ))
        .with_covered_association(7, ListingAssociationRole::Installed, 10);
        let source_html = "<body><p>G5 installed</p><p>Garmin G5X spare</p></body>";
        let source = ListingEvidenceContext::from_listing_capture(None, Some(source_html));

        let payload = PendingReviewPayload {
            version: REVIEW_PAYLOAD_VERSION,
            aspects: vec![aspect.clone()],
        };
        let generated = plan_pending_association_evidence_repair(
            &payload,
            std::slice::from_ref(&assignment),
            &approved,
            &catalog,
            Some(&source),
            Some(source_html),
        );
        assert_eq!(generated[0].link_evidence_text, None);
        assert_eq!(generated[0].aspect_evidence_text, None);

        aspect.source_evidence_text = Some("G5 installed".to_string());
        let payload = PendingReviewPayload {
            version: REVIEW_PAYLOAD_VERSION,
            aspects: vec![aspect],
        };
        let exact = plan_pending_association_evidence_repair(
            &payload,
            std::slice::from_ref(&assignment),
            &approved,
            &catalog,
            Some(&source),
            Some(source_html),
        );
        assert_eq!(exact[0].link_evidence_text, None);
        assert_eq!(
            exact[0].aspect_evidence_text.as_deref(),
            Some("G5 installed")
        );
    }

    #[test]
    fn pending_association_repair_scopes_qualifiers_to_publisher_equipment_lines() {
        let identities = [
            (10, "GDU 1044B", "011-01000-10"),
            (11, "GIA 63W", "011-01000-11"),
            (12, "GTX 33", "011-01000-12"),
        ];
        let approved = identities
            .iter()
            .map(|(id, model, part_number)| {
                (
                    *id,
                    ReviewProduct::verified(*id, "Garmin", *model, vec!["Avionics".to_string()])
                        .with_stable_identifier("manufacturer_part_number", *part_number),
                )
            })
            .collect::<HashMap<_, _>>();
        let catalog = identities
            .iter()
            .map(|(id, model, part_number)| RecoverableCatalogIdentityRow {
                id: *id,
                model: (*model).to_string(),
                manufacturer_identifier_kind: Some("manufacturer_part_number".to_string()),
                manufacturer_identifier: Some((*part_number).to_string()),
            })
            .collect::<Vec<_>>();
        let assignments = identities
            .iter()
            .map(|(id, model, _)| {
                let mut assignment = existing_assignment(*id, *id, 1, "installed", None);
                assignment.installed_model = Some((*model).to_string());
                assignment
            })
            .collect::<Vec<_>>();
        let payload = PendingReviewPayload {
            version: REVIEW_PAYLOAD_VERSION,
            aspects: Vec::new(),
        };
        let source_html = r#"<html><body>
          <div class="detail__specs-wrapper">
            <div class="detail__specs-label">Avionics/Radios</div>
            <div class="detail__specs-value">Dual GDU-1044B PFD/MFD
Dual GIA-63W NAV/COM/GPS/WAAS
Garmin GTX 33 Transponder ADS-B Compliant</div>
          </div>
        </body></html>"#;
        let source = ListingEvidenceContext::from_listing_capture(
            Some("https://www.controller.com/listing/for-sale/257737897/example"),
            Some(source_html),
        );

        let repairs = plan_pending_association_evidence_repair(
            &payload,
            &assignments,
            &approved,
            &catalog,
            Some(&source),
            Some(source_html),
        );

        assert_eq!(repairs[0].evidence_text.as_deref(), Some("GDU-1044B"));
        assert_eq!(repairs[1].evidence_text.as_deref(), Some("GIA-63W"));
        assert_eq!(repairs[2].evidence_text, None);
    }

    #[test]
    fn review_evidence_and_confidence_are_a_paired_invariant() {
        for (evidence, confidence) in [
            (Some("Garmin GTX 345".to_string()), None),
            (None, Some("high".to_string())),
            (Some("   ".to_string()), Some("high".to_string())),
        ] {
            let aspect = PendingReviewAspect::avionics(
                "pair",
                "avionics",
                "Garmin GTX 345",
                "Garmin GTX 345",
                "pending",
                1,
                "installed",
                evidence,
                confidence,
            );
            let validated = validated_aspects(&[aspect]).unwrap();
            assert_eq!(validated[0].source_evidence_text, None);
            assert_eq!(validated[0].source_confidence, None);
        }

        let paired = PendingReviewAspect::avionics(
            "pair",
            "avionics",
            "Garmin GTX 345",
            "structured observation",
            "pending",
            1,
            "installed",
            Some("  Garmin GTX 345  ".to_string()),
            Some("high".to_string()),
        );
        let validated = validated_aspects(&[paired]).unwrap();
        assert_eq!(
            validated[0].source_evidence_text.as_deref(),
            Some("Garmin GTX 345")
        );
        assert_eq!(validated[0].source_confidence.as_deref(), Some("high"));
        assert_ne!(
            validated[0].source_evidence_text.as_deref(),
            Some(validated[0].observed_text.as_str())
        );
    }

    async fn test_db() -> AppDb {
        AppDb::connect("sqlite::memory:").await.unwrap()
    }

    fn sqlite_pool(db: &AppDb) -> &SqlitePool {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test database is not SQLite");
        };
        pool
    }

    async fn insert_faa_aircraft(db: &AppDb, n_number: &str, serial_number: &str) {
        const AIRCRAFT_REFERENCE: &str = "CODE,MFR,MODEL,TYPE-ACFT,TYPE-ENG,AC-CAT,BUILD-CERT-IND,NO-ENG,NO-SEATS,AC-WEIGHT,SPEED,TC-DATA-SHEET,TC-DATA-HOLDER\n2072738,CESSNA AIRCRAFT CO,182T,4,1,1,0,01,004,CLASS 1,0145,3A13,TEXTRON AVIATION INC\n";
        const ENGINE_REFERENCE: &str =
            "CODE,MFR,MODEL,TYPE,HORSEPOWER,THRUST\n41528,LYCOMING,IO-540-AB1A5,1,00230,000000\n";
        let suffix = n_number
            .strip_prefix('N')
            .expect("test FAA N-number must include N prefix");
        let master = format!(
            "N-NUMBER,SERIAL NUMBER,MFR MDL CODE,ENG MFR MDL,YEAR MFR\n{suffix},{serial_number},2072738,41528,2020\n"
        );
        let release = ReleaseFixtureBuilder::from_csv(
            ReleaseMetadata::official("2026-07-20", "a".repeat(64)),
            Cursor::new(master),
            Cursor::new(AIRCRAFT_REFERENCE),
            Cursor::new(ENGINE_REFERENCE),
            [n_number],
        )
        .expect("test FAA release should parse");
        store_release(db, &release)
            .await
            .expect("test FAA release should store");
    }

    async fn insert_listing(db: &AppDb) -> (i64, i64) {
        let pool = sqlite_pool(db);
        let user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
            .bind(crate::db::DEVELOPER_EMAIL)
            .fetch_one(pool)
            .await
            .unwrap();
        let variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (?, ?, 'https://broker.example/aircraft/one/' ||
              (SELECT COUNT(*) + 1 FROM aircraft_sale_listings), 2020, 450000, 900)
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        (user_id, listing_id)
    }

    async fn insert_review_bound_submission(
        db: &AppDb,
        user_id: i64,
        listing_id: i64,
        rendered_html: &str,
    ) -> i64 {
        let pool = sqlite_pool(db);
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'test-key') RETURNING id",
        )
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let source_url: String =
            sqlx::query_scalar("SELECT source_url FROM aircraft_sale_listings WHERE id = ?")
                .bind(listing_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        sqlx::query_scalar(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, canonical_listing_id
            ) VALUES (?, ?, ?, ?, ?, 'test-signature', ?)
            RETURNING id
            "#,
        )
        .bind(user_id)
        .bind(install_id)
        .bind(source_url)
        .bind(rendered_html)
        .bind(rendered_html_sha256)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn insert_repairable_capture_review(
        db: &AppDb,
    ) -> (i64, i64, i64, i64, StagedPendingReview) {
        let (user_id, listing_id) = insert_listing(db).await;
        let product_id = insert_approved_product(db, "GMA 1347", "GMA1347", "Audio Panel").await;
        attest_approved_product_for_current_policy_reuse(db, product_id).await;
        let submission_id = insert_review_bound_submission(
            db,
            user_id,
            listing_id,
            "<main>Installed GMA-1347 Digital Audio Panel.</main>",
        )
        .await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Generated resolver prose', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(sqlite_pool(db))
        .await
        .unwrap();
        let aspect = PendingReviewAspect::avionics(
            "repairable-audio-panel",
            "avionics",
            "Garmin GMA-1347",
            "structured observation",
            "listing_link_confidence_not_high",
            1,
            "installed",
            None,
            None,
        )
        .with_covered_association(link_id, ListingAssociationRole::Installed, product_id);
        let staged = stage_pending_review(db, listing_id, Some(submission_id), &[aspect])
            .await
            .unwrap();
        (user_id, listing_id, submission_id, link_id, staged)
    }

    async fn insert_additional_listing(db: &AppDb, user_id: i64, source_url: &str) -> i64 {
        let pool = sqlite_pool(db);
        let variant_id: i64 = sqlx::query_scalar(
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (?, ?, ?, 2020, 455000, 950)
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(user_id)
        .bind(source_url)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn insert_catalog_product(
        db: &AppDb,
        model: &str,
        identifier: &str,
        capability: &str,
        approve: bool,
    ) -> i64 {
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
                evidence_text:
                    "Garmin's authoritative aviation site identifies Garmin as the manufacturer."
                        .to_string(),
            },
        )
        .await
        .unwrap();
        let normalized_model = normalize_avionics_model_name(model);
        let normalized_identifier = normalize_avionics_identifier(identifier);
        let model_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_evidence_kind, identity_confidence, catalog_reviewed_at
            ) VALUES (?, ?, ?, 'manufacturer_model_number', ?, ?,
                      'https://www.garmin.com/aviation/product', 'Garmin product manual',
                      'Manufacturer manual identifies this exact marketed unit.',
                      'authoritative_reference', 'very_high', CURRENT_TIMESTAMP)
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .bind(model)
        .bind(&normalized_model)
        .bind(identifier)
        .bind(&normalized_identifier)
        .fetch_one(pool)
        .await
        .unwrap();
        let normalized_capability = normalize_name(capability);
        sqlx::query(
            "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
        )
        .bind(capability)
        .bind(&normalized_capability)
        .execute(pool)
        .await
        .unwrap();
        let type_id: i64 =
            sqlx::query_scalar("SELECT id FROM avionics_types WHERE normalized_name = ?")
                .bind(&normalized_capability)
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(model_id)
        .bind(type_id)
        .execute(pool)
        .await
        .unwrap();
        if approve {
            sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
                .bind(model_id)
                .execute(pool)
                .await
                .unwrap();
        }
        model_id
    }

    async fn insert_approved_product(
        db: &AppDb,
        model: &str,
        identifier: &str,
        capability: &str,
    ) -> i64 {
        insert_catalog_product(db, model, identifier, capability, true).await
    }

    async fn attest_approved_product_for_current_policy_reuse(db: &AppDb, avionics_model_id: i64) {
        let pool = sqlite_pool(db);
        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origins (
              authority_kind, avionics_manufacturer_identity_id, https_origin,
              evidence_source_url, evidence_source_title, evidence_text,
              approval_basis, approval_reason
            )
            SELECT
              'manufacturer_primary',
              product_identity.avionics_manufacturer_identity_id,
              'https://www.garmin.com',
              'https://www.garmin.com/aviation/product',
              'Garmin aviation product catalog',
              'Garmin publishes the exact test product on its first-party aviation catalog.',
              'curated_bootstrap',
              'Test fixture for exact Garmin product source authority'
            FROM avionics_approved_product_identities product_identity
            WHERE product_identity.avionics_model_id = ?
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(avionics_model_id)
        .execute(pool)
        .await
        .expect("current-policy source origin should seed");

        let mut transaction = pool
            .begin()
            .await
            .expect("reuse-attestation transaction should start");
        assert!(
            refresh_reuse_attestation_sqlite(
                db,
                &mut transaction,
                avionics_model_id,
                "https://www.garmin.com/aviation/product",
            )
            .await
            .expect("current-policy reuse attestation should refresh"),
            "approved fixture must satisfy the complete current-policy reuse contract"
        );
        assert!(
            reuse_attestation_is_current_sqlite(db, &mut transaction, avionics_model_id)
                .await
                .expect("current-policy reuse attestation should validate")
        );
        transaction
            .commit()
            .await
            .expect("reuse attestation should commit");
    }

    async fn insert_current_same_case_authorization(
        db: &AppDb,
        listing_id: i64,
        listing_link_id: i64,
        avionics_model_id: i64,
        plugin_submission_id: i64,
        role: ListingAssociationRole,
    ) {
        let assignment = load_existing_assignments(db, listing_id)
            .await
            .unwrap()
            .into_iter()
            .find(|assignment| assignment.listing_link_id == listing_link_id)
            .expect("authorized test link must exist");
        let product_fingerprint = load_catalog_product_fingerprint_map(db)
            .await
            .unwrap()
            .remove(&avionics_model_id)
            .expect("authorized test product must be approved");
        let collision_closure_sha256 =
            grounded_collision_closure_revision_sha256(db, avionics_model_id)
                .await
                .unwrap();
        let evidence_capture_sha256: String =
            sqlx::query_scalar("SELECT rendered_html_sha256 FROM plugin_submissions WHERE id = ?")
                .bind(plugin_submission_id)
                .fetch_one(sqlite_pool(db))
                .await
                .unwrap();
        let observation_sha256 = association_observation_sha256(
            listing_id,
            &assignment,
            role,
            assignment.source_notes.as_deref().unwrap_or_default(),
        );
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics_authorizations (
              listing_link_id, association_role, avionics_model_id,
              authorization_kind, observation_sha256, product_fingerprint,
              grounded_resolution_sha256, evidence_capture_sha256,
              collision_closure_sha256, policy_version
            ) VALUES (?, ?, ?, 'same_case_grounded', ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(listing_link_id)
        .bind(association_role_label(role))
        .bind(avionics_model_id)
        .bind(observation_sha256)
        .bind(product_fingerprint)
        .bind("e".repeat(64))
        .bind(evidence_capture_sha256)
        .bind(collision_closure_sha256)
        .bind(ASSOCIATION_AUTHORIZATION_POLICY_VERSION)
        .execute(sqlite_pool(db))
        .await
        .unwrap();
    }

    async fn insert_unreviewed_product(
        db: &AppDb,
        model: &str,
        identifier: &str,
        capability: &str,
    ) -> i64 {
        insert_catalog_product(db, model, identifier, capability, false).await
    }

    async fn store_current_avionics_extraction(
        db: &AppDb,
        submission_id: i64,
        avionics: serde_json::Value,
    ) {
        sqlx::query(
            "UPDATE plugin_submissions SET extracted_listing_json = ?, extraction_error = NULL WHERE id = ?",
        )
        .bind(serde_json::json!({ "avionics": avionics }).to_string())
        .bind(submission_id)
        .execute(sqlite_pool(db))
        .await
        .unwrap();
    }

    async fn insert_listing_avionics_link(
        db: &AppDb,
        listing_id: i64,
        product_id: i64,
        quantity: i64,
        evidence: &str,
    ) -> i64 {
        sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, ?, 'listing', ?, 'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .bind(quantity)
        .bind(evidence)
        .fetch_one(sqlite_pool(db))
        .await
        .unwrap()
    }

    fn retained_occurrence_aspect(
        id: &str,
        manufacturer: &str,
        model: &str,
        quantity: i64,
        evidence: &str,
        link_id: i64,
        product_id: i64,
    ) -> PendingReviewAspect {
        PendingReviewAspect::avionics(
            id,
            "avionics",
            format!("{manufacturer} {model}"),
            format!("{manufacturer} {model}"),
            "legacy_machine_reason",
            quantity,
            "installed",
            Some(evidence.to_string()),
            Some("high".to_string()),
        )
        .with_covered_association(link_id, ListingAssociationRole::Installed, product_id)
    }

    async fn stored_review_state(db: &AppDb, listing_id: i64) -> (String, String, i64) {
        sqlx::query_as(
            r#"
            SELECT review_payload_json, review_payload_sha256, pending_aspect_count
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(db))
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn whole_listing_resolution_preserves_current_same_case_link_without_global_reuse() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "WX-500", "WX500", "Weather Radar").await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<main>Installed L3 WX-500 stormscope</main>",
        )
        .await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'L3 WX-500', 'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        insert_current_same_case_authorization(
            &db,
            listing_id,
            link_id,
            product_id,
            submission_id,
            ListingAssociationRole::Installed,
        )
        .await;
        let reuse_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?",
        )
        .bind(product_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(reuse_count, 0);

        let staged = stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[pending_aspect("discard-unrelated", product_id)],
        )
        .await
        .unwrap();
        let detail = get_listing_review(&db, user_id, listing_id).await.unwrap();
        assert_eq!(detail.review.aspects.len(), 1);
        assert_eq!(detail.review.aspects[0].id, "discard-unrelated".into());

        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::Discard {
                    aspect_id: "discard-unrelated".into(),
                    reason: "not installed".to_string(),
                }],
            },
        )
        .await
        .expect("a current same-case authorization must preserve the untouched link");

        let preserved: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT avionics_model_id, quantity FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_all(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(preserved, vec![(product_id, 1)]);
    }

    #[tokio::test]
    async fn whole_listing_resolution_preserves_current_same_case_replacement_role() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let installed_id = insert_approved_product(&db, "GTN 750Xi", "GTN750XI", "GPS").await;
        let replacement_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let evidence = "Garmin GTN 750Xi replaces Garmin GNS 430W";
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            &format!("<main>{evidence}</main>"),
        )
        .await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action,
              replaces_avionics_model_id
            ) VALUES (?, ?, 1, 'listing', ?, 'high', 'replaces', ?)
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(installed_id)
        .bind(evidence)
        .bind(replacement_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        for (product_id, role) in [
            (installed_id, ListingAssociationRole::Installed),
            (replacement_id, ListingAssociationRole::Replacement),
        ] {
            insert_current_same_case_authorization(
                &db,
                listing_id,
                link_id,
                product_id,
                submission_id,
                role,
            )
            .await;
        }
        let staged = stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[pending_aspect("discard-unrelated", installed_id)],
        )
        .await
        .unwrap();

        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::Discard {
                    aspect_id: "discard-unrelated".into(),
                    reason: "not installed".to_string(),
                }],
            },
        )
        .await
        .expect("both current same-case roles must preserve the replacement link");

        let preserved: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
            r#"
            SELECT avionics_model_id, configuration_action,
                   replaces_avionics_model_id
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_all(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(
            preserved,
            vec![(installed_id, "replaces".to_string(), Some(replacement_id))]
        );
    }

    #[tokio::test]
    async fn product_review_pages_collapse_associations_and_use_stable_keyset_cursors() {
        let db = test_db().await;
        let (user_id, first_listing_id) = insert_listing(&db).await;
        let (_, second_listing_id) = insert_listing(&db).await;
        let current_product_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let required_product_id =
            insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        let pool = sqlite_pool(&db);

        for (listing_id, product_id, evidence) in [
            (
                first_listing_id,
                current_product_id,
                "Garmin GNS 430W shown in the listing",
            ),
            (
                second_listing_id,
                current_product_id,
                "Garmin GNS 430W shown in the listing",
            ),
            (
                first_listing_id,
                required_product_id,
                "Garmin GTX 345 shown in the listing",
            ),
        ] {
            sqlx::query(
                r#"
                INSERT INTO aircraft_sale_listing_avionics (
                  aircraft_sale_listing_id, avionics_model_id, quantity, source,
                  source_notes, source_confidence, configuration_action
                ) VALUES (?, ?, 1, 'listing', ?, 'high', 'installed')
                "#,
            )
            .bind(listing_id)
            .bind(product_id)
            .bind(evidence)
            .execute(pool)
            .await
            .unwrap();
        }
        let products = load_all_approved_product_map(&db).await.unwrap();
        for listing_id in [first_listing_id, second_listing_id] {
            let assignments = load_existing_assignments(&db, listing_id).await.unwrap();
            let aspects = assignments
                .iter()
                .map(|assignment| {
                    preserved_product_aspect(
                        assignment,
                        ListingAssociationRole::Installed,
                        products.get(&assignment.avionics_model_id).unwrap(),
                        None,
                    )
                })
                .collect::<Vec<_>>();
            stage_pending_review(&db, listing_id, None, &aspects)
                .await
                .unwrap();
        }
        attest_approved_product_for_current_policy_reuse(&db, current_product_id).await;

        let first_page = list_pending_product_reviews(
            &db,
            user_id,
            ProductReviewPageQuery {
                limit: Some(1),
                cursor: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(first_page.items.len(), 1);
        assert_eq!(first_page.items[0].product.id, Some(current_product_id));
        assert_eq!(
            first_page.items[0].attestation_status,
            ProductAttestationStatus::Current
        );
        assert_eq!(first_page.items[0].pending_association_count, 2);
        assert_eq!(first_page.items[0].pending_listing_count, 2);
        // Attestation status is mutable display state, not cursor order. A
        // status change between pages must neither duplicate nor skip the
        // next immutable product ID.
        attest_approved_product_for_current_policy_reuse(&db, required_product_id).await;
        let second_page = list_pending_product_reviews(
            &db,
            user_id,
            ProductReviewPageQuery {
                limit: Some(1),
                cursor: first_page.next_cursor,
            },
        )
        .await
        .unwrap();
        assert_eq!(second_page.items.len(), 1);
        assert_eq!(second_page.items[0].product.id, Some(required_product_id));
        assert_eq!(
            second_page.items[0].attestation_status,
            ProductAttestationStatus::Current
        );
        assert!(second_page.next_cursor.is_none());

        let association_page = list_pending_product_associations(
            &db,
            user_id,
            current_product_id,
            ProductReviewPageQuery {
                limit: Some(1),
                cursor: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(association_page.associations.len(), 1);
        let association_cursor = association_page.next_cursor.clone();
        let remaining = list_pending_product_associations(
            &db,
            user_id,
            current_product_id,
            ProductReviewPageQuery {
                limit: Some(1),
                cursor: association_cursor.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(remaining.associations.len(), 1);
        assert_ne!(
            association_page.associations[0].listing_id,
            remaining.associations[0].listing_id
        );
        assert!(remaining.next_cursor.is_none());
        assert!(matches!(
            list_pending_product_associations(
                &db,
                user_id,
                required_product_id,
                ProductReviewPageQuery {
                    limit: Some(1),
                    cursor: association_cursor,
                },
            )
            .await,
            Err(ReviewError::Validation(message))
                if message.contains("different product")
        ));
        assert!(matches!(
            list_pending_product_reviews(
                &db,
                user_id,
                ProductReviewPageQuery {
                    limit: Some(1),
                    cursor: Some("not-a-valid-cursor%%%".to_string()),
                },
            )
            .await,
            Err(ReviewError::Validation(_))
        ));

        let foreign_page =
            list_pending_product_reviews(&db, user_id + 10_000, ProductReviewPageQuery::default())
                .await
                .unwrap();
        assert!(foreign_page.items.is_empty());
        let direct_authorization = &association_page.associations[0];
        assert!(matches!(
            preflight_pending_product_attestation(
                &db,
                user_id + 10_000,
                current_product_id,
                direct_authorization.listing_id,
                &direct_authorization.review_payload_sha256,
                &direct_authorization.aspect_id,
                &first_page.catalog_revision_sha256,
                "",
                "",
                "",
            )
            .await,
            Err(ReviewError::Permission(_))
        ));
        assert!(matches!(
            list_pending_product_associations(
                &db,
                user_id + 10_000,
                current_product_id,
                ProductReviewPageQuery::default(),
            )
            .await,
            Err(ReviewError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn product_association_pages_report_complete_read_only_eligibility() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let rendered_html =
            "<html><body><p>Garmin GNS 430W navigator</p><p>Garmin GTX 345 transponder</p></body></html>";
        let submission_id =
            insert_review_bound_submission(&db, user_id, listing_id, rendered_html).await;
        let attested_product_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let unattested_product_id =
            insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, attested_product_id).await;
        let aspects = vec![
            PendingReviewAspect::avionics(
                "auto",
                "avionics_identity",
                "Garmin GNS 430W",
                "Garmin GNS 430W navigator",
                "catalog_match_requires_review",
                1,
                "installed",
                Some("Garmin GNS 430W navigator".to_string()),
                Some("high".to_string()),
            )
            .with_reuse_attestation_target(attested_product_id),
            PendingReviewAspect::avionics(
                "manual",
                "avionics_identity",
                "Garmin GNS 430W",
                "Garmin GNS 430W",
                "catalog_match_requires_review",
                1,
                "installed",
                None,
                None,
            )
            .with_reuse_attestation_target(attested_product_id),
            PendingReviewAspect::avionics(
                "attestation",
                "avionics_identity",
                "Garmin GTX 345",
                "Garmin GTX 345 transponder",
                "catalog_match_requires_review",
                1,
                "installed",
                Some("Garmin GTX 345 transponder".to_string()),
                Some("high".to_string()),
            )
            .with_reuse_attestation_target(unattested_product_id),
            PendingReviewAspect::avionics(
                "attestation-missing-evidence",
                "avionics_identity",
                "Garmin GTX 345",
                "Garmin GTX 345 transponder",
                "catalog_match_requires_review",
                1,
                "installed",
                None,
                None,
            )
            .with_reuse_attestation_target(unattested_product_id),
        ];
        let staged = stage_pending_review(&db, listing_id, Some(submission_id), &aspects)
            .await
            .unwrap();
        let links_before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();

        let groups = list_pending_product_reviews(&db, user_id, ProductReviewPageQuery::default())
            .await
            .unwrap();
        let attested_group = groups
            .items
            .iter()
            .find(|group| group.product.id == Some(attested_product_id))
            .unwrap();
        assert_eq!(
            attested_group.eligibility_counts,
            ProductAssociationEligibilityCounts {
                ready_local: 1,
                source_evidence_missing: 1,
                product_attestation_required: 0,
                manual_review_required: 0,
            }
        );
        let unattested_group = groups
            .items
            .iter()
            .find(|group| group.product.id == Some(unattested_product_id))
            .unwrap();
        assert_eq!(
            unattested_group.eligibility_counts,
            ProductAssociationEligibilityCounts {
                ready_local: 0,
                source_evidence_missing: 1,
                product_attestation_required: 1,
                manual_review_required: 0,
            }
        );

        let first = list_pending_product_associations(
            &db,
            user_id,
            attested_product_id,
            ProductReviewPageQuery {
                limit: Some(1),
                cursor: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(first.associations.len(), 1);
        assert_eq!(
            first.associations[0].verification_eligibility,
            ProductAssociationVerificationEligibility {
                status: ProductAssociationEligibilityStatus::AutoVerifiable,
                reason_code: None,
                reason: None,
            }
        );
        let serialized = serde_json::to_value(&first).unwrap();
        assert_eq!(
            serialized["associations"][0]["verification_eligibility"]["status"],
            "auto_verifiable"
        );
        assert!(serialized["associations"][0]["verification_eligibility"]
            .get("reason_code")
            .is_none());
        let second = list_pending_product_associations(
            &db,
            user_id,
            attested_product_id,
            ProductReviewPageQuery {
                limit: Some(1),
                cursor: first.next_cursor,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            second.associations[0].verification_eligibility.status,
            ProductAssociationEligibilityStatus::ManualReviewRequired
        );
        assert_eq!(
            second.associations[0]
                .verification_eligibility
                .reason_code
                .as_deref(),
            Some("source_evidence_missing")
        );
        assert!(second.associations[0]
            .verification_eligibility
            .reason
            .as_deref()
            .is_some_and(|reason| reason
                == "No exact visible listing-source excerpt is retained for this association. Recover source evidence from the listing before retrying local validation."));

        let unattested = list_pending_product_associations(
            &db,
            user_id,
            unattested_product_id,
            ProductReviewPageQuery::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            unattested.associations[0].verification_eligibility,
            product_attestation_required_eligibility(unattested_product_id)
        );

        let links_after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(links_after, links_before);
        let retained_hash: String = sqlx::query_scalar(
            "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(retained_hash, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn product_attestation_uses_only_the_supplied_direct_association_authorization() {
        let db = test_db().await;
        let (user_id, malformed_listing_id) = insert_listing(&db).await;
        let target_listing_id = insert_additional_listing(
            &db,
            user_id,
            "https://broker.example/aircraft/direct-attestation",
        )
        .await;
        let product_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let pool = sqlite_pool(&db);
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GNS 430W shown in the listing',
                      'high', 'installed')
            "#,
        )
        .bind(target_listing_id)
        .bind(product_id)
        .execute(pool)
        .await
        .unwrap();

        let malformed_staged = stage_pending_review(
            &db,
            malformed_listing_id,
            None,
            &[PendingReviewAspect::avionics(
                "unrelated",
                "avionics_identity",
                "Unrelated unit",
                "Unrelated unit",
                "catalog_match_requires_review",
                1,
                "installed",
                Some("Unrelated unit".to_string()),
                Some("high".to_string()),
            )],
        )
        .await
        .unwrap();
        let assignments = load_existing_assignments(&db, target_listing_id)
            .await
            .unwrap();
        let products = load_all_approved_product_map(&db).await.unwrap();
        let target_aspect = preserved_product_aspect(
            &assignments[0],
            ListingAssociationRole::Installed,
            products.get(&product_id).unwrap(),
            None,
        );
        let target_staged =
            stage_pending_review(&db, target_listing_id, None, &[target_aspect.clone()])
                .await
                .unwrap();
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;

        sqlx::query(
            "UPDATE aircraft_sale_listing_pending_reviews SET review_payload_json = '{malformed' WHERE listing_id = ?",
        )
        .bind(malformed_listing_id)
        .execute(pool)
        .await
        .unwrap();

        let target = preflight_pending_product_attestation(
            &db,
            user_id,
            product_id,
            target_listing_id,
            &target_staged.review_payload_sha256,
            &target_aspect.id,
            &target_staged.catalog_revision_sha256,
            "",
            "",
            "",
        )
        .await
        .expect("an unrelated malformed review must not poison direct authorization");
        assert!(target.already_reuse_attested);
        assert_eq!(target.commit_guard.listing_id, target_listing_id);

        assert!(matches!(
            preflight_pending_product_attestation(
                &db,
                user_id,
                product_id,
                target_listing_id,
                &target_staged.review_payload_sha256,
                &ReviewAspectId::from("different-aspect"),
                &target_staged.catalog_revision_sha256,
                "",
                "",
                "",
            )
            .await,
            Err(ReviewError::NotFound(_))
        ));
        assert!(matches!(
            preflight_pending_product_attestation(
                &db,
                user_id,
                product_id,
                malformed_listing_id,
                &malformed_staged.review_payload_sha256,
                &target_aspect.id,
                &target_staged.catalog_revision_sha256,
                "",
                "",
                "",
            )
            .await,
            Err(ReviewError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn explicit_restage_hash_binds_hidden_preserved_product_and_is_idempotent() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let preserved_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let reviewed_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GNS 430W shown in the listing',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(preserved_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let original = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("reviewed-unit", reviewed_id)],
        )
        .await
        .unwrap();

        let restaged = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap()
            .expect("the reviewed aspect must remain pending");
        assert_ne!(
            restaged.review_payload_sha256,
            original.review_payload_sha256
        );
        assert_eq!(restaged.pending_aspect_count, 2);
        let detail = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;
        let preserved = detail
            .aspects
            .iter()
            .find(|aspect| {
                aspect
                    .reuse_attestation_target
                    .as_ref()
                    .and_then(|product| product.id)
                    == Some(preserved_id)
            })
            .expect("GET must expose the formerly hidden preserved product");
        assert_eq!(
            preserved.id,
            preserved_review_aspect_id(link_id, ListingAssociationRole::Installed)
        );
        assert_eq!(preserved.allowed_actions, vec![ReviewAction::Discard]);
        assert_eq!(
            preserved.reuse_attestation_status,
            Some(ProductAttestationStatus::Required)
        );
        assert_eq!(preserved.observed_text, "Garmin GNS 430W");
        assert_eq!(preserved.source_evidence_text, None);
        assert_eq!(preserved.source_confidence, None);

        let second = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap();
        assert_eq!(second, Some(restaged), "restaging must be idempotent");

        let stale = restage_pending_review_if_current(
            &db,
            user_id,
            listing_id,
            &original.review_payload_sha256,
        )
        .await
        .expect_err("a concurrent review mutation must not be overwritten");
        assert!(matches!(stale, ReviewError::Stale(_)));
    }

    #[tokio::test]
    async fn product_queue_prepare_is_owner_scoped_provider_free_and_idempotent() {
        let db = test_db().await;
        let (owner_user_id, owned_listing_id) = insert_listing(&db).await;
        let (_, foreign_listing_id) = insert_listing(&db).await;
        let pool = sqlite_pool(&db);
        let foreign_user_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO users (email, display_name, auth_provider, auth_subject)
            VALUES ('other-reviewer@example.test', 'Other reviewer', 'local', 'other-reviewer')
            RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE aircraft_sale_listings SET created_by_user_id = ? WHERE id = ?")
            .bind(foreign_user_id)
            .bind(foreign_listing_id)
            .execute(pool)
            .await
            .unwrap();

        let preserved_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let reviewed_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        for listing_id in [owned_listing_id, foreign_listing_id] {
            sqlx::query(
                r#"
                INSERT INTO aircraft_sale_listing_avionics (
                  aircraft_sale_listing_id, avionics_model_id, quantity, source,
                  source_notes, source_confidence, configuration_action
                ) VALUES (?, ?, 1, 'listing', 'Garmin GNS 430W shown in the listing',
                          'high', 'installed')
                "#,
            )
            .bind(listing_id)
            .bind(preserved_id)
            .execute(pool)
            .await
            .unwrap();
            stage_pending_review(
                &db,
                listing_id,
                None,
                &[pending_aspect("reviewed-unit", reviewed_id)],
            )
            .await
            .unwrap();
        }
        let gemini_usage_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();

        let first = prepare_pending_product_reviews(&db, owner_user_id)
            .await
            .unwrap();
        assert_eq!(first.inspected_listing_count, 1);
        assert_eq!(first.restaged_listing_count, 1);
        assert_eq!(first.catalog_revision_sha256.len(), 64);
        assert_eq!(
            list_pending_product_reviews(&db, owner_user_id, ProductReviewPageQuery::default())
                .await
                .unwrap()
                .items
                .iter()
                .filter(|group| group.product.id == Some(preserved_id))
                .count(),
            1
        );
        assert!(list_pending_product_reviews(
            &db,
            foreign_user_id,
            ProductReviewPageQuery::default()
        )
        .await
        .unwrap()
        .items
        .iter()
        .all(|group| group.product.id != Some(preserved_id)));

        let second = prepare_pending_product_reviews(&db, owner_user_id)
            .await
            .unwrap();
        assert_eq!(second.inspected_listing_count, 1);
        assert_eq!(second.restaged_listing_count, 0);
        assert_eq!(
            second.catalog_revision_sha256,
            first.catalog_revision_sha256
        );
        let gemini_usage_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(gemini_usage_after, gemini_usage_before);
    }

    #[tokio::test]
    async fn explicit_restage_recovers_model_only_evidence_for_local_validation() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<main><div>Garmin integrated avionics package.</div><div>Installed GMA-1347 Digital Audio Panel.</div></main>",
        )
        .await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, configuration_action
            ) VALUES (?, ?, 1, 'listing', ?, 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .bind("Generated resolver prose that is not listing evidence")
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let legacy = PendingReviewAspect::avionics(
            "legacy-audio-panel",
            "avionics",
            "Garmin GMA-1347",
            "Garmin GMA-1347 · Audio Panel · quantity 1 · installed",
            "listing_action_graph_invalid,catalog_product_unverified,listing_link_confidence_not_high",
            1,
            "installed",
            None,
            None,
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GMA-1347",
            vec!["Audio Panel".to_string()],
        ))
        .with_covered_association(link_id, ListingAssociationRole::Installed, product_id);
        let original = stage_pending_review(&db, listing_id, Some(submission_id), &[legacy])
            .await
            .unwrap();

        let restaged = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap()
            .expect("the recovered association still requires corroboration");
        assert_ne!(
            restaged.review_payload_sha256,
            original.review_payload_sha256
        );
        let link: (String, String) = sqlx::query_as(
            "SELECT source_notes, source_confidence FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(link, ("GMA-1347".to_string(), "high".to_string()));
        let row = load_review_row(&db, listing_id).await.unwrap();
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        assert_eq!(payload.aspects.len(), 1);
        let recovered = &payload.aspects[0];
        assert!(is_synthetic_preserved_attestation_aspect(recovered));
        assert_eq!(
            recovered.id,
            preserved_review_aspect_id(link_id, ListingAssociationRole::Installed)
        );
        assert_eq!(recovered.source_evidence_text.as_deref(), Some("GMA-1347"));

        assert!(matches!(
            evaluate_existing_product_association(
                &db,
                user_id,
                listing_id,
                &recovered.id,
                &restaged.review_payload_sha256,
                &restaged.catalog_revision_sha256,
            )
            .await
            .unwrap(),
            ExistingProductAssociationEvaluation::AutoVerifiable(_)
        ));

        let second = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap();
        assert_eq!(second, Some(restaged), "recovery must be idempotent");
    }

    #[tokio::test]
    async fn maintenance_repair_rejects_a_corrupt_retained_capture_hash() {
        let db = test_db().await;
        let (user_id, listing_id, submission_id, link_id, staged) =
            insert_repairable_capture_review(&db).await;
        sqlx::query("UPDATE plugin_submissions SET rendered_html_sha256 = ? WHERE id = ?")
            .bind("f".repeat(64))
            .bind(submission_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();

        let error = restage_pending_review_if_current(
            &db,
            user_id,
            listing_id,
            &staged.review_payload_sha256,
        )
        .await
        .expect_err("maintenance repair must reject a stale retained capture digest");
        assert!(
            matches!(&error, ReviewError::Stale(message) if message.contains("content hash")),
            "{error:?}"
        );
        let link: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT source_notes, source_confidence FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(link, (Some("Generated resolver prose".to_string()), None));
        assert_eq!(
            load_review_row(&db, listing_id)
                .await
                .unwrap()
                .review_payload_sha256,
            staged.review_payload_sha256
        );
    }

    #[tokio::test]
    async fn maintenance_repair_rejects_a_capture_url_mismatch() {
        let db = test_db().await;
        let (user_id, listing_id, submission_id, link_id, staged) =
            insert_repairable_capture_review(&db).await;
        sqlx::query("UPDATE plugin_submissions SET source_url = ? WHERE id = ?")
            .bind("https://broker.example/aircraft/other")
            .bind(submission_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();

        let error = restage_pending_review_if_current(
            &db,
            user_id,
            listing_id,
            &staged.review_payload_sha256,
        )
        .await
        .expect_err("maintenance repair must reject a mismatched retained capture URL");
        assert!(
            matches!(&error, ReviewError::Stale(message) if message.contains("does not match the listing source URL")),
            "{error:?}"
        );
        let link: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT source_notes, source_confidence FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(link, (Some("Generated resolver prose".to_string()), None));
        assert_eq!(
            load_review_row(&db, listing_id)
                .await
                .unwrap()
                .review_payload_sha256,
            staged.review_payload_sha256
        );
    }

    #[tokio::test]
    async fn explicit_restage_clears_unrecoverable_notes_and_staged_evidence() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GTX 33", "GTX33", "Transponder").await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<main>Garmin GTX 33 transponder and Garmin GTX 33 ES transponder.</main>",
        )
        .await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Generated resolver explanation',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let aspect = PendingReviewAspect::avionics(
            "ambiguous-transponder",
            "avionics",
            "Garmin GTX 33",
            "structured observation",
            "listing_link_confidence_not_high",
            1,
            "installed",
            Some("Generated resolver explanation".to_string()),
            Some("high".to_string()),
        )
        .with_covered_association(link_id, ListingAssociationRole::Installed, product_id);
        let original = stage_pending_review(&db, listing_id, Some(submission_id), &[aspect])
            .await
            .unwrap();

        let restaged = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap()
            .expect("ambiguous occurrence must remain pending");
        assert_ne!(
            restaged.review_payload_sha256,
            original.review_payload_sha256
        );
        let link: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT source_notes, source_confidence FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(link, (None, None));

        let row = load_review_row(&db, listing_id).await.unwrap();
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        assert_eq!(payload.aspects[0].source_evidence_text, None);
        assert_eq!(payload.aspects[0].source_confidence, None);
        assert_eq!(
            restage_unattested_preserved_products(&db, user_id, listing_id)
                .await
                .unwrap(),
            Some(restaged)
        );
    }

    #[tokio::test]
    async fn recovered_exact_association_keeps_primary_with_extra_capability() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GDL 69A", "GDL69A", "Datalink").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<main>Garmin GDL-69A Datalink Weather</main>",
        )
        .await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, configuration_action
            ) VALUES (?, ?, 1, 'listing', ?, 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .bind("Factory default description generated by a resolver")
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let primary = PendingReviewAspect::avionics(
            "gdl-capability-review",
            "avionics",
            "Garmin GDL-69A",
            "Garmin GDL-69A · Datalink, Weather Radar",
            "listing_action_graph_invalid,catalog_product_unverified,listing_link_confidence_not_high,capability_mismatch_or_unknown",
            1,
            "installed",
            None,
            None,
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GDL-69A",
            vec!["Datalink".to_string(), "Weather Radar".to_string()],
        ))
        .with_covered_association(link_id, ListingAssociationRole::Installed, product_id);
        stage_pending_review(&db, listing_id, Some(submission_id), &[primary])
            .await
            .unwrap();

        restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap();
        let row = load_review_row(&db, listing_id).await.unwrap();
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        assert_eq!(payload.aspects.len(), 1);
        let surviving = &payload.aspects[0];
        assert_eq!(surviving.id, ReviewAspectId::from("gdl-capability-review"));
        assert_eq!(surviving.kind, "avionics");
        assert!(surviving.reason.contains("capability_mismatch_or_unknown"));
        assert_eq!(
            surviving
                .proposed_product
                .as_ref()
                .map(|product| product.capabilities.clone()),
            Some(vec!["Datalink".to_string(), "Weather Radar".to_string()])
        );
        assert_eq!(
            surviving.source_evidence_text.as_deref(),
            Some("Garmin GDL-69A")
        );
    }

    #[tokio::test]
    async fn restage_transaction_recomputes_associations_after_optimistic_token_read() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let preserved_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let reviewed_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        let original = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("reviewed-unit", reviewed_id)],
        )
        .await
        .unwrap();

        // Model a link writer committing after the public restage preflight
        // read its optimistic token but before the restage transaction starts.
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'late Garmin GNS 430W association',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(preserved_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();

        let restaged = restage_pending_review_if_current(
            &db,
            user_id,
            listing_id,
            &original.review_payload_sha256,
        )
        .await
        .expect("the transaction must derive its payload from the current link set")
        .expect("the reviewed aspect must remain pending");
        assert_eq!(restaged.pending_aspect_count, 2);
        let row = load_review_row(&db, listing_id).await.unwrap();
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        assert!(payload.aspects.iter().any(|aspect| {
            aspect.id == preserved_review_aspect_id(link_id, ListingAssociationRole::Installed)
                && aspect.reuse_attestation_target_id == Some(preserved_id)
        }));
    }

    #[tokio::test]
    async fn restage_transaction_rechecks_source_revocation_after_optimistic_token_read() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let preserved_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let reviewed_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, preserved_id).await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'attested Garmin GNS 430W association',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(preserved_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let original = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("reviewed-unit", reviewed_id)],
        )
        .await
        .unwrap();

        // The optimistic token predates this revocation. A restage must not
        // reuse an eligibility result computed before the transaction locks
        // the source-origin graph.
        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origin_revocations (
              avionics_authoritative_source_origin_id, revoked_by_user_id, reason
            )
            SELECT avionics_authoritative_source_origin_id, ?,
                   'Authoritative origin revoked by concurrent policy review'
            FROM avionics_product_reuse_attestations
            WHERE avionics_model_id = ?
            "#,
        )
        .bind(user_id)
        .bind(preserved_id)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();

        let restaged = restage_pending_review_if_current(
            &db,
            user_id,
            listing_id,
            &original.review_payload_sha256,
        )
        .await
        .expect("revoked reuse eligibility must become a hash-bound aspect")
        .expect("the reviewed aspect must remain pending");
        assert_eq!(restaged.pending_aspect_count, 2);
        let row = load_review_row(&db, listing_id).await.unwrap();
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        assert!(payload.aspects.iter().any(|aspect| {
            aspect.id == preserved_review_aspect_id(link_id, ListingAssociationRole::Installed)
                && aspect.reuse_attestation_target_id == Some(preserved_id)
        }));
    }

    #[tokio::test]
    async fn explicit_restage_rebuilds_a_stale_synthetic_preserved_link_card() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let original_product_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let changed_product_id = insert_approved_product(&db, "GNS 530W", "GNS530W", "GPS").await;
        let reviewed_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GNS association',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(original_product_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("reviewed-unit", reviewed_id)],
        )
        .await
        .unwrap();
        let restaged = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap()
            .expect("the review must remain pending");

        sqlx::query("UPDATE aircraft_sale_listing_avionics SET avionics_model_id = ? WHERE id = ?")
            .bind(changed_product_id)
            .bind(link_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();
        let refreshed = restage_pending_review_if_current(
            &db,
            user_id,
            listing_id,
            &restaged.review_payload_sha256,
        )
        .await
        .expect("explicit restage may recover a synthetic preserved-link card")
        .expect("the independent reviewed aspect remains pending");
        assert_ne!(
            refreshed.review_payload_sha256,
            restaged.review_payload_sha256
        );
        let row = load_review_row(&db, listing_id).await.unwrap();
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        assert!(payload.aspects.iter().any(|aspect| {
            aspect.id == preserved_review_aspect_id(link_id, ListingAssociationRole::Installed)
                && aspect.reuse_attestation_target_id == Some(changed_product_id)
        }));
        assert!(payload
            .aspects
            .iter()
            .any(|aspect| aspect.id == ReviewAspectId::from("reviewed-unit")));
    }

    #[tokio::test]
    async fn explicit_restage_rejects_a_stale_ordinary_covered_observation() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let original_product_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let changed_product_id = insert_approved_product(&db, "GNS 530W", "GNS530W", "GPS").await;
        let link_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id, configuration_action) VALUES (?, ?, 'installed') RETURNING id",
        )
        .bind(listing_id)
        .bind(original_product_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let ordinary = pending_aspect("ordinary-covered", original_product_id)
            .with_covered_association(
                link_id,
                ListingAssociationRole::Installed,
                original_product_id,
            );
        let staged = stage_pending_review(&db, listing_id, None, &[ordinary])
            .await
            .unwrap();
        sqlx::query("UPDATE aircraft_sale_listing_avionics SET avionics_model_id = ? WHERE id = ?")
            .bind(changed_product_id)
            .bind(link_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();
        assert!(matches!(
            restage_pending_review_if_current(
                &db,
                user_id,
                listing_id,
                &staged.review_payload_sha256,
            )
            .await,
            Err(ReviewError::Stale(_))
        ));
        let row = load_review_row(&db, listing_id).await.unwrap();
        assert_eq!(row.review_payload_sha256, staged.review_payload_sha256);
    }

    #[test]
    fn postgres_restage_locks_every_mutable_reuse_dependency_in_global_order() {
        let locked_tables = POSTGRES_RESTAGE_CATALOG_LOCK_SQL
            .trim()
            .strip_prefix("LOCK TABLE ")
            .unwrap()
            .strip_suffix("IN SHARE ROW EXCLUSIVE MODE")
            .unwrap()
            .split(',')
            .map(str::trim)
            .collect::<Vec<_>>();
        assert_eq!(
            locked_tables,
            vec![
                "avionics_models",
                "avionics_model_types",
                "avionics_types",
                "avionics_manufacturers",
                "avionics_manufacturer_identities",
                "avionics_manufacturer_identity_memberships",
                "avionics_manufacturer_identity_merges",
                "avionics_approved_product_identities",
                "avionics_product_reuse_attestations",
                "avionics_authoritative_source_origins",
                "avionics_authoritative_source_origin_revocations",
            ]
        );
        assert!(
            POSTGRES_LISTING_CHILD_LOCK_SQL.contains("aircraft_sale_listing_avionics")
                && POSTGRES_LISTING_CHILD_LOCK_SQL
                    .contains("aircraft_sale_listing_avionics_authorizations")
                && POSTGRES_LISTING_CHILD_LOCK_SQL
                    .contains("aircraft_sale_listing_pending_reviews")
                && POSTGRES_LISTING_CHILD_LOCK_SQL.contains("IN SHARE ROW EXCLUSIVE MODE")
        );
    }

    #[test]
    fn aspect_scoped_approval_uses_catalog_then_listing_child_lock_order() {
        let catalog = POSTGRES_RESTAGE_CATALOG_LOCK_SQL;
        let children = POSTGRES_LISTING_CHILD_LOCK_SQL;
        assert!(catalog.contains("avionics_product_reuse_attestations"));
        assert!(catalog.contains("avionics_authoritative_source_origin_revocations"));
        let links = children
            .find("aircraft_sale_listing_avionics,")
            .expect("listing links must be locked first");
        let corroborations = children
            .find("aircraft_sale_listing_avionics_authorizations")
            .expect("association corroborations must be locked");
        let pending = children
            .find("aircraft_sale_listing_pending_reviews")
            .expect("pending review must be locked");
        assert!(links < corroborations && corroborations < pending);
    }

    #[tokio::test]
    async fn aspect_scoped_approval_preserves_exact_unlinked_quantities_and_restages() {
        for quantity in [1, 2, 3] {
            let db = test_db().await;
            let (user_id, listing_id) = insert_listing(&db).await;
            let product_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
            attest_approved_product_for_current_policy_reuse(&db, product_id).await;
            let mut selected = pending_aspect("selected", product_id);
            selected.quantity = quantity;
            let staged = stage_pending_review(
                &db,
                listing_id,
                None,
                &[selected, pending_aspect("remaining", product_id)],
            )
            .await
            .unwrap();

            let restaged = use_existing_product_for_aspect_and_restage(
                &db,
                user_id,
                listing_id,
                &ReviewAspectId::from("selected"),
                &staged.review_payload_sha256,
                &staged.catalog_revision_sha256,
                product_id,
            )
            .await
            .unwrap()
            .expect("the residual aspect must remain pending");

            assert_ne!(restaged.review_payload_sha256, staged.review_payload_sha256);
            assert_eq!(restaged.pending_aspect_count, 1);
            let link: (i64, i64, String, Option<String>, Option<String>) = sqlx::query_as(
                r#"
                SELECT avionics_model_id, quantity, source, source_notes, source_confidence
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
                    product_id,
                    quantity,
                    "listing_review".to_string(),
                    Some("GTX 345 shown in listing equipment".to_string()),
                    Some("high".to_string()),
                )
            );
            let row = load_review_row(&db, listing_id).await.unwrap();
            let payload = parse_payload(
                &row.review_payload_json,
                Some(&row.review_payload_sha256),
                row.pending_aspect_count,
            )
            .unwrap();
            assert_eq!(payload.aspects.len(), 1);
            assert_eq!(payload.aspects[0].id, ReviewAspectId::from("remaining"));

            let stale_retry = use_existing_product_for_aspect_and_restage(
                &db,
                user_id,
                listing_id,
                &ReviewAspectId::from("selected"),
                &staged.review_payload_sha256,
                &staged.catalog_revision_sha256,
                product_id,
            )
            .await
            .expect_err("a retry with the consumed review hash must be stale");
            assert!(matches!(stale_retry, ReviewError::Stale(_)));
        }
    }

    #[tokio::test]
    async fn aspect_scoped_approval_updates_exact_covered_link_in_place() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let legacy_id =
            insert_approved_product(&db, "Legacy GTX", "LEGACYGTX", "Transponder").await;
        let approved_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, approved_id).await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 2, 'listing', 'Legacy GTX shown in listing',
                      'medium', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(legacy_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let covered = PendingReviewAspect::avionics(
            "covered",
            "avionics_identity",
            "Legacy GTX",
            "Legacy GTX shown in listing",
            "catalog_match_requires_review",
            2,
            "installed",
            Some("Legacy GTX shown in listing".to_string()),
            Some("medium".to_string()),
        )
        .with_covered_association(link_id, ListingAssociationRole::Installed, legacy_id);
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[covered, pending_aspect("remaining", approved_id)],
        )
        .await
        .unwrap();

        let restaged = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("covered"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            approved_id,
        )
        .await
        .unwrap()
        .expect("the residual aspect must remain pending");

        assert_eq!(restaged.pending_aspect_count, 1);
        let link: (i64, i64, i64, String, Option<String>) = sqlx::query_as(
            r#"
            SELECT id, avionics_model_id, quantity, source, source_confidence
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
                link_id,
                approved_id,
                2,
                "listing_review".to_string(),
                Some("high".to_string()),
            )
        );
    }

    #[tokio::test]
    async fn aspect_scoped_approval_rejects_covered_quantity_mismatch_without_mutation() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 2, 'listing', 'Two Garmin GTX 345 units',
                      'medium', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let mut covered = pending_aspect("covered", product_id).with_covered_association(
            link_id,
            ListingAssociationRole::Installed,
            product_id,
        );
        covered.quantity = 3;
        let staged = stage_pending_review(&db, listing_id, None, &[covered])
            .await
            .unwrap();

        let error = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("covered"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            product_id,
        )
        .await
        .expect_err("the staged quantity must still describe the exact covered link");

        assert!(matches!(error, ReviewError::Stale(_)));
        let quantity: i64 =
            sqlx::query_scalar("SELECT quantity FROM aircraft_sale_listing_avionics WHERE id = ?")
                .bind(link_id)
                .fetch_one(sqlite_pool(&db))
                .await
                .unwrap();
        assert_eq!(quantity, 2);
        let row = load_review_row(&db, listing_id).await.unwrap();
        assert_eq!(row.review_payload_sha256, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn final_aspect_scoped_approval_clears_review_without_finalizing_listing() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let mut aspect = pending_aspect("only", product_id);
        aspect.quantity = 3;
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();

        let result = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("only"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            product_id,
        )
        .await
        .unwrap();

        assert_eq!(result, None);
        let state: (String, bool) = sqlx::query_as(
            "SELECT ingestion_state, is_verified FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(state, ("incomplete".to_string(), false));
        let quantity: i64 = sqlx::query_scalar(
            "SELECT quantity FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(quantity, 3);
    }

    #[tokio::test]
    async fn aspect_scoped_approval_rejects_same_product_without_merging_quantities() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing_review', 'first distinct observation',
                      'high', 'installed')
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();
        let mut second = pending_aspect("second", product_id);
        second.quantity = 2;
        let staged = stage_pending_review(&db, listing_id, None, &[second])
            .await
            .unwrap();

        let error = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("second"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            product_id,
        )
        .await
        .expect_err("independent quantities must never be merged implicitly");

        assert!(matches!(
            error,
            ReviewError::Conflict(message) if message.contains("refuses to merge")
        ));
        let quantity: i64 = sqlx::query_scalar(
            "SELECT quantity FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(quantity, 1);
        let row = load_review_row(&db, listing_id).await.unwrap();
        assert_eq!(row.review_payload_sha256, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn aspect_scoped_approval_rejects_displaced_product_collision_on_every_retry() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let replacement_id = insert_approved_product(&db, "GTN 750Xi", "GTN750XI", "GPS").await;
        let displaced_id = insert_approved_product(&db, "GNS 530W", "GNS530W", "GPS").await;
        attest_approved_product_for_current_policy_reuse(&db, displaced_id).await;
        let replacement_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action,
              replaces_avionics_model_id
            ) VALUES (?, ?, 1, 'listing_review',
                      'GTN 750Xi replaces GNS 530W', 'high', 'replaces', ?)
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(replacement_id)
        .bind(displaced_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let mut selected = pending_aspect("selected", displaced_id);
        selected.quantity = 3;
        let staged = stage_pending_review(&db, listing_id, None, &[selected])
            .await
            .unwrap();

        for _ in 0..2 {
            let error = use_existing_product_for_aspect_and_restage(
                &db,
                user_id,
                listing_id,
                &ReviewAspectId::from("selected"),
                &staged.review_payload_sha256,
                &staged.catalog_revision_sha256,
                displaced_id,
            )
            .await
            .expect_err("an installed product cannot also be a displacement target");
            assert!(matches!(
                error,
                ReviewError::Conflict(message)
                    if message.contains("installs or displaces")
                        && message.contains("refuses to merge or contradict")
            ));
        }

        let links: Vec<(i64, i64, i64, String, Option<i64>)> = sqlx::query_as(
            r#"
            SELECT id, avionics_model_id, quantity, configuration_action,
                   replaces_avionics_model_id
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            ORDER BY id
            "#,
        )
        .bind(listing_id)
        .fetch_all(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(
            links,
            vec![(
                replacement_link_id,
                replacement_id,
                1,
                "replaces".to_string(),
                Some(displaced_id),
            )]
        );
        let row = load_review_row(&db, listing_id).await.unwrap();
        assert_eq!(row.review_payload_sha256, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn aspect_scoped_approval_rejects_stale_revoked_and_coupled_inputs() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("selected", product_id)],
        )
        .await
        .unwrap();

        let stale = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("selected"),
            &"a".repeat(64),
            &staged.catalog_revision_sha256,
            product_id,
        )
        .await
        .expect_err("stale review hashes must fail");
        assert!(matches!(stale, ReviewError::Stale(_)));
        let stale_catalog = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("selected"),
            &staged.review_payload_sha256,
            &"b".repeat(64),
            product_id,
        )
        .await
        .expect_err("stale catalog hashes must fail");
        assert!(matches!(stale_catalog, ReviewError::Stale(_)));

        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origin_revocations (
              avionics_authoritative_source_origin_id, revoked_by_user_id, reason
            )
            SELECT avionics_authoritative_source_origin_id, ?,
                   'revoked during aspect-scoped test'
            FROM avionics_product_reuse_attestations
            WHERE avionics_model_id = ?
            "#,
        )
        .bind(user_id)
        .bind(product_id)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();
        let revoked = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("selected"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            product_id,
        )
        .await
        .expect_err("revoked products must fail under the transaction lock");
        assert!(matches!(revoked, ReviewError::Conflict(_)));

        let other_db = test_db().await;
        let (other_user_id, other_listing_id) = insert_listing(&other_db).await;
        let subject_id = insert_approved_product(&other_db, "GTN 750Xi", "GTN750XI", "GPS").await;
        let target_id = insert_approved_product(&other_db, "GNS 530W", "GNS530W", "GPS").await;
        attest_approved_product_for_current_policy_reuse(&other_db, subject_id).await;
        attest_approved_product_for_current_policy_reuse(&other_db, target_id).await;
        let mut coupled = pending_aspect("parent", subject_id).with_replacement_product(target_id);
        coupled.configuration_action = "replaces".to_string();
        let other_staged = stage_pending_review(&other_db, other_listing_id, None, &[coupled])
            .await
            .unwrap();
        let coupled_error = use_existing_product_for_aspect_and_restage(
            &other_db,
            other_user_id,
            other_listing_id,
            &ReviewAspectId::from("parent"),
            &other_staged.review_payload_sha256,
            &other_staged.catalog_revision_sha256,
            subject_id,
        )
        .await
        .expect_err("replacement actions require complete review");
        assert!(matches!(
            coupled_error,
            ReviewError::Validation(message) if message.contains("coupled")
        ));
    }

    #[tokio::test]
    async fn aspect_scoped_approval_rejects_unapproved_and_synthetic_aspects() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let unapproved_id =
            insert_unreviewed_product(&db, "Unknown Unit", "UNKNOWNUNIT", "GPS").await;
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("ordinary", unapproved_id)],
        )
        .await
        .unwrap();
        let unapproved = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("ordinary"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            unapproved_id,
        )
        .await
        .expect_err("unapproved products must not be linked");
        assert!(matches!(unapproved, ReviewError::Conflict(_)));

        let other_db = test_db().await;
        let (other_user_id, other_listing_id) = insert_listing(&other_db).await;
        let product_id = insert_approved_product(&other_db, "GNS 430W", "GNS430W", "GPS").await;
        attest_approved_product_for_current_policy_reuse(&other_db, product_id).await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GNS 430W',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(other_listing_id)
        .bind(product_id)
        .fetch_one(sqlite_pool(&other_db))
        .await
        .unwrap();
        let assignment = load_existing_assignments(&other_db, other_listing_id)
            .await
            .unwrap()
            .into_iter()
            .find(|assignment| assignment.listing_link_id == link_id)
            .unwrap();
        let product = load_all_approved_product_map(&other_db)
            .await
            .unwrap()
            .remove(&product_id)
            .unwrap();
        let synthetic = preserved_product_aspect(
            &assignment,
            ListingAssociationRole::Installed,
            &product,
            None,
        );
        let synthetic_id = synthetic.id.clone();
        let synthetic_staged =
            stage_pending_review(&other_db, other_listing_id, None, &[synthetic])
                .await
                .unwrap();
        let synthetic_error = use_existing_product_for_aspect_and_restage(
            &other_db,
            other_user_id,
            other_listing_id,
            &synthetic_id,
            &synthetic_staged.review_payload_sha256,
            &synthetic_staged.catalog_revision_sha256,
            product_id,
        )
        .await
        .expect_err("synthetic maintenance aspects use the verification endpoint");
        assert!(matches!(
            synthetic_error,
            ReviewError::Validation(message) if message.contains("ordinary")
        ));
    }

    #[tokio::test]
    async fn aspect_scoped_approval_restages_hidden_preserved_blockers() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let selected_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        let preserved_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        attest_approved_product_for_current_policy_reuse(&db, selected_id).await;
        let preserved_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GNS 430W',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(preserved_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("selected", selected_id)],
        )
        .await
        .unwrap();

        let restaged = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("selected"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            selected_id,
        )
        .await
        .unwrap()
        .expect("the hidden preserved link must become explicit");

        assert_eq!(restaged.pending_aspect_count, 1);
        let row = load_review_row(&db, listing_id).await.unwrap();
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        assert_eq!(
            payload.aspects[0].id,
            preserved_review_aspect_id(preserved_link_id, ListingAssociationRole::Installed)
        );
        assert_eq!(
            payload.aspects[0].reuse_attestation_target_id,
            Some(preserved_id)
        );
    }

    fn collision_fingerprint_row(
        id: i64,
        model: &str,
        manufacturer_identifier: &str,
    ) -> ActiveCollisionCatalogFingerprintRow {
        ActiveCollisionCatalogFingerprintRow {
            id,
            catalog_status: "unreviewed".to_string(),
            effective_manufacturer_identity_id: Some(1),
            model: model.to_string(),
            manufacturer_identifier_kind: Some("manufacturer_part_number".to_string()),
            manufacturer_identifier: Some(manufacturer_identifier.to_string()),
        }
    }

    #[test]
    fn same_case_product_fingerprint_binds_graph_identity_and_source_proof() {
        let product = CatalogFingerprintProduct {
            id: 7,
            manufacturer: "Garmin".to_string(),
            model: "GTX 345".to_string(),
            capabilities: vec!["Transponder".to_string()],
            manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
            manufacturer_identifier: "GTX345".to_string(),
            avionics_manufacturer_identity_id: 11,
            canonical_product_key: "gtx345".to_string(),
            graph_manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
            canonical_identifier_key: "gtx345".to_string(),
            identity_source_url: "https://www.garmin.com/gtx345".to_string(),
            identity_source_title: "GTX 345".to_string(),
            identity_evidence_text: "Garmin identifies the GTX 345 model.".to_string(),
        };
        let original = fingerprint_catalog_product(&product);
        let mut mutations = Vec::new();
        let mut changed = product.clone();
        changed.avionics_manufacturer_identity_id += 1;
        mutations.push(changed);
        let mut changed = product.clone();
        changed.canonical_product_key.push('w');
        mutations.push(changed);
        let mut changed = product.clone();
        changed.graph_manufacturer_identifier_kind = "sku".to_string();
        mutations.push(changed);
        let mut changed = product.clone();
        changed.canonical_identifier_key.push('w');
        mutations.push(changed);
        let mut changed = product.clone();
        changed.identity_source_url.push_str("?revision=2");
        mutations.push(changed);
        let mut changed = product.clone();
        changed.identity_source_title.push_str(" product page");
        mutations.push(changed);
        let mut changed = product.clone();
        changed.identity_evidence_text.push_str(" Updated proof.");
        mutations.push(changed);

        for changed in mutations {
            assert_ne!(fingerprint_catalog_product(&changed), original);
        }
    }

    #[test]
    fn collision_closure_includes_other_identifier_equal_to_target_model() {
        let mut target = collision_fingerprint_row(1, "GTX 345R", "011-03520-00");
        target.catalog_status = "approved".to_string();
        let eligible = HashSet::from([target.id]);
        let baseline = fingerprint_active_collision_closure(
            std::slice::from_ref(&target),
            &eligible,
            target.id,
        )
        .unwrap();
        let unrelated = collision_fingerprint_row(2, "GMA 1347", "011-00809-00");
        assert_eq!(
            fingerprint_active_collision_closure(
                &[target.clone(), unrelated],
                &eligible,
                target.id,
            )
            .unwrap(),
            baseline,
            "unrelated active catalog rows must not invalidate a target marker"
        );
        let cross_field = collision_fingerprint_row(3, "Different Product", "GTX-345R");
        assert_ne!(
            fingerprint_active_collision_closure(
                &[target.clone(), cross_field],
                &eligible,
                target.id,
            )
            .unwrap(),
            baseline,
            "an identifier equal to the target model participates in the resolver's 2x2 collision relation"
        );
    }

    #[test]
    fn ordinary_local_verification_rejects_coupled_replacements_without_id_conventions() {
        let mut coupled = PendingReviewAspect::avionics(
            "observation-17",
            "avionics_identity",
            "Garmin GTN 750Xi",
            "Garmin GTN 750Xi replaces GNS 530W",
            "catalog_match_requires_review",
            1,
            "replaces",
            Some("Garmin GTN 750Xi replaces GNS 530W".to_string()),
            Some("high".to_string()),
        )
        .with_replacement_product(44)
        .with_reuse_attestation_target(43);
        coupled.allowed_actions = vec![ReviewAction::Discard];

        let error = validate_independent_ordinary_aspect(&coupled, &[coupled.clone()])
            .expect_err("replacement graphs require complete review");
        assert!(matches!(
            error,
            ReviewError::Validation(message) if message.contains("coupled")
        ));
    }

    #[test]
    fn collision_closure_includes_other_model_equal_to_target_identifier() {
        let mut target = collision_fingerprint_row(1, "GTX 345R", "011-03520-00");
        target.catalog_status = "approved".to_string();
        let eligible = HashSet::from([target.id]);
        let baseline = fingerprint_active_collision_closure(
            std::slice::from_ref(&target),
            &eligible,
            target.id,
        )
        .unwrap();
        let cross_field = collision_fingerprint_row(4, "011-03520-00", "011-DIFFERENT-04");
        assert_ne!(
            fingerprint_active_collision_closure(
                &[target.clone(), cross_field],
                &eligible,
                target.id,
            )
            .unwrap(),
            baseline,
            "a model equal to the target identifier participates in the resolver's 2x2 collision relation"
        );
    }

    #[tokio::test]
    async fn multi_quantity_association_corroboration_retires_only_link_572_not_real_aspect_9() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<html><body>Garmin GDL 69A shown in the listing. Weather Radar.</body></html>",
        )
        .await;
        let preserved_id = insert_approved_product(&db, "GDL 69A", "GDL69A", "Connectivity").await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              id, aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (572, ?, ?, 2, 'listing', 'Garmin GDL 69A shown in the listing',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(preserved_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let real_weather_radar_aspect = PendingReviewAspect::avionics(
            "avionics:9:primary",
            "avionics_capability",
            "Weather Radar",
            "Weather Radar",
            "capability_requires_review",
            1,
            "installed",
            Some("Weather Radar".to_string()),
            Some("medium".to_string()),
        );
        stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[real_weather_radar_aspect],
        )
        .await
        .unwrap();

        let initially_restaged = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap()
            .expect("the genuine extraction aspect must remain pending");
        assert_eq!(initially_restaged.pending_aspect_count, 2);
        let row = load_review_row(&db, listing_id).await.unwrap();
        let staged_payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        let genuine_primary = staged_payload
            .aspects
            .iter()
            .find(|aspect| aspect.id == ReviewAspectId::from("avionics:9:primary"))
            .cloned()
            .expect("the original extracted aspect must remain staged");
        let synthetic = staged_payload
            .aspects
            .iter()
            .find(|aspect| {
                aspect.reuse_attestation_target_id == Some(preserved_id)
                    && is_synthetic_preserved_attestation_aspect(aspect)
            })
            .expect("the unattested preserved association must be staged");
        assert_eq!(
            synthetic.id,
            preserved_review_aspect_id(link_id, ListingAssociationRole::Installed)
        );

        attest_approved_product_for_current_policy_reuse(&db, preserved_id).await;
        let product_only = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap()
            .expect("product attestation alone must not retire an association aspect");
        assert_eq!(
            product_only, initially_restaged,
            "global product attestation alone cannot corroborate listing link 572"
        );
        let identity_before_restage = load_all_approved_product_map(&db)
            .await
            .unwrap()
            .remove(&preserved_id)
            .expect("approved identity must exist");
        let association_before_restage: (i64, i64, i64, String) = sqlx::query_as(
            r#"
            SELECT id, avionics_model_id, quantity, configuration_action
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let usage_before_restage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();

        let assignment = load_existing_assignments(&db, listing_id)
            .await
            .unwrap()
            .into_iter()
            .find(|assignment| assignment.listing_link_id == link_id)
            .expect("link 572 must remain current");
        let evidence_text = assignment
            .source_notes
            .as_deref()
            .expect("link 572 has retained listing evidence");
        let observation_sha256 = association_observation_sha256(
            listing_id,
            &assignment,
            ListingAssociationRole::Installed,
            evidence_text,
        );
        let active_collision_revision = active_collision_closure_revision_sha256(&db, preserved_id)
            .await
            .unwrap();
        let review_row = load_review_row(&db, listing_id).await.unwrap();
        let evidence_provenance = load_listing_evidence_provenance(&db, &review_row, evidence_text)
            .await
            .unwrap();
        let restaged = corroborate_existing_product_association_and_restage(
            &db,
            user_id,
            listing_id,
            &preserved_review_aspect_id(link_id, ListingAssociationRole::Installed),
            &initially_restaged.review_payload_sha256,
            &initially_restaged.catalog_revision_sha256,
            &active_collision_revision,
            preserved_id,
            &observation_sha256,
            &evidence_provenance,
        )
        .await
        .unwrap()
        .expect("real aspect avionics:9:primary must remain pending");
        assert_ne!(
            restaged.review_payload_sha256,
            initially_restaged.review_payload_sha256
        );
        assert_eq!(restaged.pending_aspect_count, 1);
        let row = load_review_row(&db, listing_id).await.unwrap();
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        assert_eq!(
            payload.aspects,
            vec![genuine_primary],
            "corroborating link 572 must retire only its synthetic maintenance aspect"
        );
        assert!(payload
            .aspects
            .iter()
            .all(|aspect| aspect.reuse_attestation_target_id != Some(preserved_id)));

        let identity_after_restage = load_all_approved_product_map(&db)
            .await
            .unwrap()
            .remove(&preserved_id)
            .expect("approved identity must remain");
        assert_eq!(identity_after_restage, identity_before_restage);
        let association_after_restage: (i64, i64, i64, String) = sqlx::query_as(
            r#"
            SELECT id, avionics_model_id, quantity, configuration_action
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(association_after_restage, association_before_restage);
        let usage_after_restage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
        assert_eq!(usage_after_restage, usage_before_restage);
        let listing_state: (String, bool) = sqlx::query_as(
            "SELECT ingestion_state, is_verified FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(listing_state, ("pending_review".to_string(), false));

        let second = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap();
        assert_eq!(
            second,
            Some(restaged),
            "cleanup restaging must be idempotent"
        );

        insert_unreviewed_product(&db, "GMA 1347", "011-UNRELATED-00", "Audio Panel").await;
        let after_unrelated_change =
            restage_unattested_preserved_products(&db, user_id, listing_id)
                .await
                .unwrap();
        assert_eq!(
            after_unrelated_change, second,
            "an unrelated active catalog row must not invalidate a scoped marker"
        );

        insert_unreviewed_product(&db, "GDL 69A SXM", "011-03177-10", "Connectivity").await;
        let after_relevant_neighbor =
            restage_unattested_preserved_products(&db, user_id, listing_id)
                .await
                .unwrap()
                .expect("the real aspect and newly stale association must remain pending");
        assert_eq!(after_relevant_neighbor.pending_aspect_count, 2);
        let row = load_review_row(&db, listing_id).await.unwrap();
        let payload = parse_payload(
            &row.review_payload_json,
            Some(&row.review_payload_sha256),
            row.pending_aspect_count,
        )
        .unwrap();
        assert!(
            payload.aspects.iter().any(|aspect| {
                aspect.reuse_attestation_target_id == Some(preserved_id)
                    && is_synthetic_preserved_attestation_aspect(aspect)
            }),
            "a newly active GDL 69A SXM prefix-neighbor must invalidate the old GDL 69A association marker"
        );
    }

    #[tokio::test]
    async fn final_association_corroboration_clears_empty_review_and_marks_listing_incomplete() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<html><body>Garmin GMA 1347 audio panel</body></html>",
        )
        .await;
        let product_id = insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'Garmin GMA 1347 audio panel',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let assignment = load_existing_assignments(&db, listing_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let product = load_all_approved_product_map(&db)
            .await
            .unwrap()
            .remove(&product_id)
            .unwrap();
        let synthetic = preserved_product_aspect(
            &assignment,
            ListingAssociationRole::Installed,
            &product,
            assignment.source_notes.as_deref(),
        );
        let staged =
            stage_pending_review(&db, listing_id, Some(submission_id), &[synthetic.clone()])
                .await
                .unwrap();
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let evidence_text = assignment.source_notes.as_deref().unwrap();
        let observation_sha256 = association_observation_sha256(
            listing_id,
            &assignment,
            ListingAssociationRole::Installed,
            evidence_text,
        );
        let active_collision_revision = active_collision_closure_revision_sha256(&db, product_id)
            .await
            .unwrap();
        let review_row = load_review_row(&db, listing_id).await.unwrap();
        let evidence_provenance = load_listing_evidence_provenance(&db, &review_row, evidence_text)
            .await
            .unwrap();

        let restaged = corroborate_existing_product_association_and_restage(
            &db,
            user_id,
            listing_id,
            &synthetic.id,
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            &active_collision_revision,
            product_id,
            &observation_sha256,
            &evidence_provenance,
        )
        .await
        .unwrap();
        assert_eq!(restaged, None);
        let state: (String, bool) = sqlx::query_as(
            "SELECT ingestion_state, is_verified FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(state, ("incomplete".to_string(), false));
        let pending_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(pending_count, 0);
        let corroboration_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(corroboration_count, 1);
    }

    #[tokio::test]
    async fn preserved_quantity_change_after_preflight_is_rejected_under_lock() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let evidence = "Two Garmin GMA 1347 audio panels";
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            &format!("<html><body>{evidence}</body></html>"),
        )
        .await;
        let product_id = insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 2, 'listing', ?, 'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .bind(evidence)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let assignment = load_existing_assignments(&db, listing_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let product = load_all_approved_product_map(&db)
            .await
            .unwrap()
            .remove(&product_id)
            .unwrap();
        let synthetic = preserved_product_aspect(
            &assignment,
            ListingAssociationRole::Installed,
            &product,
            assignment.source_notes.as_deref(),
        );
        let staged =
            stage_pending_review(&db, listing_id, Some(submission_id), &[synthetic.clone()])
                .await
                .unwrap();
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;

        let target = preflight_existing_product_association(
            &db,
            user_id,
            listing_id,
            &synthetic.id,
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
        )
        .await
        .expect("an exact preserved quantity of two must pass preflight");
        let observation_sha256 = match &target.commit {
            ExistingProductAssociationCommit::CorroboratePreserved { observation_sha256 } => {
                observation_sha256.clone()
            }
            ExistingProductAssociationCommit::ApproveOrdinary => {
                panic!("a preserved association must use the corroboration commit")
            }
        };
        let active_collision_revision = active_collision_closure_revision_sha256(&db, product_id)
            .await
            .unwrap();

        sqlx::query("UPDATE aircraft_sale_listing_avionics SET quantity = 3 WHERE id = ?")
            .bind(link_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();

        let error = corroborate_existing_product_association_and_restage(
            &db,
            user_id,
            listing_id,
            &synthetic.id,
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            &active_collision_revision,
            product_id,
            &observation_sha256,
            &target.listing_evidence_provenance,
        )
        .await
        .expect_err("a quantity change after preflight must fail under the mutation lock");
        assert!(matches!(error, ReviewError::Stale(_)));
        assert!(error.to_string().contains("changed"));

        let corroboration_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(corroboration_count, 0);
        let retained_review_hash: String = sqlx::query_scalar(
            "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(retained_review_hash, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn association_corroboration_rejects_a_new_unreviewed_collision_under_lock() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<html><body>GMA 1347 audio panel</body></html>",
        )
        .await;
        let product_id = insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'GMA 1347 audio panel',
                      'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let assignment = load_existing_assignments(&db, listing_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let product = load_all_approved_product_map(&db)
            .await
            .unwrap()
            .remove(&product_id)
            .unwrap();
        let synthetic = preserved_product_aspect(
            &assignment,
            ListingAssociationRole::Installed,
            &product,
            assignment.source_notes.as_deref(),
        );
        let staged =
            stage_pending_review(&db, listing_id, Some(submission_id), &[synthetic.clone()])
                .await
                .unwrap();
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let expected_active_collision_revision =
            active_collision_closure_revision_sha256(&db, product_id)
                .await
                .unwrap();
        let observation_sha256 = association_observation_sha256(
            listing_id,
            &assignment,
            ListingAssociationRole::Installed,
            assignment.source_notes.as_deref().unwrap(),
        );
        let review_row = load_review_row(&db, listing_id).await.unwrap();
        let evidence_provenance = load_listing_evidence_provenance(
            &db,
            &review_row,
            assignment.source_notes.as_deref().unwrap(),
        )
        .await
        .unwrap();

        // Unreviewed rows do not alter the review's approved-only catalog
        // revision, but they can invalidate an exact local identity match.
        insert_unreviewed_product(&db, "GMA-1347", "DIFFERENT-1347", "Audio Panel").await;
        let error = corroborate_existing_product_association_and_restage(
            &db,
            user_id,
            listing_id,
            &synthetic.id,
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            &expected_active_collision_revision,
            product_id,
            &observation_sha256,
            &evidence_provenance,
        )
        .await
        .expect_err("the locked commit must reject a stale local collision snapshot");
        assert!(matches!(error, ReviewError::Stale(_)));
        assert!(error
            .to_string()
            .contains("active avionics collision catalog"));

        let corroboration_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(corroboration_count, 0);
        let pending_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(pending_count, 1);
    }

    #[tokio::test]
    async fn local_ordinary_approval_rejects_a_new_unreviewed_collision_under_lock() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<html><body>Garmin GMA 1347 audio panel</body></html>",
        )
        .await;
        let product_id = insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let aspect = PendingReviewAspect::avionics(
            "observation-17",
            "avionics_identity",
            "Garmin GMA 1347",
            "Garmin GMA 1347 audio panel",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("Garmin GMA 1347 audio panel".to_string()),
            Some("high".to_string()),
        )
        .with_reuse_attestation_target(product_id);
        let staged = stage_pending_review(&db, listing_id, Some(submission_id), &[aspect])
            .await
            .unwrap();
        let expected_active_collision_revision =
            active_collision_closure_revision_sha256(&db, product_id)
                .await
                .unwrap();
        let review_row = load_review_row(&db, listing_id).await.unwrap();
        let evidence_provenance =
            load_listing_evidence_provenance(&db, &review_row, "Garmin GMA 1347 audio panel")
                .await
                .unwrap();

        // This active row is outside the approved-only review revision but
        // invalidates the exact local product decision made before commit.
        insert_unreviewed_product(&db, "GMA-1347", "DIFFERENT-1347", "Audio Panel").await;
        let error = approve_locally_verified_ordinary_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("observation-17"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            &expected_active_collision_revision,
            product_id,
            &evidence_provenance,
        )
        .await
        .expect_err("the locked commit must reject a stale local collision snapshot");
        assert!(matches!(error, ReviewError::Stale(_)));
        assert!(error
            .to_string()
            .contains("active avionics collision catalog"));

        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(link_count, 0);
        let row = load_review_row(&db, listing_id).await.unwrap();
        assert_eq!(row.review_payload_sha256, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn local_ordinary_approval_rechecks_source_capture_under_lock() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<html><body>Garmin GMA 1347 audio panel</body></html>",
        )
        .await;
        let product_id = insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let aspect = PendingReviewAspect::avionics(
            "observation-17",
            "avionics_identity",
            "Garmin GMA 1347",
            "Garmin GMA 1347 audio panel",
            "catalog_match_requires_review",
            1,
            "installed",
            Some("Garmin GMA 1347 audio panel".to_string()),
            Some("high".to_string()),
        )
        .with_reuse_attestation_target(product_id);
        let staged = stage_pending_review(&db, listing_id, Some(submission_id), &[aspect])
            .await
            .unwrap();
        let expected_active_collision_revision =
            active_collision_closure_revision_sha256(&db, product_id)
                .await
                .unwrap();
        let review_row = load_review_row(&db, listing_id).await.unwrap();
        let evidence_provenance =
            load_listing_evidence_provenance(&db, &review_row, "Garmin GMA 1347 audio panel")
                .await
                .unwrap();

        sqlx::query("UPDATE plugin_submissions SET source_url = ? WHERE id = ?")
            .bind("https://other.example/aircraft/one")
            .bind(submission_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();
        let error = approve_locally_verified_ordinary_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("observation-17"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            &expected_active_collision_revision,
            product_id,
            &evidence_provenance,
        )
        .await
        .expect_err("a changed capture URL must invalidate the preflight authorization");
        assert!(matches!(error, ReviewError::Stale(_)));
        assert!(error.to_string().contains("source capture changed"));

        sqlx::query("UPDATE plugin_submissions SET source_url = ? WHERE id = ?")
            .bind("https://broker.example/aircraft/one")
            .bind(submission_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();
        let changed_capture =
            "<html><body>Garmin GMA 1347 audio panel in a changed capture</body></html>";
        sqlx::query(
            "UPDATE plugin_submissions SET rendered_html = ?, rendered_html_sha256 = ? WHERE id = ?",
        )
        .bind(changed_capture)
        .bind(sha256_hex(changed_capture.as_bytes()))
        .bind(submission_id)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();

        let error = approve_locally_verified_ordinary_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("observation-17"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            &expected_active_collision_revision,
            product_id,
            &evidence_provenance,
        )
        .await
        .expect_err("a changed source capture must invalidate the preflight authorization");
        assert!(matches!(error, ReviewError::Stale(_)));
        assert!(error.to_string().contains("source capture changed"));

        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(link_count, 0);
        let row = load_review_row(&db, listing_id).await.unwrap();
        assert_eq!(row.review_payload_sha256, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn local_verification_never_falls_back_to_observed_text() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            "<html><body>Garmin GMA 1347 audio panel</body></html>",
        )
        .await;
        let product_id = insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let aspect = PendingReviewAspect::avionics(
            "observation-17",
            "avionics_identity",
            "Garmin GMA 1347",
            "Garmin GMA 1347 audio panel",
            "catalog_match_requires_review",
            1,
            "installed",
            None,
            Some("high".to_string()),
        )
        .with_reuse_attestation_target(product_id);
        let staged = stage_pending_review(&db, listing_id, Some(submission_id), &[aspect])
            .await
            .unwrap();

        let error = preflight_existing_product_association(
            &db,
            user_id,
            listing_id,
            &ReviewAspectId::from("observation-17"),
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
        )
        .await
        .expect_err("observed_text must never substitute for missing source_evidence_text");
        assert!(matches!(
            error,
            ReviewError::Validation(message)
                if message.contains("no retained raw listing evidence")
        ));
    }

    #[tokio::test]
    async fn listing_evidence_capture_requires_exact_owner_listing_and_content_hash() {
        let evidence = "Garmin GMA 1347 audio panel";
        let rendered_html = format!("<html><body>{evidence}</body></html>");

        let owner_mismatch_db = test_db().await;
        let (owner_user_id, listing_id) = insert_listing(&owner_mismatch_db).await;
        let submission_id = insert_review_bound_submission(
            &owner_mismatch_db,
            owner_user_id,
            listing_id,
            &rendered_html,
        )
        .await;
        stage_pending_review(
            &owner_mismatch_db,
            listing_id,
            Some(submission_id),
            &[PendingReviewAspect::avionics(
                "owner-mismatch",
                "avionics_identity",
                evidence,
                evidence,
                "catalog_match_requires_review",
                1,
                "installed",
                Some(evidence.to_string()),
                Some("high".to_string()),
            )],
        )
        .await
        .unwrap();
        let other_user_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO users (email, display_name, auth_provider, auth_subject)
            VALUES ('other@example.test', 'Other', 'test', 'other-owner')
            RETURNING id
            "#,
        )
        .fetch_one(sqlite_pool(&owner_mismatch_db))
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_submissions SET user_id = ? WHERE id = ?")
            .bind(other_user_id)
            .bind(submission_id)
            .execute(sqlite_pool(&owner_mismatch_db))
            .await
            .unwrap();
        let review = load_review_row(&owner_mismatch_db, listing_id)
            .await
            .unwrap();
        let error = load_listing_evidence_provenance(&owner_mismatch_db, &review, evidence)
            .await
            .expect_err("a different capture owner must fail provenance");
        assert!(matches!(
            error,
            ReviewError::Validation(message)
                if message.contains("exact owner and canonical listing")
        ));

        let listing_mismatch_db = test_db().await;
        let (owner_user_id, listing_id) = insert_listing(&listing_mismatch_db).await;
        let submission_id = insert_review_bound_submission(
            &listing_mismatch_db,
            owner_user_id,
            listing_id,
            &rendered_html,
        )
        .await;
        stage_pending_review(
            &listing_mismatch_db,
            listing_id,
            Some(submission_id),
            &[PendingReviewAspect::avionics(
                "listing-mismatch",
                "avionics_identity",
                evidence,
                evidence,
                "catalog_match_requires_review",
                1,
                "installed",
                Some(evidence.to_string()),
                Some("high".to_string()),
            )],
        )
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_submissions SET canonical_listing_id = NULL WHERE id = ?")
            .bind(submission_id)
            .execute(sqlite_pool(&listing_mismatch_db))
            .await
            .unwrap();
        let review = load_review_row(&listing_mismatch_db, listing_id)
            .await
            .unwrap();
        let error = load_listing_evidence_provenance(&listing_mismatch_db, &review, evidence)
            .await
            .expect_err("a missing exact canonical listing ID must fail provenance");
        assert!(matches!(
            error,
            ReviewError::Validation(message)
                if message.contains("exact owner and canonical listing")
        ));

        let hash_mismatch_db = test_db().await;
        let (owner_user_id, listing_id) = insert_listing(&hash_mismatch_db).await;
        let submission_id = insert_review_bound_submission(
            &hash_mismatch_db,
            owner_user_id,
            listing_id,
            &rendered_html,
        )
        .await;
        stage_pending_review(
            &hash_mismatch_db,
            listing_id,
            Some(submission_id),
            &[PendingReviewAspect::avionics(
                "hash-mismatch",
                "avionics_identity",
                evidence,
                evidence,
                "catalog_match_requires_review",
                1,
                "installed",
                Some(evidence.to_string()),
                Some("high".to_string()),
            )],
        )
        .await
        .unwrap();
        sqlx::query("UPDATE plugin_submissions SET rendered_html_sha256 = ? WHERE id = ?")
            .bind("a".repeat(64))
            .bind(submission_id)
            .execute(sqlite_pool(&hash_mismatch_db))
            .await
            .unwrap();
        let review = load_review_row(&hash_mismatch_db, listing_id)
            .await
            .unwrap();
        let error = load_listing_evidence_provenance(&hash_mismatch_db, &review, evidence)
            .await
            .expect_err("a stored hash that does not match rendered HTML must fail provenance");
        assert!(matches!(
            error,
            ReviewError::Validation(message)
                if message.contains("failed its content hash")
        ));
    }

    #[tokio::test]
    async fn preserved_corroboration_rechecks_source_capture_under_lock() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let evidence = "Garmin GMA 1347 audio panel";
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            listing_id,
            &format!("<html><body>{evidence}</body></html>"),
        )
        .await;
        let product_id = insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', ?, 'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .bind(evidence)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        let assignment = load_existing_assignments(&db, listing_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let product = load_all_approved_product_map(&db)
            .await
            .unwrap()
            .remove(&product_id)
            .unwrap();
        let synthetic = preserved_product_aspect(
            &assignment,
            ListingAssociationRole::Installed,
            &product,
            assignment.source_notes.as_deref(),
        );
        let staged =
            stage_pending_review(&db, listing_id, Some(submission_id), &[synthetic.clone()])
                .await
                .unwrap();
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let observation_sha256 = association_observation_sha256(
            listing_id,
            &assignment,
            ListingAssociationRole::Installed,
            evidence,
        );
        let active_collision_revision = active_collision_closure_revision_sha256(&db, product_id)
            .await
            .unwrap();
        let review = load_review_row(&db, listing_id).await.unwrap();
        let evidence_provenance = load_listing_evidence_provenance(&db, &review, evidence)
            .await
            .unwrap();

        let changed_capture = format!("<html><body>{evidence} in a changed capture</body></html>");
        sqlx::query(
            "UPDATE plugin_submissions SET rendered_html = ?, rendered_html_sha256 = ? WHERE id = ?",
        )
        .bind(&changed_capture)
        .bind(sha256_hex(changed_capture.as_bytes()))
        .bind(submission_id)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();

        let error = corroborate_existing_product_association_and_restage(
            &db,
            user_id,
            listing_id,
            &synthetic.id,
            &staged.review_payload_sha256,
            &staged.catalog_revision_sha256,
            &active_collision_revision,
            product_id,
            &observation_sha256,
            &evidence_provenance,
        )
        .await
        .expect_err("a changed capture must invalidate preserved-link corroboration");
        assert!(matches!(error, ReviewError::Stale(_)));
        assert!(error.to_string().contains("source capture changed"));

        let corroboration_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_authorizations WHERE listing_link_id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(corroboration_count, 0);
        let review = load_review_row(&db, listing_id).await.unwrap();
        assert_eq!(review.review_payload_sha256, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn restage_keeps_product_attestation_explicit_without_listing_corroboration() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let preserved_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let reviewed_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, preserved_id).await;
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'historical GPS evidence',
                      'high', 'installed')
            "#,
        )
        .bind(listing_id)
        .bind(preserved_id)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();
        let original = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("reviewed-unit", reviewed_id)],
        )
        .await
        .unwrap();
        let restaged = restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .unwrap()
            .expect("the reviewed aspect must remain pending");
        assert_ne!(
            restaged.review_payload_sha256,
            original.review_payload_sha256
        );
        assert_eq!(restaged.pending_aspect_count, 2);
    }

    #[tokio::test]
    async fn resolve_preflight_reads_fresh_review_and_catalog_revisions() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("first", product_id)],
        )
        .await
        .unwrap();
        let stale_review = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;
        let stale_request = resolve_request(
            &stale_review,
            vec![ReviewDecision::Discard {
                aspect_id: "first".into(),
                reason: "not installed".to_string(),
            }],
        );

        stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("second", product_id)],
        )
        .await
        .unwrap();
        assert!(matches!(
            preflight_listing_review_resolution(&db, &stale_review, &stale_request).await,
            Err(ReviewError::Stale(message)) if message.contains("review payload is stale")
        ));

        let catalog_stale_review = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;
        let catalog_stale_request = resolve_request(
            &catalog_stale_review,
            vec![ReviewDecision::Discard {
                aspect_id: "second".into(),
                reason: "not installed".to_string(),
            }],
        );
        insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        assert!(matches!(
            preflight_listing_review_resolution(
                &db,
                &catalog_stale_review,
                &catalog_stale_request
            )
            .await,
            Err(ReviewError::Stale(message)) if message.contains("catalog changed")
        ));
    }

    #[tokio::test]
    async fn resolve_preflight_rejects_incomplete_and_unapproved_product_decisions() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let approved_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        let unreviewed_id = insert_unreviewed_product(&db, "GNS 430", "GNS430", "GPS").await;
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[
                pending_aspect("first", approved_id),
                pending_aspect("second", approved_id),
            ],
        )
        .await
        .unwrap();
        let review = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;

        let incomplete = resolve_request(
            &review,
            vec![ReviewDecision::Discard {
                aspect_id: "first".into(),
                reason: "not installed".to_string(),
            }],
        );
        assert!(matches!(
            preflight_listing_review_resolution(&db, &review, &incomplete).await,
            Err(ReviewError::Validation(message)) if message.contains("missing: second")
        ));

        let unapproved = resolve_request(
            &review,
            vec![
                ReviewDecision::UseVerifiedProduct {
                    aspect_id: "first".into(),
                    avionics_model_id: unreviewed_id,
                },
                ReviewDecision::Discard {
                    aspect_id: "second".into(),
                    reason: "not installed".to_string(),
                },
            ],
        );
        assert!(matches!(
            preflight_listing_review_resolution(&db, &review, &unapproved).await,
            Err(ReviewError::Validation(message))
                if message.contains("not an approved verified product")
        ));
    }

    #[tokio::test]
    async fn resolve_preflight_rejects_create_collision_with_approved_catalog() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let approved_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, approved_id).await;
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[batch_create_aspect("duplicate", "Garmin", "GTX 345")],
        )
        .await
        .unwrap();
        let review = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;
        let request = resolve_request(
            &review,
            vec![create_candidate_decision("duplicate", "GTX 345", "GTX345")],
        );

        assert!(matches!(
            preflight_listing_review_resolution(&db, &review, &request).await,
            Err(ReviewError::Conflict(message))
                if message.contains(&format!(
                    "current-policy reusable catalog id {approved_id}"
                ))
        ));
    }

    #[tokio::test]
    async fn resolve_preflight_rejects_create_collision_with_unreviewed_catalog() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let unreviewed_id =
            insert_unreviewed_product(&db, "GTX 345", "GTX345", "Transponder").await;
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[batch_create_aspect("duplicate", "Garmin", "GTX 345")],
        )
        .await
        .unwrap();
        let review = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;
        let request = resolve_request(
            &review,
            vec![create_candidate_decision("duplicate", "GTX 345", "GTX345")],
        );

        assert!(matches!(
            preflight_listing_review_resolution(&db, &review, &request).await,
            Err(ReviewError::Conflict(message))
                if message.contains(&format!("unreviewed catalog id {unreviewed_id}"))
                    && message.contains("curate or consolidate")
        ));
    }

    #[tokio::test]
    async fn explicit_unreviewed_candidate_is_preflighted_and_promoted_in_place() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let candidate_id = insert_unreviewed_product(&db, "KMA 20", "KMA20", "Audio Panel").await;
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[batch_create_aspect("candidate", "Garmin", "KMA 20")],
        )
        .await
        .unwrap();
        let review = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;
        let request = resolve_request(
            &review,
            vec![create_candidate_decision_with_catalog_id(
                "candidate",
                "KMA 20",
                "KMA20",
                candidate_id,
            )],
        );

        preflight_listing_review_resolution(&db, &review, &request)
            .await
            .unwrap();
        resolve_listing_review(&db, user_id, listing_id, &request)
            .await
            .unwrap();

        let pool = sqlite_pool(&db);
        let status: String =
            sqlx::query_scalar("SELECT catalog_status FROM avionics_models WHERE id = ?")
                .bind(candidate_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let matching_products: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_models WHERE avionics_manufacturer_id = (SELECT avionics_manufacturer_id FROM avionics_models WHERE id = ?) AND normalized_name = ?",
        )
        .bind(candidate_id)
        .bind(normalize_avionics_model_name("KMA 20"))
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(status, "approved");
        assert_eq!(matching_products, 1);
    }

    #[tokio::test]
    async fn explicit_unreviewed_candidate_must_match_submitted_identity() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let candidate_id = insert_unreviewed_product(&db, "GNS 430", "GNS430", "GPS").await;
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[batch_create_aspect("candidate", "Garmin", "KMA 20")],
        )
        .await
        .unwrap();
        let review = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;
        let request = resolve_request(
            &review,
            vec![create_candidate_decision_with_catalog_id(
                "candidate",
                "KMA 20",
                "KMA20",
                candidate_id,
            )],
        );

        assert!(matches!(
            preflight_listing_review_resolution(&db, &review, &request).await,
            Err(ReviewError::Conflict(message))
                if message.contains("does not match the submitted")
        ));
    }

    #[tokio::test]
    async fn resolve_preserves_pre_resolved_links_and_adds_reviewed_link() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let existing_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let reviewed_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, existing_id).await;
        attest_approved_product_for_current_policy_reuse(&db, reviewed_id).await;
        let pool = sqlite_pool(&db);
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 2, 'listing_review', 'pre-resolved GPS evidence', 'high', 'installed')
            "#,
        )
        .bind(listing_id)
        .bind(existing_id)
        .execute(pool)
        .await
        .unwrap();

        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("avionics:1:primary", reviewed_id)],
        )
        .await
        .unwrap();
        let request = ResolveReviewRequest {
            expected_review_payload_sha256: staged.review_payload_sha256,
            expected_catalog_revision_sha256: staged.catalog_revision_sha256,
            finalize_listing: false,
            decisions: vec![ReviewDecision::UseVerifiedProduct {
                aspect_id: "avionics:1:primary".into(),
                avionics_model_id: reviewed_id,
            }],
        };
        resolve_listing_review(&db, user_id, listing_id, &request)
            .await
            .unwrap();

        let rows: Vec<(i64, i64, String, Option<String>)> = sqlx::query_as(
            r#"
            SELECT avionics_model_id, quantity, source, source_notes
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            ORDER BY avionics_model_id
            "#,
        )
        .bind(listing_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| {
            row.0 == existing_id
                && row.1 == 2
                && row.2 == "listing_review"
                && row.3.as_deref() == Some("pre-resolved GPS evidence")
        }));
        assert!(rows
            .iter()
            .any(|row| row.0 == reviewed_id && row.2 == "listing_review"));
        let listing_state: (String, bool) = sqlx::query_as(
            "SELECT ingestion_state, is_verified FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(listing_state, ("incomplete".to_string(), false));
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(pending, 0);
    }

    #[tokio::test]
    async fn review_resolution_persists_a_pure_removal() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let mut removal =
            pending_aspect("remove-gtx-345", product_id).with_replacement_product(product_id);
        removal.configuration_action = "removes".to_string();
        let staged = stage_pending_review(&db, listing_id, None, &[removal])
            .await
            .unwrap();

        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::UseVerifiedProduct {
                    aspect_id: "remove-gtx-345".into(),
                    avionics_model_id: product_id,
                }],
            },
        )
        .await
        .unwrap();

        let action: (String, i64, Option<i64>) = sqlx::query_as(
            r#"
            SELECT configuration_action, avionics_model_id, replaces_avionics_model_id
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(
            action,
            ("removes".to_string(), product_id, Some(product_id))
        );
    }

    #[tokio::test]
    async fn immutable_replacement_target_requires_current_reuse_attestation() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let subject_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        let replacement_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        attest_approved_product_for_current_policy_reuse(&db, subject_id).await;
        let mut aspect =
            pending_aspect("replace-gtx-345", subject_id).with_replacement_product(replacement_id);
        aspect.configuration_action = "replaces".to_string();
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();

        let error = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::UseVerifiedProduct {
                    aspect_id: "replace-gtx-345".into(),
                    avionics_model_id: subject_id,
                }],
            },
        )
        .await
        .expect_err("an immutable historical replacement target must fail closed");
        assert!(matches!(
            error,
            ReviewError::Stale(message)
                if message.contains("replacement catalog id")
                    && message.contains("not eligible for current-policy reuse")
        ));
        let pool = sqlite_pool(&db);
        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let review_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(link_count, 0);
        assert_eq!(review_count, 1);
    }

    #[tokio::test]
    async fn manual_review_does_not_reinsert_an_unattested_preserved_link() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let existing_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let reviewed_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, reviewed_id).await;
        let pool = sqlite_pool(&db);
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'historical GPS evidence',
                      'high', 'installed')
            "#,
        )
        .bind(listing_id)
        .bind(existing_id)
        .execute(pool)
        .await
        .unwrap();
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("reviewed-unit", reviewed_id)],
        )
        .await
        .unwrap();

        let error = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::UseVerifiedProduct {
                    aspect_id: "reviewed-unit".into(),
                    avionics_model_id: reviewed_id,
                }],
            },
        )
        .await
        .expect_err("manual review must not reinsert an unattested preserved link");
        assert!(matches!(
            error,
            ReviewError::Stale(message)
                if message.contains("preserved avionics catalog id")
                    && message.contains("lacks current association authorization")
        ));
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(ids, vec![existing_id]);
        let review_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(review_count, 1);
    }

    #[tokio::test]
    async fn loading_review_projects_but_does_not_persist_an_exact_approved_assignment() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let pool = sqlite_pool(&db);
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 2, 'listing', 'retained panel evidence', 'medium', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let aspect = PendingReviewAspect::avionics(
            "audio-panel",
            "avionics",
            "Garmin GMA 1347",
            "Garmin GMA 1347 shown in the listing",
            "listing_link_confidence_not_high",
            2,
            "installed",
            Some("retained panel evidence".to_string()),
            Some("medium".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GMA1347",
            vec!["Audio Panel".to_string()],
        ))
        .with_covered_association(link_id, ListingAssociationRole::Installed, product_id);
        stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        let before_review: (String, String) = sqlx::query_as(
            r#"
            SELECT review_payload_json, review_payload_sha256
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let before_link: (i64, i64, String) = sqlx::query_as(
            r#"
            SELECT avionics_model_id, quantity, configuration_action
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();

        let detail = get_listing_review(&db, user_id, listing_id).await.unwrap();
        assert_eq!(
            detail.review.aspects[0]
                .suggested_product
                .as_ref()
                .and_then(|product| product.id),
            Some(product_id)
        );
        assert!(detail.review.aspects[0]
            .allowed_actions
            .contains(&ReviewAction::UseVerifiedProduct));
        let after_review: (String, String) = sqlx::query_as(
            r#"
            SELECT review_payload_json, review_payload_sha256
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let after_link: (i64, i64, String) = sqlx::query_as(
            r#"
            SELECT avionics_model_id, quantity, configuration_action
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(after_review, before_review);
        assert_eq!(after_link, before_link);

        sqlx::query("UPDATE aircraft_sale_listing_avionics SET quantity = 3 WHERE id = ?")
            .bind(link_id)
            .execute(pool)
            .await
            .unwrap();
        let stale_detail = get_listing_review(&db, user_id, listing_id).await.unwrap();
        assert_eq!(stale_detail.review.aspects[0].suggested_product, None);
    }

    #[tokio::test]
    async fn loading_review_reports_aircraft_blocker_without_repairing_identity() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let selected_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("pending", selected_id)],
        )
        .await
        .unwrap();

        let detail = get_listing_review(&db, user_id, listing_id).await.unwrap();
        assert_eq!(
            detail.review.aircraft_identity.status,
            ReviewAircraftIdentityState::CurationRequired
        );
        assert_eq!(
            detail.review.aircraft_identity.reason_code.as_deref(),
            Some("missing_registration")
        );
        assert!(matches!(
            detail.review.aircraft_identity.repair,
            Some(crate::aircraft::repair::AircraftRepairPreflight::Available {
                reason_code,
                actions,
                ..
            }) if reason_code == "missing_registration"
                && actions == vec![crate::aircraft::repair::AircraftRepairAction::VisualIdentifier]
        ));
        let assignment_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_identity_assignments WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(assignment_count, 0);
    }

    #[tokio::test]
    async fn loading_review_reports_verified_after_exact_aircraft_assignment_and_projection() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let selected_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("pending", selected_id)],
        )
        .await
        .unwrap();

        let n_number = "N123RV";
        let serial_number = "REVIEW-SERIAL";
        sqlx::query(
            r#"
            UPDATE aircraft_sale_listings
            SET registration_number = ?, serial_number = ?
            WHERE id = ?
            "#,
        )
        .bind(n_number)
        .bind(serial_number)
        .bind(listing_id)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();
        insert_faa_aircraft(&db, n_number, serial_number).await;
        let grounding = require_listing_faa_admission(&db, listing_id)
            .await
            .expect("raw FAA admission should succeed");
        crate::aircraft::identity::seed_test_curated_identity_assignment(
            &db, listing_id, &grounding,
        )
        .await
        .expect("test fixture should create an exact assignment and projection");

        let detail = get_listing_review(&db, user_id, listing_id).await.unwrap();
        assert_eq!(
            detail.review.aircraft_identity,
            ReviewAircraftIdentityStatus {
                status: ReviewAircraftIdentityState::Verified,
                reason_code: None,
                faa_n_number: Some(n_number.to_string()),
                faa_snapshot_id: Some(grounding.snapshot.id),
                repair: None,
            }
        );
    }

    #[tokio::test]
    async fn loading_review_exposes_current_catalog_after_another_product_is_approved() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let selected_id = insert_approved_product(&db, "GTX 335", "GTX335", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, selected_id).await;
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("pending", selected_id)],
        )
        .await
        .unwrap();

        let new_product_id = insert_approved_product(&db, "GNC 355", "GNC355", "GPS").await;
        attest_approved_product_for_current_policy_reuse(&db, new_product_id).await;
        let current_revision = approved_catalog_revision_sha256(&db).await.unwrap();
        assert_ne!(staged.catalog_revision_sha256, current_revision);

        let detail = get_listing_review(&db, user_id, listing_id).await.unwrap();
        assert_eq!(detail.review.catalog_revision_sha256, current_revision);
        let stored_revision: String = sqlx::query_scalar(
            "SELECT catalog_revision_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(stored_revision, staged.catalog_revision_sha256);

        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: detail.review.review_payload_sha256,
                expected_catalog_revision_sha256: detail.review.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::UseVerifiedProduct {
                    aspect_id: "pending".into(),
                    avionics_model_id: selected_id,
                }],
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unlinked_candidate_is_promoted_and_other_staged_review_can_reuse_it() {
        let db = test_db().await;
        let (user_id, first_listing_id) = insert_listing(&db).await;
        let second_listing_id = insert_additional_listing(
            &db,
            user_id,
            "https://broker.example/aircraft/candidate-two",
        )
        .await;
        let candidate_id = insert_unreviewed_product(&db, "GNC 355", "GNC355", "GPS").await;
        let first = stage_pending_review(
            &db,
            first_listing_id,
            None,
            &[candidate_aspect("candidate", candidate_id, "GNC 355")],
        )
        .await
        .unwrap();
        stage_pending_review(
            &db,
            second_listing_id,
            None,
            &[candidate_aspect("candidate", candidate_id, "GNC 355")],
        )
        .await
        .unwrap();

        resolve_listing_review(
            &db,
            user_id,
            first_listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: first.review_payload_sha256,
                expected_catalog_revision_sha256: first.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![create_candidate_decision("candidate", "GNC 355", "GNC355")],
            },
        )
        .await
        .unwrap();

        let pool = sqlite_pool(&db);
        let candidate_status: String =
            sqlx::query_scalar("SELECT catalog_status FROM avionics_models WHERE id = ?")
                .bind(candidate_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(candidate_status, "approved");
        let first_linked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?",
        )
        .bind(first_listing_id)
        .bind(candidate_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(first_linked, 1);

        let second = get_listing_review(&db, user_id, second_listing_id)
            .await
            .unwrap();
        assert_eq!(
            second.review.aspects[0].allowed_actions,
            vec![
                ReviewAction::UseVerifiedProduct,
                ReviewAction::CreateVerifiedProduct,
                ReviewAction::Discard,
            ]
        );
        resolve_listing_review(
            &db,
            user_id,
            second_listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: second.review.review_payload_sha256,
                expected_catalog_revision_sha256: second.review.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::UseVerifiedProduct {
                    aspect_id: "candidate".into(),
                    avionics_model_id: candidate_id,
                }],
            },
        )
        .await
        .unwrap();
        let second_linked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?",
        )
        .bind(second_listing_id)
        .bind(candidate_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(second_linked, 1);
    }

    #[tokio::test]
    async fn candidate_and_covered_catalog_ids_must_agree() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let candidate_id = insert_unreviewed_product(&db, "GNC 355", "GNC355", "GPS").await;
        let covered_id = insert_unreviewed_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let pool = sqlite_pool(&db);
        sqlx::query("DROP TRIGGER aircraft_sale_listing_avionics_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        let link_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id) VALUES (?, ?) RETURNING id",
        )
        .bind(listing_id)
        .bind(covered_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let aspect = candidate_aspect("candidate", candidate_id, "GNC 355")
            .with_covered_association(link_id, ListingAssociationRole::Installed, covered_id);
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        let error = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
                expected_catalog_revision_sha256: staged.catalog_revision_sha256.clone(),
                finalize_listing: false,
                decisions: vec![create_candidate_decision("candidate", "GNC 355", "GNC355")],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ReviewError::Conflict(message)
                if message.contains("explicit unreviewed candidate")
                    && message.contains("restage")
        ));
    }

    #[tokio::test]
    async fn unlinked_candidate_promotion_requires_other_listing_coverage() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let other_listing_id = insert_additional_listing(
            &db,
            user_id,
            "https://broker.example/aircraft/candidate-reference",
        )
        .await;
        let candidate_id = insert_unreviewed_product(&db, "GNC 355", "GNC355", "GPS").await;
        let pool = sqlite_pool(&db);
        sqlx::query("DROP TRIGGER aircraft_sale_listing_avionics_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id) VALUES (?, ?)",
        )
        .bind(other_listing_id)
        .bind(candidate_id)
        .execute(pool)
        .await
        .unwrap();
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[candidate_aspect("candidate", candidate_id, "GNC 355")],
        )
        .await
        .unwrap();
        let review = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;
        let request = ResolveReviewRequest {
            expected_review_payload_sha256: staged.review_payload_sha256,
            expected_catalog_revision_sha256: staged.catalog_revision_sha256,
            finalize_listing: false,
            decisions: vec![create_candidate_decision("candidate", "GNC 355", "GNC355")],
        };
        // This is the same provider-free gate that the HTTP handler invokes
        // before `ground_review_product_creations`. A conflict here means no
        // extractor/Gemini dispatch is reachable for this request.
        let error = preflight_listing_review_resolution(&db, &review, &request)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ReviewError::Conflict(message)
                if message.contains("also references it")
                    && message.contains("no complete pending review")
        ));
        let transaction_error = resolve_listing_review(&db, user_id, listing_id, &request)
            .await
            .unwrap_err();
        assert!(matches!(
            transaction_error,
            ReviewError::Conflict(message)
                if message.contains("also references it")
                    && message.contains("no complete pending review")
        ));
        let status: String =
            sqlx::query_scalar("SELECT catalog_status FROM avionics_models WHERE id = ?")
                .bind(candidate_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(status, "unreviewed");
    }

    #[tokio::test]
    async fn exact_identifier_candidate_ignores_unassigned_name_only_legacy_row() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let candidate_id = insert_unreviewed_product(&db, "GNC 355", "GNC355", "GPS").await;
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[candidate_aspect("candidate", candidate_id, "GNC 355")],
        )
        .await
        .unwrap();
        let pool = sqlite_pool(&db);
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Gar min', 'gar-min-legacy')",
        )
        .execute(pool)
        .await
        .unwrap();
        let duplicate_manufacturer_id: i64 = sqlx::query_scalar(
            "SELECT id FROM avionics_manufacturers WHERE normalized_name = 'gar-min-legacy'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_models (avionics_manufacturer_id, name, normalized_name) VALUES (?, 'GNC-355', 'gnc-355-legacy')",
        )
        .bind(duplicate_manufacturer_id)
        .execute(pool)
        .await
        .unwrap();
        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
                expected_catalog_revision_sha256: staged.catalog_revision_sha256.clone(),
                finalize_listing: false,
                decisions: vec![create_candidate_decision("candidate", "GNC 355", "GNC355")],
            },
        )
        .await
        .unwrap();
        let candidate_status: String =
            sqlx::query_scalar("SELECT catalog_status FROM avionics_models WHERE id = ?")
                .bind(candidate_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let name_only_status: String = sqlx::query_scalar(
            "SELECT catalog_status FROM avionics_models WHERE avionics_manufacturer_id = ?",
        )
        .bind(duplicate_manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(candidate_status, "approved");
        assert_eq!(name_only_status, "unreviewed");
    }

    #[tokio::test]
    async fn explicit_candidate_that_is_no_longer_unreviewed_is_stale() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let candidate_id = insert_unreviewed_product(&db, "GNC 355", "GNC355", "GPS").await;
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[candidate_aspect("candidate", candidate_id, "GNC 355")],
        )
        .await
        .unwrap();
        let pool = sqlite_pool(&db);
        sqlx::query("UPDATE avionics_models SET catalog_status = 'rejected' WHERE id = ?")
            .bind(candidate_id)
            .execute(pool)
            .await
            .unwrap();
        let rejected = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![create_candidate_decision("candidate", "GNC 355", "GNC355")],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            rejected,
            ReviewError::Stale(message)
                if message.contains("candidate") && message.contains("rejected")
        ));
    }

    #[tokio::test]
    async fn corrected_create_identity_does_not_rewrite_staged_candidate() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let other_listing_id = insert_additional_listing(
            &db,
            user_id,
            "https://broker.example/aircraft/corrected-candidate-reference",
        )
        .await;
        let candidate_id = insert_unreviewed_product(&db, "GNC 355", "GNC355", "GPS").await;
        let pool = sqlite_pool(&db);
        sqlx::query("DROP TRIGGER aircraft_sale_listing_avionics_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        let covered_link_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id) VALUES (?, ?) RETURNING id",
        )
        .bind(listing_id)
        .bind(candidate_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id) VALUES (?, ?)",
        )
        .bind(other_listing_id)
        .bind(candidate_id)
        .execute(pool)
        .await
        .unwrap();
        let aspect = candidate_aspect("candidate", candidate_id, "GNC 355")
            .with_covered_association(
                covered_link_id,
                ListingAssociationRole::Installed,
                candidate_id,
            );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![create_candidate_decision("candidate", "GPS 175", "GPS175")],
            },
        )
        .await
        .unwrap();

        let candidate: (String, String) =
            sqlx::query_as("SELECT name, catalog_status FROM avionics_models WHERE id = ?")
                .bind(candidate_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(candidate, ("GNC 355".to_string(), "unreviewed".to_string()));
        let linked: (i64, String) = sqlx::query_as(
            r#"
            SELECT model.id, model.name
            FROM aircraft_sale_listing_avionics link
            JOIN avionics_models model ON model.id = link.avionics_model_id
            WHERE link.aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_ne!(linked.0, candidate_id);
        assert_eq!(linked.1, "GPS 175");
        let unrelated_link: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?",
        )
        .bind(other_listing_id)
        .bind(candidate_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(unrelated_link, 1);
    }

    #[tokio::test]
    async fn name_only_legacy_row_is_retained_while_stable_product_is_created() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let legacy_id =
            insert_unreviewed_product(&db, "GTX 345", "LEGACY-UNKNOWN", "Transponder").await;
        let pool = sqlite_pool(&db);
        sqlx::query(
            r#"
            UPDATE avionics_models
            SET manufacturer_identifier_kind = NULL,
                manufacturer_identifier = NULL,
                normalized_manufacturer_identifier = NULL
            WHERE id = ?
            "#,
        )
        .bind(legacy_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("DROP TRIGGER aircraft_sale_listing_avionics_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        let legacy_link_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id) VALUES (?, ?) RETURNING id",
        )
        .bind(listing_id)
        .bind(legacy_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let aspect = PendingReviewAspect::avionics(
            "name-only",
            "avionics_identity",
            "Garmin GTX 345",
            "Garmin GTX 345 transponder shown in the listing",
            "legacy_product_has_no_stable_identifier",
            1,
            "installed",
            Some("Listing identifies a Garmin GTX 345 transponder.".to_string()),
            Some("medium".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GTX 345",
            vec!["Transponder".to_string()],
        ))
        .with_covered_association(
            legacy_link_id,
            ListingAssociationRole::Installed,
            legacy_id,
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::CreateVerifiedProduct {
                    aspect_id: "name-only".into(),
                    unreviewed_avionics_model_id: None,
                    manufacturer: "Garmin".to_string(),
                    model: "GTX 345".to_string(),
                    capabilities: vec!["Transponder".to_string()],
                    manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
                    manufacturer_identifier: "GTX345".to_string(),
                    identity_source_url: "https://www.garmin.com/en-US/p/140949/".to_string(),
                    identity_source_title: "Garmin GTX 345 product page".to_string(),
                    identity_evidence_text:
                        "Garmin identifies GTX 345 as the exact manufacturer model number."
                            .to_string(),
                    grounded_claim_source_urls: Vec::new(),
                }],
            },
        )
        .await
        .unwrap();

        let legacy_status: String =
            sqlx::query_scalar("SELECT catalog_status FROM avionics_models WHERE id = ?")
                .bind(legacy_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let linked: (i64, String, String) = sqlx::query_as(
            r#"
            SELECT model.id, model.catalog_status,
                   model.normalized_manufacturer_identifier
            FROM aircraft_sale_listing_avionics link
            JOIN avionics_models model ON model.id = link.avionics_model_id
            WHERE link.aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(legacy_status, "unreviewed");
        assert_ne!(linked.0, legacy_id);
        assert_eq!(linked.1, "approved");
        assert_eq!(linked.2, "gtx345");
    }

    #[tokio::test]
    async fn new_product_review_admits_evidence_backed_manufacturer_identity() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let pool = sqlite_pool(&db);
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Avidyne', 'avidyne')",
        )
        .execute(pool)
        .await
        .unwrap();
        let manufacturer_id: i64 = sqlx::query_scalar(
            "SELECT id FROM avionics_manufacturers WHERE normalized_name = 'avidyne'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let manufacturer_identity = ensure_manufacturer_identity(
            &db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://www.avidyne.com/ifd550/".to_string(),
                source_title: "Avidyne IFD550 product page".to_string(),
                evidence_text: "Avidyne identifies itself as the IFD550 manufacturer.".to_string(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origins (
              authority_kind, avionics_manufacturer_identity_id, https_origin,
              evidence_source_url, evidence_source_title, evidence_text,
              approval_basis, approval_reason
            ) VALUES (
              'manufacturer_primary', ?, 'https://www.avidyne.com',
              'https://www.avidyne.com/ifd550/',
              'Avidyne IFD550 product page',
              'The first-party Avidyne page identifies the exact IFD550 product.',
              'curated_bootstrap',
              'Test fixture for exact Avidyne manufacturer authority'
            )
            "#,
        )
        .bind(manufacturer_identity.avionics_manufacturer_identity_id)
        .execute(pool)
        .await
        .unwrap();
        let aspect = PendingReviewAspect::avionics(
            "new-maker",
            "avionics_identity",
            "Avidyne IFD550",
            "Avidyne IFD550 shown in the listing",
            "new_verified_product",
            1,
            "installed",
            Some("Listing identifies an Avidyne IFD550 navigator.".to_string()),
            Some("high".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Avidyne",
            "IFD550",
            vec!["GPS".to_string()],
        ));
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::CreateVerifiedProduct {
                    aspect_id: "new-maker".into(),
                    unreviewed_avionics_model_id: None,
                    manufacturer: "Avidyne".to_string(),
                    model: "IFD550".to_string(),
                    capabilities: vec!["GPS".to_string()],
                    manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
                    manufacturer_identifier: "IFD550".to_string(),
                    identity_source_url: "https://www.avidyne.com/ifd550/".to_string(),
                    identity_source_title: "Avidyne IFD550 product page".to_string(),
                    identity_evidence_text:
                        "Avidyne identifies IFD550 as its exact navigator model.".to_string(),
                    grounded_claim_source_urls: Vec::new(),
                }],
            },
        )
        .await
        .unwrap();
        let admitted: (String, String, i64) = sqlx::query_as(
            r#"
            SELECT identity.canonical_name, membership.membership_basis,
                   product.avionics_model_id
            FROM avionics_manufacturers manufacturer
            JOIN avionics_manufacturer_identity_memberships membership
              ON membership.avionics_manufacturer_id = manufacturer.id
            JOIN avionics_manufacturer_identities identity
              ON identity.id = membership.avionics_manufacturer_identity_id
            JOIN avionics_models model
              ON model.avionics_manufacturer_id = manufacturer.id
             AND model.catalog_status = 'approved'
            JOIN avionics_approved_product_identities product
              ON product.avionics_model_id = model.id
            WHERE manufacturer.normalized_name = 'avidyne'
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(admitted.0, "Avidyne");
        assert_eq!(admitted.1, "authoritative_primary");
        let linked_model_id: i64 = sqlx::query_scalar(
            "SELECT avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(linked_model_id, admitted.2);
    }

    #[tokio::test]
    async fn new_product_review_rejects_an_uncurated_exact_source_origin_atomically() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let aspect = batch_create_aspect("uncurated-origin", "Example Avionics", "NAV 100");
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        let pool = sqlite_pool(&db);
        let model_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models")
            .fetch_one(pool)
            .await
            .unwrap();

        let error = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::CreateVerifiedProduct {
                    aspect_id: "uncurated-origin".into(),
                    unreviewed_avionics_model_id: None,
                    manufacturer: "Example Avionics".to_string(),
                    model: "NAV 100".to_string(),
                    capabilities: vec!["GPS".to_string()],
                    manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
                    manufacturer_identifier: "NAV100".to_string(),
                    identity_source_url: "https://products.example-avionics.test/nav100"
                        .to_string(),
                    identity_source_title: "Example Avionics NAV 100 product page".to_string(),
                    identity_evidence_text:
                        "Example Avionics identifies NAV 100 as its exact navigator model."
                            .to_string(),
                    grounded_claim_source_urls: Vec::new(),
                }],
            },
        )
        .await
        .expect_err("new product approval requires a curated exact source origin");
        assert!(matches!(
            error,
            ReviewError::Conflict(message)
                if message.contains("could not be bound to a current active exact manufacturer source origin")
        ));
        let model_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models")
            .fetch_one(pool)
            .await
            .unwrap();
        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let review_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(model_count_after, model_count_before);
        assert_eq!(link_count, 0);
        assert_eq!(review_count, 1);
    }

    #[tokio::test]
    async fn existing_identity_cross_collision_persists_alias_without_resolving_listing() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let approved_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, approved_id).await;
        let pool = sqlite_pool(&db);
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Garmin International', 'garmininternational')",
        )
        .execute(pool)
        .await
        .unwrap();
        let alternate_manufacturer_id: i64 = sqlx::query_scalar(
            "SELECT id FROM avionics_manufacturers WHERE normalized_name = 'garmininternational'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        ensure_manufacturer_identity(
            &db,
            alternate_manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://example.org/manufacturers/garmin-international".to_string(),
                source_title: "Garmin International manufacturer record".to_string(),
                evidence_text:
                    "The authoritative record names Garmin International as this manufacturer."
                        .to_string(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (
              ?, ?, 'listing_review',
              'Previously corroborated exact Garmin GTX 345 association',
              'high', 'installed'
            )
            "#,
        )
        .bind(listing_id)
        .bind(approved_id)
        .execute(pool)
        .await
        .unwrap();
        let aspect = PendingReviewAspect::avionics(
            "alias-collision",
            "avionics_identity",
            "Garmin International GTX 345",
            "Garmin International GTX 345 shown in the listing",
            "new_verified_product",
            1,
            "installed",
            Some("Listing uses the Garmin International spelling.".to_string()),
            Some("medium".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin International",
            "GTX 345",
            vec!["Transponder".to_string()],
        ));
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        let model_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models")
            .fetch_one(pool)
            .await
            .unwrap();
        let error = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::CreateVerifiedProduct {
                    aspect_id: "alias-collision".into(),
                    unreviewed_avionics_model_id: None,
                    manufacturer: "Garmin International".to_string(),
                    model: "GTX 345".to_string(),
                    capabilities: vec!["Transponder".to_string()],
                    manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
                    manufacturer_identifier: "GTX345".to_string(),
                    identity_source_url: "https://www.garmin.com/en-US/p/140949/".to_string(),
                    identity_source_title: "Garmin GTX 345 product page".to_string(),
                    identity_evidence_text:
                        "Garmin identifies GTX 345 as the exact manufacturer model number."
                            .to_string(),
                    grounded_claim_source_urls: Vec::new(),
                }],
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                &error,
                ReviewError::Conflict(message) if message.contains("human alias review")
            ),
            "unexpected resolution error: {error:?}"
        );

        let pending_alias: (String, String, i64) = sqlx::query_as(
            r#"
            SELECT candidate_basis, review_status, matched_avionics_model_id
            FROM avionics_manufacturer_alias_candidates
            WHERE avionics_manufacturer_id = ?
            "#,
        )
        .bind(alternate_manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(pending_alias.0, "exact_stable_identifier");
        assert_eq!(pending_alias.1, "pending");
        assert_eq!(pending_alias.2, approved_id);
        let model_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models")
            .fetch_one(pool)
            .await
            .unwrap();
        let retained_review: String = sqlx::query_scalar(
            "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let retained_link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?",
        )
        .bind(listing_id)
        .bind(approved_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(model_count_after, model_count_before);
        assert_eq!(retained_review, staged.review_payload_sha256);
        assert_eq!(retained_link_count, 1);
    }

    #[tokio::test]
    async fn same_batch_cross_identity_stable_id_collision_stages_alias_atomically() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let (alpha_manufacturer_id, alpha_identity_id) =
            insert_evidence_backed_manufacturer(&db, "Alpha Avionics", "alphaavionics").await;
        let (beta_manufacturer_id, beta_identity_id) =
            insert_evidence_backed_manufacturer(&db, "Beta Avionics", "betaavionics").await;
        let aspects = vec![
            batch_create_aspect("alpha-unit", "Alpha Avionics", "Shared 100"),
            batch_create_aspect("beta-unit", "Beta Avionics", "Shared 100"),
        ];
        let staged = stage_pending_review(&db, listing_id, None, &aspects)
            .await
            .unwrap();
        let error = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![
                    batch_create_decision(
                        "alpha-unit",
                        "Alpha Avionics",
                        "Shared 100",
                        "SHARED-100",
                    ),
                    batch_create_decision("beta-unit", "Beta Avionics", "Shared 100", "SHARED-100"),
                ],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ReviewError::Conflict(message)
                if message.contains("cross-manufacturer product collisions")
                    && message.contains("exact_stable_identifier")
        ));

        let (expected_source_manufacturer_id, expected_target_identity_id) =
            if alpha_identity_id > beta_identity_id {
                (alpha_manufacturer_id, beta_identity_id)
            } else {
                (beta_manufacturer_id, alpha_identity_id)
            };
        let pool = sqlite_pool(&db);
        let candidate: (i64, i64, String, Option<i64>, String) = sqlx::query_as(
            r#"
            SELECT avionics_manufacturer_id,
                   candidate_manufacturer_identity_id,
                   candidate_basis, matched_avionics_model_id, review_status
            FROM avionics_manufacturer_alias_candidates
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(candidate.0, expected_source_manufacturer_id);
        assert_eq!(candidate.1, expected_target_identity_id);
        assert_eq!(candidate.2, "exact_stable_identifier");
        assert_eq!(candidate.3, None);
        assert_eq!(candidate.4, "pending");
        let product_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models")
            .fetch_one(pool)
            .await
            .unwrap();
        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let retained_review: String = sqlx::query_scalar(
            "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(product_count, 0);
        assert_eq!(link_count, 0);
        assert_eq!(retained_review, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn same_batch_cross_identity_product_name_collision_stages_alias() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        insert_evidence_backed_manufacturer(&db, "Alpha Avionics", "alphaavionics").await;
        insert_evidence_backed_manufacturer(&db, "Beta Avionics", "betaavionics").await;
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[
                batch_create_aspect("alpha-unit", "Alpha Avionics", "Common Navigator"),
                batch_create_aspect("beta-unit", "Beta Avionics", "Common Navigator"),
            ],
        )
        .await
        .unwrap();
        let error = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![
                    batch_create_decision(
                        "alpha-unit",
                        "Alpha Avionics",
                        "Common Navigator",
                        "ALPHA-100",
                    ),
                    batch_create_decision(
                        "beta-unit",
                        "Beta Avionics",
                        "Common Navigator",
                        "BETA-200",
                    ),
                ],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ReviewError::Conflict(message)
                if message.contains("cross-manufacturer product collisions")
                    && message.contains("exact_product_name")
        ));
        let pool = sqlite_pool(&db);
        let basis: String = sqlx::query_scalar(
            "SELECT candidate_basis FROM avionics_manufacturer_alias_candidates",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(basis, "exact_product_name");
        let product_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(product_count, 0);
    }

    #[tokio::test]
    async fn same_batch_same_identity_duplicate_fails_without_alias_or_product() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        insert_evidence_backed_manufacturer(&db, "Alpha Avionics", "alphaavionics").await;
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[
                batch_create_aspect("first-unit", "Alpha Avionics", "Navigator One"),
                batch_create_aspect("second-unit", "Alpha Avionics", "Navigator Two"),
            ],
        )
        .await
        .unwrap();
        let error = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256.clone(),
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![
                    batch_create_decision(
                        "first-unit",
                        "Alpha Avionics",
                        "Navigator One",
                        "DUPLICATE-100",
                    ),
                    batch_create_decision(
                        "second-unit",
                        "Alpha Avionics",
                        "Navigator Two",
                        "DUPLICATE-100",
                    ),
                ],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ReviewError::Conflict(message)
                if message.contains("same evidence-backed manufacturer identity")
                    && message.contains("first-unit / second-unit")
                    && message.contains("exact_stable_identifier")
        ));
        let pool = sqlite_pool(&db);
        let alias_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_manufacturer_alias_candidates")
                .fetch_one(pool)
                .await
                .unwrap();
        let product_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models")
            .fetch_one(pool)
            .await
            .unwrap();
        let retained_review: String = sqlx::query_scalar(
            "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(alias_count, 0);
        assert_eq!(product_count, 0);
        assert_eq!(retained_review, staged.review_payload_sha256);
    }

    #[tokio::test]
    async fn invalid_decision_set_rolls_back_review_and_existing_links() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let existing_id = insert_approved_product(&db, "GNS 530W", "GNS530W", "GPS").await;
        let reviewed_id = insert_approved_product(&db, "GTX 335", "GTX335", "Transponder").await;
        let pool = sqlite_pool(&db);
        sqlx::query(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id) VALUES (?, ?)",
        )
        .bind(listing_id)
        .bind(existing_id)
        .execute(pool)
        .await
        .unwrap();
        let staged = stage_pending_review(
            &db,
            listing_id,
            None,
            &[pending_aspect("pending", reviewed_id)],
        )
        .await
        .unwrap();
        let error = resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: Vec::new(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ReviewError::Validation(_)));
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let links: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?",
        )
        .bind(listing_id)
        .bind(existing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(pending, 1);
        assert_eq!(links, 1);
    }

    #[tokio::test]
    async fn create_rejects_in_place_promotion_with_global_suite_references() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let legacy_id = insert_unreviewed_product(&db, "GTX-345", "GTX345", "Transponder").await;
        let suite_peer_id = insert_approved_product(&db, "GTN 750Xi", "GTN750XI", "GPS").await;
        let pool = sqlite_pool(&db);
        for trigger in [
            "aircraft_sale_listing_avionics_approved_insert",
            "avionics_suite_components_approved_insert",
        ] {
            sqlx::query(&format!("DROP TRIGGER {trigger}"))
                .execute(pool)
                .await
                .unwrap();
        }
        let listing_link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 'legacy_import', 'unreviewed imported identity', 'low', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(legacy_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_suite_components (suite_model_id, component_model_id) VALUES (?, ?), (?, ?)",
        )
        .bind(legacy_id)
        .bind(suite_peer_id)
        .bind(suite_peer_id)
        .bind(legacy_id)
        .execute(pool)
        .await
        .unwrap();

        // Reproduce legacy/migrated data that predates the status guard. The
        // review path must defend this state even though the current schema
        // prevents creating it through ordinary writes.
        let aspect = pending_aspect("legacy-identity", legacy_id)
            .with_proposed_product(ReviewProduct::proposed(
                "Garmin",
                "GTX 345",
                vec!["Transponder".to_string()],
            ))
            .with_covered_association(
                listing_link_id,
                ListingAssociationRole::Installed,
                legacy_id,
            );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        let review = get_listing_review(&db, user_id, listing_id)
            .await
            .unwrap()
            .review;
        let request = ResolveReviewRequest {
            expected_review_payload_sha256: staged.review_payload_sha256,
            expected_catalog_revision_sha256: staged.catalog_revision_sha256,
            finalize_listing: false,
            decisions: vec![ReviewDecision::CreateVerifiedProduct {
                aspect_id: "legacy-identity".into(),
                unreviewed_avionics_model_id: Some(legacy_id),
                manufacturer: "Garmin".to_string(),
                model: "GTX 345".to_string(),
                capabilities: vec!["Transponder".to_string()],
                manufacturer_identifier_kind: "manufacturer_model_number".to_string(),
                manufacturer_identifier: "GTX345".to_string(),
                identity_source_url: "https://www.garmin.com/en-US/p/140949/".to_string(),
                identity_source_title: "Garmin GTX 345 product page".to_string(),
                identity_evidence_text: "Garmin identifies GTX 345 as the model number."
                    .to_string(),
                grounded_claim_source_urls: Vec::new(),
            }],
        };
        // Regression for the paid-call ordering bug: this exact immutable
        // catalog/reference conflict must fail in the HTTP preflight, before
        // the server can dispatch its extractor.
        let mut provider_dispatches = 0;
        let error = preflight_listing_review_resolution(&db, &review, &request)
            .await
            .and_then(|_| {
                provider_dispatches += 1;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(
            provider_dispatches, 0,
            "a deterministic promotion conflict must not reach Gemini"
        );
        let ReviewError::Conflict(message) = error else {
            panic!("expected a catalog-reference conflict, got {error}");
        };
        assert!(message.contains("global avionics catalog/reference data"));
        assert!(message.contains("avionics_suite_components.suite_model_id (1)"));
        assert!(message.contains("avionics_suite_components.component_model_id (1)"));
        let transaction_error = resolve_listing_review(&db, user_id, listing_id, &request)
            .await
            .unwrap_err();
        assert!(matches!(
            transaction_error,
            ReviewError::Conflict(message)
                if message.contains("global avionics catalog/reference data")
        ));

        let legacy_state: (String, String) =
            sqlx::query_as("SELECT name, catalog_status FROM avionics_models WHERE id = ?")
                .bind(legacy_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(
            legacy_state,
            ("GTX-345".to_string(), "unreviewed".to_string())
        );
        let retained_global_references: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_suite_components WHERE suite_model_id = ? OR component_model_id = ?",
        )
        .bind(legacy_id)
        .bind(legacy_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained_global_references, 2);
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(pending, 1);
    }

    #[tokio::test]
    async fn covered_approved_association_can_be_discarded() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GNS 430", "GNS430", "GPS").await;
        let pool = sqlite_pool(&db);
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 'legacy_import', 'weak legacy observation', 'low', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(pool)
        .await
        .unwrap();

        let aspect = pending_aspect("weak-association", product_id).with_covered_association(
            link_id,
            ListingAssociationRole::Installed,
            product_id,
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::Discard {
                    aspect_id: "weak-association".into(),
                    reason: "not actually installed".to_string(),
                }],
            },
        )
        .await
        .unwrap();

        let links: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(links, 0);
    }

    #[tokio::test]
    async fn discarded_legacy_candidate_is_retained_for_global_safe_cleanup() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let candidate_id = insert_unreviewed_product(&db, "GNC 355", "GNC355-LEGACY", "GPS").await;
        let pool = sqlite_pool(&db);
        sqlx::query("DROP TRIGGER aircraft_sale_listing_avionics_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        let link_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id, source, source_confidence) VALUES (?, ?, 'legacy_import', 'low') RETURNING id",
        )
        .bind(listing_id)
        .bind(candidate_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let aspect = candidate_aspect("garbage", candidate_id, "GNC 355").with_covered_association(
            link_id,
            ListingAssociationRole::Installed,
            candidate_id,
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::Discard {
                    aspect_id: "garbage".into(),
                    reason: "listing text is not a concrete installed product".to_string(),
                }],
            },
        )
        .await
        .unwrap();

        let candidate_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM avionics_models WHERE id = ?")
                .bind(candidate_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(candidate_count, 1);
    }

    #[tokio::test]
    async fn reviewer_corroboration_makes_covered_association_high_confidence() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await;
        attest_approved_product_for_current_policy_reuse(&db, product_id).await;
        let pool = sqlite_pool(&db);
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 'legacy_import', 'weak legacy observation', 'low', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(pool)
        .await
        .unwrap();

        let aspect = pending_aspect("confirmed-association", product_id).with_covered_association(
            link_id,
            ListingAssociationRole::Installed,
            product_id,
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: staged.review_payload_sha256,
                expected_catalog_revision_sha256: staged.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::UseVerifiedProduct {
                    aspect_id: "confirmed-association".into(),
                    avionics_model_id: product_id,
                }],
            },
        )
        .await
        .unwrap();

        let association: (String, Option<String>) = sqlx::query_as(
            r#"
            SELECT source, source_confidence
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?
            "#,
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            association,
            ("listing_review".to_string(), Some("high".to_string()))
        );
    }

    fn observation_revision(
        staged: &StagedPendingReview,
        aspect_id: &str,
        manufacturer: &str,
        model: &str,
        capability: &str,
        quantity: i64,
    ) -> ReviseAvionicsObservationRequest {
        ReviseAvionicsObservationRequest {
            expected_review_payload_sha256: staged.review_payload_sha256.clone(),
            expected_catalog_revision_sha256: staged.catalog_revision_sha256.clone(),
            aspect_id: aspect_id.into(),
            manufacturer: manufacturer.to_string(),
            model: model.to_string(),
            capabilities: vec![capability.to_string()],
            quantity,
            configuration_action: "installed".to_string(),
            replacement_target: None,
        }
    }

    #[tokio::test]
    async fn ordinary_observation_correction_rehashes_complete_replacement_semantics() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let target_id = insert_approved_product(&db, "KX 170B", "KX170B", "NAV").await;
        attest_approved_product_for_current_policy_reuse(&db, target_id).await;
        let aspect = PendingReviewAspect::avionics(
            "ordinary",
            "avionics_identity",
            "King radio",
            "King radio listed in the panel",
            "raw_observation_identity_unusable",
            1,
            "installed",
            Some("King radio listed in the panel".to_string()),
            Some("medium".to_string()),
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        let mut request =
            observation_revision(&staged, "ordinary", "Garmin", "GI 275", "Flight Display", 2);
        request.configuration_action = "replaces".to_string();
        request.replacement_target = Some(ReviewReplacementTarget::CatalogProduct {
            avionics_model_id: target_id,
        });
        let revised = revise_avionics_observation_and_restage(&db, user_id, listing_id, &request)
            .await
            .unwrap();
        assert_ne!(revised.review_payload_sha256, staged.review_payload_sha256);

        let detail = get_listing_review(&db, user_id, listing_id).await.unwrap();
        let corrected = &detail.review.aspects[0];
        assert!(corrected.reviewer_corrected);
        assert!(corrected.configuration_action_editable);
        assert_eq!(corrected.quantity, 2);
        assert_eq!(corrected.configuration_action, "replaces");
        assert_eq!(
            corrected.replacement_product.as_ref().unwrap().id,
            Some(target_id)
        );
        assert_eq!(corrected.proposed_product.as_ref().unwrap().model, "GI 275");
        assert_eq!(
            corrected.proposed_product.as_ref().unwrap().capabilities,
            vec!["Flight Display".to_string()]
        );
        assert_eq!(
            corrected.source_evidence_text.as_deref(),
            Some("King radio listed in the panel")
        );

        let stale = revise_avionics_observation_and_restage(&db, user_id, listing_id, &request)
            .await
            .expect_err("the previous payload hash cannot be replayed");
        assert!(matches!(stale, ReviewError::Stale(_)));
    }

    #[tokio::test]
    async fn covered_observation_correction_updates_the_link_only_during_normal_resolve() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let original_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let corrected_id = insert_approved_product(&db, "GI 275", "GI275", "Flight Display").await;
        attest_approved_product_for_current_policy_reuse(&db, original_id).await;
        attest_approved_product_for_current_policy_reuse(&db, corrected_id).await;
        let pool = sqlite_pool(&db);
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'legacy_import', 'Garmin GI 275 shown in panel',
                      'low', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(original_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let aspect = PendingReviewAspect::avionics(
            "covered",
            "avionics_identity",
            "Garmin GNS 430W",
            "Garmin GI 275 shown in panel",
            "catalog_product_unverified",
            1,
            "installed",
            Some("Garmin GI 275 shown in panel".to_string()),
            Some("low".to_string()),
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GNS 430W",
            vec!["GPS".to_string()],
        ))
        .with_covered_association(link_id, ListingAssociationRole::Installed, original_id);
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        let request =
            observation_revision(&staged, "covered", "Garmin", "GI 275", "Flight Display", 3);
        let revised = revise_avionics_observation_and_restage(&db, user_id, listing_id, &request)
            .await
            .unwrap();
        let unchanged: (i64, i64, String) = sqlx::query_as(
            "SELECT avionics_model_id, quantity, source FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(unchanged, (original_id, 1, "legacy_import".to_string()));

        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: revised.review_payload_sha256,
                expected_catalog_revision_sha256: revised.catalog_revision_sha256,
                finalize_listing: false,
                decisions: vec![ReviewDecision::UseVerifiedProduct {
                    aspect_id: "covered".into(),
                    avionics_model_id: corrected_id,
                }],
            },
        )
        .await
        .unwrap();
        let corrected: (i64, i64, String, Option<String>) = sqlx::query_as(
            r#"
            SELECT avionics_model_id, quantity, source, source_confidence
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            corrected,
            (
                corrected_id,
                3,
                "listing_review".to_string(),
                Some("high".to_string())
            )
        );
    }

    #[tokio::test]
    async fn covered_correction_rejects_quantity_and_action_mutations_before_resolve() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let original_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let corrected_id = insert_approved_product(&db, "GI 275", "GI275", "Flight Display").await;
        let replacement_id = insert_approved_product(&db, "KX 170B", "KX170B", "NAV").await;
        attest_approved_product_for_current_policy_reuse(&db, original_id).await;
        attest_approved_product_for_current_policy_reuse(&db, corrected_id).await;
        attest_approved_product_for_current_policy_reuse(&db, replacement_id).await;
        let pool = sqlite_pool(&db);
        let link_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id, quantity, configuration_action) VALUES (?, ?, 1, 'installed') RETURNING id",
        )
        .bind(listing_id)
        .bind(original_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let aspect = pending_aspect("covered-concurrency", original_id).with_covered_association(
            link_id,
            ListingAssociationRole::Installed,
            original_id,
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();
        let corrected = revise_avionics_observation_and_restage(
            &db,
            user_id,
            listing_id,
            &observation_revision(
                &staged,
                "covered-concurrency",
                "Garmin",
                "GI 275",
                "Flight Display",
                2,
            ),
        )
        .await
        .unwrap();
        let resolve_request = ResolveReviewRequest {
            expected_review_payload_sha256: corrected.review_payload_sha256,
            expected_catalog_revision_sha256: corrected.catalog_revision_sha256,
            finalize_listing: false,
            decisions: vec![ReviewDecision::UseVerifiedProduct {
                aspect_id: "covered-concurrency".into(),
                avionics_model_id: corrected_id,
            }],
        };

        sqlx::query("UPDATE aircraft_sale_listing_avionics SET quantity = 4 WHERE id = ?")
            .bind(link_id)
            .execute(pool)
            .await
            .unwrap();
        assert!(matches!(
            restage_pending_review_if_current(
                &db,
                user_id,
                listing_id,
                &resolve_request.expected_review_payload_sha256,
            )
            .await,
            Err(ReviewError::Stale(_))
        ));
        let quantity_error = resolve_listing_review(&db, user_id, listing_id, &resolve_request)
            .await
            .expect_err("a concurrent quantity mutation must be rejected");
        assert!(
            matches!(quantity_error, ReviewError::Stale(_)),
            "{quantity_error:?}"
        );

        sqlx::query(
            "UPDATE aircraft_sale_listing_avionics SET quantity = 1, configuration_action = 'replaces', replaces_avionics_model_id = ? WHERE id = ?",
        )
        .bind(replacement_id)
        .bind(link_id)
        .execute(pool)
        .await
        .unwrap();
        let action_error = resolve_listing_review(&db, user_id, listing_id, &resolve_request)
            .await
            .expect_err("a concurrent action mutation must be rejected");
        assert!(
            matches!(action_error, ReviewError::Stale(_)),
            "{action_error:?}"
        );
    }

    #[tokio::test]
    async fn covered_corrections_reject_stale_catalog_link_and_action_changes() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "GPS").await;
        let replacement_id = insert_approved_product(&db, "KX 170B", "KX170B", "NAV").await;
        let pool = sqlite_pool(&db);
        let link_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id, configuration_action) VALUES (?, ?, 'installed') RETURNING id",
        )
        .bind(listing_id)
        .bind(product_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let aspect = pending_aspect("covered-lock", product_id).with_covered_association(
            link_id,
            ListingAssociationRole::Installed,
            product_id,
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .unwrap();

        let mut stale_catalog =
            observation_revision(&staged, "covered-lock", "Garmin", "GNS 430W", "GPS", 2);
        stale_catalog.expected_catalog_revision_sha256 = "f".repeat(64);
        assert!(matches!(
            revise_avionics_observation_and_restage(&db, user_id, listing_id, &stale_catalog).await,
            Err(ReviewError::Stale(_))
        ));

        let mut changed_action =
            observation_revision(&staged, "covered-lock", "Garmin", "GNS 430W", "GPS", 2);
        changed_action.configuration_action = "replaces".to_string();
        changed_action.replacement_target = Some(ReviewReplacementTarget::CatalogProduct {
            avionics_model_id: replacement_id,
        });
        assert!(matches!(
            revise_avionics_observation_and_restage(
                &db,
                user_id,
                listing_id,
                &changed_action
            )
            .await,
            Err(ReviewError::Validation(message)) if message.contains("keep the staged action")
        ));

        sqlx::query("DELETE FROM aircraft_sale_listing_avionics WHERE id = ?")
            .bind(link_id)
            .execute(pool)
            .await
            .unwrap();
        let current = observation_revision(&staged, "covered-lock", "Garmin", "GNS 430W", "GPS", 2);
        assert!(matches!(
            revise_avionics_observation_and_restage(&db, user_id, listing_id, &current).await,
            Err(ReviewError::Stale(message)) if message.contains("no longer exists")
        ));
    }

    #[tokio::test]
    async fn restage_replaces_stale_covered_cards_with_current_link_cards() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let old_product_id =
            insert_approved_product(&db, "GMA 1347", "GMA1347", "Audio Panel").await;
        let current_product_id =
            insert_approved_product(&db, "GDL 69A", "GDL69A", "Datalink").await;
        let pool = sqlite_pool(&db);
        let old_link_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id, configuration_action) VALUES (?, ?, 'installed') RETURNING id",
        )
        .bind(listing_id)
        .bind(old_product_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let preserved = PendingReviewAspect::avionics(
            format!("avionics:preserved:{old_link_id}:installed"),
            "avionics_reuse_attestation",
            "Garmin GMA 1347",
            "Garmin GMA 1347",
            PRESERVED_ASSOCIATION_REVIEW_REASON,
            1,
            "installed",
            None,
            None,
        )
        .with_covered_association(
            old_link_id,
            ListingAssociationRole::Installed,
            old_product_id,
        )
        .with_reuse_attestation_target(old_product_id);
        let raw = PendingReviewAspect::avionics(
            "raw-wx",
            "avionics_identity",
            "L3 WX 500",
            "L3 WX 500",
            "raw_observation_unlinked",
            1,
            "installed",
            Some("L3 WX 500".to_string()),
            Some("medium".to_string()),
        );
        stage_pending_review(&db, listing_id, None, &[preserved, raw])
            .await
            .unwrap();
        sqlx::query("DELETE FROM aircraft_sale_listing_avionics WHERE id = ?")
            .bind(old_link_id)
            .execute(pool)
            .await
            .unwrap();
        let current_link_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id, configuration_action) VALUES (?, ?, 'installed') RETURNING id",
        )
        .bind(listing_id)
        .bind(current_product_id)
        .fetch_one(pool)
        .await
        .unwrap();

        restage_unattested_preserved_products(&db, user_id, listing_id)
            .await
            .expect("restage is the stale covered-link recovery boundary")
            .expect("the current listing still requires review");
        let detail = get_listing_review(&db, user_id, listing_id).await.unwrap();
        let ids = detail
            .review
            .aspects
            .iter()
            .map(|aspect| aspect.id.to_string())
            .collect::<HashSet<_>>();
        assert!(ids.contains("raw-wx"));
        assert!(ids.contains(&format!("avionics:preserved:{current_link_id}:installed")));
        assert!(!ids.contains(&format!("avionics:preserved:{old_link_id}:installed")));
    }

    #[tokio::test]
    async fn explicit_avionics_rebuild_is_provider_free_idempotent_and_preserves_authorized_links()
    {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let products = [
            insert_approved_product(&db, "GTN 750Xi", "GTN750XI", "GPS").await,
            insert_approved_product(&db, "GTN 650Xi", "GTN650XI", "GPS").await,
            insert_approved_product(&db, "G5", "G5", "Flight Display").await,
            insert_approved_product(&db, "GFC 500", "GFC500", "Autopilot").await,
            insert_approved_product(&db, "GTX 345", "GTX345", "Transponder").await,
            insert_approved_product(&db, "Flight Stream 210", "FLIGHTSTREAM210", "Datalink").await,
        ];
        let observations = [
            ("GTN 750Xi", "GPS", 1, "Garmin GTN 750Xi installed"),
            ("GTN 650Xi", "GPS", 1, "Garmin GTN 650Xi installed"),
            ("G5", "Flight Display", 2, "Dual Garmin G5 installed"),
            ("GFC 500", "Autopilot", 1, "Garmin GFC 500 installed"),
            ("GTX 345", "Transponder", 1, "Garmin GTX 345 installed"),
            (
                "FlightStream 210",
                "Datalink",
                1,
                "Garmin FlightStream 210 installed",
            ),
        ];
        let rendered_html = observations
            .iter()
            .map(|observation| format!("<p>{}</p>", observation.3))
            .collect::<String>();
        let submission_id =
            insert_review_bound_submission(&db, user_id, listing_id, &rendered_html).await;
        store_current_avionics_extraction(
            &db,
            submission_id,
            serde_json::Value::Array(
                observations
                    .iter()
                    .map(|(model, capability, quantity, evidence)| {
                        serde_json::json!({
                            "manufacturer": "Garmin",
                            "model": model,
                            "types": [capability],
                            "quantity": quantity,
                            "configuration_action": "installed",
                            "replaces": null,
                            "source_evidence_text": evidence,
                            "source_confidence": "high"
                        })
                    })
                    .collect(),
            ),
        )
        .await;

        let mut links = Vec::new();
        let mut prior = Vec::new();
        for (index, ((model, _, quantity, evidence), product_id)) in
            observations.iter().zip(products).enumerate()
        {
            let link_id =
                insert_listing_avionics_link(&db, listing_id, product_id, *quantity, evidence)
                    .await;
            links.push(link_id);
            prior.push(retained_occurrence_aspect(
                &format!("legacy-avionics-{index}"),
                "Garmin",
                model,
                *quantity,
                evidence,
                link_id,
                product_id,
            ));
        }
        insert_current_same_case_authorization(
            &db,
            listing_id,
            links[3],
            products[3],
            submission_id,
            ListingAssociationRole::Installed,
        )
        .await;
        let staged = stage_pending_review(&db, listing_id, Some(submission_id), &prior)
            .await
            .unwrap();
        let usage_before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();

        let RebuildPendingAvionicsReview::Rebuilt {
            review: Some(first),
        } = rebuild_pending_avionics_review_if_current(
            &db,
            user_id,
            listing_id,
            &staged.review_payload_sha256,
        )
        .await
        .unwrap()
        else {
            panic!("strict listing-style fixture must rebuild");
        };
        assert_eq!(first.pending_aspect_count, 5);
        let state = stored_review_state(&db, listing_id).await;
        let payload = parse_payload(&state.0, Some(&state.1), state.2).unwrap();
        assert_eq!(payload.aspects.len(), 5);
        assert!(payload.aspects.iter().all(|aspect| {
            !aspect.id.to_string().contains("legacy")
                && !aspect.reason.contains("listing_action_graph_invalid")
                && aspect.label != "Garmin GFC 500"
        }));
        let flight_stream = payload
            .aspects
            .iter()
            .filter(|aspect| aspect.label == "Garmin FlightStream 210")
            .collect::<Vec<_>>();
        assert_eq!(flight_stream.len(), 1);
        assert_eq!(
            flight_stream[0].covered_associations[0].listing_link_id,
            links[5]
        );

        assert!(matches!(
            rebuild_pending_avionics_review_if_current(
                &db,
                user_id,
                listing_id,
                &staged.review_payload_sha256,
            )
            .await,
            Err(ReviewError::Stale(_))
        ));
        let RebuildPendingAvionicsReview::Rebuilt {
            review: Some(second),
        } = rebuild_pending_avionics_review_if_current(
            &db,
            user_id,
            listing_id,
            &first.review_payload_sha256,
        )
        .await
        .unwrap()
        else {
            panic!("a second current-hash rebuild must be idempotent");
        };
        assert_eq!(second, first);
        let usage_after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gemini_api_usage WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(usage_after, usage_before);

        resolve_listing_review(
            &db,
            user_id,
            listing_id,
            &ResolveReviewRequest {
                expected_review_payload_sha256: second.review_payload_sha256,
                expected_catalog_revision_sha256: second.catalog_revision_sha256,
                finalize_listing: false,
                decisions: payload
                    .aspects
                    .iter()
                    .map(|aspect| ReviewDecision::Discard {
                        aspect_id: aspect.id.clone(),
                        reason: "fixture rejection".to_string(),
                    })
                    .collect(),
            },
        )
        .await
        .unwrap();
        let remaining_links: Vec<(i64, i64, String, Option<String>, String)> = sqlx::query_as(
            "SELECT avionics_model_id, quantity, source, source_notes, configuration_action FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? ORDER BY id",
        )
        .bind(listing_id)
        .fetch_all(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(
            remaining_links,
            vec![(
                products[3],
                1,
                "listing".to_string(),
                Some(observations[3].3.to_string()),
                "installed".to_string(),
            )]
        );
    }

    #[tokio::test]
    async fn changed_identity_rebuild_survives_restage_and_replaces_the_stale_link() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let stale_product_id = insert_approved_product(&db, "G500", "G500", "Flight Display").await;
        let corrected_product_id = insert_approved_product(&db, "G5", "G5", "Flight Display").await;
        attest_approved_product_for_current_policy_reuse(&db, corrected_product_id).await;

        let evidence = "Garmin G5 installed";
        let rendered_html = format!("<main><p>{evidence}</p></main>");
        let submission_id =
            insert_review_bound_submission(&db, user_id, listing_id, &rendered_html).await;
        store_current_avionics_extraction(
            &db,
            submission_id,
            serde_json::json!([{
                "manufacturer": "Garmin",
                "model": "G5",
                "types": ["Flight Display"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": evidence,
                "source_confidence": "high"
            }]),
        )
        .await;
        let link_id =
            insert_listing_avionics_link(&db, listing_id, stale_product_id, 1, evidence).await;
        insert_current_same_case_authorization(
            &db,
            listing_id,
            link_id,
            stale_product_id,
            submission_id,
            ListingAssociationRole::Installed,
        )
        .await;
        let staged = stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[retained_occurrence_aspect(
                "legacy-g5",
                "Garmin",
                "G5",
                1,
                evidence,
                link_id,
                stale_product_id,
            )],
        )
        .await
        .unwrap();

        let RebuildPendingAvionicsReview::Rebuilt {
            review: Some(first),
        } = rebuild_pending_avionics_review_if_current(
            &db,
            user_id,
            listing_id,
            &staged.review_payload_sha256,
        )
        .await
        .unwrap()
        else {
            panic!("the strict changed-identity occurrence must rebuild");
        };
        let state = stored_review_state(&db, listing_id).await;
        let payload = parse_payload(&state.0, Some(&state.1), state.2).unwrap();
        assert_eq!(payload.aspects.len(), 1);
        let aspect = &payload.aspects[0];
        assert_eq!(aspect.label, "Garmin G5");
        assert_eq!(
            aspect.allowed_actions,
            vec![
                ReviewAction::UseVerifiedProduct,
                ReviewAction::CreateVerifiedProduct,
                ReviewAction::Discard,
            ]
        );
        assert_eq!(aspect.suggested_product, None);
        assert_eq!(aspect.reuse_attestation_target_id, None);
        assert_eq!(
            aspect.covered_associations,
            [CoveredListingAssociation {
                listing_link_id: link_id,
                role: ListingAssociationRole::Installed,
                avionics_model_id: stale_product_id,
            }]
        );

        let restaged = restage_pending_review_if_current(
            &db,
            user_id,
            listing_id,
            &first.review_payload_sha256,
        )
        .await
        .unwrap()
        .expect("the G5 correction must remain pending");
        let link_evidence: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT source_notes, source_confidence FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(link_evidence, (None, None));
        let state = stored_review_state(&db, listing_id).await;
        let payload = parse_payload(&state.0, Some(&state.1), state.2).unwrap();
        assert_eq!(payload.aspects.len(), 1);
        assert_eq!(
            payload.aspects[0].source_evidence_text.as_deref(),
            Some(evidence)
        );
        assert_eq!(
            payload.aspects[0].source_confidence.as_deref(),
            Some("high")
        );

        let RebuildPendingAvionicsReview::Rebuilt {
            review: Some(second),
        } = rebuild_pending_avionics_review_if_current(
            &db,
            user_id,
            listing_id,
            &restaged.review_payload_sha256,
        )
        .await
        .unwrap()
        else {
            panic!("the exact prior correction card must align after link evidence is cleared");
        };
        assert_eq!(second.pending_aspect_count, 1);
        let state = stored_review_state(&db, listing_id).await;
        let payload = parse_payload(&state.0, Some(&state.1), state.2).unwrap();
        assert_eq!(payload.aspects.len(), 1);
        assert_eq!(payload.aspects[0].label, "Garmin G5");
        assert_eq!(payload.aspects[0].covered_associations.len(), 1);
        assert_eq!(payload.aspects[0].reuse_attestation_target_id, None);

        let remaining = use_existing_product_for_aspect_and_restage(
            &db,
            user_id,
            listing_id,
            &payload.aspects[0].id,
            &second.review_payload_sha256,
            &second.catalog_revision_sha256,
            corrected_product_id,
        )
        .await
        .unwrap();
        assert!(remaining.is_none());
        let corrected_link: (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT avionics_model_id, source_notes, source_confidence FROM aircraft_sale_listing_avionics WHERE id = ?",
        )
        .bind(link_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
        assert_eq!(
            corrected_link,
            (
                corrected_product_id,
                Some(evidence.to_string()),
                Some("high".to_string())
            )
        );
    }

    #[tokio::test]
    async fn explicit_avionics_rebuild_refuses_non_avionics_state_without_writes() {
        let db = test_db().await;
        let (user_id, listing_id) = insert_listing(&db).await;
        let evidence = "Garmin G5 installed";
        let submission_id =
            insert_review_bound_submission(&db, user_id, listing_id, &format!("<p>{evidence}</p>"))
                .await;
        store_current_avionics_extraction(
            &db,
            submission_id,
            serde_json::json!([{
                "manufacturer": "Garmin",
                "model": "G5",
                "types": ["Flight Display"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": evidence,
                "source_confidence": "high"
            }]),
        )
        .await;
        let aircraft = PendingReviewAspect::avionics(
            "aircraft-registration",
            "aircraft_identity",
            "N12345",
            "N12345",
            "faa_review_required",
            1,
            "installed",
            None,
            None,
        );
        let mut correction = PendingReviewAspect::avionics(
            "corrected-g5",
            REVIEWER_CORRECTED_AVIONICS_KIND,
            "Garmin G5",
            "reviewer-selected Garmin G5",
            "reviewer correction",
            1,
            "installed",
            Some(evidence.to_string()),
            Some("high".to_string()),
        );
        correction.proposed_product = Some(ReviewProduct::proposed(
            "Garmin",
            "G5",
            vec!["Flight Display".to_string()],
        ));
        let staged = stage_pending_review(
            &db,
            listing_id,
            Some(submission_id),
            &[aircraft, correction],
        )
        .await
        .unwrap();
        let before = stored_review_state(&db, listing_id).await;
        let outcome = rebuild_pending_avionics_review_if_current(
            &db,
            user_id,
            listing_id,
            &staged.review_payload_sha256,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            RebuildPendingAvionicsReview::Blocked {
                listing_id,
                reason_code: RebuildPendingAvionicsReviewBlockReason::UnsupportedReviewState,
            }
        );
        assert_eq!(stored_review_state(&db, listing_id).await, before);
    }

    #[tokio::test]
    async fn explicit_avionics_rebuild_refuses_legacy_and_unrepresented_extractions_without_writes()
    {
        let db = test_db().await;
        let (user_id, legacy_listing_id) = insert_listing(&db).await;
        let product_id = insert_approved_product(&db, "G5", "G5", "Flight Display").await;
        let submission_id = insert_review_bound_submission(
            &db,
            user_id,
            legacy_listing_id,
            "<p>Garmin G5 installed</p>",
        )
        .await;
        sqlx::query(
            "UPDATE plugin_submissions SET extracted_listing_json = ?, extraction_error = NULL WHERE id = ?",
        )
        .bind(r#"{"avionics":[{"manufacturer":"Garmin","model":"G5","types":["Flight Display"],"source_evidence_text":"Garmin G5 installed","source_confidence":"high"}]}"#)
        .bind(submission_id)
        .execute(sqlite_pool(&db))
        .await
        .unwrap();
        let link_id = insert_listing_avionics_link(
            &db,
            legacy_listing_id,
            product_id,
            1,
            "Garmin G5 installed",
        )
        .await;
        let staged = stage_pending_review(
            &db,
            legacy_listing_id,
            Some(submission_id),
            &[retained_occurrence_aspect(
                "legacy-g5",
                "Garmin",
                "G5",
                1,
                "Garmin G5 installed",
                link_id,
                product_id,
            )],
        )
        .await
        .unwrap();
        let before = stored_review_state(&db, legacy_listing_id).await;
        let outcome = rebuild_pending_avionics_review_if_current(
            &db,
            user_id,
            legacy_listing_id,
            &staged.review_payload_sha256,
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            RebuildPendingAvionicsReview::Blocked {
                reason_code: RebuildPendingAvionicsReviewBlockReason::ExtractionNotCurrent,
                ..
            }
        ));
        assert_eq!(stored_review_state(&db, legacy_listing_id).await, before);

        let unrepresented_listing_id = insert_additional_listing(
            &db,
            user_id,
            "https://broker.example/aircraft/unrepresented",
        )
        .await;
        let unrepresented_submission_id = insert_review_bound_submission(
            &db,
            user_id,
            unrepresented_listing_id,
            "<p>Garmin G3X installed</p>",
        )
        .await;
        sqlx::query("UPDATE plugin_submissions SET source_url = ? WHERE id = ?")
            .bind("https://broker.example/aircraft/unrepresented")
            .bind(unrepresented_submission_id)
            .execute(sqlite_pool(&db))
            .await
            .unwrap();
        store_current_avionics_extraction(
            &db,
            unrepresented_submission_id,
            serde_json::json!([{
                "manufacturer": "Garmin",
                "model": "G3X",
                "types": ["Flight Display"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": "Garmin G3X installed",
                "source_confidence": "high"
            }]),
        )
        .await;
        let unrepresented = stage_pending_review(
            &db,
            unrepresented_listing_id,
            Some(unrepresented_submission_id),
            &[PendingReviewAspect::avionics(
                "unrelated",
                "avionics",
                "Garmin GTX 345",
                "Garmin GTX 345",
                "legacy_machine_reason",
                1,
                "installed",
                Some("Garmin GTX 345 installed".to_string()),
                Some("high".to_string()),
            )],
        )
        .await
        .unwrap();
        let before = stored_review_state(&db, unrepresented_listing_id).await;
        let outcome = rebuild_pending_avionics_review_if_current(
            &db,
            user_id,
            unrepresented_listing_id,
            &unrepresented.review_payload_sha256,
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            RebuildPendingAvionicsReview::Blocked {
                reason_code: RebuildPendingAvionicsReviewBlockReason::OccurrenceDispositionUnknown,
                ..
            }
        ));
        assert_eq!(
            stored_review_state(&db, unrepresented_listing_id).await,
            before
        );
    }

    #[tokio::test]
    async fn database_rejects_two_links_that_share_a_displacement_target() {
        let db = test_db().await;
        let (_, listing_id) = insert_listing(&db).await;
        let first_id = insert_approved_product(&db, "GNC 355", "GNC355", "GPS").await;
        let second_id = insert_approved_product(&db, "GNC 255", "GNC255", "NAV").await;
        let replacement_id = insert_approved_product(&db, "GNS 430W", "GNS430W", "COM").await;
        let pool = sqlite_pool(&db);
        let insert_link = r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, source_confidence,
              configuration_action, replaces_avionics_model_id
            ) VALUES (?, ?, 'low', 'replaces', ?)
            RETURNING id
        "#;
        let _first_link_id: i64 = sqlx::query_scalar(insert_link)
            .bind(listing_id)
            .bind(first_id)
            .bind(replacement_id)
            .fetch_one(pool)
            .await
            .unwrap();
        let duplicate_target = sqlx::query_scalar::<_, i64>(insert_link)
            .bind(listing_id)
            .bind(second_id)
            .bind(replacement_id)
            .fetch_one(pool)
            .await
            .expect_err("one listing cannot displace the same product twice");

        let remaining_links: Vec<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT avionics_model_id, replaces_avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ? ORDER BY id",
        )
        .bind(listing_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(remaining_links, vec![(first_id, Some(replacement_id))]);
        assert!(duplicate_target.to_string().contains("displaced"));
    }
}
