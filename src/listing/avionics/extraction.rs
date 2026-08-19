//! Strict current-schema avionics extraction boundary.
//!
//! The retained extraction is derived data, but it is usable only while its
//! exact signed source capture remains bound to the same owner, listing, and
//! source URL. This module deliberately has no legacy parser or scalar
//! capability fallback.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::extract::CURATED_AVIONICS_TYPES;
use crate::html::clean::listing_body_contains_exact_structurally_visible_text_span;
use crate::listing::evidence::{ListingEvidenceContext, MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES};
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
    validate_current_avionics_identity_evidence(&observations, &listing_context)?;
    for (index, observation) in observations.iter().enumerate() {
        let path = format!("avionics[{index}]");
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence");
        if !listing_body_contains_exact_structurally_visible_text_span(
            extraction.rendered_html,
            evidence,
        ) {
            return Err(format!(
                "{path}.source_evidence_text is not one exact structurally visible span in the retained capture"
            ));
        }
    }
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
    validate_current_avionics_identity_evidence(&observations, &listing_context)?;
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence");
        if !listing_body_contains_exact_structurally_visible_text_span(rendered_html, evidence) {
            return Err(format!(
                "avionics[{index}].source_evidence_text is not one exact structurally visible span in the retained capture"
            ));
        }
    }
    Ok(observations)
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
) -> Result<(), String> {
    for (index, observation) in observations.iter().enumerate() {
        let evidence = observation
            .source_evidence_text
            .as_deref()
            .expect("the canonical parser requires occurrence evidence")
            .trim();
        let bounded_source =
            listing_context.for_candidate(&observation.manufacturer, &observation.model, None);
        let evidence_context = ListingEvidenceContext::from_cleaned_text(evidence);
        if !bounded_source.contains(evidence)
            || !context_has_exact_identity(
                &evidence_context,
                &observation.manufacturer,
                &observation.model,
            )
        {
            return Err(format!(
                "avionics[{index}].source_evidence_text is not one exact bounded source excerpt containing the candidate identity"
            ));
        }
        if let Some(replacement) = observation.replaces.as_ref() {
            if !context_has_exact_identity(
                &evidence_context,
                &replacement.manufacturer,
                &replacement.model,
            ) {
                return Err(format!(
                    "avionics[{index}].source_evidence_text does not contain the exact replacement identity from avionics[{index}].replaces"
                ));
            }
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
}
