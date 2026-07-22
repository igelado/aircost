//! Conservative visual transcription of aircraft registration and serial marks.
//!
//! This resolver is deliberately upstream of FAA admission and catalog
//! curation. It can report only characters explicitly visible in supplied
//! listing photos; it does not infer identity and it performs no database
//! writes.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::gemini::config::{
    GeminiRuntimeConfig, GeminiTask, ThinkingLevel as ConfigThinkingLevel,
};
use crate::gemini::interactions::{
    CreateInteractionRequest, GeminiInteractionsClient, GenerationConfig, GroundingRequirement,
    InteractionAccountingContext, InteractionInput, InteractionInputItem, ResponseFormat,
    ThinkingLevel,
};

pub const DEFAULT_GEMINI_VISUAL_IDENTIFIER_MODEL: &str = "gemini-3.6-flash";
pub const GEMINI_VISUAL_IDENTIFIER_MODEL_ENV: &str = "GEMINI_AIRCRAFT_VISUAL_MODEL";
pub const VISUAL_IDENTIFIER_PROMPT_VERSION: &str = "aircraft-visible-identifier-v1";
pub const VISUAL_IDENTIFIER_SCHEMA_VERSION: &str = "aircraft-visible-identifier-schema-v1";

const DEFAULT_MAX_PHOTOS: usize = 12;
const DEFAULT_MAX_SINGLE_IMAGE_BYTES: usize = 8 * 1024 * 1024;
// Base64 expands by roughly 4/3. This leaves several MiB below Gemini's 20 MB
// inline request limit for the prompt, schema, and JSON framing.
const DEFAULT_MAX_TOTAL_IMAGE_BYTES: usize = 12 * 1024 * 1024;
const MAX_VISUAL_OUTPUT_TOKENS: u64 = 4_096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListingPhotoInput {
    pub image_id: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

impl ListingPhotoInput {
    pub fn new(image_id: impl Into<String>, mime_type: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            image_id: image_id.into(),
            mime_type: mime_type.into(),
            bytes,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisualIdentifierConfig {
    pub model: String,
    pub service_tier: Option<String>,
    pub thinking_level: ConfigThinkingLevel,
    pub max_output_tokens: u64,
    pub max_photos: usize,
    pub max_single_image_bytes: usize,
    pub max_total_image_bytes: usize,
}

impl Default for VisualIdentifierConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_GEMINI_VISUAL_IDENTIFIER_MODEL.to_string(),
            service_tier: None,
            thinking_level: ConfigThinkingLevel::Low,
            max_output_tokens: MAX_VISUAL_OUTPUT_TOKENS,
            max_photos: DEFAULT_MAX_PHOTOS,
            max_single_image_bytes: DEFAULT_MAX_SINGLE_IMAGE_BYTES,
            max_total_image_bytes: DEFAULT_MAX_TOTAL_IMAGE_BYTES,
        }
    }
}

impl VisualIdentifierConfig {
    pub fn from_env() -> Result<Self> {
        let runtime = GeminiRuntimeConfig::from_environment()
            .context("could not load runtime Gemini visual routing")?;
        Self::from_runtime_config(&runtime)
    }

    pub fn from_runtime_config(runtime: &GeminiRuntimeConfig) -> Result<Self> {
        runtime
            .validate()
            .context("invalid runtime Gemini routing")?;
        let route = runtime.route(GeminiTask::AircraftVisualIdentity);
        let config = Self {
            model: route.model.clone(),
            service_tier: route.service_tier.clone(),
            thinking_level: route.thinking_level,
            max_output_tokens: route.max_output_tokens,
            ..Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let model = self.model.trim();
        if model.is_empty() {
            bail!("visual identifier model must not be blank");
        }
        if !model.starts_with("gemini-3") {
            bail!("visual identifier model must support Gemini 3 per-image resolution");
        }
        if model.ends_with("-latest") {
            bail!("visual identifier model must be pinned, not a -latest alias");
        }
        if model.to_ascii_lowercase().contains("-image") {
            bail!("visual identifier resolver cannot use an image-generation model");
        }
        if self
            .service_tier
            .as_deref()
            .is_some_and(|tier| !matches!(tier, "standard" | "flex" | "priority"))
        {
            bail!("visual identifier service tier must be standard, flex, priority, or omitted");
        }
        if self.max_output_tokens == 0 {
            bail!("visual identifier max_output_tokens must be positive");
        }
        if self.max_photos == 0 {
            bail!("visual identifier max_photos must be positive");
        }
        if self.max_single_image_bytes == 0 || self.max_total_image_bytes == 0 {
            bail!("visual identifier image byte limits must be positive");
        }
        if self.max_single_image_bytes > self.max_total_image_bytes {
            bail!("single-image limit cannot exceed total-image limit");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibleIdentifierKind {
    Registration,
    ManufacturerSerial,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualEvidenceConfidence {
    High,
    VeryHigh,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityBasis {
    ExteriorRegistrationMarking,
    RegistrationLabelOrPlate,
    ManufacturerSerialLabel,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualIdentifierStatus {
    CandidatesVisible,
    NoExplicitIdentifierVisible,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VisualIdentifierImageEvidence {
    pub image_id: String,
    pub visible_text: String,
    pub confidence: VisualEvidenceConfidence,
    /// `[ymin, xmin, ymax, xmax]`, normalized to the inclusive 0..=1000
    /// coordinate system documented by Gemini.
    pub box_2d: [u16; 4],
    pub visibility_basis: VisibilityBasis,
    pub location_description: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VisibleAircraftIdentifier {
    pub kind: VisibleIdentifierKind,
    /// Exact trimmed transcription selected from the visible evidence. This is
    /// not an FAA-normalized registration or a manufacturer-normalized serial.
    pub visible_text: String,
    pub evidence_count: usize,
    pub evidence: Vec<VisualIdentifierImageEvidence>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualConsensusStatus {
    AutoAccept,
    NeedsReview,
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualConsensusBasis {
    TwoIndependentRegistrationImages,
    RegistrationAndSerialInSameImage,
    SingleRegistrationImage,
    NoCompleteNNumber,
    ConflictingRegistrations,
    ConflictingSerials,
}

/// Conservative decision helper for downstream ingestion/repair code.
///
/// `AutoAccept` is only a visual-consensus decision. It is not FAA admission
/// and does not authorize a database write.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VisualRegistrationConsensus {
    pub status: VisualConsensusStatus,
    pub basis: VisualConsensusBasis,
    pub normalized_n_number: Option<String>,
    pub literal_registrations: Vec<String>,
    pub literal_serials: Vec<String>,
    pub registration_evidence_count: usize,
    pub serial_evidence_count: usize,
    pub supporting_image_ids: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VisualPhotoAudit {
    pub image_id: String,
    pub mime_type: String,
    pub byte_count: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VisualIdentifierResolution {
    pub status: VisualIdentifierStatus,
    pub candidates: Vec<VisibleAircraftIdentifier>,
    pub registration_consensus: VisualRegistrationConsensus,
    pub refusal_reason: Option<String>,
    pub photos: Vec<VisualPhotoAudit>,
    pub interaction_id: Option<String>,
    pub model: String,
    pub prompt_version: String,
    pub schema_version: String,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ModelVisualIdentifierOutput {
    status: VisualIdentifierStatus,
    observations: Vec<ModelVisualObservation>,
    refusal_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ModelVisualObservation {
    image_id: String,
    identifier_kind: VisibleIdentifierKind,
    visible_text: String,
    confidence: VisualEvidenceConfidence,
    box_2d: [u16; 4],
    visibility_basis: VisibilityBasis,
    complete_text_visible: bool,
    location_description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ValidatedVisualOutput {
    status: VisualIdentifierStatus,
    candidates: Vec<VisibleAircraftIdentifier>,
    refusal_reason: Option<String>,
}

/// Resolve explicitly visible registration or manufacturer-serial text from
/// listing photos. This function reads no database state and performs no
/// writes; ingestion and repair callers decide how to use the candidates.
pub async fn resolve_visible_aircraft_identifiers(
    client: &GeminiInteractionsClient,
    photos: &[ListingPhotoInput],
) -> Result<VisualIdentifierResolution> {
    let config = VisualIdentifierConfig::from_env()?;
    resolve_visible_aircraft_identifiers_with_config(client, photos, &config).await
}

pub async fn resolve_visible_aircraft_identifiers_with_runtime_config(
    client: &GeminiInteractionsClient,
    photos: &[ListingPhotoInput],
    runtime: &GeminiRuntimeConfig,
) -> Result<VisualIdentifierResolution> {
    let config = VisualIdentifierConfig::from_runtime_config(runtime)?;
    resolve_visible_aircraft_identifiers_with_config(client, photos, &config).await
}

pub async fn resolve_visible_aircraft_identifiers_with_config(
    client: &GeminiInteractionsClient,
    photos: &[ListingPhotoInput],
    config: &VisualIdentifierConfig,
) -> Result<VisualIdentifierResolution> {
    resolve_visible_aircraft_identifiers_with_options(client, photos, config, None).await
}

/// Variant for ingestion and benchmarks that can attach a listing/job
/// correlation to durable Gemini accounting.
pub async fn resolve_visible_aircraft_identifiers_with_accounting(
    client: &GeminiInteractionsClient,
    photos: &[ListingPhotoInput],
    config: &VisualIdentifierConfig,
    accounting: InteractionAccountingContext,
) -> Result<VisualIdentifierResolution> {
    if accounting.task != GeminiTask::AircraftVisualIdentity {
        bail!("visual identifier accounting must use the aircraft_visual_identity task");
    }
    resolve_visible_aircraft_identifiers_with_options(client, photos, config, Some(accounting))
        .await
}

async fn resolve_visible_aircraft_identifiers_with_options(
    client: &GeminiInteractionsClient,
    photos: &[ListingPhotoInput],
    config: &VisualIdentifierConfig,
    accounting: Option<InteractionAccountingContext>,
) -> Result<VisualIdentifierResolution> {
    config.validate()?;
    let prepared = prepare_photos(photos, config)?;
    let image_ids = prepared
        .iter()
        .map(|photo| photo.audit.image_id.clone())
        .collect::<Vec<_>>();
    let prompt = build_visual_identifier_prompt(&image_ids);
    let mut input = Vec::with_capacity(prepared.len().saturating_mul(2).saturating_add(1));
    input.push(InteractionInputItem::text(prompt)?);
    for photo in &prepared {
        input.push(InteractionInputItem::text(format!(
            "IMAGE_ID {}: inspect only the immediately following image.",
            photo.audit.image_id
        ))?);
        input.push(InteractionInputItem::inline_image(
            &photo.audit.mime_type,
            photo.bytes,
        )?);
    }

    let photo_set_id = visual_photo_set_id(&prepared);
    let accounting = accounting.unwrap_or_else(|| {
        InteractionAccountingContext::new(
            GeminiTask::AircraftVisualIdentity,
            "visible_aircraft_identifier_resolution",
        )
        .with_correlation_id(photo_set_id.clone())
        .with_source("listing_photo_set", photo_set_id)
    });
    let request = CreateInteractionRequest::new(
        config.model.trim(),
        InteractionInput::multimodal(input)?,
    )
    .with_system_instruction(
        "You are a conservative visual transcription system. Return aircraft identifiers only when every reported character is explicitly and unambiguously visible in the named image. Never infer, autocomplete, normalize, or transfer text between images.",
    )
    .with_response_format(ResponseFormat::json(visual_identifier_response_schema(
        &image_ids,
    ))?)
    .with_generation_config(GenerationConfig {
        max_output_tokens: Some(config.max_output_tokens),
        thinking_level: match config.thinking_level {
            ConfigThinkingLevel::Disabled => None,
            ConfigThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
            ConfigThinkingLevel::Low => Some(ThinkingLevel::Low),
            ConfigThinkingLevel::Medium => Some(ThinkingLevel::Medium),
            ConfigThinkingLevel::High => Some(ThinkingLevel::High),
        },
        ..GenerationConfig::default()
    })
    .with_accounting_context(accounting);
    let request = match config.service_tier.as_deref() {
        Some(service_tier) => request.with_service_tier(service_tier),
        None => request,
    };
    let response = client
        .create(&request)
        .await
        .context("Gemini visual aircraft-identifier request failed")?;
    let output = response
        .interaction
        .require_curation_output(GroundingRequirement::None)
        .context("Gemini visual aircraft-identifier response was incomplete")?;
    let validated = parse_visual_identifier_output(&output, &image_ids)?;
    let registration_consensus = evaluate_visual_registration_consensus(&validated.candidates);
    let usage = response.interaction.usage.as_ref();

    Ok(VisualIdentifierResolution {
        status: validated.status,
        candidates: validated.candidates,
        registration_consensus,
        refusal_reason: validated.refusal_reason,
        photos: prepared.into_iter().map(|photo| photo.audit).collect(),
        interaction_id: response.interaction.id,
        model: response
            .interaction
            .model
            .unwrap_or_else(|| config.model.trim().to_string()),
        prompt_version: VISUAL_IDENTIFIER_PROMPT_VERSION.to_string(),
        schema_version: VISUAL_IDENTIFIER_SCHEMA_VERSION.to_string(),
        total_input_tokens: usage.map(|usage| usage.total_input_tokens),
        total_output_tokens: usage.map(|usage| usage.total_output_tokens),
    })
}

pub fn visual_identifier_response_schema(image_ids: &[String]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "status": {
                "type": "string",
                "enum": ["candidates_visible", "no_explicit_identifier_visible"]
            },
            "observations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "image_id": {"type": "string", "enum": image_ids},
                        "identifier_kind": {
                            "type": "string",
                            "enum": ["registration", "manufacturer_serial"]
                        },
                        "visible_text": {"type": "string"},
                        "confidence": {"type": "string", "enum": ["high", "very_high"]},
                        "box_2d": {
                            "type": "array",
                            "items": {"type": "integer", "minimum": 0, "maximum": 1000},
                            "minItems": 4,
                            "maxItems": 4
                        },
                        "visibility_basis": {
                            "type": "string",
                            "enum": [
                                "exterior_registration_marking",
                                "registration_label_or_plate",
                                "manufacturer_serial_label"
                            ]
                        },
                        "complete_text_visible": {"type": "boolean"},
                        "location_description": {"type": "string"}
                    },
                    "required": [
                        "image_id",
                        "identifier_kind",
                        "visible_text",
                        "confidence",
                        "box_2d",
                        "visibility_basis",
                        "complete_text_visible",
                        "location_description"
                    ]
                }
            },
            "refusal_reason": {"type": ["string", "null"]}
        },
        "required": ["status", "observations", "refusal_reason"]
    })
}

/// Evaluate whether visual evidence is strong enough for a downstream caller
/// to auto-accept a U.S. registration candidate. Literal transcriptions remain
/// available in the result; normalization occurs only inside this post-parse
/// comparison step.
pub fn evaluate_visual_registration_consensus(
    candidates: &[VisibleAircraftIdentifier],
) -> VisualRegistrationConsensus {
    let registrations = candidates
        .iter()
        .filter(|candidate| candidate.kind == VisibleIdentifierKind::Registration)
        .collect::<Vec<_>>();
    let serials = candidates
        .iter()
        .filter(|candidate| candidate.kind == VisibleIdentifierKind::ManufacturerSerial)
        .collect::<Vec<_>>();
    let literal_registrations = registrations
        .iter()
        .map(|candidate| candidate.visible_text.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let literal_serials = serials
        .iter()
        .map(|candidate| candidate.visible_text.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let registration_evidence_count = registrations
        .iter()
        .map(|candidate| candidate.evidence.len())
        .sum();
    let serial_evidence_count = serials
        .iter()
        .map(|candidate| candidate.evidence.len())
        .sum();
    let registration_image_ids = registrations
        .iter()
        .flat_map(|candidate| candidate.evidence.iter())
        .map(|evidence| evidence.image_id.clone())
        .collect::<BTreeSet<_>>();
    let all_registration_images = registration_image_ids.iter().cloned().collect::<Vec<_>>();

    let registration_keys = registrations
        .iter()
        .map(|candidate| {
            normalize_visible_n_number(&candidate.visible_text).map_or_else(
                || format!("literal:{}", candidate.visible_text.to_ascii_uppercase()),
                |normalized| format!("n_number:{normalized}"),
            )
        })
        .collect::<BTreeSet<_>>();
    if registration_keys.len() > 1 {
        return VisualRegistrationConsensus {
            status: VisualConsensusStatus::Conflict,
            basis: VisualConsensusBasis::ConflictingRegistrations,
            normalized_n_number: None,
            literal_registrations,
            literal_serials,
            registration_evidence_count,
            serial_evidence_count,
            supporting_image_ids: all_registration_images,
            reason: "distinct visible registration candidates conflict".to_string(),
        };
    }

    let normalized_n_numbers = registrations
        .iter()
        .filter_map(|candidate| normalize_visible_n_number(&candidate.visible_text))
        .collect::<BTreeSet<_>>();
    let Some(normalized_n_number) = normalized_n_numbers.iter().next().cloned() else {
        return VisualRegistrationConsensus {
            status: VisualConsensusStatus::NeedsReview,
            basis: VisualConsensusBasis::NoCompleteNNumber,
            normalized_n_number: None,
            literal_registrations,
            literal_serials,
            registration_evidence_count,
            serial_evidence_count,
            supporting_image_ids: all_registration_images,
            reason: "no complete U.S. N-number is available for visual consensus".to_string(),
        };
    };

    // Serial values are deliberately not mechanically normalized or merged.
    // Different literal serials are a conflict even when registrations are
    // case/hyphen-equivalent.
    if literal_serials.len() > 1 {
        return VisualRegistrationConsensus {
            status: VisualConsensusStatus::Conflict,
            basis: VisualConsensusBasis::ConflictingSerials,
            normalized_n_number: Some(normalized_n_number),
            literal_registrations,
            literal_serials,
            registration_evidence_count,
            serial_evidence_count,
            supporting_image_ids: all_registration_images,
            reason: "distinct visible manufacturer serial candidates conflict".to_string(),
        };
    }

    if registration_image_ids.len() >= 2 {
        return VisualRegistrationConsensus {
            status: VisualConsensusStatus::AutoAccept,
            basis: VisualConsensusBasis::TwoIndependentRegistrationImages,
            normalized_n_number: Some(normalized_n_number),
            literal_registrations,
            literal_serials,
            registration_evidence_count,
            serial_evidence_count,
            supporting_image_ids: all_registration_images,
            reason: "the same complete N-number is independently visible in at least two images"
                .to_string(),
        };
    }

    let serial_image_ids = serials
        .iter()
        .flat_map(|candidate| candidate.evidence.iter())
        .map(|evidence| evidence.image_id.clone())
        .collect::<BTreeSet<_>>();
    let same_image_ids = registration_image_ids
        .intersection(&serial_image_ids)
        .cloned()
        .collect::<Vec<_>>();
    if !same_image_ids.is_empty() {
        return VisualRegistrationConsensus {
            status: VisualConsensusStatus::AutoAccept,
            basis: VisualConsensusBasis::RegistrationAndSerialInSameImage,
            normalized_n_number: Some(normalized_n_number),
            literal_registrations,
            literal_serials,
            registration_evidence_count,
            serial_evidence_count,
            supporting_image_ids: same_image_ids,
            reason: "one image independently shows both a complete N-number and a complete manufacturer serial"
                .to_string(),
        };
    }

    VisualRegistrationConsensus {
        status: VisualConsensusStatus::AutoAccept,
        basis: VisualConsensusBasis::SingleRegistrationImage,
        normalized_n_number: Some(normalized_n_number),
        literal_registrations,
        literal_serials,
        registration_evidence_count,
        serial_evidence_count,
        supporting_image_ids: all_registration_images,
        reason: "one complete high-confidence N-number is independently visible; downstream FAA admission is still required"
            .to_string(),
    }
}

/// Conservative comparison normalization applied only after literal visual
/// transcription. This is not an FAA registry lookup or admission decision.
pub fn normalize_visible_n_number(value: &str) -> Option<String> {
    let uppercase = value.trim().to_ascii_uppercase();
    let compact = if let Some(suffix) = uppercase.strip_prefix("N-") {
        format!("N{suffix}")
    } else {
        if uppercase.contains('-') {
            return None;
        }
        uppercase
    };
    let suffix = compact.strip_prefix('N')?;
    if suffix.is_empty() || suffix.len() > 5 {
        return None;
    }
    if !suffix
        .as_bytes()
        .first()
        .is_some_and(|byte| (b'1'..=b'9').contains(byte))
    {
        return None;
    }
    let mut letter_count = 0usize;
    let mut saw_letter = false;
    for character in suffix.chars() {
        if character.is_ascii_digit() {
            if saw_letter {
                return None;
            }
        } else if character.is_ascii_uppercase() && !matches!(character, 'I' | 'O') {
            saw_letter = true;
            letter_count += 1;
            if letter_count > 2 {
                return None;
            }
        } else {
            return None;
        }
    }
    Some(compact)
}

fn build_visual_identifier_prompt(image_ids: &[String]) -> String {
    format!(
        r#"Transcribe only complete, explicitly visible aircraft identifiers from the supplied listing photos.

Allowed outputs:
- `registration`: a complete civil registration visibly painted on the aircraft or printed on an explicit registration label/plate.
- `manufacturer_serial`: a complete manufacturer serial number visibly paired with an explicit SERIAL, SERIAL NO, or S/N label on an aircraft manufacturer data plate.

Never infer an identifier from aircraft make/model, livery, filename, listing context, another image, a partial string, common registration patterns, or model memory. Never autocomplete an obscured or blurry character. Do not return model numbers, part numbers, avionics labels, certificate numbers, phone numbers, prices, dates, or decorative text. Preserve the visible characters and punctuation exactly; do not mechanically normalize them.

Create a separate observation only for an image in which the entire candidate is independently visible. `box_2d` must be [ymin, xmin, ymax, xmax] normalized to 0..1000 and tightly surround the visible text. Set `complete_text_visible` to true only when no character is obscured or ambiguous. Only `high` or `very_high` observations are allowed. If there is no qualifying observation, return status `no_explicit_identifier_visible`, an empty observations array, and a concise refusal reason. Do not guess merely to avoid an empty result.

Allowed image IDs: {}"#,
        serde_json::to_string(image_ids).expect("image ids serialize for visual prompt")
    )
}

struct PreparedPhoto<'a> {
    audit: VisualPhotoAudit,
    bytes: &'a [u8],
}

fn visual_photo_set_id(photos: &[PreparedPhoto<'_>]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"aircost-visual-photo-set-v1\0");
    for photo in photos {
        digest.update(photo.audit.image_id.as_bytes());
        digest.update(b"\0");
        digest.update(photo.audit.sha256.as_bytes());
        digest.update(b"\0");
    }
    format!("sha256:{:x}", digest.finalize())
}

fn prepare_photos<'a>(
    photos: &'a [ListingPhotoInput],
    config: &VisualIdentifierConfig,
) -> Result<Vec<PreparedPhoto<'a>>> {
    if photos.is_empty() {
        bail!("visual identifier resolution requires at least one listing photo");
    }
    if photos.len() > config.max_photos {
        bail!(
            "visual identifier resolution accepts at most {} photos, received {}",
            config.max_photos,
            photos.len()
        );
    }

    let mut seen_ids = BTreeSet::new();
    let mut seen_sha256 = BTreeSet::new();
    let mut total_bytes = 0usize;
    let mut prepared = Vec::with_capacity(photos.len());
    for photo in photos {
        validate_image_id(&photo.image_id)?;
        if !seen_ids.insert(photo.image_id.as_str()) {
            bail!("duplicate listing photo image_id {:?}", photo.image_id);
        }
        if photo.bytes.is_empty() {
            bail!("listing photo {} has no bytes", photo.image_id);
        }
        if photo.bytes.len() > config.max_single_image_bytes {
            bail!(
                "listing photo {} exceeds the {} byte inline-image limit",
                photo.image_id,
                config.max_single_image_bytes
            );
        }
        total_bytes = total_bytes
            .checked_add(photo.bytes.len())
            .ok_or_else(|| anyhow!("listing photo byte count overflow"))?;
        if total_bytes > config.max_total_image_bytes {
            bail!(
                "listing photos exceed the {} byte total inline-image limit",
                config.max_total_image_bytes
            );
        }
        // Reuse the generic Interactions validator before allocating the full
        // request so unsupported MIME types fail locally.
        InteractionInputItem::inline_image(&photo.mime_type, &photo.bytes)?;

        let mut digest = Sha256::new();
        digest.update(&photo.bytes);
        let sha256 = format!("{:x}", digest.finalize());
        if !seen_sha256.insert(sha256.clone()) {
            bail!(
                "listing photo {} duplicates another supplied image byte-for-byte",
                photo.image_id
            );
        }
        prepared.push(PreparedPhoto {
            audit: VisualPhotoAudit {
                image_id: photo.image_id.clone(),
                mime_type: photo.mime_type.clone(),
                byte_count: photo.bytes.len(),
                sha256,
            },
            bytes: &photo.bytes,
        });
    }
    Ok(prepared)
}

fn validate_image_id(image_id: &str) -> Result<()> {
    if image_id.is_empty() || image_id.len() > 80 {
        bail!("listing photo image_id must contain 1 to 80 characters");
    }
    if !image_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!(
            "listing photo image_id {:?} contains unsupported characters",
            image_id
        );
    }
    Ok(())
}

fn parse_visual_identifier_output(
    output: &str,
    image_ids: &[String],
) -> Result<ValidatedVisualOutput> {
    let parsed = serde_json::from_str::<ModelVisualIdentifierOutput>(output)
        .context("Gemini visual identifier output did not match the response contract")?;
    let allowed_ids = image_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();

    match parsed.status {
        VisualIdentifierStatus::NoExplicitIdentifierVisible => {
            if !parsed.observations.is_empty() {
                bail!("no-explicit-identifier response must not contain observations");
            }
            let reason = parsed
                .refusal_reason
                .as_deref()
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .ok_or_else(|| {
                    anyhow!("no-explicit-identifier response requires a refusal reason")
                })?;
            if reason.len() > 500 {
                bail!("visual identifier refusal reason is too long");
            }
            return Ok(ValidatedVisualOutput {
                status: parsed.status,
                candidates: Vec::new(),
                refusal_reason: Some(reason.to_string()),
            });
        }
        VisualIdentifierStatus::CandidatesVisible => {
            if parsed.observations.is_empty() {
                bail!("candidates-visible response requires at least one observation");
            }
            if parsed.refusal_reason.is_some() {
                bail!("candidates-visible response must use a null refusal_reason");
            }
        }
    }

    let mut candidates =
        BTreeMap::<(VisibleIdentifierKind, String), VisibleAircraftIdentifier>::new();
    let mut evidence_keys = BTreeSet::new();
    for observation in parsed.observations {
        if !allowed_ids.contains(observation.image_id.as_str()) {
            bail!(
                "visual identifier observation references unknown image_id {:?}",
                observation.image_id
            );
        }
        if !observation.complete_text_visible {
            bail!(
                "visual identifier observation for {} is not complete and unambiguous",
                observation.image_id
            );
        }
        let visible_text = observation.visible_text.trim();
        validate_visible_identifier_text(observation.identifier_kind, visible_text)?;
        validate_basis(observation.identifier_kind, observation.visibility_basis)?;
        validate_box(observation.box_2d)?;
        let location = observation.location_description.trim();
        if location.is_empty() || location.len() > 300 {
            bail!("visual identifier location description must contain 1 to 300 characters");
        }

        let candidate_key = (observation.identifier_kind, visible_text.to_string());
        let evidence_key = (
            candidate_key.clone(),
            observation.image_id.clone(),
            observation.box_2d,
        );
        if !evidence_keys.insert(evidence_key) {
            continue;
        }
        let candidate =
            candidates
                .entry(candidate_key)
                .or_insert_with(|| VisibleAircraftIdentifier {
                    kind: observation.identifier_kind,
                    visible_text: visible_text.to_string(),
                    evidence_count: 0,
                    evidence: Vec::new(),
                });
        candidate.evidence.push(VisualIdentifierImageEvidence {
            image_id: observation.image_id,
            visible_text: visible_text.to_string(),
            confidence: observation.confidence,
            box_2d: observation.box_2d,
            visibility_basis: observation.visibility_basis,
            location_description: location.to_string(),
        });
    }

    let mut candidates = candidates.into_values().collect::<Vec<_>>();
    for candidate in &mut candidates {
        candidate.evidence.sort_by(|left, right| {
            left.image_id
                .cmp(&right.image_id)
                .then(left.box_2d.cmp(&right.box_2d))
        });
        candidate.evidence_count = candidate.evidence.len();
    }
    Ok(ValidatedVisualOutput {
        status: parsed.status,
        candidates,
        refusal_reason: None,
    })
}

fn validate_visible_identifier_text(kind: VisibleIdentifierKind, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("visible identifier text must not be blank");
    }
    if value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '?' | '*' | '…' | '\u{fffd}' | '[' | ']' | '(' | ')'
            )
    }) {
        bail!("visible identifier text contains ambiguity or placeholder characters");
    }
    match kind {
        VisibleIdentifierKind::Registration => {
            if value.len() > 16
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || !value.bytes().any(|byte| byte.is_ascii_alphanumeric())
            {
                bail!("visible registration has an unsupported explicit-text shape");
            }
        }
        VisibleIdentifierKind::ManufacturerSerial => {
            if value.len() > 64
                || !value.is_ascii()
                || !value.bytes().any(|byte| byte.is_ascii_alphanumeric())
            {
                bail!("visible manufacturer serial has an unsupported explicit-text shape");
            }
        }
    }
    Ok(())
}

fn validate_basis(kind: VisibleIdentifierKind, basis: VisibilityBasis) -> Result<()> {
    let compatible = match kind {
        VisibleIdentifierKind::Registration => matches!(
            basis,
            VisibilityBasis::ExteriorRegistrationMarking
                | VisibilityBasis::RegistrationLabelOrPlate
        ),
        VisibleIdentifierKind::ManufacturerSerial => {
            basis == VisibilityBasis::ManufacturerSerialLabel
        }
    };
    if !compatible {
        bail!("visual identifier kind is incompatible with its visibility basis");
    }
    Ok(())
}

fn validate_box(box_2d: [u16; 4]) -> Result<()> {
    let [y_min, x_min, y_max, x_max] = box_2d;
    if box_2d.into_iter().any(|coordinate| coordinate > 1000) || y_min >= y_max || x_min >= x_max {
        bail!("visual identifier box_2d must be a non-empty normalized 0..1000 rectangle");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_ids() -> Vec<String> {
        vec!["photo-1".to_string(), "photo-2".to_string()]
    }

    fn visible_observation(image_id: &str, text: &str) -> Value {
        json!({
            "image_id": image_id,
            "identifier_kind": "registration",
            "visible_text": text,
            "confidence": "very_high",
            "box_2d": [100, 200, 220, 650],
            "visibility_basis": "exterior_registration_marking",
            "complete_text_visible": true,
            "location_description": "Painted on the aft fuselage"
        })
    }

    fn candidate(
        kind: VisibleIdentifierKind,
        text: &str,
        image_ids: &[&str],
    ) -> VisibleAircraftIdentifier {
        let basis = match kind {
            VisibleIdentifierKind::Registration => VisibilityBasis::ExteriorRegistrationMarking,
            VisibleIdentifierKind::ManufacturerSerial => VisibilityBasis::ManufacturerSerialLabel,
        };
        let evidence = image_ids
            .iter()
            .map(|image_id| VisualIdentifierImageEvidence {
                image_id: (*image_id).to_string(),
                visible_text: text.to_string(),
                confidence: VisualEvidenceConfidence::VeryHigh,
                box_2d: [100, 200, 220, 650],
                visibility_basis: basis,
                location_description: "Explicitly labeled marking".to_string(),
            })
            .collect::<Vec<_>>();
        VisibleAircraftIdentifier {
            kind,
            visible_text: text.to_string(),
            evidence_count: evidence.len(),
            evidence,
        }
    }

    #[test]
    fn schema_constrains_images_identifier_kinds_and_confidence() {
        let schema = visual_identifier_response_schema(&image_ids());
        let observation = &schema["properties"]["observations"]["items"];
        assert_eq!(observation["additionalProperties"], false);
        assert_eq!(
            observation["properties"]["image_id"]["enum"],
            json!(["photo-1", "photo-2"])
        );
        assert_eq!(
            observation["properties"]["identifier_kind"]["enum"],
            json!(["registration", "manufacturer_serial"])
        );
        assert_eq!(
            observation["properties"]["confidence"]["enum"],
            json!(["high", "very_high"])
        );
        assert_eq!(observation["properties"]["box_2d"]["maxItems"], 4);
        let bases = observation["properties"]["visibility_basis"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(
            bases
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
                .len(),
            bases.len(),
            "visibility basis enum must not contain duplicates"
        );
    }

    #[test]
    fn parses_complete_visible_registration() {
        let output = json!({
            "status": "candidates_visible",
            "observations": [visible_observation("photo-1", "N123AB")],
            "refusal_reason": null
        });
        let parsed = parse_visual_identifier_output(&output.to_string(), &image_ids()).unwrap();
        assert_eq!(parsed.status, VisualIdentifierStatus::CandidatesVisible);
        assert_eq!(parsed.candidates.len(), 1);
        assert_eq!(parsed.candidates[0].visible_text, "N123AB");
        assert_eq!(parsed.candidates[0].evidence[0].image_id, "photo-1");
    }

    #[test]
    fn accepts_refusal_and_rejects_inferred_or_partial_text() {
        let refusal = json!({
            "status": "no_explicit_identifier_visible",
            "observations": [],
            "refusal_reason": "The relevant markings are blurred or cropped."
        });
        let parsed = parse_visual_identifier_output(&refusal.to_string(), &image_ids()).unwrap();
        assert!(parsed.candidates.is_empty());
        assert!(parsed.refusal_reason.is_some());

        let partial = json!({
            "status": "candidates_visible",
            "observations": [visible_observation("photo-1", "N12?AB")],
            "refusal_reason": null
        });
        assert!(parse_visual_identifier_output(&partial.to_string(), &image_ids()).is_err());

        let mut uncertain_observation = visible_observation("photo-1", "N123AB");
        uncertain_observation["complete_text_visible"] = json!(false);
        let admitted_uncertainty = json!({
            "status": "candidates_visible",
            "observations": [uncertain_observation],
            "refusal_reason": null
        });
        assert!(
            parse_visual_identifier_output(&admitted_uncertainty.to_string(), &image_ids())
                .is_err()
        );
    }

    #[test]
    fn deduplicates_candidate_and_preserves_per_image_evidence() {
        let output = json!({
            "status": "candidates_visible",
            "observations": [
                visible_observation("photo-2", "N123AB"),
                visible_observation("photo-1", "N123AB"),
                visible_observation("photo-1", "N123AB")
            ],
            "refusal_reason": null
        });
        let parsed = parse_visual_identifier_output(&output.to_string(), &image_ids()).unwrap();
        assert_eq!(parsed.candidates.len(), 1);
        assert_eq!(parsed.candidates[0].evidence.len(), 2);
        assert_eq!(parsed.candidates[0].evidence_count, 2);
        assert_eq!(parsed.candidates[0].evidence[0].image_id, "photo-1");
        assert_eq!(parsed.candidates[0].evidence[1].image_id, "photo-2");
    }

    #[test]
    fn consensus_accepts_two_images_after_case_hyphen_comparison() {
        let candidates = vec![
            candidate(VisibleIdentifierKind::Registration, "N-123ab", &["photo-1"]),
            candidate(VisibleIdentifierKind::Registration, "N123AB", &["photo-2"]),
        ];
        let consensus = evaluate_visual_registration_consensus(&candidates);
        assert_eq!(consensus.status, VisualConsensusStatus::AutoAccept);
        assert_eq!(
            consensus.basis,
            VisualConsensusBasis::TwoIndependentRegistrationImages
        );
        assert_eq!(consensus.normalized_n_number.as_deref(), Some("N123AB"));
        assert_eq!(consensus.registration_evidence_count, 2);
    }

    #[test]
    fn consensus_accepts_registration_and_serial_in_one_image() {
        let candidates = vec![
            candidate(VisibleIdentifierKind::Registration, "N123AB", &["photo-1"]),
            candidate(
                VisibleIdentifierKind::ManufacturerSerial,
                "18281234",
                &["photo-1"],
            ),
        ];
        let consensus = evaluate_visual_registration_consensus(&candidates);
        assert_eq!(consensus.status, VisualConsensusStatus::AutoAccept);
        assert_eq!(
            consensus.basis,
            VisualConsensusBasis::RegistrationAndSerialInSameImage
        );
        assert_eq!(consensus.supporting_image_ids, vec!["photo-1"]);
    }

    #[test]
    fn consensus_accepts_single_complete_image_and_rejects_distinct_serials() {
        let registration = candidate(VisibleIdentifierKind::Registration, "N123AB", &["photo-1"]);
        let accepted = evaluate_visual_registration_consensus(std::slice::from_ref(&registration));
        assert_eq!(accepted.status, VisualConsensusStatus::AutoAccept);
        assert_eq!(
            accepted.basis,
            VisualConsensusBasis::SingleRegistrationImage
        );

        let candidates = vec![
            registration,
            candidate(
                VisibleIdentifierKind::ManufacturerSerial,
                "18281234",
                &["photo-1"],
            ),
            candidate(
                VisibleIdentifierKind::ManufacturerSerial,
                "18281235",
                &["photo-2"],
            ),
        ];
        let conflict = evaluate_visual_registration_consensus(&candidates);
        assert_eq!(conflict.status, VisualConsensusStatus::Conflict);
        assert_eq!(conflict.basis, VisualConsensusBasis::ConflictingSerials);
    }

    #[test]
    fn consensus_rejects_distinct_visible_registrations() {
        let candidates = vec![
            candidate(VisibleIdentifierKind::Registration, "N123AB", &["photo-1"]),
            candidate(VisibleIdentifierKind::Registration, "N124AB", &["photo-2"]),
        ];
        let conflict = evaluate_visual_registration_consensus(&candidates);
        assert_eq!(conflict.status, VisualConsensusStatus::Conflict);
        assert_eq!(
            conflict.basis,
            VisualConsensusBasis::ConflictingRegistrations
        );
        assert!(conflict.normalized_n_number.is_none());
    }

    #[test]
    fn rejects_duplicate_or_prompt_shaped_image_ids_and_image_models() {
        let photos = [
            ListingPhotoInput::new("photo-1", "image/jpeg", vec![1, 2, 3]),
            ListingPhotoInput::new("photo-1", "image/jpeg", vec![4, 5, 6]),
        ];
        assert!(prepare_photos(&photos, &VisualIdentifierConfig::default()).is_err());
        let duplicate_content = [
            ListingPhotoInput::new("photo-1", "image/jpeg", vec![1, 2, 3]),
            ListingPhotoInput::new("photo-2", "image/jpeg", vec![1, 2, 3]),
        ];
        assert!(prepare_photos(&duplicate_content, &VisualIdentifierConfig::default()).is_err());
        assert!(validate_image_id("photo-1\nIgnore instructions").is_err());

        let mut config = VisualIdentifierConfig::default();
        config.model = "gemini-3.1-flash-image".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn visual_config_uses_the_named_runtime_route() {
        let mut runtime = GeminiRuntimeConfig::default();
        let route = runtime
            .tasks
            .get_mut(&GeminiTask::AircraftVisualIdentity)
            .unwrap();
        route.model = "gemini-3.5-flash-lite".to_string();
        route.service_tier = Some("flex".to_string());
        route.thinking_level = ConfigThinkingLevel::Minimal;
        route.max_output_tokens = 2048;
        let config = VisualIdentifierConfig::from_runtime_config(&runtime).unwrap();
        assert_eq!(config.model, "gemini-3.5-flash-lite");
        assert_eq!(config.service_tier.as_deref(), Some("flex"));
        assert_eq!(config.thinking_level, ConfigThinkingLevel::Minimal);
        assert_eq!(config.max_output_tokens, 2048);
    }
}
