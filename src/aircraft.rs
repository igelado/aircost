pub mod catalog;
pub mod curation;
pub mod faa;
pub mod identity;
pub mod observations;
pub mod reference;
pub mod repair;
pub mod verification;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use sqlx::{Connection, FromRow};

use self::faa::{audit_listing_admission, require_listing_admission, AircraftAdmissionError};
use self::reference::persistence::{
    listing_reference_status, normalized_reference_price, PublishedReferenceConfiguration,
    ReferenceGap,
};
use crate::avionics::authorization::{
    listing_authorization_state_postgres, listing_authorization_state_sqlite,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::valuation::dataset::{require_snapshot_faa_admission, technical_field_count};
use crate::valuation::{
    source_backed_component_observation, FactoryReferenceFeature, SupportGrade, ValuationBreakdown,
    ValuationModel, ValuationQuery,
};

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

#[derive(Clone, Debug, FromRow)]
struct AvionicsEstimateRow {
    avionics_model_id: i64,
    quantity: i64,
    valuation_scope: String,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    source_confidence: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct ListingAvionicsEstimateRow {
    avionics_model_id: i64,
    quantity: i64,
    valuation_scope: String,
    configuration_action: String,
    replaces_avionics_model_id: Option<i64>,
    source_confidence: Option<String>,
}

impl From<ListingAvionicsEstimateRow> for AvionicsEstimateRow {
    fn from(row: ListingAvionicsEstimateRow) -> Self {
        Self {
            avionics_model_id: row.avionics_model_id,
            quantity: row.quantity,
            valuation_scope: row.valuation_scope,
            configuration_action: row.configuration_action,
            replaces_avionics_model_id: row.replaces_avionics_model_id,
            source_confidence: row.source_confidence,
        }
    }
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
    pub listings: Vec<AircraftListingValuePoint>,
    pub message: Option<String>,
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
    pub factory_reference: Option<PublishedReferenceConfiguration>,
    pub factory_reference_gaps: Vec<ReferenceGap>,
    pub reference_valuation_basis: Option<AircraftReferenceValuationBasis>,
    pub value_curve: Vec<AircraftValueCurvePoint>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftReferenceValuationBasis {
    pub reference_configuration_version_id: i64,
    pub direct_cited_standard_configuration_price_usd: f64,
    pub direct_cited_nominal_dollar_year: i64,
    pub dollar_normalization_factor: f64,
    pub dollar_normalization_fact_id: Option<i64>,
    pub full_standard_configuration_price_usd: f64,
    pub nominal_dollar_year: i64,
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
        JOIN aircraft_valuation_compatibility_projections projection
          ON projection.aircraft_model_variant_id = variant.id
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
        JOIN aircraft_valuation_compatibility_projections projection
          ON projection.aircraft_model_variant_id =
             l.aircraft_model_variant_id
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
) -> StoreResult<AircraftVariantDetail> {
    aircraft_variant_detail_with_model(db, user_id, variant_id, None).await
}

pub async fn aircraft_variant_detail_with_model(
    db: &AppDb,
    user_id: i64,
    variant_id: i64,
    valuation_model: Option<&Arc<dyn ValuationModel>>,
) -> StoreResult<AircraftVariantDetail> {
    require_valuation_model_faa_admission(db, valuation_model.map(Arc::as_ref)).await?;
    let mut option = aircraft_option_for_variant(db, user_id, variant_id).await?;
    let rows = listing_points_for_variant(db, user_id, variant_id).await?;
    let mut listings = Vec::with_capacity(rows.len());
    for row in rows {
        match require_listing_admission(db, row.id).await {
            Ok(_) => {}
            Err(AircraftAdmissionError::Rejected { .. }) => continue,
            Err(error) => return Err(aircraft_admission_store_error(error)),
        }
        listings.push(listing_value_point(db, &row, valuation_model.map(Arc::as_ref)).await?);
    }
    option.listing_count = listings.len() as i64;
    let message = match valuation_model {
        Some(_) => None,
        None => Some(
            "Listing-only valuation unavailable: no approved model artifact or eligible comparable snapshot is available."
                .to_string(),
        ),
    };
    Ok(AircraftVariantDetail {
        option,
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
    listing_value_point(db, &row, valuation_model.map(Arc::as_ref)).await
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
        JOIN aircraft_valuation_compatibility_projections projection
          ON projection.aircraft_model_variant_id = variant.id
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
    valuation_model: Option<&dyn ValuationModel>,
) -> StoreResult<AircraftListingValuePoint> {
    let reference = listing_reference_status(db, row.id).await?;
    let reference_ready = reference.ready;
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
        factory_reference: reference.published,
        factory_reference_gaps: reference.gaps,
        reference_valuation_basis: None,
        value_curve: Vec::new(),
    };
    if !reference_ready {
        point.estimate_error = Some(
            "Valuation unavailable: no unique complete published factory reference profile applies to this aircraft."
                .to_string(),
        );
        return Ok(point);
    }

    if let Some(model) = valuation_model {
        let valuation_year = match model.market_year() {
            Ok(year) => year,
            Err(error) => {
                point.estimate_error = Some(error.to_string());
                return Ok(point);
            }
        };
        let published_reference = point
            .factory_reference
            .as_ref()
            .expect("ready reference status includes the published profile");
        let reference_basis =
            match reference_valuation_basis(db, row.id, published_reference, valuation_year).await?
            {
                Ok(basis) => basis,
                Err(gap) => {
                    let reason = gap.message.clone();
                    point.factory_reference_gaps.push(gap);
                    point.estimate_error = Some(format!(
                        "Reference-grounded valuation unavailable: {reason}"
                    ));
                    return Ok(point);
                }
            };
        point.reference_valuation_basis = Some(reference_basis.clone());
        // The exact published configuration and its explicit listing delta are
        // the monetary equipment basis. Do not also feed listing equipment
        // tokens into a model and count the same upgrade twice.
        let equipment_tokens = Vec::new();
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
            factory_reference: Some(FactoryReferenceFeature {
                configuration_id: published_reference.configuration_id,
                version_id: published_reference.version_id,
                full_standard_configuration_price_usd: reference_basis
                    .full_standard_configuration_price_usd,
                nominal_dollar_year: reference_basis.nominal_dollar_year,
            }),
        };
        match model.estimate(&query) {
            Ok(mut estimate) => {
                ground_estimate_to_reference(&mut estimate, &reference_basis)?;
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

    point.valuation_warning = Some(
        "No approved model artifact or eligible comparable snapshot is available.".to_string(),
    );
    point.estimate_error = Some(
        "Listing-only valuation unavailable: no approved model artifact or eligible comparable snapshot is available."
            .to_string(),
    );
    Ok(point)
}

async fn reference_valuation_basis(
    db: &AppDb,
    listing_id: i64,
    reference: &PublishedReferenceConfiguration,
    valuation_year: i64,
) -> StoreResult<Result<AircraftReferenceValuationBasis, ReferenceGap>> {
    let normalized_price = match normalized_reference_price(db, reference, valuation_year).await? {
        Ok(price) => price,
        Err(gap) => return Ok(Err(gap)),
    };
    let factory = reference_avionics_estimates(db, reference.version_id).await?;
    let listing = listing_avionics_estimates(db, listing_id).await?;
    let memberships = avionics_suite_memberships(db).await?;
    let factory_effective = effective_avionics_rows(&factory, &[], &memberships);
    let listing_effective = effective_avionics_rows(&factory, &listing, &memberships);
    let factory_quantities = factory_effective
        .iter()
        .map(|row| (row.avionics_model_id, row.quantity))
        .collect::<BTreeMap<_, _>>();
    let effective_quantities = listing_effective
        .iter()
        .map(|row| (row.avionics_model_id, row.quantity))
        .collect::<BTreeMap<_, _>>();
    if factory_quantities != effective_quantities {
        return Ok(Err(ReferenceGap::new(
            "avionics_value_fact_missing",
            "the listing avionics configuration differs from the published factory baseline, but no immutable evidence-backed avionics installed-value facts are available",
        )));
    }
    if !normalized_price.normalized_amount_usd.is_finite()
        || normalized_price.normalized_amount_usd <= 0.0
    {
        return Ok(Err(ReferenceGap::new(
            "reference_valuation_basis_invalid",
            "factory standard-configuration price is invalid",
        )));
    }
    Ok(Ok(AircraftReferenceValuationBasis {
        reference_configuration_version_id: reference.version_id,
        direct_cited_standard_configuration_price_usd: reference.price_usd,
        direct_cited_nominal_dollar_year: reference.price_reference_year,
        dollar_normalization_factor: normalized_price.normalization_factor,
        dollar_normalization_fact_id: normalized_price.official_normalization_fact_id,
        full_standard_configuration_price_usd: normalized_price.normalized_amount_usd,
        nominal_dollar_year: valuation_year,
    }))
}

fn ground_estimate_to_reference(
    estimate: &mut crate::valuation::ValuationEstimate,
    reference: &AircraftReferenceValuationBasis,
) -> StoreResult<()> {
    let modeled_configuration_anchor = estimate.modeled_factory_configuration_anchor_usd;
    if !modeled_configuration_anchor.is_finite() || modeled_configuration_anchor <= 0.0 {
        return Err(AircraftStoreError::Model(
            "valuation model produced an invalid configuration anchor".to_string(),
        ));
    }
    let scale = reference.full_standard_configuration_price_usd / modeled_configuration_anchor;
    for value in [
        &mut estimate.estimated_value_usd,
        &mut estimate.low_value_usd,
        &mut estimate.high_value_usd,
    ] {
        *value *= scale;
    }
    for point in &mut estimate.depreciation {
        point.estimated_value_usd *= scale;
        point.low_value_usd *= scale;
        point.high_value_usd *= scale;
    }
    estimate.modeled_factory_configuration_anchor_usd =
        reference.full_standard_configuration_price_usd;
    estimate.breakdown.global_anchor_usd = reference.full_standard_configuration_price_usd;
    estimate.breakdown.category_factor = 1.0;
    estimate.breakdown.manufacturer_factor = 1.0;
    estimate.breakdown.model_factor = 1.0;
    estimate.breakdown.variant_factor = 1.0;
    estimate.breakdown.optional_features_factor = 1.0;
    estimate.estimated_error_fraction = ((estimate.high_value_usd / estimate.estimated_value_usd)
        - 1.0)
        .max(1.0 - estimate.low_value_usd / estimate.estimated_value_usd);
    let current_value = estimate.estimated_value_usd;
    for point in &mut estimate.depreciation {
        point.depreciation_usd = (current_value - point.estimated_value_usd).max(0.0);
        point.depreciation_fraction = point.depreciation_usd / current_value;
        point.estimated_error_fraction = ((point.high_value_usd / point.estimated_value_usd) - 1.0)
            .max(1.0 - point.low_value_usd / point.estimated_value_usd);
    }
    for index in 0..estimate.depreciation.len().saturating_sub(1) {
        estimate.depreciation[index].one_year_depreciation_fraction = (1.0
            - estimate.depreciation[index + 1].estimated_value_usd
                / estimate.depreciation[index].estimated_value_usd)
            .max(0.0);
    }
    if estimate.depreciation.len() > 1 {
        let last = estimate.depreciation.len() - 1;
        estimate.depreciation[last].one_year_depreciation_fraction =
            estimate.depreciation[last - 1].one_year_depreciation_fraction;
    }
    Ok(())
}

async fn listing_avionics_estimates(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<Vec<AvionicsEstimateRow>> {
    const LISTING_AVIONICS_ESTIMATES_SQL: &str = r#"
        SELECT
          model.id AS avionics_model_id,
          link.quantity,
          model.valuation_scope,
          link.configuration_action,
          link.replaces_avionics_model_id,
          link.source_confidence
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model
          ON model.id = link.avionics_model_id
        WHERE link.aircraft_sale_listing_id = ?
          AND link.source IN (
            'listing', 'listing_explicit_count', 'listing_review', 'human_review'
          )
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
    "#;
    macro_rules! load_in_transaction {
        ($transaction:ident, $authorization_state:path) => {{
            let authorization_state = $authorization_state(db, &mut $transaction, listing_id)
                .await
                .map_err(|error| AircraftStoreError::Database(error.to_string()))?;
            if !authorization_state.all_automatic_associations_current() {
                return Err(AircraftStoreError::Model(
                    "listing avionics association authorization is missing or stale".to_string(),
                ));
            }
            let sql = db.sql(LISTING_AVIONICS_ESTIMATES_SQL);
            let rows = sqlx::query_as::<_, ListingAvionicsEstimateRow>(&sql)
                .bind(listing_id)
                .fetch_all(&mut *$transaction)
                .await?;
            rows.into_iter().map(Into::into).collect()
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            let estimates = load_in_transaction!(transaction, listing_authorization_state_sqlite);
            transaction.commit().await?;
            Ok(estimates)
        }
        DatabaseBackend::Postgres(pool) => {
            let mut connection = pool.acquire().await?;
            let mut transaction = connection
                .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
                .await?;
            let estimates = load_in_transaction!(transaction, listing_authorization_state_postgres);
            transaction.commit().await?;
            Ok(estimates)
        }
    }
}

async fn reference_avionics_estimates(
    db: &AppDb,
    reference_configuration_version_id: i64,
) -> StoreResult<Vec<AvionicsEstimateRow>> {
    Ok(query_as_all!(
        db,
        AvionicsEstimateRow,
        r#"
        SELECT
          model.id AS avionics_model_id,
          reference_avionics.quantity,
          model.valuation_scope,
          'installed' AS configuration_action,
          NULL AS replaces_avionics_model_id,
          'high' AS source_confidence
        FROM aircraft_reference_avionics reference_avionics
        JOIN avionics_models model
          ON model.id = reference_avionics.avionics_model_id
        JOIN aircraft_reference_configuration_versions reference_version
          ON reference_version.id = reference_avionics.aircraft_reference_configuration_version_id
        WHERE reference_avionics.aircraft_reference_configuration_version_id = ?
          AND reference_version.publication_state = 'published'
          AND model.catalog_status = 'approved'
          AND reference_avionics.quantity > 0
        ORDER BY reference_avionics.id
        "#,
        reference_configuration_version_id
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

    let high_confidence_deltas = listing_deltas
        .iter()
        .filter(|link| is_high_confidence(link.source_confidence.as_deref()))
        .collect::<Vec<_>>();

    // Resolve deltas as a set, not in row-id order. Readiness validation
    // rejects ambiguous graphs (an installed identity may not also be a
    // replacement target, and a target may be displaced only once), while
    // this two-pass evaluation remains deterministic even for retained legacy
    // rows that have not reached that gate.
    for link in &high_confidence_deltas {
        if matches!(link.configuration_action.as_str(), "replaces" | "removes") {
            if let Some(replaced_id) = link.replaces_avionics_model_id {
                let removed_quantity = quantities.remove(&replaced_id).unwrap_or_default();
                for membership in suite_memberships
                    .iter()
                    .filter(|membership| membership.suite_model_id == replaced_id)
                {
                    if let Some(component_quantity) =
                        quantities.get_mut(&membership.component_model_id)
                    {
                        *component_quantity = component_quantity.saturating_sub(
                            removed_quantity.saturating_mul(membership.quantity.max(1)),
                        );
                    }
                }
            }
        }
    }
    for link in high_confidence_deltas {
        if matches!(link.configuration_action.as_str(), "installed" | "replaces") {
            quantities
                .entry(link.avionics_model_id)
                .and_modify(|quantity| *quantity = (*quantity).max(link.quantity.max(1)))
                .or_insert_with(|| link.quantity.max(1));
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        aircraft_listing_value_with_model, aircraft_option_for_variant, aircraft_options,
        avionics_suite_memberships, ground_estimate_to_reference, listing_avionics_estimates,
        listing_value_point, require_valuation_model_faa_admission, resolve_avionics_configuration,
        AircraftListingPointRow, AircraftReferenceValuationBasis, AvionicsConfigurationLink,
        AvionicsSuiteMembership,
    };
    use crate::avionics::fingerprint::active_collision_closure_revision_sha256;
    use crate::avionics::manufacturer::ensure_test_manufacturer_identity;
    use crate::avionics::reuse::refresh_reuse_attestation_sqlite;
    use crate::db::{AppDb, DatabaseBackend};
    use crate::listing::replay::retained_capture_timestamp_chronology_valid;
    use crate::listing::review::{
        association_observation_sha256_from_values, ListingAssociationRole,
    };
    use crate::plugin::current_checkpoint_contains_avionics_source_evidence;
    use crate::valuation::{
        DepreciationPoint, SupportGrade, ValuationBreakdown, ValuationError, ValuationEstimate,
        ValuationModel, ValuationQuery,
    };
    use sha2::{Digest, Sha256};

    #[derive(Debug, sqlx::FromRow)]
    struct RetainedListingAvionicsCaptureRow {
        install_created_at: String,
        submitted_at: String,
        install_revoked_at: Option<String>,
    }

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
    async fn public_variant_lookup_rejects_an_unprojected_legacy_variant() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let manufacturer_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_manufacturers (name, normalized_name)
            VALUES ('Legacy Manufacturer', 'legacy manufacturer')
            RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_models (
              aircraft_manufacturer_id, name, normalized_name
            ) VALUES (?, 'Legacy Model', 'legacy model')
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let variant_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_model_variants (
              aircraft_model_id, name, normalized_name
            ) VALUES (?, 'Legacy Variant', 'legacy variant')
            RETURNING id
            "#,
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let error = aircraft_option_for_variant(&db, 1, variant_id)
            .await
            .expect_err("an arbitrary legacy variant must not be publicly addressable");

        assert!(error.to_string().contains("aircraft variant not found"));
    }

    #[tokio::test]
    async fn direct_valuation_rejects_a_retained_non_n_listing() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let manufacturer_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_manufacturers (name, normalized_name)
            VALUES ('Gate Test', 'gate test')
            RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_models (
              aircraft_manufacturer_id, name, normalized_name
            ) VALUES (?, 'Model', 'model')
            RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let _variant_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_model_variants (
              aircraft_model_id, name, normalized_name
            ) VALUES (?, 'Variant', 'variant')
            RETURNING id
            "#,
        )
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, model_year,
              asking_price_usd, airframe_hours, registration_number,
              ingestion_state, ingestion_completed_at
            ) VALUES (
              (
                SELECT aircraft_model_variant_id
                FROM aircraft_sale_listing_pending_compatibility_placeholder
                WHERE singleton_id = 1
              ),
              1, 2020, 200000, 1000, 'C-GABC', 'incomplete', NULL
            )
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
            ensure_test_manufacturer_identity(db, manufacturer_id)
                .await
                .unwrap();
            sqlx::query(
                "UPDATE avionics_models SET catalog_status = 'approved', verification_method = 'automated', verified_by_user_id = NULL WHERE id = ?",
            )
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
    fn retained_ambiguous_avionics_deltas_are_evaluated_independently_of_row_order() {
        let defaults = vec![link(1, 1, "installed", None, "high")];
        let first_order = vec![
            link(1, 1, "installed", None, "high"),
            link(2, 1, "replaces", Some(1), "high"),
        ];
        let mut reverse_order = first_order.clone();
        reverse_order.reverse();

        assert_eq!(
            resolve_avionics_configuration(&defaults, &first_order, &[]),
            resolve_avionics_configuration(&defaults, &reverse_order, &[])
        );
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

    #[test]
    fn factory_suite_and_explicit_bundled_components_resolve_to_one_baseline() {
        let factory = vec![
            AvionicsConfigurationLink {
                valuation_scope: "integrated_suite".to_string(),
                ..link(10, 1, "installed", None, "high")
            },
            link(11, 2, "installed", None, "high"),
        ];
        let memberships = vec![AvionicsSuiteMembership {
            suite_model_id: 10,
            component_model_id: 11,
            quantity: 2,
        }];

        assert_eq!(
            resolve_avionics_configuration(&factory, &[], &memberships),
            BTreeMap::from([(10, 1)])
        );
    }

    #[test]
    fn suite_replacement_is_detected_after_symmetric_bundle_resolution() {
        let factory = vec![
            AvionicsConfigurationLink {
                valuation_scope: "integrated_suite".to_string(),
                ..link(10, 1, "installed", None, "high")
            },
            link(11, 2, "installed", None, "high"),
        ];
        let listing = vec![
            AvionicsConfigurationLink {
                valuation_scope: "integrated_suite".to_string(),
                ..link(20, 1, "replaces", Some(10), "high")
            },
            link(21, 2, "installed", None, "high"),
        ];
        let memberships = vec![
            AvionicsSuiteMembership {
                suite_model_id: 10,
                component_model_id: 11,
                quantity: 2,
            },
            AvionicsSuiteMembership {
                suite_model_id: 20,
                component_model_id: 21,
                quantity: 2,
            },
        ];

        let factory_resolved = resolve_avionics_configuration(&factory, &[], &memberships);
        let listing_resolved = resolve_avionics_configuration(&factory, &listing, &memberships);
        assert_eq!(factory_resolved, BTreeMap::from([(10, 1)]));
        assert_eq!(listing_resolved, BTreeMap::from([(20, 1)]));
        assert_ne!(factory_resolved, listing_resolved);
    }

    #[test]
    fn reference_grounding_uses_explicit_model_anchor_and_recomputes_derived_values() {
        let mut estimate = ValuationEstimate {
            modeled_factory_configuration_anchor_usd: 200_000.0,
            estimated_value_usd: 100_000.0,
            low_value_usd: 80_000.0,
            high_value_usd: 120_000.0,
            estimated_error_fraction: 0.2,
            support: SupportGrade::High,
            model_kind: "structural".to_string(),
            model_version_id: 1,
            snapshot_id: 1,
            breakdown: ValuationBreakdown {
                global_anchor_usd: 100_000.0,
                age_factor: 0.5,
                expected_airframe_hours: 1_000.0,
                hours_residual: 0.0,
                hours_factor: 1.0,
                category_factor: 1.0,
                manufacturer_factor: 2.0,
                model_factor: 1.0,
                variant_factor: 1.0,
                optional_features_factor: 1.0,
            },
            depreciation: vec![
                DepreciationPoint {
                    horizon_years: 0,
                    valuation_year: 2026,
                    age_years: 10.0,
                    airframe_hours: Some(1_000.0),
                    estimated_value_usd: 100_000.0,
                    low_value_usd: 80_000.0,
                    high_value_usd: 120_000.0,
                    depreciation_usd: 99.0,
                    depreciation_fraction: 0.99,
                    one_year_depreciation_fraction: 0.99,
                    estimated_error_fraction: 0.99,
                    support: SupportGrade::High,
                },
                DepreciationPoint {
                    horizon_years: 1,
                    valuation_year: 2027,
                    age_years: 11.0,
                    airframe_hours: Some(1_100.0),
                    estimated_value_usd: 90_000.0,
                    low_value_usd: 72_000.0,
                    high_value_usd: 108_000.0,
                    depreciation_usd: 99.0,
                    depreciation_fraction: 0.99,
                    one_year_depreciation_fraction: 0.99,
                    estimated_error_fraction: 0.99,
                    support: SupportGrade::High,
                },
            ],
        };
        let reference = AircraftReferenceValuationBasis {
            reference_configuration_version_id: 42,
            direct_cited_standard_configuration_price_usd: 480_000.0,
            direct_cited_nominal_dollar_year: 2026,
            dollar_normalization_factor: 1.0,
            dollar_normalization_fact_id: None,
            full_standard_configuration_price_usd: 480_000.0,
            nominal_dollar_year: 2026,
        };

        ground_estimate_to_reference(&mut estimate, &reference).unwrap();

        assert_eq!(estimate.estimated_value_usd, 240_000.0);
        assert_eq!(estimate.modeled_factory_configuration_anchor_usd, 480_000.0);
        assert_eq!(estimate.breakdown.global_anchor_usd, 480_000.0);
        assert_eq!(estimate.breakdown.manufacturer_factor, 1.0);
        assert_eq!(estimate.depreciation[0].estimated_value_usd, 240_000.0);
        assert_eq!(estimate.depreciation[1].estimated_value_usd, 216_000.0);
        assert_eq!(estimate.depreciation[0].depreciation_usd, 0.0);
        assert_eq!(estimate.depreciation[1].depreciation_usd, 24_000.0);
        assert!((estimate.depreciation[1].depreciation_fraction - 0.1).abs() < 1e-12);
        assert!((estimate.depreciation[0].one_year_depreciation_fraction - 0.1).abs() < 1e-12);
        assert!((estimate.depreciation[1].one_year_depreciation_fraction - 0.1).abs() < 1e-12);
        assert!((estimate.estimated_error_fraction - 0.2).abs() < 1e-12);
        assert!((estimate.depreciation[1].estimated_error_fraction - 0.2).abs() < 1e-12);
    }

    #[tokio::test]
    async fn unreviewed_catalog_rows_are_excluded_from_listing_configuration_inputs() {
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
            ) VALUES (
              (
                SELECT aircraft_model_variant_id
                FROM aircraft_sale_listing_pending_compatibility_placeholder
                WHERE singleton_id = 1
              ),
              1, 2020, 100000, 1000
            )
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
        sqlx::query("DROP TRIGGER avionics_suite_components_approved_insert")
            .execute(pool)
            .await
            .unwrap();
        for (model_id, source) in [
            (approved_suite_id, "listing_review"),
            (unreviewed_id, "listing"),
        ] {
            sqlx::query(
                r#"
                INSERT INTO aircraft_sale_listing_avionics (
                  aircraft_sale_listing_id, avionics_model_id, source, source_confidence
                ) VALUES (?, ?, ?, 'high')
                "#,
            )
            .bind(listing_id)
            .bind(model_id)
            .bind(source)
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

        let listing_error = listing_avionics_estimates(&db, listing_id)
            .await
            .expect_err("one stale automatic association must reject the whole listing graph");
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
        assert!(listing_error
            .to_string()
            .contains("association authorization is missing or stale"));
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
    async fn explicit_count_listing_estimate_requires_its_complete_current_proof() {
        const EVIDENCE: &str = "Dual Garmin GIA63W COM/NAV/GPS/WAAS";
        const INSERT_AUTHORIZATION_SQL: &str = r#"
            INSERT INTO aircraft_sale_listing_avionics_link_authorizations (
              listing_link_id, association_role, avionics_model_id,
              authorization_kind, observation_sha256, product_fingerprint,
              evidence_capture_sha256, plugin_submission_id,
              extracted_listing_sha256, collision_closure_sha256
            )
            SELECT ?, 'installed', ?, 'manufacturer_reuse', ?,
                   product_fingerprint, ?, ?, ?, ?
            FROM avionics_product_reuse_attestations
            WHERE avionics_model_id = ?
        "#;
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url, model_year,
              asking_price_usd, airframe_hours
            ) VALUES (
              (
                SELECT aircraft_model_variant_id
                FROM aircraft_sale_listing_pending_compatibility_placeholder
                WHERE singleton_id = 1
              ),
              1, 'https://example.test/listing', 2020, 100000, 1000
            )
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
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('COM', 'com') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_evidence_kind, identity_confidence, catalog_reviewed_at,
              valuation_scope
            ) VALUES (
              ?, 'GIA63W', 'gia63w',
              'manufacturer_part_number', '011-01105-00', '0110110500',
              'https://manufacturer.example/aviation/gia63w',
              'GIA63W installation manual',
              'The manufacturer manual identifies the GIA63W and its part number.',
              'authoritative_reference', 'very_high', CURRENT_TIMESTAMP,
              'unit'
            ) RETURNING id
            "#,
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(model_id)
        .bind(type_id)
        .execute(pool)
        .await
        .unwrap();
        ensure_test_manufacturer_identity(&db, manufacturer_id)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE avionics_models SET catalog_status = 'approved', verification_method = 'automated', verified_by_user_id = NULL WHERE id = ?",
        )
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origins (
              authority_kind, avionics_manufacturer_identity_id, https_origin,
              evidence_source_url, evidence_source_title, evidence_text,
              approval_basis, approval_reason
            )
            SELECT 'manufacturer_primary', avionics_manufacturer_identity_id,
                   'https://manufacturer.example',
                   'https://manufacturer.example/aviation/gia63w',
                   'Manufacturer avionics catalog',
                   'The manufacturer publishes the exact GIA63W product.',
                   'curated_bootstrap', 'aircraft valuation test fixture'
            FROM avionics_approved_product_identities
            WHERE avionics_model_id = ?
            "#,
        )
        .bind(model_id)
        .execute(pool)
        .await
        .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        assert!(refresh_reuse_attestation_sqlite(
            &db,
            &mut transaction,
            model_id,
            "https://manufacturer.example/aviation/gia63w",
        )
        .await
        .unwrap());
        transaction.commit().await.unwrap();

        let rendered_html = format!("<html><body>{EVIDENCE}</body></html>");
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        let checkpoint = serde_json::json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2020,
            "asking_price_usd": 100000,
            "currency": "USD",
            "airframe_hours": 1000,
            "engine_hours": null,
            "engine_time_basis": "unknown",
            "engine_time_evidence": null,
            "engine_time_confidence": null,
            "propeller_hours": null,
            "propeller_time_basis": "unknown",
            "propeller_time_evidence": null,
            "propeller_time_confidence": null,
            "installed_engine": null,
            "installed_propeller": null,
            "registration_number": "N12345",
            "serial_number": "TEST123",
            "status": "active",
            "avionics": [{
                "manufacturer": "Garmin",
                "model": "GIA63W",
                "types": ["COM"],
                "quantity": 2,
                "configuration_action": "installed",
                "replaces": null,
                "source_evidence_text": EVIDENCE,
                "source_confidence": "high"
            }],
            "valuation_facts": []
        })
        .to_string();
        let checkpoint_sha256 = format!("{:x}", Sha256::digest(checkpoint.as_bytes()));
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (1, 'aircraft-explicit-count-key') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let submission_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json,
              canonical_listing_id
            ) VALUES (1, ?, 'https://example.test/listing', ?, ?,
                      'aircraft-explicit-count-signature', ?, ?)
            RETURNING id
            "#,
        )
        .bind(install_id)
        .bind(&rendered_html)
        .bind(&rendered_html_sha256)
        .bind(&checkpoint)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let link_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 2, 'listing_explicit_count', ?, 'high', 'installed')
            RETURNING id
            "#,
        )
        .bind(listing_id)
        .bind(model_id)
        .bind(EVIDENCE)
        .fetch_one(pool)
        .await
        .unwrap();
        let collision_closure_sha256 = active_collision_closure_revision_sha256(&db, model_id)
            .await
            .unwrap();
        let observation_sha256 = association_observation_sha256_from_values(
            listing_id,
            link_id,
            ListingAssociationRole::Installed,
            model_id,
            model_id,
            None,
            2,
            "installed",
            EVIDENCE,
        );
        sqlx::query(INSERT_AUTHORIZATION_SQL)
            .bind(link_id)
            .bind(model_id)
            .bind(&observation_sha256)
            .bind(&rendered_html_sha256)
            .bind(submission_id)
            .bind(&checkpoint_sha256)
            .bind(&collision_closure_sha256)
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();

        assert!(current_checkpoint_contains_avionics_source_evidence(
            &checkpoint,
            EVIDENCE,
        ));
        let retained_capture: RetainedListingAvionicsCaptureRow = sqlx::query_as(
            r#"
            SELECT install.created_at AS install_created_at,
                   submission.submitted_at,
                   install.revoked_at AS install_revoked_at
            FROM plugin_submissions submission
            JOIN plugin_installs install ON install.id = submission.plugin_install_id
            WHERE submission.id = ?
            "#,
        )
        .bind(submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(retained_capture_timestamp_chronology_valid(
            &retained_capture.install_created_at,
            &retained_capture.submitted_at,
            retained_capture.install_revoked_at.as_deref(),
        ));

        let current = listing_avionics_estimates(&db, listing_id).await.unwrap();
        assert_eq!(
            current
                .iter()
                .map(|row| (row.avionics_model_id, row.quantity))
                .collect::<Vec<_>>(),
            vec![(model_id, 2)]
        );

        sqlx::query(
            "DELETE FROM aircraft_sale_listing_avionics_link_authorizations WHERE listing_link_id = ?",
        )
        .bind(link_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(INSERT_AUTHORIZATION_SQL)
            .bind(link_id)
            .bind(model_id)
            .bind(&observation_sha256)
            .bind(&rendered_html_sha256)
            .bind(submission_id)
            .bind("0".repeat(64))
            .bind(&collision_closure_sha256)
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        assert!(listing_avionics_estimates(&db, listing_id)
            .await
            .expect_err("a stale checkpoint must reject the listing")
            .to_string()
            .contains("association authorization is missing or stale"));

        sqlx::query(
            "DELETE FROM aircraft_sale_listing_avionics_link_authorizations WHERE listing_link_id = ?",
        )
        .bind(link_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(INSERT_AUTHORIZATION_SQL)
            .bind(link_id)
            .bind(model_id)
            .bind("0".repeat(64))
            .bind(&rendered_html_sha256)
            .bind(submission_id)
            .bind(&checkpoint_sha256)
            .bind(&collision_closure_sha256)
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        assert!(listing_avionics_estimates(&db, listing_id)
            .await
            .expect_err("a stale observation must reject the listing")
            .to_string()
            .contains("association authorization is missing or stale"));

        sqlx::query("UPDATE aircraft_sale_listing_avionics SET source = 'listing' WHERE id = ?")
            .bind(link_id)
            .execute(pool)
            .await
            .unwrap();
        assert!(listing_avionics_estimates(&db, listing_id)
            .await
            .expect_err("ordinary automatic listing data also requires current authority")
            .to_string()
            .contains("association authorization is missing or stale"));
    }

    #[tokio::test]
    async fn listing_without_published_reference_is_never_valued() {
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
        let point = listing_value_point(&db, &row, None).await.unwrap();
        assert!(point.estimated_value_usd.is_none());
        assert!(point.value_curve.is_empty());
        assert!(point
            .estimate_error
            .is_some_and(|error| error.contains("published factory reference")));
    }
}
