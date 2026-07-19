use std::collections::HashSet;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::CONTENT_TYPE;
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::sync::OnceCell;
use url::Url;

use crate::html_clean::clean_listing_html;
use crate::models::{ListingPreview, ParsedAvionics, ParsedListing};
use crate::normalize::{canonical_manufacturer_name, normalize_name};

const DEFAULT_GEMINI_MODEL: &str = "gemini-3.1-flash-lite";
const DEFAULT_GEMINI_GROUNDING_MODEL: &str = "gemini-3.1-flash-lite";
const DEFAULT_GEMINI_AVIONICS_REVIEW_MODEL: &str = "gemini-3.1-flash-lite";
const DEFAULT_GEMINI_MAX_OUTPUT_TOKENS: u64 = 4096;
const DEFAULT_GEMINI_TIMEOUT_SECONDS: u64 = 60;
const DEFAULT_GEMINI_THINKING_LEVEL: &str = "low";
const GEMINI_JSON_REPAIR_MAX_OUTPUT_TOKENS: u64 = 8192;

const SYSTEM_PROMPT: &str = "You extract aircraft sale listing fields from plain text. Return only a single valid JSON object with the requested keys. Fill all creation-critical fields; use null only for optional metadata fields that are absent.";

pub struct ModelFamilyConfirmationContext<'a> {
    pub manufacturer: &'a str,
    pub extracted_model: &'a str,
    pub extracted_variant: &'a str,
    pub candidate_model: &'a str,
    pub source_url: Option<&'a str>,
    pub model_year: Option<i64>,
    pub listing_context: Option<&'a str>,
}

pub struct VariantConfirmationContext<'a> {
    pub manufacturer: &'a str,
    pub extracted_model: &'a str,
    pub extracted_variant: &'a str,
    pub candidate_model: &'a str,
    pub candidate_variant: &'a str,
    pub source_url: Option<&'a str>,
    pub model_year: Option<i64>,
    pub listing_context: Option<&'a str>,
}

pub struct VariantLabelCorrectionContext<'a> {
    pub manufacturer: &'a str,
    pub model: &'a str,
    pub variant: &'a str,
    pub model_year: i64,
    pub source_url: Option<&'a str>,
    pub listing_context: Option<&'a str>,
    pub issues: &'a [String],
}

#[derive(Debug, Serialize)]
pub struct VariantNormalizationExample {
    pub model_year: i64,
    pub registration_number: Option<String>,
    pub source_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VariantNormalizationCandidate {
    pub variant: String,
    pub listing_count: i64,
    pub examples: Vec<VariantNormalizationExample>,
}

#[derive(Debug, Serialize)]
pub struct VariantNormalizationContext {
    pub manufacturer: String,
    pub model: String,
    pub variants: Vec<VariantNormalizationCandidate>,
}

pub struct AvionicsMetadataContext<'a> {
    pub manufacturer: &'a str,
    pub model: &'a str,
    pub avionics_type: &'a str,
    pub value_reference_year: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsUnitResolutionCandidate {
    pub manufacturer: String,
    pub model: String,
    pub avionics_type: String,
    pub quantity: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsUnitResolutionContext {
    pub aircraft_manufacturer: String,
    pub aircraft_model: String,
    pub aircraft_variant: String,
    pub model_year: i64,
    pub source_url: String,
    pub listing_context: String,
    pub candidate: AvionicsUnitResolutionCandidate,
    pub value_reference_year: i64,
}

#[derive(Debug, Serialize)]
pub struct AvionicsUnitResolutionCorrectionContext {
    pub issues: Vec<String>,
    pub secondary_check: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct AvionicsNormalizationCandidate {
    pub id: i64,
    pub manufacturer: String,
    pub avionics_type: String,
    pub model: String,
    pub normalized_model: String,
    pub listing_count: i64,
    pub introduced_year: Option<i64>,
    pub estimated_unit_value_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct AvionicsNormalizationContext {
    pub models: Vec<AvionicsNormalizationCandidate>,
}

pub struct DefaultAvionicsContext<'a> {
    pub manufacturer: &'a str,
    pub model: &'a str,
    pub variant: &'a str,
    pub model_year: i64,
    pub value_reference_year: i64,
    pub source_url: Option<&'a str>,
    pub nearby_price_points: &'a [AircraftPricePointContext],
}

#[derive(Serialize)]
pub struct AircraftPricePointContext {
    pub variant: String,
    pub model_year: i64,
    pub purchase_price_new_usd: f64,
    pub purchase_price_reference_year: i64,
    pub source_title: String,
    pub source_confidence: String,
}

pub struct AircraftSpecListingContext<'a> {
    pub model_year: i64,
    pub asking_price_usd: f64,
    pub airframe_hours: f64,
    pub engine_hours: f64,
    pub propeller_hours: f64,
    pub source_url: &'a str,
    pub listing_text: &'a str,
}

pub struct AircraftSpecMetadataContext<'a> {
    pub manufacturer: &'a str,
    pub model: &'a str,
    pub variant_context: &'a str,
    pub value_reference_year: i64,
    pub listing_contexts: &'a [AircraftSpecListingContext<'a>],
}

#[derive(Clone)]
pub struct GeminiListingExtractor {
    client: Client,
    api_key: String,
    url: String,
    grounded_url: String,
    avionics_review_url: String,
    max_output_tokens: u64,
    thinking_level: Option<String>,
    browser: Arc<OnceCell<eoka::Browser>>,
}

impl GeminiListingExtractor {
    pub fn from_environment() -> Result<Self> {
        let api_key = env::var("GEMINI_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("GEMINI_API_KEY must be set"))?;
        let model =
            env::var("AIRCOST_GEMINI_MODEL").unwrap_or_else(|_| DEFAULT_GEMINI_MODEL.to_string());
        let model_path = if model.starts_with("models/") {
            model
        } else {
            format!("models/{model}")
        };
        let grounding_model = env::var("AIRCOST_GEMINI_GROUNDING_MODEL")
            .unwrap_or_else(|_| DEFAULT_GEMINI_GROUNDING_MODEL.to_string());
        let grounding_model_path = if grounding_model.starts_with("models/") {
            grounding_model
        } else {
            format!("models/{grounding_model}")
        };
        let avionics_review_model = env::var("AIRCOST_GEMINI_AVIONICS_REVIEW_MODEL")
            .unwrap_or_else(|_| DEFAULT_GEMINI_AVIONICS_REVIEW_MODEL.to_string());
        let avionics_review_model_path = if avionics_review_model.starts_with("models/") {
            avionics_review_model
        } else {
            format!("models/{avionics_review_model}")
        };
        let timeout_seconds = environment_u64(
            "AIRCOST_GEMINI_TIMEOUT_SECONDS",
            DEFAULT_GEMINI_TIMEOUT_SECONDS,
        )?;
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .build()
            .context("could not create Gemini HTTP client")?;

        Ok(Self {
            client,
            api_key,
            url: format!(
                "https://generativelanguage.googleapis.com/v1beta/{model_path}:generateContent"
            ),
            grounded_url: format!(
                "https://generativelanguage.googleapis.com/v1beta/{grounding_model_path}:generateContent"
            ),
            avionics_review_url: format!(
                "https://generativelanguage.googleapis.com/v1beta/{avionics_review_model_path}:generateContent"
            ),
            max_output_tokens: environment_u64(
                "AIRCOST_GEMINI_MAX_OUTPUT_TOKENS",
                DEFAULT_GEMINI_MAX_OUTPUT_TOKENS,
            )?,
            thinking_level: env::var("AIRCOST_GEMINI_THINKING_LEVEL")
                .ok()
                .or_else(|| Some(DEFAULT_GEMINI_THINKING_LEVEL.to_string()))
                .filter(|value| !value.trim().is_empty()),
            browser: Arc::new(OnceCell::new()),
        })
    }

    async fn fetch_url(&self, source_url: &str) -> Result<String> {
        let browser = self
            .browser
            .get_or_try_init(|| async {
                eoka::Browser::launch()
                    .await
                    .context("could not launch eoka browser")
            })
            .await?;
        fetch_url(source_url, browser).await
    }

    pub async fn extract(&self, listing_text: &str) -> Result<Value> {
        self.generate_json(
            format!(
                "{SYSTEM_PROMPT}\n\n{}",
                build_extraction_prompt(listing_text)
            ),
            gemini_response_schema(),
            self.max_output_tokens,
        )
        .await
    }

    pub async fn confirm_same_aircraft_model_family(
        &self,
        context: &ModelFamilyConfirmationContext<'_>,
    ) -> Result<bool> {
        let response = self
            .generate_json(
                build_model_family_confirmation_prompt(context),
                gemini_model_confirmation_response_schema(),
                256,
            )
            .await?;
        Ok(response
            .get("same_model_family")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    pub async fn confirm_same_aircraft_variant(
        &self,
        context: &VariantConfirmationContext<'_>,
    ) -> Result<bool> {
        let response = self
            .generate_json(
                build_variant_confirmation_prompt(context),
                gemini_variant_confirmation_response_schema(),
                256,
            )
            .await?;
        Ok(response
            .get("same_variant")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    pub async fn correct_aircraft_variant_label(
        &self,
        context: &VariantLabelCorrectionContext<'_>,
    ) -> Result<Value> {
        self.generate_json(
            build_variant_label_correction_prompt(context),
            gemini_variant_label_correction_response_schema(),
            512,
        )
        .await
    }

    pub async fn normalize_aircraft_variants(
        &self,
        context: &VariantNormalizationContext,
    ) -> Result<Value> {
        self.generate_json(
            build_variant_normalization_prompt(context),
            gemini_variant_normalization_response_schema(),
            2048,
        )
        .await
    }

    pub async fn correct_aircraft_variant_normalization(
        &self,
        context: &VariantNormalizationContext,
        previous_response: &Value,
        correction_context: &Value,
    ) -> Result<Value> {
        self.generate_json(
            build_variant_normalization_correction_prompt(
                context,
                previous_response,
                correction_context,
            ),
            gemini_variant_normalization_response_schema(),
            4096,
        )
        .await
    }

    pub async fn estimate_avionics_metadata(
        &self,
        context: &AvionicsMetadataContext<'_>,
    ) -> Result<Value> {
        self.generate_grounded_json(
            build_avionics_metadata_prompt(context),
            gemini_avionics_metadata_response_schema(),
            1024,
        )
        .await
    }

    pub async fn resolve_avionics_unit(
        &self,
        context: &AvionicsUnitResolutionContext,
    ) -> Result<Value> {
        self.generate_grounded_json(
            build_avionics_unit_resolution_prompt(context),
            gemini_avionics_unit_resolution_response_schema(),
            2048,
        )
        .await
    }

    pub async fn correct_avionics_unit_resolution(
        &self,
        context: &AvionicsUnitResolutionContext,
        previous_response: &Value,
        correction_context: &AvionicsUnitResolutionCorrectionContext,
    ) -> Result<Value> {
        self.generate_grounded_json(
            build_avionics_unit_resolution_correction_prompt(
                context,
                previous_response,
                correction_context,
            ),
            gemini_avionics_unit_resolution_response_schema(),
            2048,
        )
        .await
    }

    pub async fn classify_avionics_unit_concreteness(
        &self,
        context: &AvionicsUnitResolutionContext,
    ) -> Result<Value> {
        self.generate_avionics_review_json(
            build_avionics_unit_concreteness_prompt(context),
            gemini_avionics_unit_concreteness_response_schema(),
            1024,
        )
        .await
    }

    pub async fn normalize_avionics_model_labels(
        &self,
        context: &AvionicsNormalizationContext,
    ) -> Result<Value> {
        self.generate_grounded_json(
            build_avionics_normalization_prompt(context),
            gemini_avionics_normalization_response_schema(),
            32_768,
        )
        .await
    }

    pub async fn correct_avionics_model_label_normalization(
        &self,
        context: &AvionicsNormalizationContext,
        previous_response: &Value,
        correction_context: &Value,
    ) -> Result<Value> {
        self.generate_json(
            build_avionics_normalization_correction_prompt(
                context,
                previous_response,
                correction_context,
            ),
            gemini_avionics_normalization_response_schema(),
            32_768,
        )
        .await
    }

    pub async fn estimate_default_aircraft_avionics(
        &self,
        context: &DefaultAvionicsContext<'_>,
    ) -> Result<Value> {
        self.generate_grounded_json(
            build_default_aircraft_avionics_prompt(context),
            gemini_default_aircraft_avionics_response_schema(),
            4096,
        )
        .await
    }

    pub async fn estimate_aircraft_spec_metadata(
        &self,
        context: &AircraftSpecMetadataContext<'_>,
    ) -> Result<Value> {
        self.generate_grounded_json(
            build_aircraft_spec_metadata_prompt(context),
            gemini_aircraft_spec_metadata_response_schema(),
            4096,
        )
        .await
    }

    async fn generate_json(
        &self,
        prompt: String,
        response_schema: Value,
        max_output_tokens: u64,
    ) -> Result<Value> {
        let content = self
            .generate_json_text(prompt.clone(), response_schema.clone(), max_output_tokens)
            .await?;
        match load_model_json(&content) {
            Ok(value) => Ok(value),
            Err(parse_error) => {
                let repair_prompt =
                    build_json_repair_prompt(&prompt, &content, &format!("{parse_error:#}"));
                let repair_tokens = max_output_tokens
                    .saturating_mul(2)
                    .max(max_output_tokens)
                    .min(GEMINI_JSON_REPAIR_MAX_OUTPUT_TOKENS);
                let repaired_content = self
                    .generate_json_text(repair_prompt, response_schema, repair_tokens)
                    .await?;
                load_model_json(&repaired_content).with_context(|| {
                    format!(
                        "Gemini returned invalid JSON after repair; original parse error: {parse_error:#}; repair response excerpt: {}",
                        response_excerpt(&repaired_content)
                    )
                })
            }
        }
    }

    async fn generate_grounded_json(
        &self,
        prompt: String,
        response_schema: Value,
        max_output_tokens: u64,
    ) -> Result<Value> {
        let content = self
            .generate_json_text_with_google_search(
                prompt.clone(),
                response_schema.clone(),
                max_output_tokens,
            )
            .await?;
        match load_model_json(&content) {
            Ok(value) => Ok(value),
            Err(parse_error) => {
                let repair_prompt =
                    build_json_repair_prompt(&prompt, &content, &format!("{parse_error:#}"));
                let repaired_content = self
                    .generate_json_text(repair_prompt, response_schema, max_output_tokens)
                    .await?;
                load_model_json(&repaired_content).with_context(|| {
                    format!(
                        "Gemini returned invalid grounded JSON after repair; original parse error: {parse_error:#}; repair response excerpt: {}",
                        response_excerpt(&repaired_content)
                    )
                })
            }
        }
    }

    async fn generate_avionics_review_json(
        &self,
        prompt: String,
        response_schema: Value,
        max_output_tokens: u64,
    ) -> Result<Value> {
        let content = self
            .generate_json_text_with_model_url(
                prompt.clone(),
                response_schema.clone(),
                max_output_tokens,
                false,
                &self.avionics_review_url,
            )
            .await?;
        match load_model_json(&content) {
            Ok(value) => Ok(value),
            Err(parse_error) => {
                let repair_prompt =
                    build_json_repair_prompt(&prompt, &content, &format!("{parse_error:#}"));
                let repaired_content = self
                    .generate_json_text_with_model_url(
                        repair_prompt,
                        response_schema,
                        max_output_tokens,
                        false,
                        &self.avionics_review_url,
                    )
                    .await?;
                load_model_json(&repaired_content).with_context(|| {
                    format!(
                        "Gemini avionics review returned invalid JSON after repair; original parse error: {parse_error:#}; repair response excerpt: {}",
                        response_excerpt(&repaired_content)
                    )
                })
            }
        }
    }

    async fn generate_json_text(
        &self,
        prompt: String,
        response_schema: Value,
        max_output_tokens: u64,
    ) -> Result<String> {
        self.generate_json_text_with_options(prompt, response_schema, max_output_tokens, false)
            .await
    }

    async fn generate_json_text_with_google_search(
        &self,
        prompt: String,
        response_schema: Value,
        max_output_tokens: u64,
    ) -> Result<String> {
        self.generate_json_text_with_options(prompt, response_schema, max_output_tokens, true)
            .await
    }

    async fn generate_json_text_with_options(
        &self,
        prompt: String,
        response_schema: Value,
        max_output_tokens: u64,
        google_search: bool,
    ) -> Result<String> {
        let url = if google_search {
            &self.grounded_url
        } else {
            &self.url
        };
        self.generate_json_text_with_model_url(
            prompt,
            response_schema,
            max_output_tokens,
            google_search,
            url,
        )
        .await
    }

    async fn generate_json_text_with_model_url(
        &self,
        prompt: String,
        response_schema: Value,
        max_output_tokens: u64,
        google_search: bool,
        url: &str,
    ) -> Result<String> {
        let mut generation_config = json!({
            "responseMimeType": "application/json",
            "responseSchema": response_schema,
            "maxOutputTokens": max_output_tokens,
        });
        if let Some(thinking_level) = &self.thinking_level {
            generation_config["thinkingConfig"] = json!({
                "thinkingLevel": thinking_level,
            });
        }

        let mut payload = json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [
                        {
                            "text": prompt,
                        }
                    ],
                }
            ],
            "generationConfig": generation_config,
        });
        if google_search {
            payload["tools"] = json!([
                {
                    "google_search": {}
                }
            ]);
        }

        let response = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .header("x-goog-api-key", &self.api_key)
            .json(&payload)
            .send()
            .await
            .context("Gemini extraction request failed")?;
        let status = response.status();
        let response_payload: Value = response
            .json()
            .await
            .with_context(|| format!("Gemini returned non-JSON response with status {status}"))?;
        if !status.is_success() {
            bail!("Gemini extraction failed with status {status}: {response_payload}");
        }
        gemini_response_text(&response_payload)
    }
}

pub async fn preview_listing_url(
    source_url: &str,
    extractor: &GeminiListingExtractor,
) -> Result<ListingPreview> {
    validate_source_url(source_url)?;
    let html = extractor.fetch_url(source_url).await?;
    parse_listing_html(source_url, &html, extractor).await
}

pub async fn parse_listing_html(
    source_url: &str,
    html: &str,
    extractor: &GeminiListingExtractor,
) -> Result<ListingPreview> {
    let listing_text = clean_listing_html(html);
    let structured = extractor.extract(&listing_text).await?;
    let parsed_listing = parsed_listing_from_model_output(&structured);
    let warnings = missing_field_warnings(&parsed_listing);
    Ok(ListingPreview {
        source_url: Some(source_url.to_string()),
        parsed_listing,
        warnings,
        context_text: Some(listing_text),
    })
}

pub fn preview_manual_listing(listing: &Value) -> ListingPreview {
    let parsed_listing = parsed_listing_from_model_output(listing);
    let mut warnings =
        vec!["manual listing has no source URL and will be created as invalid".to_string()];
    warnings.extend(missing_field_warnings(&parsed_listing));
    ListingPreview {
        source_url: None,
        parsed_listing,
        warnings,
        context_text: None,
    }
}

pub fn validate_source_url(source_url: &str) -> Result<()> {
    let parsed = Url::parse(source_url).context("source_url must be an absolute URL")?;
    match parsed.scheme() {
        "http" | "https" if parsed.host_str().is_some() => Ok(()),
        _ => bail!("source_url must be an absolute http or https URL"),
    }
}

fn build_extraction_prompt(listing_text: &str) -> String {
    format!(
        "Extract these fields from the aircraft sale listing text.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Fill these creation-critical fields with non-null values: manufacturer, model, variant, model_year, asking_price_usd, currency, airframe_hours, engine_hours, propeller_hours, status, avionics.\n\
- Use values from the listing text whenever possible.\n\
- Use null only for optional metadata fields: registration_number and serial_number.\n\
- asking_price_usd must be the aircraft asking price, not a loan payment.\n\
- model_year must be the aircraft model year, not an inspection or warranty date.\n\
- airframe_hours is total time, TTAF, TT, TTSN, or flight hours since new.\n\
- engine_hours is engine TTSN/SNEW/SMOH/SFRM time, not horsepower, TBO, or engine model.\n\
- propeller_hours is propeller TTSN/SNEW/SMOH/SPOH time, not blade count or model.\n\
- If engine or propeller time is absent but the listing clearly says all times are since new, reuse total time for engine_hours and propeller_hours.\n\
- registration_number may be an N-number or another registration value from Registration No/Reg/RN.\n\
- model is the depreciation/economic model family. It groups closely related variants that share the same broad aircraft family for value curves. Do not include generation, trim, turbo, pressurized, retractable, package, or serial suffix details here unless they are part of the broad family name.\n\
- variant is the concise aircraft configuration/designation label used for valuation grouping within the model family. Preserve material suffixes, generation labels, turbo/pressurized/retractable/amphibious/turbine modifiers, and other configuration-changing terms.\n\
- variant must omit the manufacturer name and model year.\n\
- variant must not repeat broad model-family or marketing-family words that are already represented by model unless that word is required to distinguish two material configurations in this model family.\n\
- If one possible label is a concise alphanumeric code and another is the same code plus a redundant family word from model, return the concise code.\n\
- If the variant is the model family plus a separable generation/configuration token, return only the generation/configuration token. Keep the model code only when the suffix is fused into an inseparable alphanumeric type designator.\n\
- model and variant are allowed to be identical only when the listing gives no more specific designation than the family name.\n\
- Do not convert model names to ICAO type designators.\n\
- avionics must come from the listing text and should include fixed installed avionics only.\n\
- For avionics model labels, preserve the full identifiable unit or suite code from the listing. Do not return bare numbers or generic labels such as 50, 60, 300, 440, 540, GPS, NAV/COM, Autopilot, or Transponder unless that exact bare label is the only supported identifier in the source text.\n\
- When a listing gives enough surrounding context to identify a common avionics unit, return that unit label, for example IFD 540 instead of 540, IFD 440 instead of 440, S-TEC 55X instead of System 55X, and Century 2000 instead of Autopilot.\n\
- Do not include explanations, markdown, comments, or extra keys.\n\n\
Listing text:\n{listing_text}",
        serde_json::to_string_pretty(&extraction_schema_description()).unwrap()
    )
}

fn extraction_schema_description() -> Value {
    json!({
        "manufacturer": "string",
        "model": "depreciation/economic model family; string",
        "variant": "exact advertised aircraft model designation; string",
        "model_year": "integer",
        "asking_price_usd": "number",
        "currency": "three-letter currency code, usually USD; string",
        "airframe_hours": "number",
        "engine_hours": "number",
        "propeller_hours": "number",
        "registration_number": "string or null",
        "serial_number": "string or null",
        "status": "active, sold, pending, or unknown",
        "avionics": [
            {
                "manufacturer": "string",
                "model": "string",
                "type": "GPS, NAV/COM, Transponder, Autopilot, Integrated Flight Deck, Audio Panel, Flight Display, Traffic, Datalink, Engine Monitor, or Unknown",
                "quantity": "integer",
            }
        ],
    })
}

fn build_model_family_confirmation_prompt(context: &ModelFamilyConfirmationContext<'_>) -> String {
    let source_url = context.source_url.unwrap_or("");
    let model_year = context
        .model_year
        .map(|value| value.to_string())
        .unwrap_or_default();
    let listing_context = context.listing_context.unwrap_or("");
    format!(
        "We need to canonicalize an aircraft sale listing to a known depreciation model family.\n\
Decide whether the extracted model family belongs to the same broad aircraft family as the candidate known model family.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Return same_model_family true when the extracted model and candidate model are the same depreciation/economic family, even if variants differ.\n\
- Treat generation, trim, turbo, pressurized, retractable, package, and minor suffix differences as variant-level details unless they identify a materially different model family in the listing context.\n\
- Return same_model_family true when extracted_model includes a variant suffix, generation, or configuration term attached to or adjacent to the base model code, and candidate_known_model is the broader family that should contain that variant.\n\
- Do not require candidate_known_model to repeat every variant suffix or configuration term from extracted_model; those belong in variant.\n\
- Return false when the strings refer to different broad model families, even if the manufacturer is the same or the names look similar.\n\
- Use the listing context, year, and source URL only as supporting context.\n\
- Do not include explanations, markdown, comments, or extra keys.\n\n\
Current extracted fields:\n\
manufacturer: {}\n\
extracted_model: {}\n\
extracted_variant: {}\n\
candidate_known_model: {}\n\
model_year: {model_year}\n\
source_url: {source_url}\n\n\
Listing context:\n{listing_context}",
        serde_json::to_string_pretty(&json!({
            "same_model_family": "boolean",
            "confidence": "high, medium, or low",
        }))
        .unwrap(),
        context.manufacturer,
        context.extracted_model,
        context.extracted_variant,
        context.candidate_model,
    )
}

fn build_variant_confirmation_prompt(context: &VariantConfirmationContext<'_>) -> String {
    let source_url = context.source_url.unwrap_or("");
    let model_year = context
        .model_year
        .map(|value| value.to_string())
        .unwrap_or_default();
    let listing_context = context.listing_context.unwrap_or("");
    format!(
        "We need to canonicalize an aircraft sale listing to a known exact aircraft variant.\n\
Decide whether the extracted variant and candidate known variant identify the same exact advertised model/configuration.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Return same_variant true only when both values identify the same exact aircraft variant/configuration.\n\
- Treat punctuation, word order, capitalization, redundant manufacturer/model-family/marketing-family words, and equivalent shorthand such as an appended T versus the word TURBO as non-material when the listing context supports the same configuration.\n\
- Treat generation and configuration-changing terms such as normally aspirated versus turbo, pressurized, retractable, turbine, or amphibious as material unless both sides refer to the same configuration.\n\
- Treat trim or package names as non-material unless the evidence shows that term is the only material distinction between variants.\n\
- Return false when the candidate is only the same model family but not the same exact variant.\n\
- Variant labels must omit manufacturer names and model years. Treat those as non-canonical noise when comparing variants.\n\
- Use the extracted model family, candidate model family, listing context, year, and source URL only as supporting context.\n\
- Do not include explanations, markdown, comments, or extra keys.\n\n\
Current extracted fields:\n\
manufacturer: {}\n\
extracted_model: {}\n\
extracted_variant: {}\n\
candidate_known_model: {}\n\
candidate_known_variant: {}\n\
model_year: {model_year}\n\
source_url: {source_url}\n\n\
Listing context:\n{listing_context}",
        serde_json::to_string_pretty(&json!({
            "same_variant": "boolean",
            "confidence": "high, medium, or low",
        }))
        .unwrap(),
        context.manufacturer,
        context.extracted_model,
        context.extracted_variant,
        context.candidate_model,
        context.candidate_variant,
    )
}

fn build_variant_label_correction_prompt(context: &VariantLabelCorrectionContext<'_>) -> String {
    let source_url = context.source_url.unwrap_or("");
    let listing_context = context.listing_context.unwrap_or("");
    format!(
        "Correct one aircraft variant label before it is stored in an aircraft valuation database.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- corrected_variant must be a concise exact aircraft variant/configuration label.\n\
- corrected_variant must omit the manufacturer name.\n\
- corrected_variant must omit the aircraft model year.\n\
- corrected_variant must not repeat broad model-family or marketing-family words already represented by model unless that word is required to distinguish two material configurations in this model family.\n\
- If one possible label is a concise alphanumeric code and another is the same code plus a redundant family word from model, return the concise code.\n\
- If the variant is the model family plus a separable generation/configuration token, return only the generation/configuration token. Keep the model code only when the suffix is fused into an inseparable alphanumeric type designator.\n\
- Do not drop material configuration words such as turbo, pressurized, retractable, amphibious, or turbine. If such a word is part of the variant, encode it in the concise corrected_variant using the best aircraft-designation form supported by the input and model family.\n\
- Keep material variant/configuration terms such as turbo, pressurized, retractable, amphibious, turbine, generation, and trim/package only when they identify the advertised configuration.\n\
- Do not convert the aircraft to an ICAO type designator.\n\
- Do not include markdown, comments, nulls, or extra keys.\n\n\
Input:\n\
manufacturer: {}\n\
model: {}\n\
variant: {}\n\
model_year: {}\n\
source_url: {source_url}\n\
issues: {}\n\n\
Listing context:\n{listing_context}",
        serde_json::to_string_pretty(&json!({
            "corrected_variant": "string",
            "confidence": "high, medium, or low",
            "rationale": "short string"
        }))
        .unwrap(),
        context.manufacturer,
        context.model,
        context.variant,
        context.model_year,
        serde_json::to_string_pretty(context.issues).unwrap(),
    )
}

fn build_variant_normalization_prompt(context: &VariantNormalizationContext) -> String {
    format!(
        "We need to clean up variant labels for existing aircraft sale listings that all belong to one manufacturer and model family.\n\
Group source variant labels that identify the same aircraft variant/configuration, and choose one canonical display label per group.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Every source variant from the input must appear exactly once across source_variants.\n\
- Do not invent source variant labels; source_variants must be copied exactly from the input variant values.\n\
- canonical_variant must be a non-empty string and must not be null.\n\
- canonical_variant must omit manufacturer names and model years.\n\
- canonical_variant must not repeat broad model-family or marketing-family words that are already represented by the input model unless that word is required to distinguish two material configurations in this model family.\n\
- If one source variant is a concise alphanumeric code and another source variant is the same code plus a redundant family word from the model family, group them and use the concise code as canonical_variant.\n\
- If one source variant expresses a material turbo/pressurized/retractable/etc. configuration as a suffix code while another writes the same configuration as a word, group them and prefer the concise aircraft-designation form.\n\
- If the source variant is the model family plus a separable generation/configuration token, canonical_variant should be only the generation/configuration token. Keep the model code only when the suffix is fused into an inseparable alphanumeric type designator.\n\
- Group labels that differ only by capitalization, punctuation, separators, word order, redundant manufacturer/model family words, or equivalent shorthand such as an appended T versus the word TURBO.\n\
- Keep variants separate when generation, normally aspirated versus turbo/pressurized/retractable/amphibious/turbine configuration, or another material aircraft configuration differs.\n\
- Treat marketing/package words as non-canonical unless the provided evidence shows that term is the only material distinction between variants.\n\
- Prefer concise canonical labels with uppercase aircraft codes and material modifiers, for example MODEL-GENERATION TURBO when that pattern matches the input evidence.\n\
- Use listing examples, years, registration numbers, and URLs only as supporting evidence.\n\
- If unsure whether two labels identify the same configuration, keep them separate.\n\
- Do not include markdown, comments, or extra keys.\n\n\
Input:\n{}",
        serde_json::to_string_pretty(&json!({
            "groups": [
                {
                    "canonical_variant": "string",
                    "source_variants": ["string"],
                    "rationale": "short string"
                }
            ]
        }))
        .unwrap(),
        serde_json::to_string_pretty(context).unwrap()
    )
}

fn build_variant_normalization_correction_prompt(
    context: &VariantNormalizationContext,
    previous_response: &Value,
    correction_context: &Value,
) -> String {
    format!(
        "Your previous aircraft variant normalization response was valid JSON but failed validation.\n\
Return one complete corrected JSON object that satisfies the same schema and replaces the previous response.\n\
Do not return a patch. Do not include markdown, comments, nulls, or extra keys.\n\n\
Validation details:\n{}\n\n\
Critical coverage rule:\n\
- Every source variant from the input must appear exactly once across source_variants.\n\
- Any input variant that is not a duplicate must be included as a singleton group.\n\
- Do not omit unchanged singleton variants.\n\n\
Specific correction instructions:\n\
- For every label listed in missing_source_variants, add it exactly once to the full replacement response.\n\
- If a missing source variant is an exact duplicate of a group already in previous_response, add that label to that group's source_variants.\n\
- If a missing source variant is not an exact duplicate, create a singleton group for it using that same label as canonical_variant.\n\
- If duplicated_source_variants is non-empty, remove duplicate occurrences and leave each repeated label in exactly one best-fitting group.\n\
- If unknown_source_variants is non-empty, remove those labels because they were not in the input.\n\
- Apply the original canonical-label rules when choosing canonical_variant: omit maker/year, remove redundant model-family words, and prefer the concise material aircraft-designation form.\n\n\
Original task and input:\n{}\n\n\
Previous response:\n{}",
        serde_json::to_string_pretty(correction_context).unwrap(),
        build_variant_normalization_prompt(context),
        serde_json::to_string_pretty(previous_response).unwrap()
    )
}

fn build_avionics_metadata_prompt(context: &AvionicsMetadataContext<'_>) -> String {
    format!(
        "Use Google Search grounding to estimate reference metadata for one installed aircraft avionics model.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- introduced_year is the first public release, certification, or common market introduction year for this avionics model. Return the best integer estimate; do not use null.\n\
- estimated_unit_value_usd is a reasonable {} USD market value contribution for one installed working unit or integrated suite as named. It should reflect the installed avionics contribution to aircraft value, including typical equipment, installation, certification, and integration value when the named item normally requires them. Do not return the value of the whole aircraft.\n\
- If the model name is a broad integrated suite or package, estimate the installed package/suite contribution represented by one parsed listing unit.\n\
- If the exact model is ambiguous, use manufacturer, model name, and avionics type to make the best conservative estimate.\n\
- Prefer manufacturer product pages, installation manuals, FAA/STC documents, reputable avionics shops, or equipment market references.\n\
- confidence must be high, medium, or low.\n\
- Do not include markdown, comments, nulls, or extra keys.\n\n\
manufacturer: {}\n\
model: {}\n\
avionics_type: {}\n\
value_reference_year: {}",
        serde_json::to_string_pretty(&json!({
            "introduced_year": "integer",
            "estimated_unit_value_usd": "number",
            "confidence": "high, medium, or low"
        }))
        .unwrap(),
        context.value_reference_year,
        context.manufacturer,
        context.model,
        context.avionics_type,
        context.value_reference_year,
    )
}

fn build_avionics_unit_resolution_prompt(context: &AvionicsUnitResolutionContext) -> String {
    format!(
        "Use Google Search grounding to verify one avionics candidate before it is stored in an aircraft valuation database.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Fill every field with a non-null value. Do not return null for any field.\n\
- status must be concrete, factory_default, or reject.\n\
- Use status=concrete only when the candidate identifies a real, concrete avionics unit, integrated suite, or named avionics package that exists as a product/configuration.\n\
- Use status=factory_default when the candidate is generic, class-only, feature-only, unknown, or not a concrete product, and you can identify a concrete factory/default unit for the same aircraft model_year and avionics type.\n\
- Use status=reject when the candidate is generic or nonexistent and no concrete factory/default replacement can be verified from reliable sources.\n\
- If the candidate term could be generic rather than a concrete model, make extra effort to validate that exact term as one concrete product/configuration before using status=concrete.\n\
- Treat status=concrete as invalid unless the source evidence supports one exact manufacturer/model or one exact named integrated suite/package, not just a capability, display size, equipment type, broad series/family, or multiple possible models.\n\
- Do not treat generic features/classes as concrete units. Examples: ADS-B, WAAS GPS, Dual WAAS, Remote Transponder, Standard Audio Panel, Audio Controller, Autopilot, Synthetic Vision, Engine Monitor, radios, NAV/COM, GPS, Traffic, Datalink Weather, Backup Instruments.\n\
- If the candidate appears to be a shorthand or malformed label, resolve it only when grounding or listing context identifies one exact unit. Otherwise use factory_default or reject.\n\
- For concrete and factory_default, manufacturer/model/type must be the verified avionics manufacturer, exact model/package label, and avionics type. Do not return the aircraft manufacturer unless it is truly the avionics unit maker.\n\
- For reject, set manufacturer, model, and type to empty strings, quantity to the input quantity, introduced_year to 0, estimated_unit_value_usd to 0, source_url and source_title to empty strings, confidence to low, and explain the reason in notes.\n\
- type must be one of: GPS, NAV/COM, Transponder, Autopilot, Integrated Flight Deck, Audio Panel, Flight Display, Traffic, Datalink, Engine Monitor, Standby Instrument, or Unknown.\n\
- introduced_year is the first public release, certification, or common market introduction year for the returned avionics model. Use 0 only for reject.\n\
- estimated_unit_value_usd is a reasonable {} USD market value contribution for one installed working unit or suite. Use 0 only for reject.\n\
- source_url and source_title must identify the best public source supporting the concrete unit or factory default. Use empty strings only for reject.\n\
- notes must briefly say whether the original candidate was verified, corrected, replaced by factory default, or rejected.\n\
- Do not include markdown, comments, nulls, or extra keys.\n\n\
Context:\n{}",
        serde_json::to_string_pretty(&json!({
            "status": "concrete, factory_default, or reject",
            "manufacturer": "string",
            "model": "string",
            "type": "GPS, NAV/COM, Transponder, Autopilot, Integrated Flight Deck, Audio Panel, Flight Display, Traffic, Datalink, Engine Monitor, Standby Instrument, or Unknown",
            "quantity": "integer",
            "introduced_year": "integer",
            "estimated_unit_value_usd": "number",
            "confidence": "high, medium, or low",
            "source_url": "string",
            "source_title": "string",
            "notes": "string"
        }))
        .unwrap(),
        context.value_reference_year,
        serde_json::to_string_pretty(context).unwrap()
    )
}

fn build_avionics_unit_concreteness_prompt(context: &AvionicsUnitResolutionContext) -> String {
    format!(
        "Classify whether an extracted avionics candidate looks like one concrete avionics product/configuration or a generic/ambiguous label.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- This is an independent validation check for a database ingestion pipeline.\n\
- classification must be concrete, generic, or ambiguous.\n\
- Use concrete only when the manufacturer/model/type together identify one specific avionics unit, installed integrated suite, or named avionics package.\n\
- Use generic when the model is primarily an equipment class, capability, feature, display size, broad series/family, marketing descriptor, or standard-equipment phrase.\n\
- Use ambiguous when it could refer to multiple models, a product family, a vendor line, or the manufacturer/type context is insufficient.\n\
- manufacturer_is_avionics_maker must be false if the manufacturer looks like an aircraft maker, alias, installer, parenthetical label, unknown/generic value, or not the maker of the avionics unit.\n\
- model_identifies_single_unit must be false if the model is class-only, feature-only, a broad series/family, slash-separated multiple model numbers, or a display/controller description rather than one exact unit.\n\
- generic_indicators should list the concrete reasons for generic/ambiguous classifications. Use an empty array for a high-confidence concrete unit.\n\
- Do not include markdown, comments, nulls, or extra keys.\n\n\
Context:\n{}",
        serde_json::to_string_pretty(&json!({
            "classification": "concrete, generic, or ambiguous",
            "manufacturer_is_avionics_maker": "boolean",
            "model_identifies_single_unit": "boolean",
            "confidence": "high, medium, or low",
            "generic_indicators": ["string"],
            "notes": "string"
        }))
        .unwrap(),
        serde_json::to_string_pretty(context).unwrap()
    )
}

fn build_avionics_unit_resolution_correction_prompt(
    context: &AvionicsUnitResolutionContext,
    previous_response: &Value,
    correction_context: &AvionicsUnitResolutionCorrectionContext,
) -> String {
    format!(
        "Use Google Search grounding to correct an avionics unit resolution before it is stored in an aircraft valuation database.\n\
The previous answer was rejected by a generic local review. Return a corrected JSON object with exactly this shape:\n{}\n\n\
Correction rules:\n\
- Fill every field with a non-null value. Do not return null for any field.\n\
- Address every issue in the review_context. Do not repeat the same problem.\n\
- status must be concrete, factory_default, or reject.\n\
- Use status=concrete only when a reliable public source verifies one exact avionics product, installed suite, or named package and supports the returned manufacturer/model.\n\
- Use status=factory_default only when the original candidate is generic or malformed but a reliable source verifies a concrete factory/default replacement for the same aircraft year/model/variant and avionics type.\n\
- Use status=reject when the previous response cannot be corrected to one verified concrete product/default.\n\
- If the prior manufacturer was an alias, aircraft manufacturer, parenthetical label, distributor, installer, or otherwise not the avionics maker, replace it with the verified avionics manufacturer or reject.\n\
- If the prior model was a capability, feature, equipment class, broad series/family, ambiguous slash-separated set, display description, controller description, or multiple possible products, replace it with one verified concrete unit/default or reject.\n\
- For reject, set manufacturer, model, and type to empty strings, quantity to the input quantity, introduced_year to 0, estimated_unit_value_usd to 0, source_url and source_title to empty strings, confidence to low, and explain the reason in notes.\n\
- For concrete and factory_default, source_url/source_title must support the corrected manufacturer/model and notes must briefly explain the correction.\n\
- Do not include markdown, comments, nulls, or extra keys.\n\n\
Original context:\n{}\n\n\
Previous rejected response:\n{}\n\n\
Review context:\n{}",
        serde_json::to_string_pretty(&json!({
            "status": "concrete, factory_default, or reject",
            "manufacturer": "string",
            "model": "string",
            "type": "GPS, NAV/COM, Transponder, Autopilot, Integrated Flight Deck, Audio Panel, Flight Display, Traffic, Datalink, Engine Monitor, Standby Instrument, or Unknown",
            "quantity": "integer",
            "introduced_year": "integer",
            "estimated_unit_value_usd": "number",
            "confidence": "high, medium, or low",
            "source_url": "string",
            "source_title": "string",
            "notes": "string"
        }))
        .unwrap(),
        serde_json::to_string_pretty(context).unwrap(),
        serde_json::to_string_pretty(previous_response).unwrap(),
        serde_json::to_string_pretty(correction_context).unwrap(),
    )
}

fn build_avionics_normalization_prompt(context: &AvionicsNormalizationContext) -> String {
    format!(
        "Use Google Search grounding to clean up avionics labels extracted from aircraft sale listings.\n\
Group source avionics model rows that identify the same installed avionics unit, suite, or package, and choose one canonical manufacturer, avionics type, and display model label per group.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Every input id must appear exactly once across source_ids.\n\
- Rows that are not duplicates must still be returned as singleton groups with source_ids containing only that row id.\n\
- The response is invalid if any input row is omitted, even when the row is unchanged.\n\
- Do not invent source ids; source_ids must be copied from input models.\n\
- canonical_manufacturer must be the avionics manufacturer or suite owner, not the aircraft manufacturer.\n\
- canonical_type must be one of: GPS, NAV/COM, Transponder, Autopilot, Integrated Flight Deck, Audio Panel, Flight Display, Traffic, Datalink, Engine Monitor, Standby Instrument, or Unknown.\n\
- canonical_model must be a non-empty string and must not be null.\n\
- Group labels that differ only by capitalization, spacing, punctuation, hyphens, slash separators, plus signs, or redundant manufacturer words.\n\
- Group obvious shorthand for the same unit or suite, for example G1000 NXi and G1000NXi.\n\
- Group rows across different source manufacturers or avionics types when the source row is clearly misclassified but the model label identifies the same installed unit.\n\
- When rows with the same model label have conflicting source avionics types, use grounding and the factual product role to choose canonical_type. Do not keep an obviously wrong source type.\n\
- Only merge rows when they identify the exact same installed hardware, exact same integrated suite generation, or exact same software-defined avionics package.\n\
- Do not create umbrella groups for a product family, product series, generation family, capability class, or vendor line.\n\
- Keep different primary model numbers separate even when they share a product family, market role, connector, display size, or manufacturer.\n\
- Keep different alphanumeric model designators separate when their digits or suffixes differ after removing spaces, hyphens, and manufacturer/family words. For example, 33ES and 330ES are different designators, not formatting variants.\n\
- Keep different suffixes or generations separate when they materially change capabilities or market value, including W, WAAS, Xi, NXi, Plus, Touch+, R, ES, and part-number display revisions.\n\
- Keep labels separate when they refer to materially different avionics generations, models, or units, for example G1000, G1000 NXi, Perspective, Perspective+, GTX 33, and GTX 345R.\n\
- Keep a broad integrated suite separate from individual components unless the input evidence clearly shows both labels are duplicate names for the same parsed listing unit.\n\
- Never merge individual components into an integrated suite or merge an integrated suite into individual components just because both appear in the same aircraft generation.\n\
- Do not combine slash-separated distinct models into one canonical_model such as 430/530, 650/750, KAP/KFC, or 55X/60. Split them unless the slash is merely formatting for the exact same named unit.\n\
- Examples that must stay separate unless an input row explicitly proves they are duplicate labels for the same parsed unit: GNS 430, GNS 430W, GNS 530, GTN 650, GTN 650Xi, GTN 750, GTN 750Xi, GNC 355, Aera 660, GPS 150, G5, GI 275, DFC90, DFC100, KAP 150, KFC 150, KMA 24, KMA 26, KT 74, KT 75, KT 76A, KT 76C, G1000, G1000 NXi, Perspective, Perspective+, and Perspective Touch+.\n\
- Generic capability/class labels are not duplicates of specific product codes. Keep labels like WAAS GPS, Dual WAAS, ADS-B, ADS-B Out, Remote Transponder, Transponder/ADS-B, Standard Audio Panel, Audio Controller, Audio Control Panel, Autopilot, Datalink Weather, Synthetic Vision, Traffic, Stormscope, Engine Monitor, Engine & Fuel Monitoring, Backup Instruments, and Standard Radio/Navigation separate from specific model-numbered units unless the generic label itself includes the exact model code.\n\
- If one row has a specific model number or named product and another row only has a capability, feature, generic standard-equipment phrase, or equipment class, keep them separate.\n\
- Generic rows may only be grouped with other generic rows when the labels have the same meaning and neither row has a more specific model code.\n\
- For shorthand labels like 430W, 540, 55X, 50, or 60, infer the canonical label only when the source manufacturer, type, and nearby labels clearly identify a specific product. Otherwise keep the row as a singleton with a conservative canonical label.\n\
- Do not use generic canonical_model values such as Series, System Components, Generic, Miscellaneous, Navigation Suite, or Integrated Avionics unless every source row in that group is already the same generic label.\n\
- Prefer concise canonical labels with the manufacturer omitted and the avionics code/version preserved.\n\
- If unsure whether two labels identify the same hardware/suite, keep them separate.\n\
- Do not include markdown, comments, nulls, or extra keys.\n\n\
Input:\n{}",
        serde_json::to_string_pretty(&json!({
            "groups": [
                {
                    "canonical_manufacturer": "string",
                    "canonical_type": "string",
                    "canonical_model": "string",
                    "source_ids": ["integer"],
                    "rationale": "short string"
                }
            ]
        }))
        .unwrap(),
        serde_json::to_string_pretty(context).unwrap()
    )
}

fn build_avionics_normalization_correction_prompt(
    context: &AvionicsNormalizationContext,
    previous_response: &Value,
    correction_context: &Value,
) -> String {
    format!(
        "Your previous avionics normalization response was valid JSON but failed validation.\n\
Return one complete corrected JSON object that satisfies the same schema and replaces the previous response.\n\
Do not return a patch. Do not include markdown, comments, nulls, or extra keys.\n\n\
Validation details:\n{}\n\n\
Critical coverage rule:\n\
- Every input id must appear exactly once across source_ids.\n\
- Any input row that is not a duplicate must be included as a singleton group.\n\
- Do not omit unchanged singleton rows.\n\n\
Specific correction instructions:\n\
- For every row listed in missing_rows, add its id exactly once to the full replacement response.\n\
- If a missing row is an exact duplicate of a group already in previous_response, add that id to that group.\n\
- If a missing row is not an exact duplicate, create a singleton group for it using that row's current manufacturer, avionics_type, and model as the canonical values.\n\
- If repeated_ids is non-empty, remove the duplicate occurrence and leave each repeated id in exactly one best-fitting group.\n\
- If unexpected_ids is non-empty, remove those ids because they were not in the input.\n\n\
Original task and input:\n{}\n\n\
Previous response:\n{}",
        serde_json::to_string_pretty(correction_context).unwrap(),
        build_avionics_normalization_prompt(context),
        serde_json::to_string_pretty(previous_response).unwrap()
    )
}

fn build_default_aircraft_avionics_prompt(context: &DefaultAvionicsContext<'_>) -> String {
    let source_url = context.source_url.unwrap_or("");
    let nearby_price_points = serde_json::to_string_pretty(context.nearby_price_points)
        .unwrap_or_else(|_| "[]".to_string());
    format!(
        "Use Google Search grounding to identify the standard factory/default avionics and nominal new-price point for this aircraft make/model/variant and model year.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Fill every field with a non-null value. Do not return null for any field.\n\
- purchase_price_new_usd must be the nominal USD new/base price for this aircraft model year with standard/default equipment. Do not convert it to current dollars.\n\
- purchase_price_reference_year must be the year of the source price. Prefer the exact model_year; otherwise use the closest reliable published new-price year and explain the offset in price_source_notes.\n\
- Prefer manufacturer price sheets, MSRP/order guides, order forms, launch material, historical aircraft price guides, or reputable archived new-price references.\n\
- Do not use ordinary used-aircraft asking prices for purchase_price_new_usd.\n\
- listing_source_url is evidence about this used listing only. Do not return listing_source_url or another ordinary listing page as price_source_url.\n\
- Use nearby_model_family_price_points only as chronology sanity context. The returned price should be plausible relative to adjacent model years unless the cited source directly supports a discontinuity and price_source_notes explains it.\n\
- Return the default or standard factory avionics for the aircraft model year, not optional upgrades from one used listing.\n\
- Include avionics that materially affect aircraft value: integrated flight decks, major flight displays, GPS/NAV/COM units, transponders/ADS-B, autopilots, audio panels, traffic/weather/datalink units, standby instruments, and engine monitors.\n\
- Do not include generic words such as glass panel, avionics suite, or radios unless that is the actual named suite/package.\n\
- manufacturer and model must identify the avionics unit or suite, not the aircraft.\n\
- type must be one of: GPS, NAV/COM, Transponder, Autopilot, Integrated Flight Deck, Audio Panel, Flight Display, Traffic, Datalink, Engine Monitor, Standby Instrument, or Unknown.\n\
- quantity is the installed count for that standard equipment item.\n\
- introduced_year is the first public release, certification, or common market introduction year for the avionics model. Return the best integer estimate.\n\
- estimated_unit_value_usd is a reasonable {} USD market value contribution for one installed working unit or integrated suite as named. It should reflect the installed avionics contribution to aircraft value, including typical equipment, installation, certification, and integration value when the named item normally requires them. Do not return the value of the whole aircraft.\n\
- confidence must be high, medium, or low.\n\
- source_url and source_title must identify the best public source supporting the default avionics for this aircraft/year.\n\
- notes must briefly state whether the source was direct manufacturer evidence or an inference from year/configuration evidence.\n\
- price_source_url and price_source_title must identify the best public source supporting the numeric new-price point and year.\n\
- price_source_confidence must be high, medium, or low.\n\
- If exact equipment differs by serial/package and you cannot tell, return the most common standard equipment for that model year and set confidence low or medium.\n\
- Do not include markdown, comments, nulls, or extra keys.\n\n\
Aircraft:\n\
manufacturer: {}\n\
model_family: {}\n\
variant: {}\n\
model_year: {}\n\
listing_source_url: {}\n\
value_reference_year: {}\n\
nearby_model_family_price_points:\n{}",
        serde_json::to_string_pretty(&json!({
            "purchase_price_new_usd": "number",
            "purchase_price_reference_year": "integer",
            "price_source_url": "string",
            "price_source_title": "string",
            "price_source_notes": "string",
            "price_source_confidence": "high, medium, or low",
            "avionics": [
                {
                    "manufacturer": "string",
                    "model": "string",
                    "type": "string",
                    "quantity": "integer",
                    "introduced_year": "integer",
                    "estimated_unit_value_usd": "number",
                    "confidence": "high, medium, or low",
                    "source_url": "string",
                    "source_title": "string",
                    "notes": "string"
                }
            ]
        }))
        .unwrap(),
        context.value_reference_year,
        context.manufacturer,
        context.model,
        context.variant,
        context.model_year,
        source_url,
        context.value_reference_year,
        nearby_price_points,
    )
}

fn build_aircraft_spec_metadata_prompt(context: &AircraftSpecMetadataContext<'_>) -> String {
    let listing_contexts = context
        .listing_contexts
        .iter()
        .enumerate()
        .map(|(index, listing)| {
            format!(
                "Listing {}:\nmodel_year: {}\nasking_price_usd: {}\nairframe_hours: {}\nengine_hours: {}\npropeller_hours: {}\nsource_url: {}\ntext:\n{}",
                index + 1,
                listing.model_year,
                listing.asking_price_usd,
                listing.airframe_hours,
                listing.engine_hours,
                listing.propeller_hours,
                listing.source_url,
                listing.listing_text,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    format!(
        "Estimate aircraft variant operating, airframe, engine, propeller, and depreciation metadata.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Fill every field with a non-null value. Do not return null for any field.\n\
- The aircraft identity is manufacturer/model_family/variant_context below. Return values for that specific variant because airframe, engine, propeller, and fuel burn can differ by variant or generation.\n\
- Do not use maker/model-specific logic. Return data values that generic code can store and reuse.\n\
- depreciation_profile must be generic:all. The fitted database model will replace it when enough listing samples exist.\n\
- Do not choose depreciation coefficients; the database fitter learns them from listings.\n\
- Do not estimate new purchase prices here. Model-year price points and default avionics are stored separately.\n\
- Avionics are depreciated separately by installed unit; do not adjust operating or powerplant values to account for a listing's upgraded avionics.\n\
- fuel_burn_gph is cruise fuel burn in gallons per hour for typical owner operation.\n\
- engine_count and propeller_count are integer installed counts.\n\
- engine_manufacturer and engine_model identify the installed engine model for this variant. Use the actual engine make/model, not the aircraft maker/model.\n\
- engine_tbo_hours and propeller_tbo_hours are overhaul intervals in hours. Use representative values for this variant so generic timed-component logic can compute remaining-life adjustments.\n\
- engine_overhaul_cost_usd and propeller_overhaul_cost_usd are {} USD overhaul costs for one engine or one propeller assembly.\n\
- engine_value_baseline_life_fraction and propeller_value_baseline_life_fraction are fractions from 0.0 to 1.0 representing typical mid-market remaining life assumed in the base asking market. Use 0.5 when unsure.\n\
- propeller_manufacturer and propeller_model identify the installed propeller model or the closest specific propeller family. Use the actual propeller make/model, not the aircraft maker/model.\n\
- powerplant_source_url, powerplant_source_title, and powerplant_source_confidence identify the best factual source supporting the engine/propeller identity and TBO assumptions. Use the listing source URL if it directly states the facts; otherwise use a manufacturer, type certificate, POH, service, or reputable maintenance reference. Confidence must be high, medium, or low.\n\
- annual_inspection_usd is the typical annual inspection/maintenance fixed cost in {} USD.\n\
- other_maintenance_per_hour is variable maintenance reserve excluding fuel, oil, engine overhaul, and propeller overhaul.\n\
- confidence must be high, medium, or low.\n\
- Use the listing asking prices only as sanity-check context for aircraft class and market; they are not the replacement/new price basis.\n\
- Do not include markdown, comments, explanations, nulls, or extra keys.\n\n\
Aircraft:\n\
manufacturer: {}\n\
model_family: {}\n\
variant_context: {}\n\
value_reference_year: {}\n\n\
Stored plugin listing evidence:\n{}",
        serde_json::to_string_pretty(&json!({
            "depreciation_profile": "generic:all",
            "fuel_burn_gph": "number",
            "oil_quarts_per_hour": "number",
            "oil_price_per_quart_usd": "number",
            "engine_manufacturer": "string",
            "engine_model": "string",
            "engine_count": "integer",
            "engine_tbo_hours": "number",
            "engine_overhaul_cost_usd": "number",
            "engine_value_baseline_life_fraction": "number",
            "propeller_manufacturer": "string",
            "propeller_model": "string",
            "propeller_count": "integer",
            "propeller_tbo_hours": "number",
            "propeller_overhaul_cost_usd": "number",
            "propeller_value_baseline_life_fraction": "number",
            "powerplant_source_url": "string",
            "powerplant_source_title": "string",
            "powerplant_source_confidence": "high, medium, or low",
            "annual_inspection_usd": "number",
            "other_maintenance_per_hour": "number",
            "confidence": "high, medium, or low"
        }))
        .unwrap(),
        context.value_reference_year,
        context.value_reference_year,
        context.manufacturer,
        context.model,
        context.variant_context,
        context.value_reference_year,
        listing_contexts,
    )
}

fn build_json_repair_prompt(
    original_prompt: &str,
    invalid_response: &str,
    parse_error: &str,
) -> String {
    format!(
        "Your previous response was not valid JSON and could not be parsed.\n\
Return only one corrected JSON object that satisfies the same response schema. Do not include markdown, comments, explanations, or extra text.\n\n\
Parse error:\n{parse_error}\n\n\
Original task:\n{original_prompt}\n\n\
Previous invalid response:\n{}",
        response_excerpt(invalid_response)
    )
}

fn gemini_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "manufacturer": {"type": "string"},
            "model": {"type": "string"},
            "variant": {"type": "string"},
            "model_year": {"type": "integer"},
            "asking_price_usd": {"type": "number"},
            "currency": {"type": "string"},
            "airframe_hours": {"type": "number"},
            "engine_hours": {"type": "number"},
            "propeller_hours": {"type": "number"},
            "registration_number": {"type": "string", "nullable": true},
            "serial_number": {"type": "string", "nullable": true},
            "status": {
                "type": "string",
                "enum": ["active", "sold", "pending", "unknown"]
            },
            "avionics": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "manufacturer": {"type": "string"},
                        "model": {"type": "string"},
                        "type": {"type": "string"},
                        "quantity": {"type": "integer"}
                    },
                    "required": ["manufacturer", "model", "type", "quantity"],
                    "propertyOrdering": ["manufacturer", "model", "type", "quantity"]
                }
            }
        },
        "required": [
            "manufacturer", "model", "variant", "model_year", "asking_price_usd",
            "currency", "airframe_hours", "engine_hours", "propeller_hours",
            "registration_number", "serial_number", "status", "avionics"
        ],
        "propertyOrdering": [
            "manufacturer", "model", "variant", "model_year", "asking_price_usd",
            "currency", "airframe_hours", "engine_hours", "propeller_hours",
            "registration_number", "serial_number", "status", "avionics"
        ]
    })
}

fn gemini_model_confirmation_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "same_model_family": {"type": "boolean"},
            "confidence": {
                "type": "string",
                "enum": ["high", "medium", "low"]
            },
        },
        "required": ["same_model_family", "confidence"],
        "propertyOrdering": ["same_model_family", "confidence"],
    })
}

fn gemini_variant_confirmation_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "same_variant": {"type": "boolean"},
            "confidence": {
                "type": "string",
                "enum": ["high", "medium", "low"]
            },
        },
        "required": ["same_variant", "confidence"],
        "propertyOrdering": ["same_variant", "confidence"],
    })
}

fn gemini_variant_label_correction_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "corrected_variant": {"type": "string"},
            "confidence": {
                "type": "string",
                "enum": ["high", "medium", "low"]
            },
            "rationale": {"type": "string"}
        },
        "required": ["corrected_variant", "confidence", "rationale"],
        "propertyOrdering": ["corrected_variant", "confidence", "rationale"],
    })
}

fn gemini_variant_normalization_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "groups": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "canonical_variant": {"type": "string"},
                        "source_variants": {
                            "type": "array",
                            "items": {"type": "string"}
                        },
                        "rationale": {"type": "string"}
                    },
                    "required": ["canonical_variant", "source_variants", "rationale"],
                    "propertyOrdering": ["canonical_variant", "source_variants", "rationale"]
                }
            }
        },
        "required": ["groups"],
        "propertyOrdering": ["groups"],
    })
}

fn gemini_avionics_metadata_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "introduced_year": {"type": "integer"},
            "estimated_unit_value_usd": {"type": "number"},
            "confidence": {
                "type": "string",
                "enum": ["high", "medium", "low"]
            }
        },
        "required": ["introduced_year", "estimated_unit_value_usd", "confidence"],
        "propertyOrdering": [
            "introduced_year",
            "estimated_unit_value_usd",
            "confidence"
        ],
    })
}

fn gemini_avionics_unit_resolution_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": {
                "type": "string",
                "enum": ["concrete", "factory_default", "reject"]
            },
            "manufacturer": {"type": "string"},
            "model": {"type": "string"},
            "type": {
                "type": "string",
                "enum": [
                    "GPS",
                    "NAV/COM",
                    "Transponder",
                    "Autopilot",
                    "Integrated Flight Deck",
                    "Audio Panel",
                    "Flight Display",
                    "Traffic",
                    "Datalink",
                    "Engine Monitor",
                    "Standby Instrument",
                    "Unknown"
                ]
            },
            "quantity": {"type": "integer"},
            "introduced_year": {"type": "integer"},
            "estimated_unit_value_usd": {"type": "number"},
            "confidence": {
                "type": "string",
                "enum": ["high", "medium", "low"]
            },
            "source_url": {"type": "string"},
            "source_title": {"type": "string"},
            "notes": {"type": "string"}
        },
        "required": [
            "status",
            "manufacturer",
            "model",
            "type",
            "quantity",
            "introduced_year",
            "estimated_unit_value_usd",
            "confidence",
            "source_url",
            "source_title",
            "notes"
        ],
        "propertyOrdering": [
            "status",
            "manufacturer",
            "model",
            "type",
            "quantity",
            "introduced_year",
            "estimated_unit_value_usd",
            "confidence",
            "source_url",
            "source_title",
            "notes"
        ],
    })
}

fn gemini_avionics_unit_concreteness_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "classification": {
                "type": "string",
                "enum": ["concrete", "generic", "ambiguous"]
            },
            "manufacturer_is_avionics_maker": {"type": "boolean"},
            "model_identifies_single_unit": {"type": "boolean"},
            "confidence": {
                "type": "string",
                "enum": ["high", "medium", "low"]
            },
            "generic_indicators": {
                "type": "array",
                "items": {"type": "string"}
            },
            "notes": {"type": "string"}
        },
        "required": [
            "classification",
            "manufacturer_is_avionics_maker",
            "model_identifies_single_unit",
            "confidence",
            "generic_indicators",
            "notes"
        ],
        "propertyOrdering": [
            "classification",
            "manufacturer_is_avionics_maker",
            "model_identifies_single_unit",
            "confidence",
            "generic_indicators",
            "notes"
        ],
    })
}

fn gemini_avionics_normalization_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "groups": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "canonical_manufacturer": {"type": "string"},
                        "canonical_type": {"type": "string"},
                        "canonical_model": {"type": "string"},
                        "source_ids": {
                            "type": "array",
                            "items": {"type": "integer"}
                        },
                        "rationale": {"type": "string"}
                    },
                    "required": ["canonical_manufacturer", "canonical_type", "canonical_model", "source_ids", "rationale"],
                    "propertyOrdering": ["canonical_manufacturer", "canonical_type", "canonical_model", "source_ids", "rationale"]
                }
            }
        },
        "required": ["groups"],
        "propertyOrdering": ["groups"],
    })
}

fn gemini_default_aircraft_avionics_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "purchase_price_new_usd": {"type": "number"},
            "purchase_price_reference_year": {"type": "integer"},
            "price_source_url": {"type": "string"},
            "price_source_title": {"type": "string"},
            "price_source_notes": {"type": "string"},
            "price_source_confidence": {
                "type": "string",
                "enum": ["high", "medium", "low"]
            },
            "avionics": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "manufacturer": {"type": "string"},
                        "model": {"type": "string"},
                        "type": {
                            "type": "string",
                            "enum": [
                                "GPS",
                                "NAV/COM",
                                "Transponder",
                                "Autopilot",
                                "Integrated Flight Deck",
                                "Audio Panel",
                                "Flight Display",
                                "Traffic",
                                "Datalink",
                                "Engine Monitor",
                                "Standby Instrument",
                                "Unknown"
                            ]
                        },
                        "quantity": {"type": "integer"},
                        "introduced_year": {"type": "integer"},
                        "estimated_unit_value_usd": {"type": "number"},
                        "confidence": {
                            "type": "string",
                            "enum": ["high", "medium", "low"]
                        },
                        "source_url": {"type": "string"},
                        "source_title": {"type": "string"},
                        "notes": {"type": "string"}
                    },
                    "required": [
                        "manufacturer",
                        "model",
                        "type",
                        "quantity",
                        "introduced_year",
                        "estimated_unit_value_usd",
                        "confidence",
                        "source_url",
                        "source_title",
                        "notes"
                    ],
                    "propertyOrdering": [
                        "manufacturer",
                        "model",
                        "type",
                        "quantity",
                        "introduced_year",
                        "estimated_unit_value_usd",
                        "confidence",
                        "source_url",
                        "source_title",
                        "notes"
                    ]
                }
            }
        },
        "required": [
            "purchase_price_new_usd",
            "purchase_price_reference_year",
            "price_source_url",
            "price_source_title",
            "price_source_notes",
            "price_source_confidence",
            "avionics"
        ],
        "propertyOrdering": [
            "purchase_price_new_usd",
            "purchase_price_reference_year",
            "price_source_url",
            "price_source_title",
            "price_source_notes",
            "price_source_confidence",
            "avionics"
        ],
    })
}

fn gemini_aircraft_spec_metadata_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "depreciation_profile": {
                "type": "string",
                "enum": ["generic:all"]
            },
            "fuel_burn_gph": {"type": "number"},
            "oil_quarts_per_hour": {"type": "number"},
            "oil_price_per_quart_usd": {"type": "number"},
            "engine_manufacturer": {"type": "string"},
            "engine_model": {"type": "string"},
            "engine_count": {"type": "integer"},
            "engine_tbo_hours": {"type": "number"},
            "engine_overhaul_cost_usd": {"type": "number"},
            "engine_value_baseline_life_fraction": {"type": "number"},
            "propeller_manufacturer": {"type": "string"},
            "propeller_model": {"type": "string"},
            "propeller_count": {"type": "integer"},
            "propeller_tbo_hours": {"type": "number"},
            "propeller_overhaul_cost_usd": {"type": "number"},
            "propeller_value_baseline_life_fraction": {"type": "number"},
            "powerplant_source_url": {"type": "string"},
            "powerplant_source_title": {"type": "string"},
            "powerplant_source_confidence": {
                "type": "string",
                "enum": ["high", "medium", "low"]
            },
            "annual_inspection_usd": {"type": "number"},
            "other_maintenance_per_hour": {"type": "number"},
            "confidence": {
                "type": "string",
                "enum": ["high", "medium", "low"]
            }
        },
        "required": [
            "depreciation_profile",
            "fuel_burn_gph",
            "oil_quarts_per_hour",
            "oil_price_per_quart_usd",
            "engine_manufacturer",
            "engine_model",
            "engine_count",
            "engine_tbo_hours",
            "engine_overhaul_cost_usd",
            "engine_value_baseline_life_fraction",
            "propeller_manufacturer",
            "propeller_model",
            "propeller_count",
            "propeller_tbo_hours",
            "propeller_overhaul_cost_usd",
            "propeller_value_baseline_life_fraction",
            "powerplant_source_url",
            "powerplant_source_title",
            "powerplant_source_confidence",
            "annual_inspection_usd",
            "other_maintenance_per_hour",
            "confidence"
        ],
        "propertyOrdering": [
            "depreciation_profile",
            "fuel_burn_gph",
            "oil_quarts_per_hour",
            "oil_price_per_quart_usd",
            "engine_manufacturer",
            "engine_model",
            "engine_count",
            "engine_tbo_hours",
            "engine_overhaul_cost_usd",
            "engine_value_baseline_life_fraction",
            "propeller_manufacturer",
            "propeller_model",
            "propeller_count",
            "propeller_tbo_hours",
            "propeller_overhaul_cost_usd",
            "propeller_value_baseline_life_fraction",
            "powerplant_source_url",
            "powerplant_source_title",
            "powerplant_source_confidence",
            "annual_inspection_usd",
            "other_maintenance_per_hour",
            "confidence"
        ],
    })
}

fn gemini_response_text(response_payload: &Value) -> Result<String> {
    let candidates = response_payload
        .get("candidates")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
        .ok_or_else(|| anyhow!("Gemini response did not include candidates"))?;
    let parts = candidates[0]
        .get("content")
        .and_then(|value| value.get("parts"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Gemini response did not include content parts"))?;
    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        bail!("Gemini response did not include text content");
    }
    Ok(text)
}

fn load_model_json(content: &str) -> Result<Value> {
    match serde_json::from_str::<Value>(content) {
        Ok(Value::Object(_)) => {
            serde_json::from_str(content).context("Gemini returned invalid JSON")
        }
        Ok(_) => bail!("Gemini JSON response must be an object"),
        Err(_) => {
            let Some(start) = content.find('{') else {
                bail!("Gemini did not return JSON");
            };
            let Some(end) = content.rfind('}') else {
                bail!("Gemini returned invalid JSON");
            };
            let parsed: Value = serde_json::from_str(&content[start..=end])
                .context("Gemini returned invalid JSON")?;
            if parsed.is_object() {
                Ok(parsed)
            } else {
                bail!("Gemini JSON response must be an object");
            }
        }
    }
}

fn response_excerpt(content: &str) -> String {
    const MAX_EXCERPT_CHARS: usize = 12_000;
    let trimmed = content.trim();
    let mut excerpt = trimmed.chars().take(MAX_EXCERPT_CHARS).collect::<String>();
    if trimmed.chars().count() > MAX_EXCERPT_CHARS {
        excerpt.push_str("\n...[truncated]");
    }
    excerpt
}

pub fn parsed_listing_from_model_output(value: &Value) -> ParsedListing {
    let object = value
        .get("parsed_listing")
        .and_then(Value::as_object)
        .or_else(|| value.as_object());
    let empty = Map::new();
    let data = object.unwrap_or(&empty);
    let manufacturer =
        optional_string(data.get("manufacturer")).map(|value| canonical_manufacturer_name(&value));
    let model = optional_string(data.get("model"));
    let variant = optional_string(data.get("variant"));
    let mut registration_number = optional_string(data.get("registration_number"));
    let serial_number = optional_string(data.get("serial_number"));
    if let (Some(registration), Some(serial)) = (&registration_number, &serial_number) {
        if normalize_name(registration) == normalize_name(serial)
            && !registration.to_uppercase().starts_with('N')
        {
            registration_number = None;
        }
    }

    ParsedListing {
        manufacturer,
        model,
        variant,
        model_year: optional_i64_in_range(data.get("model_year"), 1900, 2039),
        asking_price_usd: optional_f64_min(data.get("asking_price_usd"), 10_000.0),
        currency: optional_string(data.get("currency"))
            .unwrap_or_else(|| "USD".to_string())
            .to_uppercase(),
        airframe_hours: optional_nonnegative_f64(data.get("airframe_hours")),
        engine_hours: optional_nonnegative_f64(data.get("engine_hours")),
        propeller_hours: optional_nonnegative_f64(data.get("propeller_hours")),
        registration_number,
        serial_number,
        status: optional_string(data.get("status")).unwrap_or_else(|| "active".to_string()),
        avionics: model_avionics(data.get("avionics")),
    }
}

fn model_avionics(value: Option<&Value>) -> Vec<ParsedAvionics> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut avionics = Vec::new();
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(manufacturer) = optional_string(object.get("manufacturer")) else {
            continue;
        };
        let Some(model) = optional_string(object.get("model")) else {
            continue;
        };
        let avionics_type =
            optional_string(object.get("type")).unwrap_or_else(|| "Unknown".to_string());
        let key = (
            normalize_name(&manufacturer),
            normalize_name(&model),
            normalize_name(&avionics_type),
        );
        if !seen.insert(key) {
            continue;
        }
        avionics.push(ParsedAvionics {
            manufacturer: canonical_manufacturer_name(&manufacturer),
            model,
            avionics_type,
            quantity: optional_i64_min(object.get("quantity"), 1).unwrap_or(1),
        });
    }
    avionics
}

fn missing_field_warnings(parsed: &ParsedListing) -> Vec<String> {
    let mut warnings = Vec::new();
    for (field_name, missing) in [
        ("manufacturer", parsed.manufacturer.is_none()),
        ("model", parsed.model.is_none()),
        ("variant", parsed.variant.is_none()),
        ("model_year", parsed.model_year.is_none()),
        ("asking_price_usd", parsed.asking_price_usd.is_none()),
        ("airframe_hours", parsed.airframe_hours.is_none()),
        ("engine_hours", parsed.engine_hours.is_none()),
        ("propeller_hours", parsed.propeller_hours.is_none()),
    ] {
        if missing {
            warnings.push(format!("{field_name} not found"));
        }
    }
    if parsed.avionics.is_empty() {
        warnings.push("avionics not found".to_string());
    }
    warnings
}

pub fn optional_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            let normalized = normalize_name(trimmed);
            (!trimmed.is_empty()
                && !matches!(
                    normalized.as_str(),
                    "unknown" | "none" | "na" | "n/a" | "notavailable" | "null"
                ))
            .then(|| trimmed.to_string())
        }
        Some(Value::Number(value)) => Some(value.to_string()),
        Some(Value::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

pub fn optional_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(value)) => value.as_f64(),
        Some(Value::String(value)) => {
            let cleaned = value.replace([',', '$'], "").trim().to_string();
            if cleaned.is_empty() {
                None
            } else {
                cleaned.parse::<f64>().ok()
            }
        }
        _ => None,
    }
}

pub fn optional_i64(value: Option<&Value>) -> Option<i64> {
    optional_f64(value).map(|value| value as i64)
}

fn optional_nonnegative_f64(value: Option<&Value>) -> Option<f64> {
    optional_f64(value).filter(|value| *value >= 0.0)
}

fn optional_f64_min(value: Option<&Value>, minimum: f64) -> Option<f64> {
    optional_f64(value).filter(|value| *value >= minimum)
}

fn optional_i64_min(value: Option<&Value>, minimum: i64) -> Option<i64> {
    optional_i64(value).filter(|value| *value >= minimum)
}

fn optional_i64_in_range(value: Option<&Value>, minimum: i64, maximum: i64) -> Option<i64> {
    optional_i64(value).filter(|value| *value >= minimum && *value <= maximum)
}

fn environment_u64(name: &str, default: u64) -> Result<u64> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .with_context(|| format!("{name} must be an integer")),
        _ => Ok(default),
    }
}

async fn fetch_url(source_url: &str, browser: &eoka::Browser) -> Result<String> {
    let settle_milliseconds = environment_u64("AIRCOST_EOKA_SETTLE_MILLISECONDS", 1200)?;
    let page = browser
        .new_page(source_url)
        .await
        .context("could not open source_url with eoka")?;

    let target_id = page.target_id().to_string();
    let result = async {
        if settle_milliseconds > 0 {
            page.wait(settle_milliseconds).await;
        }

        page.content()
            .await
            .context("could not read rendered page HTML from eoka")
    }
    .await;

    let close_result = browser
        .close_tab(&target_id)
        .await
        .context("could not close eoka tab");
    match result {
        Ok(html) => {
            close_result?;
            Ok(html)
        }
        Err(error) => {
            let _ = close_result;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parsed_listing_from_model_output, preview_manual_listing};

    #[test]
    fn normalizes_model_output() {
        let parsed = parsed_listing_from_model_output(&json!({
            "manufacturer": "Cirrus Aircraft",
            "model": "SR22",
            "variant": "SR22-G6 TURBO",
            "model_year": 2022,
            "asking_price_usd": "874,900",
            "currency": "usd",
            "airframe_hours": 170,
            "engine_hours": 170,
            "propeller_hours": 170,
            "registration_number": "8680",
            "serial_number": "8680",
            "avionics": [
                {"manufacturer": "Garmin", "model": "Perspective+", "type": "Integrated Flight Deck", "quantity": 1}
            ]
        }));

        assert_eq!(parsed.manufacturer.as_deref(), Some("Cirrus"));
        assert_eq!(parsed.model.as_deref(), Some("SR22"));
        assert_eq!(parsed.variant.as_deref(), Some("SR22-G6 TURBO"));
        assert_eq!(parsed.asking_price_usd, Some(874900.0));
        assert_eq!(parsed.currency, "USD");
        assert_eq!(parsed.registration_number, None);
    }

    #[test]
    fn manual_preview_warns_when_unsourced() {
        let preview = preview_manual_listing(&json!({
            "manufacturer": "Cirrus",
            "model": "SR20",
            "variant": "SR20-G6",
            "model_year": 2023,
            "asking_price_usd": 579000,
            "airframe_hours": 75,
            "engine_hours": 75,
            "propeller_hours": 75,
            "avionics": [
                {"manufacturer": "Garmin", "model": "Perspective+", "type": "Integrated Flight Deck"}
            ]
        }));

        assert!(preview.source_url.is_none());
        assert!(preview.warnings[0].contains("manual listing"));
        assert_eq!(
            preview.parsed_listing.manufacturer.as_deref(),
            Some("Cirrus")
        );
    }
}
