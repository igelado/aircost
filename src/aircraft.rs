use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::depreciation::{
    avionics_replacement_basis, default_avionics_profile, estimate_aircraft_value_in_year,
    get_aircraft_profile, nominal_dollar_factor, AircraftProfile, AvionicsComponent, DollarBasis,
    PriceEstimate, TimedComponent, DEFAULT_ANNUAL_AIRFRAME_HOURS,
};
use crate::extract::{
    AircraftSpecListingContext, AircraftSpecMetadataContext, GeminiListingExtractor,
};
use crate::html_clean::clean_listing_html_with_limit;
use crate::normalize::normalize_name;

const DEFAULT_VALUE_REFERENCE_YEAR: i64 = 2026;
// Rounded long-run CPI-U average over roughly the last 20 completed years.
const DEFAULT_AVERAGE_INFLATION_RATE: f64 = 0.025;
const SPEC_EVIDENCE_LIMIT: i64 = 3;
const SPEC_EVIDENCE_TEXT_LIMIT: usize = 2_400;

macro_rules! query_as_optional {
    ($db:expr, $row:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_optional(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_optional(pool).await
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
pub enum AircraftStoreError {
    NotFound(String),
    Database(String),
    Model(String),
}

impl fmt::Display for AircraftStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AircraftStoreError::NotFound(message)
            | AircraftStoreError::Database(message)
            | AircraftStoreError::Model(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for AircraftStoreError {}

impl From<sqlx::Error> for AircraftStoreError {
    fn from(error: sqlx::Error) -> Self {
        AircraftStoreError::Database(error.to_string())
    }
}

impl From<anyhow::Error> for AircraftStoreError {
    fn from(error: anyhow::Error) -> Self {
        AircraftStoreError::Model(error.to_string())
    }
}

type StoreResult<T> = Result<T, AircraftStoreError>;

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

#[derive(Debug, FromRow)]
struct AircraftVariantOptionRow {
    manufacturer_id: i64,
    manufacturer: String,
    model_id: i64,
    model: String,
    variant_id: i64,
    variant: String,
    listing_count: i64,
}

#[derive(Debug, FromRow)]
struct AircraftSpecVersionRow {
    id: i64,
    aircraft_model_id: i64,
    aircraft_model_variant_id: i64,
    effective_from: String,
    effective_to: Option<String>,
    depreciation_profile_id: Option<i64>,
    depreciation_profile: Option<String>,
    depreciation_profile_age_decay_rate: Option<f64>,
    depreciation_profile_long_run_residual_fraction: Option<f64>,
    depreciation_profile_new_to_used_discount_fraction: Option<f64>,
    depreciation_profile_airframe_doubling_discount: Option<f64>,
    depreciation_profile_max_airframe_premium: Option<f64>,
    depreciation_profile_max_airframe_discount: Option<f64>,
    depreciation_profile_replacement_floor_fraction: Option<f64>,
    depreciation_profile_high_time_threshold_hours: Option<f64>,
    depreciation_profile_high_time_discount_at_double_threshold: Option<f64>,
    depreciation_fit_scope: Option<String>,
    depreciation_fit_scope_key: Option<String>,
    depreciation_fit_sample_count: Option<i64>,
    depreciation_fit_rmse_usd: Option<f64>,
    depreciation_fit_mae_fraction: Option<f64>,
    average_inflation_rate: f64,
    fuel_burn_gph: Option<f64>,
    oil_quarts_per_hour: Option<f64>,
    oil_price_per_quart_usd: Option<f64>,
    engine_model_id: Option<i64>,
    engine_manufacturer: Option<String>,
    engine_model: Option<String>,
    engine_count: i64,
    engine_tbo_hours: Option<f64>,
    engine_overhaul_cost_usd: Option<f64>,
    engine_overhaul_cost_value_reference_year: Option<i64>,
    engine_value_baseline_life_fraction: f64,
    propeller_model_id: Option<i64>,
    propeller_manufacturer: Option<String>,
    propeller_model: Option<String>,
    propeller_count: i64,
    propeller_tbo_hours: Option<f64>,
    propeller_overhaul_cost_usd: Option<f64>,
    propeller_overhaul_cost_value_reference_year: Option<i64>,
    propeller_value_baseline_life_fraction: f64,
    annual_inspection_usd: Option<f64>,
    other_maintenance_per_hour: Option<f64>,
    source_url: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, FromRow)]
struct AircraftListingPointRow {
    id: i64,
    aircraft_model_variant_id: i64,
    is_verified: bool,
    source_url: Option<String>,
    model_year: i64,
    asking_price_usd: f64,
    currency: String,
    added_at: String,
    status: String,
    registration_number: Option<String>,
    serial_number: Option<String>,
    airframe_hours: f64,
    engine_hours: f64,
    propeller_hours: f64,
}

#[derive(Debug, FromRow)]
struct AvionicsEstimateRow {
    manufacturer: String,
    model: String,
    avionics_type: String,
    quantity: i64,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
    value_reference_year: Option<i64>,
}

#[derive(Debug, FromRow)]
struct AircraftModelYearPricePointRow {
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
}

#[derive(Debug, FromRow)]
struct ReplacementBasisRow {
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
    average_inflation_rate: f64,
}

#[derive(Debug, FromRow)]
struct PluginSpecEvidenceRow {
    model_year: i64,
    asking_price_usd: f64,
    airframe_hours: f64,
    engine_hours: f64,
    propeller_hours: f64,
    source_url: String,
    rendered_html: String,
}

#[derive(Debug, FromRow)]
struct ListingSpecSeedRow {
    manufacturer_id: i64,
    manufacturer: String,
    model_id: i64,
    model: String,
    variant_id: i64,
    variant: String,
    model_year: i64,
    asking_price_usd: f64,
    airframe_hours: f64,
    engine_hours: f64,
    propeller_hours: f64,
    source_url: Option<String>,
}

#[derive(Debug, FromRow)]
struct DepreciationProfileRow {
    name: String,
    age_decay_rate: f64,
    long_run_residual_fraction: f64,
    new_to_used_discount_fraction: f64,
    new_to_used_discount_years: f64,
    airframe_doubling_discount: f64,
    max_airframe_premium: f64,
    max_airframe_discount: f64,
    replacement_floor_fraction: f64,
    minimum_value_fraction: f64,
    high_time_threshold_hours: Option<f64>,
    high_time_discount_at_double_threshold: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftVariantOption {
    pub manufacturer_id: i64,
    pub manufacturer: String,
    pub model_id: i64,
    pub model: String,
    pub variant_id: i64,
    pub variant: String,
    pub listing_count: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftVariantDetail {
    pub option: AircraftVariantOption,
    pub spec: Option<AircraftSpecDetail>,
    pub listings: Vec<AircraftListingValuePoint>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftSpecDetail {
    pub id: i64,
    pub aircraft_model_id: i64,
    pub aircraft_model_variant_id: i64,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub depreciation_profile_id: Option<i64>,
    pub depreciation_profile: String,
    pub depreciation_profile_detail: Option<AircraftDepreciationProfileDetail>,
    pub average_inflation_rate: f64,
    pub fuel_burn_gph: Option<f64>,
    pub oil_quarts_per_hour: Option<f64>,
    pub oil_price_per_quart_usd: Option<f64>,
    pub engine_model_id: Option<i64>,
    pub engine_manufacturer: Option<String>,
    pub engine_model: Option<String>,
    pub engine_count: i64,
    pub engine_tbo_hours: Option<f64>,
    pub engine_overhaul_cost_usd: Option<f64>,
    pub engine_value_baseline_life_fraction: f64,
    pub propeller_model_id: Option<i64>,
    pub propeller_manufacturer: Option<String>,
    pub propeller_model: Option<String>,
    pub propeller_count: i64,
    pub propeller_tbo_hours: Option<f64>,
    pub propeller_overhaul_cost_usd: Option<f64>,
    pub propeller_value_baseline_life_fraction: f64,
    pub annual_inspection_usd: Option<f64>,
    pub other_maintenance_per_hour: Option<f64>,
    pub source_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftDepreciationProfileDetail {
    pub name: String,
    pub age_decay_rate: f64,
    pub long_run_residual_fraction: f64,
    pub new_to_used_discount_fraction: f64,
    pub airframe_doubling_discount: f64,
    pub max_airframe_premium: f64,
    pub max_airframe_discount: f64,
    pub replacement_floor_fraction: f64,
    pub high_time_threshold_hours: Option<f64>,
    pub high_time_discount_at_double_threshold: f64,
    pub fit_scope: Option<String>,
    pub fit_scope_key: Option<String>,
    pub sample_count: Option<i64>,
    pub rmse_usd: Option<f64>,
    pub mae_fraction: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftListingValuePoint {
    pub listing_id: i64,
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
    pub estimated_value_usd: Option<f64>,
    pub estimate_error: Option<String>,
    pub breakdown: Option<AircraftValueBreakdown>,
    pub value_curve: Vec<AircraftValueCurvePoint>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftValueBreakdown {
    pub effective_new_price_usd: f64,
    pub value_reference_year: i64,
    pub valuation_year: i64,
    pub average_inflation_rate: f64,
    pub dollar_basis_factor: f64,
    pub age_residual_fraction: f64,
    pub age_baseline_value_usd: f64,
    pub expected_airframe_hours: f64,
    pub airframe_factor: f64,
    pub high_time_factor: f64,
    pub airframe_value_usd: f64,
    pub replacement_floor_basis_usd: f64,
    pub replacement_floor_value_usd: f64,
    pub engine_adjustment_usd: f64,
    pub propeller_adjustment_usd: f64,
    pub avionics_value_usd: f64,
    pub avionics_replacement_basis_usd: f64,
    pub minimum_value_usd: f64,
    pub raw_estimated_value_usd: f64,
    pub valuation_basis_usd: f64,
    pub depreciation_usd: f64,
    pub depreciation_fraction: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftValueCurvePoint {
    pub valuation_year: i64,
    pub age_years: f64,
    pub airframe_hours: f64,
    pub engine_hours: f64,
    pub propeller_hours: f64,
    pub estimated_value_usd: Option<f64>,
    pub estimate_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftSpecEnrichmentReport {
    pub applied: bool,
    pub value_reference_year: i64,
    pub variants: Vec<AircraftSpecEnrichmentItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftSpecEnrichmentItem {
    pub manufacturer_id: i64,
    pub manufacturer: String,
    pub model_id: i64,
    pub model: String,
    pub variant_id: i64,
    pub variant: String,
    pub listing_count: i64,
    pub depreciation_profile: String,
    pub average_inflation_rate: f64,
    pub fuel_burn_gph: f64,
    pub oil_quarts_per_hour: f64,
    pub oil_price_per_quart_usd: f64,
    pub engine_manufacturer: String,
    pub engine_model: String,
    pub engine_count: i64,
    pub engine_tbo_hours: f64,
    pub engine_overhaul_cost_usd: f64,
    pub engine_value_baseline_life_fraction: f64,
    pub propeller_manufacturer: String,
    pub propeller_model: String,
    pub propeller_count: i64,
    pub propeller_tbo_hours: f64,
    pub propeller_overhaul_cost_usd: f64,
    pub propeller_value_baseline_life_fraction: f64,
    pub powerplant_source_url: String,
    pub powerplant_source_title: String,
    pub powerplant_source_confidence: String,
    pub annual_inspection_usd: f64,
    pub other_maintenance_per_hour: f64,
    pub confidence: String,
    pub evidence_count: usize,
    pub applied_spec_id: Option<i64>,
}

pub async fn aircraft_options(db: &AppDb, user_id: i64) -> StoreResult<Vec<AircraftVariantOption>> {
    let rows = query_as_all!(
        db,
        AircraftVariantOptionRow,
        r#"
        SELECT
          mfr.id AS manufacturer_id,
          mfr.name AS manufacturer,
          model.id AS model_id,
          model.name AS model,
          variant.id AS variant_id,
          variant.name AS variant,
          COUNT(l.id) AS listing_count
        FROM aircraft_model_variants variant
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        JOIN aircraft_sale_listings l
          ON l.aircraft_model_variant_id = variant.id
        WHERE l.is_verified = TRUE OR l.created_by_user_id = ?
        GROUP BY
          mfr.id,
          mfr.name,
          model.id,
          model.name,
          variant.id,
          variant.name
        ORDER BY mfr.name, model.name, variant.name
        "#,
        user_id
    )?;
    Ok(rows.into_iter().map(option_from_row).collect())
}

pub async fn aircraft_variant_detail(
    db: &AppDb,
    user_id: i64,
    variant_id: i64,
    curve_annual_airframe_hours: Option<f64>,
) -> StoreResult<AircraftVariantDetail> {
    let option = aircraft_option_for_variant(db, user_id, variant_id).await?;
    let spec_row = aircraft_spec_for_variant(db, option.model_id, variant_id).await?;
    let spec = spec_row.as_ref().map(spec_detail_from_row);
    let spec_ref = spec_row.as_ref();
    let rows = listing_points_for_variant(db, user_id, variant_id).await?;
    let mut listings = Vec::with_capacity(rows.len());
    for row in rows {
        listings.push(listing_value_point(db, &row, spec_ref, curve_annual_airframe_hours).await?);
    }
    let message = match spec_ref {
        Some(_) => None,
        None => Some("Aircraft spec metadata has not been enriched for this variant.".to_string()),
    };
    Ok(AircraftVariantDetail {
        option,
        spec,
        listings,
        message,
    })
}

pub async fn aircraft_listing_value(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
) -> StoreResult<AircraftListingValuePoint> {
    let row = listing_point_for_listing(db, user_id, listing_id)
        .await?
        .ok_or_else(|| AircraftStoreError::NotFound("listing not found".to_string()))?;
    let option = aircraft_option_for_variant(db, user_id, row.aircraft_model_variant_id).await?;
    let spec_row = aircraft_spec_for_variant(db, option.model_id, option.variant_id).await?;
    listing_value_point(db, &row, spec_row.as_ref(), None).await
}

pub async fn enrich_aircraft_specs_from_plugin_submissions(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    limit: i64,
    value_reference_year: Option<i64>,
    refresh_existing: bool,
) -> StoreResult<AircraftSpecEnrichmentReport> {
    if limit < 1 {
        return Err(AircraftStoreError::Model(
            "limit must be at least 1".to_string(),
        ));
    }
    let value_reference_year = value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR);
    let variants = if refresh_existing {
        aircraft_variants_with_plugin_evidence(db, limit).await?
    } else {
        aircraft_variants_missing_specs(db, limit).await?
    };
    let mut items = Vec::with_capacity(variants.len());

    for variant in variants {
        let evidence = plugin_spec_evidence_for_variant(db, variant.variant_id).await?;
        if evidence.is_empty() {
            continue;
        }
        let listing_texts = evidence
            .iter()
            .map(|row| clean_listing_html_with_limit(&row.rendered_html, SPEC_EVIDENCE_TEXT_LIMIT))
            .collect::<Vec<_>>();
        let listing_contexts = evidence
            .iter()
            .zip(listing_texts.iter())
            .map(|(row, listing_text)| AircraftSpecListingContext {
                model_year: row.model_year,
                asking_price_usd: row.asking_price_usd,
                airframe_hours: row.airframe_hours,
                engine_hours: row.engine_hours,
                propeller_hours: row.propeller_hours,
                source_url: &row.source_url,
                listing_text,
            })
            .collect::<Vec<_>>();
        let context = AircraftSpecMetadataContext {
            manufacturer: &variant.manufacturer,
            model: &variant.model,
            variant_context: &variant.variant,
            value_reference_year,
            listing_contexts: &listing_contexts,
        };
        let response = extractor.estimate_aircraft_spec_metadata(&context).await?;
        let mut item =
            spec_enrichment_item_from_response(&variant, &response, listing_contexts.len())?;
        if apply {
            let source_url = evidence.first().map(|row| row.source_url.as_str());
            item.applied_spec_id = if refresh_existing {
                Some(upsert_aircraft_spec(db, &item, value_reference_year, source_url).await?)
            } else {
                Some(insert_aircraft_spec(db, &item, value_reference_year, source_url).await?)
            };
        }
        items.push(item);
    }

    Ok(AircraftSpecEnrichmentReport {
        applied: apply,
        value_reference_year,
        variants: items,
    })
}

pub async fn enrich_aircraft_spec_for_listing_if_missing(
    db: &AppDb,
    extractor: Option<&GeminiListingExtractor>,
    listing_id: i64,
    listing_text: Option<&str>,
) -> StoreResult<()> {
    let Some(extractor) = extractor else {
        return Ok(());
    };
    let row = listing_spec_seed(db, listing_id).await?;
    if aircraft_spec_exists_for_variant(db, row.model_id, row.variant_id).await? {
        return Ok(());
    }
    let synthetic_text;
    let listing_text = match listing_text
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => value,
        None => {
            synthetic_text = format!(
                "{} {} {} listing. model_year: {}. asking_price_usd: {}. airframe_hours: {}. engine_hours: {}. propeller_hours: {}.",
                row.manufacturer,
                row.model,
                row.variant,
                row.model_year,
                row.asking_price_usd,
                row.airframe_hours,
                row.engine_hours,
                row.propeller_hours,
            );
            synthetic_text.as_str()
        }
    };
    let source_url = row.source_url.as_deref().unwrap_or("");
    let listing_context = AircraftSpecListingContext {
        model_year: row.model_year,
        asking_price_usd: row.asking_price_usd,
        airframe_hours: row.airframe_hours,
        engine_hours: row.engine_hours,
        propeller_hours: row.propeller_hours,
        source_url,
        listing_text,
    };
    let response = extractor
        .estimate_aircraft_spec_metadata(&AircraftSpecMetadataContext {
            manufacturer: &row.manufacturer,
            model: &row.model,
            variant_context: &row.variant,
            value_reference_year: DEFAULT_VALUE_REFERENCE_YEAR,
            listing_contexts: &[listing_context],
        })
        .await?;
    let mut item = spec_enrichment_item_from_response(
        &AircraftVariantOption {
            manufacturer_id: row.manufacturer_id,
            manufacturer: row.manufacturer,
            model_id: row.model_id,
            model: row.model,
            variant_id: row.variant_id,
            variant: row.variant,
            listing_count: 1,
        },
        &response,
        1,
    )?;
    item.applied_spec_id = Some(
        insert_aircraft_spec(
            db,
            &item,
            DEFAULT_VALUE_REFERENCE_YEAR,
            row.source_url.as_deref(),
        )
        .await?,
    );
    Ok(())
}

async fn aircraft_option_for_variant(
    db: &AppDb,
    user_id: i64,
    variant_id: i64,
) -> StoreResult<AircraftVariantOption> {
    let row = query_as_optional!(
        db,
        AircraftVariantOptionRow,
        r#"
        SELECT
          mfr.id AS manufacturer_id,
          mfr.name AS manufacturer,
          model.id AS model_id,
          model.name AS model,
          variant.id AS variant_id,
          variant.name AS variant,
          COUNT(l.id) AS listing_count
        FROM aircraft_model_variants variant
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        LEFT JOIN aircraft_sale_listings l
          ON l.aircraft_model_variant_id = variant.id
          AND (l.is_verified = TRUE OR l.created_by_user_id = ?)
        WHERE variant.id = ?
        GROUP BY
          mfr.id,
          mfr.name,
          model.id,
          model.name,
          variant.id,
          variant.name
        "#,
        user_id,
        variant_id
    )?;
    row.map(option_from_row)
        .ok_or_else(|| AircraftStoreError::NotFound("aircraft variant not found".to_string()))
}

async fn aircraft_spec_for_variant(
    db: &AppDb,
    model_id: i64,
    variant_id: i64,
) -> StoreResult<Option<AircraftSpecVersionRow>> {
    Ok(query_as_optional!(
        db,
        AircraftSpecVersionRow,
        r#"
        SELECT
          spec.id,
          spec.aircraft_model_id,
          spec.aircraft_model_variant_id,
          spec.effective_from,
          spec.effective_to,
          spec.depreciation_profile_id,
          profile.name AS depreciation_profile,
          profile.age_decay_rate AS depreciation_profile_age_decay_rate,
          profile.long_run_residual_fraction AS depreciation_profile_long_run_residual_fraction,
          profile.new_to_used_discount_fraction AS depreciation_profile_new_to_used_discount_fraction,
          profile.airframe_doubling_discount AS depreciation_profile_airframe_doubling_discount,
          profile.max_airframe_premium AS depreciation_profile_max_airframe_premium,
          profile.max_airframe_discount AS depreciation_profile_max_airframe_discount,
          profile.replacement_floor_fraction AS depreciation_profile_replacement_floor_fraction,
          profile.high_time_threshold_hours AS depreciation_profile_high_time_threshold_hours,
          profile.high_time_discount_at_double_threshold AS depreciation_profile_high_time_discount_at_double_threshold,
          profile_fit.fit_scope AS depreciation_fit_scope,
          profile_fit.fit_scope_key AS depreciation_fit_scope_key,
          profile_fit.sample_count AS depreciation_fit_sample_count,
          profile_fit.rmse_usd AS depreciation_fit_rmse_usd,
          profile_fit.mae_fraction AS depreciation_fit_mae_fraction,
          spec.average_inflation_rate,
          spec.fuel_burn_gph,
          spec.oil_quarts_per_hour,
          spec.oil_price_per_quart_usd,
          spec.engine_model_id,
          engine_mfr.name AS engine_manufacturer,
          engine_model.name AS engine_model,
          spec.engine_count,
          COALESCE(spec.engine_tbo_hours, engine_model.tbo_hours) AS engine_tbo_hours,
          COALESCE(spec.engine_overhaul_cost_usd, engine_model.overhaul_cost_usd) AS engine_overhaul_cost_usd,
          engine_model.value_reference_year AS engine_overhaul_cost_value_reference_year,
          spec.engine_value_baseline_life_fraction,
          spec.propeller_model_id,
          prop_mfr.name AS propeller_manufacturer,
          prop_model.name AS propeller_model,
          spec.propeller_count,
          COALESCE(spec.propeller_tbo_hours, prop_model.tbo_hours) AS propeller_tbo_hours,
          COALESCE(spec.propeller_overhaul_cost_usd, prop_model.overhaul_cost_usd) AS propeller_overhaul_cost_usd,
          prop_model.value_reference_year AS propeller_overhaul_cost_value_reference_year,
          spec.propeller_value_baseline_life_fraction,
          spec.annual_inspection_usd,
          spec.other_maintenance_per_hour,
          spec.source_url,
          spec.created_at,
          spec.updated_at
        FROM aircraft_model_spec_versions spec
        LEFT JOIN depreciation_profiles profile
          ON profile.id = spec.depreciation_profile_id
        LEFT JOIN depreciation_profile_fit_metadata profile_fit
          ON profile_fit.depreciation_profile_id = profile.id
        LEFT JOIN engine_models engine_model
          ON engine_model.id = spec.engine_model_id
        LEFT JOIN engine_manufacturers engine_mfr
          ON engine_mfr.id = engine_model.engine_manufacturer_id
        LEFT JOIN propeller_models prop_model
          ON prop_model.id = spec.propeller_model_id
        LEFT JOIN propeller_manufacturers prop_mfr
          ON prop_mfr.id = prop_model.propeller_manufacturer_id
        WHERE spec.aircraft_model_id = ?
          AND spec.aircraft_model_variant_id = ?
        ORDER BY
          spec.effective_from DESC,
          spec.id DESC
        LIMIT 1
        "#,
        model_id,
        variant_id
    )?)
}

async fn listing_points_for_variant(
    db: &AppDb,
    user_id: i64,
    variant_id: i64,
) -> StoreResult<Vec<AircraftListingPointRow>> {
    Ok(query_as_all!(
        db,
        AircraftListingPointRow,
        r#"
        SELECT
          id,
          aircraft_model_variant_id,
          is_verified,
          source_url,
          model_year,
          asking_price_usd,
          currency,
          added_at,
          status,
          registration_number,
          serial_number,
          airframe_hours,
          engine_hours,
          propeller_hours
        FROM aircraft_sale_listings
        WHERE aircraft_model_variant_id = ?
          AND (is_verified = TRUE OR created_by_user_id = ?)
        ORDER BY model_year, airframe_hours, id
        "#,
        variant_id,
        user_id
    )?)
}

async fn listing_point_for_listing(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
) -> StoreResult<Option<AircraftListingPointRow>> {
    Ok(query_as_optional!(
        db,
        AircraftListingPointRow,
        r#"
        SELECT
          id,
          aircraft_model_variant_id,
          is_verified,
          source_url,
          model_year,
          asking_price_usd,
          currency,
          added_at,
          status,
          registration_number,
          serial_number,
          airframe_hours,
          engine_hours,
          propeller_hours
        FROM aircraft_sale_listings
        WHERE id = ?
          AND (is_verified = TRUE OR created_by_user_id = ?)
        "#,
        listing_id,
        user_id
    )?)
}

async fn listing_value_point(
    db: &AppDb,
    row: &AircraftListingPointRow,
    spec: Option<&AircraftSpecVersionRow>,
    curve_annual_airframe_hours: Option<f64>,
) -> StoreResult<AircraftListingValuePoint> {
    let mut point = AircraftListingValuePoint {
        listing_id: row.id,
        is_verified: row.is_verified,
        source_url: row.source_url.clone(),
        model_year: row.model_year,
        asking_price_usd: row.asking_price_usd,
        currency: row.currency.clone(),
        added_at: row.added_at.clone(),
        status: row.status.clone(),
        registration_number: row.registration_number.clone(),
        serial_number: row.serial_number.clone(),
        airframe_hours: row.airframe_hours,
        engine_hours: row.engine_hours,
        propeller_hours: row.propeller_hours,
        estimated_value_usd: None,
        estimate_error: None,
        breakdown: None,
        value_curve: Vec::new(),
    };

    let Some(spec) = spec else {
        point.estimate_error = Some("aircraft spec metadata missing".to_string());
        return Ok(point);
    };

    let profile = match aircraft_profile_for_spec(db, spec).await {
        Ok(profile) => profile,
        Err(error) => {
            point.estimate_error = Some(error.to_string());
            return Ok(point);
        }
    };
    let spec_effective_year = year_from_effective_from(&spec.effective_from);
    let valuation_year = year_from_date_prefix(&row.added_at).unwrap_or(spec_effective_year);
    let replacement_basis_rows =
        model_family_replacement_basis_rows(db, spec.aircraft_model_id).await?;
    let replacement_floor_basis_usd =
        replacement_basis_for_year(&replacement_basis_rows, valuation_year);
    let price_point =
        model_year_price_point(db, row.aircraft_model_variant_id, row.model_year).await?;
    let Some(price_point) = price_point else {
        point.estimate_error = Some("model-year price point missing".to_string());
        return Ok(point);
    };
    let purchase_price_reference_year = price_point.purchase_price_reference_year;
    let default_avionics_rows =
        model_year_default_avionics_estimates(db, row.aircraft_model_variant_id, row.model_year)
            .await?;
    let purchase_price_new_usd = airframe_basis_excluding_default_avionics(
        price_point.purchase_price_new_usd,
        &default_avionics_rows,
        purchase_price_reference_year,
        spec_effective_year,
        spec.average_inflation_rate,
    );
    let age_years = (valuation_year - row.model_year).max(0) as f64;
    let mut avionics_rows = listing_avionics_estimates(db, row.id).await?;
    let mut avionics = avionics_components_from_rows(
        &avionics_rows,
        valuation_year,
        spec_effective_year,
        spec.average_inflation_rate,
    );
    if avionics.is_empty() {
        avionics_rows = default_avionics_rows;
        avionics = avionics_components_from_rows(
            &avionics_rows,
            valuation_year,
            spec_effective_year,
            spec.average_inflation_rate,
        );
    }
    match estimate_listing_value(
        spec,
        profile.clone(),
        purchase_price_new_usd,
        purchase_price_reference_year,
        valuation_year,
        age_years,
        row.airframe_hours,
        row.engine_hours,
        row.propeller_hours,
        &avionics,
        replacement_floor_basis_usd,
    ) {
        Ok(estimate) => {
            point.estimated_value_usd = Some(estimate.estimated_value_usd);
            point.breakdown = Some(breakdown_from_estimate(&estimate));
        }
        Err(error) => {
            point.estimate_error = Some(error);
        }
    }
    point.value_curve = listing_value_curve(
        spec,
        profile,
        purchase_price_new_usd,
        purchase_price_reference_year,
        row,
        spec_effective_year,
        valuation_year,
        curve_annual_airframe_hours,
        &avionics_rows,
        &replacement_basis_rows,
    );
    Ok(point)
}

async fn aircraft_profile_for_spec(
    db: &AppDb,
    spec: &AircraftSpecVersionRow,
) -> StoreResult<AircraftProfile> {
    if let Some(profile_id) = spec.depreciation_profile_id {
        if let Some(row) = depreciation_profile_by_id(db, profile_id).await? {
            return Ok(aircraft_profile_from_row(row));
        }
    }
    let profile_name = spec
        .depreciation_profile
        .as_deref()
        .unwrap_or("light_piston");
    get_aircraft_profile(profile_name).map_err(AircraftStoreError::Model)
}

async fn depreciation_profile_by_id(
    db: &AppDb,
    profile_id: i64,
) -> StoreResult<Option<DepreciationProfileRow>> {
    Ok(query_as_optional!(
        db,
        DepreciationProfileRow,
        r#"
        SELECT
          name,
          age_decay_rate,
          long_run_residual_fraction,
          new_to_used_discount_fraction,
          new_to_used_discount_years,
          airframe_doubling_discount,
          max_airframe_premium,
          max_airframe_discount,
          replacement_floor_fraction,
          minimum_value_fraction,
          high_time_threshold_hours,
          high_time_discount_at_double_threshold
        FROM depreciation_profiles
        WHERE id = ?
        "#,
        profile_id
    )?)
}

fn aircraft_profile_from_row(row: DepreciationProfileRow) -> AircraftProfile {
    AircraftProfile {
        name: row.name,
        age_decay_rate: row.age_decay_rate,
        long_run_residual_fraction: row.long_run_residual_fraction,
        new_to_used_discount_fraction: row.new_to_used_discount_fraction,
        new_to_used_discount_years: row.new_to_used_discount_years,
        airframe_doubling_discount: row.airframe_doubling_discount,
        max_airframe_premium: row.max_airframe_premium,
        max_airframe_discount: row.max_airframe_discount,
        replacement_floor_fraction: row.replacement_floor_fraction,
        minimum_value_fraction: row.minimum_value_fraction,
        high_time_threshold_hours: row.high_time_threshold_hours,
        high_time_discount_at_double_threshold: row.high_time_discount_at_double_threshold,
    }
}

fn estimate_listing_value(
    spec: &AircraftSpecVersionRow,
    profile: AircraftProfile,
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
    valuation_year: i64,
    age_years: f64,
    airframe_hours: f64,
    engine_hours: f64,
    propeller_hours: f64,
    avionics: &[AvionicsComponent],
    replacement_floor_basis_usd: Option<f64>,
) -> Result<PriceEstimate, String> {
    let engine = timed_component(
        "engine",
        engine_hours,
        spec.engine_count,
        spec.engine_tbo_hours,
        spec.engine_overhaul_cost_usd,
        spec.engine_overhaul_cost_value_reference_year,
        spec.engine_value_baseline_life_fraction,
        valuation_year,
        spec.average_inflation_rate,
    );
    let propeller = timed_component(
        "propeller",
        propeller_hours,
        spec.propeller_count,
        spec.propeller_tbo_hours,
        spec.propeller_overhaul_cost_usd,
        spec.propeller_overhaul_cost_value_reference_year,
        spec.propeller_value_baseline_life_fraction,
        valuation_year,
        spec.average_inflation_rate,
    );
    estimate_aircraft_value_in_year(
        purchase_price_new_usd,
        age_years,
        airframe_hours,
        DEFAULT_ANNUAL_AIRFRAME_HOURS,
        profile,
        engine,
        propeller,
        avionics,
        replacement_floor_basis_usd,
        DollarBasis {
            value_reference_year: purchase_price_reference_year,
            valuation_year,
            average_inflation_rate: spec.average_inflation_rate,
        },
    )
}

fn listing_value_curve(
    spec: &AircraftSpecVersionRow,
    profile: AircraftProfile,
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
    listing: &AircraftListingPointRow,
    avionics_fallback_value_reference_year: i64,
    current_valuation_year: i64,
    curve_annual_airframe_hours: Option<f64>,
    avionics_rows: &[AvionicsEstimateRow],
    replacement_basis_rows: &[ReplacementBasisRow],
) -> Vec<AircraftValueCurvePoint> {
    let curve_end_year = current_valuation_year.max(listing.model_year) + 30;
    let projected_annual_airframe_hours =
        curve_annual_airframe_hours.unwrap_or(DEFAULT_ANNUAL_AIRFRAME_HOURS);
    (listing.model_year..=curve_end_year)
        .map(|valuation_year| {
            let age_years = (valuation_year - listing.model_year).max(0) as f64;
            let airframe_hours = projected_component_hours(
                listing.airframe_hours,
                listing.model_year,
                current_valuation_year,
                valuation_year,
                projected_annual_airframe_hours,
            );
            let engine_hours = baseline_timed_component_hours(
                spec.engine_tbo_hours,
                spec.engine_value_baseline_life_fraction,
            );
            let propeller_hours = baseline_timed_component_hours(
                spec.propeller_tbo_hours,
                spec.propeller_value_baseline_life_fraction,
            );
            let avionics = avionics_components_from_rows(
                avionics_rows,
                valuation_year,
                avionics_fallback_value_reference_year,
                spec.average_inflation_rate,
            );
            let replacement_floor_basis_usd =
                replacement_basis_for_year(replacement_basis_rows, valuation_year);
            let result = estimate_listing_value(
                spec,
                profile.clone(),
                purchase_price_new_usd,
                purchase_price_reference_year,
                valuation_year,
                age_years,
                airframe_hours,
                engine_hours,
                propeller_hours,
                &avionics,
                replacement_floor_basis_usd,
            );
            match result {
                Ok(estimate) => AircraftValueCurvePoint {
                    valuation_year,
                    age_years,
                    airframe_hours,
                    engine_hours,
                    propeller_hours,
                    estimated_value_usd: Some(estimate.estimated_value_usd),
                    estimate_error: None,
                },
                Err(error) => AircraftValueCurvePoint {
                    valuation_year,
                    age_years,
                    airframe_hours,
                    engine_hours,
                    propeller_hours,
                    estimated_value_usd: None,
                    estimate_error: Some(error),
                },
            }
        })
        .collect()
}

async fn listing_avionics_estimates(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<Vec<AvionicsEstimateRow>> {
    Ok(query_as_all!(
        db,
        AvionicsEstimateRow,
        r#"
        SELECT
          mfr.name AS manufacturer,
          model.name AS model,
          avionics_type.name AS avionics_type,
          link.quantity,
          model.introduced_year,
          model.estimated_unit_value_usd,
          model.value_reference_year
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model
          ON model.id = link.avionics_model_id
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        JOIN avionics_types avionics_type
          ON avionics_type.id = model.avionics_type_id
        WHERE link.aircraft_sale_listing_id = ?
        ORDER BY link.id
        "#,
        listing_id
    )?)
}

async fn model_year_default_avionics_estimates(
    db: &AppDb,
    aircraft_model_variant_id: i64,
    model_year: i64,
) -> StoreResult<Vec<AvionicsEstimateRow>> {
    Ok(query_as_all!(
        db,
        AvionicsEstimateRow,
        r#"
        SELECT
          mfr.name AS manufacturer,
          model.name AS model,
          avionics_type.name AS avionics_type,
          default_avionics.quantity,
          model.introduced_year,
          model.estimated_unit_value_usd,
          model.value_reference_year
        FROM aircraft_model_variant_default_avionics default_avionics
        JOIN avionics_models model
          ON model.id = default_avionics.avionics_model_id
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        JOIN avionics_types avionics_type
          ON avionics_type.id = model.avionics_type_id
        WHERE default_avionics.aircraft_model_variant_id = ?
          AND default_avionics.model_year = ?
        ORDER BY default_avionics.id
        "#,
        aircraft_model_variant_id,
        model_year
    )?)
}

async fn model_year_price_point(
    db: &AppDb,
    aircraft_model_variant_id: i64,
    model_year: i64,
) -> StoreResult<Option<AircraftModelYearPricePointRow>> {
    Ok(query_as_optional!(
        db,
        AircraftModelYearPricePointRow,
        r#"
        SELECT purchase_price_new_usd, purchase_price_reference_year
        FROM aircraft_model_variant_price_points
        WHERE aircraft_model_variant_id = ?
          AND model_year = ?
        "#,
        aircraft_model_variant_id,
        model_year
    )?)
}

async fn model_family_replacement_basis_rows(
    db: &AppDb,
    aircraft_model_id: i64,
) -> StoreResult<Vec<ReplacementBasisRow>> {
    Ok(query_as_all!(
        db,
        ReplacementBasisRow,
        r#"
        SELECT
          price_point.purchase_price_new_usd,
          price_point.purchase_price_reference_year,
          spec.average_inflation_rate
        FROM aircraft_model_variant_price_points price_point
        JOIN aircraft_model_variants variant
          ON variant.id = price_point.aircraft_model_variant_id
        JOIN aircraft_model_spec_versions spec
          ON spec.aircraft_model_id = variant.aircraft_model_id
         AND spec.aircraft_model_variant_id = variant.id
        WHERE variant.aircraft_model_id = ?
          AND spec.id = (
            SELECT latest.id
            FROM aircraft_model_spec_versions latest
            WHERE latest.aircraft_model_id = variant.aircraft_model_id
              AND latest.aircraft_model_variant_id = variant.id
            ORDER BY latest.effective_from DESC, latest.id DESC
            LIMIT 1
          )
        "#,
        aircraft_model_id
    )?)
}

fn replacement_basis_for_year(rows: &[ReplacementBasisRow], valuation_year: i64) -> Option<f64> {
    rows.iter()
        .filter(|row| row.purchase_price_reference_year <= valuation_year)
        .filter_map(|row| {
            nominal_dollar_factor(
                row.purchase_price_reference_year,
                valuation_year,
                row.average_inflation_rate,
            )
            .ok()
            .map(|factor| row.purchase_price_new_usd * factor)
        })
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
}

fn avionics_components_from_rows(
    rows: &[AvionicsEstimateRow],
    valuation_year: i64,
    fallback_value_reference_year: i64,
    average_inflation_rate: f64,
) -> Vec<AvionicsComponent> {
    let profile = default_avionics_profile();
    rows.iter()
        .filter_map(|row| {
            let introduced_year = row.introduced_year?;
            if introduced_year > valuation_year {
                return None;
            }
            let unit_replacement_cost_usd = row.estimated_unit_value_usd?;
            (unit_replacement_cost_usd >= 0.0).then(|| AvionicsComponent {
                name: format!("{} {} {}", row.manufacturer, row.model, row.avionics_type),
                introduced_year,
                valuation_year,
                value_reference_year: row
                    .value_reference_year
                    .unwrap_or(fallback_value_reference_year),
                average_inflation_rate,
                unit_replacement_cost_usd,
                quantity: row.quantity.max(1),
                profile: profile.clone(),
            })
        })
        .collect()
}

fn airframe_basis_excluding_default_avionics(
    model_year_purchase_price_new_usd: f64,
    default_avionics_rows: &[AvionicsEstimateRow],
    purchase_price_reference_year: i64,
    fallback_value_reference_year: i64,
    average_inflation_rate: f64,
) -> f64 {
    let default_avionics = avionics_components_from_rows(
        default_avionics_rows,
        purchase_price_reference_year,
        fallback_value_reference_year,
        average_inflation_rate,
    );
    let default_avionics_basis = avionics_replacement_basis(&default_avionics).unwrap_or_default();
    (model_year_purchase_price_new_usd - default_avionics_basis)
        .max(model_year_purchase_price_new_usd * 0.2)
}

fn projected_component_hours(
    current_hours: f64,
    model_year: i64,
    current_valuation_year: i64,
    target_year: i64,
    annual_hours: f64,
) -> f64 {
    let current_age = (current_valuation_year - model_year).max(0) as f64;
    let target_age = (target_year - model_year).max(0) as f64;
    if target_year <= current_valuation_year {
        if current_age > 0.0 {
            current_hours * (target_age / current_age)
        } else {
            annual_hours * target_age
        }
    } else {
        current_hours + annual_hours * (target_year - current_valuation_year) as f64
    }
    .max(0.0)
}

fn baseline_timed_component_hours(tbo_hours: Option<f64>, baseline_life_fraction: f64) -> f64 {
    let Some(tbo_hours) = tbo_hours else {
        return 0.0;
    };
    tbo_hours * baseline_life_fraction.clamp(0.0, 1.0)
}

fn breakdown_from_estimate(estimate: &PriceEstimate) -> AircraftValueBreakdown {
    AircraftValueBreakdown {
        effective_new_price_usd: estimate.breakdown.effective_new_price_usd,
        value_reference_year: estimate.breakdown.value_reference_year,
        valuation_year: estimate.breakdown.valuation_year,
        average_inflation_rate: estimate.breakdown.average_inflation_rate,
        dollar_basis_factor: estimate.breakdown.dollar_basis_factor,
        age_residual_fraction: estimate.breakdown.age_residual_fraction,
        age_baseline_value_usd: estimate.breakdown.age_baseline_value_usd,
        expected_airframe_hours: estimate.breakdown.expected_airframe_hours,
        airframe_factor: estimate.breakdown.airframe_factor,
        high_time_factor: estimate.breakdown.high_time_factor,
        airframe_value_usd: estimate.breakdown.airframe_value_usd,
        replacement_floor_basis_usd: estimate.breakdown.replacement_floor_basis_usd,
        replacement_floor_value_usd: estimate.breakdown.replacement_floor_value_usd,
        engine_adjustment_usd: estimate.breakdown.engine_adjustment_usd,
        propeller_adjustment_usd: estimate.breakdown.propeller_adjustment_usd,
        avionics_value_usd: estimate.breakdown.avionics_value_usd,
        avionics_replacement_basis_usd: estimate.breakdown.avionics_replacement_basis_usd,
        minimum_value_usd: estimate.breakdown.minimum_value_usd,
        raw_estimated_value_usd: estimate.breakdown.raw_estimated_value_usd,
        valuation_basis_usd: estimate.breakdown.valuation_basis_usd,
        depreciation_usd: estimate.depreciation_usd,
        depreciation_fraction: estimate.depreciation_fraction,
    }
}

async fn aircraft_variants_missing_specs(
    db: &AppDb,
    limit: i64,
) -> StoreResult<Vec<AircraftVariantOption>> {
    let rows = query_as_all!(
        db,
        AircraftVariantOptionRow,
        r#"
        SELECT
          mfr.id AS manufacturer_id,
          mfr.name AS manufacturer,
          model.id AS model_id,
          model.name AS model,
          variant.id AS variant_id,
          variant.name AS variant,
          COUNT(DISTINCT l.id) AS listing_count
        FROM aircraft_model_variants variant
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        JOIN aircraft_sale_listings l
          ON l.aircraft_model_variant_id = variant.id
        JOIN plugin_submissions submission
          ON submission.canonical_listing_id = l.id
        WHERE NOT EXISTS (
          SELECT 1
          FROM aircraft_model_spec_versions spec
          WHERE spec.aircraft_model_id = model.id
            AND spec.aircraft_model_variant_id = variant.id
        )
        GROUP BY
          mfr.id,
          mfr.name,
          model.id,
          model.name,
          variant.id,
          variant.name
        ORDER BY listing_count DESC, mfr.name, model.name, variant.name
        LIMIT ?
        "#,
        limit
    )?;
    Ok(rows.into_iter().map(option_from_row).collect())
}

async fn aircraft_variants_with_plugin_evidence(
    db: &AppDb,
    limit: i64,
) -> StoreResult<Vec<AircraftVariantOption>> {
    let rows = query_as_all!(
        db,
        AircraftVariantOptionRow,
        r#"
        SELECT
          mfr.id AS manufacturer_id,
          mfr.name AS manufacturer,
          model.id AS model_id,
          model.name AS model,
          variant.id AS variant_id,
          variant.name AS variant,
          COUNT(DISTINCT l.id) AS listing_count
        FROM aircraft_model_variants variant
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        JOIN aircraft_sale_listings l
          ON l.aircraft_model_variant_id = variant.id
        JOIN plugin_submissions submission
          ON submission.canonical_listing_id = l.id
        GROUP BY
          mfr.id,
          mfr.name,
          model.id,
          model.name,
          variant.id,
          variant.name
        ORDER BY listing_count DESC, mfr.name, model.name, variant.name
        LIMIT ?
        "#,
        limit
    )?;
    Ok(rows.into_iter().map(option_from_row).collect())
}

async fn plugin_spec_evidence_for_variant(
    db: &AppDb,
    variant_id: i64,
) -> StoreResult<Vec<PluginSpecEvidenceRow>> {
    Ok(query_as_all!(
        db,
        PluginSpecEvidenceRow,
        r#"
        SELECT
          listing.model_year,
          listing.asking_price_usd,
          listing.airframe_hours,
          listing.engine_hours,
          listing.propeller_hours,
          submission.source_url,
          submission.rendered_html
        FROM plugin_submissions submission
        JOIN aircraft_sale_listings listing
          ON listing.id = submission.canonical_listing_id
        WHERE listing.aircraft_model_variant_id = ?
        ORDER BY listing.added_at DESC, submission.submitted_at DESC
        LIMIT ?
        "#,
        variant_id,
        SPEC_EVIDENCE_LIMIT
    )?)
}

async fn aircraft_spec_exists_for_variant(
    db: &AppDb,
    model_id: i64,
    variant_id: i64,
) -> StoreResult<bool> {
    let count = query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT 1
        FROM aircraft_model_spec_versions
        WHERE aircraft_model_id = ?
          AND aircraft_model_variant_id = ?
        LIMIT 1
        "#,
        model_id,
        variant_id
    )?;
    Ok(count.is_some())
}

async fn listing_spec_seed(db: &AppDb, listing_id: i64) -> StoreResult<ListingSpecSeedRow> {
    let row = query_as_optional!(
        db,
        ListingSpecSeedRow,
        r#"
        SELECT
          mfr.id AS manufacturer_id,
          mfr.name AS manufacturer,
          model.id AS model_id,
          model.name AS model,
          variant.id AS variant_id,
          variant.name AS variant,
          listing.model_year,
          listing.asking_price_usd,
          listing.airframe_hours,
          listing.engine_hours,
          listing.propeller_hours,
          listing.source_url
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE listing.id = ?
        "#,
        listing_id
    )?;
    row.ok_or_else(|| AircraftStoreError::NotFound("listing not found".to_string()))
}

async fn insert_aircraft_spec(
    db: &AppDb,
    item: &AircraftSpecEnrichmentItem,
    value_reference_year: i64,
    source_url: Option<&str>,
) -> StoreResult<i64> {
    let profile_id = depreciation_profile_id(db, &item.depreciation_profile)
        .await?
        .ok_or_else(|| {
            AircraftStoreError::Model(format!(
                "depreciation profile not found: {}",
                item.depreciation_profile
            ))
        })?;
    let engine_model_id = ensure_engine_model(
        db,
        &item.engine_manufacturer,
        &item.engine_model,
        item.engine_tbo_hours,
        item.engine_overhaul_cost_usd,
        value_reference_year,
        &item.powerplant_source_url,
        &item.powerplant_source_title,
        &item.powerplant_source_confidence,
    )
    .await?;
    let propeller_model_id = ensure_propeller_model(
        db,
        &item.propeller_manufacturer,
        &item.propeller_model,
        item.propeller_tbo_hours,
        item.propeller_overhaul_cost_usd,
        value_reference_year,
        &item.powerplant_source_url,
        &item.powerplant_source_title,
        &item.powerplant_source_confidence,
    )
    .await?;
    let effective_from = format!("{value_reference_year:04}-01-01");
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        INSERT INTO aircraft_model_spec_versions (
          aircraft_model_id,
          aircraft_model_variant_id,
          effective_from,
          depreciation_profile_id,
          average_inflation_rate,
          fuel_burn_gph,
          oil_quarts_per_hour,
          oil_price_per_quart_usd,
          engine_model_id,
          engine_count,
          engine_tbo_hours,
          engine_overhaul_cost_usd,
          engine_value_baseline_life_fraction,
          propeller_model_id,
          propeller_count,
          propeller_tbo_hours,
          propeller_overhaul_cost_usd,
          propeller_value_baseline_life_fraction,
          annual_inspection_usd,
          other_maintenance_per_hour,
          source_url
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        RETURNING id
        "#,
        item.model_id,
        item.variant_id,
        effective_from.as_str(),
        profile_id,
        item.average_inflation_rate,
        item.fuel_burn_gph,
        item.oil_quarts_per_hour,
        item.oil_price_per_quart_usd,
        engine_model_id,
        item.engine_count,
        item.engine_tbo_hours,
        item.engine_overhaul_cost_usd,
        item.engine_value_baseline_life_fraction,
        propeller_model_id,
        item.propeller_count,
        item.propeller_tbo_hours,
        item.propeller_overhaul_cost_usd,
        item.propeller_value_baseline_life_fraction,
        item.annual_inspection_usd,
        item.other_maintenance_per_hour,
        source_url,
    )?)
}

async fn upsert_aircraft_spec(
    db: &AppDb,
    item: &AircraftSpecEnrichmentItem,
    value_reference_year: i64,
    source_url: Option<&str>,
) -> StoreResult<i64> {
    if let Some(spec_id) = latest_aircraft_spec_id(db, item.model_id, item.variant_id).await? {
        update_aircraft_spec(db, spec_id, item, value_reference_year, source_url).await?;
        Ok(spec_id)
    } else {
        insert_aircraft_spec(db, item, value_reference_year, source_url).await
    }
}

async fn latest_aircraft_spec_id(
    db: &AppDb,
    model_id: i64,
    variant_id: i64,
) -> StoreResult<Option<i64>> {
    Ok(query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT id
        FROM aircraft_model_spec_versions
        WHERE aircraft_model_id = ?
          AND aircraft_model_variant_id = ?
        ORDER BY effective_from DESC, id DESC
        LIMIT 1
        "#,
        model_id,
        variant_id
    )?)
}

async fn update_aircraft_spec(
    db: &AppDb,
    spec_id: i64,
    item: &AircraftSpecEnrichmentItem,
    value_reference_year: i64,
    source_url: Option<&str>,
) -> StoreResult<()> {
    let profile_id = depreciation_profile_id(db, &item.depreciation_profile)
        .await?
        .ok_or_else(|| {
            AircraftStoreError::Model(format!(
                "depreciation profile not found: {}",
                item.depreciation_profile
            ))
        })?;
    let engine_model_id = ensure_engine_model(
        db,
        &item.engine_manufacturer,
        &item.engine_model,
        item.engine_tbo_hours,
        item.engine_overhaul_cost_usd,
        value_reference_year,
        &item.powerplant_source_url,
        &item.powerplant_source_title,
        &item.powerplant_source_confidence,
    )
    .await?;
    let propeller_model_id = ensure_propeller_model(
        db,
        &item.propeller_manufacturer,
        &item.propeller_model,
        item.propeller_tbo_hours,
        item.propeller_overhaul_cost_usd,
        value_reference_year,
        &item.powerplant_source_url,
        &item.powerplant_source_title,
        &item.powerplant_source_confidence,
    )
    .await?;
    let effective_from = format!("{value_reference_year:04}-01-01");
    execute_query!(
        db,
        r#"
        UPDATE aircraft_model_spec_versions
        SET
          effective_from = ?,
          depreciation_profile_id = ?,
          average_inflation_rate = ?,
          fuel_burn_gph = ?,
          oil_quarts_per_hour = ?,
          oil_price_per_quart_usd = ?,
          engine_model_id = ?,
          engine_count = ?,
          engine_tbo_hours = ?,
          engine_overhaul_cost_usd = ?,
          engine_value_baseline_life_fraction = ?,
          propeller_model_id = ?,
          propeller_count = ?,
          propeller_tbo_hours = ?,
          propeller_overhaul_cost_usd = ?,
          propeller_value_baseline_life_fraction = ?,
          annual_inspection_usd = ?,
          other_maintenance_per_hour = ?,
          source_url = ?,
          updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        effective_from.as_str(),
        profile_id,
        item.average_inflation_rate,
        item.fuel_burn_gph,
        item.oil_quarts_per_hour,
        item.oil_price_per_quart_usd,
        engine_model_id,
        item.engine_count,
        item.engine_tbo_hours,
        item.engine_overhaul_cost_usd,
        item.engine_value_baseline_life_fraction,
        propeller_model_id,
        item.propeller_count,
        item.propeller_tbo_hours,
        item.propeller_overhaul_cost_usd,
        item.propeller_value_baseline_life_fraction,
        item.annual_inspection_usd,
        item.other_maintenance_per_hour,
        source_url,
        spec_id
    )?;
    Ok(())
}

async fn ensure_engine_model(
    db: &AppDb,
    manufacturer: &str,
    model: &str,
    tbo_hours: f64,
    overhaul_cost_usd: f64,
    value_reference_year: i64,
    source_url: &str,
    source_title: &str,
    source_confidence: &str,
) -> StoreResult<i64> {
    let manufacturer_id = ensure_engine_manufacturer(db, manufacturer).await?;
    let normalized_model = normalize_name(model);
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        INSERT INTO engine_models (
          engine_manufacturer_id,
          name,
          normalized_name,
          tbo_hours,
          overhaul_cost_usd,
          value_reference_year,
          source_url,
          source_title,
          source_confidence
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (engine_manufacturer_id, normalized_name) DO UPDATE SET
          name = excluded.name,
          tbo_hours = COALESCE(excluded.tbo_hours, engine_models.tbo_hours),
          overhaul_cost_usd = COALESCE(excluded.overhaul_cost_usd, engine_models.overhaul_cost_usd),
          value_reference_year = COALESCE(excluded.value_reference_year, engine_models.value_reference_year),
          source_url = COALESCE(excluded.source_url, engine_models.source_url),
          source_title = COALESCE(excluded.source_title, engine_models.source_title),
          source_confidence = COALESCE(excluded.source_confidence, engine_models.source_confidence),
          updated_at = CURRENT_TIMESTAMP
        RETURNING id
        "#,
        manufacturer_id,
        model.trim(),
        normalized_model.as_str(),
        tbo_hours,
        overhaul_cost_usd,
        value_reference_year,
        optional_non_empty(source_url),
        optional_non_empty(source_title),
        optional_non_empty(source_confidence),
    )?)
}

async fn ensure_propeller_model(
    db: &AppDb,
    manufacturer: &str,
    model: &str,
    tbo_hours: f64,
    overhaul_cost_usd: f64,
    value_reference_year: i64,
    source_url: &str,
    source_title: &str,
    source_confidence: &str,
) -> StoreResult<i64> {
    let manufacturer_id = ensure_propeller_manufacturer(db, manufacturer).await?;
    let normalized_model = normalize_name(model);
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        INSERT INTO propeller_models (
          propeller_manufacturer_id,
          name,
          normalized_name,
          tbo_hours,
          overhaul_cost_usd,
          value_reference_year,
          source_url,
          source_title,
          source_confidence
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (propeller_manufacturer_id, normalized_name) DO UPDATE SET
          name = excluded.name,
          tbo_hours = COALESCE(excluded.tbo_hours, propeller_models.tbo_hours),
          overhaul_cost_usd = COALESCE(excluded.overhaul_cost_usd, propeller_models.overhaul_cost_usd),
          value_reference_year = COALESCE(excluded.value_reference_year, propeller_models.value_reference_year),
          source_url = COALESCE(excluded.source_url, propeller_models.source_url),
          source_title = COALESCE(excluded.source_title, propeller_models.source_title),
          source_confidence = COALESCE(excluded.source_confidence, propeller_models.source_confidence),
          updated_at = CURRENT_TIMESTAMP
        RETURNING id
        "#,
        manufacturer_id,
        model.trim(),
        normalized_model.as_str(),
        tbo_hours,
        overhaul_cost_usd,
        value_reference_year,
        optional_non_empty(source_url),
        optional_non_empty(source_title),
        optional_non_empty(source_confidence),
    )?)
}

async fn ensure_engine_manufacturer(db: &AppDb, name: &str) -> StoreResult<i64> {
    let normalized_name = normalize_name(name);
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        INSERT INTO engine_manufacturers (name, normalized_name)
        VALUES (?, ?)
        ON CONFLICT (normalized_name) DO UPDATE SET
          name = excluded.name,
          updated_at = CURRENT_TIMESTAMP
        RETURNING id
        "#,
        name.trim(),
        normalized_name.as_str(),
    )?)
}

async fn ensure_propeller_manufacturer(db: &AppDb, name: &str) -> StoreResult<i64> {
    let normalized_name = normalize_name(name);
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        INSERT INTO propeller_manufacturers (name, normalized_name)
        VALUES (?, ?)
        ON CONFLICT (normalized_name) DO UPDATE SET
          name = excluded.name,
          updated_at = CURRENT_TIMESTAMP
        RETURNING id
        "#,
        name.trim(),
        normalized_name.as_str(),
    )?)
}

fn optional_non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

async fn depreciation_profile_id(db: &AppDb, profile_name: &str) -> StoreResult<Option<i64>> {
    Ok(query_scalar_optional!(
        db,
        i64,
        "SELECT id FROM depreciation_profiles WHERE name = ?",
        profile_name
    )?)
}

fn option_from_row(row: AircraftVariantOptionRow) -> AircraftVariantOption {
    AircraftVariantOption {
        manufacturer_id: row.manufacturer_id,
        manufacturer: row.manufacturer,
        model_id: row.model_id,
        model: row.model,
        variant_id: row.variant_id,
        variant: row.variant,
        listing_count: row.listing_count,
    }
}

fn spec_detail_from_row(row: &AircraftSpecVersionRow) -> AircraftSpecDetail {
    let depreciation_profile = row
        .depreciation_profile
        .clone()
        .unwrap_or_else(|| "light_piston".to_string());
    let depreciation_profile_detail = match (
        row.depreciation_profile_age_decay_rate,
        row.depreciation_profile_long_run_residual_fraction,
        row.depreciation_profile_new_to_used_discount_fraction,
        row.depreciation_profile_airframe_doubling_discount,
        row.depreciation_profile_max_airframe_premium,
        row.depreciation_profile_max_airframe_discount,
        row.depreciation_profile_replacement_floor_fraction,
        row.depreciation_profile_high_time_discount_at_double_threshold,
    ) {
        (
            Some(age_decay_rate),
            Some(long_run_residual_fraction),
            Some(new_to_used_discount_fraction),
            Some(airframe_doubling_discount),
            Some(max_airframe_premium),
            Some(max_airframe_discount),
            Some(replacement_floor_fraction),
            Some(high_time_discount_at_double_threshold),
        ) => Some(AircraftDepreciationProfileDetail {
            name: depreciation_profile.clone(),
            age_decay_rate,
            long_run_residual_fraction,
            new_to_used_discount_fraction,
            airframe_doubling_discount,
            max_airframe_premium,
            max_airframe_discount,
            replacement_floor_fraction,
            high_time_threshold_hours: row.depreciation_profile_high_time_threshold_hours,
            high_time_discount_at_double_threshold,
            fit_scope: row.depreciation_fit_scope.clone(),
            fit_scope_key: row.depreciation_fit_scope_key.clone(),
            sample_count: row.depreciation_fit_sample_count,
            rmse_usd: row.depreciation_fit_rmse_usd,
            mae_fraction: row.depreciation_fit_mae_fraction,
        }),
        _ => None,
    };
    AircraftSpecDetail {
        id: row.id,
        aircraft_model_id: row.aircraft_model_id,
        aircraft_model_variant_id: row.aircraft_model_variant_id,
        effective_from: row.effective_from.clone(),
        effective_to: row.effective_to.clone(),
        depreciation_profile_id: row.depreciation_profile_id,
        depreciation_profile,
        depreciation_profile_detail,
        average_inflation_rate: row.average_inflation_rate,
        fuel_burn_gph: row.fuel_burn_gph,
        oil_quarts_per_hour: row.oil_quarts_per_hour,
        oil_price_per_quart_usd: row.oil_price_per_quart_usd,
        engine_model_id: row.engine_model_id,
        engine_manufacturer: row.engine_manufacturer.clone(),
        engine_model: row.engine_model.clone(),
        engine_count: row.engine_count,
        engine_tbo_hours: row.engine_tbo_hours,
        engine_overhaul_cost_usd: row.engine_overhaul_cost_usd,
        engine_value_baseline_life_fraction: row.engine_value_baseline_life_fraction,
        propeller_model_id: row.propeller_model_id,
        propeller_manufacturer: row.propeller_manufacturer.clone(),
        propeller_model: row.propeller_model.clone(),
        propeller_count: row.propeller_count,
        propeller_tbo_hours: row.propeller_tbo_hours,
        propeller_overhaul_cost_usd: row.propeller_overhaul_cost_usd,
        propeller_value_baseline_life_fraction: row.propeller_value_baseline_life_fraction,
        annual_inspection_usd: row.annual_inspection_usd,
        other_maintenance_per_hour: row.other_maintenance_per_hour,
        source_url: row.source_url.clone(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

fn timed_component(
    name: &str,
    hours_since_overhaul: f64,
    count: i64,
    tbo_hours: Option<f64>,
    overhaul_cost_usd: Option<f64>,
    value_reference_year: Option<i64>,
    baseline_life_fraction: f64,
    valuation_year: i64,
    average_inflation_rate: f64,
) -> Option<TimedComponent> {
    let tbo_hours = tbo_hours?;
    let overhaul_cost_usd = overhaul_cost_usd?;
    (count > 0 && tbo_hours > 0.0 && overhaul_cost_usd >= 0.0).then(|| TimedComponent {
        name: name.to_string(),
        hours_since_overhaul,
        tbo_hours,
        overhaul_cost_usd,
        value_reference_year: value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR),
        valuation_year,
        average_inflation_rate,
        count,
        baseline_life_fraction,
    })
}

fn spec_enrichment_item_from_response(
    variant: &AircraftVariantOption,
    response: &Value,
    evidence_count: usize,
) -> StoreResult<AircraftSpecEnrichmentItem> {
    let depreciation_profile = required_string(response, "depreciation_profile")?;
    Ok(AircraftSpecEnrichmentItem {
        manufacturer_id: variant.manufacturer_id,
        manufacturer: variant.manufacturer.clone(),
        model_id: variant.model_id,
        model: variant.model.clone(),
        variant_id: variant.variant_id,
        variant: variant.variant.clone(),
        listing_count: variant.listing_count,
        depreciation_profile,
        average_inflation_rate: DEFAULT_AVERAGE_INFLATION_RATE,
        fuel_burn_gph: required_min_f64(response, "fuel_burn_gph", 0.0)?,
        oil_quarts_per_hour: required_min_f64(response, "oil_quarts_per_hour", 0.0)?,
        oil_price_per_quart_usd: required_min_f64(response, "oil_price_per_quart_usd", 0.0)?,
        engine_manufacturer: required_string(response, "engine_manufacturer")?,
        engine_model: required_string(response, "engine_model")?,
        engine_count: required_min_i64(response, "engine_count", 0)?,
        engine_tbo_hours: required_min_f64(response, "engine_tbo_hours", 0.0)?,
        engine_overhaul_cost_usd: required_min_f64(response, "engine_overhaul_cost_usd", 0.0)?,
        engine_value_baseline_life_fraction: required_fraction(
            response,
            "engine_value_baseline_life_fraction",
        )?,
        propeller_count: required_min_i64(response, "propeller_count", 0)?,
        propeller_tbo_hours: required_min_f64(response, "propeller_tbo_hours", 0.0)?,
        propeller_overhaul_cost_usd: required_min_f64(
            response,
            "propeller_overhaul_cost_usd",
            0.0,
        )?,
        propeller_value_baseline_life_fraction: required_fraction(
            response,
            "propeller_value_baseline_life_fraction",
        )?,
        propeller_manufacturer: required_string(response, "propeller_manufacturer")?,
        propeller_model: required_string(response, "propeller_model")?,
        powerplant_source_url: required_string(response, "powerplant_source_url")?,
        powerplant_source_title: required_string(response, "powerplant_source_title")?,
        powerplant_source_confidence: required_string(response, "powerplant_source_confidence")?,
        annual_inspection_usd: required_min_f64(response, "annual_inspection_usd", 0.0)?,
        other_maintenance_per_hour: required_min_f64(response, "other_maintenance_per_hour", 0.0)?,
        confidence: required_string(response, "confidence")?,
        evidence_count,
        applied_spec_id: None,
    })
}

fn required_string(response: &Value, field_name: &str) -> StoreResult<String> {
    response
        .get(field_name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AircraftStoreError::Model(format!(
                "Gemini aircraft spec response missing {field_name}"
            ))
        })
}

fn required_min_f64(response: &Value, field_name: &str, minimum: f64) -> StoreResult<f64> {
    let value = response
        .get(field_name)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| {
            AircraftStoreError::Model(format!(
                "Gemini aircraft spec response missing numeric {field_name}"
            ))
        })?;
    if value < minimum {
        return Err(AircraftStoreError::Model(format!(
            "Gemini aircraft spec response {field_name} must be at least {minimum}"
        )));
    }
    Ok(value)
}

fn required_min_i64(response: &Value, field_name: &str, minimum: i64) -> StoreResult<i64> {
    let value = response
        .get(field_name)
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_f64().map(|value| value as i64))
        })
        .ok_or_else(|| {
            AircraftStoreError::Model(format!(
                "Gemini aircraft spec response missing integer {field_name}"
            ))
        })?;
    if value < minimum {
        return Err(AircraftStoreError::Model(format!(
            "Gemini aircraft spec response {field_name} must be at least {minimum}"
        )));
    }
    Ok(value)
}

fn required_fraction(response: &Value, field_name: &str) -> StoreResult<f64> {
    let value = required_min_f64(response, field_name, 0.0)?;
    if value > 1.0 {
        return Err(AircraftStoreError::Model(format!(
            "Gemini aircraft spec response {field_name} must be between 0 and 1"
        )));
    }
    Ok(value)
}

fn year_from_effective_from(effective_from: &str) -> i64 {
    year_from_date_prefix(effective_from).unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR)
}

fn year_from_date_prefix(value: &str) -> Option<i64> {
    value.get(0..4).and_then(|year| year.parse::<i64>().ok())
}
