//! One bounded semantic correction for a parseable listing extraction whose
//! avionics member fails the current deterministic extraction contract.

use serde_json::Value;
use std::fmt;

use crate::extract::{GeminiListingExtractor, ListingAvionicsCorrectionToken};
use crate::listing::avionics::extraction::{
    recover_controller_avionics_extraction, validate_unbound_current_avionics_extraction,
    AvionicsValidationFailure,
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
            let correction = extractor
                .correct_listing_avionics(
                    correction_token,
                    listing_text,
                    &previous_avionics,
                    &primary_error.to_string(),
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

            match validate_listing_avionics(source_url, rendered_html, extracted_listing) {
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

fn validate_listing_avionics(
    source_url: &str,
    rendered_html: &str,
    extracted_listing: &mut Value,
) -> Result<Vec<ParsedAvionics>, AvionicsValidationFailure> {
    recover_controller_avionics_extraction(extracted_listing, source_url, rendered_html)?;
    let occurrences = validate_unbound_current_avionics_extraction(
        &extracted_listing.to_string(),
        source_url,
        rendered_html,
    )?;
    Ok(occurrences)
}
