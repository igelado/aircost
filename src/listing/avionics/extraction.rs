//! Strict current-schema avionics extraction boundary.
//!
//! The retained extraction is derived data, but it is usable only while its
//! exact signed source capture remains bound to the same owner, listing, and
//! source URL. This module deliberately has no legacy parser or scalar
//! capability fallback.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::extract::CURATED_AVIONICS_TYPES;
#[cfg(test)]
use crate::html::clean::listing_body_contains_exact_structurally_visible_text_span;
use crate::html::clean::normalize_source_evidence_span;
use crate::html::listing::source::listing_evidence_units;
use crate::listing::evidence::{
    controller_avionics_evidence, identity_span_has_boundaries, ListingEvidenceContext,
    MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES,
};
use crate::models::ParsedAvionics;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AvionicsValidationClass {
    Schema,
    Evidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AvionicsValidationRule {
    InvalidExtractionJson,
    MissingAvionicsArray,
    OccurrenceNotObject,
    MissingManufacturer,
    MissingModel,
    InvalidTypes,
    MissingQuantity,
    InvalidQuantity,
    MissingConfigurationAction,
    InvalidConfigurationAction,
    MissingReplacement,
    UnexpectedReplacement,
    MissingReplacementObject,
    MissingSourceEvidence,
    SourceEvidenceTooLong,
    MissingSourceConfidence,
    InvalidSourceConfidence,
    InvalidOccurrenceSchema,
    EvidenceSourceInvalid,
    SourceEvidenceNotVisible,
    CandidateIdentityNotInEvidence,
    ReplacementIdentityNotInEvidence,
    SuiteCapabilityNotExplicit,
    SuiteCapabilityCollision,
    QuantityProofAmbiguous,
    QuantityMismatch,
    QuantityEvidenceIncomplete,
    InvalidBindingIdentifiers,
    OwnerBindingMismatch,
    CanonicalListingBindingMismatch,
    MissingListingSourceUrl,
    SourceUrlBindingMismatch,
    MissingRenderedHtml,
    RenderedHtmlDigestMismatch,
    MissingExtractionJson,
}

impl AvionicsValidationRule {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidExtractionJson => "invalid_extraction_json",
            Self::MissingAvionicsArray => "missing_avionics_array",
            Self::OccurrenceNotObject => "occurrence_not_object",
            Self::MissingManufacturer => "missing_manufacturer",
            Self::MissingModel => "missing_model",
            Self::InvalidTypes => "invalid_types",
            Self::MissingQuantity => "missing_quantity",
            Self::InvalidQuantity => "invalid_quantity",
            Self::MissingConfigurationAction => "missing_configuration_action",
            Self::InvalidConfigurationAction => "invalid_configuration_action",
            Self::MissingReplacement => "missing_replacement",
            Self::UnexpectedReplacement => "unexpected_replacement",
            Self::MissingReplacementObject => "missing_replacement_object",
            Self::MissingSourceEvidence => "missing_source_evidence",
            Self::SourceEvidenceTooLong => "source_evidence_too_long",
            Self::MissingSourceConfidence => "missing_source_confidence",
            Self::InvalidSourceConfidence => "invalid_source_confidence",
            Self::InvalidOccurrenceSchema => "invalid_occurrence_schema",
            Self::EvidenceSourceInvalid => "evidence_source_invalid",
            Self::SourceEvidenceNotVisible => "source_evidence_not_visible",
            Self::CandidateIdentityNotInEvidence => "candidate_identity_not_in_evidence",
            Self::ReplacementIdentityNotInEvidence => "replacement_identity_not_in_evidence",
            Self::SuiteCapabilityNotExplicit => "suite_capability_not_explicit",
            Self::SuiteCapabilityCollision => "suite_capability_collision",
            Self::QuantityProofAmbiguous => "quantity_proof_ambiguous",
            Self::QuantityMismatch => "quantity_mismatch",
            Self::QuantityEvidenceIncomplete => "quantity_evidence_incomplete",
            Self::InvalidBindingIdentifiers => "invalid_binding_identifiers",
            Self::OwnerBindingMismatch => "owner_binding_mismatch",
            Self::CanonicalListingBindingMismatch => "canonical_listing_binding_mismatch",
            Self::MissingListingSourceUrl => "missing_listing_source_url",
            Self::SourceUrlBindingMismatch => "source_url_binding_mismatch",
            Self::MissingRenderedHtml => "missing_rendered_html",
            Self::RenderedHtmlDigestMismatch => "rendered_html_digest_mismatch",
            Self::MissingExtractionJson => "missing_extraction_json",
        }
    }

    pub(crate) const fn class(self) -> AvionicsValidationClass {
        match self {
            Self::EvidenceSourceInvalid
            | Self::SourceEvidenceNotVisible
            | Self::CandidateIdentityNotInEvidence
            | Self::ReplacementIdentityNotInEvidence
            | Self::SuiteCapabilityNotExplicit
            | Self::SuiteCapabilityCollision
            | Self::QuantityProofAmbiguous
            | Self::QuantityMismatch
            | Self::QuantityEvidenceIncomplete
            | Self::OwnerBindingMismatch
            | Self::CanonicalListingBindingMismatch
            | Self::MissingListingSourceUrl
            | Self::SourceUrlBindingMismatch
            | Self::MissingRenderedHtml
            | Self::RenderedHtmlDigestMismatch
            | Self::MissingExtractionJson => AvionicsValidationClass::Evidence,
            _ => AvionicsValidationClass::Schema,
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::InvalidExtractionJson => "retained listing extraction is invalid JSON",
            Self::MissingAvionicsArray => {
                "retained listing extraction has no top-level avionics array"
            }
            Self::OccurrenceNotObject => "avionics occurrence must be an object",
            Self::MissingManufacturer => "manufacturer must be a non-empty string",
            Self::MissingModel => "model must be a non-empty string",
            Self::InvalidTypes => {
                "types must be a non-empty array of distinct current curated capabilities, or Unknown by itself; scalar type payloads are intentionally unsupported"
            }
            Self::MissingQuantity => "quantity must be an explicit integer",
            Self::InvalidQuantity => "quantity must be at least 1",
            Self::MissingConfigurationAction => {
                "configuration_action must be an explicit installed, replaces, or removes value"
            }
            Self::InvalidConfigurationAction => {
                "configuration_action must be installed, replaces, or removes"
            }
            Self::MissingReplacement => {
                "replaces must be explicit null or one replacement object"
            }
            Self::UnexpectedReplacement => "installed occurrence must use replaces=null",
            Self::MissingReplacementObject => {
                "replaces or removes occurrence requires one replacement object"
            }
            Self::MissingSourceEvidence => {
                "source_evidence_text must be one non-empty exact listing-source excerpt"
            }
            Self::SourceEvidenceTooLong => {
                "source_evidence_text exceeds the bounded listing-evidence limit"
            }
            Self::MissingSourceConfidence => {
                "source_confidence must be high, medium, or low"
            }
            Self::InvalidSourceConfidence => {
                "source_confidence must be high, medium, or low"
            }
            Self::InvalidOccurrenceSchema => {
                "avionics occurrence does not satisfy the current schema"
            }
            Self::EvidenceSourceInvalid => "listing evidence source is invalid",
            Self::SourceEvidenceNotVisible => {
                "source_evidence_text is not one exact structurally visible source unit in the retained capture"
            }
            Self::CandidateIdentityNotInEvidence => {
                "source_evidence_text is not one exact bounded source excerpt containing the candidate identity"
            }
            Self::ReplacementIdentityNotInEvidence => {
                "source_evidence_text does not contain the exact replacement identity"
            }
            Self::SuiteCapabilityNotExplicit => {
                "integrated suite capability lacks explicit support in source_evidence_text"
            }
            Self::SuiteCapabilityCollision => {
                "integrated suite assigns a capability to a separate product"
            }
            Self::QuantityProofAmbiguous => {
                "ambiguous Controller quantity evidence cannot have high source confidence"
            }
            Self::QuantityMismatch => {
                "occurrence duplicates an earlier normalized manufacturer/model identity, or quantity greater than one cannot have high source confidence"
            }
            Self::QuantityEvidenceIncomplete => {
                "source_evidence_text does not cover the complete bounded Controller quantity ambiguity"
            }
            Self::InvalidBindingIdentifiers => {
                "listing and retained submission IDs must be positive"
            }
            Self::OwnerBindingMismatch => {
                "retained submission does not belong to the listing owner"
            }
            Self::CanonicalListingBindingMismatch => {
                "retained submission is not bound to the exact canonical listing"
            }
            Self::MissingListingSourceUrl => {
                "listing has no source URL for its retained extraction"
            }
            Self::SourceUrlBindingMismatch => {
                "retained submission source URL does not exactly match the listing source URL"
            }
            Self::MissingRenderedHtml => "retained submission has no rendered HTML",
            Self::RenderedHtmlDigestMismatch => {
                "retained submission rendered HTML failed its SHA-256 binding"
            }
            Self::MissingExtractionJson => {
                "retained submission has no extracted listing JSON"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AvionicsValidationField {
    Occurrence,
    Manufacturer,
    Model,
    Types,
    Quantity,
    ConfigurationAction,
    Replaces,
    ReplacementManufacturer,
    ReplacementModel,
    ReplacementTypes,
    SourceEvidenceText,
    SourceConfidence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AvionicsValidationFailure {
    rule: AvionicsValidationRule,
    occurrence_index: Option<usize>,
    field: Option<AvionicsValidationField>,
    related_occurrence_index: Option<usize>,
}

impl AvionicsValidationFailure {
    fn global(rule: AvionicsValidationRule) -> Self {
        Self {
            rule,
            occurrence_index: None,
            field: None,
            related_occurrence_index: None,
        }
    }

    fn occurrence(
        rule: AvionicsValidationRule,
        occurrence_index: usize,
        field: AvionicsValidationField,
    ) -> Self {
        Self {
            rule,
            occurrence_index: Some(occurrence_index),
            field: Some(field),
            related_occurrence_index: None,
        }
    }

    fn related(
        rule: AvionicsValidationRule,
        occurrence_index: usize,
        field: AvionicsValidationField,
        related_occurrence_index: usize,
    ) -> Self {
        Self {
            rule,
            occurrence_index: Some(occurrence_index),
            field: Some(field),
            related_occurrence_index: Some(related_occurrence_index),
        }
    }

    pub(crate) const fn rule(&self) -> AvionicsValidationRule {
        self.rule
    }

    pub(crate) fn path(&self) -> Option<String> {
        let index = self.occurrence_index?;
        let suffix = match self.field? {
            AvionicsValidationField::Occurrence => "",
            AvionicsValidationField::Manufacturer => ".manufacturer",
            AvionicsValidationField::Model => ".model",
            AvionicsValidationField::Types => ".types",
            AvionicsValidationField::Quantity => ".quantity",
            AvionicsValidationField::ConfigurationAction => ".configuration_action",
            AvionicsValidationField::Replaces => ".replaces",
            AvionicsValidationField::ReplacementManufacturer => ".replaces.manufacturer",
            AvionicsValidationField::ReplacementModel => ".replaces.model",
            AvionicsValidationField::ReplacementTypes => ".replaces.types",
            AvionicsValidationField::SourceEvidenceText => ".source_evidence_text",
            AvionicsValidationField::SourceConfidence => ".source_confidence",
        };
        Some(format!("avionics[{index}]{suffix}"))
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }
}

impl fmt::Display for AvionicsValidationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = self.path() {
            write!(formatter, "{path} {}", self.rule.description())?;
        } else {
            formatter.write_str(self.rule.description())?;
        }
        if let Some(index) = self.related_occurrence_index {
            write!(formatter, "; related occurrence avionics[{index}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for AvionicsValidationFailure {}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CurrentAvionicsExtraction<'a> {
    pub listing_id: i64,
    pub listing_owner_user_id: i64,
    pub listing_source_url: Option<&'a str>,
    pub submission_id: i64,
    pub submission_owner_user_id: i64,
    pub submission_canonical_listing_id: Option<i64>,
    pub submission_source_url: &'a str,
    pub rendered_html: &'a str,
    pub rendered_html_sha256: &'a str,
    pub extracted_listing_json: &'a str,
}

pub(crate) fn validate_current_avionics_extraction(
    extraction: CurrentAvionicsExtraction<'_>,
) -> Result<Vec<ParsedAvionics>, AvionicsValidationFailure> {
    validate_capture_binding(extraction)?;
    let observations = parse_current_avionics_extraction_json(extraction.extracted_listing_json)?;
    let listing_context =
        ListingEvidenceContext::from_rendered_html(Some(extraction.rendered_html));
    validate_current_avionics_observations(
        &observations,
        &listing_context,
        extraction.submission_source_url,
        extraction.rendered_html,
    )?;
    Ok(observations)
}

/// Validate an extraction before a canonical listing exists. This is the
/// extraction-only checkpoint shared by ordinary ingestion and clean replay.
/// Capture ownership and canonical binding are checked later, when the
/// checkpoint is attached to a listing.
pub(crate) fn validate_unbound_current_avionics_extraction(
    extracted_listing_json: &str,
    source_url: &str,
    rendered_html: &str,
) -> Result<Vec<ParsedAvionics>, AvionicsValidationFailure> {
    let observations = parse_current_avionics_extraction_json(extracted_listing_json)?;
    let listing_context = ListingEvidenceContext::from_rendered_html(Some(rendered_html));
    validate_current_avionics_observations(
        &observations,
        &listing_context,
        source_url,
        rendered_html,
    )?;
    Ok(observations)
}

/// Apply the complete semantic contract to already parsed current-schema
/// observations. Callers that construct observations from another current
/// representation, such as a pending-review payload, use this boundary so
/// identity evidence, capability scope, and quantity completeness cannot
/// drift apart across replay paths.
pub(crate) fn validate_current_avionics_observations(
    observations: &[ParsedAvionics],
    listing_context: &ListingEvidenceContext,
    source_url: &str,
    rendered_html: &str,
) -> Result<(), AvionicsValidationFailure> {
    validate_current_avionics_identity_evidence(
        observations,
        listing_context,
        source_url,
        rendered_html,
    )?;
    validate_current_avionics_type_scope(observations)?;
    validate_current_avionics_quantity_completeness(observations, source_url, rendered_html)
}

/// Apply the narrow Controller evidence-typography repair atomically. Quantity
/// remains model-produced and is validated without mutation.
pub(crate) fn recover_controller_avionics_extraction(
    extracted_listing: &mut Value,
    source_url: &str,
    rendered_html: &str,
) -> Result<bool, AvionicsValidationFailure> {
    let original = extracted_listing.clone();
    let recovery = (|| {
        let evidence_recovered = apply_controller_avionics_evidence_typography(
            extracted_listing,
            source_url,
            rendered_html,
        )?;
        if evidence_recovered {
            validate_unbound_current_avionics_extraction(
                &extracted_listing.to_string(),
                source_url,
                rendered_html,
            )?;
        }
        Ok(evidence_recovered)
    })();
    if recovery.is_err() {
        *extracted_listing = original;
    }
    recovery
}

/// Replace only typography-drifted occurrence evidence with an exact visible
/// span copied from one trusted Controller Avionics/Radios field.
///
/// This is deliberately an extraction-boundary repair, not product
/// normalization. The model-produced evidence remains the identity-complete
/// locator: after lowercasing and removing non-alphanumeric characters, a
/// source span must be exactly equal to that locator. Every identity and
/// action field stays model-produced and is revalidated unchanged.
fn apply_controller_avionics_evidence_typography(
    extracted_listing: &mut Value,
    source_url: &str,
    rendered_html: &str,
) -> Result<bool, AvionicsValidationFailure> {
    let observations = parse_current_avionics_extraction_value(extracted_listing)?;
    let listing_context = ListingEvidenceContext::from_rendered_html(Some(rendered_html));
    let evidence_units = listing_evidence_units(source_url, rendered_html).map_err(|_| {
        AvionicsValidationFailure::global(AvionicsValidationRule::EvidenceSourceInvalid)
    })?;
    let controller_field = controller_avionics_evidence(source_url, rendered_html);
    let invalid_occurrence = observations.iter().enumerate().any(|(index, observation)| {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence");
        !evidence_units.contains_exact_span(evidence)
            || validate_current_avionics_identity_evidence_occurrence(
                observation,
                index,
                &listing_context,
                evidence,
                controller_field.as_deref(),
            )
            .is_err()
    });
    if !invalid_occurrence {
        return Ok(false);
    }

    let Some(controller_field) = controller_field else {
        return Ok(false);
    };
    let full_source = ListingEvidenceContext::from_cleaned_text(&controller_field);
    let mut replacements = Vec::new();
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence");
        if evidence_units.contains_exact_span(evidence)
            && validate_current_avionics_identity_evidence_occurrence(
                observation,
                index,
                &listing_context,
                evidence,
                Some(&controller_field),
            )
            .is_ok()
        {
            continue;
        }
        if !context_has_exact_identity(&full_source, &observation.manufacturer, &observation.model)
            || observation.replaces.as_ref().is_some_and(|replacement| {
                !context_has_exact_identity(
                    &full_source,
                    &replacement.manufacturer,
                    &replacement.model,
                )
            })
        {
            return Ok(false);
        }

        let candidates = typography_equivalent_visible_spans(&controller_field, evidence)
            .into_iter()
            .filter(|candidate| evidence_units.contains_exact_span(candidate))
            .filter(|candidate| {
                let candidate_context = ListingEvidenceContext::from_cleaned_text(*candidate);
                context_has_exact_identity(
                    &candidate_context,
                    &observation.manufacturer,
                    &observation.model,
                ) && observation.replaces.as_ref().is_none_or(|replacement| {
                    context_has_exact_identity(
                        &candidate_context,
                        &replacement.manufacturer,
                        &replacement.model,
                    )
                }) && controller_multiline_candidate_matches_observation(candidate, observation)
            })
            .collect::<Vec<_>>();
        if candidates
            .iter()
            .any(|candidate| candidate.contains(['\r', '\n']))
            && candidates.len() != 1
        {
            return Ok(false);
        }
        let candidates = candidates.into_iter().collect::<BTreeSet<_>>();
        let mut candidates = candidates.into_iter();
        let Some(candidate) = candidates.next() else {
            return Ok(false);
        };
        if candidates.next().is_some() {
            return Ok(false);
        }
        replacements.push((index, candidate.to_string()));
    }

    if replacements.is_empty() {
        return Ok(false);
    }
    let avionics = extracted_listing
        .get_mut("avionics")
        .and_then(Value::as_array_mut)
        .expect("the canonical parser requires a top-level avionics array");
    for (index, evidence) in replacements {
        avionics[index]
            .as_object_mut()
            .expect("the canonical parser requires avionics objects")
            .insert("source_evidence_text".to_string(), Value::String(evidence));
    }

    Ok(true)
}

fn typography_equivalent_visible_spans<'a>(source: &'a str, hint: &str) -> Vec<&'a str> {
    let hint = normalize_evidence_typography(hint);
    if hint.is_empty() {
        return Vec::new();
    }
    let mut normalized = String::new();
    let mut source_offsets = Vec::new();
    for (offset, character) in source.char_indices() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            source_offsets.push(offset);
        }
    }
    normalized
        .match_indices(&hint)
        .filter_map(|(normalized_start, _)| {
            let source_start = source_offsets.get(normalized_start).copied()?;
            let normalized_last = normalized_start.checked_add(hint.len())?.checked_sub(1)?;
            let source_last = source_offsets.get(normalized_last).copied()?;
            let source_end = source_last + source[source_last..].chars().next()?.len_utf8();
            let candidate = &source[source_start..source_end];
            (candidate.len() <= MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES
                && identity_span_has_boundaries(source, source_start, source_end))
            .then_some(candidate)
        })
        .collect()
}

/// Admit only one deterministic Controller line-wrap shape. A wrapped
/// installed occurrence must retain exactly the declared number of product
/// identities, and every continuation must split between that identity and a
/// declared capability. Product/action boundaries therefore remain ineligible
/// even though generic rendered-text cleanup would flatten them to spaces.
fn controller_multiline_candidate_matches_observation(
    candidate: &str,
    observation: &ParsedAvionics,
) -> bool {
    if !candidate.contains(['\r', '\n']) {
        return true;
    }
    if observation.configuration_action != "installed"
        || observation.replaces.is_some()
        || observation.quantity < 1
        || normalized_identity_occurrence_ranges(candidate, &observation.model).len()
            != observation.quantity as usize
    {
        return false;
    }

    let normalized_model = normalize_evidence_typography(&observation.model);
    let normalized_types = observation
        .avionics_types
        .iter()
        .map(|avionics_type| normalize_evidence_typography(avionics_type))
        .collect::<BTreeSet<_>>();
    let lines = candidate
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    lines.len() >= 2
        && lines.windows(2).all(|pair| {
            normalize_evidence_typography(pair[0]).ends_with(&normalized_model)
                && pair[1]
                    .split(|character: char| !character.is_ascii_alphanumeric())
                    .find(|token| !token.is_empty())
                    .map(normalize_evidence_typography)
                    .is_some_and(|token| normalized_types.contains(&token))
        })
}

fn normalize_evidence_typography(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

pub(crate) fn parse_current_avionics_extraction_json(
    extracted_listing_json: &str,
) -> Result<Vec<ParsedAvionics>, AvionicsValidationFailure> {
    let value: Value = serde_json::from_str(extracted_listing_json).map_err(|_| {
        AvionicsValidationFailure::global(AvionicsValidationRule::InvalidExtractionJson)
    })?;
    parse_current_avionics_extraction_value(&value)
}

pub(crate) fn parse_current_avionics_extraction_value(
    value: &Value,
) -> Result<Vec<ParsedAvionics>, AvionicsValidationFailure> {
    let observations = value
        .get("avionics")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AvionicsValidationFailure::global(AvionicsValidationRule::MissingAvionicsArray)
        })?;
    observations
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let object = value.as_object().ok_or_else(|| {
                AvionicsValidationFailure::occurrence(
                    AvionicsValidationRule::OccurrenceNotObject,
                    index,
                    AvionicsValidationField::Occurrence,
                )
            })?;
            validate_required_identity(object, index, false)?;
            validate_capabilities(value, index, false)?;
            let quantity = object
                .get("quantity")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    AvionicsValidationFailure::occurrence(
                        AvionicsValidationRule::MissingQuantity,
                        index,
                        AvionicsValidationField::Quantity,
                    )
                })?;
            if quantity < 1 {
                return Err(AvionicsValidationFailure::occurrence(
                    AvionicsValidationRule::InvalidQuantity,
                    index,
                    AvionicsValidationField::Quantity,
                ));
            }
            let action = object
                .get("configuration_action")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AvionicsValidationFailure::occurrence(
                        AvionicsValidationRule::MissingConfigurationAction,
                        index,
                        AvionicsValidationField::ConfigurationAction,
                    )
                })?;
            if !matches!(action, "installed" | "replaces" | "removes") {
                return Err(AvionicsValidationFailure::occurrence(
                    AvionicsValidationRule::InvalidConfigurationAction,
                    index,
                    AvionicsValidationField::ConfigurationAction,
                ));
            }
            let replacement = object.get("replaces").ok_or_else(|| {
                AvionicsValidationFailure::occurrence(
                    AvionicsValidationRule::MissingReplacement,
                    index,
                    AvionicsValidationField::Replaces,
                )
            })?;
            match action {
                "installed" if !replacement.is_null() => {
                    return Err(AvionicsValidationFailure::occurrence(
                        AvionicsValidationRule::UnexpectedReplacement,
                        index,
                        AvionicsValidationField::Replaces,
                    ));
                }
                "replaces" | "removes" if !replacement.is_object() => {
                    return Err(AvionicsValidationFailure::occurrence(
                        AvionicsValidationRule::MissingReplacementObject,
                        index,
                        AvionicsValidationField::Replaces,
                    ));
                }
                "replaces" | "removes" => {
                    validate_required_identity(
                        replacement
                            .as_object()
                            .expect("replacement object was checked"),
                        index,
                        true,
                    )?;
                    validate_capabilities(replacement, index, true)?;
                }
                _ => {}
            }

            let evidence = object
                .get("source_evidence_text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|evidence| !evidence.is_empty())
                .ok_or_else(|| {
                    AvionicsValidationFailure::occurrence(
                        AvionicsValidationRule::MissingSourceEvidence,
                        index,
                        AvionicsValidationField::SourceEvidenceText,
                    )
                })?;
            if evidence.len() > MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES {
                return Err(AvionicsValidationFailure::occurrence(
                    AvionicsValidationRule::SourceEvidenceTooLong,
                    index,
                    AvionicsValidationField::SourceEvidenceText,
                ));
            }
            let confidence = object
                .get("source_confidence")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AvionicsValidationFailure::occurrence(
                        AvionicsValidationRule::MissingSourceConfidence,
                        index,
                        AvionicsValidationField::SourceConfidence,
                    )
                })?;
            if !matches!(confidence, "high" | "medium" | "low") {
                return Err(AvionicsValidationFailure::occurrence(
                    AvionicsValidationRule::InvalidSourceConfidence,
                    index,
                    AvionicsValidationField::SourceConfidence,
                ));
            }

            serde_json::from_value::<ParsedAvionics>(value.clone()).map_err(|_| {
                AvionicsValidationFailure::occurrence(
                    AvionicsValidationRule::InvalidOccurrenceSchema,
                    index,
                    AvionicsValidationField::Occurrence,
                )
            })
        })
        .collect()
}

pub(crate) fn validate_current_avionics_identity_evidence(
    observations: &[ParsedAvionics],
    listing_context: &ListingEvidenceContext,
    source_url: &str,
    rendered_html: &str,
) -> Result<(), AvionicsValidationFailure> {
    let evidence_units = listing_evidence_units(source_url, rendered_html).map_err(|_| {
        AvionicsValidationFailure::global(AvionicsValidationRule::EvidenceSourceInvalid)
    })?;
    let controller_field = controller_avionics_evidence(source_url, rendered_html);
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence")
            .trim();
        if !evidence_units.contains_exact_span(evidence) {
            return Err(AvionicsValidationFailure::occurrence(
                AvionicsValidationRule::SourceEvidenceNotVisible,
                index,
                AvionicsValidationField::SourceEvidenceText,
            ));
        }
        validate_current_avionics_identity_evidence_occurrence(
            observation,
            index,
            listing_context,
            evidence,
            controller_field.as_deref(),
        )?;
    }
    Ok(())
}

fn validate_current_avionics_type_scope(
    observations: &[ParsedAvionics],
) -> Result<(), AvionicsValidationFailure> {
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence");
        validate_occurrence_type_scope(index, false, &observation.avionics_types, evidence)?;
        if let Some(replacement) = observation.replaces.as_ref() {
            validate_occurrence_type_scope(index, true, &replacement.avionics_types, evidence)?;
        }
    }

    for (suite_index, suite) in observations.iter().enumerate().filter(|(_, observation)| {
        observation
            .avionics_types
            .iter()
            .any(|capability| capability == "Integrated Flight Deck")
    }) {
        for (component_index, component) in observations.iter().enumerate() {
            if suite_index == component_index || same_product_identity(suite, component) {
                continue;
            }
            let duplicated = suite
                .avionics_types
                .iter()
                .filter(|capability| capability.as_str() != "Integrated Flight Deck")
                .find(|capability| component.avionics_types.contains(capability));
            if duplicated.is_some() {
                return Err(AvionicsValidationFailure::related(
                    AvionicsValidationRule::SuiteCapabilityCollision,
                    suite_index,
                    AvionicsValidationField::Types,
                    component_index,
                ));
            }
        }
    }
    Ok(())
}

fn validate_occurrence_type_scope(
    index: usize,
    replacement: bool,
    avionics_types: &[String],
    evidence: &str,
) -> Result<(), AvionicsValidationFailure> {
    let integrated_suite = avionics_types
        .iter()
        .any(|capability| capability == "Integrated Flight Deck");
    let unsupported_suite_capability = integrated_suite
        && avionics_types.iter().any(|capability| {
            capability != "Integrated Flight Deck"
                && !evidence_explicitly_names_capability(evidence, capability)
        });
    if unsupported_suite_capability {
        return Err(AvionicsValidationFailure::occurrence(
            AvionicsValidationRule::SuiteCapabilityNotExplicit,
            index,
            if replacement {
                AvionicsValidationField::ReplacementTypes
            } else {
                AvionicsValidationField::Types
            },
        ));
    }
    Ok(())
}

fn evidence_explicitly_names_capability(evidence: &str, capability: &str) -> bool {
    let evidence = normalize_source_evidence_span(evidence);
    capability_evidence_phrases(capability)
        .iter()
        .any(|phrase| exact_normalized_phrase(&evidence, phrase))
}

fn exact_normalized_phrase(source: &str, phrase: &str) -> bool {
    source.match_indices(phrase).any(|(start, matched)| {
        let end = start + matched.len();
        (start == 0 || source.as_bytes()[start - 1] == b' ')
            && (end == source.len() || source.as_bytes()[end] == b' ')
    })
}

fn capability_evidence_phrases(capability: &str) -> &'static [&'static str] {
    match capability {
        "GPS" => &["gps", "gnss", "satellite navigation"],
        "NAV" => &["nav", "navigation", "vor", "localizer", "glideslope"],
        "COM" => &["com", "comm", "communication"],
        "Transponder" => &["transponder"],
        "Autopilot" => &["autopilot", "auto pilot"],
        "Flight Director" => &["flight director"],
        "Integrated Flight Deck" => &["integrated flight deck", "flight deck", "avionics suite"],
        "Audio Panel" => &["audio panel", "audio controller"],
        "Flight Display" => &["flight display", "pfd", "mfd"],
        "Navigation Indicator" => &["navigation indicator", "cdi", "hsi"],
        "Traffic" => &["traffic", "tcas", "tas"],
        "Datalink" => &[
            "datalink",
            "data link",
            "ads b",
            "fis b",
            "siriusxm",
            "sirius xm",
        ],
        "Weather Radar" => &["weather radar"],
        "Lightning Detection" => &["lightning", "stormscope"],
        "Terrain Awareness" => &["terrain", "taws", "egpws", "gpws"],
        "Engine Monitor" => &[
            "engine monitor",
            "engine monitoring",
            "engine indicating display",
        ],
        "Standby Instrument" => &["standby instrument", "backup instrument"],
        "ELT" => &["elt", "emergency locator transmitter"],
        "ADF" => &["adf", "automatic direction finder"],
        "DME" => &["dme", "distance measuring equipment"],
        "AHRS" => &["ahrs", "attitude heading reference"],
        "Air Data Computer" => &["air data computer", "adc"],
        "Radar Altimeter" => &["radar altimeter", "radio altimeter"],
        "Magnetometer" => &["magnetometer"],
        "Clock/Timer" => &["clock timer", "clock", "timer"],
        _ => &[],
    }
}

pub(crate) fn validate_current_avionics_quantity_completeness(
    observations: &[ParsedAvionics],
    source_url: &str,
    rendered_html: &str,
) -> Result<(), AvionicsValidationFailure> {
    for (index, observation) in observations.iter().enumerate() {
        if let Some(related_index) = observations[..index]
            .iter()
            .position(|candidate| same_product_identity(observation, candidate))
        {
            return Err(AvionicsValidationFailure::related(
                AvionicsValidationRule::QuantityMismatch,
                index,
                AvionicsValidationField::Quantity,
                related_index,
            ));
        }
        if observation.quantity > 1 && observation.source_confidence.as_deref() == Some("high") {
            return Err(AvionicsValidationFailure::occurrence(
                AvionicsValidationRule::QuantityMismatch,
                index,
                AvionicsValidationField::Quantity,
            ));
        }
    }
    let Some(installed_equipment) = controller_avionics_evidence(source_url, rendered_html) else {
        return Ok(());
    };
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence")
            .trim();
        let Some(signal) = controller_quantity_ambiguity_signal(
            &installed_equipment,
            &observation.manufacturer,
            &observation.model,
            evidence,
        ) else {
            continue;
        };
        if observation.source_confidence.as_deref() == Some("high") {
            return Err(AvionicsValidationFailure::occurrence(
                AvionicsValidationRule::QuantityProofAmbiguous,
                index,
                AvionicsValidationField::SourceConfidence,
            ));
        }
        if !evidence.contains(&installed_equipment[signal.start..signal.end]) {
            return Err(AvionicsValidationFailure::occurrence(
                AvionicsValidationRule::QuantityEvidenceIncomplete,
                index,
                AvionicsValidationField::SourceEvidenceText,
            ));
        }
    }
    Ok(())
}

fn same_product_identity(left: &ParsedAvionics, right: &ParsedAvionics) -> bool {
    punctuation_insensitive_identity_key(&left.manufacturer)
        == punctuation_insensitive_identity_key(&right.manufacturer)
        && punctuation_insensitive_identity_key(&left.model)
            == punctuation_insensitive_identity_key(&right.model)
}

fn punctuation_insensitive_identity_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct IdentityRange {
    start: usize,
    end: usize,
}

fn controller_quantity_ambiguity_signal(
    source: &str,
    manufacturer: &str,
    model: &str,
    evidence: &str,
) -> Option<IdentityRange> {
    let ranges = normalized_identity_occurrence_ranges(source, model);
    let mut ambiguity = (ranges.len() > 1).then(|| IdentityRange {
        start: ranges.first().expect("two ranges have a first").start,
        end: ranges.last().expect("two ranges have a last").end,
    });
    if let Some(run_on_dual) = controller_run_on_dual_evidence_range(source, evidence, model) {
        include_ambiguity_range(&mut ambiguity, run_on_dual);
    }
    for range in &ranges {
        if let Some(marker) = quantity_marker_in_identity_item(source, *range, manufacturer) {
            include_ambiguity_range(&mut ambiguity, marker);
        }
    }
    ambiguity
}

fn include_ambiguity_range(ambiguity: &mut Option<IdentityRange>, range: IdentityRange) {
    if let Some(ambiguity) = ambiguity {
        ambiguity.start = ambiguity.start.min(range.start);
        ambiguity.end = ambiguity.end.max(range.end);
    } else {
        *ambiguity = Some(range);
    }
}

fn controller_run_on_dual_evidence_range(
    source: &str,
    evidence: &str,
    model: &str,
) -> Option<IdentityRange> {
    if !controller_field_has_exact_evidence_line(source, evidence) {
        return None;
    }
    let Some(identity_end) = normalized_left_bounded_identity_end(evidence, model) else {
        return None;
    };
    let suffix = &evidence[identity_end..];
    let Some(capabilities) = suffix
        .get(..suffix.len().saturating_sub("(Dual)".len()))
        .filter(|_| suffix.to_ascii_lowercase().ends_with("(dual)"))
    else {
        return None;
    };
    let parts = capabilities.split('/').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    {
        return None;
    }
    let matches = source.match_indices(evidence).collect::<Vec<_>>();
    let (first, _) = *matches.first()?;
    let (last, _) = *matches.last()?;
    Some(IdentityRange {
        start: first,
        end: last + evidence.len(),
    })
}

fn normalized_left_bounded_identity_end(source: &str, identity: &str) -> Option<usize> {
    let normalized_identity = punctuation_insensitive_identity_key(identity);
    if normalized_identity.is_empty() {
        return None;
    }
    let mut normalized_source = String::new();
    let mut source_offsets = Vec::new();
    for (offset, character) in source.char_indices() {
        if character.is_ascii_alphanumeric() {
            normalized_source.push(character.to_ascii_lowercase());
            source_offsets.push(offset);
        }
    }
    let spans = normalized_source
        .match_indices(&normalized_identity)
        .filter_map(|(offset, _)| {
            let source_start = source_offsets.get(offset).copied()?;
            let last_offset = offset.checked_add(normalized_identity.len() - 1)?;
            let source_last = source_offsets.get(last_offset).copied()?;
            source[..source_start]
                .chars()
                .next_back()
                .is_none_or(|character| !character.is_ascii_alphanumeric())
                .then(|| {
                    source_last
                        + source[source_last..]
                            .chars()
                            .next()
                            .expect("a recorded source offset has one character")
                            .len_utf8()
                })
        })
        .collect::<Vec<_>>();
    let [end] = spans.as_slice() else {
        return None;
    };
    Some(*end)
}

fn normalized_identity_occurrence_ranges(source: &str, identity: &str) -> Vec<IdentityRange> {
    let normalized_identity = punctuation_insensitive_identity_key(identity);
    if normalized_identity.is_empty() {
        return Vec::new();
    }
    let mut normalized_source = String::new();
    let mut source_offsets = Vec::new();
    for (offset, character) in source.char_indices() {
        if character.is_ascii_alphanumeric() {
            normalized_source.push(character.to_ascii_lowercase());
            source_offsets.push(offset);
        }
    }
    normalized_source
        .match_indices(&normalized_identity)
        .filter_map(|(offset, _)| {
            let source_start = source_offsets.get(offset).copied()?;
            let last_offset = offset.checked_add(normalized_identity.len() - 1)?;
            let source_last = source_offsets.get(last_offset).copied()?;
            let source_end = source_last
                + source[source_last..]
                    .chars()
                    .next()
                    .expect("a recorded source offset has one character")
                    .len_utf8();
            identity_span_has_boundaries(source, source_start, source_end).then_some(
                IdentityRange {
                    start: source_start,
                    end: source_end,
                },
            )
        })
        .collect()
}

#[derive(Clone, Debug)]
struct SourceWord {
    value: String,
    start: usize,
    end: usize,
}

fn source_words(source: &str, start: usize, end: usize) -> Vec<SourceWord> {
    let mut words = Vec::new();
    let mut word_start = None;
    for (relative, character) in source[start..end].char_indices() {
        let offset = start + relative;
        if character.is_ascii_alphanumeric() {
            word_start.get_or_insert(offset);
        } else if let Some(word_start) = word_start.take() {
            words.push(SourceWord {
                value: source[word_start..offset].to_ascii_lowercase(),
                start: word_start,
                end: offset,
            });
        }
    }
    if let Some(word_start) = word_start {
        words.push(SourceWord {
            value: source[word_start..end].to_ascii_lowercase(),
            start: word_start,
            end,
        });
    }
    words
}

fn quantity_marker_in_identity_item(
    source: &str,
    identity: IdentityRange,
    manufacturer: &str,
) -> Option<IdentityRange> {
    let (item_start, item_end) = item_bounds(source, identity.start);
    let item_words = source_words(source, item_start, item_end);
    let item_range = || {
        let start = item_words.first().map_or(identity.start, |word| word.start);
        let end = item_words.last().map_or(identity.end, |word| word.end);
        IdentityRange { start, end }
    };
    if item_words.iter().any(|word| word.value == "dual")
        || item_words
            .iter()
            .any(|word| decimal_quantity_is_multiplier(&word.value))
        || source[item_start..item_end]
            .match_indices('#')
            .any(|(offset, _)| {
                source[item_start + offset + 1..item_end]
                    .bytes()
                    .next()
                    .is_some_and(|byte| byte.is_ascii_digit())
            })
    {
        return Some(item_range());
    }

    let words = source_words(source, item_start, identity.start);
    let manufacturer_words = source_words(manufacturer, 0, manufacturer.len());
    let mut end = words.len();
    if !manufacturer_words.is_empty()
        && words
            .get(end.saturating_sub(manufacturer_words.len())..end)
            .is_some_and(|tail| {
                tail.iter()
                    .map(|word| &word.value)
                    .eq(manufacturer_words.iter().map(|word| &word.value))
            })
    {
        end -= manufacturer_words.len();
    }
    let plain_decimal_prefix = end.checked_sub(1).is_some_and(|last_word| {
        let word = &words[last_word];
        let prefix = source[item_start..word.start].trim_end();
        decimal_quantity_word(&word.value)
            && !prefix.ends_with('#')
            && prefix
                .chars()
                .next_back()
                .is_none_or(|character| matches!(character, ':' | '('))
    });
    let after_words = source_words(source, identity.end, item_end);
    let labeled_decimal_suffix = after_words
        .first()
        .is_some_and(|word| decimal_quantity_word(&word.value))
        && after_words
            .get(1)
            .is_some_and(|word| matches!(word.value.as_str(), "unit" | "units" | "each" | "ea"));
    (plain_decimal_prefix || labeled_decimal_suffix).then(item_range)
}

fn decimal_quantity_word(value: &str) -> bool {
    let digits = value
        .strip_prefix('x')
        .or_else(|| value.strip_suffix('x'))
        .unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn decimal_quantity_is_multiplier(value: &str) -> bool {
    (value.starts_with('x') || value.ends_with('x')) && decimal_quantity_word(value)
}

fn item_bounds(source: &str, offset: usize) -> (usize, usize) {
    let start = source[..offset]
        .rfind([',', ';', '\r', '\n'])
        .map_or(0, |index| index + 1);
    let end = source[offset..]
        .find([',', ';', '\r', '\n'])
        .map_or(source.len(), |index| offset + index);
    (start, end)
}

fn validate_current_avionics_identity_evidence_occurrence(
    observation: &ParsedAvionics,
    index: usize,
    listing_context: &ListingEvidenceContext,
    exact_visible_evidence_locator: &str,
    controller_field: Option<&str>,
) -> Result<(), AvionicsValidationFailure> {
    let evidence = observation
        .source_evidence_text
        .as_deref()
        .expect("the canonical parser requires occurrence evidence")
        .trim();
    let bounded_source = listing_context.for_candidate(
        &observation.manufacturer,
        &observation.model,
        Some(exact_visible_evidence_locator),
    );
    let evidence_context = ListingEvidenceContext::from_cleaned_text(evidence);
    let controller_annotation_identity = controller_field.is_some_and(|field| {
        controller_field_has_exact_evidence_line(field, evidence)
            && (exact_controller_run_on_capability_annotation(
                evidence,
                &observation.model,
                &observation.avionics_types,
                true,
            ) || exact_controller_waas_capability_annotation(
                evidence,
                &observation.manufacturer,
                &observation.model,
                &observation.avionics_types,
                Some(observation.quantity),
            ))
    });
    if (!bounded_source.contains(evidence) && !controller_annotation_identity)
        || !(extraction_occurrence_has_exact_identity(
            &evidence_context,
            &observation.manufacturer,
            &observation.model,
            &observation.avionics_types,
            evidence,
        ) || controller_annotation_identity)
    {
        return Err(AvionicsValidationFailure::occurrence(
            AvionicsValidationRule::CandidateIdentityNotInEvidence,
            index,
            AvionicsValidationField::SourceEvidenceText,
        ));
    }
    if let Some(replacement) = observation.replaces.as_ref() {
        let replacement_annotation_identity = controller_field.is_some_and(|field| {
            controller_field_has_exact_evidence_line(field, evidence)
                && (exact_controller_run_on_capability_annotation(
                    evidence,
                    &replacement.model,
                    &replacement.avionics_types,
                    false,
                ) || exact_controller_waas_capability_annotation(
                    evidence,
                    &replacement.manufacturer,
                    &replacement.model,
                    &replacement.avionics_types,
                    None,
                ))
        });
        if !(extraction_occurrence_has_exact_identity(
            &evidence_context,
            &replacement.manufacturer,
            &replacement.model,
            &replacement.avionics_types,
            evidence,
        ) || replacement_annotation_identity)
        {
            return Err(AvionicsValidationFailure::occurrence(
                AvionicsValidationRule::ReplacementIdentityNotInEvidence,
                index,
                AvionicsValidationField::SourceEvidenceText,
            ));
        }
    }
    Ok(())
}

fn context_has_exact_identity(
    context: &ListingEvidenceContext,
    manufacturer: &str,
    model: &str,
) -> bool {
    context
        .unique_exact_product_slice(manufacturer, model)
        .is_some()
        || context.unique_exact_model_slice(model).is_some()
}

/// Admit one source annotation grammar only at the extraction boundary.
///
/// The strict `context_has_exact_identity` path remains the catalog-reuse
/// contract. A standalone `WAAS` followed by an exact slash-delimited list of
/// the observation's declared atomic capabilities is occurrence evidence, but
/// never proof that the unqualified model is safe for local catalog reuse. An
/// attached-`W` identity may additionally carry the exact `WAAS IFR` wording
/// and a bounded rebuilt-date note used by Controller listings.
fn extraction_occurrence_has_exact_identity(
    context: &ListingEvidenceContext,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    evidence: &str,
) -> bool {
    context_has_exact_identity(context, manufacturer, model)
        || exact_standalone_waas_capability_annotation(
            evidence,
            manufacturer,
            model,
            avionics_types,
        )
}

/// Recognize one Controller-specific publisher run-on grammar.
///
/// This function proves only the grammar inside one occurrence. Callers must
/// separately prove that `evidence` is one exact line from the structurally
/// admitted Controller `Avionics/Radios` value before using the result.
///
/// Some Controller `Avionics/Radios` values omit whitespace between a model
/// and its capability list (for example `GIA63WNAV/COM/GPS(Dual)`). The model
/// remains an identity only when its sole normalized occurrence is followed
/// immediately by an exact slash-delimited set of the same curated
/// capabilities declared for that occurrence. The suffix must end there,
/// apart from Controller's exact `(Dual)` ambiguity annotation. This grammar
/// proves identity only; it never establishes a physical count.
pub(crate) fn exact_controller_run_on_capability_annotation(
    evidence: &str,
    model: &str,
    avionics_types: &[String],
    allow_dual_annotation: bool,
) -> bool {
    let normalized_model = normalize_evidence_typography(model);
    if normalized_model.is_empty() || avionics_types.is_empty() {
        return false;
    }

    let mut normalized_evidence = String::new();
    let mut source_offsets = Vec::new();
    for (offset, character) in evidence.char_indices() {
        if character.is_ascii_alphanumeric() {
            normalized_evidence.push(character.to_ascii_lowercase());
            source_offsets.push(offset);
        }
    }
    let spans = normalized_evidence
        .match_indices(&normalized_model)
        .filter_map(|(normalized_start, _)| {
            let source_start = source_offsets.get(normalized_start).copied()?;
            let normalized_last = normalized_start
                .checked_add(normalized_model.len())?
                .checked_sub(1)?;
            let source_last = source_offsets.get(normalized_last).copied()?;
            let source_end = source_last + evidence[source_last..].chars().next()?.len_utf8();
            evidence[..source_start]
                .chars()
                .next_back()
                .is_none_or(|character| !character.is_alphanumeric())
                .then_some(source_end)
        })
        .collect::<Vec<_>>();
    let [identity_end] = spans.as_slice() else {
        return false;
    };
    let suffix = &evidence[*identity_end..];
    if suffix
        .chars()
        .next()
        .is_none_or(|character| !character.is_ascii_alphanumeric())
    {
        return false;
    }

    let declared = avionics_types
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if declared.len() != avionics_types.len()
        || declared
            .iter()
            .any(|capability| !CURATED_AVIONICS_TYPES.contains(capability))
    {
        return false;
    }

    let mut observed = BTreeSet::new();
    let mut remaining = suffix;
    loop {
        let Some((capability, consumed)) = exact_capability_phrase_prefix(remaining) else {
            return false;
        };
        if !declared.contains(capability) || !observed.insert(capability) {
            return false;
        }
        remaining = &remaining[consumed..];
        let trimmed = remaining.trim_start();
        if trimmed.is_empty() {
            return observed == declared;
        }
        if trimmed.eq_ignore_ascii_case("(dual)") {
            return allow_dual_annotation && observed == declared;
        }
        let Some(next) = trimmed.strip_prefix('/') else {
            return false;
        };
        remaining = next.trim_start();
        if remaining.is_empty() {
            return false;
        }
    }
}

/// Recognize two exact Controller publisher spellings where `WAAS` annotates
/// an attached-`W` product identity instead of extending it.
///
/// This remains an extraction-only exception. Callers must separately prove
/// that `evidence` is one complete line from Controller's admitted
/// `Avionics/Radios` field. The ordinary catalog identity matcher deliberately
/// continues to reject both qualified source spellings.
fn exact_controller_waas_capability_annotation(
    evidence: &str,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    quantity: Option<i64>,
) -> bool {
    if !model_has_attached_w_designator(model) {
        return false;
    }

    let evidence = evidence.trim();
    exact_controller_slash_waas_annotation(evidence, manufacturer, model, avionics_types, quantity)
        || exact_controller_dual_plural_waas_annotation(
            evidence,
            manufacturer,
            model,
            avionics_types,
            quantity,
        )
}

fn exact_controller_slash_waas_annotation(
    evidence: &str,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    quantity: Option<i64>,
) -> bool {
    if !matches!(quantity, None | Some(1)) {
        return false;
    }
    let Some(identity_end) = exact_normalized_identity_prefix_end(evidence, manufacturer, model)
    else {
        return false;
    };
    let Some(capabilities) = strip_ascii_case_prefix(&evidence[identity_end..], "/WAAS")
        .and_then(strip_required_whitespace_prefix)
    else {
        return false;
    };
    exact_declared_slash_capabilities(capabilities, avionics_types, false)
}

fn exact_controller_dual_plural_waas_annotation(
    evidence: &str,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    quantity: Option<i64>,
) -> bool {
    if quantity != Some(2) {
        return false;
    }
    let Some(after_dual) =
        strip_ascii_case_prefix(evidence, "Dual").and_then(strip_required_whitespace_prefix)
    else {
        return false;
    };
    let Some(identity_end) = exact_normalized_identity_prefix_end(after_dual, manufacturer, model)
    else {
        return false;
    };
    let Some(after_identity) = strip_required_whitespace_prefix(&after_dual[identity_end..]) else {
        return false;
    };
    let Some(capabilities) =
        strip_ascii_case_prefix(after_identity, "WAAS").and_then(strip_required_whitespace_prefix)
    else {
        return false;
    };
    exact_declared_slash_capabilities(capabilities, avionics_types, true)
}

fn exact_normalized_identity_prefix_end(
    source: &str,
    manufacturer: &str,
    model: &str,
) -> Option<usize> {
    let expected = normalize_evidence_typography(&format!("{manufacturer} {model}"));
    if expected.is_empty()
        || source
            .chars()
            .next()
            .is_none_or(|character| !character.is_ascii_alphanumeric())
    {
        return None;
    }

    let mut observed = String::new();
    for (offset, character) in source.char_indices() {
        if !character.is_ascii_alphanumeric() {
            continue;
        }
        observed.push(character.to_ascii_lowercase());
        if observed.len() == expected.len() {
            return (observed == expected).then_some(offset + character.len_utf8());
        }
        if !expected.starts_with(&observed) {
            return None;
        }
    }
    None
}

fn strip_ascii_case_prefix<'a>(source: &'a str, prefix: &str) -> Option<&'a str> {
    source
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .and_then(|_| source.get(prefix.len()..))
}

fn strip_required_whitespace_prefix(source: &str) -> Option<&str> {
    let trimmed = source.trim_start_matches(char::is_whitespace);
    (trimmed.len() < source.len()).then_some(trimmed)
}

fn exact_declared_slash_capabilities(
    source: &str,
    avionics_types: &[String],
    require_plural_final_capability: bool,
) -> bool {
    if avionics_types.len() < 2
        || avionics_types
            .iter()
            .any(|capability| !CURATED_AVIONICS_TYPES.contains(&capability.as_str()))
    {
        return false;
    }
    let declared = avionics_types
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if declared.len() != avionics_types.len() {
        return false;
    }

    let capabilities = source.split('/').map(str::trim).collect::<Vec<_>>();
    if capabilities.len() != avionics_types.len()
        || capabilities.iter().any(|capability| capability.is_empty())
    {
        return false;
    }

    let mut observed = BTreeSet::new();
    for (index, source_capability) in capabilities.iter().enumerate() {
        let final_capability = index + 1 == capabilities.len();
        let source_capability = if require_plural_final_capability && final_capability {
            let Some(singular) = source_capability.strip_suffix(['s', 'S']) else {
                return false;
            };
            singular
        } else {
            source_capability
        };
        let Some(declared_capability) = declared
            .iter()
            .copied()
            .find(|declared| source_capability.eq_ignore_ascii_case(declared))
        else {
            return false;
        };
        if !observed.insert(declared_capability) {
            return false;
        }
    }
    observed == declared
}

fn controller_field_has_exact_evidence_line(controller_field: &str, evidence: &str) -> bool {
    let evidence = evidence.trim();
    !evidence.is_empty()
        && controller_field.contains(evidence)
        && controller_field.lines().any(|line| line.trim() == evidence)
}

fn exact_capability_phrase_prefix(source: &str) -> Option<(&'static str, usize)> {
    CURATED_AVIONICS_TYPES
        .iter()
        .flat_map(|capability| {
            capability_evidence_phrases(capability)
                .iter()
                .map(move |phrase| (*capability, *phrase))
        })
        .filter_map(|(capability, phrase)| {
            source
                .char_indices()
                .map(|(offset, character)| offset + character.len_utf8())
                .find(|end| {
                    source[*end..]
                        .chars()
                        .next()
                        .is_none_or(|character| !character.is_ascii_alphanumeric())
                        && normalize_source_evidence_span(&source[..*end]) == phrase
                })
                .map(|end| (capability, end))
        })
        .max_by_key(|(_, end)| *end)
}

#[derive(Debug)]
struct EvidenceIdentityToken {
    value: String,
    start: usize,
    end: usize,
}

fn evidence_identity_tokens(value: &str) -> Vec<EvidenceIdentityToken> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut start = None;
    for (offset, character) in value.char_indices() {
        if character.is_ascii_alphanumeric() {
            start.get_or_insert(offset);
            current.push(character.to_ascii_lowercase());
        } else if let Some(token_start) = start.take() {
            tokens.push(EvidenceIdentityToken {
                value: std::mem::take(&mut current),
                start: token_start,
                end: offset,
            });
        }
    }
    if let Some(token_start) = start {
        tokens.push(EvidenceIdentityToken {
            value: current,
            start: token_start,
            end: value.len(),
        });
    }
    tokens
}

fn normalized_atomic_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn exact_standalone_waas_capability_annotation(
    evidence: &str,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
) -> bool {
    let evidence = evidence.trim();
    let tokens = evidence_identity_tokens(evidence);
    let identity = evidence_identity_tokens(&format!("{manufacturer} {model}"));
    if identity.is_empty() || tokens.len() <= identity.len() + 1 {
        return false;
    }
    if !tokens
        .iter()
        .zip(&identity)
        .all(|(observed, expected)| observed.value == expected.value)
        || tokens[identity.len()].value != "waas"
    {
        return false;
    }

    let waas = &tokens[identity.len()];
    let prior = &tokens[identity.len() - 1];
    let waas_separator = &evidence[prior.end..waas.start];
    if waas_separator.is_empty() || !waas_separator.chars().all(char::is_whitespace) {
        return false;
    }

    let mut capability_start = identity.len() + 1;
    if tokens
        .get(capability_start)
        .is_some_and(|token| token.value == "ifr")
    {
        if !model_has_attached_w_designator(model) {
            return false;
        }
        let ifr = &tokens[capability_start];
        let separator = &evidence[waas.end..ifr.start];
        if separator.is_empty() || !separator.chars().all(char::is_whitespace) {
            return false;
        }
        capability_start += 1;
    }

    if avionics_types.len() < 2 || tokens.len() < capability_start + avionics_types.len() {
        return false;
    }
    let capabilities = &tokens[capability_start..capability_start + avionics_types.len()];
    let annotation_end = if capability_start == identity.len() + 1 {
        waas.end
    } else {
        tokens[capability_start - 1].end
    };
    let first_separator = &evidence[annotation_end..capabilities[0].start];
    if first_separator.is_empty() || !first_separator.chars().all(char::is_whitespace) {
        return false;
    }
    if capabilities
        .windows(2)
        .any(|pair| evidence[pair[0].end..pair[1].start].trim() != "/")
    {
        return false;
    }

    let declared = avionics_types
        .iter()
        .map(|value| normalized_atomic_label(value))
        .collect::<BTreeSet<_>>();
    let annotated = capabilities
        .iter()
        .map(|token| token.value.clone())
        .collect::<BTreeSet<_>>();
    if declared.contains("")
        || annotated.len() != capabilities.len()
        || declared.len() != avionics_types.len()
        || declared != annotated
    {
        return false;
    }

    let trailing_annotation =
        evidence[capabilities.last().expect("capabilities exist").end..].trim();
    trailing_annotation.is_empty()
        || (model_has_attached_w_designator(model)
            && exact_rebuilt_date_annotation(trailing_annotation))
}

fn model_has_attached_w_designator(model: &str) -> bool {
    evidence_identity_tokens(model)
        .last()
        .and_then(|token| token.value.strip_suffix('w'))
        .and_then(|prefix| prefix.chars().last())
        .is_some_and(|character| character.is_ascii_digit())
}

fn exact_rebuilt_date_annotation(value: &str) -> bool {
    let Some(value) = value.strip_prefix('-').map(str::trim) else {
        return false;
    };
    let mut parts = value.split_ascii_whitespace();
    if !parts
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("rebuilt"))
    {
        return false;
    }
    let Some(date) = parts.next().filter(|_| parts.next().is_none()) else {
        return false;
    };
    let Some((month, year)) = date.split_once('/') else {
        return false;
    };
    month.len() <= 2
        && month
            .parse::<u8>()
            .is_ok_and(|month| (1..=12).contains(&month))
        && matches!(year.len(), 2 | 4)
        && year.bytes().all(|byte| byte.is_ascii_digit())
}

fn validate_capture_binding(
    extraction: CurrentAvionicsExtraction<'_>,
) -> Result<(), AvionicsValidationFailure> {
    if extraction.listing_id <= 0 || extraction.submission_id <= 0 {
        return Err(AvionicsValidationFailure::global(
            AvionicsValidationRule::InvalidBindingIdentifiers,
        ));
    }
    if extraction.listing_owner_user_id <= 0
        || extraction.submission_owner_user_id != extraction.listing_owner_user_id
    {
        return Err(AvionicsValidationFailure::global(
            AvionicsValidationRule::OwnerBindingMismatch,
        ));
    }
    if extraction.submission_canonical_listing_id != Some(extraction.listing_id) {
        return Err(AvionicsValidationFailure::global(
            AvionicsValidationRule::CanonicalListingBindingMismatch,
        ));
    }
    let listing_source_url = extraction
        .listing_source_url
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .ok_or_else(|| {
            AvionicsValidationFailure::global(AvionicsValidationRule::MissingListingSourceUrl)
        })?;
    if extraction.submission_source_url.trim().is_empty()
        || extraction.submission_source_url != listing_source_url
    {
        return Err(AvionicsValidationFailure::global(
            AvionicsValidationRule::SourceUrlBindingMismatch,
        ));
    }
    if extraction.rendered_html.is_empty() {
        return Err(AvionicsValidationFailure::global(
            AvionicsValidationRule::MissingRenderedHtml,
        ));
    }
    if !valid_sha256(extraction.rendered_html_sha256)
        || sha256_hex(extraction.rendered_html.as_bytes()) != extraction.rendered_html_sha256
    {
        return Err(AvionicsValidationFailure::global(
            AvionicsValidationRule::RenderedHtmlDigestMismatch,
        ));
    }
    if extraction.extracted_listing_json.trim().is_empty() {
        return Err(AvionicsValidationFailure::global(
            AvionicsValidationRule::MissingExtractionJson,
        ));
    }
    Ok(())
}

fn validate_required_identity(
    object: &serde_json::Map<String, Value>,
    index: usize,
    replacement: bool,
) -> Result<(), AvionicsValidationFailure> {
    for (field, rule, location) in [
        (
            "manufacturer",
            AvionicsValidationRule::MissingManufacturer,
            if replacement {
                AvionicsValidationField::ReplacementManufacturer
            } else {
                AvionicsValidationField::Manufacturer
            },
        ),
        (
            "model",
            AvionicsValidationRule::MissingModel,
            if replacement {
                AvionicsValidationField::ReplacementModel
            } else {
                AvionicsValidationField::Model
            },
        ),
    ] {
        if object
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            return Err(AvionicsValidationFailure::occurrence(rule, index, location));
        }
    }
    Ok(())
}

fn validate_capabilities(
    value: &Value,
    index: usize,
    replacement: bool,
) -> Result<(), AvionicsValidationFailure> {
    let Some(types) = value.get("types").and_then(Value::as_array) else {
        return Err(AvionicsValidationFailure::occurrence(
            AvionicsValidationRule::InvalidTypes,
            index,
            if replacement {
                AvionicsValidationField::ReplacementTypes
            } else {
                AvionicsValidationField::Types
            },
        ));
    };
    let mut seen = BTreeSet::new();
    if types.is_empty()
        || types.iter().any(|value| {
            value.as_str().map(str::trim).is_none_or(|value| {
                value.is_empty()
                    || (value != "Unknown" && !CURATED_AVIONICS_TYPES.contains(&value))
                    || !seen.insert(value)
            })
        })
        || (types.len() > 1 && seen.contains("Unknown"))
    {
        return Err(AvionicsValidationFailure::occurrence(
            AvionicsValidationRule::InvalidTypes,
            index,
            if replacement {
                AvionicsValidationField::ReplacementTypes
            } else {
                AvionicsValidationField::Types
            },
        ));
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTROLLER_URL: &str = "https://www.controller.com/listing/for-sale/252742967/1965-cessna-182-skylane-piston-single-aircraft";
    const SUBMISSION_26_URL: &str = "https://www.controller.com/listing/for-sale/256441619/2010-cessna-182t-skylane-piston-single-aircraft";
    const CAPTURE_25_URL: &str = "https://www.controller.com/listing/for-sale/257959105/example";
    const GENERIC_URL: &str = "https://example.test/listing/avionics";

    fn controller_html(field: &str) -> String {
        format!(
            r#"<html><head><meta content="Garmin GMA-1347"></head><body>
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

    fn capture_25_html() -> String {
        include_str!("../../../tests/fixtures/controller/id25_like_listing.html").to_string()
    }

    fn recover_controller_avionics_evidence_typography(
        payload: &mut Value,
        source_url: &str,
        rendered_html: &str,
    ) -> Result<bool, AvionicsValidationFailure> {
        recover_controller_avionics_extraction(payload, source_url, rendered_html)
    }

    fn installed(manufacturer: &str, model: &str, evidence: &str) -> Value {
        serde_json::json!({
            "avionics": [{
                "manufacturer": manufacturer,
                "model": model,
                "types": ["Flight Display"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": evidence,
                "source_confidence": "high"
            }]
        })
    }

    fn replacement(evidence: &str) -> Value {
        serde_json::json!({
            "avionics": [{
                "manufacturer": "Garmin",
                "model": "GTN 750Xi",
                "types": ["GPS"],
                "quantity": 1,
                "configuration_action": "replaces",
                "replaces": {
                    "manufacturer": "Garmin",
                    "model": "GNS 530W",
                    "types": ["GPS"]
                },
                "source_evidence_text": evidence,
                "source_confidence": "high"
            }]
        })
    }

    fn evidence(payload: &Value) -> &str {
        payload["avionics"][0]["source_evidence_text"]
            .as_str()
            .unwrap()
    }

    fn bound<'a>(html: &'a str, hash: &'a str, json: &'a str) -> CurrentAvionicsExtraction<'a> {
        CurrentAvionicsExtraction {
            listing_id: 7,
            listing_owner_user_id: 11,
            listing_source_url: Some("https://example.test/listing/7"),
            submission_id: 13,
            submission_owner_user_id: 11,
            submission_canonical_listing_id: Some(7),
            submission_source_url: "https://example.test/listing/7",
            rendered_html: html,
            rendered_html_sha256: hash,
            extracted_listing_json: json,
        }
    }

    #[test]
    fn accepts_only_explicit_current_schema_bound_to_visible_capture() {
        let html = "<html><body><p>Garmin G5 installed</p></body></html>";
        let hash = sha256_hex(html.as_bytes());
        let json = r#"{"avionics":[{"manufacturer":"Garmin","model":"G5","types":["Flight Display"],"quantity":1,"configuration_action":"installed","replaces":null,"source_evidence_text":"Garmin G5 installed","source_confidence":"high"}]}"#;
        let parsed = validate_current_avionics_extraction(bound(html, &hash, json)).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].avionics_types, ["Flight Display"]);
    }

    #[test]
    fn accepts_bounded_model_only_evidence_for_short_letter_digit_models() {
        let html = "<html><body><p>G5 installed</p><p>Garmin G5X spare</p></body></html>";
        let hash = sha256_hex(html.as_bytes());
        let json = r#"{"avionics":[{"manufacturer":"Garmin","model":"G5","types":["Flight Display"],"quantity":1,"configuration_action":"installed","replaces":null,"source_evidence_text":"G5 installed","source_confidence":"high"}]}"#;

        let parsed = validate_current_avionics_extraction(bound(html, &hash, json)).unwrap();
        assert_eq!(parsed[0].model, "G5");
    }

    #[test]
    fn exact_evidence_selects_its_repeated_identity_mention() {
        let html = format!(
            "<html><body><p>Garmin G1000 summary.</p><p>{}</p><p>Garmin G1000 avionics system</p></body></html>",
            "unrelated listing detail ".repeat(300),
        );
        let json = r#"{"avionics":[{"manufacturer":"Garmin","model":"G1000","types":["Flight Display"],"quantity":1,"configuration_action":"installed","replaces":null,"source_evidence_text":"Garmin G1000 avionics system","source_confidence":"high"}]}"#;

        let parsed =
            validate_unbound_current_avionics_extraction(json, GENERIC_URL, &html).unwrap();

        assert_eq!(parsed[0].model, "G1000");
    }

    #[test]
    fn controller_quantity_ambiguity_never_authorizes_high_confidence() {
        for (manufacturer, model, field) in [
            ("Garmin", "G1000", "Garmin G1000 PFD\nGarmin G1000 MFD"),
            (
                "Garmin",
                "G5",
                "Garmin G5 was installed previously. Later the Garmin G5 was serviced.",
            ),
            ("Garmin", "G5", "Garmin G5 PFD\nDynon G5 backup"),
            ("Garmin", "G5", "Dual Garmin G5"),
            ("Garmin", "G5", "Not Dual Garmin G5"),
            ("Garmin", "G5", "Optional Dual Garmin G5"),
            ("Garmin", "G5", "Garmin G5 dual screen"),
            ("Garmin", "G5", "Garmin G5 #3"),
            ("Garmin", "G5", "Garmin G5 2 units"),
            ("Garmin", "GIA63W", "GIA63WNAV/COM/GPS(Dual)"),
            (
                "Garmin",
                "G5",
                "Garmin G5 HSI\nGarmin 430W GPS/NAV/COM\nGarmin G5 attitude",
            ),
        ] {
            let mut payload = installed(manufacturer, model, field);
            let observations = parse_current_avionics_extraction_value(&payload).unwrap();
            let result = validate_current_avionics_quantity_completeness(
                &observations,
                CONTROLLER_URL,
                &controller_html(field),
            );
            assert!(result.is_err(), "{field:?} was admitted as high confidence");
            let error = result.unwrap_err();
            assert!(
                error.contains("ambiguous Controller quantity evidence"),
                "{field:?}: {error}"
            );

            payload["avionics"][0]["source_confidence"] = serde_json::json!("low");
            let observations = parse_current_avionics_extraction_value(&payload).unwrap();
            validate_current_avionics_quantity_completeness(
                &observations,
                CONTROLLER_URL,
                &controller_html(field),
            )
            .unwrap();
        }
    }

    #[test]
    fn controller_quantity_ambiguity_requires_complete_exact_evidence() {
        for (field, evidence) in [
            ("Garmin G1000 PFD\nGarmin G1000 MFD", "Garmin G1000 PFD"),
            ("Optional Dual Garmin G5", "Dual Garmin G5"),
            ("Garmin G5 dual screen", "Garmin G5 dual"),
            ("Garmin G5 2 units", "Garmin G5 2"),
        ] {
            let model = if field.contains("G1000") {
                "G1000"
            } else {
                "G5"
            };
            let mut payload = installed("Garmin", model, evidence);
            payload["avionics"][0]["source_confidence"] = serde_json::json!("medium");
            let observations = parse_current_avionics_extraction_value(&payload).unwrap();
            let result = validate_current_avionics_quantity_completeness(
                &observations,
                CONTROLLER_URL,
                &controller_html(field),
            );
            assert!(
                result.is_err(),
                "{field:?} admitted incomplete evidence {evidence:?}"
            );
            let error = result.unwrap_err();
            assert!(
                error.contains("complete bounded Controller quantity ambiguity"),
                "{field:?}: {error}"
            );
        }
    }

    #[test]
    fn duplicate_normalized_products_are_rejected_globally_without_source_signals() {
        let html = "<html><body><p>Garmin G5 installed</p></body></html>";
        let mut duplicate = installed("Garmin", "G5", "Garmin G5 installed");
        let second = installed("Gar-min", "G-5", "Garmin G5 installed")["avionics"][0].clone();
        duplicate["avionics"].as_array_mut().unwrap().push(second);
        let observations = parse_current_avionics_extraction_value(&duplicate).unwrap();
        assert!(
            validate_current_avionics_quantity_completeness(&observations, GENERIC_URL, html,)
                .unwrap_err()
                .contains("duplicates an earlier normalized manufacturer/model identity")
        );

        let mut different_manufacturers = installed("Garmin", "G5", "Garmin G5 installed");
        let dynon = installed("Dynon", "G5", "Dynon G5 installed")["avionics"][0].clone();
        different_manufacturers["avionics"]
            .as_array_mut()
            .unwrap()
            .push(dynon);
        let observations =
            parse_current_avionics_extraction_value(&different_manufacturers).unwrap();
        validate_current_avionics_quantity_completeness(&observations, GENERIC_URL, html).unwrap();
    }

    #[test]
    fn unsupported_high_confidence_quantity_is_rejected_without_a_controller_adapter() {
        let html = "<html><body><p>Garmin G5 installed</p></body></html>";
        let mut payload = installed("Garmin", "G5", "Garmin G5 installed");
        payload["avionics"][0]["quantity"] = serde_json::json!(99);
        let observations = parse_current_avionics_extraction_value(&payload).unwrap();
        assert!(
            validate_current_avionics_quantity_completeness(&observations, GENERIC_URL, html,)
                .unwrap_err()
                .contains("quantity greater than one cannot have high source confidence")
        );

        payload["avionics"][0]["source_confidence"] = serde_json::json!("low");
        let observations = parse_current_avionics_extraction_value(&payload).unwrap();
        validate_current_avionics_quantity_completeness(&observations, GENERIC_URL, html).unwrap();

        payload["avionics"][0]["quantity"] = serde_json::json!(1);
        payload["avionics"][0]["source_confidence"] = serde_json::json!("high");
        let observations = parse_current_avionics_extraction_value(&payload).unwrap();
        validate_current_avionics_quantity_completeness(&observations, GENERIC_URL, html).unwrap();
    }

    #[test]
    fn controller_run_on_dual_marker_is_bounded_to_its_immediate_item() {
        let field = "GIA63WNAV/COM/GPS(Dual)";
        let html = controller_html(field);
        let mut payload = installed("Garmin", "GIA63W", field);
        payload["avionics"][0]["types"] = serde_json::json!(["NAV", "COM", "GPS"]);
        payload["avionics"][0]["source_confidence"] = serde_json::json!("medium");
        validate_unbound_current_avionics_extraction(&payload.to_string(), CONTROLLER_URL, &html)
            .unwrap();
        payload["avionics"][0]["quantity"] = serde_json::json!(2);
        validate_unbound_current_avionics_extraction(&payload.to_string(), CONTROLLER_URL, &html)
            .unwrap();

        let unrelated = "GIA63WNAV/COM/GPS; Garmin GFC500 (Dual)";
        let unrelated_payload = installed("Garmin", "GIA63W", unrelated);
        let observations = parse_current_avionics_extraction_value(&unrelated_payload).unwrap();
        validate_current_avionics_quantity_completeness(
            &observations,
            CONTROLLER_URL,
            &controller_html(unrelated),
        )
        .unwrap();
    }

    #[test]
    fn nonadjacent_repeated_identity_is_rejected_without_mutating_model_output() {
        let field = "Garmin G5 HSI\nGarmin 430W GPS/NAV/COM\nGarmin G5 attitude";
        let html = controller_html(field);
        let mut payload = installed("Garmin", "G5", "Garmin G5 HSI");
        let original = payload.clone();

        assert!(
            !recover_controller_avionics_extraction(&mut payload, CONTROLLER_URL, &html).unwrap()
        );
        assert_eq!(payload, original);
        assert!(validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CONTROLLER_URL,
            &html,
        )
        .is_err());
        assert_eq!(payload, original);
    }

    #[test]
    fn shared_identity_validation_rejects_hidden_and_metadata_only_evidence() {
        let html = "<html><head><meta content=\"Garmin G1000 metadata\"></head><body><p hidden>Garmin G1000 avionics system</p></body></html>";
        let payload = installed("Garmin", "G1000", "Garmin G1000 avionics system");
        let observations = parse_current_avionics_extraction_value(&payload).unwrap();
        let context = ListingEvidenceContext::from_rendered_html(Some(&html));

        assert!(validate_current_avionics_identity_evidence(
            &observations,
            &context,
            "https://example.test/listing/hidden",
            html,
        )
        .unwrap_err()
        .contains("structurally visible"));

        let metadata_payload = installed("Garmin", "G1000", "Garmin G1000 metadata");
        let metadata_observations =
            parse_current_avionics_extraction_value(&metadata_payload).unwrap();
        assert!(validate_current_avionics_identity_evidence(
            &metadata_observations,
            &context,
            "https://example.test/listing/hidden",
            html
        )
        .unwrap_err()
        .contains("structurally visible"));
    }

    #[test]
    fn retained_capture_25_rejects_cross_field_evidence_and_inflated_suite_types() {
        let html = capture_25_html();
        let mut payload = serde_json::json!({"avionics": [
            {
                "manufacturer": "GARMIN",
                "model": "G1000 NXI",
                "types": [
                    "Integrated Flight Deck", "Flight Display", "COM", "NAV",
                    "Autopilot", "Weather Radar", "Traffic", "Terrain Awareness"
                ],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": "GARMIN G1000 NXI SVT",
                "source_confidence": "high"
            },
            {
                "manufacturer": "GARMIN",
                "model": "GFC 700",
                "types": ["Autopilot"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": "GFC 700 autopilot",
                "source_confidence": "high"
            }
        ]});

        let error = validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CAPTURE_25_URL,
            &html,
        )
        .unwrap_err();
        assert!(
            error.contains("structurally visible source unit"),
            "{error}"
        );

        payload["avionics"][0]["source_evidence_text"] = serde_json::json!("GARMIN G1000 NXI");
        let error = validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CAPTURE_25_URL,
            &html,
        )
        .unwrap_err();
        assert!(error.contains("integrated suite"), "{error}");

        payload["avionics"][0]["types"] = serde_json::json!(["Integrated Flight Deck"]);
        let observations = validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CAPTURE_25_URL,
            &html,
        )
        .unwrap();
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].avionics_types, ["Integrated Flight Deck"]);
        assert_eq!(observations[1].avionics_types, ["Autopilot"]);
    }

    #[test]
    fn integrated_suite_cannot_absorb_a_separately_extracted_component_type() {
        let html = "<html><body><p>GARMIN G1000 NXI integrated flight deck and autopilot</p><p>GFC 700 autopilot</p></body></html>";
        let payload = serde_json::json!({"avionics": [
            {
                "manufacturer": "Garmin", "model": "G1000 NXi",
                "types": ["Integrated Flight Deck", "Autopilot"], "quantity": 1,
                "configuration_action": "installed", "replaces": null,
                "source_evidence_text": "GARMIN G1000 NXI integrated flight deck and autopilot",
                "source_confidence": "high"
            },
            {
                "manufacturer": "Garmin", "model": "GFC 700",
                "types": ["Autopilot"], "quantity": 1,
                "configuration_action": "installed", "replaces": null,
                "source_evidence_text": "GFC 700 autopilot", "source_confidence": "high"
            }
        ]});

        let error = validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            "https://example.test/listing/suite",
            html,
        )
        .unwrap_err();
        assert!(error.contains("separate product"), "{error}");
    }

    #[test]
    fn integrated_suite_additions_require_exact_capability_evidence() {
        let html = "<html><body><p>Garmin GTN 750 GPS/NAV/COM installed</p></body></html>";
        let mut payload = installed("Garmin", "GTN 750", "Garmin GTN 750 GPS/NAV/COM");
        payload["avionics"][0]["types"] = serde_json::json!(["GPS", "NAV", "COM"]);
        validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            "https://example.test/listing/multifunction",
            html,
        )
        .unwrap();

        payload["avionics"][0]["types"] = serde_json::json!(["Integrated Flight Deck", "GPS"]);
        payload["avionics"][0]["source_evidence_text"] = serde_json::json!("Garmin GTN 750");
        let html = "<html><body><p>Garmin GTN 750</p></body></html>";
        let error = validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            "https://example.test/listing/multifunction",
            html,
        )
        .unwrap_err();
        assert!(error.contains("integrated suite"), "{error}");
    }

    #[test]
    fn visible_evidence_locator_does_not_erase_meaningful_model_suffixes() {
        for (model, evidence) in [
            ("G1000", "Garmin G1000 NXi avionics system"),
            ("G5", "Garmin G5X flight display"),
            ("GTN 750", "Garmin GTN 750Xi GPS/NAV/COM"),
            ("GNS 430", "Garmin GNS 430W GPS/NAV/COM"),
            ("GTX 33", "Garmin GTX 33 ES transponder"),
        ] {
            let html = format!("<html><body><p>{evidence}</p></body></html>");
            let payload = installed("Garmin", model, evidence);

            assert!(validate_unbound_current_avionics_extraction(
                &payload.to_string(),
                GENERIC_URL,
                &html,
            )
            .unwrap_err()
            .contains("candidate identity"));
        }
    }

    #[test]
    fn controller_accepts_exact_declared_run_on_capabilities_without_rewriting_evidence() {
        for (model, source_evidence, avionics_types, quantity) in [
            (
                "GIA63W",
                "GIA63WNAV/COM/GPS(Dual)",
                vec!["NAV", "COM", "GPS"],
                2,
            ),
            ("GDL690A", "GDL690ADatalink", vec!["Datalink"], 1),
        ] {
            let html = controller_html(source_evidence);
            let mut payload = installed("Garmin", model, source_evidence);
            payload["avionics"][0]["types"] = serde_json::json!(avionics_types);
            payload["avionics"][0]["quantity"] = serde_json::json!(quantity);
            if quantity > 1 {
                payload["avionics"][0]["source_confidence"] = serde_json::json!("medium");
            }
            let original = payload.clone();

            let observations = validate_unbound_current_avionics_extraction(
                &payload.to_string(),
                CONTROLLER_URL,
                &html,
            )
            .unwrap();

            assert_eq!(
                observations[0].source_evidence_text.as_deref(),
                Some(source_evidence)
            );
            assert_eq!(
                recover_controller_avionics_extraction(&mut payload, CONTROLLER_URL, &html)
                    .unwrap(),
                false
            );
            assert_eq!(payload, original);
        }
    }

    #[test]
    fn submission_26_run_ons_validate_and_recover_without_a_provider_call() {
        let controller_field = "G1000 Avionics Suite\n\
GDU1044B Primary Flight Display (PFD) \n\
GDU1044B Multi-Function Display (MFD) \n\
GFC 700 Autopilot \n\
GMA1347 Digital Audio Panel\n\
GIA63WNAV/COM/GPS(Dual)\n\
GTX355R ADS-B Out Transponder \n\
GEA71 Engine/Airframe Computer\n\
GRS77 AHRS (Attitude and Heading \n\
Reference System) \n\
GDC74 Air Data Computer\n\
GMU44 Magnetometer \n\
WX 500 Stormscope\n\
GDL690ADatalink\n\
Bendix King KTA-810 TAS (Traffic \n\
Advisory System)";
        let html = controller_html(controller_field);
        let mut payload = serde_json::json!({
            "avionics": [
                {
                    "manufacturer": "Garmin",
                    "model": "GIA63W",
                    "types": ["NAV", "COM", "GPS"],
                    "quantity": 2,
                    "configuration_action": "installed",
                    "replaces": null,
                    "source_evidence_text": "GIA63WNAV/COM/GPS(Dual)",
                    "source_confidence": "medium"
                },
                {
                    "manufacturer": "Garmin",
                    "model": "GDL690A",
                    "types": ["Datalink"],
                    "quantity": 1,
                    "configuration_action": "installed",
                    "replaces": null,
                    "source_evidence_text": "GDL690ADatalink",
                    "source_confidence": "high"
                }
            ]
        });
        let original = payload.clone();

        let observations = validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            SUBMISSION_26_URL,
            &html,
        )
        .unwrap();
        assert_eq!(observations.len(), 2);
        assert_eq!(
            observations[0].source_evidence_text.as_deref(),
            Some("GIA63WNAV/COM/GPS(Dual)")
        );
        assert_eq!(
            observations[1].source_evidence_text.as_deref(),
            Some("GDL690ADatalink")
        );
        assert_eq!(
            recover_controller_avionics_extraction(&mut payload, SUBMISSION_26_URL, &html).unwrap(),
            false
        );
        assert_eq!(payload, original);
    }

    #[test]
    fn controller_run_on_identity_requires_the_complete_declared_capability_suffix() {
        for (source_evidence, avionics_types, quantity) in [
            ("GIA63WWhatever", vec!["NAV"], 1),
            ("GIA63WNXi", vec!["NAV"], 1),
            ("GIA63WInstalled", vec!["NAV"], 1),
            ("GIA63WNAV", vec!["GPS"], 1),
            ("GIA63WNAVX", vec!["NAV"], 1),
            ("GIA63WNAV extra", vec!["NAV"], 1),
            ("GIA63WNAV/COM/GPS", vec!["NAV", "COM"], 1),
        ] {
            let html = controller_html(source_evidence);
            let mut payload = installed("Garmin", "GIA63W", source_evidence);
            payload["avionics"][0]["types"] = serde_json::json!(avionics_types);
            payload["avionics"][0]["quantity"] = serde_json::json!(quantity);

            let error = validate_unbound_current_avionics_extraction(
                &payload.to_string(),
                CONTROLLER_URL,
                &html,
            )
            .unwrap_err();

            assert!(
                error.contains("candidate identity"),
                "{source_evidence:?}: {error}"
            );
        }

        let source_evidence = "GIA63WNAV/COM/GPS(Dual)";
        let html = format!("<html><body><p>{source_evidence}</p></body></html>");
        let mut payload = installed("Garmin", "GIA63W", source_evidence);
        payload["avionics"][0]["types"] = serde_json::json!(["NAV", "COM", "GPS"]);
        payload["avionics"][0]["quantity"] = serde_json::json!(2);
        payload["avionics"][0]["source_confidence"] = serde_json::json!("medium");
        assert!(validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            GENERIC_URL,
            &html,
        )
        .unwrap_err()
        .contains("candidate identity"));

        let html = controller_html("GIA63WNAV/COM/GPS(Dual)");
        let mut truncated = installed("Garmin", "GIA63W", "GIA63WNAV");
        truncated["avionics"][0]["types"] = serde_json::json!(["NAV"]);
        assert!(validate_unbound_current_avionics_extraction(
            &truncated.to_string(),
            CONTROLLER_URL,
            &html,
        )
        .unwrap_err()
        .contains("candidate identity"));
    }

    #[test]
    fn controller_run_on_capability_grammar_also_binds_replacement_identity() {
        let source_evidence = "Garmin GTN 750Xi replaces Garmin GNS530WNAV";
        let html = controller_html(source_evidence);
        let mut payload = replacement(source_evidence);
        payload["avionics"][0]["replaces"]["types"] = serde_json::json!(["NAV"]);

        validate_unbound_current_avionics_extraction(&payload.to_string(), CONTROLLER_URL, &html)
            .unwrap();

        payload["avionics"][0]["replaces"]["types"] = serde_json::json!(["GPS"]);
        assert!(validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CONTROLLER_URL,
            &html,
        )
        .unwrap_err()
        .contains("exact replacement identity"));
    }

    #[test]
    fn controller_accepts_attached_w_slash_waas_with_exact_capabilities() {
        let evidence = "GARMIN GNS-430W/WAAS GPS/NAV/COM";
        let html = controller_html(evidence);
        let mut payload = installed("Garmin", "GNS-430W", evidence);
        payload["avionics"][0]["types"] = serde_json::json!(["GPS", "NAV", "COM"]);

        let parsed = validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CONTROLLER_URL,
            &html,
        )
        .unwrap();

        assert_eq!(parsed[0].model, "GNS-430W");
        assert_eq!(parsed[0].avionics_types, ["GPS", "NAV", "COM"]);
        let context = ListingEvidenceContext::from_cleaned_text(evidence);
        assert_eq!(
            context.unique_exact_product_slice("Garmin", "GNS-430W"),
            None,
            "the extraction exception must not weaken local catalog reuse"
        );
        assert_eq!(context.unique_exact_model_slice("GNS-430W"), None);

        let generic_html = format!("<html><body><p>{evidence}</p></body></html>");
        assert!(validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            GENERIC_URL,
            &generic_html,
        )
        .unwrap_err()
        .contains("candidate identity"));
    }

    #[test]
    fn controller_slash_waas_grammar_rejects_inexact_identity_and_capabilities() {
        for (model, evidence, avionics_types, quantity) in [
            (
                "GNS-430W",
                "GARMIN GNS-430W/WAAS GPS/NAV/COM",
                vec!["GPS", "NAV"],
                1,
            ),
            (
                "GNS-430W",
                "GARMIN GNS-430W/WAAS GPS/NAV/COMs",
                vec!["GPS", "NAV", "COM"],
                1,
            ),
            (
                "GNS-430",
                "GARMIN GNS-430W/WAAS GPS/NAV/COM",
                vec!["GPS", "NAV", "COM"],
                1,
            ),
            (
                "GNS-430W",
                "GARMIN GNS-430W/WAAS GPS/NAV/COM",
                vec!["GPS", "NAV", "COM"],
                2,
            ),
            (
                "GNS-430W",
                "GARMIN GNS-430W/WAAS GPS NAV COM",
                vec!["GPS", "NAV", "COM"],
                1,
            ),
        ] {
            let html = controller_html(evidence);
            let mut payload = installed("Garmin", model, evidence);
            payload["avionics"][0]["types"] = serde_json::json!(avionics_types);
            payload["avionics"][0]["quantity"] = serde_json::json!(quantity);

            let error = validate_unbound_current_avionics_extraction(
                &payload.to_string(),
                CONTROLLER_URL,
                &html,
            )
            .unwrap_err();
            assert!(
                error.contains("candidate identity"),
                "{evidence:?}: {error}"
            );
        }
    }

    #[test]
    fn controller_accepts_dual_waas_capabilities_with_one_final_plural() {
        let evidence = "Dual Garmin GIA-63W WAAS GPS/NAV/COMs";
        let html = controller_html(evidence);
        let mut payload = installed("Garmin", "GIA-63W", evidence);
        payload["avionics"][0]["types"] = serde_json::json!(["GPS", "NAV", "COM"]);
        payload["avionics"][0]["quantity"] = serde_json::json!(2);
        payload["avionics"][0]["source_confidence"] = serde_json::json!("medium");

        let parsed = validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CONTROLLER_URL,
            &html,
        )
        .unwrap();

        assert_eq!(parsed[0].model, "GIA-63W");
        assert_eq!(parsed[0].quantity, 2);
        assert_eq!(parsed[0].avionics_types, ["GPS", "NAV", "COM"]);
        let context = ListingEvidenceContext::from_cleaned_text(evidence);
        assert_eq!(
            context.unique_exact_product_slice("Garmin", "GIA-63W"),
            None,
            "the extraction exception must not weaken local catalog reuse"
        );
        assert_eq!(context.unique_exact_model_slice("GIA-63W"), None);

        let generic_html = format!("<html><body><p>{evidence}</p></body></html>");
        assert!(validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            GENERIC_URL,
            &generic_html,
        )
        .unwrap_err()
        .contains("candidate identity"));
    }

    #[test]
    fn controller_dual_plural_waas_grammar_rejects_near_matches() {
        for (evidence, avionics_types, quantity) in [
            (
                "Dual Garmin GIA-63W WAAS GPS/NAV/COMs",
                vec!["GPS", "NAV", "COM"],
                1,
            ),
            (
                "Triple Garmin GIA-63W WAAS GPS/NAV/COMs",
                vec!["GPS", "NAV", "COM"],
                2,
            ),
            (
                "Dual Garmin GIA-63W WAAS GPSs/NAV/COMs",
                vec!["GPS", "NAV", "COM"],
                2,
            ),
            (
                "Dual Garmin GIA-63W WAAS GPS/NAV/COMss",
                vec!["GPS", "NAV", "COM"],
                2,
            ),
            (
                "Dual Garmin GIA-63W WAAS GPS/NAV/COMs",
                vec!["GPS", "NAV"],
                2,
            ),
            (
                "Dual Garmin GIA-63W WAAS GPS/NAV/COM",
                vec!["GPS", "NAV", "COM"],
                2,
            ),
        ] {
            let html = controller_html(evidence);
            let mut payload = installed("Garmin", "GIA-63W", evidence);
            payload["avionics"][0]["types"] = serde_json::json!(avionics_types);
            payload["avionics"][0]["quantity"] = serde_json::json!(quantity);

            let error = validate_unbound_current_avionics_extraction(
                &payload.to_string(),
                CONTROLLER_URL,
                &html,
            )
            .unwrap_err();
            assert!(
                error.contains("candidate identity"),
                "{evidence:?}: {error}"
            );
        }
    }

    #[test]
    fn visible_evidence_accepts_standalone_waas_before_declared_capabilities() {
        let evidence = "Garmin GTN 750 WAAS GPS/NAV/COM";
        let html = format!("<html><body><p>{evidence}</p></body></html>");
        let mut payload = installed("Garmin", "GTN 750", evidence);
        payload["avionics"][0]["types"] = serde_json::json!(["GPS", "NAV", "COM"]);

        let parsed =
            validate_unbound_current_avionics_extraction(&payload.to_string(), GENERIC_URL, &html)
                .unwrap();

        assert_eq!(parsed[0].model, "GTN 750");
        assert_eq!(parsed[0].avionics_types, ["GPS", "NAV", "COM"]);
    }

    #[test]
    fn visible_evidence_accepts_attached_w_waas_ifr_and_rebuilt_date() {
        let evidence = "GARMIN GNS 530W WAAS IFR GPS/NAV/COM-REBUILT 1/23";
        let html = format!("<html><body><p>{evidence}</p></body></html>");
        let mut payload = installed("Garmin", "GNS 530W", evidence);
        payload["avionics"][0]["types"] = serde_json::json!(["GPS", "NAV", "COM"]);

        let parsed =
            validate_unbound_current_avionics_extraction(&payload.to_string(), GENERIC_URL, &html)
                .unwrap();

        assert_eq!(parsed[0].model, "GNS 530W");
        assert_eq!(parsed[0].avionics_types, ["GPS", "NAV", "COM"]);
        let context = ListingEvidenceContext::from_cleaned_text(evidence);
        assert_eq!(
            context.unique_exact_product_slice("Garmin", "GNS 530W"),
            None,
            "extraction-only annotation admission must not weaken local catalog reuse"
        );
        assert_eq!(context.unique_exact_model_slice("GNS 530W"), None);
    }

    #[test]
    fn standalone_waas_annotation_requires_exact_slash_delimited_declared_capabilities() {
        for (model, evidence, avionics_types) in [
            ("GNS 430", "Garmin GNS 430 WAAS upgraded", vec!["GPS"]),
            ("GNS 430", "Garmin GNS 430 WAAS", vec!["GPS"]),
            ("GNS 430", "Garmin GNS 430 WAAS GPS", vec!["GPS"]),
            ("GTN 750", "Garmin GTN 750 WAAS random", vec!["GPS"]),
            (
                "GTN 750",
                "Garmin GTN 750 WAAS GPS/NAV/COM",
                vec!["GPS", "NAV"],
            ),
            (
                "GTN 750",
                "Garmin GTN 750 WAAS GPS NAV COM",
                vec!["GPS", "NAV", "COM"],
            ),
            (
                "GNS 530W",
                "Garmin GNS 530W WAAS VFR GPS/NAV/COM-REBUILT 1/23",
                vec!["GPS", "NAV", "COM"],
            ),
            (
                "GTN 750",
                "Garmin GTN 750 WAAS IFR GPS/NAV/COM",
                vec!["GPS", "NAV", "COM"],
            ),
            (
                "GNS 530W",
                "Garmin GNS 530W WAAS IFR GPS NAV COM-REBUILT 1/23",
                vec!["GPS", "NAV", "COM"],
            ),
            (
                "GNS 530W",
                "Garmin GNS 530W WAAS IFR GPS/NAV/COM-NXi",
                vec!["GPS", "NAV", "COM"],
            ),
            (
                "GNS 530W",
                "Garmin GNS 530W WAAS IFR GPS/NAV/COM-REPLACED BY GTN 750",
                vec!["GPS", "NAV", "COM"],
            ),
            (
                "GNS 530W",
                "Garmin GNS 530W WAAS IFR GPS/NAV/COM-REBUILT JAN 2023",
                vec!["GPS", "NAV", "COM"],
            ),
            (
                "GNS 530W",
                "Garmin GNS 530W WAAS IFR GPS/NAV/COM-REBUILT 1/23",
                vec!["GPS", "NAV"],
            ),
        ] {
            let html = format!("<html><body><p>{evidence}</p></body></html>");
            let mut payload = installed("Garmin", model, evidence);
            payload["avionics"][0]["types"] = serde_json::json!(avionics_types);

            assert!(
                validate_unbound_current_avionics_extraction(
                    &payload.to_string(),
                    GENERIC_URL,
                    &html,
                )
                .unwrap_err()
                .contains("candidate identity"),
                "{evidence:?} must not cross the narrow annotation grammar"
            );
        }
    }

    #[test]
    fn rejects_scalar_types_implicit_semantics_and_stale_capture_bindings() {
        let html = "<p>Garmin G5 installed</p>";
        let hash = sha256_hex(html.as_bytes());
        let scalar = r#"{"avionics":[{"manufacturer":"Garmin","model":"G5","type":"Flight Display","quantity":1,"configuration_action":"installed","replaces":null,"source_evidence_text":"Garmin G5 installed","source_confidence":"high"}]}"#;
        assert!(
            validate_current_avionics_extraction(bound(html, &hash, scalar))
                .unwrap_err()
                .contains("scalar type payloads")
        );

        let implicit = r#"{"avionics":[{"manufacturer":"Garmin","model":"G5","types":["Flight Display"],"source_evidence_text":"Garmin G5 installed","source_confidence":"high"}]}"#;
        assert!(
            validate_current_avionics_extraction(bound(html, &hash, implicit))
                .unwrap_err()
                .contains("quantity must be an explicit")
        );

        let current = r#"{"avionics":[]}"#;
        let mut stale = bound(html, &hash, current);
        stale.submission_canonical_listing_id = Some(8);
        assert!(validate_current_avionics_extraction(stale)
            .unwrap_err()
            .contains("exact canonical listing"));

        let mut unbound = bound(html, &hash, current);
        unbound.submission_canonical_listing_id = None;
        assert!(validate_current_avionics_extraction(unbound)
            .unwrap_err()
            .contains("exact canonical listing"));
    }

    #[test]
    fn rejects_hidden_or_hash_mismatched_evidence() {
        let html = "<body><p hidden>Garmin G5 installed</p></body>";
        let hash = sha256_hex(html.as_bytes());
        let json = r#"{"avionics":[{"manufacturer":"Garmin","model":"G5","types":["Flight Display"],"quantity":1,"configuration_action":"installed","replaces":null,"source_evidence_text":"Garmin G5 installed","source_confidence":"high"}]}"#;
        assert!(
            validate_current_avionics_extraction(bound(html, &hash, json))
                .unwrap_err()
                .contains("structurally visible")
        );

        assert!(
            validate_current_avionics_extraction(bound(html, &"0".repeat(64), json))
                .unwrap_err()
                .contains("SHA-256")
        );
    }

    #[test]
    fn replacement_identity_must_be_present_in_the_same_exact_evidence() {
        let html = "<p>Garmin GTN 750Xi installed</p>";
        let hash = sha256_hex(html.as_bytes());
        let missing_target = r#"{"avionics":[{"manufacturer":"Garmin","model":"GTN 750Xi","types":["GPS"],"quantity":1,"configuration_action":"replaces","replaces":{"manufacturer":"Garmin","model":"GNS 530W","types":["GPS"]},"source_evidence_text":"Garmin GTN 750Xi installed","source_confidence":"high"}]}"#;
        assert!(
            validate_current_avionics_extraction(bound(html, &hash, missing_target))
                .unwrap_err()
                .contains("exact replacement identity")
        );

        let html = "<p>Garmin GTN 750Xi replaces Garmin GNS 530W</p>";
        let hash = sha256_hex(html.as_bytes());
        let exact = r#"{"avionics":[{"manufacturer":"Garmin","model":"GTN 750Xi","types":["GPS"],"quantity":1,"configuration_action":"replaces","replaces":{"manufacturer":"Garmin","model":"GNS 530W","types":["GPS"]},"source_evidence_text":"Garmin GTN 750Xi replaces Garmin GNS 530W","source_confidence":"high"}]}"#;
        assert_eq!(
            validate_current_avionics_extraction(bound(html, &hash, exact))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn controller_recovery_copies_hyphen_case_and_entity_typography() {
        for (field, hint, expected) in [
            ("Garmin GMA-1347", "Garmin GMA 1347", "Garmin GMA-1347"),
            ("GARMIN GMA 1347", "Garmin GMA 1347", "GARMIN GMA 1347"),
            ("Garmin GMA&#45;1347", "Garmin GMA 1347", "Garmin GMA-1347"),
        ] {
            let html = controller_html(field);
            let mut payload = installed("Garmin", "GMA 1347", hint);

            assert!(recover_controller_avionics_evidence_typography(
                &mut payload,
                CONTROLLER_URL,
                &html,
            )
            .unwrap());
            assert_eq!(evidence(&payload), expected);
        }
    }

    #[test]
    fn already_visible_evidence_stays_byte_identical() {
        let html = controller_html("Garmin GMA-1347");
        let mut payload = installed("Garmin", "GMA 1347", "Garmin GMA-1347");
        let original = payload.clone();

        assert!(!recover_controller_avionics_evidence_typography(
            &mut payload,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert_eq!(payload, original);
    }

    #[test]
    fn controller_recovery_copies_one_exact_capability_line_wrap() {
        let flattened = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W NAV/COM/GPS/WAAS with GS #2";
        let exact = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W\nNAV/COM/GPS/WAAS with GS #2";
        let html = controller_html(exact);
        let mut payload = installed("Garmin", "GIA-63W", flattened);
        payload["avionics"][0]["types"] = serde_json::json!(["NAV", "COM", "GPS"]);
        payload["avionics"][0]["quantity"] = serde_json::json!(2);
        payload["avionics"][0]["source_confidence"] = serde_json::json!("medium");
        payload["aircraft_marker"] = serde_json::json!({
            "serial": "unchanged",
            "nested": [1, 2, 3]
        });
        let mut expected = payload.clone();
        expected["avionics"][0]["source_evidence_text"] = Value::String(exact.to_string());

        assert!(listing_body_contains_exact_structurally_visible_text_span(
            &html, flattened
        ));
        assert!(listing_body_contains_exact_structurally_visible_text_span(
            &html, exact
        ));
        assert!(validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CONTROLLER_URL,
            &html,
        )
        .unwrap_err()
        .contains("bounded source excerpt"));

        assert!(recover_controller_avionics_evidence_typography(
            &mut payload,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert_eq!(payload, expected);
        assert_eq!(evidence(&payload), exact);
        validate_unbound_current_avionics_extraction(&payload.to_string(), CONTROLLER_URL, &html)
            .unwrap();
    }

    #[test]
    fn controller_evidence_recovery_rolls_back_when_quantity_remains_ambiguous() {
        let flattened = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W NAV/COM/GPS/WAAS with GS #2";
        let exact = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W\nNAV/COM/GPS/WAAS with GS #2";
        let html = controller_html(&format!("Garmin G5 attitude\nGarmin G5 HSI\n{exact}"));
        let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
        let mut gia = installed("Garmin", "GIA-63W", flattened)["avionics"][0].clone();
        gia["types"] = serde_json::json!(["NAV", "COM", "GPS"]);
        gia["quantity"] = serde_json::json!(2);
        gia["source_confidence"] = serde_json::json!("medium");
        payload["avionics"].as_array_mut().unwrap().push(gia);
        payload["aircraft_marker"] = serde_json::json!({
            "serial": "unchanged",
            "nested": [1, 2, 3]
        });
        let original = payload.clone();

        let error = recover_controller_avionics_extraction(&mut payload, CONTROLLER_URL, &html)
            .unwrap_err();

        assert!(
            error.contains("ambiguous Controller quantity evidence"),
            "{error}"
        );
        assert_eq!(payload, original);
    }

    #[test]
    fn controller_line_wrap_recovery_rejects_product_boundaries_and_ambiguous_layouts() {
        let cross_product_exact = "GIA-63W\nGarmin GMA-1347";
        let html = controller_html(cross_product_exact);
        let mut cross_product = installed("Garmin", "GIA-63W", "GIA-63W Garmin GMA-1347");
        cross_product["avionics"][0]["types"] = serde_json::json!(["NAV", "COM", "GPS"]);
        let original = cross_product.clone();

        assert!(!recover_controller_avionics_evidence_typography(
            &mut cross_product,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert_eq!(cross_product, original);

        let flattened = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W NAV/COM/GPS/WAAS with GS #2";
        let first = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W\nNAV/COM/GPS/WAAS with GS #2";
        let html = controller_html(&format!("{first}\n{first}"));
        let mut ambiguous = installed("Garmin", "GIA-63W", flattened);
        ambiguous["avionics"][0]["types"] = serde_json::json!(["NAV", "COM", "GPS"]);
        ambiguous["avionics"][0]["quantity"] = serde_json::json!(2);
        let original = ambiguous.clone();

        assert!(!recover_controller_avionics_evidence_typography(
            &mut ambiguous,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert_eq!(ambiguous, original);
    }

    #[test]
    fn recovery_never_adds_or_removes_manufacturer_tokens() {
        for (field, hint) in [
            ("Garmin GMA-1347", "BendixKing Garmin GMA 1347"),
            ("GMA-1347", "Garmin GMA 1347"),
        ] {
            let html = controller_html(field);
            let mut payload = installed("Garmin", "GMA 1347", hint);
            let original = payload.clone();

            assert!(!recover_controller_avionics_evidence_typography(
                &mut payload,
                CONTROLLER_URL,
                &html,
            )
            .unwrap());
            assert_eq!(payload, original);
        }
    }

    #[test]
    fn meaningful_product_suffixes_are_not_typography() {
        for (model, hint, field) in [
            ("G1000", "Garmin G1000", "Garmin G1000 NXi"),
            ("GDC 74", "Garmin GDC 74", "Garmin GDC-74A"),
            ("GTX 33", "Garmin GTX 33", "Garmin GTX 33 ES"),
            ("GNS 430", "Garmin GNS 430", "Garmin GNS-430W"),
        ] {
            let html = controller_html(field);
            let mut payload = installed("Garmin", model, hint);
            let original = payload.clone();

            assert!(!recover_controller_avionics_evidence_typography(
                &mut payload,
                CONTROLLER_URL,
                &html,
            )
            .unwrap());
            assert_eq!(payload, original, "{field:?} must fail closed");
        }
    }

    #[test]
    fn distinct_or_repeated_controller_spellings_fail_closed() {
        let html = controller_html("Garmin GMA-1347\nGarmin GMA 1347");
        let mut ambiguous = installed("Garmin", "GMA 1347", "garmin gma1347");
        let original = ambiguous.clone();
        assert!(!recover_controller_avionics_evidence_typography(
            &mut ambiguous,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert_eq!(ambiguous, original);

        let html = controller_html("Garmin GMA-1347\nGarmin GMA-1347");
        let mut repeated = installed("Garmin", "GMA 1347", "garmin gma1347");
        let original = repeated.clone();
        assert!(recover_controller_avionics_evidence_typography(
            &mut repeated,
            CONTROLLER_URL,
            &html,
        )
        .is_err());
        assert_eq!(repeated, original);
    }

    #[test]
    fn hidden_metadata_script_and_multiple_or_invalid_fields_fail_closed() {
        let cases = [
            controller_html("<span hidden>Garmin GMA-1347</span>"),
            controller_html("<script>Garmin GMA-1347</script>Not the product"),
            r#"<html><head><meta content="Garmin GMA-1347"></head><body>Not the product</body></html>"#
                .to_string(),
            r#"<html><body>
               <div class="detail__specs-wrapper">
                 <div class="detail__specs-label">Avionics/Radios</div>
                 <div class="detail__specs-value">Garmin GMA-1347</div>
               </div>
               <div class="detail__specs-wrapper">
                 <div class="detail__specs-label">Avionics/Radios</div>
                 <div class="detail__specs-value">Garmin GMA-1347</div>
               </div>
               </body></html>"#
                .to_string(),
            r#"<html><body>
               <div class="detail__specs-wrapper">
                 <div class="detail__specs-label">Avionics/Radios</div>
                 <div class="detail__specs-value">Garmin GMA-1347</div>
               </div>
               <div class="detail__specs-wrapper">
                 <div class="detail__specs-label" hidden>Avionics/Radios</div>
                 <div class="detail__specs-value">Garmin GMA-1347</div>
               </div>
               </body></html>"#
                .to_string(),
            r#"<html><body>
               <div class="detail__specs-wrapper">
                 <div class="detail__specs-label">Avionics/Radios</div>
                 <div class="detail__specs-value">Garmin GMA-1347</div>
               </div>
               <div class="detail__specs-wrapper">
                 <div class="detail__specs-label">Avionics/Radios<span hidden>x</span></div>
                 <div class="detail__specs-value">Garmin GMA-1347</div>
               </div>
               </body></html>"#
                .to_string(),
            r#"<html><body><div class="detail__specs-wrapper">
                 <div class="detail__specs-label">Avionics/Radios</div>
                 <div class="detail__specs-value">Garmin GMA-1347</div>
                 <div class="detail__specs-value">Garmin GMA-1347</div>
               </div></body></html>"#
                .to_string(),
        ];
        for html in cases {
            let mut payload = installed("Garmin", "GMA 1347", "Garmin GMA 1347");
            let original = payload.clone();
            let outcome = recover_controller_avionics_evidence_typography(
                &mut payload,
                CONTROLLER_URL,
                &html,
            );
            assert!(outcome.is_err() || outcome == Ok(false));
            assert_eq!(payload, original);
        }
    }

    #[test]
    fn recovery_is_controller_only_and_keeps_the_evidence_byte_bound() {
        let html = controller_html("Garmin GMA-1347");
        let mut wrong_source = installed("Garmin", "GMA 1347", "Garmin GMA 1347");
        let original = wrong_source.clone();
        assert!(!recover_controller_avionics_evidence_typography(
            &mut wrong_source,
            "https://example.test/listing/257737897",
            &html,
        )
        .unwrap());
        assert_eq!(wrong_source, original);

        let long_suffix = "x".repeat(MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES);
        let field = format!("Garmin GMA-1347 {long_suffix}");
        let hint = format!("Garmin GMA 1347 {long_suffix}");
        let html = controller_html(&field);
        let mut oversized = installed("Garmin", "GMA 1347", &hint);
        let original = oversized.clone();
        assert!(recover_controller_avionics_evidence_typography(
            &mut oversized,
            CONTROLLER_URL,
            &html,
        )
        .unwrap_err()
        .contains("bounded listing-evidence limit"));
        assert_eq!(oversized, original);
    }

    #[test]
    fn replacement_recovery_requires_both_identities_in_one_contiguous_span() {
        let html = controller_html("Garmin GTN-750Xi replaces Garmin GNS-530W");
        let mut payload = replacement("Garmin GTN 750Xi replaces Garmin GNS 530W");
        assert!(recover_controller_avionics_evidence_typography(
            &mut payload,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert_eq!(
            evidence(&payload),
            "Garmin GTN-750Xi replaces Garmin GNS-530W"
        );

        for (field, hint) in [
            (
                "Garmin GTN-750Xi\nreplaces Garmin GNS-530W",
                "Garmin GTN 750Xi replaces Garmin GNS 530W",
            ),
            ("Garmin GTN-750Xi\nGarmin GNS-530W", "Garmin GTN 750Xi"),
        ] {
            let html = controller_html(field);
            let mut payload = replacement(hint);
            let original = payload.clone();
            assert!(!recover_controller_avionics_evidence_typography(
                &mut payload,
                CONTROLLER_URL,
                &html,
            )
            .unwrap());
            assert_eq!(payload, original);
        }
    }

    #[test]
    fn recovery_changes_no_non_evidence_value() {
        let html = controller_html("Garmin GMA-1347");
        let mut payload = installed("Garmin", "GMA 1347", "garmin gma 1347");
        payload["aircraft_marker"] = serde_json::json!({
            "manufacturer": "Cessna",
            "serial": "unchanged",
            "nested": [1, 2, 3]
        });
        let mut expected = payload.clone();
        expected["avionics"][0]["source_evidence_text"] =
            Value::String("Garmin GMA-1347".to_string());

        assert!(recover_controller_avionics_evidence_typography(
            &mut payload,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert_eq!(payload, expected);
    }

    #[test]
    fn structural_failures_are_not_recovered() {
        let html = controller_html("Garmin GMA-1347");
        let mut cases = Vec::new();
        for (field, value, expected_error) in [
            ("manufacturer", Value::String(String::new()), "manufacturer"),
            (
                "quantity",
                Value::String("one".to_string()),
                "quantity must be an explicit integer",
            ),
            (
                "configuration_action",
                Value::String("upgrades".to_string()),
                "configuration_action",
            ),
            (
                "source_confidence",
                Value::String("certain".to_string()),
                "source_confidence",
            ),
        ] {
            let mut payload = installed("Garmin", "GMA 1347", "Garmin GMA 1347");
            payload["avionics"][0][field] = value;
            cases.push((payload, expected_error));
        }
        let mut invalid_types = installed("Garmin", "GMA 1347", "Garmin GMA 1347");
        invalid_types["avionics"][0]["types"] = Value::String("Flight Display".to_string());
        cases.push((invalid_types, "scalar type payloads"));
        let mut invalid_replacement = installed("Garmin", "GMA 1347", "Garmin GMA 1347");
        invalid_replacement["avionics"][0]["configuration_action"] =
            Value::String("replaces".to_string());
        cases.push((invalid_replacement, "requires one replacement object"));

        for (mut payload, expected_error) in cases {
            let original = payload.clone();
            assert!(recover_controller_avionics_evidence_typography(
                &mut payload,
                CONTROLLER_URL,
                &html,
            )
            .unwrap_err()
            .contains(expected_error));
            assert_eq!(payload, original);
        }
    }
}
