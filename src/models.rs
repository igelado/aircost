use serde::{Deserialize, Serialize};

use crate::aircraft::curation::visual::VisualIdentifierResolution;

pub const MIN_PLAUSIBLE_ASKING_PRICE_USD: f64 = 1_000.0;
pub const MAX_PLAUSIBLE_ASKING_PRICE_USD: f64 = 250_000_000.0;

pub fn is_plausible_asking_price_usd(value: f64) -> bool {
    value.is_finite()
        && (MIN_PLAUSIBLE_ASKING_PRICE_USD..=MAX_PLAUSIBLE_ASKING_PRICE_USD).contains(&value)
}

#[derive(Clone, Debug, Deserialize, Serialize, sqlx::FromRow, PartialEq)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub display_name: String,
    pub auth_provider: String,
    pub auth_subject: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ParsedAvionics {
    pub manufacturer: String,
    pub model: String,
    /// Capabilities exposed by this physical product. Product identity is
    /// independent of capability, so there is intentionally no primary type.
    #[serde(default, rename = "types")]
    pub avionics_types: Vec<String>,
    #[serde(default = "default_quantity")]
    pub quantity: i64,
    #[serde(default = "default_configuration_action")]
    pub configuration_action: String,
    #[serde(default)]
    pub replaces: Option<ParsedAvionicsReference>,
    #[serde(default)]
    pub source_evidence_text: Option<String>,
    #[serde(default)]
    pub source_confidence: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ParsedAvionicsReference {
    pub manufacturer: String,
    pub model: String,
    #[serde(default, rename = "types")]
    pub avionics_types: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ListingValuationFact {
    pub kind: String,
    pub value: String,
    pub evidence_text: String,
    pub source_url: Option<String>,
    pub confidence: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ParsedInstalledComponent {
    pub manufacturer: String,
    pub model: String,
    pub evidence_text: String,
    pub confidence: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ParsedListing {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub variant: Option<String>,
    pub model_year: Option<i64>,
    pub asking_price_usd: Option<f64>,
    pub currency: String,
    pub airframe_hours: Option<f64>,
    pub engine_hours: Option<f64>,
    #[serde(default = "default_unknown_time_basis")]
    pub engine_time_basis: String,
    pub engine_time_evidence: Option<String>,
    pub engine_time_confidence: Option<String>,
    pub propeller_hours: Option<f64>,
    #[serde(default = "default_unknown_time_basis")]
    pub propeller_time_basis: String,
    pub propeller_time_evidence: Option<String>,
    pub propeller_time_confidence: Option<String>,
    pub installed_engine: Option<ParsedInstalledComponent>,
    pub installed_propeller: Option<ParsedInstalledComponent>,
    pub registration_number: Option<String>,
    pub serial_number: Option<String>,
    pub status: String,
    pub avionics: Vec<ParsedAvionics>,
    #[serde(default)]
    pub valuation_facts: Vec<ListingValuationFact>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ListingPreview {
    pub source_url: Option<String>,
    pub parsed_listing: ParsedListing,
    pub warnings: Vec<String>,
    /// Auditable model output from optional listing-photo identity recovery.
    /// Image bytes are never retained here; the report contains hashes and
    /// per-image visible-text evidence only.
    #[serde(default, skip_deserializing)]
    pub identity_recovery: Option<VisualIdentifierResolution>,
    #[serde(skip_serializing, skip_deserializing)]
    pub context_text: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AircraftSummary {
    pub manufacturer: String,
    pub model: String,
    pub variant: String,
    pub aircraft_model_id: i64,
    pub aircraft_model_variant_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SaleListing {
    pub id: i64,
    pub aircraft_model_id: i64,
    pub aircraft_model_variant_id: i64,
    pub created_by_user_id: i64,
    pub is_verified: bool,
    pub source_url: Option<String>,
    pub model_year: i64,
    pub asking_price_usd: f64,
    pub currency: String,
    pub added_at: String,
    pub status: String,
    pub registration_number: Option<String>,
    pub serial_number: Option<String>,
    pub airframe_hours: f64,
    pub engine_hours: Option<f64>,
    pub engine_time_basis: String,
    pub engine_time_evidence: Option<String>,
    pub engine_time_confidence: Option<String>,
    pub propeller_hours: Option<f64>,
    pub propeller_time_basis: String,
    pub propeller_time_evidence: Option<String>,
    pub propeller_time_confidence: Option<String>,
    pub installed_engine_model_id: Option<i64>,
    pub installed_engine_source_url: Option<String>,
    pub installed_engine_evidence_text: Option<String>,
    pub installed_engine_confidence: Option<String>,
    pub installed_propeller_model_id: Option<i64>,
    pub installed_propeller_source_url: Option<String>,
    pub installed_propeller_evidence_text: Option<String>,
    pub installed_propeller_confidence: Option<String>,
    pub ingestion_state: String,
    pub ingestion_error: Option<String>,
    pub ingestion_completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub aircraft: AircraftSummary,
    pub avionics: Vec<ParsedAvionics>,
    pub valuation_facts: Vec<ListingValuationFact>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PreviewRequest {
    pub source_url: Option<String>,
    pub listing: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ListingUpdateRequest {
    pub listing: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PluginRegisterRequest {
    pub public_key_base64: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PluginSubmissionRequest {
    pub plugin_install_id: i64,
    pub source_url: String,
    pub rendered_html: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow, PartialEq)]
pub struct PluginInstall {
    pub id: i64,
    pub user_id: i64,
    pub public_key_base64: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PluginSubmission {
    pub id: i64,
    pub user_id: i64,
    pub plugin_install_id: i64,
    pub source_url: String,
    pub submitted_at: String,
    pub rendered_html_sha256: String,
    pub signature_base64: String,
    pub extracted_listing_json: Option<serde_json::Value>,
    pub extraction_error: Option<String>,
    pub canonical_listing_id: Option<i64>,
}

pub fn default_quantity() -> i64 {
    1
}

pub fn default_configuration_action() -> String {
    "installed".to_string()
}

pub fn default_unknown_time_basis() -> String {
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        is_plausible_asking_price_usd, MAX_PLAUSIBLE_ASKING_PRICE_USD,
        MIN_PLAUSIBLE_ASKING_PRICE_USD,
    };

    #[test]
    fn asking_price_plausibility_includes_only_the_documented_boundaries() {
        assert!(is_plausible_asking_price_usd(
            MIN_PLAUSIBLE_ASKING_PRICE_USD
        ));
        assert!(is_plausible_asking_price_usd(
            MAX_PLAUSIBLE_ASKING_PRICE_USD
        ));
        assert!(!is_plausible_asking_price_usd(
            MIN_PLAUSIBLE_ASKING_PRICE_USD - 0.01
        ));
        assert!(!is_plausible_asking_price_usd(
            MAX_PLAUSIBLE_ASKING_PRICE_USD + 0.01
        ));
        assert!(!is_plausible_asking_price_usd(f64::NAN));
    }
}
