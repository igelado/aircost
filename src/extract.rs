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

#[derive(Debug, Serialize)]
pub struct AvionicsNormalizationCandidate {
    pub id: i64,
    pub model: String,
    pub normalized_model: String,
    pub listing_count: i64,
    pub introduced_year: Option<i64>,
    pub estimated_unit_value_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct AvionicsNormalizationContext {
    pub manufacturer: String,
    pub avionics_type: String,
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

    pub async fn normalize_avionics_model_labels(
        &self,
        context: &AvionicsNormalizationContext,
    ) -> Result<Value> {
        self.generate_json(
            build_avionics_normalization_prompt(context),
            gemini_avionics_normalization_response_schema(),
            4096,
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

        let url = if google_search {
            &self.grounded_url
        } else {
            &self.url
        };
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
- variant is the exact advertised aircraft model designation from the listing. Include suffixes, generation labels, turbo/pressurized/retractable/amphibious/turbine modifiers, and marketing family words when present.\n\
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
- Treat punctuation, word order, capitalization, redundant marketing words, and equivalent shorthand such as an appended T versus the word TURBO as non-material when the listing context supports the same configuration.\n\
- Treat generation and configuration-changing terms such as normally aspirated versus turbo, pressurized, retractable, turbine, or amphibious as material unless both sides refer to the same configuration.\n\
- Treat trim or package names as non-material unless the evidence shows that term is the only material distinction between variants.\n\
- Return false when the candidate is only the same model family but not the same exact variant.\n\
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

fn build_variant_normalization_prompt(context: &VariantNormalizationContext) -> String {
    format!(
        "We need to clean up variant labels for existing aircraft sale listings that all belong to one manufacturer and model family.\n\
Group source variant labels that identify the same aircraft variant/configuration, and choose one canonical display label per group.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Every source variant from the input must appear exactly once across source_variants.\n\
- Do not invent source variant labels; source_variants must be copied exactly from the input variant values.\n\
- canonical_variant must be a non-empty string and must not be null.\n\
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

fn build_avionics_normalization_prompt(context: &AvionicsNormalizationContext) -> String {
    format!(
        "We need to clean up avionics labels extracted from aircraft sale listings.\n\
Group source avionics model rows that identify the same installed avionics unit, suite, or package, and choose one canonical display label per group.\n\
Return JSON with exactly this shape:\n{}\n\n\
Rules:\n\
- Every input id must appear exactly once across source_ids.\n\
- Do not invent source ids; source_ids must be copied from input models.\n\
- canonical_model must be a non-empty string and must not be null.\n\
- Group labels that differ only by capitalization, spacing, punctuation, hyphens, slash separators, plus signs, or redundant manufacturer words.\n\
- Group obvious shorthand for the same unit or suite, for example G1000 NXi and G1000NXi.\n\
- Keep labels separate when they refer to materially different avionics generations, models, or units, for example G1000, G1000 NXi, Perspective, Perspective+, GTX 33, and GTX 345R.\n\
- Keep a broad integrated suite separate from individual components unless the input evidence clearly shows both labels are duplicate names for the same parsed listing unit.\n\
- Prefer concise canonical labels with the manufacturer omitted and the avionics code/version preserved.\n\
- If unsure whether two labels identify the same hardware/suite, keep them separate.\n\
- Do not include markdown, comments, nulls, or extra keys.\n\n\
Input:\n{}",
        serde_json::to_string_pretty(&json!({
            "groups": [
                {
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

fn gemini_avionics_normalization_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "groups": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "canonical_model": {"type": "string"},
                        "source_ids": {
                            "type": "array",
                            "items": {"type": "integer"}
                        },
                        "rationale": {"type": "string"}
                    },
                    "required": ["canonical_model", "source_ids", "rationale"],
                    "propertyOrdering": ["canonical_model", "source_ids", "rationale"]
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
