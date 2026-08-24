//! One bounded semantic correction for a parseable listing extraction whose
//! avionics member fails the current deterministic extraction contract.

use serde_json::Value;

use crate::extract::{GeminiListingExtractor, ListingAvionicsCorrectionToken};
use crate::listing::avionics::extraction::{
    recover_controller_avionics_extraction, validate_unbound_current_avionics_extraction,
};
use crate::models::ParsedAvionics;

pub(crate) async fn validate_or_correct_listing_avionics(
    extractor: &GeminiListingExtractor,
    correction_token: Option<ListingAvionicsCorrectionToken>,
    listing_text: &str,
    source_url: &str,
    rendered_html: &str,
    extracted_listing: &mut Value,
) -> Result<Vec<ParsedAvionics>, String> {
    match validate_listing_avionics(source_url, rendered_html, extracted_listing) {
        Ok(occurrences) => Ok(occurrences),
        Err(primary_error) => {
            let Some(correction_token) = correction_token else {
                return Err(primary_error);
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
                    &primary_error,
                )
                .await
                .map_err(|error| {
                    format!(
                        "listing avionics validation failed and its single correction request failed: {error:#}; initial validation: {primary_error}"
                    )
                })?;
            let corrected_avionics = correction
                .get("avionics")
                .cloned()
                .expect("the correction response contract requires avionics");
            let object = extracted_listing.as_object_mut().ok_or_else(|| {
                "listing avionics correction requires one top-level extraction object".to_string()
            })?;
            object.insert("avionics".to_string(), corrected_avionics);

            match validate_listing_avionics(source_url, rendered_html, extracted_listing) {
                Ok(occurrences) => Ok(occurrences),
                Err(correction_error) => {
                    *extracted_listing = original;
                    Err(format!(
                        "listing avionics validation failed after its single correction request: {correction_error}; initial validation: {primary_error}"
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
) -> Result<Vec<ParsedAvionics>, String> {
    recover_controller_avionics_extraction(extracted_listing, source_url, rendered_html)?;
    let occurrences = validate_unbound_current_avionics_extraction(
        &extracted_listing.to_string(),
        source_url,
        rendered_html,
    )?;
    Ok(occurrences)
}
