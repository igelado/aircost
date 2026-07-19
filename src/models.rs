use serde::{Deserialize, Serialize};

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
    #[serde(rename = "type")]
    pub avionics_type: String,
    #[serde(default = "default_quantity")]
    pub quantity: i64,
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
    pub propeller_hours: Option<f64>,
    pub registration_number: Option<String>,
    pub serial_number: Option<String>,
    pub status: String,
    pub avionics: Vec<ParsedAvionics>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ListingPreview {
    pub source_url: Option<String>,
    pub parsed_listing: ParsedListing,
    pub warnings: Vec<String>,
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
    pub engine_hours: f64,
    pub propeller_hours: f64,
    pub created_at: String,
    pub updated_at: String,
    pub aircraft: AircraftSummary,
    pub avionics: Vec<ParsedAvionics>,
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
