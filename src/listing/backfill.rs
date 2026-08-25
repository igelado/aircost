//! Non-destructive staging of legacy listing avionics for human review.
//!
//! This module never invokes Gemini or mutates catalog/listing-link rows. It
//! reads retained plugin extraction JSON, including legacy scalar `type`
//! fields, and prepares deterministic review aspects.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::avionics::fingerprint::approved_catalog_revision_sha256;
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::CURATED_AVIONICS_TYPES;
use crate::listing::avionics::{
    approved_avionics_product_key, validate_canonical_avionics_actions, CanonicalAvionicsAction,
};
use crate::listing::review::{
    replace_pending_review, serialize_review_payload, stage_pending_review,
    CoveredListingAssociation, ListingAssociationRole, PendingReviewAspect, ReviewError,
    ReviewProduct,
};
use crate::normalize::{
    is_usable_avionics_label, normalize_avionics_manufacturer_name, normalize_avionics_model_name,
    normalize_name,
};

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 10_000;
const ASPECT_FINGERPRINT_DOMAIN: &[u8] = b"aircost:legacy-listing-avionics-review:v1";
const BACKFILL_REVIEW_PAYLOAD_VERSION: u64 = 1;

#[derive(Debug)]
pub enum BackfillError {
    Validation(String),
    Database(String),
    Review(String),
}

impl fmt::Display for BackfillError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Database(message) | Self::Review(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for BackfillError {}

impl From<sqlx::Error> for BackfillError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

impl From<ReviewError> for BackfillError {
    fn from(error: ReviewError) -> Self {
        Self::Review(error.to_string())
    }
}

pub type BackfillResult<T> = Result<T, BackfillError>;

#[derive(Clone, Debug, Serialize)]
pub struct LegacyReviewBackfillReport {
    pub dry_run: bool,
    pub requested_limit: i64,
    pub requested_listing_id: Option<i64>,
    pub approved_catalog_revision_sha256: String,
    pub listings_selected: usize,
    pub reviews_prepared: usize,
    pub reviews_staged: usize,
    pub listings_without_pending_aspects: usize,
    pub listings_with_errors: usize,
    pub reviews_cleared: usize,
    pub reviews_would_clear: usize,
    pub existing_reviews_preserved: usize,
    pub listings_with_source_issues: usize,
    pub pending_aspects: usize,
    pub associations_requiring_coverage: usize,
    pub covered_associations: usize,
    pub listings_with_incomplete_association_coverage: usize,
    pub reason_counts: BTreeMap<String, usize>,
    pub gemini_calls: usize,
    pub catalog_writes: usize,
    pub listing_link_writes: usize,
    pub listings: Vec<LegacyReviewListingReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LegacyReviewListingReport {
    pub listing_id: i64,
    pub plugin_submission_id: Option<i64>,
    pub raw_observation_count: usize,
    pub existing_link_count: usize,
    pub pending_aspect_count: usize,
    pub existing_pending_review: bool,
    pub existing_review_owned_by_backfill: bool,
    pub associations_requiring_coverage: usize,
    pub covered_association_count: usize,
    pub association_coverage_complete: bool,
    pub status: String,
    pub review_payload_sha256: Option<String>,
    pub reason_counts: BTreeMap<String, usize>,
    pub source_issues: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, FromRow)]
struct ListingSourceRow {
    listing_id: i64,
    plugin_submission_id: Option<i64>,
    extracted_listing_json: Option<String>,
    existing_review_payload_json: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct CatalogRow {
    id: i64,
    manufacturer: String,
    model: String,
    catalog_status: String,
    avionics_manufacturer_identity_id: Option<i64>,
    canonical_product_key: Option<String>,
    capability: Option<String>,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    identity_source_url: Option<String>,
    identity_source_title: Option<String>,
    identity_evidence_text: Option<String>,
}

#[derive(Clone, Debug)]
struct CatalogProduct {
    id: i64,
    manufacturer: String,
    model: String,
    catalog_status: String,
    avionics_manufacturer_identity_id: Option<i64>,
    canonical_product_key: Option<String>,
    capabilities: Vec<String>,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    identity_source_url: Option<String>,
    identity_source_title: Option<String>,
    identity_evidence_text: Option<String>,
}

impl CatalogProduct {
    fn identity_key(&self) -> IdentityKey {
        IdentityKey::new(&self.manufacturer, &self.model)
    }

    fn review_product(&self) -> ReviewProduct {
        let mut product = if self.catalog_status == "approved" {
            ReviewProduct::verified(
                self.id,
                self.manufacturer.clone(),
                self.model.clone(),
                self.capabilities.clone(),
            )
        } else if self.catalog_status == "unreviewed" {
            ReviewProduct::unreviewed_catalog_candidate(
                self.id,
                self.manufacturer.clone(),
                self.model.clone(),
                self.capabilities.clone(),
            )
        } else {
            ReviewProduct::proposed(
                self.manufacturer.clone(),
                self.model.clone(),
                self.capabilities.clone(),
            )
        };
        if let (Some(kind), Some(identifier)) = (
            self.manufacturer_identifier_kind.as_deref(),
            self.manufacturer_identifier.as_deref(),
        ) {
            if !kind.trim().is_empty() && !identifier.trim().is_empty() {
                product = product.with_stable_identifier(kind, identifier);
            }
        }
        if let (Some(url), Some(title), Some(evidence)) = (
            self.identity_source_url.as_deref(),
            self.identity_source_title.as_deref(),
            self.identity_evidence_text.as_deref(),
        ) {
            if !url.trim().is_empty() && !title.trim().is_empty() && !evidence.trim().is_empty() {
                product = product.with_identity_evidence(url, title, evidence);
            }
        }
        product
    }

    fn approved_graph_key(&self) -> Result<String, String> {
        if self.catalog_status != "approved" {
            return Err(format!("catalog id {} is not approved", self.id));
        }
        let manufacturer_identity_id = self.avionics_manufacturer_identity_id.ok_or_else(|| {
            format!(
                "approved catalog id {} has no manufacturer identity",
                self.id
            )
        })?;
        let product_key = self.canonical_product_key.as_deref().ok_or_else(|| {
            format!(
                "approved catalog id {} has no canonical product key",
                self.id
            )
        })?;
        approved_avionics_product_key(manufacturer_identity_id, product_key)
    }
}

#[derive(Clone, Debug, FromRow)]
struct ListingLinkRow {
    id: i64,
    avionics_model_id: i64,
    quantity: i64,
    source: String,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    source_confidence: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct IdentityKey {
    manufacturer: String,
    model: String,
}

impl IdentityKey {
    fn new(manufacturer: &str, model: &str) -> Self {
        Self {
            manufacturer: normalize_avionics_manufacturer_name(manufacturer),
            model: normalize_avionics_model_name(model),
        }
    }

    fn is_empty(&self) -> bool {
        self.manufacturer.is_empty() || self.model.is_empty()
    }
}

#[derive(Clone, Debug)]
struct RawIdentity {
    manufacturer: String,
    model: String,
    capabilities: Vec<String>,
}

impl RawIdentity {
    fn identity_key(&self) -> IdentityKey {
        IdentityKey::new(&self.manufacturer, &self.model)
    }

    fn label(&self) -> String {
        let label = format!("{} {}", self.manufacturer.trim(), self.model.trim())
            .trim()
            .to_string();
        if label.is_empty() {
            "Unidentified avionics observation".to_string()
        } else {
            label
        }
    }

    fn proposed_product(&self) -> Option<ReviewProduct> {
        is_usable_avionics_label(&self.manufacturer, &self.model).then(|| {
            ReviewProduct::proposed(
                self.manufacturer.trim(),
                self.model.trim(),
                self.capabilities.clone(),
            )
        })
    }
}

#[derive(Clone, Debug)]
struct RawObservation {
    source_index: usize,
    identity: RawIdentity,
    quantity: i64,
    configuration_action: String,
    replaces: Option<RawIdentity>,
    source_evidence_text: Option<String>,
    source_confidence: Option<String>,
    issues: Vec<String>,
}

impl RawObservation {
    fn has_usable_identity(&self) -> bool {
        is_usable_avionics_label(&self.identity.manufacturer, &self.identity.model)
    }

    fn grouping_key(&self) -> String {
        let identity = self.identity.identity_key();
        if identity.is_empty() {
            return format!("invalid:{}", self.source_index);
        }
        let replacement = self
            .replaces
            .as_ref()
            .map(RawIdentity::identity_key)
            .map(|key| format!("{}:{}", key.manufacturer, key.model))
            .unwrap_or_default();
        format!(
            "{}:{}:{}:{}",
            identity.manufacturer, identity.model, self.configuration_action, replacement
        )
    }

    fn observed_text(&self) -> String {
        let mut text = self.identity.label();
        if !self.identity.capabilities.is_empty() {
            text.push_str(" [");
            text.push_str(&self.identity.capabilities.join(", "));
            text.push(']');
        }
        if let Some(replacement) = &self.replaces {
            text.push_str("; ");
            text.push_str(&self.configuration_action);
            text.push(' ');
            text.push_str(&replacement.label());
        }
        text
    }
}

struct PreparedListingReview {
    aspects: Vec<PendingReviewAspect>,
    raw_observation_count: usize,
    existing_link_count: usize,
    reason_counts: BTreeMap<String, usize>,
    source_issues: Vec<String>,
    associations_requiring_coverage: usize,
    covered_association_count: usize,
    association_coverage_complete: bool,
}

/// Stages review bundles for existing listings. Passing `apply = false` is a
/// strict read-only preview.
pub async fn stage_legacy_listing_reviews(
    db: &AppDb,
    apply: bool,
    limit: i64,
    listing_id: Option<i64>,
) -> BackfillResult<LegacyReviewBackfillReport> {
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(BackfillError::Validation(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    if listing_id.is_some_and(|id| id <= 0) {
        return Err(BackfillError::Validation(
            "listing_id must be positive".to_string(),
        ));
    }

    let sources = load_listing_sources(db, limit, listing_id).await?;
    let catalog = load_catalog(db).await?;
    let catalog_by_id = catalog
        .iter()
        .cloned()
        .map(|product| (product.id, product))
        .collect::<HashMap<_, _>>();
    let catalog_revision = approved_catalog_revision_sha256(db)
        .await
        .map_err(ReviewError::from)?;
    let mut listings = Vec::with_capacity(sources.len());

    for source in &sources {
        let links = load_listing_links(db, source.listing_id).await?;
        let prepared = prepare_listing_review(
            source.listing_id,
            source.extracted_listing_json.as_deref(),
            &links,
            &catalog,
            &catalog_by_id,
        );
        let existing_pending_review = source.existing_review_payload_json.is_some();
        let existing_review_owned_by_backfill = existing_review_owned_by_backfill(
            source.listing_id,
            source.existing_review_payload_json.as_deref(),
        );
        let mut report = LegacyReviewListingReport {
            listing_id: source.listing_id,
            plugin_submission_id: source.plugin_submission_id,
            raw_observation_count: prepared.raw_observation_count,
            existing_link_count: prepared.existing_link_count,
            pending_aspect_count: prepared.aspects.len(),
            existing_pending_review,
            existing_review_owned_by_backfill,
            associations_requiring_coverage: prepared.associations_requiring_coverage,
            covered_association_count: prepared.covered_association_count,
            association_coverage_complete: prepared.association_coverage_complete,
            status: "no_review_needed".to_string(),
            review_payload_sha256: None,
            reason_counts: prepared.reason_counts,
            source_issues: prepared.source_issues,
            error: None,
        };
        if existing_pending_review && !existing_review_owned_by_backfill {
            report.status = "existing_review_preserved".to_string();
        } else if !prepared.association_coverage_complete {
            report.status = "error".to_string();
            report.error = Some(
                "pending review does not cover every association component requiring adjudication"
                    .to_string(),
            );
        } else if !prepared.aspects.is_empty() {
            match serialize_review_payload(&prepared.aspects) {
                Ok(serialized) => {
                    report.review_payload_sha256 = Some(serialized.review_payload_sha256);
                    if apply {
                        match stage_pending_review(
                            db,
                            source.listing_id,
                            source.plugin_submission_id,
                            &prepared.aspects,
                        )
                        .await
                        {
                            Ok(staged) => {
                                report.status = "staged".to_string();
                                report.review_payload_sha256 = Some(staged.review_payload_sha256);
                            }
                            Err(error) => {
                                report.status = "error".to_string();
                                report.error = Some(error.to_string());
                            }
                        }
                    } else {
                        report.status = "would_stage".to_string();
                    }
                }
                Err(error) => {
                    report.status = "error".to_string();
                    report.error = Some(error.to_string());
                }
            }
        } else if existing_pending_review {
            if apply {
                match replace_pending_review(db, source.listing_id, None, &[]).await {
                    Ok(None) => report.status = "cleared".to_string(),
                    Ok(Some(_)) => unreachable!("an empty review cannot be staged"),
                    Err(error) => {
                        report.status = "error".to_string();
                        report.error = Some(error.to_string());
                    }
                }
            } else {
                report.status = "would_clear".to_string();
            }
        }
        listings.push(report);
    }

    let mut reason_counts = BTreeMap::new();
    for listing in &listings {
        for (reason, count) in &listing.reason_counts {
            *reason_counts.entry(reason.clone()).or_insert(0) += count;
        }
    }
    Ok(LegacyReviewBackfillReport {
        dry_run: !apply,
        requested_limit: limit,
        requested_listing_id: listing_id,
        approved_catalog_revision_sha256: catalog_revision,
        listings_selected: listings.len(),
        reviews_prepared: listings
            .iter()
            .filter(|listing| listing.pending_aspect_count > 0)
            .count(),
        reviews_staged: listings
            .iter()
            .filter(|listing| listing.status == "staged")
            .count(),
        listings_without_pending_aspects: listings
            .iter()
            .filter(|listing| listing.pending_aspect_count == 0)
            .count(),
        listings_with_errors: listings
            .iter()
            .filter(|listing| listing.status == "error")
            .count(),
        reviews_cleared: listings
            .iter()
            .filter(|listing| listing.status == "cleared")
            .count(),
        reviews_would_clear: listings
            .iter()
            .filter(|listing| listing.status == "would_clear")
            .count(),
        existing_reviews_preserved: listings
            .iter()
            .filter(|listing| listing.status == "existing_review_preserved")
            .count(),
        listings_with_source_issues: listings
            .iter()
            .filter(|listing| !listing.source_issues.is_empty())
            .count(),
        pending_aspects: listings
            .iter()
            .map(|listing| listing.pending_aspect_count)
            .sum(),
        associations_requiring_coverage: listings
            .iter()
            .map(|listing| listing.associations_requiring_coverage)
            .sum(),
        covered_associations: listings
            .iter()
            .map(|listing| listing.covered_association_count)
            .sum(),
        listings_with_incomplete_association_coverage: listings
            .iter()
            .filter(|listing| !listing.association_coverage_complete)
            .count(),
        reason_counts,
        gemini_calls: 0,
        catalog_writes: 0,
        listing_link_writes: 0,
        listings,
    })
}

pub const fn default_stage_limit() -> i64 {
    DEFAULT_LIMIT
}

fn prepare_listing_review(
    listing_id: i64,
    extracted_listing_json: Option<&str>,
    links: &[ListingLinkRow],
    catalog: &[CatalogProduct],
    catalog_by_id: &HashMap<i64, CatalogProduct>,
) -> PreparedListingReview {
    let (observations, mut source_issues) = parse_retained_observations(extracted_listing_json);
    let raw_observation_count = observations.len();
    let observations = coalesce_observations(observations);
    let action_graph_issue = listing_action_graph_issue(links, catalog_by_id);
    let action_graph_invalid = action_graph_issue.is_some();
    if let Some(issue) = &action_graph_issue {
        source_issues.push(format!("listing_action_graph_invalid: {issue}"));
    }
    let has_usable_retained_observations =
        observations.iter().any(RawObservation::has_usable_identity);
    let mut aspects = BTreeMap::<String, PendingReviewAspect>::new();
    let mut reason_counts = BTreeMap::new();
    let mut matched_link_ids = HashSet::new();

    let raw_replacement_context = RawReplacementContext {
        listing_id,
        catalog,
        catalog_by_id,
    };
    for observation in &observations {
        let key = observation.identity.identity_key();
        let matching_links = if observation.has_usable_identity() {
            links
                .iter()
                .filter(|link| {
                    catalog_by_id
                        .get(&link.avionics_model_id)
                        .is_some_and(|product| product.identity_key() == key)
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if matching_links.len() == 1 {
            matched_link_ids.insert(matching_links[0].id);
        }
        if !action_graph_invalid
            && matching_links.len() == 1
            && matching_links[0].source == "listing_review"
        {
            // A prior reviewer decision is stronger evidence than the retained
            // extraction that originally fed the legacy link. Do not reopen or
            // cover that association during backfill.
            continue;
        }
        if matching_links.len() == 1
            && !action_graph_invalid
            && observation.issues.is_empty()
            && confidently_linked(observation, matching_links[0], catalog_by_id)
        {
            continue;
        }

        let mut reasons = observation.issues.clone();
        if action_graph_invalid {
            reasons.push("listing_action_graph_invalid".to_string());
        }
        match matching_links.as_slice() {
            [] => reasons.push("raw_observation_unlinked".to_string()),
            [link] => {
                let product = catalog_by_id.get(&link.avionics_model_id);
                if product.is_none_or(|product| product.catalog_status != "approved") {
                    reasons.push("catalog_product_unverified".to_string());
                }
                if link.source_confidence.as_deref() != Some("high") {
                    reasons.push("listing_link_confidence_not_high".to_string());
                }
                if link.configuration_action != observation.configuration_action {
                    reasons.push("configuration_action_mismatch".to_string());
                }
                if link.quantity.max(1) != observation.quantity.max(1) {
                    reasons.push("quantity_mismatch".to_string());
                }
                if !capabilities_supported(observation, product) {
                    reasons.push("capability_mismatch_or_unknown".to_string());
                }
                if !replacement_matches(observation, link, catalog_by_id) {
                    reasons.push("replacement_identity_mismatch".to_string());
                }
            }
            _ => reasons.push("raw_observation_ambiguous".to_string()),
        }
        deduplicate_strings(&mut reasons);
        count_reasons(&mut reason_counts, &reasons);
        let id = deterministic_aspect_id(listing_id, "primary", &observation.grouping_key());
        let mut aspect = PendingReviewAspect::avionics(
            id.clone(),
            "avionics",
            observation.identity.label(),
            observation.observed_text(),
            reasons.join(","),
            observation.quantity.max(1),
            observation.configuration_action.clone(),
            observation.source_evidence_text.clone(),
            observation.source_confidence.clone(),
        );
        if let Some(suggestion) = approved_suggestion(&key, catalog) {
            aspect = aspect.with_suggested_product(suggestion.review_product());
        }
        let proposal = if matching_links.is_empty() {
            unique_unreviewed_candidate(&key, catalog)
                .map(CatalogProduct::review_product)
                .or_else(|| observation.identity.proposed_product())
        } else {
            observation.identity.proposed_product()
        };
        if let Some(proposal) = proposal {
            aspect = aspect.with_proposed_product(proposal);
        }
        if matching_links.len() == 1 {
            let link = matching_links[0];
            aspect = aspect.with_covered_association(
                link.id,
                ListingAssociationRole::Installed,
                link.avionics_model_id,
            );
        }
        attach_raw_replacement(
            &raw_replacement_context,
            observation,
            &matching_links,
            &mut aspect,
            &mut aspects,
            &mut reason_counts,
        );
        aspects.insert(id, aspect);
    }

    for link in links {
        if matched_link_ids.contains(&link.id) {
            continue;
        }
        if link.source == "listing_review" && !action_graph_invalid {
            // Backfill is not allowed to overturn an explicit reviewer
            // decision, even when the old retained extraction has drifted.
            continue;
        }
        let Some(product) = catalog_by_id.get(&link.avionics_model_id) else {
            source_issues.push(format!(
                "listing link references missing avionics model {}",
                link.avionics_model_id
            ));
            continue;
        };
        let approved_high_link_unmatched_by_retained_observation = has_usable_retained_observations
            && product.catalog_status == "approved"
            && link.source_confidence.as_deref() == Some("high");
        let replacement_needs_review = link.replaces_avionics_model_id.is_some_and(|id| {
            action_graph_invalid
                || approved_high_link_unmatched_by_retained_observation
                || link.source_confidence.as_deref() != Some("high")
                || catalog_by_id
                    .get(&id)
                    .is_none_or(|replacement| replacement.catalog_status != "approved")
        });
        let installed_needs_review = action_graph_invalid
            || product.catalog_status != "approved"
            || link.source_confidence.as_deref() != Some("high")
            || approved_high_link_unmatched_by_retained_observation;
        if !installed_needs_review && !replacement_needs_review {
            continue;
        }
        let mut reasons = Vec::new();
        if action_graph_invalid {
            reasons.push("listing_action_graph_invalid".to_string());
        }
        if approved_high_link_unmatched_by_retained_observation {
            reasons.push(
                "approved_high_confidence_link_unmatched_by_retained_observation".to_string(),
            );
        } else if product.catalog_status != "approved" {
            reasons.push("catalog_product_unverified_without_raw_observation".to_string());
        } else if link.source_confidence.as_deref() != Some("high") {
            reasons.push("approved_link_confidence_not_high".to_string());
        }
        if replacement_needs_review {
            reasons.push("replacement_association_requires_review".to_string());
        }
        count_reasons(&mut reason_counts, &reasons);
        let key = product.identity_key();
        let id = deterministic_aspect_id(
            listing_id,
            "legacy-link",
            &format!("{}:{}:link:{}", key.manufacturer, key.model, link.id),
        );
        let mut aspect = PendingReviewAspect::avionics(
            id.clone(),
            "avionics",
            format!("{} {}", product.manufacturer, product.model),
            format!("{} {}", product.manufacturer, product.model),
            reasons.join(","),
            link.quantity.max(1),
            link.configuration_action.clone(),
            None,
            None,
        );
        if product.catalog_status == "approved" {
            aspect = aspect.with_suggested_product(product.review_product());
        } else {
            if is_usable_avionics_label(&product.manufacturer, &product.model) {
                aspect = aspect.with_proposed_product(product.review_product());
            }
        }
        aspect = aspect.with_covered_association(
            link.id,
            ListingAssociationRole::Installed,
            link.avionics_model_id,
        );
        attach_link_replacement(
            listing_id,
            link,
            catalog_by_id,
            &mut aspect,
            &mut aspects,
            &mut reason_counts,
            &mut source_issues,
            approved_high_link_unmatched_by_retained_observation,
        );
        aspects.insert(id, aspect);
    }

    let aspects = aspects.into_values().collect::<Vec<_>>();
    let covered_associations = aspects
        .iter()
        .flat_map(|aspect| aspect.covered_associations.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut required_associations = BTreeSet::new();
    for link in links {
        if link.source == "listing_review" && !action_graph_invalid {
            continue;
        }
        let unmatched_approved_high_requires_review = has_usable_retained_observations
            && !matched_link_ids.contains(&link.id)
            && link.source_confidence.as_deref() == Some("high")
            && catalog_by_id
                .get(&link.avionics_model_id)
                .is_some_and(|product| product.catalog_status == "approved");
        let installed_requires_review = action_graph_invalid
            || unmatched_approved_high_requires_review
            || link.source_confidence.as_deref() != Some("high")
            || catalog_by_id
                .get(&link.avionics_model_id)
                .is_none_or(|product| product.catalog_status != "approved");
        if installed_requires_review {
            required_associations.insert(CoveredListingAssociation {
                listing_link_id: link.id,
                role: ListingAssociationRole::Installed,
                avionics_model_id: link.avionics_model_id,
            });
        }
        if let Some(replacement_id) = link.replaces_avionics_model_id {
            let replacement_requires_review = action_graph_invalid
                || unmatched_approved_high_requires_review
                || link.source_confidence.as_deref() != Some("high")
                || catalog_by_id
                    .get(&replacement_id)
                    .is_none_or(|product| product.catalog_status != "approved");
            if replacement_requires_review {
                required_associations.insert(CoveredListingAssociation {
                    listing_link_id: link.id,
                    role: ListingAssociationRole::Installed,
                    avionics_model_id: link.avionics_model_id,
                });
                required_associations.insert(CoveredListingAssociation {
                    listing_link_id: link.id,
                    role: ListingAssociationRole::Replacement,
                    avionics_model_id: replacement_id,
                });
            }
        }
    }
    required_associations.extend(covered_associations.iter().cloned());
    let association_coverage_complete = required_associations == covered_associations;
    if !association_coverage_complete {
        source_issues.push(format!(
            "listing association coverage mismatch: expected {:?}, covered {:?}",
            required_associations, covered_associations
        ));
    }
    PreparedListingReview {
        aspects,
        raw_observation_count,
        existing_link_count: links.len(),
        reason_counts,
        source_issues,
        associations_requiring_coverage: required_associations.len(),
        covered_association_count: covered_associations.len(),
        association_coverage_complete,
    }
}

fn listing_action_graph_issue(
    links: &[ListingLinkRow],
    catalog_by_id: &HashMap<i64, CatalogProduct>,
) -> Option<String> {
    let mut actions = Vec::with_capacity(links.len());
    for link in links {
        let subject = catalog_by_id.get(&link.avionics_model_id).ok_or_else(|| {
            format!(
                "listing link {} references missing subject catalog id {}",
                link.id, link.avionics_model_id
            )
        });
        let subject = match subject {
            Ok(subject) => subject,
            Err(error) => return Some(error),
        };
        let subject_key = match subject.approved_graph_key() {
            Ok(key) => key,
            Err(error) => return Some(error),
        };
        let displaced_key = match link.configuration_action.as_str() {
            "installed" => {
                if link.replaces_avionics_model_id.is_some() {
                    return Some(format!(
                        "installed listing link {} unexpectedly has a displaced target",
                        link.id
                    ));
                }
                None
            }
            "replaces" => {
                let Some(target) = link.replaces_avionics_model_id else {
                    return Some(format!(
                        "replacement listing link {} has no displaced target",
                        link.id
                    ));
                };
                if target == link.avionics_model_id {
                    return Some(format!(
                        "listing link {} makes catalog id {target} replace itself",
                        link.id
                    ));
                }
                let displaced = match catalog_by_id.get(&target) {
                    Some(displaced) => displaced,
                    None => {
                        return Some(format!(
                            "listing link {} references missing displaced catalog id {target}",
                            link.id
                        ))
                    }
                };
                match displaced.approved_graph_key() {
                    Ok(key) => Some(key),
                    Err(error) => return Some(error),
                }
            }
            "removes" => {
                let Some(target) = link.replaces_avionics_model_id else {
                    return Some(format!(
                        "removal listing link {} has no displaced target",
                        link.id
                    ));
                };
                if target != link.avionics_model_id {
                    return Some(format!(
                        "listing link {} removal subject {} differs from displaced catalog id {target}",
                        link.id, link.avionics_model_id
                    ));
                }
                Some(subject_key.clone())
            }
            unsupported => {
                return Some(format!(
                    "listing link {} has unsupported action {unsupported:?}",
                    link.id
                ))
            }
        };
        actions.push(CanonicalAvionicsAction::new(
            subject_key,
            link.configuration_action.clone(),
            displaced_key,
        ));
    }
    validate_canonical_avionics_actions(&actions).err()
}

struct RawReplacementContext<'a> {
    listing_id: i64,
    catalog: &'a [CatalogProduct],
    catalog_by_id: &'a HashMap<i64, CatalogProduct>,
}

fn attach_raw_replacement(
    context: &RawReplacementContext<'_>,
    observation: &RawObservation,
    matching_links: &[&ListingLinkRow],
    aspect: &mut PendingReviewAspect,
    aspects: &mut BTreeMap<String, PendingReviewAspect>,
    reason_counts: &mut BTreeMap<String, usize>,
) {
    if !matches!(
        observation.configuration_action.as_str(),
        "replaces" | "removes"
    ) {
        return;
    }
    let Some(replacement) = observation.replaces.as_ref() else {
        // Keep malformed input reviewable without persisting an invalid
        // replacement relationship.
        aspect.configuration_action = "installed".to_string();
        return;
    };
    let replacement_key = replacement.identity_key();
    let matching_link = (matching_links.len() == 1).then_some(matching_links[0]);
    let replacement_requires_coverage = matching_link.is_some_and(|link| {
        link.replaces_avionics_model_id.is_some_and(|id| {
            link.source_confidence.as_deref() != Some("high")
                || context
                    .catalog_by_id
                    .get(&id)
                    .is_none_or(|product| product.catalog_status != "approved")
        })
    });
    if !replacement_requires_coverage {
        if let Some(product) = approved_suggestion(&replacement_key, context.catalog) {
            *aspect = aspect.clone().with_replacement_product(product.id);
            return;
        }
    }
    let replacement_id = deterministic_aspect_id(
        context.listing_id,
        "replacement",
        &format!(
            "{}:{}:{}",
            replacement_key.manufacturer,
            replacement_key.model,
            matching_link
                .map(|link| format!("link:{}", link.id))
                .unwrap_or_else(|| "unlinked".to_string())
        ),
    );
    let reasons = vec!["replacement_product_unverified".to_string()];
    count_reasons(reason_counts, &reasons);
    let mut replacement_aspect = PendingReviewAspect::avionics(
        replacement_id.clone(),
        "avionics",
        replacement.label(),
        replacement.label(),
        reasons.join(","),
        1,
        "installed",
        observation.source_evidence_text.clone(),
        observation.source_confidence.clone(),
    );
    let proposal = if matching_link.is_none() {
        unique_unreviewed_candidate(&replacement_key, context.catalog)
            .map(CatalogProduct::review_product)
            .or_else(|| replacement.proposed_product())
    } else {
        replacement.proposed_product()
    };
    if let Some(proposal) = proposal {
        replacement_aspect = replacement_aspect.with_proposed_product(proposal);
    }
    if let Some(suggestion) = approved_suggestion(&replacement_key, context.catalog) {
        replacement_aspect = replacement_aspect.with_suggested_product(suggestion.review_product());
    }
    if replacement_requires_coverage {
        if let Some(link) = matching_link {
            if let Some(replacement_id) = link.replaces_avionics_model_id {
                replacement_aspect = replacement_aspect.with_covered_association(
                    link.id,
                    ListingAssociationRole::Replacement,
                    replacement_id,
                );
            }
        }
    }
    aspects
        .entry(replacement_id.clone())
        .or_insert(replacement_aspect);
    *aspect = aspect.clone().with_replacement_aspect(replacement_id);
}

#[allow(clippy::too_many_arguments)]
fn attach_link_replacement(
    listing_id: i64,
    link: &ListingLinkRow,
    catalog_by_id: &HashMap<i64, CatalogProduct>,
    aspect: &mut PendingReviewAspect,
    aspects: &mut BTreeMap<String, PendingReviewAspect>,
    reason_counts: &mut BTreeMap<String, usize>,
    source_issues: &mut Vec<String>,
    force_association_review: bool,
) {
    if !matches!(link.configuration_action.as_str(), "replaces" | "removes") {
        return;
    }
    let Some(replacement_id) = link.replaces_avionics_model_id else {
        aspect.configuration_action = "installed".to_string();
        return;
    };
    let Some(replacement) = catalog_by_id.get(&replacement_id) else {
        source_issues.push(format!(
            "listing link {} references missing replacement model {replacement_id}",
            link.avionics_model_id
        ));
        aspect.configuration_action = "installed".to_string();
        return;
    };
    let replacement_requires_coverage = force_association_review
        || replacement.catalog_status != "approved"
        || link.source_confidence.as_deref() != Some("high");
    if !replacement_requires_coverage {
        *aspect = aspect.clone().with_replacement_product(replacement_id);
        return;
    }
    let key = replacement.identity_key();
    let replacement_aspect_id = deterministic_aspect_id(
        listing_id,
        "replacement",
        &format!("{}:{}:link:{}", key.manufacturer, key.model, link.id),
    );
    let reasons = if force_association_review {
        vec!["approved_high_confidence_replacement_unmatched_by_retained_observation".to_string()]
    } else if replacement.catalog_status == "approved" {
        vec!["replacement_association_confidence_not_high".to_string()]
    } else {
        vec!["replacement_product_unverified".to_string()]
    };
    count_reasons(reason_counts, &reasons);
    let mut replacement_aspect = PendingReviewAspect::avionics(
        replacement_aspect_id.clone(),
        "avionics",
        format!("{} {}", replacement.manufacturer, replacement.model),
        format!("{} {}", replacement.manufacturer, replacement.model),
        reasons.join(","),
        1,
        "installed",
        None,
        None,
    )
    .with_covered_association(link.id, ListingAssociationRole::Replacement, replacement.id);
    if replacement.catalog_status == "approved" {
        replacement_aspect =
            replacement_aspect.with_suggested_product(replacement.review_product());
    } else if is_usable_avionics_label(&replacement.manufacturer, &replacement.model) {
        replacement_aspect = replacement_aspect.with_proposed_product(replacement.review_product());
    }
    aspects
        .entry(replacement_aspect_id.clone())
        .or_insert(replacement_aspect);
    *aspect = aspect
        .clone()
        .with_replacement_aspect(replacement_aspect_id);
}

fn confidently_linked(
    observation: &RawObservation,
    link: &ListingLinkRow,
    catalog_by_id: &HashMap<i64, CatalogProduct>,
) -> bool {
    catalog_by_id
        .get(&link.avionics_model_id)
        .is_some_and(|product| product.catalog_status == "approved")
        && link.source_confidence.as_deref() == Some("high")
        && link.configuration_action == observation.configuration_action
        && link.quantity.max(1) == observation.quantity.max(1)
        && capabilities_supported(observation, catalog_by_id.get(&link.avionics_model_id))
        && replacement_matches(observation, link, catalog_by_id)
}

fn capabilities_supported(observation: &RawObservation, product: Option<&CatalogProduct>) -> bool {
    let Some(product) = product else {
        return false;
    };
    !observation.identity.capabilities.is_empty()
        && observation.identity.capabilities.iter().all(|observed| {
            product
                .capabilities
                .iter()
                .any(|stored| normalize_name(stored) == normalize_name(observed))
        })
}

fn replacement_matches(
    observation: &RawObservation,
    link: &ListingLinkRow,
    catalog_by_id: &HashMap<i64, CatalogProduct>,
) -> bool {
    match observation.configuration_action.as_str() {
        "installed" => observation.replaces.is_none() && link.replaces_avionics_model_id.is_none(),
        "replaces" | "removes" => {
            let Some(observed) = observation.replaces.as_ref().map(RawIdentity::identity_key)
            else {
                return false;
            };
            let Some(product) = link
                .replaces_avionics_model_id
                .and_then(|id| catalog_by_id.get(&id))
            else {
                return false;
            };
            product.catalog_status == "approved" && product.identity_key() == observed
        }
        _ => false,
    }
}

fn approved_suggestion<'a>(
    identity: &IdentityKey,
    catalog: &'a [CatalogProduct],
) -> Option<&'a CatalogProduct> {
    if identity.is_empty() {
        return None;
    }
    let mut candidates = catalog.iter().filter(|product| {
        product.catalog_status == "approved" && product.identity_key() == *identity
    });
    let candidate = candidates.next()?;
    candidates.next().is_none().then_some(candidate)
}

/// Returns an explicit promotion candidate only when the normalized identity
/// selects exactly one catalog row and that row is still unreviewed. An
/// approved/rejected row or any duplicate normalized identity is deliberately
/// left for the reviewer to resolve without an in-place target.
fn unique_unreviewed_candidate<'a>(
    identity: &IdentityKey,
    catalog: &'a [CatalogProduct],
) -> Option<&'a CatalogProduct> {
    if identity.is_empty() {
        return None;
    }
    let mut matches = catalog
        .iter()
        .filter(|product| product.identity_key() == *identity);
    let candidate = matches.next()?;
    (matches.next().is_none() && candidate.catalog_status == "unreviewed").then_some(candidate)
}

fn parse_retained_observations(raw_json: Option<&str>) -> (Vec<RawObservation>, Vec<String>) {
    let Some(raw_json) = raw_json.filter(|value| !value.trim().is_empty()) else {
        return (Vec::new(), vec!["retained_extraction_missing".to_string()]);
    };
    let value: Value = match serde_json::from_str(raw_json) {
        Ok(value) => value,
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("retained_extraction_invalid_json: {error}")],
            )
        }
    };
    let Some(values) = value.get("avionics").and_then(Value::as_array) else {
        return (
            Vec::new(),
            vec!["retained_extraction_missing_avionics_array".to_string()],
        );
    };
    let mut issues = Vec::new();
    let observations = values
        .iter()
        .enumerate()
        .map(|(index, value)| parse_observation(index, value, &mut issues))
        .collect();
    if values.is_empty() {
        issues.push("retained_extraction_empty_avionics_array".to_string());
    }
    (observations, issues)
}

fn parse_observation(
    index: usize,
    value: &Value,
    source_issues: &mut Vec<String>,
) -> RawObservation {
    let object = value.as_object();
    let manufacturer = object
        .and_then(|object| object.get("manufacturer"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let model = object
        .and_then(|object| object.get("model"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut issues = Vec::new();
    if object.is_none() {
        issues.push("raw_observation_not_an_object".to_string());
    }
    if !is_usable_avionics_label(&manufacturer, &model) {
        issues.push("raw_observation_identity_unusable".to_string());
    }
    let capabilities = parse_capabilities(value, &mut issues);
    let quantity = object
        .and_then(|object| object.get("quantity"))
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let quantity = if quantity > 0 {
        quantity
    } else {
        issues.push("raw_observation_quantity_invalid".to_string());
        1
    };
    let raw_action = object
        .and_then(|object| object.get("configuration_action"))
        .and_then(Value::as_str)
        .unwrap_or("installed");
    let configuration_action = if matches!(raw_action, "installed" | "replaces" | "removes") {
        raw_action.to_string()
    } else {
        issues.push("raw_observation_configuration_action_invalid".to_string());
        "installed".to_string()
    };
    let replaces = object
        .and_then(|object| object.get("replaces"))
        .filter(|value| !value.is_null())
        .map(|replacement| parse_identity(replacement, &mut issues, "replacement"));
    if configuration_action == "installed" && replaces.is_some() {
        issues.push("installed_observation_has_replacement".to_string());
    }
    if matches!(configuration_action.as_str(), "replaces" | "removes") && replaces.is_none() {
        issues.push("replacement_identity_missing".to_string());
    }
    let mut source_evidence_text = object
        .and_then(|object| object.get("source_evidence_text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let mut source_confidence = object
        .and_then(|object| object.get("source_confidence"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|confidence| {
            if matches!(confidence, "high" | "medium" | "low") {
                Some(confidence.to_string())
            } else {
                issues.push("raw_observation_source_confidence_invalid".to_string());
                None
            }
        });
    if source_evidence_text.is_none() != source_confidence.is_none() {
        issues.push("raw_observation_source_evidence_pair_incomplete".to_string());
        source_evidence_text = None;
        source_confidence = None;
    }
    if !issues.is_empty() {
        source_issues.push(format!("avionics[{index}]: {}", issues.join(",")));
    }
    RawObservation {
        source_index: index,
        identity: RawIdentity {
            manufacturer,
            model,
            capabilities,
        },
        quantity,
        configuration_action,
        replaces,
        source_evidence_text,
        source_confidence,
        issues,
    }
}

fn parse_identity(value: &Value, issues: &mut Vec<String>, role: &str) -> RawIdentity {
    let manufacturer = value
        .get("manufacturer")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if !is_usable_avionics_label(&manufacturer, &model) {
        issues.push(format!("{role}_identity_unusable"));
    }
    RawIdentity {
        manufacturer,
        model,
        capabilities: parse_capabilities(value, issues),
    }
}

fn parse_capabilities(value: &Value, issues: &mut Vec<String>) -> Vec<String> {
    let mut raw = Vec::new();
    if let Some(types) = value.get("types") {
        match types.as_array() {
            Some(types) => {
                for value in types {
                    if let Some(value) = value
                        .as_str()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        raw.push(value.to_string());
                    } else {
                        issues.push("raw_observation_types_member_invalid".to_string());
                    }
                }
            }
            None => issues.push("raw_observation_types_not_array".to_string()),
        }
    }
    if let Some(legacy_type) = value.get("type") {
        if let Some(legacy_type) = legacy_type
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            raw.push(legacy_type.to_string());
        } else {
            issues.push("raw_observation_legacy_type_invalid".to_string());
        }
    }
    if raw.is_empty() {
        issues.push("raw_observation_capability_missing".to_string());
        return Vec::new();
    }

    let mut canonical = BTreeSet::new();
    for capability in raw {
        let mapped = canonical_capabilities(&capability);
        if mapped.is_empty() {
            issues.push("raw_observation_capability_unrecognized".to_string());
        }
        canonical.extend(mapped);
    }
    CURATED_AVIONICS_TYPES
        .iter()
        .copied()
        .filter(|capability| canonical.contains(capability))
        .map(ToString::to_string)
        .collect()
}

fn canonical_capabilities(value: &str) -> Vec<&'static str> {
    let key = normalize_name(value);
    if let Some(capability) = CURATED_AVIONICS_TYPES
        .iter()
        .copied()
        .find(|capability| normalize_name(capability) == key)
    {
        return vec![capability];
    }
    match key.as_str() {
        "comm" | "communications radio" | "communication radio" => vec!["COM"],
        "navigation receiver" | "nav receiver" => vec!["NAV"],
        "nav com"
        | "nav comm"
        | "navcom"
        | "navigation communication"
        | "navigation communications" => vec!["NAV", "COM"],
        "gps nav com"
        | "gps nav comm"
        | "gps navigation communication"
        | "gps navigation communications" => vec!["GPS", "NAV", "COM"],
        "automatic direction finder" => vec!["ADF"],
        "distance measuring equipment" | "distance measurement equipment" => vec!["DME"],
        "attitude heading reference system" | "attitude and heading reference system" => {
            vec!["AHRS"]
        }
        "adc" | "air data unit" => vec!["Air Data Computer"],
        "emergency locator transmitter" | "emergency locator beacon" => vec!["ELT"],
        "cdi"
        | "hsi"
        | "cdi hsi"
        | "course deviation indicator"
        | "horizontal situation indicator" => vec!["Navigation Indicator"],
        "stormscope" | "lightning detector" | "lightning detection system" => {
            vec!["Lightning Detection"]
        }
        "radio altimeter" => vec!["Radar Altimeter"],
        "clock" | "timer" | "chronometer" => vec!["Clock/Timer"],
        "taws" | "egpws" | "terrain awareness and warning system" => {
            vec!["Terrain Awareness"]
        }
        _ => Vec::new(),
    }
}

fn coalesce_observations(observations: Vec<RawObservation>) -> Vec<RawObservation> {
    let mut grouped = BTreeMap::<String, RawObservation>::new();
    for observation in observations {
        let key = observation.grouping_key();
        if let Some(existing) = grouped.get_mut(&key) {
            existing.quantity = existing.quantity.max(observation.quantity);
            for capability in observation.identity.capabilities {
                if !existing.identity.capabilities.contains(&capability) {
                    existing.identity.capabilities.push(capability);
                }
            }
            existing.source_evidence_text = combine_evidence(
                existing.source_evidence_text.as_deref(),
                observation.source_evidence_text.as_deref(),
            );
            existing.source_confidence = conservative_confidence(
                existing.source_confidence.as_deref(),
                observation.source_confidence.as_deref(),
            );
            existing.issues.extend(observation.issues);
            deduplicate_strings(&mut existing.issues);
        } else {
            grouped.insert(key, observation);
        }
    }
    let mut observations = grouped.into_values().collect::<Vec<_>>();
    for observation in &mut observations {
        observation.identity.capabilities.sort_by_key(|capability| {
            CURATED_AVIONICS_TYPES
                .iter()
                .position(|candidate| candidate == capability)
                .unwrap_or(usize::MAX)
        });
    }
    observations
}

fn combine_evidence(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left, right) {
        (None, None) => None,
        (Some(value), None) | (None, Some(value)) => Some(value.to_string()),
        (Some(left), Some(right)) if left == right => Some(left.to_string()),
        (Some(left), Some(right)) => Some(format!("{left}\n{right}")),
    }
}

fn conservative_confidence(left: Option<&str>, right: Option<&str>) -> Option<String> {
    let rank = |value: &str| match value {
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

fn deterministic_aspect_id(listing_id: i64, role: &str, identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ASPECT_FINGERPRINT_DOMAIN);
    hasher.update(listing_id.to_le_bytes());
    hasher.update((role.len() as u64).to_le_bytes());
    hasher.update(role.as_bytes());
    hasher.update((identity.len() as u64).to_le_bytes());
    hasher.update(identity.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("legacy-avionics-{listing_id}-{}", &digest[..20])
}

fn existing_review_owned_by_backfill(listing_id: i64, payload_json: Option<&str>) -> bool {
    let Some(payload_json) = payload_json else {
        return false;
    };
    let Ok(payload) = serde_json::from_str::<Value>(payload_json) else {
        return false;
    };
    if payload.get("version").and_then(Value::as_u64) != Some(BACKFILL_REVIEW_PAYLOAD_VERSION) {
        return false;
    }
    let Some(aspects) = payload.get("aspects").and_then(Value::as_array) else {
        return false;
    };
    if aspects.is_empty() {
        return false;
    }
    let prefix = format!("legacy-avionics-{listing_id}-");
    aspects.iter().all(|aspect| {
        let Some(id) = aspect.get("id").and_then(Value::as_str) else {
            return false;
        };
        let Some(suffix) = id.strip_prefix(&prefix) else {
            return false;
        };
        suffix.len() == 20
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn count_reasons(counts: &mut BTreeMap<String, usize>, reasons: &[String]) {
    for reason in reasons {
        *counts.entry(reason.clone()).or_insert(0) += 1;
    }
}

fn deduplicate_strings(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

async fn load_listing_sources(
    db: &AppDb,
    limit: i64,
    listing_id: Option<i64>,
) -> BackfillResult<Vec<ListingSourceRow>> {
    let predicate = if listing_id.is_some() {
        "WHERE listing.id = ?"
    } else {
        ""
    };
    let source_sql = format!(
        r#"
        SELECT
          listing.id AS listing_id,
          submission.id AS plugin_submission_id,
          submission.extracted_listing_json,
          pending.review_payload_json AS existing_review_payload_json
        FROM aircraft_sale_listings listing
        LEFT JOIN plugin_submissions submission
          ON submission.id = (
            SELECT candidate.id
            FROM plugin_submissions candidate
            WHERE candidate.user_id = listing.created_by_user_id
              AND (
                candidate.canonical_listing_id = listing.id
                OR (
                  candidate.source_url = listing.source_url
                  AND candidate.canonical_listing_id IS NULL
                )
              )
            ORDER BY
              CASE WHEN candidate.canonical_listing_id IS NOT NULL THEN 0 ELSE 1 END,
              candidate.submitted_at DESC,
              candidate.id DESC
            LIMIT 1
          )
        LEFT JOIN aircraft_sale_listing_pending_reviews pending
          ON pending.listing_id = listing.id
        {predicate}
        ORDER BY listing.id
        LIMIT ?
        "#
    );
    let sql = db.sql(&source_sql);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let query = sqlx::query_as::<_, ListingSourceRow>(&sql);
            if let Some(listing_id) = listing_id {
                query.bind(listing_id).bind(limit).fetch_all(pool).await?
            } else {
                query.bind(limit).fetch_all(pool).await?
            }
        }
        DatabaseBackend::Postgres(pool) => {
            let query = sqlx::query_as::<_, ListingSourceRow>(&sql);
            if let Some(listing_id) = listing_id {
                query.bind(listing_id).bind(limit).fetch_all(pool).await?
            } else {
                query.bind(limit).fetch_all(pool).await?
            }
        }
    };
    Ok(rows)
}

async fn load_catalog(db: &AppDb) -> BackfillResult<Vec<CatalogProduct>> {
    let sql = db.sql(
        r#"
        SELECT
          model.id,
          manufacturer.name AS manufacturer,
          model.name AS model,
          model.catalog_status,
          graph_identity.avionics_manufacturer_identity_id,
          graph_identity.canonical_product_key,
          capability.name AS capability,
          model.manufacturer_identifier_kind,
          model.manufacturer_identifier,
          model.identity_source_url,
          model.identity_source_title,
          model.identity_evidence_text
        FROM avionics_models model
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        LEFT JOIN avionics_approved_product_graph_identities graph_identity
          ON graph_identity.avionics_model_id = model.id
        LEFT JOIN avionics_model_types membership
          ON membership.avionics_model_id = model.id
        LEFT JOIN avionics_types capability
          ON capability.id = membership.avionics_type_id
        ORDER BY model.id, capability.normalized_name, capability.id
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, CatalogRow>(&sql)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, CatalogRow>(&sql)
                .fetch_all(pool)
                .await?
        }
    };
    let mut products = BTreeMap::<i64, CatalogProduct>::new();
    for row in rows {
        let product = products.entry(row.id).or_insert_with(|| CatalogProduct {
            id: row.id,
            manufacturer: row.manufacturer,
            model: row.model,
            catalog_status: row.catalog_status,
            avionics_manufacturer_identity_id: row.avionics_manufacturer_identity_id,
            canonical_product_key: row.canonical_product_key,
            capabilities: Vec::new(),
            manufacturer_identifier_kind: row.manufacturer_identifier_kind,
            manufacturer_identifier: row.manufacturer_identifier,
            identity_source_url: row.identity_source_url,
            identity_source_title: row.identity_source_title,
            identity_evidence_text: row.identity_evidence_text,
        });
        if let Some(capability) = row.capability {
            if !product.capabilities.contains(&capability) {
                product.capabilities.push(capability);
            }
        }
    }
    Ok(products.into_values().collect())
}

async fn load_listing_links(db: &AppDb, listing_id: i64) -> BackfillResult<Vec<ListingLinkRow>> {
    let sql = db.sql(
        r#"
        SELECT
          id,
          avionics_model_id,
          quantity,
          source,
          configuration_action,
          replaces_avionics_model_id,
          source_confidence
        FROM aircraft_sale_listing_avionics
        WHERE aircraft_sale_listing_id = ?
        ORDER BY id
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ListingLinkRow>(&sql)
                .bind(listing_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ListingLinkRow>(&sql)
                .bind(listing_id)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::{
        coalesce_observations, deterministic_aspect_id, existing_review_owned_by_backfill,
        listing_action_graph_issue, parse_retained_observations, prepare_listing_review,
        stage_legacy_listing_reviews, CatalogProduct, ListingLinkRow,
    };
    use crate::db::{AppDb, DatabaseBackend};
    use crate::listing::review::{
        serialize_review_payload, stage_pending_review, CoveredListingAssociation,
        ListingAssociationRole, PendingReviewAspect, ReviewProduct,
    };
    use std::collections::HashMap;

    fn product(
        id: i64,
        manufacturer: &str,
        model: &str,
        status: &str,
        capabilities: &[&str],
    ) -> CatalogProduct {
        CatalogProduct {
            id,
            manufacturer: manufacturer.to_string(),
            model: model.to_string(),
            catalog_status: status.to_string(),
            avionics_manufacturer_identity_id: (status == "approved").then_some(
                match manufacturer {
                    "Garmin" => 1,
                    "Avidyne" => 2,
                    "BendixKing" | "Bendix/King" => 3,
                    _ => 99,
                },
            ),
            canonical_product_key: (status == "approved").then(|| {
                model
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect()
            }),
            capabilities: capabilities.iter().map(|value| value.to_string()).collect(),
            manufacturer_identifier_kind: None,
            manufacturer_identifier: None,
            identity_source_url: None,
            identity_source_title: None,
            identity_evidence_text: None,
        }
    }

    async fn insert_test_listing(db: &AppDb) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test database must be SQLite");
        };
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
        sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (?, ?, 'https://broker.example/aircraft/backfill-test', 2020, 450000, 900)
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[test]
    fn parses_current_arrays_and_legacy_scalar_capabilities() {
        let raw = r#"{
          "avionics": [
            {"manufacturer":"Garmin","model":"GTN 750Xi","type":"GPS/NAV/COM","quantity":1},
            {"manufacturer":"Garmin","model":"GNX 375","types":["GPS","Transponder"],"quantity":1}
          ]
        }"#;
        let (observations, issues) = parse_retained_observations(Some(raw));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(
            observations[0].identity.capabilities,
            vec!["GPS", "NAV", "COM"]
        );
        assert_eq!(
            observations[1].identity.capabilities,
            vec!["GPS", "Transponder"]
        );
    }

    #[test]
    fn coalesces_capability_rows_for_the_same_physical_product() {
        let raw = r#"{
          "avionics": [
            {"manufacturer":"Garmin","model":"GMA-1347","type":"Audio Panel"},
            {"manufacturer":"Garmin","model":"GMA 1347","type":"COM"}
          ]
        }"#;
        let (observations, _) = parse_retained_observations(Some(raw));
        let observations = coalesce_observations(observations);
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].identity.capabilities,
            vec!["COM", "Audio Panel"]
        );
    }

    #[test]
    fn unreviewed_link_is_not_a_verified_suggestion_and_is_explicitly_covered() {
        let raw = r#"{"avionics":[{"manufacturer":"Garmin","model":"GTN 750Xi","types":["GPS","NAV","COM"]}]}"#;
        let unreviewed = product(
            7,
            "Garmin",
            "GTN 750Xi",
            "unreviewed",
            &["GPS", "NAV", "COM"],
        );
        let catalog = vec![unreviewed.clone()];
        let catalog_by_id = HashMap::from([(7, unreviewed)]);
        let links = vec![ListingLinkRow {
            id: 70,
            avionics_model_id: 7,
            quantity: 1,
            source: "listing".to_string(),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            source_confidence: Some("high".to_string()),
        }];
        let prepared = prepare_listing_review(42, Some(raw), &links, &catalog, &catalog_by_id);
        assert_eq!(prepared.aspects.len(), 1);
        assert!(prepared.aspects[0].suggested_product.is_none());
        assert!(prepared.aspects[0].proposed_product.is_some());
        assert_eq!(
            prepared.aspects[0].covered_associations,
            vec![CoveredListingAssociation {
                listing_link_id: 70,
                role: ListingAssociationRole::Installed,
                avionics_model_id: 7,
            }]
        );
        assert!(prepared.aspects[0]
            .reason
            .contains("catalog_product_unverified"));
    }

    #[test]
    fn unlinked_observation_exposes_only_a_unique_unreviewed_catalog_candidate() {
        let raw = r#"{"avionics":[{"manufacturer":"Garmin","model":"GTN 750Xi","types":["GPS","NAV","COM"]}]}"#;
        let unreviewed = product(
            7,
            "Garmin",
            "GTN 750Xi",
            "unreviewed",
            &["GPS", "NAV", "COM"],
        );
        let prepared = prepare_listing_review(
            42,
            Some(raw),
            &[],
            std::slice::from_ref(&unreviewed),
            &HashMap::from([(7, unreviewed.clone())]),
        );
        assert_eq!(prepared.aspects.len(), 1);
        assert_eq!(
            prepared.aspects[0]
                .proposed_product
                .as_ref()
                .and_then(|product| product.id),
            Some(7)
        );
        assert!(prepared.aspects[0].covered_associations.is_empty());

        let duplicate = product(
            8,
            "Garmin",
            "GTN-750Xi",
            "unreviewed",
            &["GPS", "NAV", "COM"],
        );
        let ambiguous_catalog = vec![unreviewed.clone(), duplicate.clone()];
        let ambiguous = prepare_listing_review(
            42,
            Some(raw),
            &[],
            &ambiguous_catalog,
            &HashMap::from([(7, unreviewed), (8, duplicate)]),
        );
        assert_eq!(
            ambiguous.aspects[0]
                .proposed_product
                .as_ref()
                .and_then(|product| product.id),
            None
        );

        let rejected = product(9, "Garmin", "GTN 750Xi", "rejected", &["GPS", "NAV", "COM"]);
        let rejected_match = prepare_listing_review(
            42,
            Some(raw),
            &[],
            std::slice::from_ref(&rejected),
            &HashMap::from([(9, rejected.clone())]),
        );
        assert_eq!(
            rejected_match.aspects[0]
                .proposed_product
                .as_ref()
                .and_then(|product| product.id),
            None
        );
    }

    #[test]
    fn approved_high_confidence_exact_link_needs_no_review() {
        let raw = r#"{"avionics":[{"manufacturer":"Garmin","model":"GTN 750Xi","types":["GPS","NAV","COM"]}]}"#;
        let approved = product(7, "Garmin", "GTN 750Xi", "approved", &["GPS", "NAV", "COM"]);
        let catalog = vec![approved.clone()];
        let catalog_by_id = HashMap::from([(7, approved)]);
        let links = vec![ListingLinkRow {
            id: 70,
            avionics_model_id: 7,
            quantity: 1,
            source: "listing".to_string(),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            source_confidence: Some("high".to_string()),
        }];
        let prepared = prepare_listing_review(42, Some(raw), &links, &catalog, &catalog_by_id);
        assert!(prepared.aspects.is_empty());
    }

    #[test]
    fn listing_graph_validation_accepts_pure_removal() {
        let approved = product(7, "Garmin", "GNS 430W", "approved", &["GPS"]);
        let catalog_by_id = HashMap::from([(7, approved)]);
        let links = [ListingLinkRow {
            id: 70,
            avionics_model_id: 7,
            quantity: 1,
            source: "listing".to_string(),
            configuration_action: "removes".to_string(),
            replaces_avionics_model_id: Some(7),
            source_confidence: Some("high".to_string()),
        }];

        assert_eq!(listing_action_graph_issue(&links, &catalog_by_id), None);
    }

    #[test]
    fn listing_graph_validation_detects_target_collisions_and_chains() {
        let old = product(7, "Garmin", "GNS 430W", "approved", &["GPS"]);
        let first = product(8, "Garmin", "GNC 355", "approved", &["GPS"]);
        let second = product(9, "Avidyne", "IFD 440", "approved", &["GPS"]);
        let catalog_by_id = HashMap::from([(7, old), (8, first), (9, second)]);
        let duplicate_target = [
            ListingLinkRow {
                id: 70,
                avionics_model_id: 8,
                quantity: 1,
                source: "listing".to_string(),
                configuration_action: "replaces".to_string(),
                replaces_avionics_model_id: Some(7),
                source_confidence: Some("high".to_string()),
            },
            ListingLinkRow {
                id: 71,
                avionics_model_id: 9,
                quantity: 1,
                source: "listing".to_string(),
                configuration_action: "replaces".to_string(),
                replaces_avionics_model_id: Some(7),
                source_confidence: Some("high".to_string()),
            },
        ];
        assert!(listing_action_graph_issue(&duplicate_target, &catalog_by_id).is_some());

        let chain = [
            duplicate_target[0].clone(),
            ListingLinkRow {
                id: 72,
                avionics_model_id: 9,
                quantity: 1,
                source: "listing".to_string(),
                configuration_action: "replaces".to_string(),
                replaces_avionics_model_id: Some(8),
                source_confidence: Some("high".to_string()),
            },
        ];
        assert!(listing_action_graph_issue(&chain, &catalog_by_id).is_some());
    }

    #[test]
    fn unmatched_approved_high_legacy_replacement_link_is_exactly_covered() {
        let raw = r#"{"avionics":[{"manufacturer":"Garmin","model":"GNX 375","types":["GPS","Transponder"]}]}"#;
        let installed = product(7, "Garmin", "GTN 750Xi", "approved", &["GPS", "NAV", "COM"]);
        let replaced = product(8, "Garmin", "GNS 530W", "approved", &["GPS", "NAV", "COM"]);
        let catalog = vec![installed.clone(), replaced.clone()];
        let catalog_by_id = HashMap::from([(7, installed), (8, replaced)]);
        let links = vec![ListingLinkRow {
            id: 70,
            avionics_model_id: 7,
            quantity: 1,
            source: "listing".to_string(),
            configuration_action: "replaces".to_string(),
            replaces_avionics_model_id: Some(8),
            source_confidence: Some("high".to_string()),
        }];

        let prepared = prepare_listing_review(42, Some(raw), &links, &catalog, &catalog_by_id);
        let covered = prepared
            .aspects
            .iter()
            .flat_map(|aspect| aspect.covered_associations.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            covered,
            std::collections::BTreeSet::from([
                CoveredListingAssociation {
                    listing_link_id: 70,
                    role: ListingAssociationRole::Installed,
                    avionics_model_id: 7,
                },
                CoveredListingAssociation {
                    listing_link_id: 70,
                    role: ListingAssociationRole::Replacement,
                    avionics_model_id: 8,
                },
            ])
        );
        assert!(prepared
            .aspects
            .iter()
            .all(|aspect| aspect.source_evidence_text.is_none()
                && aspect.source_confidence.is_none()));
        assert_eq!(prepared.associations_requiring_coverage, 2);
        assert_eq!(prepared.covered_association_count, 2);
        assert!(prepared.association_coverage_complete);
        assert_eq!(
            prepared
                .reason_counts
                .get("approved_high_confidence_link_unmatched_by_retained_observation"),
            Some(&1)
        );
        assert_eq!(
            prepared
                .reason_counts
                .get("approved_high_confidence_replacement_unmatched_by_retained_observation"),
            Some(&1)
        );
        serialize_review_payload(&prepared.aspects).expect("valid review payload");
    }

    #[test]
    fn unmatched_reviewer_corroborated_link_is_preserved() {
        let raw = r#"{"avionics":[{"manufacturer":"Garmin","model":"GNX 375","types":["GPS","Transponder"]}]}"#;
        let approved = product(7, "Garmin", "GTN 750Xi", "approved", &["GPS", "NAV", "COM"]);
        let catalog = vec![approved.clone()];
        let catalog_by_id = HashMap::from([(7, approved)]);
        let links = vec![ListingLinkRow {
            id: 70,
            avionics_model_id: 7,
            quantity: 1,
            source: "listing_review".to_string(),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            source_confidence: Some("high".to_string()),
        }];

        let prepared = prepare_listing_review(42, Some(raw), &links, &catalog, &catalog_by_id);
        assert_eq!(
            prepared.aspects.len(),
            1,
            "the unmatched raw item remains reviewable"
        );
        assert!(prepared.aspects[0].covered_associations.is_empty());
        assert_eq!(prepared.associations_requiring_coverage, 0);
        assert_eq!(prepared.covered_association_count, 0);
        assert!(prepared.association_coverage_complete);
        assert!(!prepared
            .reason_counts
            .contains_key("approved_high_confidence_link_unmatched_by_retained_observation"));
    }

    #[test]
    fn approved_high_legacy_link_is_preserved_without_usable_retained_observations() {
        let raw = r#"{"avionics":[]}"#;
        let approved = product(7, "Garmin", "GTN 750Xi", "approved", &["GPS", "NAV", "COM"]);
        let catalog = vec![approved.clone()];
        let catalog_by_id = HashMap::from([(7, approved)]);
        let links = vec![ListingLinkRow {
            id: 70,
            avionics_model_id: 7,
            quantity: 1,
            source: "listing".to_string(),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            source_confidence: Some("high".to_string()),
        }];

        let prepared = prepare_listing_review(42, Some(raw), &links, &catalog, &catalog_by_id);
        assert!(prepared.aspects.is_empty());
        assert_eq!(prepared.associations_requiring_coverage, 0);
        assert_eq!(prepared.covered_association_count, 0);
        assert!(prepared.association_coverage_complete);
    }

    #[test]
    fn weak_approved_association_without_retained_observation_is_explicitly_covered() {
        let raw = r#"{"avionics":[]}"#;
        let approved = product(7, "Garmin", "GTN 750Xi", "approved", &["GPS", "NAV", "COM"]);
        let catalog = vec![approved.clone()];
        let catalog_by_id = HashMap::from([(7, approved)]);
        let links = vec![ListingLinkRow {
            id: 71,
            avionics_model_id: 7,
            quantity: 1,
            source: "listing".to_string(),
            configuration_action: "installed".to_string(),
            replaces_avionics_model_id: None,
            source_confidence: Some("medium".to_string()),
        }];
        let prepared = prepare_listing_review(42, Some(raw), &links, &catalog, &catalog_by_id);
        assert_eq!(prepared.aspects.len(), 1);
        assert_eq!(
            prepared.aspects[0].covered_associations,
            vec![CoveredListingAssociation {
                listing_link_id: 71,
                role: ListingAssociationRole::Installed,
                avionics_model_id: 7,
            }]
        );
        assert!(prepared.aspects[0].suggested_product.is_some());
        assert_eq!(prepared.aspects[0].source_evidence_text, None);
        assert_eq!(prepared.aspects[0].source_confidence, None);
        assert!(prepared.association_coverage_complete);
    }

    #[test]
    fn retained_extraction_keeps_only_a_complete_evidence_confidence_pair() {
        let raw = r#"{"avionics":[{
          "manufacturer":"Garmin",
          "model":"GNX 375",
          "type":"GPS",
          "source_evidence_text":"Installed Garmin GNX 375",
          "source_confidence":"high"
        }]}"#;
        let prepared = prepare_listing_review(42, Some(raw), &[], &[], &HashMap::new());
        assert_eq!(
            prepared.aspects[0].source_evidence_text.as_deref(),
            Some("Installed Garmin GNX 375")
        );
        assert_eq!(
            prepared.aspects[0].source_confidence.as_deref(),
            Some("high")
        );

        let incomplete = r#"{"avionics":[{
          "manufacturer":"Garmin",
          "model":"GNX 375",
          "type":"GPS",
          "source_evidence_text":"Installed Garmin GNX 375"
        }]}"#;
        let prepared = prepare_listing_review(42, Some(incomplete), &[], &[], &HashMap::new());
        assert_eq!(prepared.aspects[0].source_evidence_text, None);
        assert_eq!(prepared.aspects[0].source_confidence, None);
        assert!(prepared
            .source_issues
            .iter()
            .any(|issue| { issue.contains("raw_observation_source_evidence_pair_incomplete") }));
    }

    #[test]
    fn unreviewed_replacement_target_gets_its_own_covered_aspect() {
        let raw = r#"{"avionics":[{
          "manufacturer":"Garmin","model":"GTN 750Xi","types":["GPS","NAV","COM"],
          "configuration_action":"replaces",
          "replaces":{"manufacturer":"Garmin","model":"GNS 530W","types":["GPS","NAV","COM"]}
        }]}"#;
        let installed = product(
            7,
            "Garmin",
            "GTN 750Xi",
            "unreviewed",
            &["GPS", "NAV", "COM"],
        );
        let replaced = product(
            8,
            "Garmin",
            "GNS 530W",
            "unreviewed",
            &["GPS", "NAV", "COM"],
        );
        let catalog = vec![installed.clone(), replaced.clone()];
        let catalog_by_id = HashMap::from([(7, installed), (8, replaced)]);
        let links = vec![ListingLinkRow {
            id: 70,
            avionics_model_id: 7,
            quantity: 1,
            source: "listing".to_string(),
            configuration_action: "replaces".to_string(),
            replaces_avionics_model_id: Some(8),
            source_confidence: Some("medium".to_string()),
        }];
        let prepared = prepare_listing_review(42, Some(raw), &links, &catalog, &catalog_by_id);
        assert_eq!(prepared.aspects.len(), 2);
        let primary = prepared
            .aspects
            .iter()
            .find(|aspect| aspect.configuration_action == "replaces")
            .expect("primary replacement aspect");
        let replacement = prepared
            .aspects
            .iter()
            .find(|aspect| aspect.configuration_action == "installed")
            .expect("replacement target aspect");
        assert_eq!(
            primary.covered_associations,
            vec![CoveredListingAssociation {
                listing_link_id: 70,
                role: ListingAssociationRole::Installed,
                avionics_model_id: 7,
            }]
        );
        assert_eq!(
            replacement.covered_associations,
            vec![CoveredListingAssociation {
                listing_link_id: 70,
                role: ListingAssociationRole::Replacement,
                avionics_model_id: 8,
            }]
        );
        assert_eq!(
            primary.replacement_aspect_id.as_ref(),
            Some(&replacement.id)
        );
        serialize_review_payload(&prepared.aspects).expect("valid review payload");
    }

    #[test]
    fn aspect_ids_are_deterministic_and_listing_scoped() {
        let first = deterministic_aspect_id(10, "primary", "garmin:gtn750xi:installed:");
        assert_eq!(
            first,
            deterministic_aspect_id(10, "primary", "garmin:gtn750xi:installed:")
        );
        assert_ne!(
            first,
            deterministic_aspect_id(11, "primary", "garmin:gtn750xi:installed:")
        );
    }

    #[test]
    fn review_provenance_only_accepts_this_backfills_listing_scoped_namespace() {
        let id = deterministic_aspect_id(42, "primary", "garmin:gtn750xi:installed:");
        let owned = serde_json::json!({"version": 1, "aspects": [{"id": id}]}).to_string();
        assert!(existing_review_owned_by_backfill(42, Some(&owned)));

        let live = serde_json::json!({
            "version": 1,
            "aspects": [{"id": "listing-ingestion-avionics-0"}]
        })
        .to_string();
        assert!(!existing_review_owned_by_backfill(42, Some(&live)));
        assert!(!existing_review_owned_by_backfill(41, Some(&owned)));
        assert!(!existing_review_owned_by_backfill(42, Some("not json")));
    }

    #[tokio::test]
    async fn empty_rerun_only_clears_a_backfill_owned_pending_review() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = insert_test_listing(&db).await;
        let owned_aspect = PendingReviewAspect::avionics(
            deterministic_aspect_id(listing_id, "primary", "garmin:gtn750xi:installed:"),
            "avionics",
            "Garmin GTN 750Xi",
            "Garmin GTN 750Xi [GPS]",
            "raw_observation_unlinked",
            1,
            "installed",
            None,
            None,
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GTN 750Xi",
            vec!["GPS".to_string()],
        ));
        stage_pending_review(&db, listing_id, None, &[owned_aspect])
            .await
            .unwrap();

        let preview = stage_legacy_listing_reviews(&db, false, 1, Some(listing_id))
            .await
            .unwrap();
        assert_eq!(preview.listings[0].status, "would_clear");
        assert!(!preview.listings[0].source_issues.is_empty());

        let applied = stage_legacy_listing_reviews(&db, true, 1, Some(listing_id))
            .await
            .unwrap();
        assert_eq!(applied.listings[0].status, "cleared");

        let live_aspect = PendingReviewAspect::avionics(
            "listing-ingestion-avionics-0",
            "avionics",
            "Garmin GTN 650Xi",
            "Garmin GTN 650Xi [GPS]",
            "grounding_uncertain",
            1,
            "installed",
            None,
            None,
        )
        .with_proposed_product(ReviewProduct::proposed(
            "Garmin",
            "GTN 650Xi",
            vec!["GPS".to_string()],
        ));
        stage_pending_review(&db, listing_id, None, &[live_aspect])
            .await
            .unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!();
        };
        let user_id: i64 = sqlx::query_scalar(
            "SELECT created_by_user_id FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'test-key') RETURNING id",
        )
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              canonical_listing_id
            ) VALUES (?, ?, 'https://broker.example/aircraft/backfill-test', '<html></html>',
                      'test-hash', 'test-signature',
                      '{"avionics":[{"manufacturer":"Garmin","model":"GTN 650Xi","type":"GPS"}]}', ?)
            "#,
        )
        .bind(user_id)
        .bind(install_id)
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();
        let preserved = stage_legacy_listing_reviews(&db, true, 1, Some(listing_id))
            .await
            .unwrap();
        assert_eq!(preserved.listings[0].status, "existing_review_preserved");
        assert_eq!(preserved.listings[0].pending_aspect_count, 1);
        let pending_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(pending_count, 1, "live-ingestion review must be preserved");
    }
}
