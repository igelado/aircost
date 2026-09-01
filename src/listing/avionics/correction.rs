//! One bounded semantic correction for a parseable listing extraction whose
//! avionics member fails the current deterministic extraction contract.

use serde_json::Value;
use std::fmt;

use crate::extract::{GeminiListingExtractor, ListingAvionicsCorrectionToken};
use crate::listing::avionics::extraction::{
    current_avionics_duplicate_identity_failures, independent_current_avionics_validation_failures,
    repair_corrected_listing_avionics_extraction, repair_listing_avionics_extraction,
    validate_unbound_current_avionics_extraction, AvionicsValidationFailure,
};
use crate::models::ParsedAvionics;

#[derive(Clone, Debug, Eq, PartialEq)]
/// Bounded fail-fast diagnostics for the primary extraction and, when used,
/// its single semantic correction attempt.
pub struct ListingAvionicsDeterministicFailure {
    pub(crate) initial: AvionicsValidationFailure,
    pub(crate) corrected: Option<AvionicsValidationFailure>,
}

impl fmt::Display for ListingAvionicsDeterministicFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "primary_avionics_validation:{}",
            safe_failure_summary(&self.initial)
        )?;
        if let Some(corrected) = self.corrected.as_ref() {
            write!(
                formatter,
                "; corrected_avionics_validation:{}",
                safe_failure_summary(corrected)
            )?;
        }
        Ok(())
    }
}

fn safe_failure_summary(failure: &AvionicsValidationFailure) -> String {
    let mut summary = failure.rule().code().to_string();
    if let Some(path) = failure.path() {
        summary.push_str(" at ");
        summary.push_str(&path);
    }
    summary
}

#[derive(Debug)]
pub(crate) enum ListingAvionicsValidationError {
    Deterministic(ListingAvionicsDeterministicFailure),
    CorrectionOperation(String),
}

impl fmt::Display for ListingAvionicsValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deterministic(error) => write!(
                formatter,
                "listing avionics deterministic validation failed: {error}"
            ),
            Self::CorrectionOperation(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ListingAvionicsValidationError {}

pub(crate) async fn validate_or_correct_listing_avionics(
    extractor: &GeminiListingExtractor,
    correction_token: Option<ListingAvionicsCorrectionToken>,
    listing_text: &str,
    source_url: &str,
    rendered_html: &str,
    extracted_listing: &mut Value,
) -> Result<Vec<ParsedAvionics>, ListingAvionicsValidationError> {
    match validate_listing_avionics(source_url, rendered_html, extracted_listing) {
        Ok(occurrences) => Ok(occurrences),
        Err(primary_error) => {
            let Some(correction_token) = correction_token else {
                return Err(ListingAvionicsValidationError::Deterministic(
                    ListingAvionicsDeterministicFailure {
                        initial: primary_error,
                        corrected: None,
                    },
                ));
            };
            let original = extracted_listing.clone();
            let previous_avionics = extracted_listing
                .as_object()
                .and_then(|object| object.get("avionics"))
                .cloned()
                .unwrap_or(Value::Null);
            let validation_feedback = complete_correction_feedback(
                extracted_listing,
                source_url,
                rendered_html,
                &primary_error,
            );
            let correction = extractor
                .correct_listing_avionics(
                    correction_token,
                    listing_text,
                    &previous_avionics,
                    &validation_feedback,
                )
                .await
                .map_err(|error| {
                    format!(
                        "listing avionics validation failed and its single correction request failed: {error:#}; initial validation: {primary_error}"
                    )
                })
                .map_err(ListingAvionicsValidationError::CorrectionOperation)?;
            let corrected_avionics = correction
                .get("avionics")
                .cloned()
                .expect("the correction response contract requires avionics");
            let object = extracted_listing.as_object_mut().ok_or_else(|| {
                ListingAvionicsValidationError::CorrectionOperation(
                    "listing avionics correction requires one top-level extraction object"
                        .to_string(),
                )
            })?;
            object.insert("avionics".to_string(), corrected_avionics);

            match validate_corrected_listing_avionics(source_url, rendered_html, extracted_listing)
            {
                Ok(occurrences) => Ok(occurrences),
                Err(correction_error) => {
                    *extracted_listing = original;
                    Err(ListingAvionicsValidationError::Deterministic(
                        ListingAvionicsDeterministicFailure {
                            initial: primary_error,
                            corrected: Some(correction_error),
                        },
                    ))
                }
            }
        }
    }
}

fn complete_correction_feedback(
    extracted_listing: &Value,
    source_url: &str,
    rendered_html: &str,
    primary_error: &AvionicsValidationFailure,
) -> String {
    let primary = primary_error.to_string();
    let mut feedback = vec![primary.clone()];
    for failure in independent_current_avionics_validation_failures(
        extracted_listing,
        source_url,
        rendered_html,
    ) {
        let diagnostic = failure.to_string();
        if diagnostic != primary && !feedback.contains(&diagnostic) {
            feedback.push(format!(
                "additional independent deterministic defect: {diagnostic}"
            ));
        }
    }
    for failure in current_avionics_duplicate_identity_failures(extracted_listing) {
        let diagnostic = failure.to_string();
        if diagnostic != primary && !feedback.iter().any(|entry| entry.ends_with(&diagnostic)) {
            feedback.push(format!(
                "additional independent deterministic defect: {diagnostic}"
            ));
        }
    }
    feedback.join("\n")
}

fn validate_listing_avionics(
    source_url: &str,
    rendered_html: &str,
    extracted_listing: &mut Value,
) -> Result<Vec<ParsedAvionics>, AvionicsValidationFailure> {
    repair_listing_avionics_extraction(extracted_listing, source_url, rendered_html)?;
    let occurrences = validate_unbound_current_avionics_extraction(
        &extracted_listing.to_string(),
        source_url,
        rendered_html,
    )?;
    Ok(occurrences)
}

fn validate_corrected_listing_avionics(
    source_url: &str,
    rendered_html: &str,
    extracted_listing: &mut Value,
) -> Result<Vec<ParsedAvionics>, AvionicsValidationFailure> {
    repair_corrected_listing_avionics_extraction(extracted_listing, source_url, rendered_html)?;
    validate_unbound_current_avionics_extraction(
        &extracted_listing.to_string(),
        source_url,
        rendered_html,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{complete_correction_feedback, validate_listing_avionics};

    const CONTROLLER_URL: &str = "https://www.controller.com/listing/for-sale/123/test-aircraft";

    fn controller_html(field: &str) -> String {
        format!(
            r#"<html><body><main id="main-content" class="detail__main-content">
            <h1 class="detail__title">Test Aircraft</h1>
            <div class="listing-prices__retail-price">$100,000</div>
            <div class="detail__specs"><h3 class="detail__specs-heading">Avionics</h3>
            <div class="detail__specs-wrapper">
            <div class="detail__specs-label">Avionics/Radios</div>
            <div class="detail__specs-value">{field}</div>
            </div></div></main></body></html>"#
        )
    }

    #[test]
    fn correction_feedback_includes_an_independent_duplicate_after_identity_failure() {
        let field = "King KX-170B Nav-Com w/ VOR, Localizer &amp; Glideslope\n\
King KX-170B Nav-Com #2 w/ VOR &amp; Localizer\n\
Uavionix Wingtip Beacons with ADS-B IN &amp; OUT\n\
Century I Wingleveler Autopilot (INOP)";
        let html = controller_html(field);
        let mut payload = json!({
            "avionics": [
                {
                    "manufacturer": "King", "model": "KX-170B", "types": ["NAV", "COM"],
                    "quantity": 1, "configuration_action": "installed", "replaces": null,
                    "source_evidence_text": "King KX-170B Nav-Com w/ VOR, Localizer & Glideslope",
                    "source_confidence": "high"
                },
                {
                    "manufacturer": "King", "model": "KX-170B", "types": ["NAV", "COM"],
                    "quantity": 1, "configuration_action": "installed", "replaces": null,
                    "source_evidence_text": "King KX-170B Nav-Com #2 w/ VOR & Localizer",
                    "source_confidence": "high"
                },
                {
                    "manufacturer": "Uavionix", "model": "skyBeacon", "types": ["Transponder"],
                    "quantity": 1, "configuration_action": "installed", "replaces": null,
                    "source_evidence_text": "Uavionix Wingtip Beacons with ADS-B IN & OUT",
                    "source_confidence": "high"
                },
                {
                    "manufacturer": "Century", "model": "I", "types": ["Autopilot"],
                    "quantity": 1, "configuration_action": "installed", "replaces": null,
                    "source_evidence_text": "Century I Wingleveler Autopilot (INOP)",
                    "source_confidence": "high"
                }
            ]
        });

        let primary = validate_listing_avionics(CONTROLLER_URL, &html, &mut payload)
            .expect_err("the invented model must fail before quantity validation");
        let feedback = complete_correction_feedback(&payload, CONTROLLER_URL, &html, &primary);

        assert!(feedback.contains("candidate identity"), "{feedback}");
        assert!(feedback.contains("additional independent deterministic defect"));
        assert!(feedback.contains("duplicates an earlier normalized manufacturer/model identity"));
        assert!(feedback.contains("avionics[1].quantity"), "{feedback}");
        assert!(
            feedback.contains("explicitly marked inoperative"),
            "{feedback}"
        );
        assert!(
            feedback.contains("avionics[3].source_evidence_text"),
            "{feedback}"
        );
    }

    #[test]
    fn correction_feedback_enumerates_every_duplicate_identity_group() {
        let field =
            "Garmin G5 PFD\nGarmin G5 HSI\nGarmin GSA 28 roll servo\nGarmin GSA 28 pitch servo";
        let html = controller_html(field);
        let payload = json!({
            "avionics": [
                {
                    "manufacturer": "Garmin", "model": "G5", "types": ["Flight Display"],
                    "quantity": 1, "configuration_action": "installed", "replaces": null,
                    "source_evidence_text": "Garmin G5 PFD", "source_confidence": "high"
                },
                {
                    "manufacturer": "Garmin", "model": "G5", "types": ["Navigation Indicator"],
                    "quantity": 1, "configuration_action": "installed", "replaces": null,
                    "source_evidence_text": "Garmin G5 HSI", "source_confidence": "high"
                },
                {
                    "manufacturer": "Garmin", "model": "GSA 28", "types": ["Unknown"],
                    "quantity": 1, "configuration_action": "installed", "replaces": null,
                    "source_evidence_text": "Garmin GSA 28 roll servo", "source_confidence": "high"
                },
                {
                    "manufacturer": "Garmin", "model": "GSA 28", "types": ["Unknown"],
                    "quantity": 1, "configuration_action": "installed", "replaces": null,
                    "source_evidence_text": "Garmin GSA 28 pitch servo", "source_confidence": "high"
                }
            ]
        });
        let mut validating_payload = payload.clone();
        let primary = validate_listing_avionics(CONTROLLER_URL, &html, &mut validating_payload)
            .expect_err("both normalized identity groups are duplicated");
        let feedback = complete_correction_feedback(&payload, CONTROLLER_URL, &html, &primary);

        assert!(feedback.contains("avionics[1].quantity"), "{feedback}");
        assert!(
            feedback.contains("related occurrence avionics[0]"),
            "{feedback}"
        );
        assert!(feedback.contains("avionics[3].quantity"), "{feedback}");
        assert!(
            feedback.contains("related occurrence avionics[2]"),
            "{feedback}"
        );
    }
}
