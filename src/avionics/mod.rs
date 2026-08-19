pub mod catalog;
pub mod consolidation;
pub mod inspection;
pub mod manufacturer;
pub(crate) mod model;
pub(crate) mod reuse;
pub(crate) mod source;
pub mod verification;

use std::collections::BTreeMap;
use std::future::Future;

use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;

use crate::aircraft::faa::{require_listing_admission, AircraftAdmissionError};
use crate::avionics::catalog::{
    preview_avionics_identity, resolve_avionics_identity, ApprovedAvionicsIdentity,
    AvionicsIdentityOutcome, AvionicsIdentityRequest,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{AvionicsMetadataContext, GeminiListingExtractor};
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

#[derive(FromRow)]
struct ListingAircraftIdentityContextRow {
    manufacturer: String,
    model: String,
    variant: String,
    model_year: i64,
    source_url: String,
}

async fn listing_aircraft_identity_context(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<AvionicsIdentityAircraftContext> {
    let rows = query_as_all!(
        db,
        ListingAircraftIdentityContextRow,
        r#"
        SELECT make.name AS manufacturer,
               family.name AS model,
               designation.official_designation
                 || CASE WHEN generation.id IS NULL
                      THEN '' ELSE ' / ' || generation.name END
                 || CASE WHEN package.id IS NULL
                      THEN '' ELSE ' / ' || package.name END AS variant,
               listing.model_year,
               COALESCE(listing.source_url, '') AS source_url
        FROM aircraft_sale_listings listing
        JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = listing.selected_aircraft_identity_assignment_id
         AND assignment.aircraft_sale_listing_id = listing.id
        JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
        JOIN aircraft_model_families family
          ON family.id = assignment.aircraft_model_family_id
        JOIN aircraft_designations designation
          ON designation.id = assignment.aircraft_designation_id
        LEFT JOIN aircraft_generations generation
          ON generation.id = assignment.aircraft_generation_id
        LEFT JOIN aircraft_factory_packages package
          ON package.id = assignment.aircraft_factory_package_id
        WHERE listing.id = ?
        "#,
        listing_id,
    )?;
    let [row]: [ListingAircraftIdentityContextRow; 1] = rows.try_into().map_err(|rows: Vec<_>| {
        AvionicsStoreError::Model(format!(
            "listing {listing_id} must have exactly one selected canonical aircraft identity, found {}",
            rows.len()
        ))
    })?;
    Ok(AvionicsIdentityAircraftContext {
        manufacturer: row.manufacturer,
        model: row.model,
        variant: row.variant,
        model_year: row.model_year,
        source_url: row.source_url,
    })
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
    pub introduced_year_evidence: AvionicsFactEvidenceItem,
    pub installed_value_contribution_usd: f64,
    pub installed_value_evidence: AvionicsFactEvidenceItem,
    pub replacement_cost_usd: f64,
    pub replacement_cost_evidence: AvionicsFactEvidenceItem,
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
pub struct AvionicsFactEvidenceItem {
    pub source_url: String,
    pub source_title: String,
    pub evidence: String,
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
        let mut item =
            estimate_avionics_enrichment_item(extractor, &row, value_reference_year).await?;
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
    let aircraft_context = listing_aircraft_identity_context(db, listing_id).await?;
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let source_model_id = row.id;
        let mut item =
            estimate_avionics_enrichment_item(extractor, &row, value_reference_year).await?;
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

async fn estimate_avionics_enrichment_item(
    extractor: &GeminiListingExtractor,
    row: &AvionicsModelReferenceRow,
    value_reference_year: i64,
) -> StoreResult<AvionicsEnrichmentItem> {
    let context = AvionicsMetadataContext {
        manufacturer: &row.manufacturer,
        model: &row.model,
        avionics_types: &row.avionics_types,
        value_reference_year,
    };
    let response = extractor.estimate_avionics_metadata(&context).await?;
    let evidence = response.verified_evidence.map(|verified| verified.dossier);
    parse_with_one_evidence_correction(
        response.value,
        evidence,
        |value| enrichment_item_from_response(row, value),
        |previous_response, evidence, validation_error| async move {
            let corrected = extractor
                .correct_avionics_metadata_reusing(
                    &context,
                    &previous_response,
                    &validation_error.to_string(),
                    &evidence,
                )
                .await?;
            Ok::<Value, AvionicsStoreError>(corrected.value)
        },
    )
    .await
}

async fn parse_with_one_evidence_correction<T, E, Parse, Correct, Correction>(
    initial_value: Value,
    evidence: Option<E>,
    parse: Parse,
    correct: Correct,
) -> StoreResult<T>
where
    Parse: Fn(&Value) -> StoreResult<T>,
    Correct: FnOnce(Value, E, AvionicsStoreError) -> Correction,
    Correction: Future<Output = StoreResult<Value>>,
{
    match parse(&initial_value) {
        Ok(value) => Ok(value),
        Err(validation_error) => {
            let Some(evidence) = evidence else {
                return Err(validation_error);
            };
            let corrected_value = correct(initial_value, evidence, validation_error).await?;
            parse(&corrected_value)
        }
    }
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
            authoritative_direct_source_urls: Vec::new(),
            authoritative_identity_anchors: Vec::new(),
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
                        // The listing-review payload is an evidence record but
                        // its product references are JSON, not foreign keys.
                        // Keep the legacy candidate until the explicit catalog
                        // cleanup workflow can prove that no pending review or
                        // relational role still names it.
                        "approved_mapped_cleanup_pending"
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
                    "rejected_cleanup_pending"
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
    let introduced_year_evidence = fact_evidence_from_response(response, "introduced_year")?;
    let installed_value_evidence = fact_evidence_from_response(response, "installed_value")?;
    let replacement_cost_evidence = fact_evidence_from_response(response, "replacement_cost")?;
    for (field, value, evidence) in [
        (
            "introduced_year",
            introduced_year as f64,
            &introduced_year_evidence.evidence,
        ),
        (
            "installed_value_contribution_usd",
            installed_value_contribution_usd,
            &installed_value_evidence.evidence,
        ),
        (
            "replacement_cost_usd",
            replacement_cost_usd,
            &replacement_cost_evidence.evidence,
        ),
    ] {
        if !evidence_mentions_number(evidence, value) {
            return Err(AvionicsStoreError::Model(format!(
                "Gemini avionics {field} evidence does not state the returned value {value}"
            )));
        }
    }
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
        introduced_year_evidence,
        installed_value_contribution_usd,
        installed_value_evidence,
        replacement_cost_usd,
        replacement_cost_evidence,
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
        authoritative_direct_source_urls: Vec::new(),
        authoritative_identity_anchors: Vec::new(),
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
    let value_source = item.installed_value_evidence.source_url.as_str();
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

fn fact_evidence_from_response(
    value: &Value,
    field_prefix: &str,
) -> StoreResult<AvionicsFactEvidenceItem> {
    let source_url = required_present_string(value, &format!("{field_prefix}_source_url"))?;
    if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics {field_prefix}_source_url must be http(s): {source_url}"
        )));
    }
    let source_title = required_present_string(value, &format!("{field_prefix}_source_title"))?;
    let evidence = required_present_string(value, &format!("{field_prefix}_evidence"))?;
    if source_title.is_empty() || evidence.is_empty() {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics {field_prefix} evidence requires a source title and exact cited span"
        )));
    }
    Ok(AvionicsFactEvidenceItem {
        source_url,
        source_title,
        evidence,
    })
}

fn evidence_mentions_number(evidence: &str, expected: f64) -> bool {
    let expected = if expected.fract().abs() < f64::EPSILON {
        format!("{expected:.0}")
    } else {
        expected.to_string()
    };
    evidence
        .split(|character: char| !(character.is_ascii_digit() || matches!(character, ',' | '.')))
        .filter(|token| !token.is_empty())
        .map(|token| token.replace(',', ""))
        .map(|token| {
            let token = token.trim_end_matches('.');
            token
                .strip_suffix(".00")
                .or_else(|| token.strip_suffix(".0"))
                .unwrap_or(token)
                .to_string()
        })
        .any(|token| token == expected)
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
        enrich_listing_avionics_metadata, enrichment_item_from_response,
        included_components_from_response, parse_with_one_evidence_correction,
        validate_avionics_values, AvionicsModelReferenceRow, AvionicsStoreError,
    };
    use crate::db::{AppDb, DatabaseBackend};
    use crate::extract::GeminiListingExtractor;
    use serde_json::json;
    use std::cell::Cell;

    #[tokio::test]
    async fn listing_enrichment_rejects_before_gemini_and_persistence() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, model_year,
              asking_price_usd, airframe_hours, ingestion_state,
              ingestion_error
            ) VALUES (
              (
                SELECT aircraft_model_variant_id
                FROM aircraft_sale_listing_pending_compatibility_placeholder
                WHERE singleton_id = 1
              ),
              1, 2020, 200000, 1000, 'quarantined',
              'test fixture retained for mandatory FAA admission rejection'
            )
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
    }

    #[test]
    fn installed_contribution_must_be_distinct_and_bounded_by_replacement_cost() {
        assert!(validate_avionics_values(12_000.0, 12_000.0, 25_000.0).is_ok());
        assert!(validate_avionics_values(25_000.0, 12_000.0, 25_000.0).is_err());
        assert!(validate_avionics_values(12_000.0, 12_000.0, 10_000.0).is_err());
    }

    #[tokio::test]
    async fn metadata_semantic_correction_accepts_one_corrected_response() {
        let correction_calls = Cell::new(0);
        let parsed = parse_with_one_evidence_correction(
            json!({"supported_value": 5000}),
            Some(()),
            |value| {
                (value["supported_value"] == 4500)
                    .then_some(4500)
                    .ok_or_else(|| {
                        AvionicsStoreError::Model(
                            "numeric evidence does not state the returned value".to_string(),
                        )
                    })
            },
            |previous, (), validation_error| {
                correction_calls.set(correction_calls.get() + 1);
                assert_eq!(previous["supported_value"], 5000);
                assert!(validation_error
                    .to_string()
                    .contains("numeric evidence does not state"));
                std::future::ready(Ok(json!({"supported_value": 4500})))
            },
        )
        .await
        .unwrap();

        assert_eq!(parsed, 4500);
        assert_eq!(correction_calls.get(), 1);
    }

    #[tokio::test]
    async fn metadata_semantic_correction_never_opens_a_second_retry() {
        let correction_calls = Cell::new(0);
        let error = parse_with_one_evidence_correction(
            json!({"supported_value": 5000}),
            Some(()),
            |value| {
                (value["supported_value"] == 4500)
                    .then_some(4500)
                    .ok_or_else(|| {
                        AvionicsStoreError::Model(format!(
                            "unsupported value {}",
                            value["supported_value"]
                        ))
                    })
            },
            |_, (), _| {
                correction_calls.set(correction_calls.get() + 1);
                std::future::ready(Ok(json!({"supported_value": 4000})))
            },
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("unsupported value 4000"));
        assert_eq!(correction_calls.get(), 1);
    }

    #[tokio::test]
    async fn valid_metadata_spends_no_correction_budget() {
        let correction_calls = Cell::new(0);
        let parsed = parse_with_one_evidence_correction(
            json!({"supported_value": 4500}),
            Some(()),
            |value| {
                (value["supported_value"] == 4500)
                    .then_some(4500)
                    .ok_or_else(|| AvionicsStoreError::Model("invalid metadata".to_string()))
            },
            |_, (), _| {
                correction_calls.set(correction_calls.get() + 1);
                std::future::ready(Ok(json!({"supported_value": 4500})))
            },
        )
        .await
        .unwrap();

        assert_eq!(parsed, 4500);
        assert_eq!(correction_calls.get(), 0);
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
            "introduced_year_source_url": "https://static.garmin.com/manuals/gtx345r.pdf",
            "introduced_year_source_title": "GTX 345R installation manual",
            "introduced_year_evidence": "The manual was published in 2016.",
            "estimated_unit_value_usd": 5000.0,
            "installed_value_contribution_usd": 5000.0,
            "installed_value_source_url": "https://avionics.example/gtx345r-market",
            "installed_value_source_title": "GTX 345R market reference",
            "installed_value_evidence": "Working GTX 345R units sell for $5,000.",
            "replacement_cost_usd": 9000.0,
            "replacement_cost_source_url": "https://avionics.example/gtx345r-installed",
            "replacement_cost_source_title": "GTX 345R installed pricing",
            "replacement_cost_evidence": "Typical equipment and installation cost is $9,000.",
            "valuation_scope": "unit",
            "included_components": [],
            "confidence": "high"
        });

        let item = enrichment_item_from_response(&row, &response).unwrap();
        assert_eq!(item.identity.identity_confidence, "medium");
        assert_eq!(item.confidence, "high");
        assert_eq!(
            item.installed_value_evidence.source_url,
            "https://avionics.example/gtx345r-market"
        );

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
}
