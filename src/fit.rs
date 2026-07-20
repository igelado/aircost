use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::depreciation::{
    avionics_replacement_basis, default_avionics_profile, estimate_aircraft_value_in_year,
    nominal_dollar_factor, AircraftProfile, AvionicsComponent, DollarBasis, TimedComponent,
    DEFAULT_ANNUAL_AIRFRAME_HOURS,
};
use crate::valuation::dataset::{load_snapshot, sha256_hex};
use crate::valuation::store::persist_structural_candidate;
use crate::valuation::types::StructuralArtifactV1;
use crate::valuation::validation::{fit_validated_structural, ValidationReport};
use crate::valuation::StructuralFitConfig;

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
pub enum FitError {
    Database(String),
    Model(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct StructuralValuationFitReport {
    pub applied: bool,
    pub model_version_id: Option<i64>,
    pub snapshot_id: i64,
    pub deduplicated_sample_count: usize,
    pub configuration: StructuralFitConfig,
    pub artifact: StructuralArtifactV1,
    pub validation: ValidationReport,
    pub artifact_sha256: String,
}

pub async fn fit_structural_valuation(
    db: &AppDb,
    snapshot_id: i64,
    apply: bool,
) -> Result<StructuralValuationFitReport, crate::valuation::ValuationError> {
    let rows = load_snapshot(db, snapshot_id).await?;
    let mut config = StructuralFitConfig::default();
    let (mut artifact, mut validation) = fit_validated_structural(&rows, snapshot_id, &config)?;
    if rows
        .iter()
        .filter(|row| !row.equipment_tokens.is_empty())
        .count()
        >= 5
    {
        let mut equipment_config = config.clone();
        equipment_config.enable_equipment_count = true;
        let (equipment_artifact, equipment_validation) =
            fit_validated_structural(&rows, snapshot_id, &equipment_config)?;
        if equipment_validation
            .structural_metrics
            .median_absolute_percentage_error
            < validation
                .structural_metrics
                .median_absolute_percentage_error
        {
            config = equipment_config;
            artifact = equipment_artifact;
            validation = equipment_validation;
        }
    }
    let artifact_sha256 = sha256_hex(&serde_json::to_vec(&artifact)?);
    let model_version_id = if apply {
        Some(persist_structural_candidate(db, snapshot_id, &artifact, &validation, &config).await?)
    } else {
        None
    };
    Ok(StructuralValuationFitReport {
        applied: apply,
        model_version_id,
        snapshot_id,
        deduplicated_sample_count: rows.len(),
        configuration: config,
        artifact,
        validation,
        artifact_sha256,
    })
}

impl fmt::Display for FitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FitError::Database(message) | FitError::Model(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl std::error::Error for FitError {}

impl From<sqlx::Error> for FitError {
    fn from(error: sqlx::Error) -> Self {
        FitError::Database(error.to_string())
    }
}

type FitResult<T> = Result<T, FitError>;

#[derive(Debug, FromRow)]
struct FitListingRow {
    listing_id: i64,
    aircraft_model_id: i64,
    model_year: i64,
    asking_price_usd: f64,
    airframe_hours: f64,
    engine_hours: f64,
    propeller_hours: f64,
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
    average_inflation_rate: f64,
    engine_count: i64,
    engine_tbo_hours: Option<f64>,
    engine_overhaul_cost_usd: Option<f64>,
    engine_overhaul_cost_value_reference_year: Option<i64>,
    propeller_count: i64,
    propeller_tbo_hours: Option<f64>,
    propeller_overhaul_cost_usd: Option<f64>,
    propeller_overhaul_cost_value_reference_year: Option<i64>,
}

#[derive(Debug, FromRow)]
struct FitAvionicsRow {
    listing_id: i64,
    manufacturer: String,
    model: String,
    avionics_type: String,
    quantity: i64,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
    value_reference_year: Option<i64>,
}

#[derive(Debug, FromRow)]
struct FitReplacementBasisRow {
    aircraft_model_id: i64,
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
    average_inflation_rate: f64,
}

#[derive(Clone)]
struct FitSample {
    model_id: i64,
    model_year: i64,
    asking_price_usd: f64,
    airframe_hours: f64,
    engine_hours: f64,
    propeller_hours: f64,
    value_reference_year: i64,
    purchase_price_new_usd: f64,
    replacement_floor_basis_usd: Option<f64>,
    average_inflation_rate: f64,
    engine_count: i64,
    engine_tbo_hours: Option<f64>,
    engine_overhaul_cost_usd: Option<f64>,
    engine_overhaul_cost_value_reference_year: Option<i64>,
    propeller_count: i64,
    propeller_tbo_hours: Option<f64>,
    propeller_overhaul_cost_usd: Option<f64>,
    propeller_overhaul_cost_value_reference_year: Option<i64>,
    avionics: Vec<FitAvionicsComponent>,
}

#[derive(Clone)]
struct FitAvionicsComponent {
    name: String,
    quantity: i64,
    introduced_year: i64,
    estimated_unit_value_usd: f64,
    value_reference_year: i64,
}

#[derive(Clone)]
struct FitCandidate {
    profile: AircraftProfile,
    engine_baseline_life_fraction: f64,
    propeller_baseline_life_fraction: f64,
}

#[derive(Clone, Copy)]
struct AirframeFitParams {
    airframe_doubling_discount: f64,
    max_airframe_premium: f64,
    max_airframe_discount: f64,
    high_time_threshold_hours: Option<f64>,
    high_time_discount_at_double_threshold: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DepreciationFitReport {
    pub applied: bool,
    pub value_reference_year: i64,
    pub min_model_samples: usize,
    pub generic_profile: Option<FittedProfileReport>,
    pub model_profiles: Vec<FittedProfileReport>,
    pub assigned_model_profile_count: usize,
    pub assigned_generic_profile_count: usize,
    pub sample_count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct FittedProfileReport {
    pub scope: String,
    pub scope_key: String,
    pub profile_name: String,
    pub sample_count: usize,
    pub rmse_usd: f64,
    pub mae_fraction: f64,
    pub age_decay_rate: f64,
    pub long_run_residual_fraction: f64,
    pub new_to_used_discount_fraction: f64,
    pub airframe_doubling_discount: f64,
    pub max_airframe_premium: f64,
    pub max_airframe_discount: f64,
    pub replacement_floor_fraction: f64,
    pub high_time_threshold_hours: Option<f64>,
    pub high_time_discount_at_double_threshold: f64,
    pub engine_baseline_life_fraction: f64,
    pub propeller_baseline_life_fraction: f64,
}

pub async fn fit_depreciation_profiles(
    db: &AppDb,
    apply: bool,
    min_model_samples: usize,
    value_reference_year: Option<i64>,
) -> FitResult<DepreciationFitReport> {
    let min_model_samples = min_model_samples.max(2);
    let value_reference_year = value_reference_year.unwrap_or(2026);
    let samples = fit_samples(db, value_reference_year).await?;
    if samples.len() < 2 {
        return Ok(DepreciationFitReport {
            applied: apply,
            value_reference_year,
            min_model_samples,
            generic_profile: None,
            model_profiles: Vec::new(),
            assigned_model_profile_count: 0,
            assigned_generic_profile_count: 0,
            sample_count: samples.len(),
        });
    }

    let generic = fit_scope(
        "global",
        "all",
        "generic:all",
        &samples,
        value_reference_year,
    )?;
    let mut model_groups: BTreeMap<i64, Vec<FitSample>> = BTreeMap::new();
    for sample in &samples {
        model_groups
            .entry(sample.model_id)
            .or_default()
            .push(sample.clone());
    }

    let mut model_reports = Vec::new();
    for (model_id, model_samples) in model_groups {
        if model_samples.len() < min_model_samples {
            continue;
        }
        let profile_name = format!("model:{model_id}");
        model_reports.push(fit_scope(
            "model",
            &model_id.to_string(),
            &profile_name,
            &model_samples,
            value_reference_year,
        )?);
    }

    let mut assigned_model_profile_count = 0;
    let mut assigned_generic_profile_count = 0;
    if apply {
        save_fitted_profile(db, &generic, "global", "all", "all").await?;
        for report in &model_reports {
            save_fitted_profile(db, report, "model", &report.scope_key, "all").await?;
        }
        save_component_baseline(
            db,
            "engine",
            generic.engine_baseline_life_fraction,
            &generic,
        )
        .await?;
        save_component_baseline(
            db,
            "propeller",
            generic.propeller_baseline_life_fraction,
            &generic,
        )
        .await?;
        let model_profile_keys = model_reports
            .iter()
            .map(|report| report.scope_key.parse::<i64>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| FitError::Model(error.to_string()))?;
        for model_id in model_profile_keys {
            let profile_id = profile_id_by_name(db, &format!("model:{model_id}")).await?;
            assigned_model_profile_count +=
                assign_profile_to_model_specs(db, model_id, profile_id).await?;
        }
        let generic_profile_id = profile_id_by_name(db, "generic:all").await?;
        assigned_generic_profile_count =
            assign_profile_to_unfit_specs(db, generic_profile_id, min_model_samples as i64).await?;
    }

    Ok(DepreciationFitReport {
        applied: apply,
        value_reference_year,
        min_model_samples,
        generic_profile: Some(generic),
        model_profiles: model_reports,
        assigned_model_profile_count,
        assigned_generic_profile_count,
        sample_count: samples.len(),
    })
}

pub async fn fit_depreciation_profile_for_model(
    db: &AppDb,
    aircraft_model_id: i64,
    apply: bool,
    min_model_samples: usize,
    value_reference_year: Option<i64>,
) -> FitResult<DepreciationFitReport> {
    let min_model_samples = min_model_samples.max(2);
    let value_reference_year = value_reference_year.unwrap_or(2026);
    let samples = fit_samples(db, value_reference_year).await?;
    if samples.len() < 2 {
        return Ok(DepreciationFitReport {
            applied: apply,
            value_reference_year,
            min_model_samples,
            generic_profile: None,
            model_profiles: Vec::new(),
            assigned_model_profile_count: 0,
            assigned_generic_profile_count: 0,
            sample_count: samples.len(),
        });
    }

    let generic = fit_scope(
        "global",
        "all",
        "generic:all",
        &samples,
        value_reference_year,
    )?;
    let model_samples = samples
        .iter()
        .filter(|sample| sample.model_id == aircraft_model_id)
        .cloned()
        .collect::<Vec<_>>();
    let model_report = if model_samples.len() >= min_model_samples {
        Some(fit_scope(
            "model",
            &aircraft_model_id.to_string(),
            &format!("model:{aircraft_model_id}"),
            &model_samples,
            value_reference_year,
        )?)
    } else {
        None
    };
    let model_profiles = model_report.iter().cloned().collect::<Vec<_>>();

    let mut assigned_model_profile_count = 0;
    let mut assigned_generic_profile_count = 0;
    if apply {
        save_fitted_profile(db, &generic, "global", "all", "all").await?;
        save_component_baseline(
            db,
            "engine",
            generic.engine_baseline_life_fraction,
            &generic,
        )
        .await?;
        save_component_baseline(
            db,
            "propeller",
            generic.propeller_baseline_life_fraction,
            &generic,
        )
        .await?;

        let profile_id = if let Some(report) = &model_report {
            save_fitted_profile(db, report, "model", &report.scope_key, "all").await?;
            assigned_model_profile_count += 1;
            profile_id_by_name(db, &report.profile_name).await?
        } else {
            assigned_generic_profile_count += 1;
            profile_id_by_name(db, "generic:all").await?
        };
        assign_profile_to_model_specs(db, aircraft_model_id, profile_id).await?;
    }

    Ok(DepreciationFitReport {
        applied: apply,
        value_reference_year,
        min_model_samples,
        generic_profile: Some(generic),
        model_profiles,
        assigned_model_profile_count,
        assigned_generic_profile_count,
        sample_count: samples.len(),
    })
}

fn fit_scope(
    scope: &str,
    scope_key: &str,
    profile_name: &str,
    samples: &[FitSample],
    valuation_year: i64,
) -> FitResult<FittedProfileReport> {
    let mut best: Option<(FitCandidate, f64, f64)> = None;
    for age_decay_rate in [0.015, 0.025, 0.035, 0.045, 0.060, 0.080, 0.105] {
        for long_run_residual_fraction in [0.12, 0.18, 0.24, 0.30, 0.38, 0.46, 0.56] {
            for new_to_used_discount_fraction in [0.0, 0.04, 0.08, 0.12, 0.16] {
                for component_baseline in [0.35, 0.45, 0.50, 0.60, 0.70] {
                    for replacement_floor_fraction in [0.0, 0.12, 0.16, 0.20, 0.24, 0.28, 0.32] {
                        for airframe in airframe_fit_candidates() {
                            let candidate = FitCandidate {
                                profile: AircraftProfile {
                                    name: profile_name.to_string(),
                                    age_decay_rate,
                                    long_run_residual_fraction,
                                    new_to_used_discount_fraction,
                                    new_to_used_discount_years: 1.0,
                                    airframe_doubling_discount: airframe.airframe_doubling_discount,
                                    max_airframe_premium: airframe.max_airframe_premium,
                                    max_airframe_discount: airframe.max_airframe_discount,
                                    replacement_floor_fraction,
                                    minimum_value_fraction: 0.05,
                                    high_time_threshold_hours: airframe.high_time_threshold_hours,
                                    high_time_discount_at_double_threshold: airframe
                                        .high_time_discount_at_double_threshold,
                                },
                                engine_baseline_life_fraction: component_baseline,
                                propeller_baseline_life_fraction: component_baseline,
                            };
                            let (rmse, mae_fraction) =
                                score_candidate(&candidate, samples, valuation_year)?;
                            let score = mae_fraction;
                            if best
                                .as_ref()
                                .map(|(_, _, best_mae)| score < *best_mae)
                                .unwrap_or(true)
                            {
                                best = Some((candidate, rmse, mae_fraction));
                            }
                        }
                    }
                }
            }
        }
    }
    let Some((best, rmse_usd, mae_fraction)) = best else {
        return Err(FitError::Model(
            "no depreciation fit candidates produced".to_string(),
        ));
    };
    Ok(FittedProfileReport {
        scope: scope.to_string(),
        scope_key: scope_key.to_string(),
        profile_name: profile_name.to_string(),
        sample_count: samples.len(),
        rmse_usd,
        mae_fraction,
        age_decay_rate: best.profile.age_decay_rate,
        long_run_residual_fraction: best.profile.long_run_residual_fraction,
        new_to_used_discount_fraction: best.profile.new_to_used_discount_fraction,
        airframe_doubling_discount: best.profile.airframe_doubling_discount,
        max_airframe_premium: best.profile.max_airframe_premium,
        max_airframe_discount: best.profile.max_airframe_discount,
        replacement_floor_fraction: best.profile.replacement_floor_fraction,
        high_time_threshold_hours: best.profile.high_time_threshold_hours,
        high_time_discount_at_double_threshold: best.profile.high_time_discount_at_double_threshold,
        engine_baseline_life_fraction: best.engine_baseline_life_fraction,
        propeller_baseline_life_fraction: best.propeller_baseline_life_fraction,
    })
}

fn airframe_fit_candidates() -> &'static [AirframeFitParams] {
    &[
        AirframeFitParams {
            airframe_doubling_discount: 0.08,
            max_airframe_premium: 0.08,
            max_airframe_discount: 0.20,
            high_time_threshold_hours: None,
            high_time_discount_at_double_threshold: 0.0,
        },
        AirframeFitParams {
            airframe_doubling_discount: 0.13,
            max_airframe_premium: 0.12,
            max_airframe_discount: 0.25,
            high_time_threshold_hours: Some(10_000.0),
            high_time_discount_at_double_threshold: 0.10,
        },
        AirframeFitParams {
            airframe_doubling_discount: 0.15,
            max_airframe_premium: 0.12,
            max_airframe_discount: 0.30,
            high_time_threshold_hours: None,
            high_time_discount_at_double_threshold: 0.0,
        },
        AirframeFitParams {
            airframe_doubling_discount: 0.15,
            max_airframe_premium: 0.12,
            max_airframe_discount: 0.30,
            high_time_threshold_hours: Some(10_000.0),
            high_time_discount_at_double_threshold: 0.12,
        },
        AirframeFitParams {
            airframe_doubling_discount: 0.18,
            max_airframe_premium: 0.06,
            max_airframe_discount: 0.45,
            high_time_threshold_hours: Some(6_000.0),
            high_time_discount_at_double_threshold: 0.12,
        },
        AirframeFitParams {
            airframe_doubling_discount: 0.20,
            max_airframe_premium: 0.18,
            max_airframe_discount: 0.40,
            high_time_threshold_hours: Some(8_000.0),
            high_time_discount_at_double_threshold: 0.12,
        },
        AirframeFitParams {
            airframe_doubling_discount: 0.22,
            max_airframe_premium: 0.10,
            max_airframe_discount: 0.40,
            high_time_threshold_hours: Some(8_000.0),
            high_time_discount_at_double_threshold: 0.15,
        },
        AirframeFitParams {
            airframe_doubling_discount: 0.22,
            max_airframe_premium: 0.10,
            max_airframe_discount: 0.45,
            high_time_threshold_hours: Some(6_000.0),
            high_time_discount_at_double_threshold: 0.18,
        },
        AirframeFitParams {
            airframe_doubling_discount: 0.30,
            max_airframe_premium: 0.12,
            max_airframe_discount: 0.50,
            high_time_threshold_hours: Some(6_000.0),
            high_time_discount_at_double_threshold: 0.20,
        },
    ]
}

fn score_candidate(
    candidate: &FitCandidate,
    samples: &[FitSample],
    valuation_year: i64,
) -> FitResult<(f64, f64)> {
    let mut squared_error_sum = 0.0;
    let mut absolute_fraction_sum = 0.0;
    let mut count = 0.0;
    for sample in samples {
        let estimated = estimate_sample_value(candidate, sample, valuation_year)?;
        let error = estimated - sample.asking_price_usd;
        squared_error_sum += error * error;
        absolute_fraction_sum += (error / sample.asking_price_usd.max(1.0)).abs();
        count += 1.0;
    }
    Ok((
        (squared_error_sum / count).sqrt(),
        absolute_fraction_sum / count,
    ))
}

fn estimate_sample_value(
    candidate: &FitCandidate,
    sample: &FitSample,
    valuation_year: i64,
) -> FitResult<f64> {
    let age_years = (valuation_year - sample.model_year).max(0) as f64;
    let engine = sample
        .engine_tbo_hours
        .zip(sample.engine_overhaul_cost_usd)
        .map(|(tbo_hours, overhaul_cost_usd)| TimedComponent {
            name: "engine".to_string(),
            hours_since_overhaul: sample.engine_hours,
            tbo_hours,
            overhaul_cost_usd,
            value_reference_year: sample
                .engine_overhaul_cost_value_reference_year
                .unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR),
            valuation_year,
            average_inflation_rate: sample.average_inflation_rate,
            count: sample.engine_count,
            baseline_life_fraction: candidate.engine_baseline_life_fraction,
        });
    let propeller = sample
        .propeller_tbo_hours
        .zip(sample.propeller_overhaul_cost_usd)
        .map(|(tbo_hours, overhaul_cost_usd)| TimedComponent {
            name: "propeller".to_string(),
            hours_since_overhaul: sample.propeller_hours,
            tbo_hours,
            overhaul_cost_usd,
            value_reference_year: sample
                .propeller_overhaul_cost_value_reference_year
                .unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR),
            valuation_year,
            average_inflation_rate: sample.average_inflation_rate,
            count: sample.propeller_count,
            baseline_life_fraction: candidate.propeller_baseline_life_fraction,
        });
    let avionics_profile = default_avionics_profile();
    let avionics = sample
        .avionics
        .iter()
        .map(|component| AvionicsComponent {
            name: component.name.clone(),
            introduced_year: component.introduced_year,
            valuation_year,
            value_reference_year: component.value_reference_year,
            average_inflation_rate: sample.average_inflation_rate,
            unit_replacement_cost_usd: component.estimated_unit_value_usd,
            quantity: component.quantity,
            profile: avionics_profile.clone(),
        })
        .collect::<Vec<_>>();
    estimate_aircraft_value_in_year(
        sample.purchase_price_new_usd,
        age_years,
        sample.airframe_hours,
        DEFAULT_ANNUAL_AIRFRAME_HOURS,
        candidate.profile.clone(),
        engine,
        propeller,
        &avionics,
        sample.replacement_floor_basis_usd,
        DollarBasis {
            value_reference_year: sample.value_reference_year,
            valuation_year,
            average_inflation_rate: sample.average_inflation_rate,
        },
    )
    .map(|estimate| estimate.estimated_value_usd)
    .map_err(FitError::Model)
}

async fn fit_samples(db: &AppDb, valuation_year: i64) -> FitResult<Vec<FitSample>> {
    let rows = query_as_all!(
        db,
        FitListingRow,
        r#"
        SELECT
          listing.id AS listing_id,
          model.id AS aircraft_model_id,
          listing.model_year,
          listing.asking_price_usd,
          listing.airframe_hours,
          listing.engine_hours,
          listing.propeller_hours,
          price_point.purchase_price_new_usd,
          price_point.purchase_price_reference_year,
          spec.average_inflation_rate,
          spec.engine_count,
          COALESCE(spec.engine_tbo_hours, engine_model.tbo_hours) AS engine_tbo_hours,
          COALESCE(spec.engine_overhaul_cost_usd, engine_model.overhaul_cost_usd) AS engine_overhaul_cost_usd,
          engine_model.value_reference_year AS engine_overhaul_cost_value_reference_year,
          spec.propeller_count,
          COALESCE(spec.propeller_tbo_hours, prop_model.tbo_hours) AS propeller_tbo_hours,
          COALESCE(spec.propeller_overhaul_cost_usd, prop_model.overhaul_cost_usd) AS propeller_overhaul_cost_usd,
          prop_model.value_reference_year AS propeller_overhaul_cost_value_reference_year
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_model_spec_versions spec
          ON spec.aircraft_model_id = model.id
         AND spec.aircraft_model_variant_id = variant.id
        LEFT JOIN engine_models engine_model
          ON engine_model.id = spec.engine_model_id
        LEFT JOIN propeller_models prop_model
          ON prop_model.id = spec.propeller_model_id
        JOIN aircraft_model_variant_price_points price_point
          ON price_point.aircraft_model_variant_id = variant.id
         AND price_point.model_year = listing.model_year
        WHERE listing.asking_price_usd > 0
          AND spec.id = (
            SELECT latest.id
            FROM aircraft_model_spec_versions latest
            WHERE latest.aircraft_model_id = model.id
              AND latest.aircraft_model_variant_id = variant.id
            ORDER BY latest.effective_from DESC, latest.id DESC
            LIMIT 1
          )
        ORDER BY listing.id
        "#
    )?;
    let avionics = fit_avionics_rows(db).await?;
    let mut avionics_by_listing: BTreeMap<i64, Vec<FitAvionicsComponent>> = BTreeMap::new();
    for row in avionics {
        let Some(introduced_year) = row.introduced_year else {
            continue;
        };
        let Some(estimated_unit_value_usd) = row.estimated_unit_value_usd else {
            continue;
        };
        avionics_by_listing
            .entry(row.listing_id)
            .or_default()
            .push(FitAvionicsComponent {
                name: format!("{} {} {}", row.manufacturer, row.model, row.avionics_type),
                quantity: row.quantity.max(1),
                introduced_year,
                estimated_unit_value_usd,
                value_reference_year: row.value_reference_year.unwrap_or(2026),
            });
    }
    let default_avionics = fit_default_avionics_rows(db).await?;
    let mut default_avionics_by_listing: BTreeMap<i64, Vec<FitAvionicsComponent>> = BTreeMap::new();
    for row in default_avionics {
        let Some(introduced_year) = row.introduced_year else {
            continue;
        };
        let Some(estimated_unit_value_usd) = row.estimated_unit_value_usd else {
            continue;
        };
        default_avionics_by_listing
            .entry(row.listing_id)
            .or_default()
            .push(FitAvionicsComponent {
                name: format!("{} {} {}", row.manufacturer, row.model, row.avionics_type),
                quantity: row.quantity.max(1),
                introduced_year,
                estimated_unit_value_usd,
                value_reference_year: row.value_reference_year.unwrap_or(2026),
            });
    }
    let replacement_basis_by_model = fit_replacement_basis_by_model(db, valuation_year).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let listing_avionics = avionics_by_listing
                .remove(&row.listing_id)
                .unwrap_or_default();
            let default_avionics = default_avionics_by_listing
                .remove(&row.listing_id)
                .unwrap_or_default();
            let avionics = if listing_avionics.is_empty() {
                default_avionics.clone()
            } else {
                listing_avionics
            };
            let value_reference_year = row.purchase_price_reference_year;
            let purchase_price_new_usd = fit_airframe_basis_excluding_default_avionics(
                row.purchase_price_new_usd,
                &default_avionics,
                value_reference_year,
                row.average_inflation_rate,
            );
            FitSample {
                model_id: row.aircraft_model_id,
                model_year: row.model_year,
                asking_price_usd: row.asking_price_usd,
                airframe_hours: row.airframe_hours,
                engine_hours: row.engine_hours,
                propeller_hours: row.propeller_hours,
                value_reference_year,
                purchase_price_new_usd,
                replacement_floor_basis_usd: replacement_basis_by_model
                    .get(&row.aircraft_model_id)
                    .copied(),
                average_inflation_rate: row.average_inflation_rate,
                engine_count: row.engine_count,
                engine_tbo_hours: row.engine_tbo_hours,
                engine_overhaul_cost_usd: row.engine_overhaul_cost_usd,
                engine_overhaul_cost_value_reference_year: row
                    .engine_overhaul_cost_value_reference_year,
                propeller_count: row.propeller_count,
                propeller_tbo_hours: row.propeller_tbo_hours,
                propeller_overhaul_cost_usd: row.propeller_overhaul_cost_usd,
                propeller_overhaul_cost_value_reference_year: row
                    .propeller_overhaul_cost_value_reference_year,
                avionics,
            }
        })
        .collect())
}

fn fit_airframe_basis_excluding_default_avionics(
    model_year_purchase_price_new_usd: f64,
    default_avionics: &[FitAvionicsComponent],
    purchase_price_reference_year: i64,
    average_inflation_rate: f64,
) -> f64 {
    let profile = default_avionics_profile();
    let components = default_avionics
        .iter()
        .map(|component| AvionicsComponent {
            name: component.name.clone(),
            introduced_year: component.introduced_year,
            valuation_year: purchase_price_reference_year,
            value_reference_year: component.value_reference_year,
            average_inflation_rate,
            unit_replacement_cost_usd: component.estimated_unit_value_usd,
            quantity: component.quantity,
            profile: profile.clone(),
        })
        .collect::<Vec<_>>();
    let default_avionics_basis = avionics_replacement_basis(&components).unwrap_or_default();
    (model_year_purchase_price_new_usd - default_avionics_basis)
        .max(model_year_purchase_price_new_usd * 0.2)
}

async fn fit_replacement_basis_by_model(
    db: &AppDb,
    valuation_year: i64,
) -> FitResult<BTreeMap<i64, f64>> {
    let rows = query_as_all!(
        db,
        FitReplacementBasisRow,
        r#"
        SELECT
          model.id AS aircraft_model_id,
          price_point.purchase_price_new_usd,
          price_point.purchase_price_reference_year,
          spec.average_inflation_rate
        FROM aircraft_model_variant_price_points price_point
        JOIN aircraft_model_variants variant
          ON variant.id = price_point.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_model_spec_versions spec
          ON spec.aircraft_model_id = model.id
         AND spec.aircraft_model_variant_id = variant.id
        WHERE price_point.purchase_price_reference_year <= ?
          AND spec.id = (
            SELECT latest.id
            FROM aircraft_model_spec_versions latest
            WHERE latest.aircraft_model_id = model.id
              AND latest.aircraft_model_variant_id = variant.id
            ORDER BY latest.effective_from DESC, latest.id DESC
            LIMIT 1
          )
        "#,
        valuation_year
    )?;
    let mut by_model = BTreeMap::new();
    for row in rows {
        let basis = row.purchase_price_new_usd
            * nominal_dollar_factor(
                row.purchase_price_reference_year,
                valuation_year,
                row.average_inflation_rate,
            )
            .map_err(FitError::Model)?;
        by_model
            .entry(row.aircraft_model_id)
            .and_modify(|existing| {
                if basis > *existing {
                    *existing = basis;
                }
            })
            .or_insert(basis);
    }
    Ok(by_model)
}

async fn fit_avionics_rows(db: &AppDb) -> FitResult<Vec<FitAvionicsRow>> {
    Ok(query_as_all!(
        db,
        FitAvionicsRow,
        r#"
        SELECT
          link.aircraft_sale_listing_id AS listing_id,
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
        ORDER BY link.aircraft_sale_listing_id, link.id
        "#
    )?)
}

async fn fit_default_avionics_rows(db: &AppDb) -> FitResult<Vec<FitAvionicsRow>> {
    Ok(query_as_all!(
        db,
        FitAvionicsRow,
        r#"
        SELECT
          listing.id AS listing_id,
          mfr.name AS manufacturer,
          model.name AS model,
          avionics_type.name AS avionics_type,
          default_avionics.quantity,
          model.introduced_year,
          model.estimated_unit_value_usd,
          model.value_reference_year
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variant_default_avionics default_avionics
          ON default_avionics.aircraft_model_variant_id = listing.aircraft_model_variant_id
         AND default_avionics.model_year = listing.model_year
        JOIN avionics_models model
          ON model.id = default_avionics.avionics_model_id
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        JOIN avionics_types avionics_type
          ON avionics_type.id = model.avionics_type_id
        ORDER BY listing.id, default_avionics.id
        "#
    )?)
}

async fn save_fitted_profile(
    db: &AppDb,
    report: &FittedProfileReport,
    fit_scope: &str,
    fit_scope_key: &str,
    fit_category: &str,
) -> FitResult<()> {
    let profile = AircraftProfile {
        name: report.profile_name.clone(),
        age_decay_rate: report.age_decay_rate,
        long_run_residual_fraction: report.long_run_residual_fraction,
        new_to_used_discount_fraction: report.new_to_used_discount_fraction,
        new_to_used_discount_years: 1.0,
        airframe_doubling_discount: report.airframe_doubling_discount,
        max_airframe_premium: report.max_airframe_premium,
        max_airframe_discount: report.max_airframe_discount,
        replacement_floor_fraction: report.replacement_floor_fraction,
        minimum_value_fraction: 0.05,
        high_time_threshold_hours: report.high_time_threshold_hours,
        high_time_discount_at_double_threshold: report.high_time_discount_at_double_threshold,
    };
    let profile_id = upsert_profile(db, &profile).await?;
    execute_query!(
        db,
        r#"
        INSERT INTO depreciation_profile_fit_metadata (
          depreciation_profile_id,
          fit_scope,
          fit_scope_key,
          fit_category,
          sample_count,
          rmse_usd,
          mae_fraction
        )
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (fit_scope, fit_scope_key) DO UPDATE SET
          depreciation_profile_id = excluded.depreciation_profile_id,
          fit_category = excluded.fit_category,
          sample_count = excluded.sample_count,
          rmse_usd = excluded.rmse_usd,
          mae_fraction = excluded.mae_fraction,
          updated_at = CURRENT_TIMESTAMP
        "#,
        profile_id,
        fit_scope,
        fit_scope_key,
        fit_category,
        report.sample_count as i64,
        report.rmse_usd,
        report.mae_fraction
    )?;
    Ok(())
}

async fn upsert_profile(db: &AppDb, profile: &AircraftProfile) -> FitResult<i64> {
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        INSERT INTO depreciation_profiles (
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
          high_time_discount_at_double_threshold,
          is_system_profile
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, FALSE)
        ON CONFLICT (name) DO UPDATE SET
          age_decay_rate = excluded.age_decay_rate,
          long_run_residual_fraction = excluded.long_run_residual_fraction,
          new_to_used_discount_fraction = excluded.new_to_used_discount_fraction,
          new_to_used_discount_years = excluded.new_to_used_discount_years,
          airframe_doubling_discount = excluded.airframe_doubling_discount,
          max_airframe_premium = excluded.max_airframe_premium,
          max_airframe_discount = excluded.max_airframe_discount,
          replacement_floor_fraction = excluded.replacement_floor_fraction,
          minimum_value_fraction = excluded.minimum_value_fraction,
          high_time_threshold_hours = excluded.high_time_threshold_hours,
          high_time_discount_at_double_threshold = excluded.high_time_discount_at_double_threshold,
          is_system_profile = FALSE,
          updated_at = CURRENT_TIMESTAMP
        RETURNING id
        "#,
        profile.name.as_str(),
        profile.age_decay_rate,
        profile.long_run_residual_fraction,
        profile.new_to_used_discount_fraction,
        profile.new_to_used_discount_years,
        profile.airframe_doubling_discount,
        profile.max_airframe_premium,
        profile.max_airframe_discount,
        profile.replacement_floor_fraction,
        profile.minimum_value_fraction,
        profile.high_time_threshold_hours,
        profile.high_time_discount_at_double_threshold,
    )?)
}

async fn save_component_baseline(
    db: &AppDb,
    component_type: &str,
    baseline_life_fraction: f64,
    report: &FittedProfileReport,
) -> FitResult<()> {
    execute_query!(
        db,
        r#"
        INSERT INTO component_depreciation_profiles (
          component_type,
          baseline_life_fraction,
          sample_count,
          rmse_usd,
          mae_fraction
        )
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT (component_type) DO UPDATE SET
          baseline_life_fraction = excluded.baseline_life_fraction,
          sample_count = excluded.sample_count,
          rmse_usd = excluded.rmse_usd,
          mae_fraction = excluded.mae_fraction,
          updated_at = CURRENT_TIMESTAMP
        "#,
        component_type,
        baseline_life_fraction,
        report.sample_count as i64,
        report.rmse_usd,
        report.mae_fraction,
    )?;
    Ok(())
}

async fn profile_id_by_name(db: &AppDb, name: &str) -> FitResult<i64> {
    Ok(query_scalar_one!(
        db,
        i64,
        "SELECT id FROM depreciation_profiles WHERE name = ?",
        name
    )?)
}

async fn assign_profile_to_model_specs(
    db: &AppDb,
    model_id: i64,
    profile_id: i64,
) -> FitResult<usize> {
    execute_query!(
        db,
        r#"
        UPDATE aircraft_model_spec_versions
        SET depreciation_profile_id = ?,
            engine_value_baseline_life_fraction = (
              SELECT COALESCE(baseline_life_fraction, engine_value_baseline_life_fraction)
              FROM component_depreciation_profiles
              WHERE component_type = 'engine'
            ),
            propeller_value_baseline_life_fraction = (
              SELECT COALESCE(baseline_life_fraction, propeller_value_baseline_life_fraction)
              FROM component_depreciation_profiles
              WHERE component_type = 'propeller'
            ),
            updated_at = CURRENT_TIMESTAMP
        WHERE aircraft_model_id = ?
        "#,
        profile_id,
        model_id
    )?;
    Ok(1)
}

async fn assign_profile_to_unfit_specs(
    db: &AppDb,
    generic_profile_id: i64,
    min_model_samples: i64,
) -> FitResult<usize> {
    execute_query!(
        db,
        r#"
        UPDATE aircraft_model_spec_versions
        SET depreciation_profile_id = ?,
            engine_value_baseline_life_fraction = (
              SELECT COALESCE(baseline_life_fraction, engine_value_baseline_life_fraction)
              FROM component_depreciation_profiles
              WHERE component_type = 'engine'
            ),
            propeller_value_baseline_life_fraction = (
              SELECT COALESCE(baseline_life_fraction, propeller_value_baseline_life_fraction)
              FROM component_depreciation_profiles
              WHERE component_type = 'propeller'
            ),
            updated_at = CURRENT_TIMESTAMP
        WHERE aircraft_model_id NOT IN (
          SELECT model.id
          FROM aircraft_models model
          JOIN aircraft_model_variants variant
            ON variant.aircraft_model_id = model.id
          JOIN aircraft_sale_listings listing
            ON listing.aircraft_model_variant_id = variant.id
          GROUP BY model.id
          HAVING COUNT(listing.id) >= ?
        )
        "#,
        generic_profile_id,
        min_model_samples
    )?;
    Ok(1)
}
