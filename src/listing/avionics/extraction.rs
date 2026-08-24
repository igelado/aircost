//! Strict current-schema avionics extraction boundary.
//!
//! The retained extraction is derived data, but it is usable only while its
//! exact signed source capture remains bound to the same owner, listing, and
//! source URL. This module deliberately has no legacy parser or scalar
//! capability fallback.

use std::collections::BTreeSet;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::extract::CURATED_AVIONICS_TYPES;
use crate::html::clean::{
    clean_publisher_source_html, listing_body_contains_exact_structurally_visible_text_span,
    normalize_source_evidence_span,
};
use crate::html::listing::source::listing_evidence_units;
use crate::listing::evidence::{
    controller_avionics_evidence, identity_span_has_boundaries, ListingEvidenceContext,
    MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES,
};
use crate::models::ParsedAvionics;

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
) -> Result<Vec<ParsedAvionics>, String> {
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
) -> Result<Vec<ParsedAvionics>, String> {
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
) -> Result<(), String> {
    validate_current_avionics_identity_evidence(
        observations,
        listing_context,
        source_url,
        rendered_html,
    )?;
    validate_current_avionics_type_scope(observations)?;
    validate_current_avionics_quantity_completeness(observations, source_url, rendered_html)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ControllerAvionicsExtractionRecovery {
    pub quantity_recovered: bool,
    pub evidence_recovered: bool,
}

/// Apply every deterministic Controller extraction repair as one atomic
/// mutation. A payload that needs more than one scoped repair is validated
/// only after all qualifying mutations have been applied. Any scoped error or
/// final extraction-contract failure restores the byte-for-byte input value.
pub(crate) fn recover_controller_avionics_extraction(
    extracted_listing: &mut Value,
    source_url: &str,
    rendered_html: &str,
) -> Result<ControllerAvionicsExtractionRecovery, String> {
    let original = extracted_listing.clone();
    let recovery = (|| {
        let quantity_recovered = apply_exact_role_separated_avionics_quantity(
            extracted_listing,
            source_url,
            rendered_html,
        )?;
        let evidence_recovered = apply_controller_avionics_evidence_typography(
            extracted_listing,
            source_url,
            rendered_html,
        )?;
        let recovery = ControllerAvionicsExtractionRecovery {
            quantity_recovered,
            evidence_recovered,
        };
        if quantity_recovered || evidence_recovered {
            validate_unbound_current_avionics_extraction(
                &extracted_listing.to_string(),
                source_url,
                rendered_html,
            )?;
        }
        Ok(recovery)
    })();
    if recovery.is_err() {
        *extracted_listing = original;
    }
    recovery
}

/// Recover one under-counted display product from an exact role-separated
/// equipment enumeration.
///
/// This is deliberately not general mention counting. One structurally valid
/// Controller `Avionics/Radios` field must contain one unambiguous, comma- or
/// semicolon-delimited pair of the same complete manufacturer/model identity,
/// and the two list items must name complementary physical installation roles
/// such as `attitude` and `HSI`. Qualifying prose and arbitrary prefixes are
/// rejected. The model must already have emitted exactly one ordinary
/// installed occurrence with quantity one and evidence equal to one of those
/// two list items. Only its quantity and exact evidence locator are replaced.
fn apply_exact_role_separated_avionics_quantity(
    extracted_listing: &mut Value,
    source_url: &str,
    rendered_html: &str,
) -> Result<bool, String> {
    let observations = parse_current_avionics_extraction_value(extracted_listing)?;
    let Some(installed_equipment) = controller_avionics_evidence(source_url, rendered_html) else {
        return Ok(false);
    };
    let mut repairs = Vec::new();
    for (index, observation) in observations.iter().enumerate() {
        if observation.quantity != 1
            || observation.configuration_action != "installed"
            || observation.replaces.is_some()
            || observations.iter().enumerate().any(|(other_index, other)| {
                other_index != index && same_product_identity(observation, other)
            })
        {
            continue;
        }
        let RoleSeparatedQuantityEvidence::Unique(proof) = role_separated_quantity_evidence(
            &installed_equipment,
            rendered_html,
            &observation.manufacturer,
            &observation.model,
        ) else {
            continue;
        };
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence")
            .trim();
        if !proof.items.iter().any(|item| item == evidence) {
            continue;
        }
        repairs.push((index, proof.evidence));
    }

    if repairs.is_empty() {
        return Ok(false);
    }
    let avionics = extracted_listing
        .get_mut("avionics")
        .and_then(Value::as_array_mut)
        .expect("the canonical parser requires a top-level avionics array");
    for (index, evidence) in repairs {
        let occurrence = avionics[index]
            .as_object_mut()
            .expect("the canonical parser requires avionics objects");
        occurrence.insert("quantity".to_string(), Value::from(2));
        occurrence.insert("source_evidence_text".to_string(), Value::String(evidence));
    }

    Ok(true)
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
) -> Result<bool, String> {
    let observations = parse_current_avionics_extraction_value(extracted_listing)?;
    let listing_context = ListingEvidenceContext::from_rendered_html(Some(rendered_html));
    let evidence_units = listing_evidence_units(source_url, rendered_html)
        .map_err(|error| format!("listing evidence source is invalid: {error}"))?;
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
        || normalized_identity_occurrence_count(candidate, &observation.model)
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
) -> Result<Vec<ParsedAvionics>, String> {
    let value: Value = serde_json::from_str(extracted_listing_json)
        .map_err(|error| format!("retained listing extraction is invalid JSON: {error}"))?;
    parse_current_avionics_extraction_value(&value)
}

pub(crate) fn parse_current_avionics_extraction_value(
    value: &Value,
) -> Result<Vec<ParsedAvionics>, String> {
    let observations = value
        .get("avionics")
        .and_then(Value::as_array)
        .ok_or_else(|| "retained listing extraction has no top-level avionics array".to_string())?;
    observations
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let path = format!("avionics[{index}]");
            let object = value
                .as_object()
                .ok_or_else(|| format!("{path} must be an object"))?;
            validate_required_identity(object, &path)?;
            validate_capabilities(value, &path)?;
            let quantity = object
                .get("quantity")
                .and_then(Value::as_i64)
                .ok_or_else(|| format!("{path}.quantity must be an explicit integer"))?;
            if quantity < 1 {
                return Err(format!("{path}.quantity must be at least 1"));
            }
            let action = object
                .get("configuration_action")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!(
                        "{path}.configuration_action must be an explicit installed, replaces, or removes value"
                    )
                })?;
            if !matches!(action, "installed" | "replaces" | "removes") {
                return Err(format!(
                    "{path}.configuration_action must be installed, replaces, or removes"
                ));
            }
            let replacement = object.get("replaces").ok_or_else(|| {
                format!("{path}.replaces must be explicit null or one replacement object")
            })?;
            match action {
                "installed" if !replacement.is_null() => {
                    return Err(format!("{path} installed occurrence must use replaces=null"));
                }
                "replaces" | "removes" if !replacement.is_object() => {
                    return Err(format!(
                        "{path} {action} occurrence requires one replacement object"
                    ));
                }
                "replaces" | "removes" => {
                    let replacement_path = format!("{path}.replaces");
                    validate_required_identity(
                        replacement
                            .as_object()
                            .expect("replacement object was checked"),
                        &replacement_path,
                    )?;
                    validate_capabilities(replacement, &replacement_path)?;
                }
                _ => {}
            }

            let evidence = object
                .get("source_evidence_text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|evidence| !evidence.is_empty())
                .ok_or_else(|| {
                    format!(
                        "{path}.source_evidence_text must be one non-empty exact listing-source excerpt"
                    )
                })?;
            if evidence.len() > MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES {
                return Err(format!(
                    "{path}.source_evidence_text exceeds the bounded listing-evidence limit"
                ));
            }
            let confidence = object
                .get("source_confidence")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{path}.source_confidence must be high, medium, or low"))?;
            if !matches!(confidence, "high" | "medium" | "low") {
                return Err(format!(
                    "{path}.source_confidence must be high, medium, or low"
                ));
            }

            serde_json::from_value::<ParsedAvionics>(value.clone())
                .map_err(|error| format!("{path} is invalid: {error}"))
        })
        .collect()
}

pub(crate) fn validate_current_avionics_identity_evidence(
    observations: &[ParsedAvionics],
    listing_context: &ListingEvidenceContext,
    source_url: &str,
    rendered_html: &str,
) -> Result<(), String> {
    let evidence_units = listing_evidence_units(source_url, rendered_html)
        .map_err(|error| format!("listing evidence source is invalid: {error}"))?;
    let controller_field = controller_avionics_evidence(source_url, rendered_html);
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence")
            .trim();
        if !evidence_units.contains_exact_span(evidence) {
            return Err(format!(
                "avionics[{index}].source_evidence_text is not one exact structurally visible source unit in the retained capture"
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

fn validate_current_avionics_type_scope(observations: &[ParsedAvionics]) -> Result<(), String> {
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence");
        validate_occurrence_type_scope(index, "", &observation.avionics_types, evidence)?;
        if let Some(replacement) = observation.replaces.as_ref() {
            validate_occurrence_type_scope(
                index,
                ".replaces",
                &replacement.avionics_types,
                evidence,
            )?;
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
            if let Some(capability) = duplicated {
                return Err(format!(
                    "avionics[{suite_index}].types assigns {capability} to an integrated suite even though avionics[{component_index}] identifies a separate product with that capability"
                ));
            }
        }
    }
    Ok(())
}

fn validate_occurrence_type_scope(
    index: usize,
    path_suffix: &str,
    avionics_types: &[String],
    evidence: &str,
) -> Result<(), String> {
    let integrated_suite = avionics_types
        .iter()
        .any(|capability| capability == "Integrated Flight Deck");
    let unsupported_suite_capability = integrated_suite
        && avionics_types.iter().any(|capability| {
            capability != "Integrated Flight Deck"
                && !evidence_explicitly_names_capability(evidence, capability)
        });
    if unsupported_suite_capability {
        return Err(format!(
            "avionics[{index}]{path_suffix}.types assigns a capability to an integrated suite without explicit support in the same source_evidence_text; the suite identity may establish Integrated Flight Deck, but every additional category requires exact listing evidence"
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
) -> Result<(), String> {
    let Some(installed_equipment) = controller_avionics_evidence(source_url, rendered_html) else {
        return Ok(());
    };
    for (index, observation) in observations.iter().enumerate() {
        let proof = match role_separated_quantity_evidence(
            &installed_equipment,
            rendered_html,
            &observation.manufacturer,
            &observation.model,
        ) {
            RoleSeparatedQuantityEvidence::None => continue,
            RoleSeparatedQuantityEvidence::Unique(proof) => proof,
            RoleSeparatedQuantityEvidence::Ambiguous => {
                return Err(format!(
                    "avionics[{index}] has multiple distinct role-separated quantity proofs"
                ));
            }
        };
        let matching = observations
            .iter()
            .enumerate()
            .filter(|(_, candidate)| same_product_identity(observation, candidate))
            .collect::<Vec<_>>();
        if matching.len() != 1
            || observation.configuration_action != "installed"
            || observation.replaces.is_some()
            || observation.quantity != 2
        {
            return Err(format!(
                "avionics[{index}] does not preserve the exact quantity of two proved by complementary role-separated source items"
            ));
        }
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence")
            .trim();
        if !evidence.contains(&proof.evidence) {
            return Err(format!(
                "avionics[{index}].source_evidence_text does not cover both exact role-separated source items"
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayInstallationRole {
    Attitude,
    Hsi,
}

impl DisplayInstallationRole {
    fn is_complementary_to(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Attitude, Self::Hsi) | (Self::Hsi, Self::Attitude)
        )
    }
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RoleSeparatedQuantityProof {
    evidence: String,
    items: [String; 2],
}

#[derive(Debug)]
enum RoleSeparatedQuantityEvidence {
    None,
    Unique(RoleSeparatedQuantityProof),
    Ambiguous,
}

#[derive(Clone, Copy, Debug)]
struct EnumeratedItem<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    separator_after: Option<char>,
}

fn role_separated_quantity_evidence(
    visible_source: &str,
    rendered_html: &str,
    manufacturer: &str,
    model: &str,
) -> RoleSeparatedQuantityEvidence {
    let mut proofs = BTreeSet::new();
    let mut disqualified_proof = false;
    let exact_product_occurrences =
        normalized_identity_occurrence_count(visible_source, &format!("{manufacturer} {model}"));
    let exact_model_occurrences = normalized_identity_occurrence_count(visible_source, model);
    for line in visible_source.lines() {
        let items = enumerated_items(line);
        for pair in items.windows(2) {
            if !matches!(pair[0].separator_after, Some(',' | ';')) {
                continue;
            }
            let Some((first_role, first_identity_start)) = exact_display_installation_role(
                pair[0].text,
                manufacturer,
                model,
                false,
                rendered_html,
            ) else {
                continue;
            };
            let Some((second_role, second_identity_start)) = exact_display_installation_role(
                pair[1].text,
                manufacturer,
                model,
                true,
                rendered_html,
            ) else {
                continue;
            };
            if !first_role.is_complementary_to(second_role) {
                continue;
            }
            if exact_product_occurrences != 2 || exact_model_occurrences != 2 {
                disqualified_proof = true;
                continue;
            }
            let evidence_start = pair[0].start + first_identity_start;
            let evidence_end = pair[1].end;
            let evidence = line[evidence_start..evidence_end].trim();
            if evidence.len() > MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES
                || !listing_body_contains_exact_structurally_visible_text_span(
                    rendered_html,
                    evidence,
                )
            {
                continue;
            }
            proofs.insert(RoleSeparatedQuantityProof {
                evidence: evidence.to_string(),
                items: [
                    pair[0].text[first_identity_start..].trim().to_string(),
                    pair[1].text[second_identity_start..].trim().to_string(),
                ],
            });
        }
    }
    match (disqualified_proof, proofs.len()) {
        (false, 0) => RoleSeparatedQuantityEvidence::None,
        (false, 1) => RoleSeparatedQuantityEvidence::Unique(
            proofs.into_iter().next().expect("one proof exists"),
        ),
        _ => RoleSeparatedQuantityEvidence::Ambiguous,
    }
}

fn normalized_identity_occurrence_count(source: &str, identity: &str) -> usize {
    let normalized_identity = punctuation_insensitive_identity_key(identity);
    if normalized_identity.is_empty() {
        return 0;
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
        .filter(|(offset, _)| {
            let Some(source_start) = source_offsets.get(*offset).copied() else {
                return false;
            };
            let Some(last_offset) = offset.checked_add(normalized_identity.len() - 1) else {
                return false;
            };
            let Some(source_last) = source_offsets.get(last_offset).copied() else {
                return false;
            };
            let source_end = source_last
                + source[source_last..]
                    .chars()
                    .next()
                    .expect("a recorded source offset has one character")
                    .len_utf8();
            identity_span_has_boundaries(source, source_start, source_end)
        })
        .count()
}

fn enumerated_items(line: &str) -> Vec<EnumeratedItem<'_>> {
    let mut items = Vec::new();
    let mut start = 0;
    for (offset, character) in line.char_indices() {
        if !matches!(character, ',' | ';') {
            continue;
        }
        let end = offset;
        items.push(EnumeratedItem {
            text: &line[start..end],
            start,
            end,
            separator_after: Some(character),
        });
        start = offset + character.len_utf8();
    }
    items.push(EnumeratedItem {
        text: &line[start..],
        start,
        end: line.len(),
        separator_after: None,
    });
    items
}

fn exact_display_installation_role(
    item: &str,
    manufacturer: &str,
    model: &str,
    require_identity_at_start: bool,
    rendered_html: &str,
) -> Option<(DisplayInstallationRole, usize)> {
    let identity = ListingEvidenceContext::from_cleaned_text(item)
        .unique_exact_product_slice(manufacturer, model)?;
    let identity_start = item.find(&identity)?;
    let prefix = item[..identity_start].trim();
    if (require_identity_at_start && !prefix.is_empty())
        || (!require_identity_at_start && !exact_installed_equipment_prefix(prefix, rendered_html))
    {
        return None;
    }
    let role = item[identity_start + identity.len()..]
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let role = match role.as_str() {
        "attitude" | "attitude display" | "attitude indicator" | "adi" => {
            DisplayInstallationRole::Attitude
        }
        "hsi" | "horizontal situation indicator" => DisplayInstallationRole::Hsi,
        _ => return None,
    };
    Some((role, identity_start))
}

fn exact_installed_equipment_prefix(prefix: &str, rendered_html: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if prefix.len() != 4
        || !prefix.starts_with('K')
        || !prefix.bytes().all(|byte| byte.is_ascii_uppercase())
    {
        return false;
    }
    let source = clean_publisher_source_html(rendered_html);
    let lowercase_source = source.to_ascii_lowercase();
    let needle = format!("hangared at {}", prefix.to_ascii_lowercase());
    lowercase_source.match_indices(&needle).any(|(start, _)| {
        let end = start + needle.len();
        let has_boundaries = source[..start]
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_ascii_alphanumeric())
            && source[end..]
                .chars()
                .next()
                .is_none_or(|character| !character.is_ascii_alphanumeric());
        has_boundaries
            && listing_body_contains_exact_structurally_visible_text_span(
                rendered_html,
                &source[start..end],
            )
    })
}

fn validate_current_avionics_identity_evidence_occurrence(
    observation: &ParsedAvionics,
    index: usize,
    listing_context: &ListingEvidenceContext,
    exact_visible_evidence_locator: &str,
    controller_field: Option<&str>,
) -> Result<(), String> {
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
    let controller_run_on_identity = controller_field.is_some_and(|field| {
        controller_field_has_exact_evidence_line(field, evidence)
            && exact_controller_run_on_capability_annotation(
                evidence,
                &observation.model,
                &observation.avionics_types,
                observation.quantity == 2,
            )
    });
    if (!bounded_source.contains(evidence) && !controller_run_on_identity)
        || !(extraction_occurrence_has_exact_identity(
            &evidence_context,
            &observation.manufacturer,
            &observation.model,
            &observation.avionics_types,
            evidence,
        ) || controller_run_on_identity)
    {
        return Err(format!(
            "avionics[{index}].source_evidence_text is not one exact bounded source excerpt containing the candidate identity"
        ));
    }
    if let Some(replacement) = observation.replaces.as_ref() {
        let replacement_run_on_identity = controller_field.is_some_and(|field| {
            controller_field_has_exact_evidence_line(field, evidence)
                && exact_controller_run_on_capability_annotation(
                    evidence,
                    &replacement.model,
                    &replacement.avionics_types,
                    false,
                )
        });
        if !(extraction_occurrence_has_exact_identity(
            &evidence_context,
            &replacement.manufacturer,
            &replacement.model,
            &replacement.avionics_types,
            evidence,
        ) || replacement_run_on_identity)
        {
            return Err(format!(
                "avionics[{index}].source_evidence_text does not contain the exact replacement identity from avionics[{index}].replaces"
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
/// apart from Controller's exact `(Dual)` annotation on a quantity-two
/// occurrence.
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

fn validate_capture_binding(extraction: CurrentAvionicsExtraction<'_>) -> Result<(), String> {
    if extraction.listing_id <= 0 || extraction.submission_id <= 0 {
        return Err("listing and retained submission IDs must be positive".to_string());
    }
    if extraction.listing_owner_user_id <= 0
        || extraction.submission_owner_user_id != extraction.listing_owner_user_id
    {
        return Err("retained submission does not belong to the listing owner".to_string());
    }
    if extraction.submission_canonical_listing_id != Some(extraction.listing_id) {
        return Err("retained submission is not bound to the exact canonical listing".to_string());
    }
    let listing_source_url = extraction
        .listing_source_url
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .ok_or_else(|| "listing has no source URL for its retained extraction".to_string())?;
    if extraction.submission_source_url.trim().is_empty()
        || extraction.submission_source_url != listing_source_url
    {
        return Err(
            "retained submission source URL does not exactly match the listing source URL"
                .to_string(),
        );
    }
    if extraction.rendered_html.is_empty() {
        return Err("retained submission has no rendered HTML".to_string());
    }
    if !valid_sha256(extraction.rendered_html_sha256)
        || sha256_hex(extraction.rendered_html.as_bytes()) != extraction.rendered_html_sha256
    {
        return Err("retained submission rendered HTML failed its SHA-256 binding".to_string());
    }
    if extraction.extracted_listing_json.trim().is_empty() {
        return Err("retained submission has no extracted listing JSON".to_string());
    }
    Ok(())
}

fn validate_required_identity(
    object: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), String> {
    for field in ["manufacturer", "model"] {
        if object
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            return Err(format!("{path}.{field} must be a non-empty string"));
        }
    }
    Ok(())
}

fn validate_capabilities(value: &Value, path: &str) -> Result<(), String> {
    let Some(types) = value.get("types").and_then(Value::as_array) else {
        return Err(format!(
            "{path}.types must be a non-empty array; scalar type payloads are intentionally unsupported"
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
        return Err(format!(
            "{path}.types must contain distinct current curated capabilities, or Unknown by itself"
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

    fn recover_exact_role_separated_avionics_quantity(
        payload: &mut Value,
        source_url: &str,
        rendered_html: &str,
    ) -> Result<bool, String> {
        recover_controller_avionics_extraction(payload, source_url, rendered_html)
            .map(|recovery| recovery.quantity_recovered)
    }

    fn recover_controller_avionics_evidence_typography(
        payload: &mut Value,
        source_url: &str,
        rendered_html: &str,
    ) -> Result<bool, String> {
        recover_controller_avionics_extraction(payload, source_url, rendered_html)
            .map(|recovery| recovery.evidence_recovered)
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
    fn repairs_retained_submission_20_complementary_role_enumeration_to_two_units() {
        let html =
            controller_html("KSAR Garmin G5 attitude, Garmin G5 HSI, Garmin GFC500 auto pilot");
        let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
        let original = payload.clone();

        assert!(recover_exact_role_separated_avionics_quantity(
            &mut payload,
            CONTROLLER_URL,
            &html
        )
        .unwrap());
        assert_eq!(payload["avionics"][0]["quantity"], 2);
        assert_eq!(evidence(&payload), "Garmin G5 attitude, Garmin G5 HSI");
        assert_eq!(
            payload["avionics"][0]["manufacturer"],
            original["avionics"][0]["manufacturer"]
        );
        assert_eq!(
            payload["avionics"][0]["model"],
            original["avionics"][0]["model"]
        );
        assert_eq!(
            payload["avionics"][0]["types"],
            original["avionics"][0]["types"]
        );
        validate_unbound_current_avionics_extraction(&payload.to_string(), CONTROLLER_URL, &html)
            .unwrap();
    }

    #[test]
    fn complementary_role_quantity_is_a_fail_closed_completeness_contract() {
        let html = controller_html("Garmin G5 attitude, Garmin G5 HSI");
        let quantity_one = installed("Garmin", "G5", "Garmin G5 attitude");
        assert!(validate_unbound_current_avionics_extraction(
            &quantity_one.to_string(),
            CONTROLLER_URL,
            &html,
        )
        .unwrap_err()
        .contains("exact quantity of two"));

        let mut incomplete_evidence = quantity_one.clone();
        incomplete_evidence["avionics"][0]["quantity"] = serde_json::json!(2);
        assert!(validate_unbound_current_avionics_extraction(
            &incomplete_evidence.to_string(),
            CONTROLLER_URL,
            &html
        )
        .unwrap_err()
        .contains("does not cover both exact role-separated"));

        let mut complete = incomplete_evidence;
        complete["avionics"][0]["source_evidence_text"] =
            serde_json::json!("Garmin G5 attitude, Garmin G5 HSI");
        validate_unbound_current_avionics_extraction(&complete.to_string(), CONTROLLER_URL, &html)
            .unwrap();
    }

    #[test]
    fn role_quantity_recovery_rejects_narrative_repetition_and_identity_ambiguity() {
        for field in [
            "Garmin G5 attitude. Later, Garmin G5 HSI",
            "Garmin G5 attitude, Garmin G5 attitude",
            "Garmin G5 attitude, Garmin G5X HSI",
            "Garmin G5 attitude, Garmin GTX 345 transponder, Garmin G5 HSI",
            "Garmin G5 attitude and HSI",
        ] {
            let html = controller_html(field);
            let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
            let original = payload.clone();

            assert!(
                !recover_exact_role_separated_avionics_quantity(
                    &mut payload,
                    CONTROLLER_URL,
                    &html,
                )
                .unwrap(),
                "{field:?} must not prove two physical units"
            );
            assert_eq!(payload, original);
            validate_unbound_current_avionics_extraction(
                &payload.to_string(),
                CONTROLLER_URL,
                &html,
            )
            .unwrap();
        }
    }

    #[test]
    fn extra_same_product_occurrences_make_role_quantity_proof_ambiguous() {
        for field in [
            "Garmin G5 attitude, Garmin G5 HSI, Garmin G5 HSI",
            "Garmin G5 attitude, Garmin G5 HSI, Garmin G5 attitude",
            "Garmin G5 attitude, Garmin G5 HSI, Garmin GTX 345 transponder, Garmin G5 standby display",
        ] {
            let html = controller_html(field);
            let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
            let original = payload.clone();

            assert!(!recover_exact_role_separated_avionics_quantity(
                &mut payload,
                CONTROLLER_URL,
                &html,
            )
            .unwrap());
            assert_eq!(payload, original);
            assert!(validate_unbound_current_avionics_extraction(
                &payload.to_string(),
                CONTROLLER_URL,
                &html,
            )
            .unwrap_err()
            .contains("multiple distinct role-separated quantity proofs"));
        }
    }

    #[test]
    fn optional_or_untrusted_role_pairs_never_prove_installed_quantity() {
        let cases = [
            (
                CONTROLLER_URL,
                controller_html("Optional configurations: Garmin G5 attitude, Garmin G5 HSI"),
            ),
            (
                CONTROLLER_URL,
                controller_html("NEW Garmin G5 attitude, Garmin G5 HSI"),
            ),
            (
                CONTROLLER_URL,
                controller_html("KEEP Garmin G5 attitude, Garmin G5 HSI"),
            ),
            (
                CONTROLLER_URL,
                controller_html("KING Garmin G5 attitude, Garmin G5 HSI"),
            ),
            (
                GENERIC_URL,
                r#"<html><body>
                    <p>Garmin G5 attitude, Garmin G5 HSI</p>
                    <div class="detail__specs-wrapper">
                      <div class="detail__specs-label">Avionics/Radios</div>
                      <div class="detail__specs-value">Garmin GTX 345 transponder</div>
                    </div>
                    </body></html>"#
                    .to_string(),
            ),
            (
                "https://example.test/listing/1",
                controller_html("Garmin G5 attitude, Garmin G5 HSI"),
            ),
        ];
        for (source_url, html) in cases {
            let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
            let original = payload.clone();

            assert!(!recover_exact_role_separated_avionics_quantity(
                &mut payload,
                source_url,
                &html,
            )
            .unwrap());
            assert_eq!(payload, original);
            validate_unbound_current_avionics_extraction(&payload.to_string(), source_url, &html)
                .unwrap();
        }
    }

    #[test]
    fn punctuation_equivalent_output_rows_block_every_quantity_repair() {
        let html = controller_html("Garmin G5 attitude, Garmin G5 HSI");
        for (second_manufacturer, second_model) in [("Garmin", "G-5"), ("Gar-min", "G5")] {
            let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
            let second = installed(second_manufacturer, second_model, "Garmin G5 HSI")["avionics"]
                [0]
            .clone();
            payload["avionics"].as_array_mut().unwrap().push(second);
            let original = payload.clone();

            assert!(!recover_exact_role_separated_avionics_quantity(
                &mut payload,
                CONTROLLER_URL,
                &html,
            )
            .unwrap());
            assert_eq!(payload, original);
            assert!(validate_unbound_current_avionics_extraction(
                &payload.to_string(),
                CONTROLLER_URL,
                &html,
            )
            .unwrap_err()
            .contains("exact quantity of two"));
        }
    }

    #[test]
    fn role_quantity_recovery_accepts_only_one_ordinary_installed_output_row() {
        let html = controller_html("Garmin G5 attitude; Garmin G5 HSI");
        for mutation in ["replacement", "duplicate"] {
            let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
            if mutation == "replacement" {
                payload["avionics"][0]["configuration_action"] = serde_json::json!("replaces");
                payload["avionics"][0]["replaces"] = serde_json::json!({
                    "manufacturer": "Garmin",
                    "model": "G3X",
                    "types": ["Flight Display"]
                });
            } else {
                let duplicate = payload["avionics"][0].clone();
                payload["avionics"].as_array_mut().unwrap().push(duplicate);
            }
            let original = payload.clone();

            assert!(!recover_exact_role_separated_avionics_quantity(
                &mut payload,
                CONTROLLER_URL,
                &html,
            )
            .unwrap());
            assert_eq!(payload, original);
            assert!(validate_unbound_current_avionics_extraction(
                &payload.to_string(),
                CONTROLLER_URL,
                &html,
            )
            .is_err());
        }
    }

    #[test]
    fn multiple_distinct_role_quantity_proofs_fail_closed() {
        let html =
            controller_html("Garmin G5 attitude, Garmin G5 HSI; Garmin G5 attitude, Garmin G5 HSI");
        let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
        let original = payload.clone();

        assert!(!recover_exact_role_separated_avionics_quantity(
            &mut payload,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert_eq!(payload, original);
        assert!(validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CONTROLLER_URL,
            &html,
        )
        .unwrap_err()
        .contains("multiple distinct role-separated"));
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
                ControllerAvionicsExtractionRecovery::default()
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
                    "source_confidence": "high"
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
            ControllerAvionicsExtractionRecovery::default()
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
            ("GIA63WNAV(Dual)", vec!["NAV"], 1),
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
    fn controller_quantity_and_multiline_evidence_repairs_compose_atomically() {
        let flattened = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W NAV/COM/GPS/WAAS with GS #2";
        let exact = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W\nNAV/COM/GPS/WAAS with GS #2";
        let html = controller_html(&format!("Garmin G5 attitude, Garmin G5 HSI\n{exact}"));
        let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
        let mut gia = installed("Garmin", "GIA-63W", flattened)["avionics"][0].clone();
        gia["types"] = serde_json::json!(["NAV", "COM", "GPS"]);
        gia["quantity"] = serde_json::json!(2);
        payload["avionics"].as_array_mut().unwrap().push(gia);
        payload["aircraft_marker"] = serde_json::json!({
            "serial": "unchanged",
            "nested": [1, 2, 3]
        });
        let mut expected = payload.clone();
        expected["avionics"][0]["quantity"] = serde_json::json!(2);
        expected["avionics"][0]["source_evidence_text"] =
            serde_json::json!("Garmin G5 attitude, Garmin G5 HSI");
        expected["avionics"][1]["source_evidence_text"] = Value::String(exact.to_string());

        assert!(validate_unbound_current_avionics_extraction(
            &payload.to_string(),
            CONTROLLER_URL,
            &html,
        )
        .is_err());

        let recovery =
            recover_controller_avionics_extraction(&mut payload, CONTROLLER_URL, &html).unwrap();

        assert_eq!(
            recovery,
            ControllerAvionicsExtractionRecovery {
                quantity_recovered: true,
                evidence_recovered: true,
            }
        );
        assert_eq!(payload, expected);
        validate_unbound_current_avionics_extraction(&payload.to_string(), CONTROLLER_URL, &html)
            .unwrap();
    }

    #[test]
    fn controller_combined_repair_rolls_back_every_mutation_on_final_validation_failure() {
        let flattened = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W NAV/COM/GPS/WAAS with GS #2";
        let exact = "GIA-63W NAV/COM/GPS/WAAS with GS #1 GIA-63W\nNAV/COM/GPS/WAAS with GS #2";
        let html = controller_html(&format!(
            "Garmin G5 attitude, Garmin G5 HSI\n{exact}\nGarmin GI 275 attitude, Garmin GI 275 HSI"
        ));
        let mut payload = installed("Garmin", "G5", "Garmin G5 attitude");
        let mut gia = installed("Garmin", "GIA-63W", flattened)["avionics"][0].clone();
        gia["types"] = serde_json::json!(["NAV", "COM", "GPS"]);
        gia["quantity"] = serde_json::json!(2);
        payload["avionics"].as_array_mut().unwrap().push(gia);
        for evidence in ["Garmin GI 275 attitude", "Garmin GI 275 HSI"] {
            let gi_275 = installed("Garmin", "GI 275", evidence)["avionics"][0].clone();
            payload["avionics"].as_array_mut().unwrap().push(gi_275);
        }
        let original = payload.clone();
        let mut mutations = payload.clone();
        assert!(apply_exact_role_separated_avionics_quantity(
            &mut mutations,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert!(apply_controller_avionics_evidence_typography(
            &mut mutations,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());

        let error = recover_controller_avionics_extraction(&mut payload, CONTROLLER_URL, &html)
            .unwrap_err();

        assert!(error.contains("exact quantity of two"), "{error}");
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
    fn distinct_spellings_are_ambiguous_but_repeated_exact_spans_are_safe() {
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
        assert!(recover_controller_avionics_evidence_typography(
            &mut repeated,
            CONTROLLER_URL,
            &html,
        )
        .unwrap());
        assert_eq!(evidence(&repeated), "Garmin GMA-1347");
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
