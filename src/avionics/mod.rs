pub mod catalog;
pub mod repopulate;

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;

use crate::aircraft::faa::{require_listing_admission, AircraftAdmissionError};
use crate::avionics::catalog::{
    preview_avionics_identity, resolve_avionics_identity, ApprovedAvionicsIdentity,
    AvionicsIdentityOutcome, AvionicsIdentityRequest,
};
use crate::cleanup::{cleanup_orphan_records, CleanupError};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{
    AircraftPricePointContext, AvionicsMetadataContext, DefaultAvionicsContext,
    GeminiListingExtractor,
};
use crate::normalize::normalize_avionics_manufacturer_name;
use crate::normalize::{is_usable_avionics_label, normalize_avionics_model_name, normalize_name};

const DEFAULT_VALUE_REFERENCE_YEAR: i64 = 2026;

macro_rules! execute_query {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|_| ())
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|_| ())
            }
        }
    }};
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

macro_rules! query_scalar_optional {
    ($db:expr, $ty:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_optional(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_optional(pool).await
            }
        }
    }};
}

macro_rules! query_scalar_one {
    ($db:expr, $ty:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_one(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_one(pool).await
            }
        }
    }};
}

#[derive(Debug)]
pub enum AvionicsStoreError {
    Database(String),
    Model(String),
}

impl std::fmt::Display for AvionicsStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AvionicsStoreError::Database(message) | AvionicsStoreError::Model(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl std::error::Error for AvionicsStoreError {}

impl From<sqlx::Error> for AvionicsStoreError {
    fn from(error: sqlx::Error) -> Self {
        AvionicsStoreError::Database(error.to_string())
    }
}

impl From<anyhow::Error> for AvionicsStoreError {
    fn from(error: anyhow::Error) -> Self {
        AvionicsStoreError::Model(error.to_string())
    }
}

impl From<CleanupError> for AvionicsStoreError {
    fn from(error: CleanupError) -> Self {
        AvionicsStoreError::Database(error.to_string())
    }
}

fn aircraft_admission_store_error(error: AircraftAdmissionError) -> AvionicsStoreError {
    let message = error.to_string();
    match error {
        AircraftAdmissionError::Rejected { .. }
        | AircraftAdmissionError::ListingNotFound { .. } => AvionicsStoreError::Model(message),
        AircraftAdmissionError::LookupFailed { .. } => AvionicsStoreError::Database(message),
    }
}

type StoreResult<T> = Result<T, AvionicsStoreError>;

#[derive(Clone, Debug)]
struct AvionicsModelReferenceRow {
    id: i64,
    manufacturer: String,
    model: String,
    avionics_types: Vec<String>,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
    replacement_cost_usd: Option<f64>,
    valuation_scope: String,
}

#[derive(Clone, Debug, FromRow)]
struct AvionicsModelReferenceDbRow {
    id: i64,
    manufacturer: String,
    model: String,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
    replacement_cost_usd: Option<f64>,
    valuation_scope: String,
}

#[derive(Clone, Debug)]
struct AvionicsModelNormalizeRow {
    id: i64,
    avionics_manufacturer_id: i64,
    manufacturer: String,
    avionics_types: Vec<String>,
    name: String,
    normalized_name: String,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
    value_basis: String,
    replacement_cost_usd: Option<f64>,
    value_reference_year: Option<i64>,
    value_source: Option<String>,
    valuation_scope: String,
}

#[derive(Clone, Debug, FromRow)]
struct AvionicsModelNormalizeDbRow {
    id: i64,
    avionics_manufacturer_id: i64,
    manufacturer: String,
    name: String,
    normalized_name: String,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
    value_basis: String,
    replacement_cost_usd: Option<f64>,
    value_reference_year: Option<i64>,
    value_source: Option<String>,
    valuation_scope: String,
}

#[derive(Debug, FromRow)]
struct AvionicsListingLinkRow {
    aircraft_sale_listing_id: i64,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    source_confidence: Option<String>,
}

#[derive(Debug, FromRow)]
struct AvionicsSuiteLinkRow {
    suite_model_id: i64,
    component_model_id: i64,
    quantity: i64,
}

#[derive(Debug, FromRow)]
struct AvionicsDefaultProfileLinkRow {
    aircraft_model_variant_id: i64,
    model_year: i64,
    quantity: i64,
    source_url: String,
    source_title: String,
    source_notes: String,
    source_confidence: String,
}

#[derive(Debug)]
struct AvionicsNormalizationInputRow {
    id: i64,
    manufacturer: String,
    avionics_types: Vec<String>,
    model: String,
    normalized_model: String,
    listing_count: i64,
    introduced_year: Option<i64>,
}

#[derive(Debug, FromRow)]
struct AvionicsNormalizationInputDbRow {
    id: i64,
    manufacturer: String,
    model: String,
    normalized_model: String,
    listing_count: i64,
    introduced_year: Option<i64>,
}

#[derive(Debug, FromRow)]
struct AvionicsCapabilityRow {
    avionics_model_id: i64,
    avionics_type: String,
}

async fn avionics_capability_map(db: &AppDb) -> StoreResult<BTreeMap<i64, Vec<String>>> {
    let rows = query_as_all!(
        db,
        AvionicsCapabilityRow,
        r#"
        SELECT membership.avionics_model_id, avionics_type.name AS avionics_type
        FROM avionics_model_types membership
        JOIN avionics_types avionics_type
          ON avionics_type.id = membership.avionics_type_id
        ORDER BY membership.avionics_model_id, avionics_type.normalized_name
        "#
    )?;
    let mut capabilities = BTreeMap::new();
    for row in rows {
        capabilities
            .entry(row.avionics_model_id)
            .or_insert_with(Vec::new)
            .push(row.avionics_type);
    }
    Ok(capabilities)
}

fn required_model_capabilities(
    capabilities: &BTreeMap<i64, Vec<String>>,
    avionics_model_id: i64,
) -> StoreResult<Vec<String>> {
    capabilities
        .get(&avionics_model_id)
        .filter(|values| !values.is_empty())
        .cloned()
        .ok_or_else(|| {
            AvionicsStoreError::Database(format!(
                "avionics catalog id {avionics_model_id} has no capability memberships"
            ))
        })
}

fn hydrate_reference_rows(
    rows: Vec<AvionicsModelReferenceDbRow>,
    capabilities: &BTreeMap<i64, Vec<String>>,
) -> StoreResult<Vec<AvionicsModelReferenceRow>> {
    rows.into_iter()
        .map(|row| {
            Ok(AvionicsModelReferenceRow {
                id: row.id,
                manufacturer: row.manufacturer,
                model: row.model,
                avionics_types: required_model_capabilities(capabilities, row.id)?,
                introduced_year: row.introduced_year,
                estimated_unit_value_usd: row.estimated_unit_value_usd,
                replacement_cost_usd: row.replacement_cost_usd,
                valuation_scope: row.valuation_scope,
            })
        })
        .collect()
}

#[derive(Debug, FromRow)]
struct AircraftModelYearProfileRow {
    aircraft_model_variant_id: i64,
    manufacturer: String,
    model: String,
    variant: String,
    model_year: i64,
    source_url: Option<String>,
    listing_count: i64,
}

#[derive(Debug, FromRow)]
struct AircraftModelYearProfileCandidateRow {
    listing_id: i64,
    aircraft_model_variant_id: i64,
    manufacturer: String,
    model: String,
    variant: String,
    model_year: i64,
    source_url: Option<String>,
}

#[derive(Clone, Debug)]
struct AvionicsIdentityAircraftContext {
    manufacturer: String,
    model: String,
    variant: String,
    model_year: i64,
    source_url: String,
}

impl AvionicsIdentityAircraftContext {
    fn unknown(model_year: i64) -> Self {
        Self {
            manufacturer: String::new(),
            model: String::new(),
            variant: String::new(),
            model_year,
            source_url: String::new(),
        }
    }
}

impl From<AircraftModelYearProfileRow> for AvionicsIdentityAircraftContext {
    fn from(row: AircraftModelYearProfileRow) -> Self {
        Self {
            manufacturer: row.manufacturer,
            model: row.model,
            variant: row.variant,
            model_year: row.model_year,
            source_url: row.source_url.unwrap_or_default(),
        }
    }
}

#[derive(Debug, FromRow)]
struct NearbyAircraftPricePointRow {
    variant: String,
    model_year: i64,
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
    source_title: String,
    source_confidence: String,
}

#[derive(Debug, FromRow)]
struct StoredPricePointQualityRow {
    source_confidence: String,
    evidence_kind: String,
    is_valuation_eligible: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsEnrichmentReport {
    pub applied: bool,
    pub value_reference_year: i64,
    pub items: Vec<AvionicsEnrichmentItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsEnrichmentItem {
    pub avionics_model_id: i64,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub previous_introduced_year: Option<i64>,
    pub previous_estimated_unit_value_usd: Option<f64>,
    pub previous_replacement_cost_usd: Option<f64>,
    pub previous_valuation_scope: String,
    pub introduced_year: i64,
    pub installed_value_contribution_usd: f64,
    pub replacement_cost_usd: f64,
    pub valuation_scope: String,
    pub included_components: Vec<AvionicsIncludedComponentItem>,
    pub identity: AvionicsIdentityEvidenceItem,
    pub confidence: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsIncludedComponentItem {
    pub avionics_model_id: i64,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub quantity: i64,
    pub identity: AvionicsIdentityEvidenceItem,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsIdentityEvidenceItem {
    pub manufacturer_identifier_kind: String,
    pub manufacturer_identifier: String,
    pub identity_source_url: String,
    pub identity_source_title: String,
    pub identity_evidence: String,
    pub identity_confidence: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsNormalizationReport {
    pub applied: bool,
    pub items: Vec<AvionicsNormalizationItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsNormalizationItem {
    pub canonical_model_id: i64,
    pub canonical_manufacturer: String,
    pub canonical_avionics_types: Vec<String>,
    pub canonical_name: String,
    pub canonical_normalized_name: String,
    pub source_model_ids: Vec<i64>,
    pub source_names: Vec<String>,
    pub resolution_status: String,
    pub resolution_reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsModelYearProfileReport {
    pub applied: bool,
    pub value_reference_year: i64,
    pub faa_admitted_candidate_count: usize,
    pub faa_rejected_candidate_count: usize,
    pub faa_rejections: Vec<String>,
    pub items: Vec<AvionicsModelYearProfileItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsModelYearProfileItem {
    pub aircraft_model_variant_id: i64,
    pub manufacturer: String,
    pub model: String,
    pub variant: String,
    pub model_year: i64,
    pub listing_count: i64,
    pub purchase_price_new_usd: f64,
    pub purchase_price_reference_year: i64,
    pub price_source_url: String,
    pub price_source_title: String,
    pub price_source_notes: String,
    pub price_source_confidence: String,
    pub price_evidence_kind: String,
    pub price_discontinuity_explanation: Option<String>,
    pub is_price_valuation_eligible: bool,
    pub price_eligibility_notes: Vec<String>,
    pub avionics: Vec<AvionicsModelYearProfileAvionicsItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsModelYearProfileAvionicsItem {
    pub avionics_model_id: i64,
    pub manufacturer: String,
    pub model: String,
    pub avionics_types: Vec<String>,
    pub quantity: i64,
    pub introduced_year: i64,
    pub installed_value_contribution_usd: f64,
    pub replacement_cost_usd: f64,
    pub valuation_scope: String,
    pub included_components: Vec<AvionicsIncludedComponentItem>,
    pub identity: AvionicsIdentityEvidenceItem,
    pub confidence: String,
    pub source_url: String,
    pub source_title: String,
    pub notes: String,
}

pub async fn normalize_avionics_models(
    db: &AppDb,
    apply: bool,
) -> StoreResult<AvionicsNormalizationReport> {
    let rows = avionics_models_for_normalization(db).await?;
    let canonical_manufacturer_ids =
        rows.iter()
            .fold(BTreeMap::<String, i64>::new(), |mut ids, row| {
                ids.entry(normalize_avionics_manufacturer_name(&row.manufacturer))
                    .and_modify(|id| *id = (*id).min(row.avionics_manufacturer_id))
                    .or_insert(row.avionics_manufacturer_id);
                ids
            });
    let mut groups: BTreeMap<(String, String), Vec<AvionicsModelNormalizeRow>> = BTreeMap::new();
    for row in rows {
        let manufacturer_key = normalize_avionics_manufacturer_name(&row.manufacturer);
        let model_key = normalize_avionics_model_name(&row.name);
        groups
            .entry((manufacturer_key, model_key))
            .or_default()
            .push(row);
    }

    let mut items = Vec::new();
    for ((manufacturer_key, canonical_normalized_name), rows) in groups {
        let canonical_manufacturer_id = canonical_manufacturer_ids[&manufacturer_key];
        let needs_normalization = rows.len() > 1
            || rows.iter().any(|row| {
                row.avionics_manufacturer_id != canonical_manufacturer_id
                    || row.normalized_name != canonical_normalized_name
            });
        if !needs_normalization {
            continue;
        }
        let canonical = rows
            .iter()
            .min_by_key(|row| {
                (
                    row.introduced_year.is_none() || row.estimated_unit_value_usd.is_none(),
                    row.id,
                )
            })
            .expect("normalization group is not empty")
            .clone();
        let canonical_manufacturer = rows
            .iter()
            .find(|row| row.avionics_manufacturer_id == canonical_manufacturer_id)
            .map(|row| row.manufacturer.clone())
            .unwrap_or_else(|| canonical.manufacturer.clone());
        let item = AvionicsNormalizationItem {
            canonical_model_id: canonical.id,
            canonical_manufacturer,
            canonical_avionics_types: canonical.avionics_types.clone(),
            canonical_name: canonical.name.clone(),
            canonical_normalized_name: canonical_normalized_name.clone(),
            source_model_ids: rows.iter().map(|row| row.id).collect(),
            source_names: rows.iter().map(|row| row.name.clone()).collect(),
            resolution_status: "legacy_unreviewed_merge".to_string(),
            resolution_reason: "mechanical normalization is restricted to legacy unreviewed catalog rows and does not approve product identity".to_string(),
        };
        if apply {
            apply_avionics_normalization_group(
                db,
                &canonical,
                canonical_manufacturer_id,
                &canonical.name,
                &canonical_normalized_name,
                &rows,
            )
            .await?;
        }
        items.push(item);
    }

    if apply {
        cleanup_orphan_records(db).await?;
    }

    Ok(AvionicsNormalizationReport {
        applied: apply,
        items,
    })
}

pub async fn enrich_missing_avionics_metadata(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    limit: i64,
    value_reference_year: Option<i64>,
    refresh_existing: bool,
) -> StoreResult<AvionicsEnrichmentReport> {
    if limit < 1 {
        return Err(AvionicsStoreError::Model(
            "limit must be at least 1".to_string(),
        ));
    }
    let value_reference_year = value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR);
    let rows = avionics_models_to_enrich(db, limit, refresh_existing).await?;
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let response = extractor
            .estimate_avionics_metadata(&AvionicsMetadataContext {
                manufacturer: &row.manufacturer,
                model: &row.model,
                avionics_types: &row.avionics_types,
                value_reference_year,
            })
            .await?;
        let mut item = enrichment_item_from_response(&row, &response.value)?;
        resolve_enrichment_item_identities(
            db,
            extractor,
            apply,
            &mut item,
            &AvionicsIdentityAircraftContext::unknown(value_reference_year),
            "standalone avionics metadata enrichment",
        )
        .await?;
        if apply {
            update_avionics_metadata(db, &item, value_reference_year, refresh_existing).await?;
        }
        items.push(item);
    }

    Ok(AvionicsEnrichmentReport {
        applied: apply,
        value_reference_year,
        items,
    })
}

pub async fn enrich_listing_avionics_metadata(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    listing_id: i64,
    value_reference_year: Option<i64>,
    refresh_existing: bool,
) -> StoreResult<AvionicsEnrichmentReport> {
    let value_reference_year = value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR);
    require_listing_admission(db, listing_id)
        .await
        .map_err(aircraft_admission_store_error)?;
    let rows = listing_avionics_models_to_enrich(db, listing_id, refresh_existing).await?;
    let aircraft_context = aircraft_model_year_profile_for_listing(db, listing_id, true)
        .await?
        .map(AvionicsIdentityAircraftContext::from)
        .unwrap_or_else(|| AvionicsIdentityAircraftContext::unknown(value_reference_year));
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let source_model_id = row.id;
        let response = extractor
            .estimate_avionics_metadata(&AvionicsMetadataContext {
                manufacturer: &row.manufacturer,
                model: &row.model,
                avionics_types: &row.avionics_types,
                value_reference_year,
            })
            .await?;
        let mut item = enrichment_item_from_response(&row, &response.value)?;
        resolve_enrichment_item_identities(
            db,
            extractor,
            apply,
            &mut item,
            &aircraft_context,
            "listing-linked avionics metadata enrichment",
        )
        .await?;
        if apply {
            if item.avionics_model_id != source_model_id {
                return Err(AvionicsStoreError::Model(format!(
                    "listing {listing_id} references legacy avionics model {source_model_id}, but grounded identity resolution selected approved catalog id {}; explicit transactional association remediation is required before value enrichment",
                    item.avionics_model_id
                )));
            }
            update_avionics_metadata(db, &item, value_reference_year, refresh_existing).await?;
        }
        items.push(item);
    }

    Ok(AvionicsEnrichmentReport {
        applied: apply,
        value_reference_year,
        items,
    })
}

pub async fn curate_avionics_models_with_gemini(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    limit: i64,
) -> StoreResult<AvionicsNormalizationReport> {
    if limit < 1 {
        return Err(AvionicsStoreError::Model(
            "limit must be at least 1".to_string(),
        ));
    }
    let rows = avionics_models_for_gemini_normalization(db, limit).await?;
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        if apply {
            let current_status = query_scalar_optional!(
                db,
                String,
                "SELECT catalog_status FROM avionics_models WHERE id = ?",
                row.id
            )?;
            match current_status.as_deref() {
                Some("unreviewed") => {}
                Some("approved") | None => {
                    // An earlier resolution in this same run may have promoted
                    // this collision target. Never feed it back through the
                    // legacy-unreviewed curation path.
                    continue;
                }
                Some(status) => {
                    return Err(AvionicsStoreError::Model(format!(
                        "legacy avionics catalog row {} changed to unexpected status {status} during curation",
                        row.id
                    )));
                }
            }
        }
        let request = AvionicsIdentityRequest {
            aircraft_manufacturer: String::new(),
            aircraft_model: String::new(),
            aircraft_variant: String::new(),
            model_year: row.introduced_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR),
            source_url: String::new(),
            listing_context: json!({
                "source": "legacy_unreviewed_catalog_row",
                "catalog_id": row.id,
                "listing_count": row.listing_count,
                "introduced_year": row.introduced_year,
            })
            .to_string(),
            requires_listing_evidence: false,
            manufacturer: row.manufacturer.clone(),
            model: row.model.clone(),
            avionics_types: row.avionics_types.clone(),
            quantity: 1,
        };
        let outcome = if apply {
            resolve_avionics_identity(db, extractor, &request).await
        } else {
            preview_avionics_identity(db, extractor, &request).await
        }
        .map_err(|error| {
            AvionicsStoreError::Model(format!(
                "could not resolve legacy avionics catalog row {}: {error}",
                row.id
            ))
        })?;

        let item = match outcome {
            AvionicsIdentityOutcome::Approved(approved) => {
                let status = if apply {
                    if approved.id == row.id {
                        "approved_promoted"
                    } else {
                        delete_unreferenced_legacy_catalog_row(
                            db,
                            row.id,
                            "approved catalog mapping",
                        )
                        .await?;
                        "approved_mapped"
                    }
                } else if approved.id == 0 {
                    "would_create_approved"
                } else if approved.id == row.id {
                    "would_promote"
                } else {
                    "would_map_to_approved"
                };
                normalization_item_from_identity(&row, &approved, status, approved.reason.clone())
            }
            AvionicsIdentityOutcome::Rejected { reason } => {
                let status = if apply {
                    delete_unreferenced_legacy_catalog_row(db, row.id, "rejected identity").await?;
                    "rejected_deleted"
                } else {
                    "would_reject"
                };
                AvionicsNormalizationItem {
                    canonical_model_id: 0,
                    canonical_manufacturer: row.manufacturer.clone(),
                    canonical_avionics_types: row.avionics_types.clone(),
                    canonical_name: row.model.clone(),
                    canonical_normalized_name: row.normalized_model.clone(),
                    source_model_ids: vec![row.id],
                    source_names: vec![row.model.clone()],
                    resolution_status: status.to_string(),
                    resolution_reason: reason,
                }
            }
            AvionicsIdentityOutcome::Unresolved { reason } => AvionicsNormalizationItem {
                canonical_model_id: row.id,
                canonical_manufacturer: row.manufacturer.clone(),
                canonical_avionics_types: row.avionics_types.clone(),
                canonical_name: row.model.clone(),
                canonical_normalized_name: row.normalized_model.clone(),
                source_model_ids: vec![row.id],
                source_names: vec![row.model.clone()],
                resolution_status: "unresolved".to_string(),
                resolution_reason: reason,
            },
        };
        items.push(item);
    }

    Ok(AvionicsNormalizationReport {
        applied: apply,
        items,
    })
}

fn normalization_item_from_identity(
    row: &AvionicsNormalizationInputRow,
    approved: &ApprovedAvionicsIdentity,
    status: &str,
    reason: String,
) -> AvionicsNormalizationItem {
    AvionicsNormalizationItem {
        canonical_model_id: approved.id,
        canonical_manufacturer: approved.manufacturer.clone(),
        canonical_avionics_types: approved.avionics_types.clone(),
        canonical_name: approved.model.clone(),
        canonical_normalized_name: normalize_avionics_model_name(&approved.model),
        source_model_ids: vec![row.id],
        source_names: vec![row.model.clone()],
        resolution_status: status.to_string(),
        resolution_reason: reason,
    }
}

async fn delete_unreferenced_legacy_catalog_row(
    db: &AppDb,
    avionics_model_id: i64,
    outcome: &str,
) -> StoreResult<()> {
    let status = query_scalar_optional!(
        db,
        String,
        "SELECT catalog_status FROM avionics_models WHERE id = ?",
        avionics_model_id
    )?;
    if status.as_deref() != Some("unreviewed") {
        return Err(AvionicsStoreError::Model(format!(
            "legacy avionics catalog row {avionics_model_id} changed from unreviewed to {} during curation; retry instead of deleting it",
            status.as_deref().unwrap_or("missing")
        )));
    }
    let reference_count = query_scalar_one!(
        db,
        i64,
        r#"
        SELECT
          (SELECT COUNT(*) FROM aircraft_sale_listing_avionics
           WHERE avionics_model_id = ? OR replaces_avionics_model_id = ?)
          + (SELECT COUNT(*) FROM aircraft_model_variant_default_avionics
             WHERE avionics_model_id = ?)
          + (SELECT COUNT(*) FROM avionics_suite_components
             WHERE suite_model_id = ? OR component_model_id = ?)
        "#,
        avionics_model_id,
        avionics_model_id,
        avionics_model_id,
        avionics_model_id,
        avionics_model_id,
    )?;
    if reference_count > 0 {
        return Err(AvionicsStoreError::Model(format!(
            "legacy avionics catalog row {avionics_model_id} resolved as {outcome} but still has {reference_count} association(s); explicit association remediation is required before it can be deleted"
        )));
    }
    execute_query!(
        db,
        "DELETE FROM avionics_models WHERE id = ? AND catalog_status = 'unreviewed'",
        avionics_model_id
    )?;
    if query_scalar_optional!(
        db,
        i64,
        "SELECT id FROM avionics_models WHERE id = ?",
        avionics_model_id
    )?
    .is_some()
    {
        return Err(AvionicsStoreError::Model(format!(
            "legacy avionics catalog row {avionics_model_id} changed during deletion; retry curation"
        )));
    }
    Ok(())
}

pub async fn enrich_model_year_avionics_and_price_points(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    limit: i64,
    value_reference_year: Option<i64>,
    refresh_existing: bool,
) -> StoreResult<AvionicsModelYearProfileReport> {
    if limit < 1 {
        return Err(AvionicsStoreError::Model(
            "limit must be at least 1".to_string(),
        ));
    }
    let value_reference_year = value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR);
    let candidates = aircraft_model_year_profile_candidates_to_enrich(db, refresh_existing).await?;
    let mut faa_admitted_candidate_count = 0usize;
    let mut faa_rejections = Vec::new();
    let mut grouped = BTreeMap::<(i64, i64), AircraftModelYearProfileRow>::new();
    for candidate in candidates {
        match require_listing_admission(db, candidate.listing_id).await {
            Ok(_) => {
                faa_admitted_candidate_count += 1;
                let key = (candidate.aircraft_model_variant_id, candidate.model_year);
                let row = grouped
                    .entry(key)
                    .or_insert_with(|| AircraftModelYearProfileRow {
                        aircraft_model_variant_id: candidate.aircraft_model_variant_id,
                        manufacturer: candidate.manufacturer,
                        model: candidate.model,
                        variant: candidate.variant,
                        model_year: candidate.model_year,
                        source_url: candidate.source_url.clone(),
                        listing_count: 0,
                    });
                row.listing_count += 1;
                if row.source_url.is_none() {
                    row.source_url = candidate.source_url;
                }
            }
            Err(error) => faa_rejections.push(error.to_string()),
        }
    }
    let mut rows = grouped.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .listing_count
            .cmp(&left.listing_count)
            .then_with(|| left.manufacturer.cmp(&right.manufacturer))
            .then_with(|| left.model.cmp(&right.model))
            .then_with(|| left.variant.cmp(&right.variant))
            .then_with(|| left.model_year.cmp(&right.model_year))
    });
    rows.truncate(limit as usize);
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let nearby_price_points =
            nearby_model_family_price_points(db, row.aircraft_model_variant_id, row.model_year)
                .await?;
        let response = extractor
            .estimate_default_aircraft_avionics(&DefaultAvionicsContext {
                manufacturer: &row.manufacturer,
                model: &row.model,
                variant: &row.variant,
                model_year: row.model_year,
                value_reference_year,
                source_url: row.source_url.as_deref(),
                nearby_price_points: &nearby_price_points,
            })
            .await?;
        let mut item =
            model_year_profile_item_from_response(&row, &nearby_price_points, &response)?;
        resolve_default_profile_identities(db, extractor, apply, &row, &mut item).await?;
        if apply {
            upsert_model_year_price_point(db, &item).await?;
            for avionics in &mut item.avionics {
                upsert_default_avionics_profile_item(
                    db,
                    row.aircraft_model_variant_id,
                    row.model_year,
                    value_reference_year,
                    avionics,
                )
                .await?;
            }
        }
        items.push(item);
    }

    Ok(AvionicsModelYearProfileReport {
        applied: apply,
        value_reference_year,
        faa_admitted_candidate_count,
        faa_rejected_candidate_count: faa_rejections.len(),
        faa_rejections,
        items,
    })
}

pub async fn enrich_model_year_avionics_and_price_point_for_listing(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    listing_id: i64,
    value_reference_year: Option<i64>,
    refresh_existing: bool,
) -> StoreResult<Option<AvionicsModelYearProfileItem>> {
    let value_reference_year = value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR);
    require_listing_admission(db, listing_id)
        .await
        .map_err(aircraft_admission_store_error)?;
    let Some(row) =
        aircraft_model_year_profile_for_listing(db, listing_id, refresh_existing).await?
    else {
        return Ok(None);
    };
    let nearby_price_points =
        nearby_model_family_price_points(db, row.aircraft_model_variant_id, row.model_year).await?;
    let response = extractor
        .estimate_default_aircraft_avionics(&DefaultAvionicsContext {
            manufacturer: &row.manufacturer,
            model: &row.model,
            variant: &row.variant,
            model_year: row.model_year,
            value_reference_year,
            source_url: row.source_url.as_deref(),
            nearby_price_points: &nearby_price_points,
        })
        .await?;
    let mut item = model_year_profile_item_from_response(&row, &nearby_price_points, &response)?;
    resolve_default_profile_identities(db, extractor, apply, &row, &mut item).await?;
    if apply {
        upsert_model_year_price_point(db, &item).await?;
        for avionics in &mut item.avionics {
            upsert_default_avionics_profile_item(
                db,
                row.aircraft_model_variant_id,
                row.model_year,
                value_reference_year,
                avionics,
            )
            .await?;
        }
    }
    Ok(Some(item))
}

async fn avionics_models_for_normalization(
    db: &AppDb,
) -> StoreResult<Vec<AvionicsModelNormalizeRow>> {
    let capabilities = avionics_capability_map(db).await?;
    let rows = query_as_all!(
        db,
        AvionicsModelNormalizeDbRow,
        r#"
        SELECT
          model.id,
          model.avionics_manufacturer_id,
          mfr.name AS manufacturer,
          model.name,
          model.normalized_name,
          model.introduced_year,
          model.estimated_unit_value_usd,
          model.value_basis,
          model.replacement_cost_usd,
          model.value_reference_year,
          model.value_source,
          model.valuation_scope
        FROM avionics_models model
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        WHERE model.catalog_status = 'unreviewed'
        ORDER BY model.id
        "#
    )?;
    rows.into_iter()
        .map(|row| {
            Ok(AvionicsModelNormalizeRow {
                id: row.id,
                avionics_manufacturer_id: row.avionics_manufacturer_id,
                manufacturer: row.manufacturer,
                avionics_types: required_model_capabilities(&capabilities, row.id)?,
                name: row.name,
                normalized_name: row.normalized_name,
                introduced_year: row.introduced_year,
                estimated_unit_value_usd: row.estimated_unit_value_usd,
                value_basis: row.value_basis,
                replacement_cost_usd: row.replacement_cost_usd,
                value_reference_year: row.value_reference_year,
                value_source: row.value_source,
                valuation_scope: row.valuation_scope,
            })
        })
        .collect()
}

async fn avionics_models_for_gemini_normalization(
    db: &AppDb,
    limit: i64,
) -> StoreResult<Vec<AvionicsNormalizationInputRow>> {
    let capabilities = avionics_capability_map(db).await?;
    let rows = query_as_all!(
        db,
        AvionicsNormalizationInputDbRow,
        r#"
        SELECT
          model.id,
          mfr.name AS manufacturer,
          model.name AS model,
          model.normalized_name AS normalized_model,
          COUNT(link.id) AS listing_count,
          model.introduced_year
        FROM avionics_models model
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        LEFT JOIN aircraft_sale_listing_avionics link
          ON link.avionics_model_id = model.id
        WHERE model.catalog_status = 'unreviewed'
        GROUP BY
          model.id,
          mfr.name,
          model.name,
          model.normalized_name,
          model.introduced_year
        ORDER BY mfr.name, listing_count DESC, model.name
        LIMIT ?
        "#,
        limit
    )?;
    rows.into_iter()
        .map(|row| {
            Ok(AvionicsNormalizationInputRow {
                id: row.id,
                manufacturer: row.manufacturer,
                avionics_types: required_model_capabilities(&capabilities, row.id)?,
                model: row.model,
                normalized_model: row.normalized_model,
                listing_count: row.listing_count,
                introduced_year: row.introduced_year,
            })
        })
        .collect()
}

async fn aircraft_model_year_profile_candidates_to_enrich(
    db: &AppDb,
    refresh_existing: bool,
) -> StoreResult<Vec<AircraftModelYearProfileCandidateRow>> {
    let predicate = if refresh_existing {
        "1 = 1"
    } else {
        r#"
        (
          NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_price_points price_point
            WHERE price_point.aircraft_model_variant_id = variant.id
              AND price_point.model_year = listing.model_year
              AND price_point.source_confidence = 'high'
              AND price_point.evidence_kind = 'direct_model_year'
              AND price_point.is_valuation_eligible = TRUE
              AND price_point.purchase_price_reference_year = price_point.model_year
          )
          OR NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_default_avionics default_avionics
            JOIN avionics_models model
              ON model.id = default_avionics.avionics_model_id
            WHERE default_avionics.aircraft_model_variant_id = variant.id
              AND default_avionics.model_year = listing.model_year
              AND default_avionics.source_confidence = 'high'
              AND default_avionics.quantity > 0
              AND TRIM(default_avionics.source_url) <> ''
              AND LOWER(default_avionics.source_url) NOT LIKE '%/listing/%'
              AND LOWER(default_avionics.source_url) NOT LIKE '%/listings/%'
              AND LOWER(default_avionics.source_url) NOT LIKE '%/aircraft-for-sale/%'
              AND LOWER(default_avionics.source_url) NOT LIKE '%/classifieds/%'
              AND model.introduced_year IS NOT NULL
              AND model.estimated_unit_value_usd >= 0
              AND model.value_basis = 'installed_contribution'
              AND model.replacement_cost_usd >= model.estimated_unit_value_usd
              AND model.value_reference_year BETWEEN 1900 AND 2200
              AND model.value_source IS NOT NULL
              AND TRIM(model.value_source) <> ''
              AND (
                model.valuation_scope = 'unit'
                OR (
                  model.valuation_scope = 'integrated_suite'
                  AND EXISTS (
                    SELECT 1
                    FROM avionics_suite_components suite_component
                    WHERE suite_component.suite_model_id = model.id
                  )
                )
              )
          )
        )
        "#
    };
    let sql = format!(
        r#"
        SELECT
          listing.id AS listing_id,
          variant.id AS aircraft_model_variant_id,
          mfr.name AS manufacturer,
          model.name AS model,
          variant.name AS variant,
          listing.model_year,
          listing.source_url
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE listing.ingestion_state = 'ready'
          AND {predicate}
        ORDER BY mfr.name, model.name, variant.name, listing.model_year, listing.id
        "#
    );
    Ok(query_as_all!(
        db,
        AircraftModelYearProfileCandidateRow,
        &sql
    )?)
}

async fn aircraft_model_year_profile_for_listing(
    db: &AppDb,
    listing_id: i64,
    refresh_existing: bool,
) -> StoreResult<Option<AircraftModelYearProfileRow>> {
    let predicate = if refresh_existing {
        "1 = 1"
    } else {
        r#"
        (
          NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_price_points price_point
            WHERE price_point.aircraft_model_variant_id = variant.id
              AND price_point.model_year = listing.model_year
              AND price_point.source_confidence = 'high'
              AND price_point.evidence_kind = 'direct_model_year'
              AND price_point.is_valuation_eligible = TRUE
              AND price_point.purchase_price_reference_year = price_point.model_year
          )
          OR NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_default_avionics default_avionics
            JOIN avionics_models model
              ON model.id = default_avionics.avionics_model_id
            WHERE default_avionics.aircraft_model_variant_id = variant.id
              AND default_avionics.model_year = listing.model_year
              AND default_avionics.source_confidence = 'high'
              AND default_avionics.quantity > 0
              AND TRIM(default_avionics.source_url) <> ''
              AND LOWER(default_avionics.source_url) NOT LIKE '%/listing/%'
              AND LOWER(default_avionics.source_url) NOT LIKE '%/listings/%'
              AND LOWER(default_avionics.source_url) NOT LIKE '%/aircraft-for-sale/%'
              AND LOWER(default_avionics.source_url) NOT LIKE '%/classifieds/%'
              AND model.introduced_year IS NOT NULL
              AND model.estimated_unit_value_usd >= 0
              AND model.value_basis = 'installed_contribution'
              AND model.replacement_cost_usd >= model.estimated_unit_value_usd
              AND model.value_reference_year BETWEEN 1900 AND 2200
              AND model.value_source IS NOT NULL
              AND TRIM(model.value_source) <> ''
              AND (
                model.valuation_scope = 'unit'
                OR (
                  model.valuation_scope = 'integrated_suite'
                  AND EXISTS (
                    SELECT 1
                    FROM avionics_suite_components suite_component
                    WHERE suite_component.suite_model_id = model.id
                  )
                )
              )
          )
        )
        "#
    };
    let sql = format!(
        r#"
        SELECT
          variant.id AS aircraft_model_variant_id,
          mfr.name AS manufacturer,
          model.name AS model,
          variant.name AS variant,
          listing.model_year,
          listing.source_url,
          1 AS listing_count
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE listing.id = ?
          AND listing.ingestion_state <> 'quarantined'
          AND {predicate}
        LIMIT 1
        "#
    );
    Ok(
        query_as_all!(db, AircraftModelYearProfileRow, &sql, listing_id)?
            .into_iter()
            .next(),
    )
}

async fn nearby_model_family_price_points(
    db: &AppDb,
    aircraft_model_variant_id: i64,
    model_year: i64,
) -> StoreResult<Vec<AircraftPricePointContext>> {
    let rows = query_as_all!(
        db,
        NearbyAircraftPricePointRow,
        r#"
        SELECT
          variant.name AS variant,
          price_point.model_year,
          price_point.purchase_price_new_usd,
          price_point.purchase_price_reference_year,
          price_point.source_title,
          price_point.source_confidence
        FROM aircraft_model_variant_price_points price_point
        JOIN aircraft_model_variants variant
          ON variant.id = price_point.aircraft_model_variant_id
        WHERE variant.aircraft_model_id = (
            SELECT source_variant.aircraft_model_id
            FROM aircraft_model_variants source_variant
            WHERE source_variant.id = ?
          )
          AND NOT (
            price_point.aircraft_model_variant_id = ?
            AND price_point.model_year = ?
          )
          AND price_point.model_year BETWEEN ? AND ?
          AND price_point.source_confidence = 'high'
          AND price_point.evidence_kind = 'direct_model_year'
          AND price_point.is_valuation_eligible = TRUE
        ORDER BY ABS(price_point.model_year - ?), price_point.model_year
        LIMIT 8
        "#,
        aircraft_model_variant_id,
        aircraft_model_variant_id,
        model_year,
        model_year - 5,
        model_year + 5,
        model_year
    )?;
    Ok(rows
        .into_iter()
        .map(|row| AircraftPricePointContext {
            variant: row.variant,
            model_year: row.model_year,
            purchase_price_new_usd: row.purchase_price_new_usd,
            purchase_price_reference_year: row.purchase_price_reference_year,
            source_title: row.source_title,
            source_confidence: row.source_confidence,
        })
        .collect())
}

fn model_year_profile_item_from_response(
    row: &AircraftModelYearProfileRow,
    nearby_price_points: &[AircraftPricePointContext],
    response: &Value,
) -> StoreResult<AvionicsModelYearProfileItem> {
    let purchase_price_new_usd = required_min_f64(response, "purchase_price_new_usd", 10_000.0)?;
    let purchase_price_reference_year = required_year(response, "purchase_price_reference_year")?;
    let price_source_url = required_string(response, "price_source_url")?;
    if !(price_source_url.starts_with("https://") || price_source_url.starts_with("http://")) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini model-year profile price_source_url must be http(s): {price_source_url}"
        )));
    }
    if row.source_url.as_deref() == Some(price_source_url.as_str())
        || looks_like_used_listing_url(&price_source_url)
    {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini model-year profile price_source_url must cite a new-price reference, not an ordinary listing URL: {price_source_url}"
        )));
    }
    let price_source_title = required_string(response, "price_source_title")?;
    let price_source_notes = required_string(response, "price_source_notes")?;
    let price_source_confidence = required_confidence(response, "price_source_confidence")?;
    let price_evidence_kind = required_price_evidence_kind(response, "price_evidence_kind")?;
    let price_discontinuity_explanation = response
        .get("price_discontinuity_explanation")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let mut price_eligibility_notes = Vec::new();
    if price_source_confidence != "high" {
        price_eligibility_notes.push("source confidence is not high".to_string());
    }
    if price_evidence_kind != "direct_model_year" {
        price_eligibility_notes.push(format!(
            "evidence kind is {price_evidence_kind}, not direct_model_year"
        ));
    }
    if purchase_price_reference_year != row.model_year {
        price_eligibility_notes.push(format!(
            "price reference year {purchase_price_reference_year} does not equal model year {}",
            row.model_year
        ));
    }
    if price_evidence_kind == "direct_model_year"
        && !format!("{price_source_title} {price_source_notes}")
            .contains(&row.model_year.to_string())
    {
        price_eligibility_notes.push(
            "direct evidence title/notes do not identify the requested model year".to_string(),
        );
    }
    if price_evidence_kind == "direct_model_year"
        && url::Url::parse(&price_source_url)
            .ok()
            .is_none_or(|url| url.path().trim_matches('/').is_empty())
    {
        price_eligibility_notes.push("direct evidence URL is only a site homepage".to_string());
    }
    if has_material_price_discontinuity(
        &row.variant,
        row.model_year,
        purchase_price_new_usd,
        nearby_price_points,
    ) && price_discontinuity_explanation.is_none()
    {
        price_eligibility_notes.push(
            "price has an unexplained greater than 35% annualized discontinuity versus an adjacent eligible point"
                .to_string(),
        );
    }
    let avionics = response
        .get("avionics")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AvionicsStoreError::Model(
                "Gemini model-year profile response missing avionics".to_string(),
            )
        })?
        .iter()
        .map(|value| default_avionics_item_from_response(value, row.model_year))
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(AvionicsModelYearProfileItem {
        aircraft_model_variant_id: row.aircraft_model_variant_id,
        manufacturer: row.manufacturer.clone(),
        model: row.model.clone(),
        variant: row.variant.clone(),
        model_year: row.model_year,
        listing_count: row.listing_count,
        purchase_price_new_usd,
        purchase_price_reference_year,
        price_source_url,
        price_source_title,
        price_source_notes,
        price_source_confidence,
        price_evidence_kind,
        price_discontinuity_explanation,
        is_price_valuation_eligible: price_eligibility_notes.is_empty(),
        price_eligibility_notes,
        avionics,
    })
}

fn has_material_price_discontinuity(
    variant: &str,
    model_year: i64,
    purchase_price_new_usd: f64,
    nearby_price_points: &[AircraftPricePointContext],
) -> bool {
    nearby_price_points.iter().any(|point| {
        if normalize_name(&point.variant) != normalize_name(variant) {
            return false;
        }
        let year_gap = (point.model_year - model_year).abs();
        if !(1..=3).contains(&year_gap) || point.purchase_price_new_usd <= 0.0 {
            return false;
        }
        let ratio = (purchase_price_new_usd / point.purchase_price_new_usd)
            .max(point.purchase_price_new_usd / purchase_price_new_usd);
        ratio.powf(1.0 / year_gap as f64) - 1.0 > 0.35
    })
}

fn looks_like_used_listing_url(url: &str) -> bool {
    let path = url::Url::parse(url)
        .ok()
        .map(|url| url.path().to_ascii_lowercase())
        .unwrap_or_else(|| url.to_ascii_lowercase());
    path.contains("/listing/")
        || path.contains("/listings/")
        || path.contains("/aircraft-for-sale/")
        || path.contains("/classifieds/")
}

fn default_avionics_item_from_response(
    value: &Value,
    model_year: i64,
) -> StoreResult<AvionicsModelYearProfileAvionicsItem> {
    let quantity = required_i64(value, "quantity")?;
    if quantity < 1 {
        return Err(AvionicsStoreError::Model(
            "Gemini default avionics quantity must be at least 1".to_string(),
        ));
    }
    let introduced_year = required_year(value, "introduced_year")?;
    if introduced_year > model_year {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini default avionics introduction year {introduced_year} is after aircraft model year {model_year}"
        )));
    }
    let compatibility_value = required_min_f64(value, "estimated_unit_value_usd", 0.0)?;
    let installed_value_contribution_usd =
        required_min_f64(value, "installed_value_contribution_usd", 0.0)?;
    let replacement_cost_usd = required_min_f64(value, "replacement_cost_usd", 0.0)?;
    validate_avionics_values(
        compatibility_value,
        installed_value_contribution_usd,
        replacement_cost_usd,
    )?;
    let valuation_scope = required_valuation_scope(value, "valuation_scope")?;
    let manufacturer = required_string(value, "manufacturer")?;
    let model = required_string(value, "model")?;
    let identity = identity_evidence_from_response(value)?;
    let included_components =
        included_components_from_response(value, &manufacturer, &model, valuation_scope.as_str())?;
    let source_url = required_string(value, "source_url")?;
    if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini default avionics source_url must be http(s): {source_url}"
        )));
    }
    if looks_like_used_listing_url(&source_url) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini default avionics source_url must cite factory/reference evidence, not an ordinary sale listing: {source_url}"
        )));
    }
    Ok(AvionicsModelYearProfileAvionicsItem {
        avionics_model_id: 0,
        manufacturer,
        model,
        avionics_types: required_string_array(value, "types")?,
        quantity,
        introduced_year,
        installed_value_contribution_usd,
        replacement_cost_usd,
        valuation_scope,
        included_components,
        identity,
        confidence: required_confidence(value, "confidence")?,
        source_url,
        source_title: required_string(value, "source_title")?,
        notes: required_string(value, "notes")?,
    })
}

async fn upsert_model_year_price_point(
    db: &AppDb,
    item: &AvionicsModelYearProfileItem,
) -> StoreResult<()> {
    let existing = query_as_all!(
        db,
        StoredPricePointQualityRow,
        r#"
        SELECT source_confidence, evidence_kind, is_valuation_eligible
        FROM aircraft_model_variant_price_points
        WHERE aircraft_model_variant_id = ? AND model_year = ?
        LIMIT 1
        "#,
        item.aircraft_model_variant_id,
        item.model_year
    )?
    .into_iter()
    .next();
    if existing.is_some_and(|existing| {
        !price_evidence_is_stronger(
            item.price_source_confidence.as_str(),
            item.price_evidence_kind.as_str(),
            item.is_price_valuation_eligible,
            existing.source_confidence.as_str(),
            existing.evidence_kind.as_str(),
            existing.is_valuation_eligible,
        )
    }) {
        return Ok(());
    }
    let source_notes = match &item.price_discontinuity_explanation {
        Some(explanation) => format!(
            "{} Discontinuity evidence: {explanation}",
            item.price_source_notes
        ),
        None => item.price_source_notes.clone(),
    };
    execute_query!(
        db,
        r#"
        INSERT INTO aircraft_model_variant_price_points (
          aircraft_model_variant_id,
          model_year,
          purchase_price_new_usd,
          purchase_price_reference_year,
          source_url,
          source_title,
          source_notes,
          source_confidence,
          evidence_kind,
          is_valuation_eligible
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (aircraft_model_variant_id, model_year) DO UPDATE SET
          purchase_price_new_usd = excluded.purchase_price_new_usd,
          purchase_price_reference_year = excluded.purchase_price_reference_year,
          source_url = excluded.source_url,
          source_title = excluded.source_title,
          source_notes = excluded.source_notes,
          source_confidence = excluded.source_confidence,
          evidence_kind = excluded.evidence_kind,
          is_valuation_eligible = excluded.is_valuation_eligible,
          updated_at = CURRENT_TIMESTAMP
        WHERE aircraft_model_variant_price_points.is_valuation_eligible = FALSE
        "#,
        item.aircraft_model_variant_id,
        item.model_year,
        item.purchase_price_new_usd,
        item.purchase_price_reference_year,
        item.price_source_url.as_str(),
        item.price_source_title.as_str(),
        source_notes.as_str(),
        item.price_source_confidence.as_str(),
        item.price_evidence_kind.as_str(),
        item.is_price_valuation_eligible,
    )?;
    Ok(())
}

fn price_evidence_is_stronger(
    candidate_confidence: &str,
    candidate_evidence_kind: &str,
    candidate_eligible: bool,
    existing_confidence: &str,
    existing_evidence_kind: &str,
    existing_eligible: bool,
) -> bool {
    let quality = |confidence: &str, evidence_kind: &str, eligible: bool| {
        (
            u8::from(eligible),
            match confidence {
                "high" => 3_u8,
                "medium" => 2,
                "low" => 1,
                _ => 0,
            },
            match evidence_kind {
                "direct_model_year" => 4_u8,
                "direct_other_year" => 3,
                "interpolated" => 2,
                "inferred" => 1,
                _ => 0,
            },
        )
    };
    quality(
        candidate_confidence,
        candidate_evidence_kind,
        candidate_eligible,
    ) > quality(
        existing_confidence,
        existing_evidence_kind,
        existing_eligible,
    )
}

async fn upsert_default_avionics_profile_item(
    db: &AppDb,
    aircraft_model_variant_id: i64,
    model_year: i64,
    value_reference_year: i64,
    item: &mut AvionicsModelYearProfileAvionicsItem,
) -> StoreResult<()> {
    let avionics_model_id = item.avionics_model_id;
    require_approved_catalog_model(db, avionics_model_id).await?;
    update_avionics_model_metadata(
        db,
        avionics_model_id,
        item.introduced_year,
        item.installed_value_contribution_usd,
        item.replacement_cost_usd,
        item.valuation_scope.as_str(),
        value_reference_year,
        "gemini-grounded",
        item.confidence.as_str(),
    )
    .await?;
    execute_query!(
        db,
        r#"
        INSERT INTO aircraft_model_variant_default_avionics (
          aircraft_model_variant_id,
          model_year,
          avionics_model_id,
          quantity,
          source_url,
          source_title,
          source_notes,
          source_confidence
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (aircraft_model_variant_id, model_year, avionics_model_id) DO UPDATE SET
          quantity = excluded.quantity,
          source_url = excluded.source_url,
          source_title = excluded.source_title,
          source_notes = excluded.source_notes,
          source_confidence = excluded.source_confidence,
          updated_at = CURRENT_TIMESTAMP
        "#,
        aircraft_model_variant_id,
        model_year,
        avionics_model_id,
        item.quantity,
        item.source_url.as_str(),
        item.source_title.as_str(),
        item.notes.as_str(),
        item.confidence.as_str(),
    )?;
    if item.confidence == "high" {
        replace_suite_memberships(
            db,
            avionics_model_id,
            item.valuation_scope.as_str(),
            &item.included_components,
        )
        .await?;
    }
    Ok(())
}

async fn apply_avionics_normalization_group(
    db: &AppDb,
    canonical: &AvionicsModelNormalizeRow,
    canonical_manufacturer_id: i64,
    canonical_name: &str,
    canonical_normalized_name: &str,
    rows: &[AvionicsModelNormalizeRow],
) -> StoreResult<()> {
    for row in rows {
        let status = query_scalar_optional!(
            db,
            String,
            "SELECT catalog_status FROM avionics_models WHERE id = ?",
            row.id
        )?;
        if status.as_deref() != Some("unreviewed") {
            return Err(AvionicsStoreError::Model(format!(
                "legacy mechanical normalization refuses catalog row {} because it is not unreviewed",
                row.id
            )));
        }
    }
    for row in rows.iter().filter(|row| row.id != canonical.id) {
        execute_query!(
            db,
            r#"
            INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
            SELECT ?, membership.avionics_type_id
            FROM avionics_model_types membership
            WHERE membership.avionics_model_id = ?
            ON CONFLICT (avionics_model_id, avionics_type_id) DO NOTHING
            "#,
            canonical.id,
            row.id
        )?;
        let links = avionics_listing_links_for_model(db, row.id).await?;
        for link in links {
            upsert_listing_avionics_link(db, canonical.id, &link).await?;
        }
        execute_query!(
            db,
            "DELETE FROM aircraft_sale_listing_avionics WHERE avionics_model_id = ?",
            row.id
        )?;

        let default_links = avionics_default_profile_links_for_model(db, row.id).await?;
        for link in default_links {
            upsert_default_avionics_profile_link(db, canonical.id, &link).await?;
        }
        execute_query!(
            db,
            "DELETE FROM aircraft_model_variant_default_avionics WHERE avionics_model_id = ?",
            row.id
        )?;

        rewire_suite_memberships(db, row.id, canonical.id).await?;
        execute_query!(
            db,
            "UPDATE aircraft_sale_listing_avionics SET replaces_avionics_model_id = ? WHERE replaces_avionics_model_id = ?",
            canonical.id,
            row.id
        )?;

        execute_query!(
            db,
            "DELETE FROM avionics_models WHERE id = ? AND catalog_status = 'unreviewed'",
            row.id
        )?;
    }

    let introduced_year = consensus_year(rows.iter().filter_map(|row| row.introduced_year));
    let installed_value_contribution_usd = consensus_money(rows.iter().filter_map(|row| {
        (row.value_basis == "installed_contribution")
            .then_some(row.estimated_unit_value_usd)
            .flatten()
    }));
    let replacement_cost_usd = consensus_money(rows.iter().filter_map(|row| {
        row.replacement_cost_usd.or_else(|| {
            (row.value_basis == "replacement_cost")
                .then_some(row.estimated_unit_value_usd)
                .flatten()
        })
    }));
    let value_reference_year = consensus_year(
        rows.iter()
            .filter(|row| row.value_basis != "unreviewed" || row.replacement_cost_usd.is_some())
            .filter_map(|row| row.value_reference_year),
    );
    let value_source = (installed_value_contribution_usd.is_some()
        || replacement_cost_usd.is_some())
    .then(|| {
        canonical
            .value_source
            .clone()
            .or_else(|| rows.iter().find_map(|row| row.value_source.clone()))
    })
    .flatten();
    let value_basis = if installed_value_contribution_usd.is_some() {
        "installed_contribution"
    } else if replacement_cost_usd.is_some() {
        "replacement_cost"
    } else {
        "unreviewed"
    };
    let valuation_scope = if rows
        .iter()
        .any(|row| row.valuation_scope == "integrated_suite")
    {
        "integrated_suite"
    } else {
        "unit"
    };

    execute_query!(
        db,
        r#"
        UPDATE avionics_models
        SET
          avionics_manufacturer_id = ?,
          name = ?,
          normalized_name = ?,
          introduced_year = ?,
          estimated_unit_value_usd = ?,
          value_basis = ?,
          replacement_cost_usd = ?,
          value_reference_year = ?,
          value_source = ?,
          valuation_scope = ?,
          updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND catalog_status = 'unreviewed'
        "#,
        canonical_manufacturer_id,
        canonical_name.trim(),
        canonical_normalized_name,
        introduced_year,
        installed_value_contribution_usd,
        value_basis,
        replacement_cost_usd,
        value_reference_year,
        value_source.as_deref(),
        valuation_scope,
        canonical.id
    )?;
    Ok(())
}

fn consensus_year(values: impl Iterator<Item = i64>) -> Option<i64> {
    let mut values = values.collect::<Vec<_>>();
    values.sort_unstable();
    let first = *values.first()?;
    let last = *values.last()?;
    (last - first <= 2).then_some(values[values.len() / 2])
}

fn consensus_money(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut values = values
        .filter(|value| value.is_finite() && *value >= 0.0)
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.total_cmp(right));
    let first = *values.first()?;
    let last = *values.last()?;
    let median = values[values.len() / 2];
    let tolerance = (median * 0.25).max(250.0);
    (last - first <= tolerance).then_some(median)
}

async fn rewire_suite_memberships(
    db: &AppDb,
    source_model_id: i64,
    canonical_model_id: i64,
) -> StoreResult<()> {
    let links = query_as_all!(
        db,
        AvionicsSuiteLinkRow,
        r#"
        SELECT suite_model_id, component_model_id, quantity
        FROM avionics_suite_components
        WHERE suite_model_id = ? OR component_model_id = ?
        "#,
        source_model_id,
        source_model_id
    )?;
    execute_query!(
        db,
        "DELETE FROM avionics_suite_components WHERE suite_model_id = ? OR component_model_id = ?",
        source_model_id,
        source_model_id
    )?;
    for link in links {
        let suite_model_id = if link.suite_model_id == source_model_id {
            canonical_model_id
        } else {
            link.suite_model_id
        };
        let component_model_id = if link.component_model_id == source_model_id {
            canonical_model_id
        } else {
            link.component_model_id
        };
        if suite_model_id == component_model_id {
            continue;
        }
        execute_query!(
            db,
            r#"
            INSERT INTO avionics_suite_components (
              suite_model_id, component_model_id, quantity
            )
            VALUES (?, ?, ?)
            ON CONFLICT (suite_model_id, component_model_id) DO UPDATE SET
              quantity = CASE
                WHEN excluded.quantity > avionics_suite_components.quantity
                THEN excluded.quantity
                ELSE avionics_suite_components.quantity
              END
            "#,
            suite_model_id,
            component_model_id,
            link.quantity.max(1)
        )?;
    }
    Ok(())
}

async fn avionics_listing_links_for_model(
    db: &AppDb,
    avionics_model_id: i64,
) -> StoreResult<Vec<AvionicsListingLinkRow>> {
    Ok(query_as_all!(
        db,
        AvionicsListingLinkRow,
        r#"
        SELECT
          aircraft_sale_listing_id,
          quantity,
          source,
          source_notes,
          configuration_action,
          replaces_avionics_model_id,
          source_confidence
        FROM aircraft_sale_listing_avionics
        WHERE avionics_model_id = ?
        "#,
        avionics_model_id
    )?)
}

async fn avionics_default_profile_links_for_model(
    db: &AppDb,
    avionics_model_id: i64,
) -> StoreResult<Vec<AvionicsDefaultProfileLinkRow>> {
    Ok(query_as_all!(
        db,
        AvionicsDefaultProfileLinkRow,
        r#"
        SELECT
          aircraft_model_variant_id,
          model_year,
          quantity,
          source_url,
          source_title,
          source_notes,
          source_confidence
        FROM aircraft_model_variant_default_avionics
        WHERE avionics_model_id = ?
        "#,
        avionics_model_id
    )?)
}

async fn upsert_listing_avionics_link(
    db: &AppDb,
    avionics_model_id: i64,
    link: &AvionicsListingLinkRow,
) -> StoreResult<()> {
    let existing_quantity = query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT quantity
        FROM aircraft_sale_listing_avionics
        WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?
        "#,
        link.aircraft_sale_listing_id,
        avionics_model_id
    )?;
    match existing_quantity {
        Some(existing_quantity) => {
            execute_query!(
                db,
                r#"
                UPDATE aircraft_sale_listing_avionics
                SET
                  quantity = ?,
                  source = CASE
                    WHEN configuration_action = 'installed' AND ? <> 'installed' THEN ?
                    ELSE source
                  END,
                  source_notes = CASE
                    WHEN configuration_action = 'installed' AND ? <> 'installed' THEN ?
                    ELSE source_notes
                  END,
                  configuration_action = CASE
                    WHEN configuration_action = 'installed' AND ? <> 'installed' THEN ?
                    ELSE configuration_action
                  END,
                  replaces_avionics_model_id = CASE
                    WHEN configuration_action = 'installed' AND ? <> 'installed' THEN ?
                    ELSE replaces_avionics_model_id
                  END,
                  source_confidence = CASE
                    WHEN source_confidence IS NULL OR source_confidence <> 'high' THEN ?
                    ELSE source_confidence
                  END,
                  updated_at = CURRENT_TIMESTAMP
                WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?
                "#,
                existing_quantity.max(link.quantity),
                link.configuration_action.as_str(),
                link.source.as_str(),
                link.configuration_action.as_str(),
                link.source_notes.as_deref(),
                link.configuration_action.as_str(),
                link.configuration_action.as_str(),
                link.configuration_action.as_str(),
                link.replaces_avionics_model_id,
                link.source_confidence.as_deref(),
                link.aircraft_sale_listing_id,
                avionics_model_id
            )?;
        }
        None => {
            execute_query!(
                db,
                r#"
                INSERT INTO aircraft_sale_listing_avionics (
                  aircraft_sale_listing_id,
                  avionics_model_id,
                  quantity,
                  source,
                  source_notes,
                  configuration_action,
                  replaces_avionics_model_id,
                  source_confidence
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                link.aircraft_sale_listing_id,
                avionics_model_id,
                link.quantity.max(1),
                link.source.as_str(),
                link.source_notes.as_deref(),
                link.configuration_action.as_str(),
                link.replaces_avionics_model_id,
                link.source_confidence.as_deref()
            )?;
        }
    }
    Ok(())
}

async fn upsert_default_avionics_profile_link(
    db: &AppDb,
    avionics_model_id: i64,
    link: &AvionicsDefaultProfileLinkRow,
) -> StoreResult<()> {
    let existing_quantity = query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT quantity
        FROM aircraft_model_variant_default_avionics
        WHERE aircraft_model_variant_id = ?
          AND model_year = ?
          AND avionics_model_id = ?
        "#,
        link.aircraft_model_variant_id,
        link.model_year,
        avionics_model_id
    )?;
    match existing_quantity {
        Some(existing_quantity) => {
            execute_query!(
                db,
                r#"
                UPDATE aircraft_model_variant_default_avionics
                SET
                  quantity = ?,
                  source_url = ?,
                  source_title = ?,
                  source_notes = ?,
                  source_confidence = ?,
                  updated_at = CURRENT_TIMESTAMP
                WHERE aircraft_model_variant_id = ?
                  AND model_year = ?
                  AND avionics_model_id = ?
                "#,
                existing_quantity.max(link.quantity).max(1),
                link.source_url.as_str(),
                link.source_title.as_str(),
                link.source_notes.as_str(),
                link.source_confidence.as_str(),
                link.aircraft_model_variant_id,
                link.model_year,
                avionics_model_id
            )?;
        }
        None => {
            execute_query!(
                db,
                r#"
                INSERT INTO aircraft_model_variant_default_avionics (
                  aircraft_model_variant_id,
                  model_year,
                  avionics_model_id,
                  quantity,
                  source_url,
                  source_title,
                  source_notes,
                  source_confidence
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                link.aircraft_model_variant_id,
                link.model_year,
                avionics_model_id,
                link.quantity.max(1),
                link.source_url.as_str(),
                link.source_title.as_str(),
                link.source_notes.as_str(),
                link.source_confidence.as_str()
            )?;
        }
    }
    Ok(())
}

async fn avionics_models_to_enrich(
    db: &AppDb,
    limit: i64,
    refresh_existing: bool,
) -> StoreResult<Vec<AvionicsModelReferenceRow>> {
    let capabilities = avionics_capability_map(db).await?;
    let rows = if refresh_existing {
        query_as_all!(
            db,
            AvionicsModelReferenceDbRow,
            r#"
            SELECT
              model.id,
              mfr.name AS manufacturer,
              model.name AS model,
              model.introduced_year,
              model.estimated_unit_value_usd,
              model.replacement_cost_usd,
              model.valuation_scope
            FROM avionics_models model
            JOIN avionics_manufacturers mfr
              ON mfr.id = model.avionics_manufacturer_id
            WHERE model.catalog_status <> 'rejected'
            ORDER BY model.id
            LIMIT ?
            "#,
            limit
        )?
    } else {
        query_as_all!(
            db,
            AvionicsModelReferenceDbRow,
            r#"
            SELECT
              model.id,
              mfr.name AS manufacturer,
              model.name AS model,
              model.introduced_year,
              model.estimated_unit_value_usd,
              model.replacement_cost_usd,
              model.valuation_scope
            FROM avionics_models model
            JOIN avionics_manufacturers mfr
              ON mfr.id = model.avionics_manufacturer_id
            WHERE model.catalog_status <> 'rejected'
              AND (
                model.introduced_year IS NULL
                OR model.estimated_unit_value_usd IS NULL
                OR model.value_basis <> 'installed_contribution'
                OR model.replacement_cost_usd IS NULL
                OR model.value_reference_year IS NULL
                OR model.value_source IS NULL
                OR TRIM(model.value_source) = ''
                OR (
                  model.valuation_scope = 'integrated_suite'
                  AND NOT EXISTS (
                    SELECT 1 FROM avionics_suite_components membership
                    WHERE membership.suite_model_id = model.id
                  )
                )
              )
            ORDER BY model.id
            LIMIT ?
            "#,
            limit
        )?
    };
    hydrate_reference_rows(rows, &capabilities)
}

async fn listing_avionics_models_to_enrich(
    db: &AppDb,
    listing_id: i64,
    refresh_existing: bool,
) -> StoreResult<Vec<AvionicsModelReferenceRow>> {
    let capabilities = avionics_capability_map(db).await?;
    let rows = if refresh_existing {
        query_as_all!(
            db,
            AvionicsModelReferenceDbRow,
            r#"
            SELECT DISTINCT
              model.id,
              mfr.name AS manufacturer,
              model.name AS model,
              model.introduced_year,
              model.estimated_unit_value_usd,
              model.replacement_cost_usd,
              model.valuation_scope
            FROM aircraft_sale_listing_avionics link
            JOIN avionics_models model
              ON model.id = link.avionics_model_id
            JOIN avionics_manufacturers mfr
              ON mfr.id = model.avionics_manufacturer_id
            WHERE link.aircraft_sale_listing_id = ?
              AND model.catalog_status <> 'rejected'
            ORDER BY model.id
            "#,
            listing_id
        )?
    } else {
        query_as_all!(
            db,
            AvionicsModelReferenceDbRow,
            r#"
            SELECT DISTINCT
              model.id,
              mfr.name AS manufacturer,
              model.name AS model,
              model.introduced_year,
              model.estimated_unit_value_usd,
              model.replacement_cost_usd,
              model.valuation_scope
            FROM aircraft_sale_listing_avionics link
            JOIN avionics_models model
              ON model.id = link.avionics_model_id
            JOIN avionics_manufacturers mfr
              ON mfr.id = model.avionics_manufacturer_id
            WHERE link.aircraft_sale_listing_id = ?
              AND model.catalog_status <> 'rejected'
              AND (
                model.introduced_year IS NULL
                OR model.estimated_unit_value_usd IS NULL
                OR model.value_basis <> 'installed_contribution'
                OR model.replacement_cost_usd IS NULL
                OR model.value_reference_year IS NULL
                OR model.value_source IS NULL
                OR TRIM(model.value_source) = ''
                OR (
                  model.valuation_scope = 'integrated_suite'
                  AND NOT EXISTS (
                    SELECT 1 FROM avionics_suite_components membership
                    WHERE membership.suite_model_id = model.id
                  )
                )
              )
            ORDER BY model.id
            "#,
            listing_id
        )?
    };
    hydrate_reference_rows(rows, &capabilities)
}

fn enrichment_item_from_response(
    row: &AvionicsModelReferenceRow,
    response: &Value,
) -> StoreResult<AvionicsEnrichmentItem> {
    let introduced_year = response
        .get("introduced_year")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            AvionicsStoreError::Model(
                "Gemini avionics response missing introduced_year".to_string(),
            )
        })?;
    if !(1940..=2100).contains(&introduced_year) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response introduced_year out of range: {introduced_year}"
        )));
    }
    let compatibility_value = required_min_f64(response, "estimated_unit_value_usd", 0.0)?;
    let installed_value_contribution_usd =
        required_min_f64(response, "installed_value_contribution_usd", 0.0)?;
    let replacement_cost_usd = required_min_f64(response, "replacement_cost_usd", 0.0)?;
    validate_avionics_values(
        compatibility_value,
        installed_value_contribution_usd,
        replacement_cost_usd,
    )?;
    let valuation_scope = required_valuation_scope(response, "valuation_scope")?;
    let included_components = included_components_from_response(
        response,
        &row.manufacturer,
        &row.model,
        valuation_scope.as_str(),
    )?;
    let identity = identity_evidence_from_response(response)?;
    let confidence = required_confidence(response, "confidence")?;
    Ok(AvionicsEnrichmentItem {
        avionics_model_id: row.id,
        manufacturer: row.manufacturer.clone(),
        model: row.model.clone(),
        avionics_types: row.avionics_types.clone(),
        previous_introduced_year: row.introduced_year,
        previous_estimated_unit_value_usd: row.estimated_unit_value_usd,
        previous_replacement_cost_usd: row.replacement_cost_usd,
        previous_valuation_scope: row.valuation_scope.clone(),
        introduced_year,
        installed_value_contribution_usd,
        replacement_cost_usd,
        valuation_scope,
        included_components,
        identity,
        confidence,
    })
}

async fn resolve_enrichment_item_identities(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    persist: bool,
    item: &mut AvionicsEnrichmentItem,
    aircraft: &AvionicsIdentityAircraftContext,
    context_kind: &str,
) -> StoreResult<()> {
    let outcome = resolve_or_preview_identity(
        db,
        extractor,
        persist,
        identity_request(
            aircraft,
            context_kind,
            (&item.manufacturer, &item.model, &item.avionics_types, 1),
            &item.identity,
            Value::Null,
        ),
    )
    .await?;
    let approved = require_approved_identity(outcome, &item.manufacturer, &item.model)?;
    apply_approved_enrichment_identity(item, &approved);
    resolve_component_identities(
        db,
        extractor,
        persist,
        aircraft,
        context_kind,
        item.avionics_model_id,
        &mut item.included_components,
    )
    .await
}

async fn resolve_default_profile_identities(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    persist: bool,
    row: &AircraftModelYearProfileRow,
    profile: &mut AvionicsModelYearProfileItem,
) -> StoreResult<()> {
    let aircraft = AvionicsIdentityAircraftContext {
        manufacturer: row.manufacturer.clone(),
        model: row.model.clone(),
        variant: row.variant.clone(),
        model_year: row.model_year,
        source_url: row.source_url.clone().unwrap_or_default(),
    };
    let mut approved_items = Vec::with_capacity(profile.avionics.len());
    for mut item in std::mem::take(&mut profile.avionics) {
        let labels_are_concrete = is_usable_avionics_label(&item.manufacturer, &item.model);
        let outcome = resolve_or_preview_identity(
            db,
            extractor,
            persist,
            identity_request(
                &aircraft,
                "factory/default aircraft avionics profile",
                (
                    &item.manufacturer,
                    &item.model,
                    &item.avionics_types,
                    item.quantity,
                ),
                &item.identity,
                json!({
                    "factory_default_source_url": item.source_url,
                    "factory_default_source_title": item.source_title,
                    "factory_default_notes": item.notes,
                }),
            ),
        )
        .await?;
        let approved = match outcome {
            AvionicsIdentityOutcome::Approved(approved) => approved,
            AvionicsIdentityOutcome::Rejected { .. } if !labels_are_concrete => {
                // A generic default is omitted only after the identity classifier
                // explicitly rejects it. Local label heuristics alone never skip it.
                continue;
            }
            other => require_approved_identity(other, &item.manufacturer, &item.model)?,
        };
        apply_approved_default_identity(&mut item, &approved);
        resolve_component_identities(
            db,
            extractor,
            persist,
            &aircraft,
            "factory/default integrated avionics suite component",
            item.avionics_model_id,
            &mut item.included_components,
        )
        .await?;
        approved_items.push(item);
    }
    profile.avionics = approved_items;
    Ok(())
}

async fn resolve_component_identities(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    persist: bool,
    aircraft: &AvionicsIdentityAircraftContext,
    context_kind: &str,
    suite_model_id: i64,
    components: &mut Vec<AvionicsIncludedComponentItem>,
) -> StoreResult<()> {
    for component in components.iter_mut() {
        let outcome = resolve_or_preview_identity(
            db,
            extractor,
            persist,
            identity_request(
                aircraft,
                context_kind,
                (
                    &component.manufacturer,
                    &component.model,
                    &component.avionics_types,
                    component.quantity,
                ),
                &component.identity,
                json!({"approved_or_preview_suite_model_id": suite_model_id}),
            ),
        )
        .await?;
        let approved =
            require_approved_identity(outcome, &component.manufacturer, &component.model)?;
        component.avionics_model_id = approved.id;
        component.manufacturer = approved.manufacturer.clone();
        component.model = approved.model.clone();
        component.avionics_types = approved.avionics_types.clone();
        component.identity = approved_identity_evidence(&approved);
        if suite_model_id > 0 && component.avionics_model_id == suite_model_id {
            return Err(AvionicsStoreError::Model(format!(
                "grounded identity resolution mapped suite component {} {} back to its parent suite catalog id {suite_model_id}",
                component.manufacturer, component.model
            )));
        }
    }

    // Different raw aliases can independently resolve to one approved catalog
    // identity. Collapse those aliases before suite storage so membership is
    // deterministic and quantities are not overwritten by insertion order.
    let mut canonical_components: Vec<AvionicsIncludedComponentItem> = Vec::new();
    for component in std::mem::take(components) {
        if component.avionics_model_id > 0 {
            if let Some(existing) = canonical_components
                .iter_mut()
                .find(|existing| existing.avionics_model_id == component.avionics_model_id)
            {
                existing.quantity = existing.quantity.max(component.quantity);
                existing.avionics_types =
                    merge_capability_names(&existing.avionics_types, &component.avionics_types);
                continue;
            }
        }
        canonical_components.push(component);
    }
    *components = canonical_components;
    Ok(())
}

fn identity_request(
    aircraft: &AvionicsIdentityAircraftContext,
    context_kind: &str,
    candidate: (&str, &str, &[String], i64),
    identity: &AvionicsIdentityEvidenceItem,
    additional_context: Value,
) -> AvionicsIdentityRequest {
    let (manufacturer, model, avionics_types, quantity) = candidate;
    AvionicsIdentityRequest {
        aircraft_manufacturer: aircraft.manufacturer.clone(),
        aircraft_model: aircraft.model.clone(),
        aircraft_variant: aircraft.variant.clone(),
        model_year: aircraft.model_year,
        // This is the sale-listing URL when one exists. Authoritative identity
        // evidence remains in listing_context, so the resolver can reject any
        // attempt to reuse listing evidence as product-identity evidence.
        source_url: aircraft.source_url.clone(),
        listing_context: json!({
            "context_kind": context_kind,
            "metadata_identity_claim": identity,
            "additional_context": additional_context,
        })
        .to_string(),
        requires_listing_evidence: false,
        manufacturer: manufacturer.to_string(),
        model: model.to_string(),
        avionics_types: avionics_types.to_vec(),
        quantity: quantity.max(1),
    }
}

async fn resolve_or_preview_identity(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    persist: bool,
    request: AvionicsIdentityRequest,
) -> StoreResult<AvionicsIdentityOutcome> {
    let outcome = if persist {
        resolve_avionics_identity(db, extractor, &request).await
    } else {
        preview_avionics_identity(db, extractor, &request).await
    };
    outcome.map_err(|error| {
        AvionicsStoreError::Model(format!(
            "avionics identity resolution failed for {} {}: {error}",
            request.manufacturer, request.model
        ))
    })
}

fn require_approved_identity(
    outcome: AvionicsIdentityOutcome,
    manufacturer: &str,
    model: &str,
) -> StoreResult<ApprovedAvionicsIdentity> {
    match outcome {
        AvionicsIdentityOutcome::Approved(approved) => Ok(approved),
        AvionicsIdentityOutcome::Rejected { reason } => Err(AvionicsStoreError::Model(format!(
            "avionics identity was rejected for {manufacturer} {model}: {reason}"
        ))),
        AvionicsIdentityOutcome::Unresolved { reason } => Err(AvionicsStoreError::Model(format!(
            "avionics identity remains unresolved for {manufacturer} {model}: {reason}"
        ))),
    }
}

fn apply_approved_enrichment_identity(
    item: &mut AvionicsEnrichmentItem,
    approved: &ApprovedAvionicsIdentity,
) {
    item.avionics_model_id = approved.id;
    item.manufacturer = approved.manufacturer.clone();
    item.model = approved.model.clone();
    item.avionics_types = approved.avionics_types.clone();
    item.identity = approved_identity_evidence(approved);
}

fn apply_approved_default_identity(
    item: &mut AvionicsModelYearProfileAvionicsItem,
    approved: &ApprovedAvionicsIdentity,
) {
    item.avionics_model_id = approved.id;
    item.manufacturer = approved.manufacturer.clone();
    item.model = approved.model.clone();
    item.avionics_types = approved.avionics_types.clone();
    item.identity = approved_identity_evidence(approved);
}

fn approved_identity_evidence(approved: &ApprovedAvionicsIdentity) -> AvionicsIdentityEvidenceItem {
    AvionicsIdentityEvidenceItem {
        manufacturer_identifier_kind: approved.manufacturer_identifier_kind.clone(),
        manufacturer_identifier: approved.manufacturer_identifier.clone(),
        identity_source_url: approved.evidence_url.clone(),
        identity_source_title: approved.evidence_title.clone(),
        identity_evidence: approved.evidence.clone(),
        // Approved catalog rows require very-high authoritative identity
        // evidence. This is deliberately independent of item.confidence, which
        // continues to control numeric value writes.
        identity_confidence: "very_high".to_string(),
    }
}

async fn update_avionics_metadata(
    db: &AppDb,
    item: &AvionicsEnrichmentItem,
    value_reference_year: i64,
    overwrite_existing: bool,
) -> StoreResult<()> {
    if item.confidence != "high" {
        return Ok(());
    }
    require_approved_catalog_model(db, item.avionics_model_id).await?;
    let value_source = "gemini";
    if overwrite_existing {
        execute_query!(
            db,
            r#"
            UPDATE avionics_models
            SET
              introduced_year = ?,
              estimated_unit_value_usd = ?,
              value_basis = 'installed_contribution',
              replacement_cost_usd = ?,
              value_reference_year = ?,
              value_source = ?,
              valuation_scope = ?,
              updated_at = CURRENT_TIMESTAMP
            WHERE id = ? AND catalog_status = 'approved'
            "#,
            item.introduced_year,
            item.installed_value_contribution_usd,
            item.replacement_cost_usd,
            value_reference_year,
            value_source,
            item.valuation_scope.as_str(),
            item.avionics_model_id
        )?;
    } else {
        execute_query!(
            db,
            r#"
            UPDATE avionics_models
            SET
              introduced_year = COALESCE(introduced_year, ?),
              estimated_unit_value_usd = CASE
                WHEN value_basis = 'installed_contribution'
                  AND estimated_unit_value_usd IS NOT NULL
                THEN estimated_unit_value_usd
                ELSE ?
              END,
              value_basis = 'installed_contribution',
              replacement_cost_usd = COALESCE(replacement_cost_usd, ?),
              value_reference_year = ?,
              value_source = ?,
              valuation_scope = ?,
              updated_at = CURRENT_TIMESTAMP
            WHERE id = ? AND catalog_status = 'approved'
            "#,
            item.introduced_year,
            item.installed_value_contribution_usd,
            item.replacement_cost_usd,
            value_reference_year,
            value_source,
            item.valuation_scope.as_str(),
            item.avionics_model_id
        )?;
    }
    replace_suite_memberships(
        db,
        item.avionics_model_id,
        item.valuation_scope.as_str(),
        &item.included_components,
    )
    .await
}

async fn replace_suite_memberships(
    db: &AppDb,
    suite_model_id: i64,
    valuation_scope: &str,
    included_components: &[AvionicsIncludedComponentItem],
) -> StoreResult<()> {
    macro_rules! replace_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let status_sql = db.sql("SELECT catalog_status FROM avionics_models WHERE id = ?");
            let suite_status: Option<String> = sqlx::query_scalar(&status_sql)
                .bind(suite_model_id)
                .fetch_optional(&mut *transaction)
                .await?;
            if suite_status.as_deref() != Some("approved") {
                return Err(AvionicsStoreError::Model(format!(
                    "avionics suite catalog id {suite_model_id} is not approved"
                )));
            }
            if valuation_scope == "integrated_suite" {
                for component in included_components {
                    if component.avionics_model_id <= 0 {
                        return Err(AvionicsStoreError::Model(format!(
                            "suite component {} {} has no approved catalog id",
                            component.manufacturer, component.model
                        )));
                    }
                    if component.avionics_model_id == suite_model_id {
                        return Err(AvionicsStoreError::Model(format!(
                            "approved integrated suite {suite_model_id} cannot contain itself"
                        )));
                    }
                    let component_status: Option<String> = sqlx::query_scalar(&status_sql)
                        .bind(component.avionics_model_id)
                        .fetch_optional(&mut *transaction)
                        .await?;
                    if component_status.as_deref() != Some("approved") {
                        return Err(AvionicsStoreError::Model(format!(
                            "suite component catalog id {} is not approved",
                            component.avionics_model_id
                        )));
                    }
                }
            }

            let delete_sql =
                db.sql("DELETE FROM avionics_suite_components WHERE suite_model_id = ?");
            sqlx::query(&delete_sql)
                .bind(suite_model_id)
                .execute(&mut *transaction)
                .await?;
            if valuation_scope == "integrated_suite" {
                let insert_sql = db.sql(
                    r#"
                    INSERT INTO avionics_suite_components (
                      suite_model_id, component_model_id, quantity
                    )
                    VALUES (?, ?, ?)
                    ON CONFLICT (suite_model_id, component_model_id) DO UPDATE SET
                      quantity = excluded.quantity
                    "#,
                );
                for component in included_components {
                    sqlx::query(&insert_sql)
                        .bind(suite_model_id)
                        .bind(component.avionics_model_id)
                        .bind(component.quantity.max(1))
                        .execute(&mut *transaction)
                        .await?;
                }
            }
            transaction.commit().await?;
            Ok(())
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => replace_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => replace_in_transaction!(pool),
    }
}

async fn require_approved_catalog_model(db: &AppDb, avionics_model_id: i64) -> StoreResult<()> {
    let status = query_scalar_optional!(
        db,
        String,
        "SELECT catalog_status FROM avionics_models WHERE id = ?",
        avionics_model_id
    )?;
    match status.as_deref() {
        Some("approved") => Ok(()),
        Some(status) => Err(AvionicsStoreError::Model(format!(
            "avionics catalog id {avionics_model_id} is {status}; an approved identity is required"
        ))),
        None => Err(AvionicsStoreError::Model(format!(
            "avionics catalog id {avionics_model_id} does not exist"
        ))),
    }
}

async fn update_avionics_model_metadata(
    db: &AppDb,
    avionics_model_id: i64,
    introduced_year: i64,
    installed_value_contribution_usd: f64,
    replacement_cost_usd: f64,
    valuation_scope: &str,
    value_reference_year: i64,
    value_source: &str,
    confidence: &str,
) -> StoreResult<()> {
    if confidence != "high" {
        return Ok(());
    }
    require_approved_catalog_model(db, avionics_model_id).await?;
    execute_query!(
        db,
        r#"
        UPDATE avionics_models
        SET
          introduced_year = COALESCE(introduced_year, ?),
          estimated_unit_value_usd = CASE
            WHEN value_basis = 'installed_contribution'
              AND estimated_unit_value_usd IS NOT NULL
            THEN estimated_unit_value_usd
            ELSE ?
          END,
          value_basis = 'installed_contribution',
          replacement_cost_usd = COALESCE(replacement_cost_usd, ?),
          value_reference_year = COALESCE(value_reference_year, ?),
          value_source = COALESCE(value_source, ?),
          valuation_scope = ?,
          updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND catalog_status = 'approved'
        "#,
        introduced_year,
        installed_value_contribution_usd,
        replacement_cost_usd,
        value_reference_year,
        value_source,
        valuation_scope,
        avionics_model_id
    )?;
    Ok(())
}

fn identity_evidence_from_response(value: &Value) -> StoreResult<AvionicsIdentityEvidenceItem> {
    let manufacturer_identifier_kind =
        required_present_string(value, "manufacturer_identifier_kind")?.to_ascii_lowercase();
    if !matches!(
        manufacturer_identifier_kind.as_str(),
        "manufacturer_part_number" | "manufacturer_model_number" | "sku" | "none"
    ) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response manufacturer_identifier_kind has unsupported value {manufacturer_identifier_kind}"
        )));
    }
    let manufacturer_identifier = required_present_string(value, "manufacturer_identifier")?;
    if manufacturer_identifier_kind == "none" && !manufacturer_identifier.is_empty() {
        return Err(AvionicsStoreError::Model(
            "Gemini avionics response cannot provide manufacturer_identifier when its kind is none"
                .to_string(),
        ));
    }
    if manufacturer_identifier_kind != "none" && manufacturer_identifier.is_empty() {
        return Err(AvionicsStoreError::Model(
            "Gemini avionics response requires manufacturer_identifier for the selected identifier kind"
                .to_string(),
        ));
    }
    let identity_source_url = required_present_string(value, "identity_source_url")?;
    if !(identity_source_url.is_empty()
        || identity_source_url.starts_with("https://")
        || identity_source_url.starts_with("http://"))
    {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics identity_source_url must be http(s): {identity_source_url}"
        )));
    }
    if !identity_source_url.is_empty() && looks_like_used_listing_url(&identity_source_url) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics identity_source_url must cite authoritative product evidence, not an ordinary sale listing: {identity_source_url}"
        )));
    }
    Ok(AvionicsIdentityEvidenceItem {
        manufacturer_identifier_kind,
        manufacturer_identifier,
        identity_source_url,
        identity_source_title: required_present_string(value, "identity_source_title")?,
        identity_evidence: required_present_string(value, "identity_evidence")?,
        identity_confidence: required_identity_confidence(value, "identity_confidence")?,
    })
}

fn required_present_string(value: &Value, field: &str) -> StoreResult<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .map(ToString::to_string)
        .ok_or_else(|| {
            AvionicsStoreError::Model(format!(
                "Gemini avionics response missing required string field {field}"
            ))
        })
}

fn required_identity_confidence(value: &Value, field: &str) -> StoreResult<String> {
    let confidence = required_string(value, field)?.to_ascii_lowercase();
    if !matches!(confidence.as_str(), "very_high" | "high" | "medium" | "low") {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response {field} must be very_high, high, medium, or low"
        )));
    }
    Ok(confidence)
}

fn required_string(value: &Value, field: &str) -> StoreResult<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            AvionicsStoreError::Model(format!(
                "Gemini avionics response missing required string field {field}"
            ))
        })
}

fn required_string_array(value: &Value, field: &str) -> StoreResult<Vec<String>> {
    let values = value.get(field).and_then(Value::as_array).ok_or_else(|| {
        AvionicsStoreError::Model(format!(
            "Gemini avionics response missing required string array field {field}"
        ))
    })?;
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        let value = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AvionicsStoreError::Model(format!(
                    "Gemini avionics response {field} must contain only non-empty strings"
                ))
            })?;
        if !result
            .iter()
            .any(|known: &String| normalize_name(known) == normalize_name(value))
        {
            result.push(value.to_string());
        }
    }
    if result.is_empty() {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response {field} must contain at least one capability"
        )));
    }
    result.sort_by_key(|value| normalize_name(value));
    Ok(result)
}

fn merge_capability_names(left: &[String], right: &[String]) -> Vec<String> {
    let mut result = left.to_vec();
    for value in right {
        if !result
            .iter()
            .any(|known| normalize_name(known) == normalize_name(value))
        {
            result.push(value.clone());
        }
    }
    result.sort_by_key(|value| normalize_name(value));
    result
}

fn required_confidence(value: &Value, field: &str) -> StoreResult<String> {
    let confidence = required_string(value, field)?.to_ascii_lowercase();
    if !matches!(confidence.as_str(), "high" | "medium" | "low") {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response {field} must be high, medium, or low"
        )));
    }
    Ok(confidence)
}

fn required_price_evidence_kind(value: &Value, field: &str) -> StoreResult<String> {
    let evidence_kind = required_string(value, field)?.to_ascii_lowercase();
    if !matches!(
        evidence_kind.as_str(),
        "direct_model_year" | "direct_other_year" | "interpolated" | "inferred"
    ) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response {field} has unsupported value {evidence_kind}"
        )));
    }
    Ok(evidence_kind)
}

fn required_valuation_scope(value: &Value, field: &str) -> StoreResult<String> {
    let scope = required_string(value, field)?.to_ascii_lowercase();
    if !matches!(scope.as_str(), "unit" | "integrated_suite") {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response {field} must be unit or integrated_suite"
        )));
    }
    Ok(scope)
}

fn validate_avionics_values(
    compatibility_value: f64,
    installed_value_contribution_usd: f64,
    replacement_cost_usd: f64,
) -> StoreResult<()> {
    let compatibility_tolerance = (installed_value_contribution_usd * 0.01).max(1.0);
    if (compatibility_value - installed_value_contribution_usd).abs() > compatibility_tolerance {
        return Err(AvionicsStoreError::Model(
            "Gemini avionics response estimated_unit_value_usd must repeat installed_value_contribution_usd"
                .to_string(),
        ));
    }
    if replacement_cost_usd < installed_value_contribution_usd {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics replacement cost {replacement_cost_usd} cannot be below installed contribution {installed_value_contribution_usd}"
        )));
    }
    Ok(())
}

fn included_components_from_response(
    value: &Value,
    parent_manufacturer: &str,
    parent_model: &str,
    valuation_scope: &str,
) -> StoreResult<Vec<AvionicsIncludedComponentItem>> {
    let values = value
        .get("included_components")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AvionicsStoreError::Model(
                "Gemini avionics response missing included_components".to_string(),
            )
        })?;
    if valuation_scope == "unit" && !values.is_empty() {
        return Err(AvionicsStoreError::Model(
            "unit-scope avionics cannot declare included components".to_string(),
        ));
    }
    if valuation_scope == "integrated_suite" && values.is_empty() {
        return Err(AvionicsStoreError::Model(
            "integrated-suite avionics must declare grounded included components".to_string(),
        ));
    }

    let parent_key = (
        normalize_avionics_manufacturer_name(parent_manufacturer),
        normalize_avionics_model_name(parent_model),
    );
    let mut components = BTreeMap::<(String, String), AvionicsIncludedComponentItem>::new();
    for value in values {
        let manufacturer = required_string(value, "manufacturer")?;
        let model = required_string(value, "model")?;
        let avionics_types = required_string_array(value, "types")?;
        let identity = identity_evidence_from_response(value)?;
        if !is_usable_avionics_label(&manufacturer, &model) {
            return Err(AvionicsStoreError::Model(format!(
                "suite component must identify concrete avionics: {manufacturer} {model}"
            )));
        }
        let component_key = (
            normalize_avionics_manufacturer_name(&manufacturer),
            normalize_avionics_model_name(&model),
        );
        if component_key == parent_key {
            return Err(AvionicsStoreError::Model(
                "integrated suite cannot contain itself".to_string(),
            ));
        }
        let key = (component_key.0, component_key.1);
        let quantity = required_i64(value, "quantity")?;
        if quantity < 1 {
            return Err(AvionicsStoreError::Model(
                "suite component quantity must be at least 1".to_string(),
            ));
        }
        components
            .entry(key)
            .and_modify(|component| {
                component.quantity = component.quantity.max(quantity);
                component.avionics_types =
                    merge_capability_names(&component.avionics_types, &avionics_types);
            })
            .or_insert(AvionicsIncludedComponentItem {
                avionics_model_id: 0,
                manufacturer,
                model,
                avionics_types,
                quantity,
                identity,
            });
    }
    Ok(components.into_values().collect())
}

fn required_i64(value: &Value, field: &str) -> StoreResult<i64> {
    value.get(field).and_then(Value::as_i64).ok_or_else(|| {
        AvionicsStoreError::Model(format!(
            "Gemini avionics response missing required integer field {field}"
        ))
    })
}

fn required_year(value: &Value, field: &str) -> StoreResult<i64> {
    let year = required_i64(value, field)?;
    if !(1900..=2100).contains(&year) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response {field} out of range: {year}"
        )));
    }
    Ok(year)
}

fn required_min_f64(value: &Value, field: &str, minimum: f64) -> StoreResult<f64> {
    let number = value.get(field).and_then(Value::as_f64).ok_or_else(|| {
        AvionicsStoreError::Model(format!(
            "Gemini avionics response missing required number field {field}"
        ))
    })?;
    if number < minimum {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response {field} below minimum {minimum}: {number}"
        )));
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::{
        consensus_money, default_avionics_item_from_response, enrich_listing_avionics_metadata,
        enrich_model_year_avionics_and_price_point_for_listing, enrichment_item_from_response,
        has_material_price_discontinuity, included_components_from_response,
        model_year_profile_item_from_response, price_evidence_is_stronger,
        validate_avionics_values, AircraftModelYearProfileRow, AvionicsModelReferenceRow,
    };
    use crate::db::{AppDb, DatabaseBackend};
    use crate::extract::{AircraftPricePointContext, GeminiListingExtractor};
    use serde_json::json;

    #[tokio::test]
    async fn listing_enrichment_paths_reject_before_gemini_and_persistence() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Gate Test', 'gate test')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) VALUES (1, 'Model', 'model')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) VALUES (1, 'Variant', 'variant')",
        )
        .execute(pool)
        .await
        .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, model_year,
              asking_price_usd, airframe_hours, ingestion_state,
              ingestion_completed_at
            ) VALUES (1, 1, 2020, 200000, 1000, 'ready', CURRENT_TIMESTAMP)
            RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        let metadata_error =
            enrich_listing_avionics_metadata(&db, &extractor, true, listing_id, None, true)
                .await
                .unwrap_err();
        assert!(metadata_error.to_string().contains("missing_registration"));

        let profile_error = enrich_model_year_avionics_and_price_point_for_listing(
            &db, &extractor, true, listing_id, None, true,
        )
        .await
        .unwrap_err();
        assert!(profile_error.to_string().contains("missing_registration"));

        let price_points: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aircraft_model_variant_price_points")
                .fetch_one(pool)
                .await
                .unwrap();
        let default_avionics: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aircraft_model_variant_default_avionics")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!((price_points, default_avionics), (0, 0));
    }

    #[test]
    fn chronology_check_flags_large_adjacent_same_variant_jump() {
        let nearby = vec![AircraftPricePointContext {
            variant: "Model A".to_string(),
            model_year: 2000,
            purchase_price_new_usd: 100_000.0,
            purchase_price_reference_year: 2000,
            source_title: "Direct guide".to_string(),
            source_confidence: "high".to_string(),
        }];

        assert!(has_material_price_discontinuity(
            "Model A", 2001, 150_000.0, &nearby
        ));
        assert!(!has_material_price_discontinuity(
            "Model B", 2001, 150_000.0, &nearby
        ));
    }

    #[test]
    fn conflicting_duplicate_values_are_not_silently_selected() {
        assert_eq!(
            consensus_money([4_000.0, 4_500.0].into_iter()),
            Some(4_500.0)
        );
        assert_eq!(consensus_money([4_000.0, 15_000.0].into_iter()), None);
    }

    #[test]
    fn installed_contribution_must_be_distinct_and_bounded_by_replacement_cost() {
        assert!(validate_avionics_values(12_000.0, 12_000.0, 25_000.0).is_ok());
        assert!(validate_avionics_values(25_000.0, 12_000.0, 25_000.0).is_err());
        assert!(validate_avionics_values(12_000.0, 12_000.0, 10_000.0).is_err());
    }

    #[test]
    fn metadata_parser_keeps_identity_confidence_separate_from_value_confidence() {
        let row = AvionicsModelReferenceRow {
            id: 17,
            manufacturer: "Garmin".to_string(),
            model: "GTX 345R".to_string(),
            avionics_types: vec!["Transponder".to_string()],
            introduced_year: None,
            estimated_unit_value_usd: None,
            replacement_cost_usd: None,
            valuation_scope: "unit".to_string(),
        };
        let mut response = json!({
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-03520-00",
            "identity_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "identity_source_title": "GTX 345R installation manual",
            "identity_evidence": "The manual identifies GTX 345R part 011-03520-00.",
            "identity_confidence": "medium",
            "introduced_year": 2016,
            "estimated_unit_value_usd": 5000.0,
            "installed_value_contribution_usd": 5000.0,
            "replacement_cost_usd": 9000.0,
            "valuation_scope": "unit",
            "included_components": [],
            "confidence": "high"
        });

        let item = enrichment_item_from_response(&row, &response).unwrap();
        assert_eq!(item.identity.identity_confidence, "medium");
        assert_eq!(item.confidence, "high");

        response
            .as_object_mut()
            .unwrap()
            .remove("identity_evidence");
        assert!(enrichment_item_from_response(&row, &response).is_err());
    }

    #[test]
    fn integrated_suite_requires_concrete_members_but_unit_forbids_them() {
        let response = json!({
            "included_components": [{
                "manufacturer": "Component Maker",
                "model": "ABC 123",
                "types": ["Flight Display"],
                "manufacturer_identifier_kind": "manufacturer_part_number",
                "manufacturer_identifier": "CMP-ABC-123",
                "identity_source_url": "https://component.example/manuals/abc-123",
                "identity_source_title": "ABC 123 installation manual",
                "identity_evidence": "The manual identifies model ABC 123 and part CMP-ABC-123.",
                "identity_confidence": "very_high",
                "quantity": 2
            }]
        });
        let components = included_components_from_response(
            &response,
            "Suite Maker",
            "Suite 1000",
            "integrated_suite",
        )
        .unwrap();
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].quantity, 2);
        assert!(
            included_components_from_response(&response, "Suite Maker", "Suite 1000", "unit")
                .is_err()
        );
    }

    #[test]
    fn default_avionics_rejects_sale_listing_evidence_and_invalid_quantity() {
        let mut response = json!({
            "manufacturer": "Garmin",
            "model": "G500 TXi",
            "types": ["Flight Display"],
            "manufacturer_identifier_kind": "manufacturer_part_number",
            "manufacturer_identifier": "011-00000-00",
            "identity_source_url": "https://static.garmin.com/manuals/g500-txi.pdf",
            "identity_source_title": "G500 TXi installation manual",
            "identity_evidence": "The manual identifies G500 TXi part 011-00000-00.",
            "identity_confidence": "very_high",
            "quantity": 1,
            "introduced_year": 2017,
            "estimated_unit_value_usd": 12000.0,
            "installed_value_contribution_usd": 12000.0,
            "replacement_cost_usd": 25000.0,
            "valuation_scope": "unit",
            "included_components": [],
            "confidence": "high",
            "source_url": "https://market.example/aircraft-for-sale/123",
            "source_title": "Aircraft listing",
            "notes": "Listing equipment panel"
        });
        assert!(default_avionics_item_from_response(&response, 2020).is_err());

        response["source_url"] = json!("https://manufacturer.example/g500-txi");
        response["quantity"] = json!(0);
        assert!(default_avionics_item_from_response(&response, 2020).is_err());
    }

    #[test]
    fn price_evidence_updates_are_quality_monotonic() {
        assert!(!price_evidence_is_stronger(
            "low",
            "inferred",
            false,
            "medium",
            "direct_other_year",
            false,
        ));
        assert!(!price_evidence_is_stronger(
            "high",
            "direct_model_year",
            true,
            "high",
            "direct_model_year",
            true,
        ));
        assert!(price_evidence_is_stronger(
            "high",
            "direct_model_year",
            true,
            "medium",
            "direct_other_year",
            false,
        ));
    }

    #[test]
    fn only_high_confidence_direct_exact_year_price_is_eligible() {
        let row = AircraftModelYearProfileRow {
            aircraft_model_variant_id: 7,
            manufacturer: "Maker".to_string(),
            model: "Family".to_string(),
            variant: "Variant".to_string(),
            model_year: 2001,
            source_url: Some("https://market.example/listing/7".to_string()),
            listing_count: 1,
        };
        let mut response = json!({
            "purchase_price_new_usd": 150000.0,
            "purchase_price_reference_year": 2001,
            "price_source_url": "https://reference.example/guides/2001-variant",
            "price_source_title": "2001 Variant price guide",
            "price_source_notes": "Direct 2001 model-year base price.",
            "price_source_confidence": "high",
            "price_evidence_kind": "direct_model_year",
            "price_discontinuity_explanation": null,
            "avionics": []
        });
        let direct = model_year_profile_item_from_response(&row, &[], &response).unwrap();
        assert!(direct.is_price_valuation_eligible);

        response["price_source_confidence"] = json!("low");
        response["price_evidence_kind"] = json!("inferred");
        let inferred = model_year_profile_item_from_response(&row, &[], &response).unwrap();
        assert!(!inferred.is_price_valuation_eligible);
        assert_eq!(inferred.price_eligibility_notes.len(), 2);
    }
}
