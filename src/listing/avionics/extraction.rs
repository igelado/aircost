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
use crate::html::clean::listing_body_contains_exact_structurally_visible_text_span;
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
    validate_current_avionics_identity_evidence(
        &observations,
        &listing_context,
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
    rendered_html: &str,
) -> Result<Vec<ParsedAvionics>, String> {
    let observations = parse_current_avionics_extraction_json(extracted_listing_json)?;
    let listing_context = ListingEvidenceContext::from_rendered_html(Some(rendered_html));
    validate_current_avionics_identity_evidence(&observations, &listing_context, rendered_html)?;
    Ok(observations)
}

/// Replace only typography-drifted occurrence evidence with an exact visible
/// span copied from one trusted Controller Avionics/Radios field.
///
/// This is deliberately an extraction-boundary repair, not product
/// normalization. The model-produced evidence remains the identity-complete
/// locator: after lowercasing and removing non-alphanumeric characters, a
/// source span must be exactly equal to that locator. Every identity and
/// action field stays model-produced and is revalidated unchanged.
pub(crate) fn recover_controller_avionics_evidence_typography(
    extracted_listing: &mut Value,
    source_url: &str,
    rendered_html: &str,
) -> Result<bool, String> {
    let observations = parse_current_avionics_extraction_value(extracted_listing)?;
    let missing_exact_span = observations.iter().any(|observation| {
        !listing_body_contains_exact_structurally_visible_text_span(
            rendered_html,
            observation
                .source_evidence_text
                .as_deref()
                .expect("the canonical parser requires occurrence evidence"),
        )
    });
    if !missing_exact_span {
        return Ok(false);
    }

    let Some(controller_field) = controller_avionics_evidence(source_url, rendered_html) else {
        return Ok(false);
    };
    let full_source = ListingEvidenceContext::from_cleaned_text(&controller_field);
    let mut replacements = Vec::new();
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence");
        if listing_body_contains_exact_structurally_visible_text_span(rendered_html, evidence) {
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
            .filter(|candidate| {
                listing_body_contains_exact_structurally_visible_text_span(rendered_html, candidate)
            })
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
                })
            })
            .collect::<BTreeSet<_>>();
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
    let original = extracted_listing.clone();
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

    if let Err(error) =
        validate_unbound_current_avionics_extraction(&extracted_listing.to_string(), rendered_html)
    {
        *extracted_listing = original;
        return Err(error);
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
                && !candidate.contains(['\r', '\n'])
                && identity_span_has_boundaries(source, source_start, source_end))
            .then_some(candidate)
        })
        .collect()
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
    rendered_html: &str,
) -> Result<(), String> {
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence")
            .trim();
        if !listing_body_contains_exact_structurally_visible_text_span(rendered_html, evidence) {
            return Err(format!(
                "avionics[{index}].source_evidence_text is not one exact structurally visible span in the retained capture"
            ));
        }
        validate_current_avionics_identity_evidence_occurrence(
            observation,
            index,
            listing_context,
            evidence,
        )?;
    }
    Ok(())
}

fn validate_current_avionics_identity_evidence_occurrence(
    observation: &ParsedAvionics,
    index: usize,
    listing_context: &ListingEvidenceContext,
    exact_visible_evidence_locator: &str,
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
    if !bounded_source.contains(evidence)
        || !extraction_occurrence_has_exact_identity(
            &evidence_context,
            &observation.manufacturer,
            &observation.model,
            &observation.avionics_types,
            evidence,
        )
    {
        return Err(format!(
            "avionics[{index}].source_evidence_text is not one exact bounded source excerpt containing the candidate identity"
        ));
    }
    if let Some(replacement) = observation.replaces.as_ref() {
        if !extraction_occurrence_has_exact_identity(
            &evidence_context,
            &replacement.manufacturer,
            &replacement.model,
            &replacement.avionics_types,
            evidence,
        ) {
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
    if types.is_empty()
        || types.iter().any(|value| {
            value.as_str().map(str::trim).is_none_or(|value| {
                value.is_empty() || (value != "Unknown" && !CURATED_AVIONICS_TYPES.contains(&value))
            })
        })
    {
        return Err(format!(
            "{path}.types must contain only current curated capabilities or Unknown"
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

    const CONTROLLER_URL: &str = "https://www.controller.com/listing/for-sale/257737897/example";

    fn controller_html(field: &str) -> String {
        format!(
            r#"<html><head><meta content="Garmin GMA-1347"></head><body>
            <div class="detail__specs-wrapper">
              <div class="detail__specs-label">Avionics/Radios</div>
              <div class="detail__specs-value">{field}</div>
            </div>
            </body></html>"#
        )
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

        let parsed = validate_unbound_current_avionics_extraction(json, &html).unwrap();

        assert_eq!(parsed[0].model, "G1000");
    }

    #[test]
    fn shared_identity_validation_rejects_hidden_and_metadata_only_evidence() {
        let html = "<html><head><meta content=\"Garmin G1000 metadata\"></head><body><p hidden>Garmin G1000 avionics system</p></body></html>";
        let payload = installed("Garmin", "G1000", "Garmin G1000 avionics system");
        let observations = parse_current_avionics_extraction_value(&payload).unwrap();
        let context = ListingEvidenceContext::from_rendered_html(Some(&html));

        assert!(
            validate_current_avionics_identity_evidence(&observations, &context, html)
                .unwrap_err()
                .contains("structurally visible")
        );

        let metadata_payload = installed("Garmin", "G1000", "Garmin G1000 metadata");
        let metadata_observations =
            parse_current_avionics_extraction_value(&metadata_payload).unwrap();
        assert!(validate_current_avionics_identity_evidence(
            &metadata_observations,
            &context,
            html
        )
        .unwrap_err()
        .contains("structurally visible"));
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

            assert!(
                validate_unbound_current_avionics_extraction(&payload.to_string(), &html)
                    .unwrap_err()
                    .contains("candidate identity")
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
            validate_unbound_current_avionics_extraction(&payload.to_string(), &html).unwrap();

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
            validate_unbound_current_avionics_extraction(&payload.to_string(), &html).unwrap();

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
                validate_unbound_current_avionics_extraction(&payload.to_string(), &html)
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
