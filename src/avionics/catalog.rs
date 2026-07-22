use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{
    AvionicsCatalogCandidate, AvionicsCatalogCollisionReviewContext, AvionicsProposedIdentity,
    AvionicsUnitResolutionCandidate, AvionicsUnitResolutionContext,
    AvionicsUnitResolutionCorrectionContext, GeminiGroundingSource, GeminiGroundingSupport,
    GeminiListingExtractor, CURATED_AVIONICS_TYPES,
};
use crate::normalize::{
    is_usable_avionics_label, normalize_avionics_identifier, normalize_avionics_manufacturer_name,
    normalize_avionics_model_name, normalize_name,
};

const CANDIDATE_LIMIT: usize = 16;
const COLLISION_CANDIDATE_LIMIT: usize = 32;
const CANDIDATE_MINIMUM_SCORE: f64 = 0.28;
const CATALOG_SELECT_SQL: &str = r#"
    SELECT
      model.id,
      mfr.name AS manufacturer,
      model.name AS model,
      capability_type.name AS capability_type,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier,
      model.catalog_status
    FROM avionics_models model
    JOIN avionics_manufacturers mfr
      ON mfr.id = model.avionics_manufacturer_id
    JOIN avionics_model_types model_type
      ON model_type.avionics_model_id = model.id
    JOIN avionics_types capability_type
      ON capability_type.id = model_type.avionics_type_id
    WHERE model.catalog_status IN ('approved', 'unreviewed')
    ORDER BY model.id, capability_type.normalized_name, capability_type.id
"#;

#[derive(Debug)]
pub enum CatalogError {
    Validation(String),
    Database(String),
    Gemini(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Database(message) | Self::Gemini(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<sqlx::Error> for CatalogError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

pub type CatalogResult<T> = Result<T, CatalogError>;

#[derive(Clone, Debug)]
pub struct AvionicsIdentityRequest {
    pub aircraft_manufacturer: String,
    pub aircraft_model: String,
    pub aircraft_variant: String,
    pub model_year: i64,
    pub source_url: String,
    pub listing_context: String,
    pub requires_listing_evidence: bool,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub quantity: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApprovedAvionicsIdentity {
    pub id: i64,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub manufacturer_identifier_kind: String,
    pub manufacturer_identifier: String,
    pub evidence_url: String,
    pub evidence_title: String,
    pub evidence: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AvionicsIdentityOutcome {
    Approved(ApprovedAvionicsIdentity),
    Rejected { reason: String },
    Unresolved { reason: String },
}

#[derive(Clone, Debug, FromRow)]
struct CatalogRow {
    id: i64,
    manufacturer: String,
    model: String,
    capability_type: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    catalog_status: String,
}

#[derive(Clone, Debug)]
struct VerifiedIdentity {
    canonical_manufacturer: String,
    canonical_model: String,
    canonical_types: Vec<String>,
    manufacturer_identifier_kind: String,
    manufacturer_identifier: String,
    identity_source_url: String,
    identity_source_title: String,
    identity_evidence: String,
    reason: String,
}

#[derive(Clone, Debug)]
struct CollisionReview {
    catalog_id: i64,
    decision: String,
    source_url: String,
    source_title: String,
    evidence: String,
    reason: String,
}

#[derive(Clone, Debug)]
struct ProposalAttestation {
    confirmed: bool,
    source_url: String,
    source_title: String,
    evidence: String,
    reason: String,
}

macro_rules! query_as_all {
    ($db:expr, $row:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_all(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_all(pool).await
            }
        }
    }};
}

/// Resolve one raw listing avionics label against the curated catalog.
///
/// Similarity only determines which existing identities Gemini must compare. It
/// never determines the outcome. A new or legacy-unreviewed identity is written
/// only after a separate Gemini call reviews every shortlisted collision.
pub async fn resolve_avionics_identity(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    request: &AvionicsIdentityRequest,
) -> CatalogResult<AvionicsIdentityOutcome> {
    resolve_avionics_identity_with_write_mode(db, extractor, request, true).await
}

/// Run the same grounded classification and independent collision review
/// without changing the catalog. New identities are returned with id `0`;
/// legacy identities that would be promoted retain their existing id.
pub async fn preview_avionics_identity(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    request: &AvionicsIdentityRequest,
) -> CatalogResult<AvionicsIdentityOutcome> {
    resolve_avionics_identity_with_write_mode(db, extractor, request, false).await
}

async fn resolve_avionics_identity_with_write_mode(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    request: &AvionicsIdentityRequest,
    persist: bool,
) -> CatalogResult<AvionicsIdentityOutcome> {
    if request.manufacturer.trim().is_empty() || request.model.trim().is_empty() {
        return Ok(AvionicsIdentityOutcome::Unresolved {
            reason: "candidate is missing a manufacturer or model label".to_string(),
        });
    }
    let mut seen_types = HashSet::new();
    let input_types = request
        .avionics_types
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .filter(|value| seen_types.insert(normalize_name(value)))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if input_types.is_empty() {
        return Ok(AvionicsIdentityOutcome::Unresolved {
            reason: "candidate is missing an avionics capability observation".to_string(),
        });
    }

    let catalog = load_catalog_candidates(db).await?;
    let catalog_snapshot = catalog_fingerprint(&catalog);
    let shortlist = shortlist_avionics_candidates(
        &request.manufacturer,
        &request.model,
        &input_types,
        None,
        &catalog,
    );
    let context = AvionicsUnitResolutionContext {
        aircraft_manufacturer: request.aircraft_manufacturer.clone(),
        aircraft_model: request.aircraft_model.clone(),
        aircraft_variant: request.aircraft_variant.clone(),
        model_year: request.model_year,
        source_url: request.source_url.clone(),
        listing_context: request.listing_context.clone(),
        requires_listing_evidence: request.requires_listing_evidence,
        candidate: AvionicsUnitResolutionCandidate {
            manufacturer: request.manufacturer.clone(),
            model: request.model.clone(),
            avionics_types: input_types,
            quantity: request.quantity.max(1),
        },
        catalog_candidates: shortlist.clone(),
    };

    let mut grounded_response =
        extractor
            .resolve_avionics_unit(&context)
            .await
            .map_err(|error| {
                CatalogError::Gemini(format!(
                    "Gemini avionics identity resolution failed: {error:#}"
                ))
            })?;
    let mut response = grounded_response.value;
    let mut issues = resolution_issues(
        &context,
        &response,
        grounded_response.google_search_used,
        &grounded_response.grounding_sources,
        &grounded_response.grounding_supports,
    );
    if !issues.is_empty() {
        grounded_response = extractor
            .correct_avionics_unit_resolution(
                &context,
                &response,
                &AvionicsUnitResolutionCorrectionContext {
                    issues,
                    secondary_check: None,
                },
            )
            .await
            .map_err(|error| {
                CatalogError::Gemini(format!(
                    "Gemini avionics identity correction failed: {error:#}"
                ))
            })?;
        response = grounded_response.value;
        issues = resolution_issues(
            &context,
            &response,
            grounded_response.google_search_used,
            &grounded_response.grounding_sources,
            &grounded_response.grounding_supports,
        );
    }
    if !issues.is_empty() {
        return Err(CatalogError::Validation(format!(
            "Gemini avionics identity response remained invalid after correction: {}",
            issues.join("; ")
        )));
    }

    let status = response["status"].as_str().unwrap_or_default();
    let reason = response["reason"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    match status {
        "reject" => Ok(AvionicsIdentityOutcome::Rejected { reason }),
        "unresolved" => Ok(AvionicsIdentityOutcome::Unresolved { reason }),
        "existing_match" => {
            let catalog_id = response["catalog_id"].as_i64().unwrap_or_default();
            let _selected = shortlist
                .iter()
                .find(|candidate| candidate.id == catalog_id)
                .ok_or_else(|| {
                    CatalogError::Validation(format!(
                        "Gemini selected unknown catalog id {catalog_id}"
                    ))
                })?;
            let proposed = verified_identity_from_response(&response)?;
            let collision_context = expanded_collision_context(&context, &proposed, &catalog);
            resolve_verified_identity(
                db,
                extractor,
                &collision_context,
                proposed,
                Some(catalog_id),
                &catalog_snapshot,
                persist,
            )
            .await
        }
        "propose_new" => {
            let proposed = verified_identity_from_response(&response)?;
            let collision_context = expanded_collision_context(&context, &proposed, &catalog);
            resolve_verified_identity(
                db,
                extractor,
                &collision_context,
                proposed,
                None,
                &catalog_snapshot,
                persist,
            )
            .await
        }
        _ => Err(CatalogError::Validation(format!(
            "unexpected Gemini avionics identity status: {status}"
        ))),
    }
}

async fn resolve_verified_identity(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    context: &AvionicsUnitResolutionContext,
    mut proposed: VerifiedIdentity,
    selected_existing_id: Option<i64>,
    reviewed_catalog_fingerprint: &str,
    persist: bool,
) -> CatalogResult<AvionicsIdentityOutcome> {
    let review_context = AvionicsCatalogCollisionReviewContext {
        classification_context: context.clone(),
        proposed_identity: AvionicsProposedIdentity {
            canonical_manufacturer: proposed.canonical_manufacturer.clone(),
            canonical_model: proposed.canonical_model.clone(),
            canonical_types: proposed.canonical_types.clone(),
            manufacturer_identifier_kind: proposed.manufacturer_identifier_kind.clone(),
            manufacturer_identifier: proposed.manufacturer_identifier.clone(),
        },
    };
    let review_response = extractor
        .review_avionics_catalog_collisions(&review_context)
        .await
        .map_err(|error| {
            CatalogError::Gemini(format!(
                "Gemini avionics collision review failed: {error:#}"
            ))
        })?;
    let attestation = proposal_attestation(
        context,
        &proposed,
        &review_response.value,
        &review_response.grounding_sources,
        &review_response.grounding_supports,
    )?;
    if !attestation.confirmed {
        return Ok(AvionicsIdentityOutcome::Unresolved {
            reason: attestation.reason,
        });
    }
    if !review_response.google_search_used {
        return Err(CatalogError::Validation(
            "confirmed Gemini proposal review did not return Google Search grounding metadata"
                .to_string(),
        ));
    }
    proposed.identity_source_url = attestation.source_url;
    proposed.identity_source_title = attestation.source_title;
    proposed.identity_evidence = attestation.evidence;
    proposed.reason = attestation.reason;
    let reviews = collision_reviews(
        context,
        &review_response.value,
        &review_response.grounding_sources,
        &review_response.grounding_supports,
    )?;
    let same_ids = reviews
        .iter()
        .filter(|review| review.decision == "same_product")
        .map(|review| review.catalog_id)
        .collect::<Vec<_>>();

    // A first-stage existing_match is still only a proposal. Require the
    // independent collision pass to confirm the selected row, regardless of
    // whether that row is already approved or remains legacy-unreviewed.
    if let Some(selected_id) = selected_existing_id {
        if !same_ids.contains(&selected_id) {
            return Ok(AvionicsIdentityOutcome::Unresolved {
                reason: format!(
                    "independent collision review did not confirm selected catalog id {selected_id} as the same product"
                ),
            });
        }
    }

    let approved_same = context
        .catalog_candidates
        .iter()
        .filter(|candidate| {
            same_ids.contains(&candidate.id) && candidate.catalog_status == "approved"
        })
        .collect::<Vec<_>>();
    if approved_same.len() > 1 {
        return Ok(AvionicsIdentityOutcome::Unresolved {
            reason: format!(
                "collision review found multiple approved identities for the same product: {:?}",
                approved_same
                    .iter()
                    .map(|candidate| candidate.id)
                    .collect::<Vec<_>>()
            ),
        });
    }
    if let Some(existing) = approved_same.first() {
        let review = reviews
            .iter()
            .find(|review| review.catalog_id == existing.id)
            .expect("approved same-product id came from collision reviews");
        let additions = match approved_capability_additions(existing, &proposed) {
            Ok(additions) => additions,
            Err(error) => {
                return Ok(AvionicsIdentityOutcome::Unresolved {
                    reason: error.to_string(),
                });
            }
        };
        if !additions.is_empty() {
            if !persist {
                return Ok(AvionicsIdentityOutcome::Approved(
                    approved_identity_from_verified(existing.id, &proposed),
                ));
            }
            let stored = persist_approved_capability_enrichment(
                db,
                existing,
                &proposed,
                reviewed_catalog_fingerprint,
            )
            .await?;
            return Ok(AvionicsIdentityOutcome::Approved(stored));
        }
        return Ok(AvionicsIdentityOutcome::Approved(
            ApprovedAvionicsIdentity {
                id: existing.id,
                manufacturer: existing.manufacturer.clone(),
                model: existing.model.clone(),
                avionics_types: existing.avionics_types.clone(),
                manufacturer_identifier_kind: existing.manufacturer_identifier_kind.clone(),
                manufacturer_identifier: existing.manufacturer_identifier.clone(),
                evidence_url: review.source_url.clone(),
                evidence_title: review.source_title.clone(),
                evidence: review.evidence.clone(),
                reason: review.reason.clone(),
            },
        ));
    }

    let target_id = selected_existing_id.or_else(|| same_ids.iter().copied().min());
    if !persist {
        return Ok(AvionicsIdentityOutcome::Approved(
            approved_identity_from_verified(target_id.unwrap_or(0), &proposed),
        ));
    }
    let stored = persist_approved_identity(
        db,
        target_id,
        &same_ids,
        &proposed,
        reviewed_catalog_fingerprint,
    )
    .await?;
    Ok(AvionicsIdentityOutcome::Approved(stored))
}

async fn load_catalog_candidates(db: &AppDb) -> CatalogResult<Vec<AvionicsCatalogCandidate>> {
    let rows = query_as_all!(db, CatalogRow, CATALOG_SELECT_SQL)?;
    Ok(catalog_candidates_from_rows(rows))
}

fn catalog_candidates_from_rows(rows: Vec<CatalogRow>) -> Vec<AvionicsCatalogCandidate> {
    let mut by_id = HashMap::<i64, AvionicsCatalogCandidate>::new();
    for row in rows
        .into_iter()
        .filter(|row| is_usable_avionics_label(&row.manufacturer, &row.model))
    {
        let capabilities = canonical_avionics_types_for_label(&row.capability_type);
        let candidate = by_id
            .entry(row.id)
            .or_insert_with(|| AvionicsCatalogCandidate {
                id: row.id,
                manufacturer: row.manufacturer,
                model: row.model,
                avionics_types: Vec::new(),
                manufacturer_identifier_kind: row.manufacturer_identifier_kind.unwrap_or_default(),
                manufacturer_identifier: row.manufacturer_identifier.unwrap_or_default(),
                catalog_status: row.catalog_status,
            });
        candidate
            .avionics_types
            .extend(capabilities.into_iter().map(str::to_string));
    }
    let mut catalog = by_id.into_values().collect::<Vec<_>>();
    for candidate in &mut catalog {
        candidate.avionics_types = canonicalize_avionics_types(&candidate.avionics_types);
    }
    catalog.sort_by_key(|candidate| candidate.id);
    catalog
}

/// Map common capability labels to the server-owned taxonomy. This mapping is
/// used only for catalog loading and similarity retrieval. It must never be
/// used to turn a raw listing label into a product identity.
fn canonical_avionics_types_for_label(value: &str) -> Vec<&'static str> {
    let key = normalize_name(value);
    if let Some(canonical) = CURATED_AVIONICS_TYPES
        .iter()
        .copied()
        .find(|canonical| normalize_name(canonical) == key)
    {
        return vec![canonical];
    }
    match key.as_str() {
        "comm" | "communications radio" | "communication radio" => vec!["COM"],
        "navigation receiver" | "nav receiver" => vec!["NAV"],
        "nav com"
        | "nav comm"
        | "navcom"
        | "navigation communication"
        | "navigation communications" => {
            vec!["NAV", "COM"]
        }
        "automatic direction finder" => vec!["ADF"],
        "distance measuring equipment" | "distance measurement equipment" => vec!["DME"],
        "attitude heading reference system" | "attitude and heading reference system" => {
            vec!["AHRS"]
        }
        "adc" | "air data unit" => vec!["Air Data Computer"],
        "emergency locator transmitter" | "emergency locator beacon" => vec!["ELT"],
        "cdi"
        | "hsi"
        | "cdi hsi"
        | "course deviation indicator"
        | "horizontal situation indicator" => vec!["Navigation Indicator"],
        "stormscope" | "lightning detector" | "lightning detection system" => {
            vec!["Lightning Detection"]
        }
        "radio altimeter" => vec!["Radar Altimeter"],
        "clock" | "timer" | "chronometer" => vec!["Clock/Timer"],
        "taws" | "egpws" | "terrain awareness and warning system" => {
            vec!["Terrain Awareness"]
        }
        _ => Vec::new(),
    }
}

fn retrieval_avionics_type_keys(value: &str) -> Vec<String> {
    let canonical = canonical_avionics_types_for_label(value);
    if canonical.is_empty() {
        vec![normalize_name(value)]
    } else {
        canonical.into_iter().map(normalize_name).collect()
    }
}

fn canonicalize_avionics_types(values: &[String]) -> Vec<String> {
    let present = values
        .iter()
        .flat_map(|value| canonical_avionics_types_for_label(value))
        .collect::<HashSet<_>>();
    CURATED_AVIONICS_TYPES
        .iter()
        .copied()
        .filter(|value| present.contains(value))
        .map(str::to_string)
        .collect()
}

fn approved_capability_additions(
    existing: &AvionicsCatalogCandidate,
    proposed: &VerifiedIdentity,
) -> CatalogResult<Vec<String>> {
    for (field, actual, expected) in [
        (
            "canonical_manufacturer",
            proposed.canonical_manufacturer.as_str(),
            existing.manufacturer.as_str(),
        ),
        (
            "canonical_model",
            proposed.canonical_model.as_str(),
            existing.model.as_str(),
        ),
        (
            "manufacturer_identifier_kind",
            proposed.manufacturer_identifier_kind.as_str(),
            existing.manufacturer_identifier_kind.as_str(),
        ),
        (
            "manufacturer_identifier",
            proposed.manufacturer_identifier.as_str(),
            existing.manufacturer_identifier.as_str(),
        ),
    ] {
        if actual != expected {
            return Err(CatalogError::Validation(format!(
                "approved capability enrichment must preserve {field} exactly"
            )));
        }
    }

    let proposed_types = proposed
        .canonical_types
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if let Some(missing) = existing
        .avionics_types
        .iter()
        .find(|capability| !proposed_types.contains(capability.as_str()))
    {
        return Err(CatalogError::Validation(format!(
            "approved capability enrichment cannot remove stored capability {missing:?}"
        )));
    }
    let existing_types = existing
        .avionics_types
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    Ok(proposed
        .canonical_types
        .iter()
        .filter(|capability| !existing_types.contains(capability.as_str()))
        .cloned()
        .collect())
}

fn canonical_types_from_response(response: &Value, field: &str) -> CatalogResult<Vec<String>> {
    let values = response
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CatalogError::Validation(format!(
                "Gemini avionics identity response requires {field} to be an array"
            ))
        })?;
    let mut present = HashSet::new();
    for value in values {
        let capability = value.as_str().map(str::trim).ok_or_else(|| {
            CatalogError::Validation(format!(
                "Gemini avionics identity response {field} must contain only strings"
            ))
        })?;
        if !CURATED_AVIONICS_TYPES.contains(&capability) {
            return Err(CatalogError::Validation(format!(
                "unsupported canonical avionics capability {capability:?}"
            )));
        }
        present.insert(capability);
    }
    Ok(CURATED_AVIONICS_TYPES
        .iter()
        .copied()
        .filter(|capability| present.contains(capability))
        .map(str::to_string)
        .collect())
}

fn catalog_fingerprint(catalog: &[AvionicsCatalogCandidate]) -> String {
    let mut hasher = Sha256::new();
    for row in catalog {
        for value in [
            row.id.to_string(),
            row.manufacturer.clone(),
            row.model.clone(),
            row.avionics_types.join("\u{1f}"),
            row.manufacturer_identifier_kind.clone(),
            row.manufacturer_identifier.clone(),
            row.catalog_status.clone(),
        ] {
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

fn shortlist_avionics_candidates(
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    manufacturer_identifier: Option<&str>,
    catalog: &[AvionicsCatalogCandidate],
) -> Vec<AvionicsCatalogCandidate> {
    let raw_identity = format!(
        "{manufacturer} {model} {} {}",
        avionics_types.join(" "),
        manufacturer_identifier.unwrap_or_default()
    );
    let raw_identifier = normalize_avionics_identifier(&raw_identity);
    let manufacturer_key = normalize_avionics_manufacturer_name(manufacturer);
    let model_key = normalize_avionics_model_name(model);
    let type_keys = avionics_types
        .iter()
        .flat_map(|value| retrieval_avionics_type_keys(value))
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    let mut scored = catalog
        .iter()
        .filter_map(|candidate| {
            let candidate_model = normalize_avionics_model_name(&candidate.model);
            let model_score = string_similarity(&model_key, &candidate_model);
            let manufacturer_match =
                normalize_avionics_manufacturer_name(&candidate.manufacturer) == manufacturer_key;
            // Capability similarity affects retrieval rank only. It is never
            // a product identity key or evidence for a same-product decision.
            let type_match = candidate
                .avionics_types
                .iter()
                .flat_map(|value| retrieval_avionics_type_keys(value))
                .any(|value| type_keys.contains(&value));
            let identifier = normalize_avionics_identifier(&candidate.manufacturer_identifier);
            let identifier_match = !identifier.is_empty()
                && (raw_identifier.contains(&identifier) || identifier.contains(&raw_identifier));
            let score = model_score
                + if manufacturer_match { 0.18 } else { 0.0 }
                + if type_match { 0.08 } else { 0.0 }
                + if identifier_match { 0.75 } else { 0.0 };
            (score >= CANDIDATE_MINIMUM_SCORE || identifier_match).then_some((
                score,
                candidate.id,
                candidate.clone(),
            ))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
    });
    scored
        .into_iter()
        .take(CANDIDATE_LIMIT)
        .map(|(_, _, candidate)| candidate)
        .collect()
}

fn expanded_collision_context(
    classification_context: &AvionicsUnitResolutionContext,
    proposed: &VerifiedIdentity,
    catalog: &[AvionicsCatalogCandidate],
) -> AvionicsUnitResolutionContext {
    // Grounding often supplies a clean canonical label and official part
    // number that were absent from the listing. Re-run retrieval with that new
    // evidence before the independent collision decision.
    let mut candidates = shortlist_avionics_candidates(
        &proposed.canonical_manufacturer,
        &proposed.canonical_model,
        &proposed.canonical_types,
        Some(&proposed.manufacturer_identifier),
        catalog,
    );
    for candidate in &classification_context.catalog_candidates {
        if candidates.iter().all(|item| item.id != candidate.id) {
            candidates.push(candidate.clone());
        }
    }
    candidates.truncate(COLLISION_CANDIDATE_LIMIT);
    let mut context = classification_context.clone();
    context.catalog_candidates = candidates;
    context
}

fn string_similarity(left: &str, right: &str) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    if left == right {
        return 1.0;
    }
    let left_tokens = left.split_whitespace().collect::<HashSet<_>>();
    let right_tokens = right.split_whitespace().collect::<HashSet<_>>();
    let intersection = left_tokens.intersection(&right_tokens).count() as f64;
    let union = left_tokens.union(&right_tokens).count() as f64;
    let token_score = if union > 0.0 {
        intersection / union
    } else {
        0.0
    };
    token_score.max(bigram_dice(left, right))
}

fn bigram_dice(left: &str, right: &str) -> f64 {
    fn bigrams(value: &str) -> HashMap<(char, char), usize> {
        let characters = value.chars().collect::<Vec<_>>();
        let mut output = HashMap::new();
        for window in characters.windows(2) {
            *output.entry((window[0], window[1])).or_insert(0) += 1;
        }
        output
    }
    if left.len() < 2 || right.len() < 2 {
        return f64::from(left == right);
    }
    let left_bigrams = bigrams(left);
    let right_bigrams = bigrams(right);
    let overlap = left_bigrams
        .iter()
        .map(|(bigram, count)| count.min(right_bigrams.get(bigram).unwrap_or(&0)))
        .sum::<usize>();
    let total = left_bigrams.values().sum::<usize>() + right_bigrams.values().sum::<usize>();
    2.0 * overlap as f64 / total as f64
}

fn resolution_issues(
    context: &AvionicsUnitResolutionContext,
    response: &Value,
    google_search_used: bool,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
) -> Vec<String> {
    let mut issues = Vec::new();
    let status = string_field(response, "status");
    let catalog_id = response
        .get("catalog_id")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let confidence = string_field(response, "confidence");
    let reason = string_field(response, "reason");
    if !matches!(confidence, "very_high" | "high" | "medium" | "low") {
        issues.push("confidence must be very_high, high, medium, or low".to_string());
    }
    if reason.is_empty() {
        issues.push("reason must be non-empty".to_string());
    }
    match status {
        "existing_match" => {
            if !google_search_used {
                issues.push(
                    "existing_match requires Gemini Google Search grounding metadata".to_string(),
                );
            }
            let selected = context
                .catalog_candidates
                .iter()
                .find(|candidate| candidate.id == catalog_id);
            let Some(selected) = selected else {
                issues.push("existing_match must select one supplied catalog id".to_string());
                return issues;
            };
            if !matches!(confidence, "very_high" | "high") {
                issues.push("existing_match requires high or very_high confidence".to_string());
            }
            if selected.catalog_status == "unreviewed" && confidence != "very_high" {
                issues
                    .push("an unreviewed existing_match requires very_high confidence".to_string());
            }
            if selected.catalog_status == "approved" {
                for (field, expected) in [
                    ("canonical_manufacturer", selected.manufacturer.as_str()),
                    ("canonical_model", selected.model.as_str()),
                    (
                        "manufacturer_identifier_kind",
                        selected.manufacturer_identifier_kind.as_str(),
                    ),
                    (
                        "manufacturer_identifier",
                        selected.manufacturer_identifier.as_str(),
                    ),
                ] {
                    if string_field(response, field) != expected {
                        issues.push(format!(
                            "approved existing_match must repeat {field} exactly"
                        ));
                    }
                }
                match canonical_types_from_response(response, "canonical_types") {
                    Ok(types) => {
                        let returned = types.iter().map(String::as_str).collect::<HashSet<_>>();
                        if let Some(missing) = selected
                            .avionics_types
                            .iter()
                            .find(|capability| !returned.contains(capability.as_str()))
                        {
                            issues.push(format!(
                                "approved existing_match cannot remove stored capability {missing:?}"
                            ));
                        }
                        let stored = selected
                            .avionics_types
                            .iter()
                            .map(String::as_str)
                            .collect::<HashSet<_>>();
                        for observed in
                            canonicalize_avionics_types(&context.candidate.avionics_types)
                                .into_iter()
                                .filter(|capability| !stored.contains(capability.as_str()))
                        {
                            if !returned.contains(observed.as_str()) {
                                issues.push(format!(
                                    "approved existing_match must include the verified newly observed capability {observed:?} or return unresolved"
                                ));
                            }
                        }
                    }
                    Err(error) => issues.push(error.to_string()),
                }
            } else {
                if let Err(error) = verified_identity_from_response(response) {
                    issues.push(error.to_string());
                }
                let old_identifier =
                    normalize_avionics_identifier(&selected.manufacturer_identifier);
                let proposed_identifier = normalize_avionics_identifier(string_field(
                    response,
                    "manufacturer_identifier",
                ));
                if !old_identifier.is_empty() && old_identifier != proposed_identifier {
                    issues.push(
                        "unreviewed existing_match cannot overwrite a conflicting legacy manufacturer identifier"
                            .to_string(),
                    );
                }
                if !selected.manufacturer_identifier_kind.is_empty()
                    && selected.manufacturer_identifier_kind != "none"
                    && string_field(response, "manufacturer_identifier_kind")
                        != selected.manufacturer_identifier_kind
                {
                    issues.push(
                        "unreviewed existing_match cannot change the kind of a non-empty legacy manufacturer identifier"
                            .to_string(),
                    );
                }
            }
            validate_authoritative_evidence(
                response,
                &context.source_url,
                grounding_sources,
                grounding_supports,
                &mut issues,
            );
        }
        "propose_new" => {
            if !google_search_used {
                issues.push(
                    "propose_new requires Gemini Google Search grounding metadata".to_string(),
                );
            }
            if catalog_id != 0 {
                issues.push("propose_new must use catalog_id=0".to_string());
            }
            if confidence != "very_high" {
                issues.push("propose_new requires very_high confidence".to_string());
            }
            if let Err(error) = verified_identity_from_response(response) {
                issues.push(error.to_string());
            }
            validate_authoritative_evidence(
                response,
                &context.source_url,
                grounding_sources,
                grounding_supports,
                &mut issues,
            );
        }
        "reject" | "unresolved" => {
            if catalog_id != 0 {
                issues.push(format!("{status} must use catalog_id=0"));
            }
            if status == "reject" && !matches!(confidence, "very_high" | "high") {
                issues.push("reject requires high or very_high confidence".to_string());
            }
            for field in [
                "canonical_manufacturer",
                "canonical_model",
                "manufacturer_identifier",
                "identity_source_url",
                "identity_source_title",
                "identity_evidence",
            ] {
                if !string_field(response, field).is_empty() {
                    issues.push(format!("{status} must leave {field} empty"));
                }
            }
            match canonical_types_from_response(response, "canonical_types") {
                Ok(types) if types.is_empty() => {}
                Ok(_) => issues.push(format!("{status} must leave canonical_types empty")),
                Err(error) => issues.push(error.to_string()),
            }
            if string_field(response, "manufacturer_identifier_kind") != "none" {
                issues.push(format!(
                    "{status} must use manufacturer_identifier_kind=none"
                ));
            }
        }
        _ => issues
            .push("status must be existing_match, propose_new, reject, or unresolved".to_string()),
    }
    issues
}

fn verified_identity_from_response(response: &Value) -> CatalogResult<VerifiedIdentity> {
    let identity = VerifiedIdentity {
        canonical_manufacturer: required_field(response, "canonical_manufacturer")?,
        canonical_model: required_field(response, "canonical_model")?,
        canonical_types: canonical_types_from_response(response, "canonical_types")?,
        manufacturer_identifier_kind: required_field(response, "manufacturer_identifier_kind")?,
        manufacturer_identifier: required_field(response, "manufacturer_identifier")?,
        identity_source_url: required_field(response, "identity_source_url")?,
        identity_source_title: required_field(response, "identity_source_title")?,
        identity_evidence: required_field(response, "identity_evidence")?,
        reason: required_field(response, "reason")?,
    };
    if !matches!(
        identity.manufacturer_identifier_kind.as_str(),
        "manufacturer_part_number" | "manufacturer_model_number" | "sku"
    ) {
        return Err(CatalogError::Validation(
            "verified identity requires manufacturer_part_number, manufacturer_model_number, or sku"
                .to_string(),
        ));
    }
    if !is_usable_avionics_label(&identity.canonical_manufacturer, &identity.canonical_model) {
        return Err(CatalogError::Validation(
            "verified identity must name one concrete manufacturer and model".to_string(),
        ));
    }
    if identity.canonical_types.is_empty() {
        return Err(CatalogError::Validation(
            "verified identity requires at least one canonical avionics capability".to_string(),
        ));
    }
    let normalized_model = normalize_avionics_model_name(&identity.canonical_model);
    if identity
        .canonical_types
        .iter()
        .any(|capability| normalized_model == normalize_name(capability))
        || normalized_model
            .split_whitespace()
            .any(|token| matches!(token, "series" | "family"))
        || combines_multiple_model_numbers(&identity.canonical_model)
    {
        return Err(CatalogError::Validation(
            "verified identity must describe one exact product or suite generation, not a class, family, series, or combined model label"
                .to_string(),
        ));
    }
    if normalize_avionics_identifier(&identity.manufacturer_identifier).is_empty() {
        return Err(CatalogError::Validation(
            "verified identity has an unusable manufacturer identifier".to_string(),
        ));
    }
    Ok(identity)
}

fn combines_multiple_model_numbers(value: &str) -> bool {
    if !value.contains('/') {
        return false;
    }
    let mut numeric_groups = 0;
    let mut in_digits = false;
    for character in value.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                numeric_groups += 1;
                in_digits = true;
            }
        } else {
            in_digits = false;
        }
    }
    numeric_groups > 1
}

fn proposal_attestation(
    context: &AvionicsUnitResolutionContext,
    proposed: &VerifiedIdentity,
    response: &Value,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
) -> CatalogResult<ProposalAttestation> {
    for (field, expected) in [
        (
            "canonical_manufacturer",
            proposed.canonical_manufacturer.as_str(),
        ),
        ("canonical_model", proposed.canonical_model.as_str()),
        (
            "manufacturer_identifier_kind",
            proposed.manufacturer_identifier_kind.as_str(),
        ),
        (
            "manufacturer_identifier",
            proposed.manufacturer_identifier.as_str(),
        ),
    ] {
        if string_field(response, field) != expected {
            return Err(CatalogError::Validation(format!(
                "independent proposal review must repeat {field} exactly"
            )));
        }
    }
    if canonical_types_from_response(response, "canonical_types")? != proposed.canonical_types {
        return Err(CatalogError::Validation(
            "independent proposal review must repeat canonical_types exactly".to_string(),
        ));
    }
    let decision = required_field(response, "proposal_decision")?;
    let reason = required_field(response, "proposal_reason")?;
    if decision == "not_confirmed" {
        return Ok(ProposalAttestation {
            confirmed: false,
            source_url: String::new(),
            source_title: String::new(),
            evidence: String::new(),
            reason,
        });
    }
    if decision != "confirmed_same_as_input" {
        return Err(CatalogError::Validation(format!(
            "unexpected proposal_decision {decision}"
        )));
    }
    if string_field(response, "proposal_confidence") != "very_high" {
        return Err(CatalogError::Validation(
            "confirmed proposal review requires very_high confidence".to_string(),
        ));
    }
    if !is_usable_avionics_label(&context.candidate.manufacturer, &context.candidate.model) {
        return Err(CatalogError::Validation(
            "generic raw listing labels cannot receive a confirmed catalog identity".to_string(),
        ));
    }
    if context.requires_listing_evidence {
        let input_evidence = required_field(response, "input_evidence_text")?;
        if !context.listing_context.contains(&input_evidence) {
            return Err(CatalogError::Validation(
                "input_evidence_text must be copied exactly from listing_context".to_string(),
            ));
        }
        let input_key = normalize_avionics_model_name(&input_evidence);
        let raw_model_key = normalize_avionics_model_name(&context.candidate.model);
        if raw_model_key.is_empty() || !input_key.contains(&raw_model_key) {
            return Err(CatalogError::Validation(
                "listing input evidence does not contain the discriminating raw model label"
                    .to_string(),
            ));
        }
    }

    let source_url = required_field(response, "proposal_source_url")?;
    let source_title = required_field(response, "proposal_source_title")?;
    let evidence = required_field(response, "proposal_evidence")?;
    let mut issues = Vec::new();
    validate_evidence_values(
        &source_url,
        &source_title,
        &evidence,
        &context.source_url,
        grounding_sources,
        grounding_supports,
        &mut issues,
    );
    if !issues.is_empty() {
        return Err(CatalogError::Validation(format!(
            "independent proposal attestation lacks grounded authoritative evidence: {}",
            issues.join("; ")
        )));
    }
    Ok(ProposalAttestation {
        confirmed: true,
        source_url,
        source_title,
        evidence,
        reason,
    })
}

fn collision_reviews(
    context: &AvionicsUnitResolutionContext,
    response: &Value,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
) -> CatalogResult<Vec<CollisionReview>> {
    let values = response
        .get("reviews")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CatalogError::Validation("collision response missing reviews".to_string())
        })?;
    let expected = context
        .catalog_candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut reviews = Vec::with_capacity(values.len());
    for value in values {
        let catalog_id = value
            .get("catalog_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                CatalogError::Validation(
                    "collision review catalog_id must be an integer".to_string(),
                )
            })?;
        if !expected.contains(&catalog_id) {
            return Err(CatalogError::Validation(format!(
                "collision review returned unknown catalog id {catalog_id}"
            )));
        }
        if !seen.insert(catalog_id) {
            return Err(CatalogError::Validation(format!(
                "collision review repeated catalog id {catalog_id}"
            )));
        }
        let decision = required_field(value, "decision")?;
        if !matches!(decision.as_str(), "same_product" | "different_product") {
            return Err(CatalogError::Validation(format!(
                "collision review {catalog_id} has invalid decision {decision}"
            )));
        }
        if string_field(value, "confidence") != "very_high" {
            return Err(CatalogError::Validation(format!(
                "collision review {catalog_id} must have very_high confidence before catalog storage"
            )));
        }
        let source_url = required_field(value, "source_url")?;
        let source_title = required_field(value, "source_title")?;
        let evidence = required_field(value, "evidence")?;
        let reason = required_field(value, "reason")?;
        let mut issues = Vec::new();
        validate_evidence_values(
            &source_url,
            &source_title,
            &evidence,
            &context.source_url,
            grounding_sources,
            grounding_supports,
            &mut issues,
        );
        if !issues.is_empty() {
            return Err(CatalogError::Validation(format!(
                "collision review {catalog_id} lacks authoritative evidence: {}",
                issues.join("; ")
            )));
        }
        reviews.push(CollisionReview {
            catalog_id,
            decision,
            source_url,
            source_title,
            evidence,
            reason,
        });
    }
    if seen != expected {
        let mut missing = expected.difference(&seen).copied().collect::<Vec<_>>();
        missing.sort_unstable();
        return Err(CatalogError::Validation(format!(
            "collision review omitted shortlisted catalog ids {missing:?}"
        )));
    }
    Ok(reviews)
}

fn validate_authoritative_evidence(
    response: &Value,
    listing_source_url: &str,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
    issues: &mut Vec<String>,
) {
    validate_evidence_values(
        string_field(response, "identity_source_url"),
        string_field(response, "identity_source_title"),
        string_field(response, "identity_evidence"),
        listing_source_url,
        grounding_sources,
        grounding_supports,
        issues,
    );
}

fn validate_evidence_values(
    source_url: &str,
    source_title: &str,
    evidence: &str,
    listing_source_url: &str,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
    issues: &mut Vec<String>,
) {
    let parsed = url::Url::parse(source_url).ok();
    if parsed
        .as_ref()
        .is_none_or(|url| !matches!(url.scheme(), "http" | "https"))
    {
        issues.push("identity source must be an http(s) URL".to_string());
    }
    let lowered = source_url.to_ascii_lowercase();
    if [
        "/listing/",
        "/listings/",
        "/aircraft-for-sale/",
        "/classifieds/",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
    {
        issues.push("ordinary sale listings are not authoritative identity evidence".to_string());
    }
    let host = parsed
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or_default()
        .trim_start_matches("www.");
    if [
        "ebay.com",
        "amazon.com",
        "facebook.com",
        "craigslist.org",
        "controller.com",
        "trade-a-plane.com",
        "barnstormers.com",
        "aircraft.com",
        "globalair.com",
    ]
    .iter()
    .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        issues.push(
            "marketplace or broker pages are not authoritative identity evidence".to_string(),
        );
    }
    if !listing_source_url.trim().is_empty() && source_url.trim() == listing_source_url.trim() {
        issues.push("listing source URL cannot also be identity evidence".to_string());
    }
    if source_title.trim().chars().count() < 4 {
        issues.push("identity source title must be specific and non-empty".to_string());
    }
    if evidence.trim().chars().count() < 20 {
        issues.push("identity evidence must contain a specific supporting fact".to_string());
    }
    if !evidence_is_bound_to_grounding(
        source_url,
        source_title,
        evidence,
        grounding_sources,
        grounding_supports,
    ) {
        issues.push(
            "identity evidence must be linked by Gemini grounding support to the claimed web source"
                .to_string(),
        );
    }
}

fn evidence_is_bound_to_grounding(
    source_url: &str,
    source_title: &str,
    evidence: &str,
    grounding_sources: &[GeminiGroundingSource],
    grounding_supports: &[GeminiGroundingSupport],
) -> bool {
    let evidence_key = normalize_name(evidence);
    if evidence_key.len() < 12 {
        return false;
    }
    grounding_supports.iter().any(|support| {
        let support_key = normalize_name(&support.text);
        let supports_claim = support_key.contains(&evidence_key)
            || evidence_key.contains(&support_key)
            || string_similarity(&support_key, &evidence_key) >= 0.4;
        supports_claim
            && support.source_indices.iter().any(|source_index| {
                grounding_sources.iter().any(|source| {
                    source.chunk_index == *source_index
                        && grounding_source_matches_claim(source, source_url, source_title)
                })
            })
    })
}

fn grounding_source_matches_claim(
    source: &GeminiGroundingSource,
    claimed_url: &str,
    claimed_title: &str,
) -> bool {
    if normalized_evidence_url(&source.url)
        .zip(normalized_evidence_url(claimed_url))
        .is_some_and(|(source, claimed)| source == claimed)
    {
        return true;
    }
    let source_title = normalize_name(&source.title);
    let claimed_title = normalize_name(claimed_title);
    let shared_title_tokens = source_title
        .split_whitespace()
        .collect::<HashSet<_>>()
        .intersection(&claimed_title.split_whitespace().collect::<HashSet<_>>())
        .count();
    shared_title_tokens >= 2 && string_similarity(&source_title, &claimed_title) >= 0.45
}

fn normalized_evidence_url(value: &str) -> Option<String> {
    let mut parsed = url::Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.set_fragment(None);
    parsed.set_query(None);
    let normalized_path = parsed.path().trim_end_matches('/').to_string();
    parsed.set_path(if normalized_path.is_empty() {
        "/"
    } else {
        &normalized_path
    });
    Some(parsed.to_string())
}

fn approved_identity_from_verified(
    id: i64,
    identity: &VerifiedIdentity,
) -> ApprovedAvionicsIdentity {
    ApprovedAvionicsIdentity {
        id,
        manufacturer: identity.canonical_manufacturer.clone(),
        model: identity.canonical_model.clone(),
        avionics_types: identity.canonical_types.clone(),
        manufacturer_identifier_kind: identity.manufacturer_identifier_kind.clone(),
        manufacturer_identifier: identity.manufacturer_identifier.clone(),
        evidence_url: identity.identity_source_url.clone(),
        evidence_title: identity.identity_source_title.clone(),
        evidence: identity.identity_evidence.clone(),
        reason: identity.reason.clone(),
    }
}

async fn persist_approved_capability_enrichment(
    db: &AppDb,
    reviewed_existing: &AvionicsCatalogCandidate,
    identity: &VerifiedIdentity,
    reviewed_catalog_fingerprint: &str,
) -> CatalogResult<ApprovedAvionicsIdentity> {
    let reviewed_additions = approved_capability_additions(reviewed_existing, identity)?;
    if reviewed_additions.is_empty() {
        return Ok(approved_identity_from_verified(
            reviewed_existing.id,
            identity,
        ));
    }
    let catalog_lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            "LOCK TABLE avionics_models, avionics_model_types, avionics_types, avionics_manufacturers IN SHARE ROW EXCLUSIVE MODE",
        ),
    };

    macro_rules! enrich {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&catalog_lock_sql)
                .execute(&mut *transaction)
                .await?;

            let catalog_select_sql = db.sql(CATALOG_SELECT_SQL);
            let current_rows = sqlx::query_as::<_, CatalogRow>(&catalog_select_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let current_catalog = catalog_candidates_from_rows(current_rows);
            if catalog_fingerprint(&current_catalog) != reviewed_catalog_fingerprint {
                return Err(CatalogError::Validation(
                    "avionics catalog changed during Gemini capability review; retry against the current catalog"
                        .to_string(),
                ));
            }
            let current = current_catalog
                .iter()
                .find(|candidate| candidate.id == reviewed_existing.id)
                .ok_or_else(|| {
                    CatalogError::Validation(format!(
                        "approved catalog id {} disappeared during capability review",
                        reviewed_existing.id
                    ))
                })?;
            if current.catalog_status != "approved" {
                return Err(CatalogError::Validation(format!(
                    "catalog id {} is no longer approved; retry capability review",
                    reviewed_existing.id
                )));
            }
            let additions = approved_capability_additions(current, identity)?;
            if additions.is_empty() {
                return Err(CatalogError::Validation(
                    "approved catalog capabilities changed during Gemini review; retry against the current catalog"
                        .to_string(),
                ));
            }

            for capability in &additions {
                let type_key = normalize_name(capability);
                let insert_type = db.sql(
                    "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
                );
                sqlx::query(&insert_type)
                    .bind(capability.as_str())
                    .bind(type_key.as_str())
                    .execute(&mut *transaction)
                    .await?;
                let select_type =
                    db.sql("SELECT id FROM avionics_types WHERE normalized_name = ?");
                let type_id: i64 = sqlx::query_scalar(&select_type)
                    .bind(type_key.as_str())
                    .fetch_one(&mut *transaction)
                    .await?;
                let insert_membership = db.sql(
                    "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?) ON CONFLICT (avionics_model_id, avionics_type_id) DO NOTHING",
                );
                sqlx::query(&insert_membership)
                    .bind(reviewed_existing.id)
                    .bind(type_id)
                    .execute(&mut *transaction)
                    .await?;
            }

            let touch_model = db.sql(
                "UPDATE avionics_models SET updated_at = CURRENT_TIMESTAMP WHERE id = ? AND catalog_status = 'approved'",
            );
            let touched = sqlx::query(&touch_model)
                .bind(reviewed_existing.id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if touched != 1 {
                return Err(CatalogError::Validation(
                    "approved catalog identity changed during capability enrichment".to_string(),
                ));
            }
            transaction.commit().await?;
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => enrich!(pool),
        DatabaseBackend::Postgres(pool) => enrich!(pool),
    }
    Ok(approved_identity_from_verified(
        reviewed_existing.id,
        identity,
    ))
}

async fn persist_approved_identity(
    db: &AppDb,
    requested_target_id: Option<i64>,
    confirmed_same_ids: &[i64],
    identity: &VerifiedIdentity,
    reviewed_catalog_fingerprint: &str,
) -> CatalogResult<ApprovedAvionicsIdentity> {
    if identity.canonical_types.is_empty() {
        return Err(CatalogError::Validation(
            "cannot persist an avionics product without a canonical capability".to_string(),
        ));
    }
    let manufacturer_key = normalize_avionics_manufacturer_name(&identity.canonical_manufacturer);
    let model_key = normalize_avionics_model_name(&identity.canonical_model);
    let identifier_key = normalize_avionics_identifier(&identity.manufacturer_identifier);
    let allowed_ids = confirmed_same_ids.iter().copied().collect::<HashSet<_>>();
    let catalog_lock_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE avionics_models SET updated_at = updated_at WHERE id = (SELECT id FROM avionics_models ORDER BY id LIMIT 1)",
        ),
        DatabaseBackend::Postgres(_) => {
            db.sql(
                "LOCK TABLE avionics_models, avionics_model_types, avionics_types, avionics_manufacturers IN SHARE ROW EXCLUSIVE MODE",
            )
        }
    };

    macro_rules! persist {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            // Serialize catalog approvals, then prove that Gemini reviewed the
            // same active identity snapshot that is about to be mutated.
            sqlx::query(&catalog_lock_sql)
                .execute(&mut *transaction)
                .await?;
            let catalog_select_sql = db.sql(CATALOG_SELECT_SQL);
            let current_rows = sqlx::query_as::<_, CatalogRow>(&catalog_select_sql)
                .fetch_all(&mut *transaction)
                .await?;
            let current_fingerprint =
                catalog_fingerprint(&catalog_candidates_from_rows(current_rows));
            if current_fingerprint != reviewed_catalog_fingerprint {
                return Err(CatalogError::Validation(
                    "avionics catalog changed during Gemini review; retry against the current catalog"
                        .to_string(),
                ));
            }
            let insert_manufacturer = db.sql(
                "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
            );
            sqlx::query(&insert_manufacturer)
                .bind(identity.canonical_manufacturer.trim())
                .bind(manufacturer_key.as_str())
                .execute(&mut *transaction)
                .await?;
            let select_manufacturer = db.sql(
                "SELECT id FROM avionics_manufacturers WHERE normalized_name = ?",
            );
            let manufacturer_id: i64 = sqlx::query_scalar(&select_manufacturer)
                .bind(manufacturer_key.as_str())
                .fetch_one(&mut *transaction)
                .await?;

            let mut type_ids = Vec::with_capacity(identity.canonical_types.len());
            for canonical_type in &identity.canonical_types {
                let type_key = normalize_name(canonical_type);
                let insert_type = db.sql(
                    "INSERT INTO avionics_types (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING",
                );
                sqlx::query(&insert_type)
                    .bind(canonical_type.trim())
                    .bind(type_key.as_str())
                    .execute(&mut *transaction)
                    .await?;
                let select_type =
                    db.sql("SELECT id FROM avionics_types WHERE normalized_name = ?");
                let type_id: i64 = sqlx::query_scalar(&select_type)
                    .bind(type_key.as_str())
                    .fetch_one(&mut *transaction)
                    .await?;
                type_ids.push(type_id);
            }

            let select_identifier_collision = db.sql(
                "SELECT id FROM avionics_models WHERE avionics_manufacturer_id = ? AND normalized_manufacturer_identifier = ? ORDER BY id LIMIT 1",
            );
            let identifier_collision: Option<i64> = sqlx::query_scalar(&select_identifier_collision)
                .bind(manufacturer_id)
                .bind(identifier_key.as_str())
                .fetch_optional(&mut *transaction)
                .await?;
            let select_name_collision = db.sql(
                "SELECT id FROM avionics_models WHERE avionics_manufacturer_id = ? AND normalized_name = ? ORDER BY id LIMIT 1",
            );
            let name_collision: Option<i64> = sqlx::query_scalar(&select_name_collision)
                .bind(manufacturer_id)
                .bind(model_key.as_str())
                .fetch_optional(&mut *transaction)
                .await?;

            for collision in [identifier_collision, name_collision].into_iter().flatten() {
                if Some(collision) != requested_target_id && !allowed_ids.contains(&collision) {
                    return Err(CatalogError::Validation(format!(
                        "catalog changed during review: unreviewed collision with catalog id {collision}; Gemini must re-evaluate"
                    )));
                }
            }
            let mut target_id = requested_target_id;
            if let (Some(identifier_id), Some(name_id)) = (identifier_collision, name_collision) {
                if identifier_id != name_id {
                    return Err(CatalogError::Validation(format!(
                        "verified identifier and canonical name collide with different legacy rows ({identifier_id}, {name_id}); explicit duplicate merge is required"
                    )));
                }
                target_id = Some(identifier_id);
            } else if let Some(collision_id) = identifier_collision.or(name_collision) {
                // The independent review confirmed this collision as the same
                // product. Promote the row already owning the verified key so
                // canonicalization cannot violate a uniqueness constraint.
                target_id = Some(collision_id);
            }

            let stored_id = if let Some(target_id) = target_id {
                let target_check = db.sql(
                    "SELECT catalog_status, normalized_manufacturer_identifier FROM avionics_models WHERE id = ?",
                );
                let target_state: Option<(String, Option<String>)> = sqlx::query_as(&target_check)
                    .bind(target_id)
                    .fetch_optional(&mut *transaction)
                    .await?;
                let target_status = target_state.as_ref().map(|state| state.0.as_str());
                match target_status {
                    Some("unreviewed") => {}
                    Some("approved") => {
                        return Err(CatalogError::Validation(format!(
                            "catalog id {target_id} became approved during review; retry identity resolution"
                        )));
                    }
                    Some("rejected") => {
                        return Err(CatalogError::Validation(format!(
                            "catalog id {target_id} was rejected during review"
                        )));
                    }
                    _ => {
                        return Err(CatalogError::Validation(format!(
                            "catalog id {target_id} disappeared during review"
                        )));
                    }
                }
                if target_state
                    .as_ref()
                    .and_then(|state| state.1.as_deref())
                    .is_some_and(|existing| {
                        !existing.is_empty() && existing != identifier_key.as_str()
                    })
                {
                    return Err(CatalogError::Validation(format!(
                        "catalog id {target_id} has a conflicting legacy manufacturer identifier; explicit adjudication is required"
                    )));
                }
                let update = db.sql(
                    r#"
                    UPDATE avionics_models
                    SET
                      avionics_manufacturer_id = ?,
                      name = ?,
                      normalized_name = ?,
                      manufacturer_identifier_kind = ?,
                      manufacturer_identifier = ?,
                      normalized_manufacturer_identifier = ?,
                      identity_source_url = ?,
                      identity_source_title = ?,
                      identity_evidence_text = ?,
                      identity_evidence_kind = 'authoritative_reference',
                      identity_confidence = 'very_high',
                      catalog_reviewed_at = CURRENT_TIMESTAMP,
                      introduced_year = NULL,
                      discontinued_year = NULL,
                      estimated_unit_value_usd = NULL,
                      value_basis = 'unreviewed',
                      replacement_cost_usd = NULL,
                      value_reference_year = NULL,
                      value_source = NULL,
                      valuation_scope = 'unit',
                      updated_at = CURRENT_TIMESTAMP
                    WHERE id = ? AND catalog_status = 'unreviewed'
                    "#,
                );
                let updated = sqlx::query(&update)
                    .bind(manufacturer_id)
                    .bind(identity.canonical_model.trim())
                    .bind(model_key.as_str())
                    .bind(identity.manufacturer_identifier_kind.as_str())
                    .bind(identity.manufacturer_identifier.trim())
                    .bind(identifier_key.as_str())
                    .bind(identity.identity_source_url.as_str())
                    .bind(identity.identity_source_title.as_str())
                    .bind(identity.identity_evidence.as_str())
                    .bind(target_id)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if updated != 1 {
                    return Err(CatalogError::Validation(
                        "catalog entry changed while it was being approved; retry identity resolution".to_string(),
                    ));
                }
                // Legacy value and suite metadata were not part of the
                // identity review. Remove the graph as well as the numeric
                // fields above so catalog approval cannot bless stale value
                // assumptions by association.
                let delete_suite_memberships = db.sql(
                    "DELETE FROM avionics_suite_components WHERE suite_model_id = ? OR component_model_id = ?",
                );
                sqlx::query(&delete_suite_memberships)
                    .bind(target_id)
                    .bind(target_id)
                    .execute(&mut *transaction)
                    .await?;
                target_id
            } else {
                let insert = db.sql(
                    r#"
                    INSERT INTO avionics_models (
                      avionics_manufacturer_id,
                      name,
                      normalized_name,
                      catalog_status,
                      manufacturer_identifier_kind,
                      manufacturer_identifier,
                      normalized_manufacturer_identifier,
                      identity_source_url,
                      identity_source_title,
                      identity_evidence_text,
                      identity_evidence_kind,
                      identity_confidence,
                      catalog_reviewed_at
                    )
                    VALUES (?, ?, ?, 'unreviewed', ?, ?, ?, ?, ?, ?, 'authoritative_reference', 'very_high', CURRENT_TIMESTAMP)
                    "#,
                );
                sqlx::query(&insert)
                    .bind(manufacturer_id)
                    .bind(identity.canonical_model.trim())
                    .bind(model_key.as_str())
                    .bind(identity.manufacturer_identifier_kind.as_str())
                    .bind(identity.manufacturer_identifier.trim())
                    .bind(identifier_key.as_str())
                    .bind(identity.identity_source_url.as_str())
                    .bind(identity.identity_source_title.as_str())
                    .bind(identity.identity_evidence.as_str())
                    .execute(&mut *transaction)
                    .await?;
                let select = db.sql(
                    "SELECT id FROM avionics_models WHERE avionics_manufacturer_id = ? AND normalized_manufacturer_identifier = ?",
                );
                sqlx::query_scalar(&select)
                    .bind(manufacturer_id)
                    .bind(identifier_key.as_str())
                    .fetch_one(&mut *transaction)
                    .await?
            };
            for type_id in &type_ids {
                let insert_membership = db.sql(
                    "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?) ON CONFLICT (avionics_model_id, avionics_type_id) DO NOTHING",
                );
                sqlx::query(&insert_membership)
                    .bind(stored_id)
                    .bind(*type_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            let select_existing_types = db.sql(
                "SELECT avionics_type_id FROM avionics_model_types WHERE avionics_model_id = ?",
            );
            let existing_type_ids: Vec<i64> = sqlx::query_scalar(&select_existing_types)
                .bind(stored_id)
                .fetch_all(&mut *transaction)
                .await?;
            let desired_type_ids = type_ids.iter().copied().collect::<HashSet<_>>();
            for stale_type_id in existing_type_ids
                .into_iter()
                .filter(|type_id| !desired_type_ids.contains(type_id))
            {
                let delete_membership = db.sql(
                    "DELETE FROM avionics_model_types WHERE avionics_model_id = ? AND avionics_type_id = ?",
                );
                sqlx::query(&delete_membership)
                    .bind(stored_id)
                    .bind(stale_type_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            let approve = db.sql(
                r#"
                UPDATE avionics_models
                SET catalog_status = 'approved',
                    catalog_reviewed_at = CURRENT_TIMESTAMP,
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?
                  AND catalog_status = 'unreviewed'
                  AND EXISTS (
                    SELECT 1
                    FROM avionics_model_types model_type
                    WHERE model_type.avionics_model_id = avionics_models.id
                  )
                "#,
            );
            let approved = sqlx::query(&approve)
                .bind(stored_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if approved != 1 {
                return Err(CatalogError::Validation(
                    "catalog product could not be approved with its verified capabilities"
                        .to_string(),
                ));
            }
            transaction.commit().await?;
            stored_id
        }};
    }

    let stored_id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => persist!(pool),
        DatabaseBackend::Postgres(pool) => persist!(pool),
    };
    Ok(approved_identity_from_verified(stored_id, identity))
}

fn string_field<'a>(value: &'a Value, field: &str) -> &'a str {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
}

fn required_field(value: &Value, field: &str) -> CatalogResult<String> {
    let value = string_field(value, field);
    if value.is_empty() {
        return Err(CatalogError::Validation(format!(
            "Gemini avionics identity response missing {field}"
        )));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        canonical_avionics_types_for_label, canonical_types_from_response, catalog_fingerprint,
        collision_reviews, expanded_collision_context, load_catalog_candidates,
        persist_approved_capability_enrichment, persist_approved_identity, proposal_attestation,
        resolution_issues, shortlist_avionics_candidates, verified_identity_from_response,
        AvionicsCatalogCandidate, AvionicsUnitResolutionCandidate, AvionicsUnitResolutionContext,
        GeminiGroundingSource, GeminiGroundingSupport, VerifiedIdentity,
    };
    use crate::db::{AppDb, DatabaseBackend};

    fn candidate(id: i64, model: &str, status: &str) -> AvionicsCatalogCandidate {
        AvionicsCatalogCandidate {
            id,
            manufacturer: "Garmin".to_string(),
            model: model.to_string(),
            avionics_types: vec!["Transponder".to_string()],
            manufacturer_identifier_kind: "manufacturer_part_number".to_string(),
            manufacturer_identifier: format!("011-TEST-{id}"),
            catalog_status: status.to_string(),
        }
    }

    fn context(candidates: Vec<AvionicsCatalogCandidate>) -> AvionicsUnitResolutionContext {
        AvionicsUnitResolutionContext {
            aircraft_manufacturer: "Cessna".to_string(),
            aircraft_model: "172".to_string(),
            aircraft_variant: "S".to_string(),
            model_year: 2020,
            source_url: "https://broker.example/aircraft/1".to_string(),
            listing_context: "Garmin GTX345R installed".to_string(),
            requires_listing_evidence: true,
            candidate: AvionicsUnitResolutionCandidate {
                manufacturer: "Garmin".to_string(),
                model: "GTX345R".to_string(),
                avionics_types: vec!["Transponder".to_string()],
                quantity: 1,
            },
            catalog_candidates: candidates,
        }
    }

    fn verified_identity() -> VerifiedIdentity {
        VerifiedIdentity {
            canonical_manufacturer: "Garmin".to_string(),
            canonical_model: "GTX 345R".to_string(),
            canonical_types: vec!["Transponder".to_string()],
            manufacturer_identifier_kind: "manufacturer_part_number".to_string(),
            manufacturer_identifier: "011-03520-00".to_string(),
            identity_source_url: "https://static.garmin.com/manuals/gtx345r.pdf".to_string(),
            identity_source_title: "GTX 345R installation manual".to_string(),
            identity_evidence:
                "The manufacturer manual identifies the exact product and part number.".to_string(),
            reason: "Authoritative manufacturer documentation.".to_string(),
        }
    }

    fn grounding(
        url: &str,
        title: &str,
        evidence: &str,
    ) -> (Vec<GeminiGroundingSource>, Vec<GeminiGroundingSupport>) {
        (
            vec![GeminiGroundingSource {
                chunk_index: 0,
                url: url.to_string(),
                title: title.to_string(),
            }],
            vec![GeminiGroundingSupport {
                text: evidence.to_string(),
                source_indices: vec![0],
            }],
        )
    }

    #[test]
    fn similarity_retrieval_includes_exact_typography_variant_but_does_not_resolve_it() {
        let catalog = vec![
            candidate(1, "GTX 345R", "approved"),
            candidate(2, "GMA 350", "approved"),
        ];
        let shortlist = shortlist_avionics_candidates(
            "Garmin",
            "GTX-345R",
            &["Transponder".to_string()],
            None,
            &catalog,
        );
        assert_eq!(
            shortlist.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn grounded_identifier_expands_collision_set_before_storage() {
        let mut catalog_row = candidate(9, "Legacy Imported Label", "unreviewed");
        catalog_row.manufacturer_identifier = "011-03520-00".to_string();
        let initial = context(vec![]);
        let expanded = expanded_collision_context(&initial, &verified_identity(), &[catalog_row]);
        assert_eq!(
            expanded
                .catalog_candidates
                .iter()
                .map(|candidate| candidate.id)
                .collect::<Vec<_>>(),
            vec![9]
        );
    }

    #[test]
    fn expanded_curated_capabilities_are_accepted_and_deduplicated() {
        for capability in [
            "ELT",
            "ADF",
            "DME",
            "AHRS",
            "Air Data Computer",
            "Navigation Indicator",
            "Weather Radar",
            "Lightning Detection",
            "Radar Altimeter",
            "Magnetometer",
            "Clock/Timer",
        ] {
            let response = json!({"canonical_types": [capability, capability]});
            assert_eq!(
                canonical_types_from_response(&response, "canonical_types")
                    .expect("curated capability should validate"),
                vec![capability.to_string()]
            );
        }
        assert_eq!(
            canonical_avionics_types_for_label("CDI/HSI"),
            vec!["Navigation Indicator"]
        );
        assert_eq!(
            canonical_avionics_types_for_label("attitude and heading reference system"),
            vec!["AHRS"]
        );
        assert_eq!(
            canonical_avionics_types_for_label("emergency locator transmitter"),
            vec!["ELT"]
        );
        assert_eq!(
            canonical_avionics_types_for_label("NAV/COM"),
            vec!["NAV", "COM"]
        );
        assert!(canonical_types_from_response(
            &json!({"canonical_types": ["NAV/COM"]}),
            "canonical_types"
        )
        .is_err());
    }

    #[test]
    fn gnx_375_is_one_identity_with_gps_and_transponder_capabilities() {
        let response = json!({
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GNX 375",
            "canonical_types": ["Transponder", "GPS", "GPS"],
            "manufacturer_identifier_kind": "manufacturer_model_number",
            "manufacturer_identifier": "GNX 375",
            "identity_source_url": "https://www.garmin.com/gnx-375",
            "identity_source_title": "Garmin GNX 375",
            "identity_evidence": "Garmin identifies GNX 375 as one GPS navigator with a transponder.",
            "reason": "The manufacturer documents both functions on one product."
        });
        let identity = verified_identity_from_response(&response)
            .expect("multifunction identity should validate");
        assert_eq!(
            identity.canonical_types,
            vec!["GPS".to_string(), "Transponder".to_string()]
        );
    }

    #[test]
    fn approved_match_requires_exact_server_identity_and_authoritative_evidence() {
        let context = context(vec![candidate(1, "GTX 345R", "approved")]);
        let response = json!({
            "status": "existing_match",
            "catalog_id": 1,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-TEST-1",
            "confidence": "very_high",
            "identity_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "identity_source_title": "GTX 345R installation manual",
            "identity_evidence": "The manual identifies the remote GTX 345R and part number.",
            "reason": "Official installation manual establishes the identity."
        });
        let (sources, supports) = grounding(
            "https://static.garmin.com/manuals/gtx345r.pdf",
            "GTX 345R installation manual",
            "The manual identifies the remote GTX 345R and part number.",
        );
        assert!(resolution_issues(&context, &response, true, &sources, &supports).is_empty());
        assert!(
            resolution_issues(&context, &response, false, &sources, &supports)
                .iter()
                .any(|issue| issue.contains("Google Search grounding metadata"))
        );
    }

    #[test]
    fn approved_match_can_only_enrich_an_observed_capability_as_a_monotonic_union() {
        let mut approved = candidate(1, "GNX 375", "approved");
        approved.manufacturer_identifier_kind = "manufacturer_model_number".to_string();
        approved.manufacturer_identifier = "GNX 375".to_string();
        let mut context = context(vec![approved]);
        context.listing_context = "Garmin GNX 375 navigator/transponder installed".to_string();
        context.candidate.model = "GNX 375".to_string();
        context.candidate.avionics_types = vec!["GPS".to_string()];
        let mut response = json!({
            "status": "existing_match",
            "catalog_id": 1,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GNX 375",
            "canonical_types": ["GPS", "Transponder"],
            "manufacturer_identifier_kind": "manufacturer_model_number",
            "manufacturer_identifier": "GNX 375",
            "confidence": "very_high",
            "identity_source_url": "https://static.garmin.com/manuals/gnx375.pdf",
            "identity_source_title": "Garmin GNX 375 pilot guide",
            "identity_evidence": "Garmin documents the GNX 375 as a GPS navigator with an integrated transponder.",
            "reason": "The official guide verifies both capabilities on the exact product."
        });
        let (sources, supports) = grounding(
            "https://static.garmin.com/manuals/gnx375.pdf",
            "Garmin GNX 375 pilot guide",
            "Garmin documents the GNX 375 as a GPS navigator with an integrated transponder.",
        );
        assert!(resolution_issues(&context, &response, true, &sources, &supports).is_empty());

        response["canonical_types"] = json!(["Transponder"]);
        let issues = resolution_issues(&context, &response, true, &sources, &supports);
        assert!(issues
            .iter()
            .any(|issue| issue.contains("newly observed capability \"GPS\" or return unresolved")));
    }

    #[test]
    fn new_identity_requires_very_high_confidence() {
        let context = context(vec![]);
        let response = json!({
            "status": "propose_new",
            "catalog_id": 0,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "confidence": "high",
            "identity_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "identity_source_title": "GTX 345R installation manual",
            "identity_evidence": "The manual identifies the part number.",
            "reason": "Official source."
        });
        assert!(resolution_issues(&context, &response, true, &[], &[])
            .iter()
            .any(|issue| issue.contains("very_high")));
    }

    #[test]
    fn new_identity_rejects_combined_model_labels() {
        let context = context(vec![]);
        let response = json!({
            "status": "propose_new",
            "catalog_id": 0,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GNS 430/530",
            "canonical_types": ["NAV", "COM"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-00000-00",
            "confidence": "very_high",
            "identity_source_url": "https://static.garmin.com/manuals/gns.pdf",
            "identity_source_title": "GNS manual",
            "identity_evidence": "The document describes two separate units.",
            "reason": "Combined label."
        });
        assert!(resolution_issues(&context, &response, true, &[], &[])
            .iter()
            .any(|issue| issue.contains("combined model label")));
    }

    #[test]
    fn collision_review_must_cover_every_shortlist_candidate_once() {
        let context = context(vec![
            candidate(1, "GTX 345", "approved"),
            candidate(2, "GTX 345R", "unreviewed"),
        ]);
        let response = json!({
            "reviews": [{
                "catalog_id": 1,
                "decision": "different_product",
                "confidence": "very_high",
                "source_url": "https://static.garmin.com/manuals/gtx345.pdf",
                "source_title": "GTX 345 manual",
                "evidence": "The manual distinguishes the panel and remote variants.",
                "reason": "Different form factor and part number."
            }]
        });
        let (sources, supports) = grounding(
            "https://static.garmin.com/manuals/gtx345.pdf",
            "GTX 345 manual",
            "The manual distinguishes the panel and remote variants.",
        );
        let error = collision_reviews(&context, &response, &sources, &supports).unwrap_err();
        assert!(error.to_string().contains("omitted"));
    }

    #[test]
    fn empty_shortlist_still_requires_independent_grounded_proposal_attestation() {
        let context = context(vec![]);
        let proposed = verified_identity();
        let mut response = json!({
            "proposal_decision": "confirmed_same_as_input",
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "proposal_confidence": "very_high",
            "input_evidence_text": "GTX345R",
            "proposal_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "proposal_source_title": "GTX 345R installation manual",
            "proposal_evidence": "The manufacturer manual identifies the exact product and part number.",
            "proposal_reason": "The listing excerpt and official manual identify one exact unit.",
            "reviews": []
        });
        let (sources, supports) = grounding(
            "https://static.garmin.com/manuals/gtx345r.pdf",
            "GTX 345R installation manual",
            "The manufacturer manual identifies the exact product and part number.",
        );
        let attestation = proposal_attestation(&context, &proposed, &response, &sources, &supports)
            .expect("grounded attestation should validate");
        assert!(attestation.confirmed);

        response["input_evidence_text"] = json!("GTX 345R");
        let error =
            proposal_attestation(&context, &proposed, &response, &sources, &supports).unwrap_err();
        assert!(error.to_string().contains("copied exactly"));
    }

    #[test]
    fn honest_negative_proposal_attestation_returns_unconfirmed_without_citations() {
        let context = context(vec![]);
        let proposed = verified_identity();
        let response = json!({
            "proposal_decision": "not_confirmed",
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "proposal_confidence": "medium",
            "input_evidence_text": "",
            "proposal_source_url": "",
            "proposal_source_title": "",
            "proposal_evidence": "",
            "proposal_reason": "The stored listing excerpt is insufficient to prove the mapping.",
            "reviews": []
        });
        let attestation = proposal_attestation(&context, &proposed, &response, &[], &[])
            .expect("an honest negative should remain a normal unresolved outcome");
        assert!(!attestation.confirmed);
    }

    #[test]
    fn listing_pages_are_not_identity_evidence() {
        let context = context(vec![candidate(1, "GTX 345R", "approved")]);
        let mut response = json!({
            "status": "existing_match",
            "catalog_id": 1,
            "canonical_manufacturer": "Garmin",
            "canonical_model": "GTX 345R",
            "canonical_types": ["Transponder"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-TEST-1",
            "confidence": "very_high",
            "identity_source_url": "https://broker.example/listings/1",
            "identity_source_title": "Aircraft for sale",
            "identity_evidence": "Seller says it is installed.",
            "reason": "Listing text."
        });
        assert!(resolution_issues(&context, &response, true, &[], &[])
            .iter()
            .any(|issue| issue.contains("sale listings")));
        response["identity_source_url"] = json!("https://broker.example/aircraft/1");
        assert!(resolution_issues(&context, &response, true, &[], &[])
            .iter()
            .any(|issue| issue.contains("cannot also be identity evidence")));
    }

    #[tokio::test]
    async fn verified_identity_persistence_creates_only_an_approved_stable_catalog_row() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let empty_fingerprint = catalog_fingerprint(&[]);
        let stored =
            persist_approved_identity(&db, None, &[], &verified_identity(), &empty_fingerprint)
                .await
                .expect("verified identity should persist");
        assert!(stored.id > 0);

        let catalog = load_catalog_candidates(&db)
            .await
            .expect("catalog should load");
        let row = catalog
            .iter()
            .find(|candidate| candidate.id == stored.id)
            .expect("stored identity should be in the catalog");
        assert_eq!(row.catalog_status, "approved");
        assert_eq!(row.manufacturer_identifier, "011-03520-00");
    }

    #[tokio::test]
    async fn multifunction_identity_persists_one_product_and_every_capability() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let mut identity = verified_identity();
        identity.canonical_model = "GNX 375".to_string();
        identity.canonical_types = vec!["GPS".to_string(), "Transponder".to_string()];
        identity.manufacturer_identifier = "GNX-375-TEST".to_string();
        let stored =
            persist_approved_identity(&db, None, &[], &identity, &catalog_fingerprint(&[]))
                .await
                .expect("multifunction identity should persist");

        let catalog = load_catalog_candidates(&db)
            .await
            .expect("catalog should load");
        let rows = catalog
            .iter()
            .filter(|candidate| candidate.id == stored.id)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "capabilities must not duplicate products");
        assert_eq!(
            rows[0].avionics_types,
            vec!["GPS".to_string(), "Transponder".to_string()]
        );
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let membership_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM avionics_model_types WHERE avionics_model_id = ?",
        )
        .bind(stored.id)
        .fetch_one(pool)
        .await
        .expect("capability memberships should load");
        assert_eq!(membership_count, 2);
    }

    #[tokio::test]
    async fn approved_product_enrichment_adds_capability_without_replacing_identity_or_memberships()
    {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let mut original = verified_identity();
        original.canonical_model = "GNX 375".to_string();
        original.manufacturer_identifier_kind = "manufacturer_model_number".to_string();
        original.manufacturer_identifier = "GNX 375".to_string();
        let stored =
            persist_approved_identity(&db, None, &[], &original, &catalog_fingerprint(&[]))
                .await
                .expect("initial approved product should persist");
        let reviewed_catalog = load_catalog_candidates(&db)
            .await
            .expect("approved product should load");
        let reviewed = reviewed_catalog
            .iter()
            .find(|candidate| candidate.id == stored.id)
            .expect("approved product should be reviewed")
            .clone();

        let mut enriched = original.clone();
        enriched.canonical_types = vec!["GPS".to_string(), "Transponder".to_string()];
        enriched.identity_source_url = "https://static.garmin.com/manuals/gnx375.pdf".to_string();
        enriched.identity_source_title = "Garmin GNX 375 pilot guide".to_string();
        enriched.identity_evidence =
            "Garmin documents the GNX 375 as a GPS navigator with an integrated transponder."
                .to_string();
        let result = persist_approved_capability_enrichment(
            &db,
            &reviewed,
            &enriched,
            &catalog_fingerprint(&reviewed_catalog),
        )
        .await
        .expect("independently verified capability should enrich the product");
        assert_eq!(result.id, stored.id);

        let catalog = load_catalog_candidates(&db)
            .await
            .expect("enriched catalog should load");
        assert_eq!(catalog.len(), 1, "enrichment must not create a product");
        assert_eq!(
            catalog[0].avionics_types,
            vec!["GPS".to_string(), "Transponder".to_string()]
        );
        assert_eq!(catalog[0].manufacturer_identifier, "GNX 375");
    }

    #[tokio::test]
    async fn rejected_non_monotonic_enrichment_leaves_approved_capabilities_unchanged() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let stored = persist_approved_identity(
            &db,
            None,
            &[],
            &verified_identity(),
            &catalog_fingerprint(&[]),
        )
        .await
        .expect("initial approved product should persist");
        let reviewed_catalog = load_catalog_candidates(&db)
            .await
            .expect("approved product should load");
        let reviewed = reviewed_catalog[0].clone();
        let mut invalid = verified_identity();
        invalid.canonical_types = vec!["GPS".to_string()];
        let error = persist_approved_capability_enrichment(
            &db,
            &reviewed,
            &invalid,
            &catalog_fingerprint(&reviewed_catalog),
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot remove stored capability"));

        let unchanged = load_catalog_candidates(&db)
            .await
            .expect("unchanged product should load");
        assert_eq!(unchanged.len(), 1);
        assert_eq!(unchanged[0].id, stored.id);
        assert_eq!(unchanged[0].avionics_types, vec!["Transponder".to_string()]);
    }

    #[tokio::test]
    async fn capability_enrichment_rejects_a_stale_catalog_fingerprint() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let stored = persist_approved_identity(
            &db,
            None,
            &[],
            &verified_identity(),
            &catalog_fingerprint(&[]),
        )
        .await
        .expect("initial approved product should persist");
        let reviewed_catalog = load_catalog_candidates(&db)
            .await
            .expect("approved product should load");
        let reviewed = reviewed_catalog[0].clone();
        let stale_fingerprint = catalog_fingerprint(&reviewed_catalog);

        let mut second = verified_identity();
        second.canonical_model = "GMA 350c".to_string();
        second.canonical_types = vec!["Audio Panel".to_string()];
        second.manufacturer_identifier = "011-02385-20".to_string();
        persist_approved_identity(
            &db,
            None,
            &[],
            &second,
            &catalog_fingerprint(&reviewed_catalog),
        )
        .await
        .expect("concurrent catalog addition should persist");

        let mut enriched = verified_identity();
        enriched.canonical_types = vec!["GPS".to_string(), "Transponder".to_string()];
        let error =
            persist_approved_capability_enrichment(&db, &reviewed, &enriched, &stale_fingerprint)
                .await
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("changed during Gemini capability review"));
        let unchanged = load_catalog_candidates(&db)
            .await
            .expect("catalog should remain readable");
        let original = unchanged
            .iter()
            .find(|candidate| candidate.id == stored.id)
            .expect("original product should remain");
        assert_eq!(original.avionics_types, vec!["Transponder".to_string()]);
    }

    #[tokio::test]
    async fn persistence_refuses_to_approve_a_product_without_capabilities() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let mut identity = verified_identity();
        identity.canonical_types.clear();
        let error = persist_approved_identity(&db, None, &[], &identity, &catalog_fingerprint(&[]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("without a canonical capability"));
    }

    #[tokio::test]
    async fn persistence_rejects_a_catalog_snapshot_that_changed_during_review() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let stale_empty_fingerprint = catalog_fingerprint(&[]);
        persist_approved_identity(
            &db,
            None,
            &[],
            &verified_identity(),
            &stale_empty_fingerprint,
        )
        .await
        .expect("first identity should persist");

        let mut second = verified_identity();
        second.canonical_model = "GMA 350c".to_string();
        second.canonical_types = vec!["Audio Panel".to_string()];
        second.manufacturer_identifier = "011-02385-20".to_string();
        let error = persist_approved_identity(&db, None, &[], &second, &stale_empty_fingerprint)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("changed during Gemini review"));
    }

    #[tokio::test]
    async fn legacy_promotion_invalidates_unreviewed_value_metadata() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let identity = verified_identity();
        let empty_fingerprint = catalog_fingerprint(&[]);
        let stored = persist_approved_identity(&db, None, &[], &identity, &empty_fingerprint)
            .await
            .expect("verified identity should persist");
        let sql = db.sql(
            r#"
            UPDATE avionics_models
            SET catalog_status = 'unreviewed',
                introduced_year = 1999,
                estimated_unit_value_usd = 999999,
                value_basis = 'installed_contribution',
                replacement_cost_usd = 1000000,
                value_reference_year = 2026,
                value_source = 'legacy-import',
                valuation_scope = 'integrated_suite'
            WHERE id = ?
            "#,
        );
        match db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(&sql)
                    .bind(stored.id)
                    .execute(pool)
                    .await
                    .expect("legacy values should seed");
            }
            DatabaseBackend::Postgres(_) => unreachable!("test uses SQLite"),
        }

        let legacy_catalog = load_catalog_candidates(&db)
            .await
            .expect("legacy catalog should load");
        let legacy_fingerprint = catalog_fingerprint(&legacy_catalog);
        persist_approved_identity(
            &db,
            Some(stored.id),
            &[stored.id],
            &identity,
            &legacy_fingerprint,
        )
        .await
        .expect("legacy identity should promote");
        let sql = db.sql(
            "SELECT COUNT(*) FROM avionics_models WHERE id = ? AND catalog_status = 'approved' AND introduced_year IS NULL AND estimated_unit_value_usd IS NULL AND replacement_cost_usd IS NULL AND value_reference_year IS NULL AND value_source IS NULL AND value_basis = 'unreviewed' AND valuation_scope = 'unit'",
        );
        let clean_count: i64 = match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(&sql)
                .bind(stored.id)
                .fetch_one(pool)
                .await
                .expect("promoted row should load"),
            DatabaseBackend::Postgres(_) => unreachable!("test uses SQLite"),
        };
        assert_eq!(clean_count, 1);
    }

    #[tokio::test]
    async fn legacy_promotion_refuses_to_overwrite_a_conflicting_identifier() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let identity = verified_identity();
        let empty_fingerprint = catalog_fingerprint(&[]);
        let stored = persist_approved_identity(&db, None, &[], &identity, &empty_fingerprint)
            .await
            .expect("verified identity should persist");
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        sqlx::query(
            r#"
            UPDATE avionics_models
            SET catalog_status = 'unreviewed',
                manufacturer_identifier = 'LEGACY-CONFLICT',
                normalized_manufacturer_identifier = 'legacyconflict'
            WHERE id = ?
            "#,
        )
        .bind(stored.id)
        .execute(pool)
        .await
        .expect("legacy conflict should seed");
        let legacy_catalog = load_catalog_candidates(&db)
            .await
            .expect("legacy catalog should load");
        let legacy_fingerprint = catalog_fingerprint(&legacy_catalog);
        let error = persist_approved_identity(
            &db,
            Some(stored.id),
            &[stored.id],
            &identity,
            &legacy_fingerprint,
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("conflicting legacy manufacturer identifier"));
    }
}
