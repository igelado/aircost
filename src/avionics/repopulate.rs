use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

use crate::aircraft::faa::require_listing_admission;
use crate::avionics::catalog::{
    preview_avionics_identity, resolve_avionics_identity, ApprovedAvionicsIdentity,
    AvionicsIdentityOutcome, AvionicsIdentityRequest,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::GeminiListingExtractor;
use crate::html::clean::{clean_listing_html, clean_listing_html_with_limit};
use crate::models::ParsedAvionics;
use crate::normalize::{normalize_avionics_identifier, normalize_avionics_manufacturer_name};

const LISTING_CONTEXT_LIMIT: usize = 12_000;
const LISTING_HEADER_CONTEXT_LIMIT: usize = 2_500;
const CANDIDATE_CONTEXT_LIMIT: usize = 9_000;
const LOCAL_CLEANED_CONTEXT_LIMIT: usize = 4_000_000;

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
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsRepopulationReport {
    pub dry_run: bool,
    pub requested_limit: i64,
    pub requested_listing_id: Option<i64>,
    pub estimated_grounded_calls_before_corrections: usize,
    pub listing_extraction_calls: usize,
    pub estimated_total_gemini_calls_before_corrections: usize,
    pub grounded_call_estimate_note: String,
    pub reextraction_policy_note: String,
    pub listings: Vec<AvionicsRepopulationListingReport>,
    pub summary: AvionicsRepopulationSummary,
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
    listing_source_url: Option<String>,
    aircraft_manufacturer: String,
    aircraft_model: String,
    aircraft_variant: String,
    model_year: i64,
    old_link_count: i64,
    submission_id: Option<i64>,
    submission_canonical_listing_id: Option<i64>,
    submission_source_url: Option<String>,
    rendered_html: Option<String>,
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
}

#[derive(Default)]
struct ListingEvidenceContext {
    cleaned: String,
    lowercase: String,
    normalized: String,
    normalized_source_offsets: Vec<usize>,
}

impl ListingEvidenceContext {
    fn from_rendered_html(rendered_html: Option<&str>) -> Self {
        let Some(rendered_html) = rendered_html else {
            return Self::default();
        };
        let cleaned = clean_listing_html_with_limit(rendered_html, LOCAL_CLEANED_CONTEXT_LIMIT);
        let lowercase = cleaned.to_ascii_lowercase();
        let mut normalized = String::new();
        let mut normalized_source_offsets = Vec::new();
        for (offset, character) in cleaned.char_indices() {
            if character.is_ascii_alphanumeric() {
                normalized.push(character.to_ascii_lowercase());
                normalized_source_offsets.push(offset);
            }
        }
        Self {
            cleaned,
            lowercase,
            normalized,
            normalized_source_offsets,
        }
    }

    fn for_candidate(&self, manufacturer: &str, model: &str, raw_evidence: Option<&str>) -> String {
        if self.cleaned.is_empty() {
            return String::new();
        }
        let header = prefix_at_char_boundary(&self.cleaned, LISTING_HEADER_CONTEXT_LIMIT);
        let anchor = self
            .exact_anchor(raw_evidence.unwrap_or_default())
            .or_else(|| self.exact_anchor(&format!("{manufacturer} {model}")))
            .or_else(|| self.exact_anchor(model))
            .or_else(|| self.normalized_anchor(&format!("{manufacturer} {model}")))
            .or_else(|| self.normalized_anchor(model));
        let Some(anchor) = anchor else {
            return prefix_at_char_boundary(&self.cleaned, LISTING_CONTEXT_LIMIT).to_string();
        };
        let excerpt = excerpt_around_anchor(&self.cleaned, anchor, CANDIDATE_CONTEXT_LIMIT);
        let mut context = String::with_capacity(header.len() + excerpt.len() + 80);
        context.push_str("Stored listing header/context:\n");
        context.push_str(header);
        if excerpt != header {
            context.push_str("\nStored listing candidate evidence neighborhood:\n");
            context.push_str(excerpt);
        }
        prefix_at_char_boundary(&context, LISTING_CONTEXT_LIMIT).to_string()
    }

    fn exact_anchor(&self, value: &str) -> Option<usize> {
        let value = value.trim().to_ascii_lowercase();
        if value.is_empty() {
            None
        } else {
            self.lowercase.find(&value)
        }
    }

    fn normalized_anchor(&self, value: &str) -> Option<usize> {
        let value = value
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .map(|character| character.to_ascii_lowercase())
            .collect::<String>();
        if value.len() < 3 {
            return None;
        }
        self.normalized
            .find(&value)
            .and_then(|offset| self.normalized_source_offsets.get(offset).copied())
    }
}

fn prefix_at_char_boundary(value: &str, limit: usize) -> &str {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn excerpt_around_anchor(value: &str, anchor: usize, limit: usize) -> &str {
    if value.len() <= limit {
        return value;
    }
    let mut start = anchor.saturating_sub(limit / 4);
    while start > 0 && !value.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + limit).min(value.len());
    while end > start && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[start..end]
}

/// Rebuild listing-avionics associations from current immutable plugin
/// extraction payloads. Legacy payloads are never transformed: when they do
/// not contain capability arrays, the tool runs the current Gemini listing
/// extractor against the retained HTML and uses that transient result. Dry-run
/// still makes those extraction calls but uses the catalog's non-persisting
/// identity preview. Apply mode mutates catalog identities only through the
/// normal grounded resolver, then swaps a listing's links in one transaction;
/// signed plugin payloads are never overwritten.
pub async fn repopulate_listing_avionics(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    limit: i64,
    listing_id: Option<i64>,
) -> RepopulationResult<AvionicsRepopulationReport> {
    if limit < 1 {
        return Err(AvionicsRepopulationError::Validation(
            "limit must be at least 1".to_string(),
        ));
    }
    if listing_id.is_some_and(|listing_id| listing_id < 1) {
        return Err(AvionicsRepopulationError::Validation(
            "listing_id must be a positive integer".to_string(),
        ));
    }
    let rows = load_listing_sources(db, limit, listing_id).await?;
    if listing_id.is_some() && rows.is_empty() {
        return Err(AvionicsRepopulationError::Validation(format!(
            "listing {} was not found",
            listing_id.unwrap_or_default()
        )));
    }
    let mut catalog_statuses = load_catalog_statuses(db).await?;
    let mut listings = Vec::with_capacity(rows.len());
    for row in rows {
        listings.push(process_listing(db, extractor, apply, &row, &mut catalog_statuses).await);
    }
    let summary = summarize(&listings);
    let estimated_grounded_calls_before_corrections =
        summary.identity_resolution_attempts.saturating_mul(2);
    let listing_extraction_calls = summary.listing_reextraction_attempts;
    let estimated_total_gemini_calls_before_corrections =
        listing_extraction_calls.saturating_add(estimated_grounded_calls_before_corrections);
    Ok(AvionicsRepopulationReport {
        dry_run: !apply,
        requested_limit: limit,
        requested_listing_id: listing_id,
        estimated_grounded_calls_before_corrections,
        listing_extraction_calls,
        estimated_total_gemini_calls_before_corrections,
        grounded_call_estimate_note: "Budget approximately two grounded Gemini calls per attempted identity candidate; validation corrections can add retries."
            .to_string(),
        reextraction_policy_note: if apply {
            "Apply mode re-extracts incompatible legacy payloads from retained HTML, then persists only approved catalog identities and listing links; signed plugin payloads are never overwritten."
                .to_string()
        } else {
            "Dry-run mode still calls Gemini to re-extract incompatible legacy payloads from retained HTML, but neither the generated extraction nor catalog/listing changes are persisted."
                .to_string()
        },
        listings,
        summary,
    })
}

async fn process_listing(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    row: &ListingSourceRow,
    catalog_statuses: &mut HashMap<i64, String>,
) -> AvionicsRepopulationListingReport {
    let source_url = row
        .submission_source_url
        .clone()
        .or_else(|| row.listing_source_url.clone());
    let source_match = row.submission_id.map(|_| {
        if row.submission_canonical_listing_id == Some(row.listing_id) {
            "canonical_listing_id".to_string()
        } else {
            "source_url".to_string()
        }
    });
    let mut listing_report = AvionicsRepopulationListingReport {
        listing_id: row.listing_id,
        submission_id: row.submission_id,
        source_match,
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
        status: "blocked".to_string(),
        applied: false,
        candidates: Vec::new(),
        error: None,
    };
    if let Err(error) = require_listing_admission(db, row.listing_id).await {
        listing_report.status = "faa_rejected".to_string();
        listing_report.error = Some(error.to_string());
        return listing_report;
    }
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
            match reextract_avionics(extractor, &listing_text).await {
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
    if raw_avionics.is_empty() && row.old_link_count > 0 {
        listing_report.status = "blocked".to_string();
        listing_report.error = Some(
            "raw extraction has an empty avionics array but the listing has existing links; refusing to erase them without an explicit grounded rejection"
                .to_string(),
        );
        return listing_report;
    }
    let listing_context = ListingEvidenceContext::from_rendered_html(row.rendered_html.as_deref());
    let mut prepared: Vec<PreparedLink> = Vec::new();
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
            blocking_reasons.push(format!("candidate {candidate_index}: {issue}"));
            continue;
        }

        let primary = resolve_identity_attempt(
            db,
            extractor,
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
        let primary_status = primary.report.status.clone();
        let primary_id = primary.approved_id;
        let primary_identity_key = primary.identity_key;
        listing_report.candidates.push(primary.report);
        if primary_status == "rejected" {
            continue;
        }
        let Some(primary_id) = primary_id else {
            blocking_reasons.push(format!(
                "candidate {candidate_index} primary identity is {primary_status}"
            ));
            continue;
        };
        let Some(primary_identity_key) = primary_identity_key else {
            blocking_reasons.push(format!(
                "candidate {candidate_index} primary identity has no stable product key"
            ));
            continue;
        };

        let (replaces_avionics_model_id, replacement_identity_key) = if raw.configuration_action
            == "installed"
        {
            (None, None)
        } else {
            let replacement = raw
                .replaces
                .as_ref()
                .expect("raw_candidate_issue requires replacement identity");
            let attempt = resolve_identity_attempt(
                db,
                extractor,
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
            let replacement_status = attempt.report.status.clone();
            let replacement_id = attempt.approved_id;
            let replacement_identity_key = attempt.identity_key;
            listing_report.candidates.push(attempt.report);
            let Some(replacement_id) = replacement_id else {
                blocking_reasons.push(format!(
                    "candidate {candidate_index} required replacement identity is {replacement_status}"
                ));
                continue;
            };
            let Some(replacement_identity_key) = replacement_identity_key else {
                blocking_reasons.push(format!(
                    "candidate {candidate_index} replacement identity has no stable product key"
                ));
                continue;
            };
            if replacement_identity_key == primary_identity_key {
                if let Some(report) = listing_report.candidates.last_mut() {
                    report.status = "error".to_string();
                    report.reason = format!(
                        "catalog id {primary_id} cannot be both the installed identity and its replacement target"
                    );
                }
                blocking_reasons.push(format!(
                    "candidate {candidate_index} resolves its primary and replacement to the same catalog id {primary_id}"
                ));
                continue;
            }
            (Some(replacement_id), Some(replacement_identity_key))
        };

        let incoming_link = PreparedLink {
            identity_key: primary_identity_key.clone(),
            avionics_model_id: primary_id,
            quantity: raw.quantity,
            source_notes: raw.source_evidence_text.clone(),
            source_confidence: raw.source_confidence.clone(),
            configuration_action: raw.configuration_action.clone(),
            replaces_avionics_model_id,
            replacement_identity_key,
        };
        match merge_or_push_prepared_link(&mut prepared, incoming_link) {
            Ok(true) => {
                if let Some(report) = listing_report.candidates.iter_mut().rev().find(|report| {
                    report.candidate_index == candidate_index && report.role == "primary"
                }) {
                    report.reason.push_str(
                        "; coalesced with another independently resolved capability row for the same verified product",
                    );
                }
            }
            Ok(false) => {}
            Err(error) => {
                if let Some(report) = listing_report.candidates.iter_mut().rev().find(|report| {
                    report.candidate_index == candidate_index && report.role == "primary"
                }) {
                    report.status = "error".to_string();
                    report.reason = error.clone();
                }
                blocking_reasons.push(format!("candidate {candidate_index}: {error}"));
            }
        }
    }

    listing_report.prepared_link_count = prepared.len();
    if !blocking_reasons.is_empty() {
        listing_report.status = "blocked".to_string();
        listing_report.error = Some(blocking_reasons.join("; "));
        return listing_report;
    }
    if !apply {
        listing_report.status = "previewed".to_string();
        return listing_report;
    }
    match replace_listing_links_transactionally(db, row.listing_id, &prepared).await {
        Ok(()) => {
            listing_report.status = "applied".to_string();
            listing_report.applied = true;
        }
        Err(error) => {
            listing_report.status = "error".to_string();
            listing_report.error = Some(format!(
                "transactional link replacement failed; old links were retained: {error}"
            ));
        }
    }
    listing_report
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
    let context =
        listing_context.for_candidate(identity.manufacturer, identity.model, source_evidence_text);
    let request = AvionicsIdentityRequest {
        aircraft_manufacturer: row.aircraft_manufacturer.clone(),
        aircraft_model: row.aircraft_model.clone(),
        aircraft_variant: row.aircraft_variant.clone(),
        model_year: row.model_year,
        source_url: source_url.unwrap_or_default().to_string(),
        listing_context: context,
        requires_listing_evidence: true,
        manufacturer: identity.manufacturer.to_string(),
        model: identity.model.to_string(),
        avionics_types: identity.avionics_types.to_vec(),
        quantity: identity.quantity,
    };
    let outcome = if apply {
        resolve_avionics_identity(db, extractor, &request).await
    } else {
        preview_avionics_identity(db, extractor, &request).await
    };
    match outcome {
        Ok(AvionicsIdentityOutcome::Approved(approved)) => approved_attempt(
            apply,
            candidate_index,
            role,
            &identity,
            configuration_action,
            source_evidence_text,
            source_confidence,
            approved,
            catalog_statuses,
        ),
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
        },
    }
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
    catalog_statuses: &mut HashMap<i64, String>,
) -> IdentityAttempt {
    let identity_key = stable_approved_identity_key(&approved);
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
    }
}

fn stable_approved_identity_key(approved: &ApprovedAvionicsIdentity) -> Option<String> {
    if approved.id > 0 {
        return Some(format!("catalog:{}", approved.id));
    }
    let manufacturer = normalize_avionics_manufacturer_name(&approved.manufacturer);
    let identifier = normalize_avionics_identifier(&approved.manufacturer_identifier);
    if manufacturer.is_empty() || identifier.is_empty() {
        return None;
    }
    Some(format!("verified:{manufacturer}:{identifier}"))
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
    limit: i64,
    listing_id: Option<i64>,
) -> RepopulationResult<Vec<ListingSourceRow>> {
    let predicate = if listing_id.is_some() {
        "WHERE listing.id = ?"
    } else {
        ""
    };
    let sql = format!(
        r#"
        SELECT
          listing.id AS listing_id,
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
          submission.id AS submission_id,
          submission.canonical_listing_id AS submission_canonical_listing_id,
          submission.source_url AS submission_source_url,
          submission.rendered_html,
          submission.extracted_listing_json,
          submission.extraction_error AS submission_extraction_error
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models aircraft_model
          ON aircraft_model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers aircraft_mfr
          ON aircraft_mfr.id = aircraft_model.aircraft_manufacturer_id
        LEFT JOIN plugin_submissions submission
          ON submission.id = (
            SELECT candidate.id
            FROM plugin_submissions candidate
            WHERE (
                candidate.extracted_listing_json IS NOT NULL
                OR candidate.rendered_html IS NOT NULL
              )
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
        {predicate}
        ORDER BY listing.id
        LIMIT ?
        "#
    );
    let sql = db.sql(&sql);
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

async fn replace_listing_links_transactionally(
    db: &AppDb,
    listing_id: i64,
    links: &[PreparedLink],
) -> RepopulationResult<()> {
    let status_sql = db.sql("SELECT catalog_status FROM avionics_models WHERE id = ?");
    let delete_sql =
        db.sql("DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?");
    let insert_sql = db.sql(
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
        ) VALUES (?, ?, ?, 'listing', ?, ?, ?, ?)
        "#,
    );
    let mut required_ids = HashSet::new();
    for link in links {
        required_ids.insert(link.avionics_model_id);
        if let Some(replaced_id) = link.replaces_avionics_model_id {
            required_ids.insert(replaced_id);
        }
    }

    macro_rules! replace_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            for model_id in &required_ids {
                let status: Option<String> = sqlx::query_scalar(&status_sql)
                    .bind(model_id)
                    .fetch_optional(&mut *transaction)
                    .await?;
                if status.as_deref() != Some("approved") {
                    return Err(AvionicsRepopulationError::Validation(format!(
                        "catalog id {model_id} is not approved"
                    )));
                }
            }
            sqlx::query(&delete_sql)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            for link in links {
                sqlx::query(&insert_sql)
                    .bind(listing_id)
                    .bind(link.avionics_model_id)
                    .bind(link.quantity)
                    .bind(link.source_notes.as_deref())
                    .bind(link.source_confidence.as_deref())
                    .bind(link.configuration_action.as_str())
                    .bind(link.replaces_avionics_model_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            transaction.commit().await?;
            Ok::<(), AvionicsRepopulationError>(())
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => replace_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => replace_in_transaction!(pool),
    }
}

fn summarize(listings: &[AvionicsRepopulationListingReport]) -> AvionicsRepopulationSummary {
    let mut summary = AvionicsRepopulationSummary {
        listings_selected: listings.len(),
        ..AvionicsRepopulationSummary::default()
    };
    for listing in listings {
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

    #[tokio::test]
    async fn repopulation_rejects_before_gemini_and_link_replacement() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let listing_id = seed_listing(&db, "https://example.test/listing/faa-gate").await;
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        let report = repopulate_listing_avionics(&db, &extractor, true, 1, Some(listing_id))
            .await
            .unwrap();

        assert_eq!(report.listings.len(), 1);
        assert_eq!(report.listings[0].status, "faa_rejected");
        assert!(report.listings[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("missing_registration")));
        assert_eq!(report.summary.listings_faa_rejected, 1);
        assert_eq!(report.listing_extraction_calls, 0);
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
        assert!(context.len() <= LISTING_CONTEXT_LIMIT);
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
        };
        let transponder = ApprovedAvionicsIdentity {
            avionics_types: vec!["Transponder".to_string()],
            reason: "Verified transponder capability".to_string(),
            ..gps.clone()
        };
        let gps_key = stable_approved_identity_key(&gps).unwrap();
        let transponder_key = stable_approved_identity_key(&transponder).unwrap();
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

    #[tokio::test]
    async fn source_loader_falls_back_to_exact_listing_url() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let listing_id = seed_listing(&db, "https://example.test/listing/51").await;
        sqlx::query(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (1, 'fixture')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              extraction_error, canonical_listing_id
            ) VALUES (
              1, 1, 'https://example.test/listing/51', '<p>GTX 345R installed</p>',
              'hash', 'signature', '{"avionics":[]}',
              'downstream model-year grounding failed', NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let rows = load_listing_sources(&db, 1, Some(listing_id))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].submission_id.is_some());
        assert_eq!(rows[0].submission_canonical_listing_id, None);
        assert_eq!(
            rows[0].submission_extraction_error.as_deref(),
            Some("downstream model-year grounding failed")
        );
        assert_eq!(
            rows[0].submission_source_url.as_deref(),
            Some("https://example.test/listing/51")
        );
    }

    #[tokio::test]
    async fn source_loader_retains_html_even_when_prior_extraction_is_missing() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let listing_id = seed_listing(&db, "https://example.test/listing/52").await;
        sqlx::query(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (1, 'fixture')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              extraction_error, canonical_listing_id
            ) VALUES (
              1, 1, 'https://example.test/listing/52', '<p>Garmin GNX 375 installed</p>',
              'hash', 'signature', NULL, 'legacy extraction unavailable', ?
            )
            "#,
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();

        let rows = load_listing_sources(&db, 1, Some(listing_id))
            .await
            .unwrap();

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
    }

    #[tokio::test]
    async fn source_loader_prefers_canonical_match_over_url_fallback() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let listing_id = seed_listing(&db, "https://example.test/listing/29").await;
        sqlx::query(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (1, 'fixture')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              canonical_listing_id
            ) VALUES
              (1, 1, 'https://example.test/listing/29', '<p>fallback</p>',
               'fallback-hash', 'fallback-signature',
               '{"avionics":[{"manufacturer":"Garmin","model":"Fallback","types":["GPS"]}]}', NULL),
              (1, 1, 'https://example.test/listing/29', '<p>canonical</p>',
               'canonical-hash', 'canonical-signature',
               '{"avionics":[{"manufacturer":"Garmin","model":"Canonical","types":["GPS"]}]}', ?)
            "#,
        )
        .bind(listing_id)
        .execute(pool)
        .await
        .unwrap();

        let rows = load_listing_sources(&db, 1, Some(listing_id))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].submission_canonical_listing_id, Some(listing_id));
        let raw = parse_raw_avionics(rows[0].extracted_listing_json.as_deref().unwrap()).unwrap();
        assert_eq!(raw[0].model, "Canonical");
    }

    #[tokio::test]
    async fn failed_multi_insert_rolls_back_and_success_preserves_null_confidence() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let listing_id = seed_listing(&db, "https://example.test/listing/1").await;
        let old_id = seed_approved_avionics(&db, "Old Unit", "OLD-1").await;
        let new_id = seed_approved_avionics(&db, "New Unit", "NEW-1").await;
        sqlx::query(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id, source_confidence) VALUES (?, ?, 'high')",
        )
        .bind(listing_id)
        .bind(old_id)
        .execute(pool)
        .await
        .unwrap();
        let duplicate_links = vec![
            PreparedLink {
                identity_key: format!("catalog:{new_id}"),
                avionics_model_id: new_id,
                quantity: 1,
                source_notes: None,
                source_confidence: None,
                configuration_action: "installed".to_string(),
                replaces_avionics_model_id: None,
                replacement_identity_key: None,
            },
            PreparedLink {
                identity_key: format!("catalog:{new_id}"),
                avionics_model_id: new_id,
                quantity: 1,
                source_notes: None,
                source_confidence: None,
                configuration_action: "installed".to_string(),
                replaces_avionics_model_id: None,
                replacement_identity_key: None,
            },
        ];

        sqlx::query("UPDATE avionics_models SET catalog_status = 'unreviewed' WHERE id = ?")
            .bind(new_id)
            .execute(pool)
            .await
            .unwrap();
        assert!(
            replace_listing_links_transactionally(&db, listing_id, &duplicate_links[..1])
                .await
                .is_err()
        );
        let retained_unapproved: Vec<i64> = sqlx::query_scalar(
            "SELECT avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(retained_unapproved, vec![old_id]);
        sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
            .bind(new_id)
            .execute(pool)
            .await
            .unwrap();

        assert!(
            replace_listing_links_transactionally(&db, listing_id, &duplicate_links)
                .await
                .is_err()
        );
        let retained: Vec<i64> = sqlx::query_scalar(
            "SELECT avionics_model_id FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(retained, vec![old_id]);

        replace_listing_links_transactionally(&db, listing_id, &duplicate_links[..1])
            .await
            .unwrap();
        let stored: (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT avionics_model_id, source_notes, source_confidence FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(stored, (new_id, None, None));
    }

    async fn seed_listing(db: &AppDb, source_url: &str) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Cessna', 'cessna')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) VALUES (1, '182', '182')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) VALUES (1, '182T', '182t')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (1, 1, ?, 2020, 300000, 1000)
            RETURNING id
            "#,
        )
        .bind(source_url)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn seed_approved_avionics(db: &AppDb, name: &str, identifier: &str) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Garmin', 'garmin') ON CONFLICT (normalized_name) DO NOTHING",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('GPS', 'gps') ON CONFLICT (normalized_name) DO NOTHING",
        )
        .execute(pool)
        .await
        .unwrap();
        let normalized_name = name.to_ascii_lowercase().replace(' ', "");
        let normalized_identifier = identifier.to_ascii_lowercase().replace('-', "");
        let model_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text, identity_evidence_kind,
              identity_confidence, catalog_reviewed_at
            ) VALUES (
              1, ?, ?, 'manufacturer_model_number', ?, ?,
              'https://www.garmin.com/aviation/test-product/', 'Garmin product',
              'Manufacturer reference identifies this exact product.',
              'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
            ) RETURNING id
            "#,
        )
        .bind(name)
        .bind(normalized_name)
        .bind(identifier)
        .bind(normalized_identifier)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, 1)",
        )
        .bind(model_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        model_id
    }
}
