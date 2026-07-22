//! Reproducible, read-only Gemini model comparison inputs and reports.
//!
//! This module deliberately has no Gemini client and no database write path.
//! It builds cases from retained production-shaped submissions, then evaluates
//! outputs supplied by a caller-owned runner (or deserialized from an offline
//! result file). Keeping execution behind [`BenchmarkRunner`] makes a paid run
//! an explicit admin-layer decision rather than a side effect of loading a
//! suite.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::html::clean::clean_listing_html;
use crate::html::listing::media::{discover, MediaReference};
use crate::models::{is_plausible_asking_price_usd, ParsedAvionics, ParsedListing};

pub const BENCHMARK_SCHEMA_VERSION: &str = "gemini-model-benchmark-v1";
pub const LISTING_EXTRACTION_PROMPT_VERSION: &str = "listing-extraction-benchmark-v1";
pub const GROUNDED_METADATA_PROMPT_VERSION: &str = "grounded-metadata-benchmark-v1";
pub const AVIONICS_REVIEW_PROMPT_VERSION: &str = "avionics-grounding-review-benchmark-v1";
pub const VISUAL_IDENTITY_PROMPT_VERSION: &str = "visual-identity-benchmark-v1";
pub const DEFAULT_PRICING_EFFECTIVE_DATE: &str = "2026-07-09";
pub const GEMINI_PRICING_SOURCE_URL: &str = "https://ai.google.dev/gemini-api/docs/pricing";

const MAX_SEED_BYTES: usize = 256;
const MAX_LISTINGS: usize = 100;
const MAX_AVIONICS_PER_LISTING: usize = 20;
const MAX_VISUAL_ASSETS: usize = 12;
const BENCHMARK_VALUE_REFERENCE_YEAR: i64 = 2026;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkSelection {
    pub seed: String,
    pub listing_limit: usize,
    /// Canonical listing IDs to select exactly instead of sampling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listing_ids: Vec<i64>,
    /// Retained plugin submission IDs to select exactly instead of sampling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submission_ids: Vec<i64>,
    pub max_avionics_per_listing: usize,
    pub max_visual_assets: usize,
}

impl Default for BenchmarkSelection {
    fn default() -> Self {
        Self {
            seed: "aircost-gemini-benchmark-v1".to_string(),
            listing_limit: 12,
            listing_ids: Vec::new(),
            submission_ids: Vec::new(),
            max_avionics_per_listing: 4,
            max_visual_assets: 8,
        }
    }
}

impl BenchmarkSelection {
    pub fn validate(&self) -> Result<()> {
        if self.seed.trim().is_empty() {
            bail!("benchmark seed must not be blank");
        }
        if self.seed.len() > MAX_SEED_BYTES {
            bail!("benchmark seed exceeds {MAX_SEED_BYTES} bytes");
        }
        if !self.listing_ids.is_empty() && !self.submission_ids.is_empty() {
            bail!("benchmark selection cannot combine listing IDs and submission IDs");
        }
        if self.listing_ids.is_empty()
            && self.submission_ids.is_empty()
            && !(1..=MAX_LISTINGS).contains(&self.listing_limit)
        {
            bail!("listing_limit must be between 1 and {MAX_LISTINGS}");
        }
        validate_explicit_ids("listing", &self.listing_ids)?;
        validate_explicit_ids("submission", &self.submission_ids)?;
        if !(1..=MAX_AVIONICS_PER_LISTING).contains(&self.max_avionics_per_listing) {
            bail!("max_avionics_per_listing must be between 1 and {MAX_AVIONICS_PER_LISTING}");
        }
        if !(1..=MAX_VISUAL_ASSETS).contains(&self.max_visual_assets) {
            bail!("max_visual_assets must be between 1 and {MAX_VISUAL_ASSETS}");
        }
        Ok(())
    }
}

fn validate_explicit_ids(kind: &str, ids: &[i64]) -> Result<()> {
    let mut unique = BTreeSet::new();
    for id in ids {
        if *id < 1 {
            bail!("benchmark {kind} IDs must be positive");
        }
        if !unique.insert(*id) {
            bail!("benchmark {kind} ID {id} is duplicated");
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkTaskKind {
    ListingExtraction,
    GroundedMetadata,
    AvionicsGroundingReview,
    VisualIdentity,
}

impl BenchmarkTaskKind {
    fn label(self) -> &'static str {
        match self {
            Self::ListingExtraction => "listing_extraction",
            Self::GroundedMetadata => "grounded_metadata",
            Self::AvionicsGroundingReview => "avionics_grounding_review",
            Self::VisualIdentity => "visual_identity",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkSuite {
    pub schema_version: String,
    pub seed: String,
    pub available_submission_count: usize,
    pub selected_submission_ids: Vec<i64>,
    pub cases: Vec<BenchmarkCase>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkCase {
    pub id: String,
    pub task: BenchmarkTaskKind,
    pub prompt_version: String,
    pub submission_id: i64,
    pub listing_id: Option<i64>,
    pub source_url: String,
    pub input_sha256: String,
    pub input: BenchmarkInput,
    /// Historical extraction/audit output is useful for regression comparison,
    /// but it was model-produced and must never be treated as labeled truth.
    pub reference_output: Option<Value>,
    pub reference_is_ground_truth: bool,
    pub prior_extraction_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BenchmarkInput {
    ListingExtraction {
        listing_text: String,
        retained_html_sha256: String,
    },
    GroundedMetadata {
        candidate: ParsedAvionics,
        value_reference_year: i64,
    },
    AvionicsGroundingReview {
        aircraft: BenchmarkAircraftContext,
        candidate: ParsedAvionics,
        listing_evidence: String,
        catalog_candidates: Vec<BenchmarkCatalogCandidate>,
    },
    VisualIdentity {
        assets: Vec<BenchmarkVisualAsset>,
        prior_audit: Option<Value>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkAircraftContext {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub variant: Option<String>,
    pub model_year: Option<i64>,
    pub registration_number: Option<String>,
    pub serial_number: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkCatalogCandidate {
    pub id: i64,
    pub manufacturer: String,
    pub model: String,
    pub catalog_status: String,
    pub manufacturer_identifier_kind: Option<String>,
    pub manufacturer_identifier: Option<String>,
    pub identity_source_url: Option<String>,
    pub identity_confidence: Option<String>,
    pub avionics_types: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkVisualAsset {
    pub image_id: String,
    pub media_url: String,
    pub media_host: String,
    pub media_kind: String,
    pub media_location: String,
    pub mime_type: String,
    pub maximum_bytes: usize,
    pub is_original: bool,
}

#[derive(Clone, Debug, FromRow)]
struct SourceRow {
    submission_id: i64,
    listing_id: Option<i64>,
    source_url: String,
    rendered_html: String,
    rendered_html_sha256: String,
    extracted_listing_json: Option<String>,
    extraction_error: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct CatalogRow {
    id: i64,
    manufacturer: String,
    model: String,
    catalog_status: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    identity_source_url: Option<String>,
    identity_confidence: Option<String>,
    avionics_type: Option<String>,
}

/// Load a deterministic benchmark suite using SELECT statements only.
pub async fn load_suite(db: &AppDb, selection: &BenchmarkSelection) -> Result<BenchmarkSuite> {
    selection.validate()?;
    let rows = load_source_rows(db).await?;
    let catalog = load_catalog_candidates(db).await?;
    build_suite(rows, catalog, selection)
}

async fn load_source_rows(db: &AppDb) -> Result<Vec<SourceRow>> {
    let sql = r#"
        SELECT
          id AS submission_id,
          canonical_listing_id AS listing_id,
          source_url,
          rendered_html,
          rendered_html_sha256,
          extracted_listing_json,
          extraction_error
        FROM plugin_submissions
        WHERE canonical_listing_id IS NOT NULL
          AND length(trim(rendered_html)) > 0
        ORDER BY id
    "#;
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_as::<_, SourceRow>(sql)
            .fetch_all(pool)
            .await
            .context("could not load SQLite benchmark submissions"),
        DatabaseBackend::Postgres(pool) => sqlx::query_as::<_, SourceRow>(sql)
            .fetch_all(pool)
            .await
            .context("could not load Postgres benchmark submissions"),
    }
}

async fn load_catalog_candidates(db: &AppDb) -> Result<Vec<BenchmarkCatalogCandidate>> {
    let sql = r#"
        SELECT
          model.id,
          manufacturer.name AS manufacturer,
          model.name AS model,
          model.catalog_status,
          model.manufacturer_identifier_kind,
          model.manufacturer_identifier,
          model.identity_source_url,
          model.identity_confidence,
          avionics_type.name AS avionics_type
        FROM avionics_models model
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        LEFT JOIN avionics_model_types membership
          ON membership.avionics_model_id = model.id
        LEFT JOIN avionics_types avionics_type
          ON avionics_type.id = membership.avionics_type_id
        WHERE model.catalog_status <> 'rejected'
        ORDER BY model.id, avionics_type.name
    "#;
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_as::<_, CatalogRow>(sql)
            .fetch_all(pool)
            .await
            .context("could not load SQLite avionics catalog candidates")?,
        DatabaseBackend::Postgres(pool) => sqlx::query_as::<_, CatalogRow>(sql)
            .fetch_all(pool)
            .await
            .context("could not load Postgres avionics catalog candidates")?,
    };
    Ok(group_catalog_rows(rows))
}

fn group_catalog_rows(rows: Vec<CatalogRow>) -> Vec<BenchmarkCatalogCandidate> {
    let mut grouped = BTreeMap::<i64, BenchmarkCatalogCandidate>::new();
    for row in rows {
        let candidate = grouped
            .entry(row.id)
            .or_insert_with(|| BenchmarkCatalogCandidate {
                id: row.id,
                manufacturer: row.manufacturer,
                model: row.model,
                catalog_status: row.catalog_status,
                manufacturer_identifier_kind: row.manufacturer_identifier_kind,
                manufacturer_identifier: row.manufacturer_identifier,
                identity_source_url: row.identity_source_url,
                identity_confidence: row.identity_confidence,
                avionics_types: Vec::new(),
            });
        if let Some(avionics_type) = row.avionics_type {
            if !candidate.avionics_types.contains(&avionics_type) {
                candidate.avionics_types.push(avionics_type);
            }
        }
    }
    grouped.into_values().collect()
}

fn build_suite(
    rows: Vec<SourceRow>,
    catalog: Vec<BenchmarkCatalogCandidate>,
    selection: &BenchmarkSelection,
) -> Result<BenchmarkSuite> {
    selection.validate()?;
    let available_submission_count = rows.len();
    let selected = if !selection.submission_ids.is_empty() {
        select_submission_ids(rows, &selection.submission_ids)?
    } else if !selection.listing_ids.is_empty() {
        select_listing_ids(rows, &selection.listing_ids)?
    } else {
        deterministic_sample(rows, &selection.seed, selection.listing_limit)
    };
    let selected_submission_ids = selected.iter().map(|row| row.submission_id).collect();
    let mut cases = Vec::new();

    for row in &selected {
        let listing_text = clean_listing_html(&row.rendered_html);
        let reference_output = row
            .extracted_listing_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok());

        cases.push(make_case(
            BenchmarkTaskKind::ListingExtraction,
            LISTING_EXTRACTION_PROMPT_VERSION,
            row,
            BenchmarkInput::ListingExtraction {
                listing_text: listing_text.clone(),
                retained_html_sha256: row.rendered_html_sha256.clone(),
            },
            reference_output.clone(),
        )?);

        if let Some(reference) = reference_output.as_ref() {
            let parsed = crate::extract::parsed_listing_from_model_output(reference);
            let aircraft = aircraft_context(&parsed);
            let literal_observations = literal_legacy_avionics(reference);
            let observed_avionics = if literal_observations.is_empty() {
                parsed.avionics.clone()
            } else {
                literal_observations
            };
            let avionics = deterministic_avionics_sample(
                &observed_avionics,
                &selection.seed,
                row.submission_id,
                selection.max_avionics_per_listing,
            );
            if let Some(candidate) = avionics.first() {
                cases.push(make_case(
                    BenchmarkTaskKind::GroundedMetadata,
                    GROUNDED_METADATA_PROMPT_VERSION,
                    row,
                    BenchmarkInput::GroundedMetadata {
                        candidate: candidate.clone(),
                        value_reference_year: BENCHMARK_VALUE_REFERENCE_YEAR,
                    },
                    None,
                )?);
            }
            for candidate in avionics {
                let catalog_candidates = closest_catalog_candidates(&candidate, &catalog, 8);
                cases.push(make_case(
                    BenchmarkTaskKind::AvionicsGroundingReview,
                    AVIONICS_REVIEW_PROMPT_VERSION,
                    row,
                    BenchmarkInput::AvionicsGroundingReview {
                        aircraft: aircraft.clone(),
                        listing_evidence: candidate
                            .source_evidence_text
                            .clone()
                            .unwrap_or_else(|| listing_text.clone()),
                        candidate: candidate.clone(),
                        catalog_candidates,
                    },
                    Some(json!(candidate)),
                )?);
            }
        }

        let prior_audit = reference_output
            .as_ref()
            .and_then(|value| value.get("visual_identity_recovery"))
            .cloned();
        let assets = discover(&row.source_url, &row.rendered_html)
            .map(|discovery| {
                select_visual_assets(
                    &discovery.aircraft_photos,
                    &discovery.logbook_attachments,
                    selection.max_visual_assets,
                )
            })
            .unwrap_or_default();
        if !assets.is_empty() || prior_audit.is_some() {
            cases.push(make_case(
                BenchmarkTaskKind::VisualIdentity,
                VISUAL_IDENTITY_PROMPT_VERSION,
                row,
                BenchmarkInput::VisualIdentity {
                    assets,
                    prior_audit: prior_audit.clone(),
                },
                prior_audit,
            )?);
        }
    }

    Ok(BenchmarkSuite {
        schema_version: BENCHMARK_SCHEMA_VERSION.to_string(),
        seed: selection.seed.clone(),
        available_submission_count,
        selected_submission_ids,
        cases,
    })
}

fn select_submission_ids(
    mut rows: Vec<SourceRow>,
    submission_ids: &[i64],
) -> Result<Vec<SourceRow>> {
    let requested = submission_ids.iter().copied().collect::<BTreeSet<_>>();
    rows.retain(|row| requested.contains(&row.submission_id));
    let selected = rows
        .iter()
        .map(|row| row.submission_id)
        .collect::<BTreeSet<_>>();
    for submission_id in submission_ids {
        if !selected.contains(submission_id) {
            bail!("--submission-id {submission_id} does not identify a retained plugin submission");
        }
    }
    rows.sort_by_key(|row| row.submission_id);
    Ok(rows)
}

fn select_listing_ids(mut rows: Vec<SourceRow>, listing_ids: &[i64]) -> Result<Vec<SourceRow>> {
    let requested = listing_ids.iter().copied().collect::<BTreeSet<_>>();
    rows.retain(|row| row.listing_id.is_some_and(|id| requested.contains(&id)));
    let selected = rows
        .iter()
        .filter_map(|row| row.listing_id)
        .collect::<BTreeSet<_>>();
    for listing_id in listing_ids {
        if !selected.contains(listing_id) {
            bail!(
                "benchmark listing ID {listing_id} does not identify a retained plugin submission"
            );
        }
    }
    rows.sort_by_key(|row| row.submission_id);
    Ok(rows)
}

/// Read-only compatibility for benchmark inputs produced before the join-only
/// multi-capability schema. Values remain literal listing observations and are
/// never normalized or written back. This lets the comparison exercise the
/// real retained corpus instead of silently dropping its avionics cases.
fn literal_legacy_avionics(reference: &Value) -> Vec<ParsedAvionics> {
    reference
        .get("avionics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            if let Ok(current) = serde_json::from_value::<ParsedAvionics>(value.clone()) {
                if !current.avionics_types.is_empty() {
                    return Some(current);
                }
            }
            let manufacturer = value.get("manufacturer")?.as_str()?.trim().to_string();
            let model = value.get("model")?.as_str()?.trim().to_string();
            if manufacturer.is_empty() || model.is_empty() {
                return None;
            }
            let avionics_types = value
                .get("types")
                .and_then(Value::as_array)
                .map(|types| {
                    types
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
                .filter(|types| !types.is_empty())
                .or_else(|| {
                    value
                        .get("type")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|value| vec![value.to_string()])
                })?;
            Some(ParsedAvionics {
                manufacturer,
                model,
                avionics_types,
                quantity: value.get("quantity").and_then(Value::as_i64).unwrap_or(1),
                configuration_action: value
                    .get("configuration_action")
                    .and_then(Value::as_str)
                    .unwrap_or("installed")
                    .to_string(),
                replaces: None,
                source_evidence_text: value
                    .get("source_evidence_text")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                source_confidence: value
                    .get("source_confidence")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            })
        })
        .collect()
}

fn deterministic_sample(mut rows: Vec<SourceRow>, seed: &str, limit: usize) -> Vec<SourceRow> {
    rows.sort_by(|left, right| {
        sample_digest(seed, left)
            .cmp(&sample_digest(seed, right))
            .then_with(|| left.submission_id.cmp(&right.submission_id))
    });
    rows.truncate(limit.min(rows.len()));
    rows
}

fn sample_digest(seed: &str, row: &SourceRow) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(BENCHMARK_SCHEMA_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(seed.as_bytes());
    hasher.update([0]);
    hasher.update(row.submission_id.to_le_bytes());
    hasher.update([0]);
    hasher.update(row.rendered_html_sha256.as_bytes());
    hasher.finalize().into()
}

fn make_case(
    task: BenchmarkTaskKind,
    prompt_version: &str,
    row: &SourceRow,
    input: BenchmarkInput,
    reference_output: Option<Value>,
) -> Result<BenchmarkCase> {
    let input_json = serde_json::to_vec(&input).context("could not serialize benchmark input")?;
    let input_sha256 = sha256_hex(&input_json);
    let id_material = format!(
        "{BENCHMARK_SCHEMA_VERSION}\0{}\0{}\0{input_sha256}",
        task.label(),
        row.submission_id
    );
    let id = format!(
        "{}-{}",
        task.label(),
        &sha256_hex(id_material.as_bytes())[..20]
    );
    Ok(BenchmarkCase {
        id,
        task,
        prompt_version: prompt_version.to_string(),
        submission_id: row.submission_id,
        listing_id: row.listing_id,
        source_url: row.source_url.clone(),
        input_sha256,
        input,
        reference_output,
        reference_is_ground_truth: false,
        prior_extraction_error: row.extraction_error.clone(),
    })
}

fn aircraft_context(parsed: &ParsedListing) -> BenchmarkAircraftContext {
    BenchmarkAircraftContext {
        manufacturer: parsed.manufacturer.clone(),
        model: parsed.model.clone(),
        variant: parsed.variant.clone(),
        model_year: parsed.model_year,
        registration_number: parsed.registration_number.clone(),
        serial_number: parsed.serial_number.clone(),
    }
}

fn deterministic_avionics_sample(
    avionics: &[ParsedAvionics],
    seed: &str,
    submission_id: i64,
    limit: usize,
) -> Vec<ParsedAvionics> {
    let mut unique = BTreeMap::<String, ParsedAvionics>::new();
    for candidate in avionics {
        let key = avionics_identity(candidate);
        unique.entry(key).or_insert_with(|| candidate.clone());
    }
    let mut scored = unique.into_iter().collect::<Vec<_>>();
    scored.sort_by(|(left_key, _), (right_key, _)| {
        stable_digest(&[seed, &submission_id.to_string(), left_key])
            .cmp(&stable_digest(&[
                seed,
                &submission_id.to_string(),
                right_key,
            ]))
            .then_with(|| left_key.cmp(right_key))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, candidate)| candidate)
        .collect()
}

fn avionics_identity(candidate: &ParsedAvionics) -> String {
    format!(
        "{}|{}|{}|{}",
        comparable_text(&candidate.manufacturer),
        comparable_text(&candidate.model),
        candidate
            .avionics_types
            .iter()
            .map(|value| comparable_text(value))
            .collect::<Vec<_>>()
            .join(","),
        comparable_text(&candidate.configuration_action)
    )
}

fn closest_catalog_candidates(
    candidate: &ParsedAvionics,
    catalog: &[BenchmarkCatalogCandidate],
    limit: usize,
) -> Vec<BenchmarkCatalogCandidate> {
    let query = format!("{} {}", candidate.manufacturer, candidate.model);
    let query_tokens = comparable_tokens(&query);
    let mut scored = catalog
        .iter()
        .map(|catalog_candidate| {
            let candidate_text = format!(
                "{} {} {}",
                catalog_candidate.manufacturer,
                catalog_candidate.model,
                catalog_candidate
                    .manufacturer_identifier
                    .as_deref()
                    .unwrap_or_default()
            );
            let candidate_tokens = comparable_tokens(&candidate_text);
            let overlap = query_tokens.intersection(&candidate_tokens).count();
            let exact_manufacturer = comparable_text(&candidate.manufacturer)
                == comparable_text(&catalog_candidate.manufacturer);
            let exact_model =
                comparable_text(&candidate.model) == comparable_text(&catalog_candidate.model);
            (
                exact_model,
                exact_manufacturer,
                overlap,
                catalog_candidate.id,
                catalog_candidate,
            )
        })
        .filter(|(exact_model, exact_manufacturer, overlap, _, _)| {
            *exact_model || *exact_manufacturer || *overlap > 0
        })
        .collect::<Vec<_>>();
    scored.sort_by(
        |(left_model, left_maker, left_overlap, left_id, _),
         (right_model, right_maker, right_overlap, right_id, _)| {
            right_model
                .cmp(left_model)
                .then_with(|| right_maker.cmp(left_maker))
                .then_with(|| right_overlap.cmp(left_overlap))
                .then_with(|| left_id.cmp(right_id))
        },
    );
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, _, _, candidate)| candidate.clone())
        .collect()
}

fn comparable_tokens(value: &str) -> BTreeSet<String> {
    comparable_text(value)
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn comparable_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn select_visual_assets(
    photos: &[MediaReference],
    attachments: &[MediaReference],
    limit: usize,
) -> Vec<BenchmarkVisualAsset> {
    let visual_attachments = attachments
        .iter()
        .filter(|reference| reference.is_visual_image())
        .collect::<Vec<_>>();
    let mut selected = Vec::new();
    let mut photo_index = 0usize;
    let mut attachment_index = 0usize;
    while selected.len() < limit
        && (photo_index < photos.len() || attachment_index < visual_attachments.len())
    {
        if let Some(photo) = photos.get(photo_index) {
            selected.push(visual_asset(photo));
            photo_index += 1;
            if selected.len() == limit {
                break;
            }
        }
        if let Some(attachment) = visual_attachments.get(attachment_index) {
            selected.push(visual_asset(attachment));
            attachment_index += 1;
        }
    }
    selected
}

fn visual_asset(reference: &MediaReference) -> BenchmarkVisualAsset {
    BenchmarkVisualAsset {
        image_id: reference.asset_id.clone(),
        media_url: reference.media_url.clone(),
        media_host: reference.media_host.clone(),
        media_kind: serde_enum_name(reference.kind),
        media_location: serde_enum_name(reference.location),
        mime_type: reference.expected_media_type.clone(),
        maximum_bytes: reference.fetch_policy.maximum_bytes,
        is_original: reference.is_original,
    }
}

fn serde_enum_name<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn stable_digest(parts: &[&str]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher.finalize().into()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkUsage {
    pub total_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub total_output_tokens: u64,
    pub thought_tokens: u64,
    pub tool_use_tokens: u64,
    pub grounded_requests: u64,
    pub successful_google_search_calls: u64,
    pub search_queries: u64,
    pub successful_url_context_calls: u64,
    pub citation_url_count: u64,
    pub attempts: u64,
    /// Whether every durable request record contributing to these totals has
    /// the counters required to estimate paid-list cost. Missing fields in
    /// older serialized benchmark attempts default to incomplete rather than
    /// restoring false precision.
    #[serde(default)]
    pub billable_usage_complete: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkAttempt {
    pub output: Option<Value>,
    pub usage: BenchmarkUsage,
    pub error: Option<String>,
}

pub type BenchmarkRunFuture<'a> = Pin<Box<dyn Future<Output = BenchmarkAttempt> + Send + 'a>>;

/// An execution adapter supplied by the admin layer. The benchmark module
/// itself cannot construct a network client or make a paid request.
pub trait BenchmarkRunner: Sync {
    fn run<'a>(&'a self, model: &'a str, case: &'a BenchmarkCase) -> BenchmarkRunFuture<'a>;
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkValidation {
    pub structured_output_success: bool,
    pub grounding_success: Option<bool>,
    pub independent_review_success: Option<bool>,
    pub errors: Vec<String>,
}

impl BenchmarkValidation {
    pub fn overall_success(&self) -> bool {
        self.structured_output_success
            && self.grounding_success.unwrap_or(true)
            && self.independent_review_success.unwrap_or(true)
            && self.errors.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkRun {
    pub case_id: String,
    pub task: BenchmarkTaskKind,
    pub model: String,
    pub latency_ms: u64,
    pub output: Option<Value>,
    pub usage: BenchmarkUsage,
    pub error: Option<String>,
    pub validation: BenchmarkValidation,
    pub estimated_cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkReport {
    pub schema_version: String,
    pub suite_seed: String,
    pub pricing_effective_date: String,
    pub pricing_source_url: String,
    pub search_free_tier_applied: bool,
    pub runs: Vec<BenchmarkRun>,
    pub summaries: Vec<BenchmarkSummary>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkSummary {
    pub model: String,
    pub task: BenchmarkTaskKind,
    pub run_count: usize,
    pub successful_run_count: usize,
    pub structured_output_success_count: usize,
    pub grounding_success_count: usize,
    pub independent_review_success_count: usize,
    pub error_count: usize,
    pub total_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub total_output_tokens: u64,
    pub thought_tokens: u64,
    pub tool_use_tokens: u64,
    pub grounded_requests: u64,
    pub successful_google_search_calls: u64,
    pub search_queries: u64,
    pub citation_url_count: u64,
    pub mean_latency_ms: u64,
    pub p50_latency_ms: u64,
    pub p95_latency_ms: u64,
    pub estimated_cost_usd: Option<f64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchBillingUnit {
    None,
    SearchQuery,
    GroundedRequest,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkModelPricing {
    pub input_usd_per_million_tokens: f64,
    pub cached_input_usd_per_million_tokens: f64,
    /// Gemini list pricing includes thinking tokens in the output rate.
    pub output_usd_per_million_tokens: f64,
    pub search_billing_unit: SearchBillingUnit,
    pub search_usd_per_thousand_units: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkPricing {
    pub effective_date: String,
    pub source_url: String,
    pub models: BTreeMap<String, BenchmarkModelPricing>,
}

impl BenchmarkPricing {
    /// Dated standard paid-tier list rates. Search free tiers are intentionally
    /// not subtracted because account-wide consumption is unavailable here.
    pub fn official_standard_defaults() -> Self {
        let mut models = BTreeMap::new();
        models.insert(
            "gemini-3.6-flash".to_string(),
            model_pricing(1.50, 0.15, 7.50, SearchBillingUnit::SearchQuery, 14.0),
        );
        models.insert(
            "gemini-3.5-flash".to_string(),
            model_pricing(1.50, 0.15, 9.00, SearchBillingUnit::SearchQuery, 14.0),
        );
        models.insert(
            "gemini-3.5-flash-lite".to_string(),
            model_pricing(0.30, 0.03, 2.50, SearchBillingUnit::SearchQuery, 14.0),
        );
        models.insert(
            "gemini-3.1-flash-lite".to_string(),
            model_pricing(0.25, 0.025, 1.50, SearchBillingUnit::SearchQuery, 14.0),
        );
        models.insert(
            "gemini-2.5-flash".to_string(),
            model_pricing(0.30, 0.03, 2.50, SearchBillingUnit::GroundedRequest, 35.0),
        );
        models.insert(
            "gemini-2.5-flash-lite".to_string(),
            model_pricing(0.10, 0.01, 0.40, SearchBillingUnit::GroundedRequest, 35.0),
        );
        Self {
            effective_date: DEFAULT_PRICING_EFFECTIVE_DATE.to_string(),
            source_url: GEMINI_PRICING_SOURCE_URL.to_string(),
            models,
        }
    }

    pub fn estimate(&self, model: &str, usage: &BenchmarkUsage) -> Option<f64> {
        if !usage.billable_usage_complete {
            return None;
        }
        let pricing = self.models.get(model)?;
        let cached = usage.cached_input_tokens.min(usage.total_input_tokens);
        let uncached = usage.total_input_tokens.saturating_sub(cached);
        // Tool-use tokens are an informational breakdown of tokens already
        // represented by provider input/output totals; do not bill them twice.
        let billed_output = usage
            .total_output_tokens
            .saturating_add(usage.thought_tokens);
        let token_cost = uncached as f64 / 1_000_000.0 * pricing.input_usd_per_million_tokens
            + cached as f64 / 1_000_000.0 * pricing.cached_input_usd_per_million_tokens
            + billed_output as f64 / 1_000_000.0 * pricing.output_usd_per_million_tokens;
        let search_units = match pricing.search_billing_unit {
            SearchBillingUnit::None => 0,
            SearchBillingUnit::SearchQuery => usage.search_queries,
            SearchBillingUnit::GroundedRequest => usage.grounded_requests,
        };
        Some(token_cost + search_units as f64 / 1_000.0 * pricing.search_usd_per_thousand_units)
    }
}

fn model_pricing(
    input: f64,
    cached_input: f64,
    output: f64,
    search_billing_unit: SearchBillingUnit,
    search: f64,
) -> BenchmarkModelPricing {
    BenchmarkModelPricing {
        input_usd_per_million_tokens: input,
        cached_input_usd_per_million_tokens: cached_input,
        output_usd_per_million_tokens: output,
        search_billing_unit,
        search_usd_per_thousand_units: search,
    }
}

pub async fn execute<R: BenchmarkRunner>(
    suite: &BenchmarkSuite,
    models: &[String],
    runner: &R,
    pricing: &BenchmarkPricing,
) -> Result<BenchmarkReport> {
    validate_models(models)?;
    let mut runs = Vec::with_capacity(suite.cases.len().saturating_mul(models.len()));
    for model in models {
        for case in &suite.cases {
            let started = Instant::now();
            let attempt = runner.run(model, case).await;
            let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            runs.push(evaluate_attempt(case, model, attempt, latency_ms, pricing));
        }
    }
    let summaries = summarize(&runs);
    Ok(BenchmarkReport {
        schema_version: BENCHMARK_SCHEMA_VERSION.to_string(),
        suite_seed: suite.seed.clone(),
        pricing_effective_date: pricing.effective_date.clone(),
        pricing_source_url: pricing.source_url.clone(),
        search_free_tier_applied: false,
        runs,
        summaries,
    })
}

fn validate_models(models: &[String]) -> Result<()> {
    if models.is_empty() {
        bail!("at least one benchmark model is required");
    }
    let mut unique = BTreeSet::new();
    for model in models {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            bail!("benchmark model must not be blank");
        }
        if trimmed.ends_with("-latest") {
            bail!("benchmark models must be pinned, not -latest aliases");
        }
        if !unique.insert(trimmed) {
            bail!("duplicate benchmark model: {trimmed}");
        }
    }
    Ok(())
}

pub fn evaluate_attempt(
    case: &BenchmarkCase,
    model: &str,
    attempt: BenchmarkAttempt,
    latency_ms: u64,
    pricing: &BenchmarkPricing,
) -> BenchmarkRun {
    let mut validation = validate_output(case, attempt.output.as_ref(), &attempt.usage);
    if let Some(error) = attempt.error.as_ref() {
        validation
            .errors
            .push(format!("runner returned error: {error}"));
        validation.structured_output_success = false;
    }
    let estimated_cost_usd = pricing.estimate(model, &attempt.usage);
    BenchmarkRun {
        case_id: case.id.clone(),
        task: case.task,
        model: model.to_string(),
        latency_ms,
        output: attempt.output,
        usage: attempt.usage,
        error: attempt.error,
        validation,
        estimated_cost_usd,
    }
}

pub fn validate_output(
    case: &BenchmarkCase,
    output: Option<&Value>,
    usage: &BenchmarkUsage,
) -> BenchmarkValidation {
    match case.task {
        BenchmarkTaskKind::ListingExtraction => validate_listing_output(output),
        BenchmarkTaskKind::GroundedMetadata => validate_grounded_metadata_output(output, usage),
        BenchmarkTaskKind::AvionicsGroundingReview => validate_avionics_output(case, output, usage),
        BenchmarkTaskKind::VisualIdentity => validate_visual_output(case, output),
    }
}

fn validate_listing_output(output: Option<&Value>) -> BenchmarkValidation {
    let mut errors = Vec::new();
    let Some(output) = output else {
        return failed_validation(None, None, "missing listing extraction output");
    };
    let payload = output.get("parsed_listing").unwrap_or(output);
    let parsed = match serde_json::from_value::<ParsedListing>(payload.clone()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return failed_validation(
                None,
                None,
                &format!("listing output does not match ParsedListing: {error}"),
            )
        }
    };
    if parsed.manufacturer.as_deref().is_none_or(str::is_empty) {
        errors.push("manufacturer is missing".to_string());
    }
    if parsed.model.as_deref().is_none_or(str::is_empty) {
        errors.push("model is missing".to_string());
    }
    if parsed.currency.len() != 3
        || !parsed
            .currency
            .bytes()
            .all(|byte| byte.is_ascii_uppercase())
    {
        errors.push("currency must be a three-letter uppercase code".to_string());
    }
    if let Some(year) = parsed.model_year {
        if !(1900..=2039).contains(&year) {
            errors.push("model_year is outside 1900..=2039".to_string());
        }
    }
    if let Some(price) = parsed.asking_price_usd {
        if !is_plausible_asking_price_usd(price) {
            errors.push("asking_price_usd is outside the accepted range".to_string());
        }
    }
    for (field, hours) in [
        ("airframe_hours", parsed.airframe_hours),
        ("engine_hours", parsed.engine_hours),
        ("propeller_hours", parsed.propeller_hours),
    ] {
        if hours.is_some_and(|value| !value.is_finite() || value < 0.0) {
            errors.push(format!("{field} must be finite and nonnegative"));
        }
    }
    for (index, avionics) in parsed.avionics.iter().enumerate() {
        if avionics.manufacturer.trim().is_empty() || avionics.model.trim().is_empty() {
            errors.push(format!(
                "avionics[{index}] must have nonblank manufacturer and model"
            ));
        }
        if avionics.avionics_types.is_empty() {
            errors.push(format!("avionics[{index}] has no capability types"));
        }
    }
    BenchmarkValidation {
        structured_output_success: errors.is_empty(),
        grounding_success: None,
        independent_review_success: None,
        errors,
    }
}

fn validate_grounded_metadata_output(
    output: Option<&Value>,
    usage: &BenchmarkUsage,
) -> BenchmarkValidation {
    let mut errors = Vec::new();
    let object = output.and_then(Value::as_object);
    if object.is_none() {
        errors.push("grounded metadata output must be an object".to_string());
    }
    if let Some(output) = output {
        validate_metadata_identity_fields(output, "", &mut errors);

        let introduced_year = output.get("introduced_year").and_then(Value::as_i64);
        if introduced_year.is_none_or(|year| !(1940..=2100).contains(&year)) {
            errors.push("introduced_year must be an integer in 1940..=2100".to_string());
        }

        let mut values = BTreeMap::new();
        for field in [
            "estimated_unit_value_usd",
            "installed_value_contribution_usd",
            "replacement_cost_usd",
        ] {
            match output.get(field).and_then(Value::as_f64) {
                Some(value) if value.is_finite() && value >= 0.0 => {
                    values.insert(field, value);
                }
                _ => errors.push(format!("{field} must be a finite nonnegative number")),
            }
        }
        if let (Some(compatibility), Some(installed)) = (
            values.get("estimated_unit_value_usd"),
            values.get("installed_value_contribution_usd"),
        ) {
            let tolerance = (*installed * 0.01).max(1.0);
            if (*compatibility - *installed).abs() > tolerance {
                errors.push(
                    "estimated_unit_value_usd must repeat installed_value_contribution_usd"
                        .to_string(),
                );
            }
        }
        if let (Some(installed), Some(replacement)) = (
            values.get("installed_value_contribution_usd"),
            values.get("replacement_cost_usd"),
        ) {
            if replacement < installed {
                errors.push(
                    "replacement_cost_usd cannot be below installed_value_contribution_usd"
                        .to_string(),
                );
            }
        }

        let scope = output.get("valuation_scope").and_then(Value::as_str);
        if !matches!(scope, Some("unit" | "integrated_suite")) {
            errors.push("valuation_scope must be unit or integrated_suite".to_string());
        }
        let components = output.get("included_components").and_then(Value::as_array);
        if components.is_none() {
            errors.push("included_components must be an array".to_string());
        }
        if scope == Some("unit") && components.is_some_and(|components| !components.is_empty()) {
            errors.push("unit metadata cannot include suite components".to_string());
        }
        if scope == Some("integrated_suite") && components.is_none_or(Vec::is_empty) {
            errors.push("integrated_suite metadata requires included components".to_string());
        }
        for (index, component) in components.into_iter().flatten().enumerate() {
            let prefix = format!("included_components[{index}].");
            for field in ["manufacturer", "model"] {
                if component
                    .get(field)
                    .and_then(Value::as_str)
                    .is_none_or(|value| value.trim().is_empty())
                {
                    errors.push(format!("{prefix}{field} must be nonblank"));
                }
            }
            if component
                .get("types")
                .and_then(Value::as_array)
                .is_none_or(|types| {
                    types.is_empty()
                        || types
                            .iter()
                            .any(|value| value.as_str().is_none_or(|value| value.trim().is_empty()))
                })
            {
                errors.push(format!("{prefix}types must contain nonblank strings"));
            }
            if component
                .get("quantity")
                .and_then(Value::as_i64)
                .is_none_or(|quantity| quantity <= 0)
            {
                errors.push(format!("{prefix}quantity must be positive"));
            }
            validate_metadata_identity_fields(component, &prefix, &mut errors);
        }

        if !matches!(
            output.get("confidence").and_then(Value::as_str),
            Some("high" | "medium" | "low")
        ) {
            errors.push("confidence must be high, medium, or low".to_string());
        }
    }

    let structural_error_count = errors.len();
    let grounding_success = Some(
        usage.grounded_requests >= 1
            && usage.successful_google_search_calls >= 1
            && usage.citation_url_count >= 1,
    );
    if grounding_success == Some(false) {
        errors.push(
            "grounded metadata requires an observed Google Search call and cited grounding source"
                .to_string(),
        );
    }
    BenchmarkValidation {
        structured_output_success: object.is_some() && structural_error_count == 0,
        grounding_success,
        independent_review_success: None,
        errors,
    }
}

fn validate_metadata_identity_fields(value: &Value, prefix: &str, errors: &mut Vec<String>) {
    let kind = value
        .get("manufacturer_identifier_kind")
        .and_then(Value::as_str);
    if !matches!(
        kind,
        Some("manufacturer_part_number" | "manufacturer_model_number" | "sku" | "none")
    ) {
        errors.push(format!(
            "{prefix}manufacturer_identifier_kind has an unsupported value"
        ));
    }
    let identifier = value.get("manufacturer_identifier").and_then(Value::as_str);
    if identifier.is_none()
        || (kind == Some("none") && identifier.is_some_and(|value| !value.trim().is_empty()))
        || (kind != Some("none") && identifier.is_none_or(|value| value.trim().is_empty()))
    {
        errors.push(format!(
            "{prefix}manufacturer_identifier does not match its identifier kind"
        ));
    }
    let source_url = value.get("identity_source_url").and_then(Value::as_str);
    if source_url.is_none_or(|url| {
        let url = url.trim();
        url.is_empty() || !(url.starts_with("https://") || url.starts_with("http://"))
    }) {
        errors.push(format!(
            "{prefix}identity_source_url must be a nonblank HTTP(S) URL"
        ));
    }
    for field in ["identity_source_title", "identity_evidence"] {
        if value
            .get(field)
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            errors.push(format!("{prefix}{field} must be nonblank"));
        }
    }
    if !matches!(
        value.get("identity_confidence").and_then(Value::as_str),
        Some("very_high" | "high" | "medium" | "low")
    ) {
        errors.push(format!(
            "{prefix}identity_confidence has an unsupported value"
        ));
    }
}

fn validate_avionics_output(
    case: &BenchmarkCase,
    output: Option<&Value>,
    usage: &BenchmarkUsage,
) -> BenchmarkValidation {
    let mut errors = Vec::new();
    let root = output.and_then(Value::as_object);
    // A runner may return the first-stage response directly, or wrap both
    // production stages as {"classification": ..., "review": ...}.
    let classification = root
        .and_then(|object| object.get("classification"))
        .and_then(Value::as_object)
        .or(root);
    let status = classification
        .and_then(|object| object.get("status"))
        .and_then(Value::as_str);
    if !matches!(
        status,
        Some("existing_match" | "propose_new" | "reject" | "unresolved")
    ) {
        errors
            .push("status must be existing_match, propose_new, reject, or unresolved".to_string());
    }
    let reason = classification
        .and_then(|object| object.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if reason.trim().is_empty() {
        errors.push("reason must be nonblank".to_string());
    }
    let catalog_id = classification
        .and_then(|object| object.get("catalog_id"))
        .and_then(Value::as_i64);
    let confidence = classification
        .and_then(|object| object.get("confidence"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(confidence, "very_high" | "high" | "medium" | "low") {
        errors.push("confidence must be very_high, high, medium, or low".to_string());
    }
    let positive = matches!(status, Some("existing_match" | "propose_new"));
    if positive {
        for field in [
            "canonical_manufacturer",
            "canonical_model",
            "manufacturer_identifier_kind",
            "manufacturer_identifier",
            "identity_source_url",
            "identity_source_title",
            "identity_evidence",
        ] {
            if classification
                .and_then(|object| object.get(field))
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty() || value == "none")
            {
                errors.push(format!("positive identity requires nonblank {field}"));
            }
        }
        if classification
            .and_then(|object| object.get("canonical_types"))
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            errors.push("positive identity requires canonical_types".to_string());
        }
    }

    let catalog_candidates: &[BenchmarkCatalogCandidate] = match &case.input {
        BenchmarkInput::AvionicsGroundingReview {
            catalog_candidates, ..
        } => catalog_candidates,
        _ => &[],
    };
    if status == Some("existing_match") {
        if catalog_id.is_none_or(|catalog_id| {
            !catalog_candidates
                .iter()
                .any(|candidate| candidate.id == catalog_id)
        }) {
            errors.push(
                "existing_match must select one of the supplied catalog candidate IDs".to_string(),
            );
        }
    }
    if status == Some("propose_new") && catalog_id != Some(0) {
        errors.push("propose_new must use catalog_id=0".to_string());
    }
    if matches!(status, Some("reject" | "unresolved")) && catalog_id != Some(0) {
        errors.push("reject and unresolved must use catalog_id=0".to_string());
    }
    let structural_error_count = errors.len();

    let review = root
        .and_then(|object| object.get("review"))
        .and_then(Value::as_object);
    let mut review_shape_success = !positive;
    if positive {
        let proposal_confirmed = review
            .and_then(|object| object.get("proposal_decision"))
            .and_then(Value::as_str)
            == Some("confirmed_same_as_input");
        let reviewed = review
            .and_then(|object| object.get("reviews"))
            .and_then(Value::as_array)
            .map(|reviews| {
                reviews
                    .iter()
                    .filter_map(|review| {
                        Some((
                            review.get("catalog_id")?.as_i64()?,
                            review.get("decision")?.as_str()?,
                        ))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let supplied_ids = catalog_candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<BTreeSet<_>>();
        let reviewed_ids = reviewed.keys().copied().collect::<BTreeSet<_>>();
        let exact_coverage = supplied_ids == reviewed_ids;
        let selected_confirmed = status != Some("existing_match")
            || catalog_id
                .and_then(|id| reviewed.get(&id))
                .is_some_and(|decision| *decision == "same_product");
        review_shape_success = proposal_confirmed && exact_coverage && selected_confirmed;
        if review.is_none() {
            errors.push("positive identity is missing independent review output".to_string());
        } else if !proposal_confirmed {
            errors.push("independent review did not confirm the input identity".to_string());
        } else if !exact_coverage {
            errors.push(
                "independent review must cover every supplied catalog candidate exactly once"
                    .to_string(),
            );
        } else if !selected_confirmed {
            errors.push(
                "independent review did not confirm the selected existing catalog row".to_string(),
            );
        }
    }

    let required_grounded_requests = if positive { 2 } else { 1 };
    let grounding_success = Some(
        usage.grounded_requests >= required_grounded_requests
            && usage.successful_google_search_calls >= required_grounded_requests
            && usage.citation_url_count > 0,
    );
    if grounding_success == Some(false) {
        errors.push(if positive {
            "positive identity requires grounded classification and review calls with cited Search results"
                .to_string()
        } else {
            "negative identity requires a grounded classification call with cited Search results"
                .to_string()
        });
    }
    let independent_review_success =
        positive.then(|| review_shape_success && usage.grounded_requests >= 2);
    if independent_review_success == Some(false) {
        errors.push(
            "positive avionics identity requires a complete independent grounded review"
                .to_string(),
        );
    }
    BenchmarkValidation {
        structured_output_success: classification.is_some() && structural_error_count == 0,
        grounding_success,
        independent_review_success,
        errors,
    }
}

fn validate_visual_output(case: &BenchmarkCase, output: Option<&Value>) -> BenchmarkValidation {
    let mut errors = Vec::new();
    let object = output.and_then(Value::as_object);
    let status = object
        .and_then(|object| object.get("status"))
        .and_then(Value::as_str);
    if !matches!(
        status,
        Some("candidates_visible" | "no_explicit_identifier_visible")
    ) {
        errors.push(
            "status must be candidates_visible or no_explicit_identifier_visible".to_string(),
        );
    }
    let observations = object
        .and_then(|object| object.get("observations"))
        .and_then(Value::as_array);
    if observations.is_none() {
        errors.push("observations must be an array".to_string());
    }
    let allowed_image_ids = match &case.input {
        BenchmarkInput::VisualIdentity { assets, .. } => assets
            .iter()
            .map(|asset| asset.image_id.as_str())
            .collect::<BTreeSet<_>>(),
        _ => BTreeSet::new(),
    };
    for (index, observation) in observations.into_iter().flatten().enumerate() {
        let image_id = observation
            .get("image_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !allowed_image_ids.contains(image_id) {
            errors.push(format!("observations[{index}] uses unknown image_id"));
        }
        let confidence = observation
            .get("confidence")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(confidence, "high" | "very_high") {
            errors.push(format!(
                "observations[{index}] confidence must be high or very_high"
            ));
        }
        if observation
            .get("visible_text")
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            errors.push(format!("observations[{index}] visible_text is blank"));
        }
    }
    if status == Some("candidates_visible") && observations.is_none_or(Vec::is_empty) {
        errors.push("candidates_visible requires at least one observation".to_string());
    }
    if status == Some("no_explicit_identifier_visible")
        && observations.is_some_and(|observations| !observations.is_empty())
    {
        errors.push("no_explicit_identifier_visible requires no observations".to_string());
    }
    let expected_registration = match &case.input {
        BenchmarkInput::VisualIdentity {
            prior_audit: Some(prior_audit),
            ..
        } => prior_audit
            .pointer("/registration_consensus/normalized_n_number")
            .and_then(Value::as_str),
        _ => None,
    };
    if let Some(expected_registration) = expected_registration {
        let expected = comparable_registration(expected_registration);
        let exact_match = observations.into_iter().flatten().any(|observation| {
            observation
                .get("identifier_kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "registration")
                && observation
                    .get("visible_text")
                    .and_then(Value::as_str)
                    .is_some_and(|value| comparable_registration(value) == expected)
        });
        if !exact_match {
            errors.push(format!(
                "visual output did not reproduce previously FAA-validated registration {expected_registration}"
            ));
        }
    }
    BenchmarkValidation {
        structured_output_success: object.is_some() && errors.is_empty(),
        grounding_success: None,
        independent_review_success: None,
        errors,
    }
}

fn comparable_registration(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_uppercase())
        .collect()
}

fn failed_validation(
    grounding_success: Option<bool>,
    independent_review_success: Option<bool>,
    error: &str,
) -> BenchmarkValidation {
    BenchmarkValidation {
        structured_output_success: false,
        grounding_success,
        independent_review_success,
        errors: vec![error.to_string()],
    }
}

fn summarize(runs: &[BenchmarkRun]) -> Vec<BenchmarkSummary> {
    let mut grouped = BTreeMap::<(String, BenchmarkTaskKind), Vec<&BenchmarkRun>>::new();
    for run in runs {
        grouped
            .entry((run.model.clone(), run.task))
            .or_default()
            .push(run);
    }
    grouped
        .into_iter()
        .map(|((model, task), runs)| summarize_group(model, task, &runs))
        .collect()
}

fn summarize_group(
    model: String,
    task: BenchmarkTaskKind,
    runs: &[&BenchmarkRun],
) -> BenchmarkSummary {
    let mut latencies = runs.iter().map(|run| run.latency_ms).collect::<Vec<_>>();
    latencies.sort_unstable();
    let sum_latency = latencies.iter().copied().fold(0_u64, u64::saturating_add);
    let cost_known = runs.iter().all(|run| run.estimated_cost_usd.is_some());
    BenchmarkSummary {
        model,
        task,
        run_count: runs.len(),
        successful_run_count: runs
            .iter()
            .filter(|run| run.validation.overall_success())
            .count(),
        structured_output_success_count: runs
            .iter()
            .filter(|run| run.validation.structured_output_success)
            .count(),
        grounding_success_count: runs
            .iter()
            .filter(|run| run.validation.grounding_success == Some(true))
            .count(),
        independent_review_success_count: runs
            .iter()
            .filter(|run| run.validation.independent_review_success == Some(true))
            .count(),
        error_count: runs.iter().filter(|run| run.error.is_some()).count(),
        total_input_tokens: usage_sum(runs, |usage| usage.total_input_tokens),
        cached_input_tokens: usage_sum(runs, |usage| usage.cached_input_tokens),
        total_output_tokens: usage_sum(runs, |usage| usage.total_output_tokens),
        thought_tokens: usage_sum(runs, |usage| usage.thought_tokens),
        tool_use_tokens: usage_sum(runs, |usage| usage.tool_use_tokens),
        grounded_requests: usage_sum(runs, |usage| usage.grounded_requests),
        successful_google_search_calls: usage_sum(runs, |usage| {
            usage.successful_google_search_calls
        }),
        search_queries: usage_sum(runs, |usage| usage.search_queries),
        citation_url_count: usage_sum(runs, |usage| usage.citation_url_count),
        mean_latency_ms: if runs.is_empty() {
            0
        } else {
            sum_latency / runs.len() as u64
        },
        p50_latency_ms: percentile(&latencies, 50),
        p95_latency_ms: percentile(&latencies, 95),
        estimated_cost_usd: cost_known
            .then(|| runs.iter().filter_map(|run| run.estimated_cost_usd).sum()),
    }
}

fn usage_sum(runs: &[&BenchmarkRun], value: impl Fn(&BenchmarkUsage) -> u64) -> u64 {
    runs.iter()
        .map(|run| value(&run.usage))
        .fold(0, u64::saturating_add)
}

fn percentile(sorted_values: &[u64], percentile: usize) -> u64 {
    if sorted_values.is_empty() {
        return 0;
    }
    let index = ((sorted_values.len() - 1) * percentile).div_ceil(100);
    sorted_values[index.min(sorted_values.len() - 1)]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    fn source_row(id: i64, html: &str, extracted: Option<Value>) -> SourceRow {
        SourceRow {
            submission_id: id,
            listing_id: Some(id + 100),
            source_url: format!(
                "https://www.controller.com/listing/for-sale/{}/test-aircraft",
                100_000_000 + id
            ),
            rendered_html: html.to_string(),
            rendered_html_sha256: sha256_hex(html.as_bytes()),
            extracted_listing_json: extracted.map(|value| value.to_string()),
            extraction_error: None,
        }
    }

    fn parsed_listing_value() -> Value {
        json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "T",
            "model_year": 2008,
            "asking_price_usd": 525000.0,
            "currency": "USD",
            "airframe_hours": 1100.0,
            "engine_hours": 350.0,
            "engine_time_basis": "SNEW",
            "engine_time_evidence": "350 SNEW",
            "engine_time_confidence": "high",
            "propeller_hours": null,
            "propeller_time_basis": "unknown",
            "propeller_time_evidence": null,
            "propeller_time_confidence": null,
            "installed_engine": null,
            "installed_propeller": null,
            "registration_number": "N123AB",
            "serial_number": "18281234",
            "status": "active",
            "avionics": [{
                "manufacturer": "Garmin",
                "model": "G1000 NXi",
                "types": ["Flight Display", "Navigation"],
                "quantity": 1,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": "Garmin G1000 NXi",
                "source_confidence": "high"
            }],
            "valuation_facts": []
        })
    }

    fn gallery_html() -> String {
        r#"
        <html><body>
          <main><h1>2008 Cessna 182T</h1><p>Garmin G1000 NXi</p></main>
          <div class="mc-items"><img
            data-fullscreen="https://media.sandhills.com/img.axd?id=11002241579&amp;p=&amp;w=1200&amp;h=900&amp;t=&amp;lp=TH&amp;wt=False&amp;sz=Max&amp;rs=On&amp;checksum=abc"
          ></div>
        </body></html>
        "#
        .to_string()
    }

    fn grounded_metadata_value() -> Value {
        json!({
            "manufacturer_identifier_kind": "manufacturer_model_number",
            "manufacturer_identifier": "GTX345R",
            "identity_source_url": "https://www.garmin.com/example/gtx-345r",
            "identity_source_title": "GTX 345R product page",
            "identity_evidence": "The manufacturer identifies the GTX 345R model.",
            "identity_confidence": "high",
            "introduced_year": 2016,
            "estimated_unit_value_usd": 5000.0,
            "installed_value_contribution_usd": 5000.0,
            "replacement_cost_usd": 9000.0,
            "valuation_scope": "unit",
            "included_components": [],
            "confidence": "high"
        })
    }

    #[test]
    fn deterministic_sampling_is_stable_across_input_order_and_seeded() {
        let rows = (1..=20)
            .map(|id| source_row(id, &format!("<p>{id}</p>"), None))
            .collect::<Vec<_>>();
        let mut reversed = rows.clone();
        reversed.reverse();
        let first = deterministic_sample(rows.clone(), "stable", 5)
            .into_iter()
            .map(|row| row.submission_id)
            .collect::<Vec<_>>();
        let second = deterministic_sample(reversed, "stable", 5)
            .into_iter()
            .map(|row| row.submission_id)
            .collect::<Vec<_>>();
        let other_seed = deterministic_sample(rows, "different", 5)
            .into_iter()
            .map(|row| row.submission_id)
            .collect::<Vec<_>>();
        assert_eq!(first, second);
        assert_ne!(first, other_seed);
    }

    #[test]
    fn canonical_listing_ids_bypass_sampling_and_are_not_submission_ids() {
        let rows = vec![
            source_row(101, "<p>listing 201</p>", None),
            source_row(2, "<p>listing 102</p>", None),
            source_row(1, "<p>listing 101</p>", None),
        ];
        let suite = build_suite(
            rows,
            Vec::new(),
            &BenchmarkSelection {
                listing_limit: 1,
                listing_ids: vec![102, 201],
                ..BenchmarkSelection::default()
            },
        )
        .expect("canonical listing IDs should select their retained submissions");

        assert_eq!(suite.selected_submission_ids, [2, 101]);
        assert_eq!(
            suite
                .cases
                .iter()
                .map(|case| case.listing_id)
                .collect::<Vec<_>>(),
            [Some(102), Some(201)]
        );
    }

    #[test]
    fn explicit_selection_rejects_ids_without_retained_submissions() {
        let error = build_suite(
            vec![source_row(1, "<p>listing 101</p>", None)],
            Vec::new(),
            &BenchmarkSelection {
                listing_ids: vec![999],
                ..BenchmarkSelection::default()
            },
        )
        .expect_err("missing canonical listing IDs must fail closed");

        assert!(error
            .to_string()
            .contains("listing ID 999 does not identify a retained plugin submission"));
    }

    #[test]
    fn suite_builder_emits_all_four_tasks_with_one_retained_metadata_candidate() {
        let html = gallery_html();
        let mut reference = parsed_listing_value();
        reference["avionics"].as_array_mut().unwrap().push(json!({
            "manufacturer": "Garmin",
            "model": "GTX 345R",
            "types": ["Transponder"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": "Garmin GTX 345R",
            "source_confidence": "high"
        }));
        let suite = build_suite(
            vec![source_row(7, &html, Some(reference))],
            vec![BenchmarkCatalogCandidate {
                id: 42,
                manufacturer: "Garmin".to_string(),
                model: "G1000 NXi".to_string(),
                catalog_status: "approved".to_string(),
                manufacturer_identifier_kind: Some("manufacturer_model_number".to_string()),
                manufacturer_identifier: Some("G1000NXI".to_string()),
                identity_source_url: Some("https://www.garmin.com/example".to_string()),
                identity_confidence: Some("very_high".to_string()),
                avionics_types: vec!["Flight Display".to_string()],
            }],
            &BenchmarkSelection {
                listing_limit: 1,
                ..BenchmarkSelection::default()
            },
        )
        .unwrap();
        let tasks = suite
            .cases
            .iter()
            .map(|case| case.task)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            tasks,
            BTreeSet::from([
                BenchmarkTaskKind::ListingExtraction,
                BenchmarkTaskKind::GroundedMetadata,
                BenchmarkTaskKind::AvionicsGroundingReview,
                BenchmarkTaskKind::VisualIdentity,
            ])
        );
        let metadata_cases = suite
            .cases
            .iter()
            .filter(|case| case.task == BenchmarkTaskKind::GroundedMetadata)
            .collect::<Vec<_>>();
        assert_eq!(metadata_cases.len(), 1);
        let BenchmarkInput::GroundedMetadata {
            candidate,
            value_reference_year,
        } = &metadata_cases[0].input
        else {
            panic!("grounded metadata case has the wrong input kind")
        };
        assert_eq!(candidate.manufacturer, "Garmin");
        assert!(matches!(candidate.model.as_str(), "G1000 NXi" | "GTX 345R"));
        assert_eq!(*value_reference_year, 2026);
        assert!(suite
            .cases
            .iter()
            .all(|case| !case.reference_is_ground_truth));
    }

    #[test]
    fn validators_separate_structure_grounding_and_review() {
        let row = source_row(9, &gallery_html(), Some(parsed_listing_value()));
        let suite = build_suite(
            vec![row],
            Vec::new(),
            &BenchmarkSelection {
                listing_limit: 1,
                ..BenchmarkSelection::default()
            },
        )
        .unwrap();
        let listing = suite
            .cases
            .iter()
            .find(|case| case.task == BenchmarkTaskKind::ListingExtraction)
            .unwrap();
        assert!(validate_output(
            listing,
            Some(&parsed_listing_value()),
            &BenchmarkUsage::default()
        )
        .overall_success());

        let avionics = suite
            .cases
            .iter()
            .find(|case| case.task == BenchmarkTaskKind::AvionicsGroundingReview)
            .unwrap();
        let ungrounded = validate_output(
            avionics,
            Some(&json!({
                "classification": {
                    "status": "propose_new",
                    "catalog_id": 0,
                    "canonical_manufacturer": "Garmin",
                    "canonical_model": "G1000 NXi",
                    "canonical_types": ["Flight Display", "Navigation"],
                    "manufacturer_identifier_kind": "manufacturer_model_number",
                    "manufacturer_identifier": "G1000NXI",
                    "confidence": "very_high",
                    "identity_source_url": "https://www.garmin.com/example",
                    "identity_source_title": "G1000 NXi",
                    "identity_evidence": "Official product documentation",
                    "reason": "manufacturer documentation distinguishes it"
                },
                "review": {
                    "proposal_decision": "confirmed_same_as_input",
                    "reviews": []
                }
            })),
            &BenchmarkUsage::default(),
        );
        assert!(ungrounded.structured_output_success);
        assert_eq!(ungrounded.grounding_success, Some(false));
        assert_eq!(ungrounded.independent_review_success, Some(false));
    }

    #[test]
    fn grounded_metadata_validator_requires_structure_search_and_citations() {
        let suite = build_suite(
            vec![source_row(
                12,
                &gallery_html(),
                Some(parsed_listing_value()),
            )],
            Vec::new(),
            &BenchmarkSelection {
                listing_limit: 1,
                ..BenchmarkSelection::default()
            },
        )
        .unwrap();
        let metadata = suite
            .cases
            .iter()
            .find(|case| case.task == BenchmarkTaskKind::GroundedMetadata)
            .unwrap();
        let grounded_usage = BenchmarkUsage {
            grounded_requests: 1,
            successful_google_search_calls: 1,
            citation_url_count: 1,
            ..BenchmarkUsage::default()
        };

        assert!(
            validate_output(metadata, Some(&grounded_metadata_value()), &grounded_usage)
                .overall_success()
        );

        let ungrounded = validate_output(
            metadata,
            Some(&grounded_metadata_value()),
            &BenchmarkUsage::default(),
        );
        assert!(ungrounded.structured_output_success);
        assert_eq!(ungrounded.grounding_success, Some(false));

        let mut malformed = grounded_metadata_value();
        malformed.as_object_mut().unwrap().remove("introduced_year");
        let invalid = validate_output(metadata, Some(&malformed), &grounded_usage);
        assert!(!invalid.structured_output_success);
        assert_eq!(invalid.grounding_success, Some(true));
    }

    #[test]
    fn visual_validator_rejects_unknown_image_ids() {
        let suite = build_suite(
            vec![source_row(4, &gallery_html(), Some(parsed_listing_value()))],
            Vec::new(),
            &BenchmarkSelection {
                listing_limit: 1,
                ..BenchmarkSelection::default()
            },
        )
        .unwrap();
        let visual = suite
            .cases
            .iter()
            .find(|case| case.task == BenchmarkTaskKind::VisualIdentity)
            .unwrap();
        let validation = validate_output(
            visual,
            Some(&json!({
                "status": "candidates_visible",
                "observations": [{
                    "image_id": "invented",
                    "visible_text": "N123AB",
                    "confidence": "very_high"
                }]
            })),
            &BenchmarkUsage::default(),
        );
        assert!(!validation.structured_output_success);
        assert!(validation
            .errors
            .iter()
            .any(|error| error.contains("unknown image_id")));
    }

    #[test]
    fn pricing_uses_uncached_cached_output_and_post_free_search_rates() {
        let pricing = BenchmarkPricing::official_standard_defaults();
        let usage = BenchmarkUsage {
            total_input_tokens: 1_000_000,
            cached_input_tokens: 250_000,
            total_output_tokens: 100_000,
            thought_tokens: 50_000,
            search_queries: 2,
            billable_usage_complete: true,
            ..BenchmarkUsage::default()
        };
        let cost = pricing.estimate("gemini-3.5-flash", &usage).unwrap();
        let expected = 0.75 * 1.50 + 0.25 * 0.15 + 0.15 * 9.00 + 2.0 / 1000.0 * 14.0;
        assert!((cost - expected).abs() < 1e-10);

        let mut incomplete_usage = usage.clone();
        incomplete_usage.billable_usage_complete = false;
        assert_eq!(
            pricing.estimate("gemini-3.5-flash", &incomplete_usage),
            None
        );
        assert!(pricing.estimate("unknown-model", &usage).is_none());
    }

    #[test]
    fn legacy_usage_json_defaults_billable_completeness_to_false() {
        let usage: BenchmarkUsage = serde_json::from_value(json!({
            "total_input_tokens": 120,
            "cached_input_tokens": 20,
            "total_output_tokens": 30,
            "thought_tokens": 4,
            "tool_use_tokens": 2,
            "grounded_requests": 1,
            "successful_google_search_calls": 1,
            "search_queries": 1,
            "successful_url_context_calls": 0,
            "citation_url_count": 2,
            "attempts": 1
        }))
        .unwrap();

        assert_eq!(usage.total_input_tokens, 120);
        assert!(!usage.billable_usage_complete);

        let encoded = serde_json::to_value(&usage).unwrap();
        assert_eq!(encoded["billable_usage_complete"], false);
        assert_eq!(
            serde_json::from_value::<BenchmarkUsage>(encoded).unwrap(),
            usage
        );
    }

    struct FakeRunner;

    impl BenchmarkRunner for FakeRunner {
        fn run<'a>(&'a self, _model: &'a str, case: &'a BenchmarkCase) -> BenchmarkRunFuture<'a> {
            Box::pin(async move {
                let output = match case.task {
                    BenchmarkTaskKind::ListingExtraction => Some(parsed_listing_value()),
                    BenchmarkTaskKind::GroundedMetadata => Some(grounded_metadata_value()),
                    BenchmarkTaskKind::AvionicsGroundingReview => Some(json!({
                        "status": "reject",
                        "catalog_id": 0,
                        "confidence": "high",
                        "reason": "generic capability, not a product identity"
                    })),
                    BenchmarkTaskKind::VisualIdentity => Some(json!({
                        "status": "no_explicit_identifier_visible",
                        "observations": []
                    })),
                };
                BenchmarkAttempt {
                    output,
                    usage: BenchmarkUsage {
                        total_input_tokens: 100,
                        total_output_tokens: 20,
                        grounded_requests: match case.task {
                            BenchmarkTaskKind::GroundedMetadata => 1,
                            BenchmarkTaskKind::AvionicsGroundingReview => 2,
                            _ => 0,
                        },
                        successful_google_search_calls: match case.task {
                            BenchmarkTaskKind::GroundedMetadata => 1,
                            BenchmarkTaskKind::AvionicsGroundingReview => 2,
                            _ => 0,
                        },
                        search_queries: match case.task {
                            BenchmarkTaskKind::GroundedMetadata => 1,
                            BenchmarkTaskKind::AvionicsGroundingReview => 2,
                            _ => 0,
                        },
                        citation_url_count: if matches!(
                            case.task,
                            BenchmarkTaskKind::GroundedMetadata
                                | BenchmarkTaskKind::AvionicsGroundingReview
                        ) {
                            1
                        } else {
                            0
                        },
                        attempts: 1,
                        billable_usage_complete: true,
                        ..BenchmarkUsage::default()
                    },
                    error: None,
                }
            })
        }
    }

    #[tokio::test]
    async fn fake_runner_executes_offline_and_aggregates_every_model_task() {
        let suite = build_suite(
            vec![source_row(3, &gallery_html(), Some(parsed_listing_value()))],
            Vec::new(),
            &BenchmarkSelection {
                listing_limit: 1,
                ..BenchmarkSelection::default()
            },
        )
        .unwrap();
        let report = execute(
            &suite,
            &["gemini-3.1-flash-lite".to_string()],
            &FakeRunner,
            &BenchmarkPricing::official_standard_defaults(),
        )
        .await
        .unwrap();
        assert_eq!(report.runs.len(), suite.cases.len());
        assert_eq!(report.summaries.len(), 4);
        assert!(report
            .runs
            .iter()
            .all(|run| run.estimated_cost_usd.is_some()));
    }

    #[test]
    fn group_catalog_rows_preserves_multiple_types() {
        let rows = vec![
            CatalogRow {
                id: 1,
                manufacturer: "Garmin".to_string(),
                model: "GTN 750Xi".to_string(),
                catalog_status: "approved".to_string(),
                manufacturer_identifier_kind: Some("manufacturer_model_number".to_string()),
                manufacturer_identifier: Some("GTN750XI".to_string()),
                identity_source_url: Some("https://www.garmin.com/".to_string()),
                identity_confidence: Some("very_high".to_string()),
                avionics_type: Some("Navigation".to_string()),
            },
            CatalogRow {
                id: 1,
                manufacturer: "Garmin".to_string(),
                model: "GTN 750Xi".to_string(),
                catalog_status: "approved".to_string(),
                manufacturer_identifier_kind: Some("manufacturer_model_number".to_string()),
                manufacturer_identifier: Some("GTN750XI".to_string()),
                identity_source_url: Some("https://www.garmin.com/".to_string()),
                identity_confidence: Some("very_high".to_string()),
                avionics_type: Some("Communication".to_string()),
            },
        ];
        let grouped = group_catalog_rows(rows);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].avionics_types.len(), 2);
    }

    #[test]
    fn unknown_pricing_propagates_to_summary() {
        let run = BenchmarkRun {
            case_id: "case".to_string(),
            task: BenchmarkTaskKind::ListingExtraction,
            model: "private-model".to_string(),
            latency_ms: 10,
            output: None,
            usage: BenchmarkUsage {
                billable_usage_complete: true,
                ..BenchmarkUsage::default()
            },
            error: Some("offline fixture".to_string()),
            validation: failed_validation(None, None, "offline fixture"),
            estimated_cost_usd: None,
        };
        let summaries = summarize(&[run]);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].estimated_cost_usd, None);
    }

    #[test]
    fn custom_pricing_is_serializable_for_reproducible_runs() {
        let pricing = BenchmarkPricing {
            effective_date: "2099-01-01".to_string(),
            source_url: "https://example.test/pricing".to_string(),
            models: BTreeMap::from([(
                "test-model".to_string(),
                model_pricing(1.0, 0.1, 2.0, SearchBillingUnit::None, 0.0),
            )]),
        };
        let encoded = serde_json::to_string(&pricing).unwrap();
        let decoded: BenchmarkPricing = serde_json::from_str(&encoded).unwrap();
        assert_eq!(pricing, decoded);
    }
}
