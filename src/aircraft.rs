pub mod catalog;
pub mod curation;
pub mod faa;
pub mod observations;
pub mod reference;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

use self::faa::{audit_listing_admission, require_listing_admission, AircraftAdmissionError};
use crate::db::{AppDb, DatabaseBackend};
use crate::depreciation::{
    avionics_replacement_basis, default_avionics_profile, estimate_aircraft_value_in_year,
    get_aircraft_profile, nominal_dollar_factor, AircraftProfile, AvionicsComponent, DollarBasis,
    PriceEstimate, TimedComponent, DEFAULT_ANNUAL_AIRFRAME_HOURS,
};
use crate::extract::{
    AircraftSpecListingContext, AircraftSpecMetadataContext, GeminiListingExtractor,
};
use crate::html::clean::clean_listing_html_with_limit;
use crate::normalize::normalize_name;
use crate::valuation::dataset::{
    equipment_feature_token, require_snapshot_faa_admission, technical_field_count,
};
use crate::valuation::{
    source_backed_component_observation, SupportGrade, ValuationBreakdown, ValuationModel,
    ValuationQuery,
};

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

fn aircraft_admission_store_error(error: AircraftAdmissionError) -> AircraftStoreError {
    let message = error.to_string();
    match error {
        AircraftAdmissionError::Rejected { .. } => AircraftStoreError::Model(message),
        AircraftAdmissionError::LookupFailed { .. } => AircraftStoreError::Database(message),
        AircraftAdmissionError::ListingNotFound { .. } => AircraftStoreError::NotFound(message),
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
struct AircraftOptionListingRow {
    variant_id: i64,
    listing_id: i64,
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
    configuration_scope: String,
    source_confidence: Option<String>,
    evidence_kind: String,
    is_valuation_eligible: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, FromRow)]
struct AircraftListingPointRow {
    id: i64,
    manufacturer_id: i64,
    model_id: i64,
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
    engine_hours: Option<f64>,
    engine_time_basis: String,
    engine_time_evidence: Option<String>,
    engine_time_confidence: Option<String>,
    propeller_hours: Option<f64>,
    propeller_time_basis: String,
    propeller_time_evidence: Option<String>,
    propeller_time_confidence: Option<String>,
}

#[derive(Debug, FromRow)]
struct ListingEquipmentTokenRow {
    equipment_kind: String,
    manufacturer: String,
    model: String,
    quantity: i64,
}

#[derive(Clone, Debug, FromRow)]
struct AvionicsEstimateRow {
    avionics_model_id: i64,
    manufacturer: String,
    model: String,
    quantity: i64,
    introduced_year: Option<i64>,
    installed_value_contribution_usd: Option<f64>,
    replacement_cost_usd: Option<f64>,
    value_reference_year: Option<i64>,
    valuation_scope: String,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    source_confidence: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct AvionicsSuiteComponentRow {
    suite_model_id: i64,
    component_model_id: i64,
    quantity: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AvionicsConfigurationLink {
    pub avionics_model_id: i64,
    pub quantity: i64,
    pub configuration_action: String,
    pub replaces_avionics_model_id: Option<i64>,
    pub source_confidence: Option<String>,
    pub valuation_scope: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AvionicsSuiteMembership {
    pub suite_model_id: i64,
    pub component_model_id: i64,
    pub quantity: i64,
}

#[derive(Debug, FromRow)]
struct AircraftModelYearPricePointRow {
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
}

#[derive(Debug, FromRow)]
struct ReplacementBasisRow {
    model_year: i64,
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
    average_inflation_rate: f64,
}

#[derive(Debug, FromRow)]
struct PluginSpecEvidenceRow {
    listing_id: i64,
    model_year: i64,
    asking_price_usd: f64,
    airframe_hours: f64,
    engine_hours: Option<f64>,
    propeller_hours: Option<f64>,
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
    engine_hours: Option<f64>,
    propeller_hours: Option<f64>,
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
    pub configuration_scope: String,
    pub source_confidence: Option<String>,
    pub evidence_kind: String,
    pub is_valuation_eligible: bool,
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
    pub engine_hours: Option<f64>,
    pub propeller_hours: Option<f64>,
    pub estimated_value_usd: Option<f64>,
    pub estimated_value_low_usd: Option<f64>,
    pub estimated_value_high_usd: Option<f64>,
    pub estimated_error_fraction: Option<f64>,
    pub valuation_support: Option<SupportGrade>,
    pub valuation_model_kind: Option<String>,
    pub valuation_model_version_id: Option<i64>,
    pub valuation_snapshot_id: Option<i64>,
    pub valuation_calibrated: bool,
    pub valuation_warning: Option<String>,
    pub valuation_breakdown: Option<ValuationBreakdown>,
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
    pub engine_hours: Option<f64>,
    pub propeller_hours: Option<f64>,
    pub estimated_value_usd: Option<f64>,
    pub estimated_value_low_usd: Option<f64>,
    pub estimated_value_high_usd: Option<f64>,
    pub depreciation_usd: Option<f64>,
    pub depreciation_fraction: Option<f64>,
    pub one_year_depreciation_fraction: Option<f64>,
    pub estimated_error_fraction: Option<f64>,
    pub valuation_support: Option<SupportGrade>,
    pub estimate_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftSpecEnrichmentReport {
    pub applied: bool,
    pub value_reference_year: i64,
    pub faa_admitted_evidence_count: usize,
    pub faa_rejected_evidence_count: usize,
    pub faa_rejections: Vec<String>,
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
    pub configuration_scope: String,
    pub evidence_kind: String,
    pub source_confidence: String,
    pub is_valuation_eligible: bool,
    pub valuation_eligibility_notes: Vec<String>,
    pub annual_inspection_usd: f64,
    pub other_maintenance_per_hour: f64,
    pub confidence: String,
    pub evidence_count: usize,
    pub applied_spec_id: Option<i64>,
}

pub async fn aircraft_options(db: &AppDb, user_id: i64) -> StoreResult<Vec<AircraftVariantOption>> {
    let mut rows = query_as_all!(
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
        WHERE l.ingestion_state = 'ready'
          AND (l.is_verified = TRUE OR l.created_by_user_id = ?)
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
    let listing_rows = query_as_all!(
        db,
        AircraftOptionListingRow,
        r#"
        SELECT
          l.aircraft_model_variant_id AS variant_id,
          l.id AS listing_id
        FROM aircraft_sale_listings l
        WHERE l.ingestion_state = 'ready'
          AND (l.is_verified = TRUE OR l.created_by_user_id = ?)
        ORDER BY l.aircraft_model_variant_id, l.id
        "#,
        user_id
    )?;
    let listing_ids = listing_rows
        .iter()
        .map(|row| row.listing_id)
        .collect::<BTreeSet<_>>();
    let admission = audit_listing_admission(db, Some(&listing_ids))
        .await
        .map_err(aircraft_admission_store_error)?;
    let mut admitted_counts = BTreeMap::<i64, i64>::new();
    for listing in listing_rows {
        if admission.is_admitted(listing.listing_id) {
            *admitted_counts.entry(listing.variant_id).or_default() += 1;
        }
    }
    rows.retain_mut(|row| {
        row.listing_count = admitted_counts.get(&row.variant_id).copied().unwrap_or(0);
        row.listing_count > 0
    });
    Ok(rows.into_iter().map(option_from_row).collect())
}

pub async fn aircraft_variant_detail(
    db: &AppDb,
    user_id: i64,
    variant_id: i64,
    curve_annual_airframe_hours: Option<f64>,
) -> StoreResult<AircraftVariantDetail> {
    aircraft_variant_detail_with_model(db, user_id, variant_id, curve_annual_airframe_hours, None)
        .await
}

pub async fn aircraft_variant_detail_with_model(
    db: &AppDb,
    user_id: i64,
    variant_id: i64,
    curve_annual_airframe_hours: Option<f64>,
    valuation_model: Option<&Arc<dyn ValuationModel>>,
) -> StoreResult<AircraftVariantDetail> {
    require_valuation_model_faa_admission(db, valuation_model.map(Arc::as_ref)).await?;
    let mut option = aircraft_option_for_variant(db, user_id, variant_id).await?;
    let spec_row = aircraft_spec_for_variant(db, option.model_id, variant_id).await?;
    let spec = spec_row.as_ref().map(spec_detail_from_row);
    let spec_ref = spec_row.as_ref();
    let rows = listing_points_for_variant(db, user_id, variant_id).await?;
    let mut listings = Vec::with_capacity(rows.len());
    for row in rows {
        match require_listing_admission(db, row.id).await {
            Ok(_) => {}
            Err(AircraftAdmissionError::Rejected { .. }) => continue,
            Err(error) => return Err(aircraft_admission_store_error(error)),
        }
        listings.push(
            listing_value_point(
                db,
                &row,
                spec_ref,
                curve_annual_airframe_hours,
                valuation_model.map(Arc::as_ref),
                false,
            )
            .await?,
        );
    }
    option.listing_count = listings.len() as i64;
    let message = match (valuation_model, spec_ref) {
        (Some(_), _) => None,
        (None, _) => Some(
            "Listing-only valuation unavailable: no approved model artifact or eligible comparable snapshot is available."
                .to_string(),
        ),
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
    aircraft_listing_value_with_model(db, user_id, listing_id, None).await
}

pub async fn aircraft_listing_value_with_model(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
    valuation_model: Option<&Arc<dyn ValuationModel>>,
) -> StoreResult<AircraftListingValuePoint> {
    require_listing_admission(db, listing_id)
        .await
        .map_err(aircraft_admission_store_error)?;
    require_valuation_model_faa_admission(db, valuation_model.map(Arc::as_ref)).await?;
    let row = listing_point_for_listing(db, user_id, listing_id)
        .await?
        .ok_or_else(|| AircraftStoreError::NotFound("listing not found".to_string()))?;
    listing_value_point(
        db,
        &row,
        None,
        None,
        valuation_model.map(Arc::as_ref),
        false,
    )
    .await
}

async fn require_valuation_model_faa_admission(
    db: &AppDb,
    valuation_model: Option<&dyn ValuationModel>,
) -> StoreResult<()> {
    let Some(valuation_model) = valuation_model else {
        return Ok(());
    };
    require_snapshot_faa_admission(db, valuation_model.snapshot_id())
        .await
        .map_err(|error| AircraftStoreError::Model(error.to_string()))?;
    Ok(())
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
    let mut faa_admitted_evidence_count = 0usize;
    let mut faa_rejections = Vec::new();

    for variant in variants {
        let candidates = plugin_spec_evidence_for_variant(db, variant.variant_id).await?;
        let mut evidence = Vec::with_capacity(SPEC_EVIDENCE_LIMIT as usize);
        for row in candidates {
            match require_listing_admission(db, row.listing_id).await {
                Ok(_) => {
                    faa_admitted_evidence_count += 1;
                    evidence.push(row);
                    if evidence.len() >= SPEC_EVIDENCE_LIMIT as usize {
                        break;
                    }
                }
                Err(error) => faa_rejections.push(error.to_string()),
            }
        }
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
        let listing_source_urls = evidence
            .iter()
            .map(|row| row.source_url.as_str())
            .collect::<Vec<_>>();
        let mut item = spec_enrichment_item_from_response(
            &variant,
            &response,
            listing_contexts.len(),
            &listing_source_urls,
        )?;
        if apply && item.is_valuation_eligible {
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
        faa_admitted_evidence_count,
        faa_rejected_evidence_count: faa_rejections.len(),
        faa_rejections,
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
    require_listing_admission(db, listing_id)
        .await
        .map_err(aircraft_admission_store_error)?;
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
                optional_hours_text(row.engine_hours),
                optional_hours_text(row.propeller_hours),
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
        &[source_url],
    )?;
    if !item.is_valuation_eligible {
        return Err(AircraftStoreError::Model(format!(
            "aircraft factory spec evidence rejected: {}",
            item.valuation_eligibility_notes.join("; ")
        )));
    }
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
          AND l.ingestion_state = 'ready'
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
          spec.configuration_scope,
          spec.source_confidence,
          spec.evidence_kind,
          spec.is_valuation_eligible,
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
          AND spec.configuration_scope = 'factory_default'
          AND spec.is_valuation_eligible = TRUE
          AND spec.source_confidence = 'high'
          AND spec.evidence_kind = 'authoritative_reference'
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
          listing.id,
          manufacturer.id AS manufacturer_id,
          model.id AS model_id,
          listing.aircraft_model_variant_id,
          listing.is_verified,
          listing.source_url,
          listing.model_year,
          listing.asking_price_usd,
          listing.currency,
          listing.added_at,
          listing.status,
          listing.registration_number,
          listing.serial_number,
          listing.airframe_hours,
          listing.engine_hours,
          listing.engine_time_basis,
          listing.engine_time_evidence,
          listing.engine_time_confidence,
          listing.propeller_hours,
          listing.propeller_time_basis,
          listing.propeller_time_evidence,
          listing.propeller_time_confidence
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = model.aircraft_manufacturer_id
        WHERE listing.aircraft_model_variant_id = ?
          AND listing.ingestion_state = 'ready'
          AND (listing.is_verified = TRUE OR listing.created_by_user_id = ?)
        ORDER BY listing.model_year, listing.airframe_hours, listing.id
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
          listing.id,
          manufacturer.id AS manufacturer_id,
          model.id AS model_id,
          listing.aircraft_model_variant_id,
          listing.is_verified,
          listing.source_url,
          listing.model_year,
          listing.asking_price_usd,
          listing.currency,
          listing.added_at,
          listing.status,
          listing.registration_number,
          listing.serial_number,
          listing.airframe_hours,
          listing.engine_hours,
          listing.engine_time_basis,
          listing.engine_time_evidence,
          listing.engine_time_confidence,
          listing.propeller_hours,
          listing.propeller_time_basis,
          listing.propeller_time_evidence,
          listing.propeller_time_confidence
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = model.aircraft_manufacturer_id
        WHERE listing.id = ?
          AND listing.ingestion_state = 'ready'
          AND (listing.is_verified = TRUE OR listing.created_by_user_id = ?)
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
    valuation_model: Option<&dyn ValuationModel>,
    allow_uncalibrated_compatibility: bool,
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
        estimated_value_low_usd: None,
        estimated_value_high_usd: None,
        estimated_error_fraction: None,
        valuation_support: None,
        valuation_model_kind: None,
        valuation_model_version_id: None,
        valuation_snapshot_id: None,
        valuation_calibrated: false,
        valuation_warning: None,
        valuation_breakdown: None,
        estimate_error: None,
        breakdown: None,
        value_curve: Vec::new(),
    };

    if let Some(model) = valuation_model {
        let valuation_year = match model.market_year() {
            Ok(year) => year,
            Err(error) => {
                point.estimate_error = Some(error.to_string());
                return Ok(point);
            }
        };
        let equipment_tokens = listing_equipment_tokens(db, row.id).await?;
        let technical_field_count = technical_field_count(
            row.engine_hours.is_some(),
            row.propeller_hours.is_some(),
            row.registration_number.is_some(),
            row.serial_number.is_some(),
            !equipment_tokens.is_empty(),
        );
        let query = ValuationQuery {
            category_key: None,
            manufacturer_id: Some(row.manufacturer_id),
            model_id: Some(row.model_id),
            variant_id: Some(row.aircraft_model_variant_id),
            model_year: row.model_year,
            valuation_year,
            airframe_hours: Some(row.airframe_hours),
            engine_times: vec![source_backed_component_observation(
                row.engine_hours,
                &row.engine_time_basis,
                row.engine_time_evidence.as_deref(),
                row.engine_time_confidence.as_deref(),
                1,
            )],
            propeller_times: vec![source_backed_component_observation(
                row.propeller_hours,
                &row.propeller_time_basis,
                row.propeller_time_evidence.as_deref(),
                row.propeller_time_confidence.as_deref(),
                1,
            )],
            equipment_tokens,
            technical_field_count,
        };
        match model.estimate(&query) {
            Ok(estimate) => {
                point.estimated_value_usd = Some(estimate.estimated_value_usd);
                point.estimated_value_low_usd = Some(estimate.low_value_usd);
                point.estimated_value_high_usd = Some(estimate.high_value_usd);
                point.estimated_error_fraction = Some(estimate.estimated_error_fraction);
                point.valuation_support = Some(estimate.support);
                point.valuation_calibrated =
                    matches!(estimate.model_kind.as_str(), "structural" | "dnn");
                point.valuation_model_kind = Some(estimate.model_kind);
                point.valuation_model_version_id = Some(estimate.model_version_id);
                point.valuation_snapshot_id = Some(estimate.snapshot_id);
                if !point.valuation_calibrated {
                    point.valuation_warning = Some(
                        "No approved model artifact is active; estimate uses an adjusted-comparable snapshot fallback."
                            .to_string(),
                    );
                }
                point.valuation_breakdown = Some(estimate.breakdown);
                point.value_curve = estimate
                    .depreciation
                    .into_iter()
                    .map(|curve| AircraftValueCurvePoint {
                        valuation_year: curve.valuation_year,
                        age_years: curve.age_years,
                        airframe_hours: curve.airframe_hours.unwrap_or(row.airframe_hours),
                        engine_hours: row.engine_hours,
                        propeller_hours: row.propeller_hours,
                        estimated_value_usd: Some(curve.estimated_value_usd),
                        estimated_value_low_usd: Some(curve.low_value_usd),
                        estimated_value_high_usd: Some(curve.high_value_usd),
                        depreciation_usd: Some(curve.depreciation_usd),
                        depreciation_fraction: Some(curve.depreciation_fraction),
                        one_year_depreciation_fraction: Some(curve.one_year_depreciation_fraction),
                        estimated_error_fraction: Some(curve.estimated_error_fraction),
                        valuation_support: Some(curve.support),
                        estimate_error: None,
                    })
                    .collect();
            }
            Err(error) => point.estimate_error = Some(error.to_string()),
        }
        return Ok(point);
    }

    if !allow_uncalibrated_compatibility {
        point.valuation_warning = Some(
            "No approved model artifact or eligible comparable snapshot is available.".to_string(),
        );
        point.estimate_error = Some(
            "Listing-only valuation unavailable: no approved model artifact or eligible comparable snapshot is available."
                .to_string(),
        );
        return Ok(point);
    }

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
    let raw_default_avionics_rows =
        model_year_default_avionics_estimates(db, row.aircraft_model_variant_id, row.model_year)
            .await?;
    let suite_memberships = avionics_suite_memberships(db).await?;
    let default_avionics_rows =
        effective_avionics_rows(&raw_default_avionics_rows, &[], &suite_memberships);
    let purchase_price_new_usd = airframe_basis_excluding_default_avionics(
        price_point.purchase_price_new_usd,
        &default_avionics_rows,
        purchase_price_reference_year,
        spec_effective_year,
        spec.average_inflation_rate,
    );
    let age_years = (valuation_year - row.model_year).max(0) as f64;
    let listing_avionics_rows = listing_avionics_estimates(db, row.id).await?;
    let avionics_rows = effective_avionics_rows(
        &raw_default_avionics_rows,
        &listing_avionics_rows,
        &suite_memberships,
    );
    let avionics = avionics_components_from_rows(
        &avionics_rows,
        valuation_year,
        spec_effective_year,
        spec.average_inflation_rate,
    );
    match estimate_listing_value(
        spec,
        profile.clone(),
        purchase_price_new_usd,
        purchase_price_reference_year,
        valuation_year,
        age_years,
        row.airframe_hours,
        known_component_time(row.engine_hours, &row.engine_time_basis),
        known_component_time(row.propeller_hours, &row.propeller_time_basis),
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
    engine_hours: Option<f64>,
    propeller_hours: Option<f64>,
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
                Some(engine_hours),
                Some(propeller_hours),
                &avionics,
                replacement_floor_basis_usd,
            );
            match result {
                Ok(estimate) => AircraftValueCurvePoint {
                    valuation_year,
                    age_years,
                    airframe_hours,
                    engine_hours: Some(engine_hours),
                    propeller_hours: Some(propeller_hours),
                    estimated_value_usd: Some(estimate.estimated_value_usd),
                    estimated_value_low_usd: None,
                    estimated_value_high_usd: None,
                    depreciation_usd: None,
                    depreciation_fraction: None,
                    one_year_depreciation_fraction: None,
                    estimated_error_fraction: None,
                    valuation_support: None,
                    estimate_error: None,
                },
                Err(error) => AircraftValueCurvePoint {
                    valuation_year,
                    age_years,
                    airframe_hours,
                    engine_hours: Some(engine_hours),
                    propeller_hours: Some(propeller_hours),
                    estimated_value_usd: None,
                    estimated_value_low_usd: None,
                    estimated_value_high_usd: None,
                    depreciation_usd: None,
                    depreciation_fraction: None,
                    one_year_depreciation_fraction: None,
                    estimated_error_fraction: None,
                    valuation_support: None,
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
          model.id AS avionics_model_id,
          mfr.name AS manufacturer,
          model.name AS model,
          link.quantity,
          model.introduced_year,
          CASE
            WHEN model.value_basis = 'installed_contribution'
              AND model.estimated_unit_value_usd >= 0
              AND model.replacement_cost_usd >= model.estimated_unit_value_usd
              AND model.value_reference_year IS NOT NULL
              AND model.value_source IS NOT NULL
              AND TRIM(model.value_source) <> ''
            THEN model.estimated_unit_value_usd
          END AS installed_value_contribution_usd,
          model.replacement_cost_usd,
          model.value_reference_year,
          model.valuation_scope,
          link.configuration_action,
          link.replaces_avionics_model_id,
          link.source_confidence
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model
          ON model.id = link.avionics_model_id
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        WHERE link.aircraft_sale_listing_id = ?
          AND link.source = 'listing'
          AND model.catalog_status = 'approved'
          AND (
            link.replaces_avionics_model_id IS NULL
            OR EXISTS (
              SELECT 1
              FROM avionics_models replaced_model
              WHERE replaced_model.id = link.replaces_avionics_model_id
                AND replaced_model.catalog_status = 'approved'
            )
          )
        ORDER BY link.id
        "#,
        listing_id
    )?)
}

async fn listing_equipment_tokens(db: &AppDb, listing_id: i64) -> StoreResult<Vec<String>> {
    let rows = query_as_all!(
        db,
        ListingEquipmentTokenRow,
        r#"
        SELECT
          'avionics' AS equipment_kind,
          manufacturer.name AS manufacturer,
          model.name AS model,
          link.quantity
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model ON model.id = link.avionics_model_id
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = model.avionics_manufacturer_id
        WHERE link.aircraft_sale_listing_id = ?
          AND link.source = 'listing'
          AND link.configuration_action IN ('installed', 'replaces')
          AND link.source_confidence = 'high'
          AND model.catalog_status = 'approved'
          AND (
            link.replaces_avionics_model_id IS NULL
            OR EXISTS (
              SELECT 1
              FROM avionics_models replaced_model
              WHERE replaced_model.id = link.replaces_avionics_model_id
                AND replaced_model.catalog_status = 'approved'
            )
          )
        UNION ALL
        SELECT
          'engine' AS equipment_kind,
          manufacturer.name AS manufacturer,
          model.name AS model,
          1 AS quantity
        FROM aircraft_sale_listings listing
        JOIN engine_models model ON model.id = listing.installed_engine_model_id
        JOIN engine_manufacturers manufacturer
          ON manufacturer.id = model.engine_manufacturer_id
        WHERE listing.id = ?
          AND listing.installed_engine_confidence = 'high'
        UNION ALL
        SELECT
          'propeller' AS equipment_kind,
          manufacturer.name AS manufacturer,
          model.name AS model,
          1 AS quantity
        FROM aircraft_sale_listings listing
        JOIN propeller_models model ON model.id = listing.installed_propeller_model_id
        JOIN propeller_manufacturers manufacturer
          ON manufacturer.id = model.propeller_manufacturer_id
        WHERE listing.id = ?
          AND listing.installed_propeller_confidence = 'high'
        UNION ALL
        SELECT
          'fact' AS equipment_kind,
          fact.fact_kind AS manufacturer,
          fact.fact_value AS model,
          1 AS quantity
        FROM aircraft_sale_listing_facts fact
        WHERE fact.aircraft_sale_listing_id = ?
          AND fact.source_confidence = 'high'
        ORDER BY equipment_kind, manufacturer, model
        "#,
        listing_id,
        listing_id,
        listing_id,
        listing_id
    )?;
    let mut tokens = Vec::new();
    for row in rows {
        let token = equipment_feature_token(&row.equipment_kind, &row.manufacturer, &row.model);
        for _ in 0..row.quantity.max(1) {
            tokens.push(token.clone());
        }
    }
    Ok(tokens)
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
          model.id AS avionics_model_id,
          mfr.name AS manufacturer,
          model.name AS model,
          default_avionics.quantity,
          model.introduced_year,
          CASE
            WHEN model.value_basis = 'installed_contribution'
              AND model.estimated_unit_value_usd >= 0
              AND model.replacement_cost_usd >= model.estimated_unit_value_usd
              AND model.value_reference_year IS NOT NULL
              AND model.value_source IS NOT NULL
              AND TRIM(model.value_source) <> ''
            THEN model.estimated_unit_value_usd
          END AS installed_value_contribution_usd,
          model.replacement_cost_usd,
          model.value_reference_year,
          model.valuation_scope,
          'installed' AS configuration_action,
          NULL AS replaces_avionics_model_id,
          default_avionics.source_confidence
        FROM aircraft_model_variant_default_avionics default_avionics
        JOIN avionics_models model
          ON model.id = default_avionics.avionics_model_id
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        WHERE default_avionics.aircraft_model_variant_id = ?
          AND default_avionics.model_year = ?
          AND model.catalog_status = 'approved'
          AND default_avionics.source_confidence = 'high'
          AND default_avionics.quantity > 0
          AND TRIM(default_avionics.source_url) <> ''
          AND LOWER(default_avionics.source_url) NOT LIKE '%/listing/%'
          AND LOWER(default_avionics.source_url) NOT LIKE '%/listings/%'
          AND LOWER(default_avionics.source_url) NOT LIKE '%/aircraft-for-sale/%'
          AND LOWER(default_avionics.source_url) NOT LIKE '%/classifieds/%'
        ORDER BY default_avionics.id
        "#,
        aircraft_model_variant_id,
        model_year
    )?)
}

async fn avionics_suite_memberships(db: &AppDb) -> StoreResult<Vec<AvionicsSuiteMembership>> {
    let rows = query_as_all!(
        db,
        AvionicsSuiteComponentRow,
        r#"
        SELECT membership.suite_model_id, membership.component_model_id, membership.quantity
        FROM avionics_suite_components membership
        JOIN avionics_models suite
          ON suite.id = membership.suite_model_id
         AND suite.catalog_status = 'approved'
        JOIN avionics_models component
          ON component.id = membership.component_model_id
         AND component.catalog_status = 'approved'
        ORDER BY membership.suite_model_id, membership.component_model_id
        "#
    )?;
    Ok(rows
        .into_iter()
        .map(|row| AvionicsSuiteMembership {
            suite_model_id: row.suite_model_id,
            component_model_id: row.component_model_id,
            quantity: row.quantity,
        })
        .collect())
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
          AND source_confidence = 'high'
          AND evidence_kind = 'direct_model_year'
          AND is_valuation_eligible = TRUE
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
          price_point.model_year,
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
          AND price_point.source_confidence = 'high'
          AND price_point.evidence_kind = 'direct_model_year'
          AND price_point.is_valuation_eligible = TRUE
          AND spec.configuration_scope = 'factory_default'
          AND spec.is_valuation_eligible = TRUE
          AND spec.source_confidence = 'high'
          AND spec.evidence_kind = 'authoritative_reference'
          AND spec.id = (
            SELECT latest.id
            FROM aircraft_model_spec_versions latest
            WHERE latest.aircraft_model_id = variant.aircraft_model_id
              AND latest.aircraft_model_variant_id = variant.id
              AND latest.configuration_scope = 'factory_default'
              AND latest.is_valuation_eligible = TRUE
              AND latest.source_confidence = 'high'
              AND latest.evidence_kind = 'authoritative_reference'
            ORDER BY latest.effective_from DESC, latest.id DESC
            LIMIT 1
          )
        "#,
        aircraft_model_id
    )?)
}

fn replacement_basis_for_year(rows: &[ReplacementBasisRow], valuation_year: i64) -> Option<f64> {
    rows.iter()
        .filter(|row| row.model_year <= valuation_year)
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

fn avionics_configuration_link(row: &AvionicsEstimateRow) -> AvionicsConfigurationLink {
    AvionicsConfigurationLink {
        avionics_model_id: row.avionics_model_id,
        quantity: row.quantity,
        configuration_action: row.configuration_action.clone(),
        replaces_avionics_model_id: row.replaces_avionics_model_id,
        source_confidence: row.source_confidence.clone(),
        valuation_scope: row.valuation_scope.clone(),
    }
}

fn is_high_confidence(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("high"))
}

/// Resolve a factory configuration plus explicit listing deltas into one set
/// of installed avionics quantities.
///
/// Only high-confidence links can add, replace, or remove equipment. A suite
/// membership consumes the quantities bundled by the suite, preventing the
/// integrated suite and the same constituent hardware from being valued
/// additively. Any quantity above the declared bundled count remains an
/// independently installed unit.
pub(crate) fn resolve_avionics_configuration(
    factory_defaults: &[AvionicsConfigurationLink],
    listing_deltas: &[AvionicsConfigurationLink],
    suite_memberships: &[AvionicsSuiteMembership],
) -> BTreeMap<i64, i64> {
    let mut quantities = BTreeMap::<i64, i64>::new();
    let integrated_suite_ids = factory_defaults
        .iter()
        .chain(listing_deltas)
        .filter(|link| link.valuation_scope == "integrated_suite")
        .map(|link| link.avionics_model_id)
        .collect::<std::collections::BTreeSet<_>>();

    for link in factory_defaults
        .iter()
        .filter(|link| is_high_confidence(link.source_confidence.as_deref()))
    {
        quantities
            .entry(link.avionics_model_id)
            .and_modify(|quantity| *quantity = (*quantity).max(link.quantity.max(1)))
            .or_insert_with(|| link.quantity.max(1));
    }

    for link in listing_deltas
        .iter()
        .filter(|link| is_high_confidence(link.source_confidence.as_deref()))
    {
        match link.configuration_action.as_str() {
            "removes" => {
                if let Some(replaced_id) = link.replaces_avionics_model_id {
                    quantities.remove(&replaced_id);
                }
            }
            "replaces" => {
                let Some(replaced_id) = link.replaces_avionics_model_id else {
                    continue;
                };
                quantities.remove(&replaced_id);
                quantities
                    .entry(link.avionics_model_id)
                    .and_modify(|quantity| *quantity = (*quantity).max(link.quantity.max(1)))
                    .or_insert_with(|| link.quantity.max(1));
            }
            "installed" => {
                quantities
                    .entry(link.avionics_model_id)
                    .and_modify(|quantity| *quantity = (*quantity).max(link.quantity.max(1)))
                    .or_insert_with(|| link.quantity.max(1));
            }
            _ => {}
        }
    }

    for membership in suite_memberships {
        if !integrated_suite_ids.contains(&membership.suite_model_id) {
            continue;
        }
        let Some(suite_quantity) = quantities.get(&membership.suite_model_id).copied() else {
            continue;
        };
        let bundled_quantity = suite_quantity.saturating_mul(membership.quantity.max(1));
        if let Some(component_quantity) = quantities.get_mut(&membership.component_model_id) {
            *component_quantity = component_quantity.saturating_sub(bundled_quantity);
        }
    }
    quantities.retain(|_, quantity| *quantity > 0);
    quantities
}

fn effective_avionics_rows(
    factory_defaults: &[AvionicsEstimateRow],
    listing_deltas: &[AvionicsEstimateRow],
    suite_memberships: &[AvionicsSuiteMembership],
) -> Vec<AvionicsEstimateRow> {
    let quantities = resolve_avionics_configuration(
        &factory_defaults
            .iter()
            .map(avionics_configuration_link)
            .collect::<Vec<_>>(),
        &listing_deltas
            .iter()
            .map(avionics_configuration_link)
            .collect::<Vec<_>>(),
        suite_memberships,
    );
    let rows_by_id = factory_defaults
        .iter()
        .chain(listing_deltas)
        .map(|row| (row.avionics_model_id, row))
        .collect::<BTreeMap<_, _>>();

    quantities
        .into_iter()
        .filter_map(|(model_id, quantity)| {
            rows_by_id.get(&model_id).map(|row| {
                let mut row = (*row).clone();
                row.quantity = quantity;
                row
            })
        })
        .collect()
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
            let installed_value_contribution_usd = row.installed_value_contribution_usd?;
            let replacement_cost_usd = row.replacement_cost_usd?;
            (installed_value_contribution_usd >= 0.0 && replacement_cost_usd >= 0.0).then(|| {
                AvionicsComponent {
                    name: format!("{} {}", row.manufacturer, row.model),
                    introduced_year,
                    valuation_year,
                    value_reference_year: row
                        .value_reference_year
                        .unwrap_or(fallback_value_reference_year),
                    average_inflation_rate,
                    installed_value_contribution_usd,
                    replacement_cost_usd,
                    quantity: row.quantity.max(1),
                    profile: profile.clone(),
                }
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
            AND spec.configuration_scope = 'factory_default'
            AND spec.is_valuation_eligible = TRUE
            AND spec.source_confidence = 'high'
            AND spec.evidence_kind = 'authoritative_reference'
        )
          AND l.ingestion_state = 'ready'
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
        WHERE l.ingestion_state = 'ready'
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
          listing.id AS listing_id,
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
          AND listing.ingestion_state = 'ready'
        ORDER BY listing.added_at DESC, submission.submitted_at DESC
        "#,
        variant_id
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
          AND configuration_scope = 'factory_default'
          AND is_valuation_eligible = TRUE
          AND source_confidence = 'high'
          AND evidence_kind = 'authoritative_reference'
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
          AND listing.ingestion_state <> 'quarantined'
        "#,
        listing_id
    )?;
    row.ok_or_else(|| AircraftStoreError::NotFound("listing not found".to_string()))
}

async fn insert_aircraft_spec(
    db: &AppDb,
    item: &AircraftSpecEnrichmentItem,
    value_reference_year: i64,
    _listing_source_url: Option<&str>,
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
        &item.evidence_kind,
        item.is_valuation_eligible,
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
        &item.evidence_kind,
        item.is_valuation_eligible,
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
          source_url,
          configuration_scope,
          source_confidence,
          evidence_kind,
          is_valuation_eligible
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        item.powerplant_source_url.as_str(),
        item.configuration_scope.as_str(),
        item.source_confidence.as_str(),
        item.evidence_kind.as_str(),
        item.is_valuation_eligible,
    )?)
}

async fn upsert_aircraft_spec(
    db: &AppDb,
    item: &AircraftSpecEnrichmentItem,
    value_reference_year: i64,
    listing_source_url: Option<&str>,
) -> StoreResult<i64> {
    if let Some(spec_id) = latest_aircraft_spec_id(db, item.model_id, item.variant_id).await? {
        update_aircraft_spec(db, spec_id, item, value_reference_year, listing_source_url).await?;
        Ok(spec_id)
    } else {
        insert_aircraft_spec(db, item, value_reference_year, listing_source_url).await
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
          AND configuration_scope = 'factory_default'
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
    _listing_source_url: Option<&str>,
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
        &item.evidence_kind,
        item.is_valuation_eligible,
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
        &item.evidence_kind,
        item.is_valuation_eligible,
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
          configuration_scope = ?,
          source_confidence = ?,
          evidence_kind = ?,
          is_valuation_eligible = ?,
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
        item.powerplant_source_url.as_str(),
        item.configuration_scope.as_str(),
        item.source_confidence.as_str(),
        item.evidence_kind.as_str(),
        item.is_valuation_eligible,
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
    evidence_kind: &str,
    is_valuation_eligible: bool,
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
          source_confidence,
          evidence_kind,
          is_valuation_eligible
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (engine_manufacturer_id, normalized_name) DO UPDATE SET
          name = excluded.name,
          tbo_hours = COALESCE(excluded.tbo_hours, engine_models.tbo_hours),
          overhaul_cost_usd = COALESCE(excluded.overhaul_cost_usd, engine_models.overhaul_cost_usd),
          value_reference_year = COALESCE(excluded.value_reference_year, engine_models.value_reference_year),
          source_url = COALESCE(excluded.source_url, engine_models.source_url),
          source_title = COALESCE(excluded.source_title, engine_models.source_title),
          source_confidence = COALESCE(excluded.source_confidence, engine_models.source_confidence),
          evidence_kind = excluded.evidence_kind,
          is_valuation_eligible = excluded.is_valuation_eligible,
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
        evidence_kind,
        is_valuation_eligible,
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
    evidence_kind: &str,
    is_valuation_eligible: bool,
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
          source_confidence,
          evidence_kind,
          is_valuation_eligible
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (propeller_manufacturer_id, normalized_name) DO UPDATE SET
          name = excluded.name,
          tbo_hours = COALESCE(excluded.tbo_hours, propeller_models.tbo_hours),
          overhaul_cost_usd = COALESCE(excluded.overhaul_cost_usd, propeller_models.overhaul_cost_usd),
          value_reference_year = COALESCE(excluded.value_reference_year, propeller_models.value_reference_year),
          source_url = COALESCE(excluded.source_url, propeller_models.source_url),
          source_title = COALESCE(excluded.source_title, propeller_models.source_title),
          source_confidence = COALESCE(excluded.source_confidence, propeller_models.source_confidence),
          evidence_kind = excluded.evidence_kind,
          is_valuation_eligible = excluded.is_valuation_eligible,
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
        evidence_kind,
        is_valuation_eligible,
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
        configuration_scope: row.configuration_scope.clone(),
        source_confidence: row.source_confidence.clone(),
        evidence_kind: row.evidence_kind.clone(),
        is_valuation_eligible: row.is_valuation_eligible,
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

fn timed_component(
    name: &str,
    hours_since_overhaul: Option<f64>,
    count: i64,
    tbo_hours: Option<f64>,
    overhaul_cost_usd: Option<f64>,
    value_reference_year: Option<i64>,
    baseline_life_fraction: f64,
    valuation_year: i64,
    average_inflation_rate: f64,
) -> Option<TimedComponent> {
    let hours_since_overhaul = hours_since_overhaul?;
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

fn known_component_time(hours: Option<f64>, basis: &str) -> Option<f64> {
    matches!(basis, "SNEW" | "SMOH" | "SFOH" | "SPOH")
        .then_some(hours)
        .flatten()
        .filter(|hours| hours.is_finite() && *hours >= 0.0)
}

fn optional_hours_text(value: Option<f64>) -> String {
    value
        .map(|hours| hours.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn spec_enrichment_item_from_response(
    variant: &AircraftVariantOption,
    response: &Value,
    evidence_count: usize,
    listing_source_urls: &[&str],
) -> StoreResult<AircraftSpecEnrichmentItem> {
    let depreciation_profile = required_string(response, "depreciation_profile")?;
    let powerplant_source_url = required_string(response, "powerplant_source_url")?;
    let powerplant_source_confidence =
        required_confidence(response, "powerplant_source_confidence")?;
    let configuration_scope = match required_string(response, "configuration_scope")?.as_str() {
        "factory" => "factory_default".to_string(),
        "listing_installed" => "listing_specific".to_string(),
        value => {
            return Err(AircraftStoreError::Model(format!(
                "Gemini aircraft spec response configuration_scope is invalid: {value}"
            )))
        }
    };
    let evidence_kind = required_string(response, "evidence_kind")?.to_ascii_lowercase();
    if !matches!(
        evidence_kind.as_str(),
        "authoritative_reference" | "listing_only"
    ) {
        return Err(AircraftStoreError::Model(format!(
            "Gemini aircraft spec response evidence_kind is invalid: {evidence_kind}"
        )));
    }
    let source_confidence = required_confidence(response, "source_confidence")?;
    let model_marked_eligible = response
        .get("is_valuation_eligible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let overall_confidence = required_confidence(response, "confidence")?;
    let mut valuation_eligibility_notes = Vec::new();
    if configuration_scope != "factory_default" {
        valuation_eligibility_notes.push("configuration is listing-specific".to_string());
    }
    if evidence_kind != "authoritative_reference" {
        valuation_eligibility_notes
            .push("factory configuration lacks authoritative evidence".to_string());
    }
    if source_confidence != "high" || powerplant_source_confidence != "high" {
        valuation_eligibility_notes.push("factory source confidence is not high".to_string());
    }
    if overall_confidence != "high" {
        valuation_eligibility_notes.push("overall spec confidence is not high".to_string());
    }
    if !model_marked_eligible {
        valuation_eligibility_notes
            .push("grounded response marked the spec ineligible".to_string());
    }
    if !(powerplant_source_url.starts_with("https://")
        || powerplant_source_url.starts_with("http://"))
    {
        valuation_eligibility_notes.push("powerplant source URL is not http(s)".to_string());
    }
    if looks_like_sale_listing_url(&powerplant_source_url)
        || listing_source_urls
            .iter()
            .filter(|url| !url.is_empty())
            .any(|url| urls_match(url, &powerplant_source_url))
    {
        valuation_eligibility_notes
            .push("powerplant source is one of the ordinary sale listings".to_string());
    }
    let engine_manufacturer = required_string(response, "engine_manufacturer")?;
    let propeller_manufacturer = required_string(response, "propeller_manufacturer")?;
    if normalize_name(&engine_manufacturer) == normalize_name(&variant.manufacturer)
        || normalize_name(&propeller_manufacturer) == normalize_name(&variant.manufacturer)
    {
        valuation_eligibility_notes
            .push("aircraft manufacturer was returned as a component manufacturer".to_string());
    }
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
        engine_manufacturer,
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
        propeller_manufacturer,
        propeller_model: required_string(response, "propeller_model")?,
        powerplant_source_url,
        powerplant_source_title: required_string(response, "powerplant_source_title")?,
        powerplant_source_confidence,
        configuration_scope,
        evidence_kind,
        source_confidence,
        is_valuation_eligible: valuation_eligibility_notes.is_empty(),
        valuation_eligibility_notes,
        annual_inspection_usd: required_min_f64(response, "annual_inspection_usd", 0.0)?,
        other_maintenance_per_hour: required_min_f64(response, "other_maintenance_per_hour", 0.0)?,
        confidence: overall_confidence,
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

fn required_confidence(response: &Value, field_name: &str) -> StoreResult<String> {
    let confidence = required_string(response, field_name)?.to_ascii_lowercase();
    if !matches!(confidence.as_str(), "high" | "medium" | "low") {
        return Err(AircraftStoreError::Model(format!(
            "Gemini aircraft spec response {field_name} must be high, medium, or low"
        )));
    }
    Ok(confidence)
}

fn urls_match(left: &str, right: &str) -> bool {
    let Ok(left) = url::Url::parse(left) else {
        return left.trim_end_matches('/') == right.trim_end_matches('/');
    };
    let Ok(right) = url::Url::parse(right) else {
        return false;
    };
    left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
        && left.path().trim_end_matches('/') == right.path().trim_end_matches('/')
}

fn looks_like_sale_listing_url(value: &str) -> bool {
    let path = url::Url::parse(value)
        .ok()
        .map(|url| url.path().to_ascii_lowercase())
        .unwrap_or_else(|| value.to_ascii_lowercase());
    path.contains("/listing/")
        || path.contains("/listings/")
        || path.contains("/aircraft-for-sale/")
        || path.contains("/classifieds/")
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

#[cfg(test)]
mod tests {
    use super::{
        aircraft_listing_value_with_model, aircraft_options, avionics_suite_memberships,
        enrich_aircraft_spec_for_listing_if_missing, listing_avionics_estimates,
        listing_value_point, model_year_default_avionics_estimates,
        require_valuation_model_faa_admission, resolve_avionics_configuration,
        spec_enrichment_item_from_response, AircraftListingPointRow, AircraftVariantOption,
        AvionicsConfigurationLink, AvionicsSuiteMembership,
    };
    use crate::db::{AppDb, DatabaseBackend};
    use crate::extract::GeminiListingExtractor;
    use crate::valuation::{
        ComparableConfig, ComparableModel, TrainingListing, ValuationError, ValuationEstimate,
        ValuationModel, ValuationQuery,
    };
    use serde_json::{json, Value};

    fn link(
        model_id: i64,
        quantity: i64,
        action: &str,
        replaces: Option<i64>,
        confidence: &str,
    ) -> AvionicsConfigurationLink {
        AvionicsConfigurationLink {
            avionics_model_id: model_id,
            quantity,
            configuration_action: action.to_string(),
            replaces_avionics_model_id: replaces,
            source_confidence: Some(confidence.to_string()),
            valuation_scope: "unit".to_string(),
        }
    }

    struct SnapshotOnlyModel {
        snapshot_id: i64,
    }

    impl ValuationModel for SnapshotOnlyModel {
        fn model_version_id(&self) -> i64 {
            1
        }

        fn model_kind(&self) -> &'static str {
            "test"
        }

        fn snapshot_id(&self) -> i64 {
            self.snapshot_id
        }

        fn market_year(&self) -> Result<i64, ValuationError> {
            Ok(2026)
        }

        fn estimate(&self, _query: &ValuationQuery) -> Result<ValuationEstimate, ValuationError> {
            Err(ValuationError::InvalidQuery(
                "test model does not estimate".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn cached_model_is_rejected_when_its_snapshot_predates_faa_manifests() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let snapshot_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO valuation_snapshots (
              capture_time, input_sha256, selection_policy_json,
              feature_schema_version, included_count, excluded_count
            ) VALUES ('2026-07-20', lower(hex(randomblob(32))), '{}', ?, 0, 0)
            RETURNING id
            "#,
        )
        .bind(crate::valuation::FEATURE_SCHEMA_VERSION as i64)
        .fetch_one(pool)
        .await
        .unwrap();
        let model = SnapshotOnlyModel { snapshot_id };

        let error = require_valuation_model_faa_admission(&db, Some(&model))
            .await
            .expect_err("a cached pre-FAA model must not remain serving");

        assert!(error.to_string().contains("predates the mandatory FAA"));
    }

    #[tokio::test]
    async fn direct_valuation_rejects_a_retained_non_n_listing() {
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
              asking_price_usd, airframe_hours, registration_number,
              ingestion_state, ingestion_completed_at
            ) VALUES (1, 1, 2020, 200000, 1000, 'C-GABC', 'ready', CURRENT_TIMESTAMP)
            RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();

        let error = aircraft_listing_value_with_model(&db, 1, listing_id, None)
            .await
            .expect_err("a retained foreign aircraft must never receive a valuation");

        assert!(error.to_string().contains("non_n_registration"));
        assert!(
            aircraft_options(&db, 1).await.unwrap().is_empty(),
            "a foreign-only variant must not appear as a valuation option"
        );
    }

    #[tokio::test]
    async fn listing_spec_enrichment_rejects_before_gemini_and_persistence() {
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

        let error = enrich_aircraft_spec_for_listing_if_missing(
            &db,
            Some(&extractor),
            listing_id,
            Some("This text must never be sent to Gemini."),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("missing_registration"));
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM aircraft_model_spec_versions")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(stored, 0);
    }

    async fn insert_test_avionics_model(
        db: &AppDb,
        manufacturer_id: i64,
        type_id: i64,
        name: &str,
        normalized_name: &str,
        identifier: &str,
        catalog_status: &str,
        valuation_scope: &str,
    ) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let model_id = if catalog_status == "approved" {
            sqlx::query_scalar(
                r#"
                INSERT INTO avionics_models (
                  avionics_manufacturer_id, name, normalized_name,
                  manufacturer_identifier_kind, manufacturer_identifier,
                  normalized_manufacturer_identifier, identity_source_url,
                  identity_source_title, identity_evidence_text, identity_evidence_kind,
                  identity_confidence, catalog_reviewed_at, introduced_year,
                  estimated_unit_value_usd, value_basis, replacement_cost_usd,
                  value_reference_year, value_source, valuation_scope
                ) VALUES (
                  ?, ?, ?, 'manufacturer_model_number', ?, ?,
                  'https://www.garmin.com/aviation/test-product/',
                  'Garmin test product',
                  'Manufacturer reference identifies this exact test product.',
                  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP, 2020,
                  10000, 'installed_contribution', 20000, 2026,
                  'authoritative test fixture', ?
                ) RETURNING id
                "#,
            )
            .bind(manufacturer_id)
            .bind(name)
            .bind(normalized_name)
            .bind(identifier)
            .bind(
                identifier
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase(),
            )
            .bind(valuation_scope)
            .fetch_one(pool)
            .await
            .unwrap()
        } else {
            sqlx::query_scalar(
                r#"
                INSERT INTO avionics_models (
                  avionics_manufacturer_id, name, normalized_name,
                  introduced_year, estimated_unit_value_usd, value_basis,
                  replacement_cost_usd, value_reference_year, value_source,
                  valuation_scope
                ) VALUES (
                  ?, ?, ?, 2020, 99999, 'installed_contribution',
                  120000, 2026, 'legacy unreviewed fixture', ?
                ) RETURNING id
                "#,
            )
            .bind(manufacturer_id)
            .bind(name)
            .bind(normalized_name)
            .bind(valuation_scope)
            .fetch_one(pool)
            .await
            .unwrap()
        };
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(model_id)
        .bind(type_id)
        .execute(pool)
        .await
        .unwrap();
        if catalog_status == "approved" {
            sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
                .bind(model_id)
                .execute(pool)
                .await
                .unwrap();
        }
        model_id
    }

    #[test]
    fn listing_avionics_are_deltas_from_factory_configuration() {
        let defaults = vec![
            link(1, 1, "installed", None, "high"),
            link(2, 1, "installed", None, "high"),
        ];
        let listing = vec![
            link(3, 1, "replaces", Some(1), "high"),
            link(4, 1, "installed", None, "high"),
        ];

        let resolved = resolve_avionics_configuration(&defaults, &listing, &[]);

        assert_eq!(resolved.get(&1), None);
        assert_eq!(resolved.get(&2), Some(&1));
        assert_eq!(resolved.get(&3), Some(&1));
        assert_eq!(resolved.get(&4), Some(&1));
    }

    #[test]
    fn weak_listing_evidence_cannot_replace_factory_equipment() {
        let defaults = vec![link(1, 1, "installed", None, "high")];
        let listing = vec![link(2, 1, "replaces", Some(1), "low")];

        let resolved = resolve_avionics_configuration(&defaults, &listing, &[]);

        assert_eq!(resolved.get(&1), Some(&1));
        assert_eq!(resolved.get(&2), None);
    }

    #[test]
    fn integrated_suite_consumes_only_its_bundled_component_quantity() {
        let listing = vec![
            AvionicsConfigurationLink {
                valuation_scope: "integrated_suite".to_string(),
                ..link(10, 1, "installed", None, "high")
            },
            link(11, 3, "installed", None, "high"),
        ];
        let memberships = vec![AvionicsSuiteMembership {
            suite_model_id: 10,
            component_model_id: 11,
            quantity: 2,
        }];

        let resolved = resolve_avionics_configuration(&[], &listing, &memberships);

        assert_eq!(resolved.get(&10), Some(&1));
        assert_eq!(resolved.get(&11), Some(&1));
    }

    #[tokio::test]
    async fn legacy_unreviewed_catalog_rows_are_excluded_from_valuation_inputs() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Test', 'test')",
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
              asking_price_usd, airframe_hours
            ) VALUES (1, 1, 2020, 100000, 1000)
            RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let manufacturer_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Garmin', 'garmin') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let type_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Flight Display', 'flight display') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let approved_suite_id = insert_test_avionics_model(
            &db,
            manufacturer_id,
            type_id,
            "Approved Suite",
            "approved suite",
            "APPROVED-SUITE-1",
            "approved",
            "integrated_suite",
        )
        .await;
        let transponder_type_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Transponder', 'transponder') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(approved_suite_id)
        .bind(transponder_type_id)
        .execute(pool)
        .await
        .unwrap();
        let approved_component_id = insert_test_avionics_model(
            &db,
            manufacturer_id,
            type_id,
            "Approved Display",
            "approved display",
            "APPROVED-DISPLAY-1",
            "approved",
            "unit",
        )
        .await;
        let unreviewed_id = insert_test_avionics_model(
            &db,
            manufacturer_id,
            type_id,
            "Legacy Guess",
            "legacy guess",
            "",
            "unreviewed",
            "unit",
        )
        .await;

        // These rows model associations preserved by the one-time migration.
        sqlx::query("DROP TRIGGER aircraft_sale_listing_avionics_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER aircraft_model_variant_default_avionics_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER avionics_suite_components_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        for model_id in [approved_suite_id, unreviewed_id] {
            sqlx::query(
                r#"
                INSERT INTO aircraft_sale_listing_avionics (
                  aircraft_sale_listing_id, avionics_model_id, source_confidence
                ) VALUES (?, ?, 'high')
                "#,
            )
            .bind(listing_id)
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                r#"
                INSERT INTO aircraft_model_variant_default_avionics (
                  aircraft_model_variant_id, model_year, avionics_model_id,
                  quantity, source_url, source_title, source_notes, source_confidence
                ) VALUES (
                  1, 2020, ?, 1, 'https://example.test/factory-reference',
                  'Factory reference', 'Fixture', 'high'
                )
                "#,
            )
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO avionics_suite_components (suite_model_id, component_model_id, quantity) VALUES (?, ?, 1), (?, ?, 1)",
        )
        .bind(approved_suite_id)
        .bind(approved_component_id)
        .bind(approved_suite_id)
        .bind(unreviewed_id)
        .execute(pool)
        .await
        .unwrap();

        let listing_rows = listing_avionics_estimates(&db, listing_id).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM avionics_model_types WHERE avionics_model_id = ?",
            )
            .bind(approved_suite_id)
            .fetch_one(pool)
            .await
            .unwrap(),
            2,
            "the fixture must exercise a multi-capability physical product"
        );
        assert_eq!(
            listing_rows
                .iter()
                .map(|row| row.avionics_model_id)
                .collect::<Vec<_>>(),
            vec![approved_suite_id]
        );
        let default_rows = model_year_default_avionics_estimates(&db, 1, 2020)
            .await
            .unwrap();
        assert_eq!(
            default_rows
                .iter()
                .map(|row| row.avionics_model_id)
                .collect::<Vec<_>>(),
            vec![approved_suite_id]
        );
        assert_eq!(
            avionics_suite_memberships(&db).await.unwrap(),
            vec![AvionicsSuiteMembership {
                suite_model_id: approved_suite_id,
                component_model_id: approved_component_id,
                quantity: 1,
            }]
        );
    }

    #[tokio::test]
    async fn unavailable_listing_only_model_never_exposes_legacy_as_primary_estimate() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let row = AircraftListingPointRow {
            id: 1,
            manufacturer_id: 1,
            model_id: 1,
            aircraft_model_variant_id: 1,
            is_verified: false,
            source_url: None,
            model_year: 2000,
            asking_price_usd: 100_000.0,
            currency: "USD".to_string(),
            added_at: "2026-07-20".to_string(),
            status: "active".to_string(),
            registration_number: None,
            serial_number: None,
            airframe_hours: 1_000.0,
            engine_hours: None,
            engine_time_basis: "unknown".to_string(),
            engine_time_evidence: None,
            engine_time_confidence: None,
            propeller_hours: None,
            propeller_time_basis: "unknown".to_string(),
            propeller_time_evidence: None,
            propeller_time_confidence: None,
        };
        let point = listing_value_point(&db, &row, None, None, None, false)
            .await
            .unwrap();
        assert!(point.estimated_value_usd.is_none());
        assert!(point.value_curve.is_empty());
        assert!(point
            .estimate_error
            .is_some_and(|error| error.contains("Listing-only valuation unavailable")));
    }

    #[tokio::test]
    async fn serving_uses_the_models_snapshot_market_year() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let row = AircraftListingPointRow {
            id: 1,
            manufacturer_id: 1,
            model_id: 1,
            aircraft_model_variant_id: 1,
            is_verified: false,
            source_url: None,
            model_year: 2000,
            asking_price_usd: 100_000.0,
            currency: "USD".to_string(),
            added_at: "2025-12-31".to_string(),
            status: "active".to_string(),
            registration_number: None,
            serial_number: Some("MARKET-YEAR-1".to_string()),
            airframe_hours: 1_000.0,
            engine_hours: None,
            engine_time_basis: "unknown".to_string(),
            engine_time_evidence: None,
            engine_time_confidence: None,
            propeller_hours: None,
            propeller_time_basis: "unknown".to_string(),
            propeller_time_evidence: None,
            propeller_time_confidence: None,
        };
        let training = TrainingListing {
            listing_id: 10,
            duplicate_group_key: "serial:TRAINING-1".to_string(),
            category_key: None,
            manufacturer_id: 1,
            model_id: 1,
            variant_id: 1,
            model_year: 2000,
            snapshot_year: 2030,
            asking_price_usd: 100_000.0,
            airframe_hours: Some(1_000.0),
            engine_times: vec![],
            propeller_times: vec![],
            equipment_tokens: vec![],
            valuation_facts: vec![],
            technical_field_count: 2,
        };
        let model =
            ComparableModel::new(0, 7, vec![training], ComparableConfig::default()).unwrap();

        let point = listing_value_point(&db, &row, None, None, Some(&model), false)
            .await
            .unwrap();

        assert!(point.estimated_value_usd.is_some());
        assert_eq!(point.value_curve[0].valuation_year, 2030);
        assert_eq!(point.value_curve[0].age_years, 30.0);
    }

    fn factory_spec_response() -> Value {
        json!({
            "depreciation_profile": "generic:all",
            "fuel_burn_gph": 13.0,
            "oil_quarts_per_hour": 0.1,
            "oil_price_per_quart_usd": 12.0,
            "engine_manufacturer": "Continental",
            "engine_model": "O-470-R",
            "engine_count": 1,
            "engine_tbo_hours": 1500.0,
            "engine_overhaul_cost_usd": 40000.0,
            "engine_value_baseline_life_fraction": 0.5,
            "propeller_manufacturer": "McCauley",
            "propeller_model": "D2A34C58",
            "propeller_count": 1,
            "propeller_tbo_hours": 2000.0,
            "propeller_overhaul_cost_usd": 10000.0,
            "propeller_value_baseline_life_fraction": 0.5,
            "powerplant_source_url": "https://www.faa.gov/aircraft/reference.pdf",
            "powerplant_source_title": "Type certificate data",
            "powerplant_source_confidence": "high",
            "configuration_scope": "factory",
            "evidence_kind": "authoritative_reference",
            "source_confidence": "high",
            "is_valuation_eligible": true,
            "annual_inspection_usd": 2500.0,
            "other_maintenance_per_hour": 35.0,
            "confidence": "high"
        })
    }

    fn variant() -> AircraftVariantOption {
        AircraftVariantOption {
            manufacturer_id: 1,
            manufacturer: "Airframe Maker".to_string(),
            model_id: 2,
            model: "Family".to_string(),
            variant_id: 3,
            variant: "Variant".to_string(),
            listing_count: 1,
        }
    }

    #[test]
    fn authoritative_factory_spec_is_eligible() {
        let item = spec_enrichment_item_from_response(
            &variant(),
            &factory_spec_response(),
            1,
            &["https://market.example/listing/1"],
        )
        .unwrap();

        assert_eq!(item.configuration_scope, "factory_default");
        assert!(item.is_valuation_eligible);
        assert!(item.valuation_eligibility_notes.is_empty());
    }

    #[test]
    fn sale_listing_cannot_seed_factory_spec() {
        let mut response = factory_spec_response();
        response["powerplant_source_url"] =
            json!("https://market.example/listing/1?tracking=ignored");
        let item = spec_enrichment_item_from_response(
            &variant(),
            &response,
            1,
            &["https://market.example/listing/1"],
        )
        .unwrap();

        assert!(!item.is_valuation_eligible);
        assert!(item
            .valuation_eligibility_notes
            .iter()
            .any(|note| note.contains("sale listings")));
    }

    #[test]
    fn unrelated_sale_listing_url_cannot_pose_as_authoritative_spec_evidence() {
        let mut response = factory_spec_response();
        response["powerplant_source_url"] =
            json!("https://another-market.example/aircraft-for-sale/reference-looking-title");
        let item = spec_enrichment_item_from_response(
            &variant(),
            &response,
            1,
            &["https://market.example/listing/1"],
        )
        .unwrap();

        assert!(!item.is_valuation_eligible);
        assert!(item
            .valuation_eligibility_notes
            .iter()
            .any(|note| note.contains("sale listings")));
    }
}
