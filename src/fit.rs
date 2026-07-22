use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;
use sqlx::FromRow;

use crate::aircraft::{
    resolve_avionics_configuration, AvionicsConfigurationLink, AvionicsSuiteMembership,
};
use crate::db::{AppDb, DatabaseBackend};
use crate::depreciation::{
    avionics_replacement_basis, default_avionics_profile, estimate_aircraft_value_in_year,
    AircraftProfile, AvionicsComponent, DollarBasis, TimedComponent, DEFAULT_ANNUAL_AIRFRAME_HOURS,
};
use crate::valuation::dataset::{load_snapshot, require_snapshot_faa_admission, sha256_hex};
use crate::valuation::store::persist_structural_candidate;
use crate::valuation::types::StructuralArtifactV1;
use crate::valuation::validation::{fit_validated_structural, ValidationReport};
use crate::valuation::{StructuralFitConfig, FEATURE_SCHEMA_VERSION};

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
    duplicate_group_key: String,
    aircraft_model_id: i64,
    model_year: i64,
    asking_price_usd: f64,
    airframe_hours: f64,
    engine_hours: Option<f64>,
    engine_time_basis: String,
    engine_time_confidence: Option<String>,
    propeller_hours: Option<f64>,
    propeller_time_basis: String,
    propeller_time_confidence: Option<String>,
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

#[derive(Debug, FromRow)]
struct FitSuiteRow {
    suite_model_id: i64,
    component_model_id: i64,
    quantity: i64,
}

#[derive(Clone)]
struct FitSample {
    duplicate_group_key: String,
    model_id: i64,
    model_year: i64,
    asking_price_usd: f64,
    airframe_hours: f64,
    engine_hours: Option<f64>,
    engine_time_basis: String,
    engine_time_confidence: Option<String>,
    propeller_hours: Option<f64>,
    propeller_time_basis: String,
    propeller_time_confidence: Option<String>,
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
    installed_value_contribution_usd: f64,
    replacement_cost_usd: f64,
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
    pub validation_method: String,
    pub out_of_fold_sample_count: usize,
    pub rmse_usd: f64,
    pub mae_fraction: f64,
    pub training_rmse_usd: f64,
    pub training_mae_fraction: f64,
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
    let min_model_samples = min_model_samples.max(3);
    let value_reference_year = value_reference_year.unwrap_or(2026);
    let (samples, snapshot_id) = fit_samples(db, value_reference_year).await?;
    if samples.len() < 3 {
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
        require_complete_grouped_validation(&generic)?;
        save_fitted_profile(db, &generic, "global", "all", "all").await?;
        for report in &model_reports {
            require_complete_grouped_validation(report)?;
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
        assigned_generic_profile_count = assign_profile_to_unfit_specs(
            db,
            generic_profile_id,
            min_model_samples as i64,
            snapshot_id,
        )
        .await?;
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
    let min_model_samples = min_model_samples.max(3);
    let value_reference_year = value_reference_year.unwrap_or(2026);
    let (samples, _snapshot_id) = fit_samples(db, value_reference_year).await?;
    if samples.len() < 3 {
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
        require_complete_grouped_validation(&generic)?;
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
            require_complete_grouped_validation(report)?;
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

fn require_complete_grouped_validation(report: &FittedProfileReport) -> FitResult<()> {
    if report.out_of_fold_sample_count != report.sample_count {
        return Err(FitError::Model(format!(
            "refusing to apply {}: grouped out-of-fold validation covered {} of {} samples",
            report.profile_name, report.out_of_fold_sample_count, report.sample_count
        )));
    }
    Ok(())
}

fn fit_scope(
    scope: &str,
    scope_key: &str,
    profile_name: &str,
    samples: &[FitSample],
    valuation_year: i64,
) -> FitResult<FittedProfileReport> {
    let (best, training_rmse_usd, training_mae_fraction) =
        select_best_candidate(profile_name, samples, valuation_year)?;
    let out_of_fold = grouped_out_of_fold_score(profile_name, samples, valuation_year)?;
    let (validation_method, out_of_fold_sample_count, rmse_usd, mae_fraction) =
        if let Some((count, rmse, mae)) = out_of_fold {
            ("grouped-out-of-fold".to_string(), count, rmse, mae)
        } else {
            (
                "training-only-insufficient-groups".to_string(),
                0,
                training_rmse_usd,
                training_mae_fraction,
            )
        };
    Ok(FittedProfileReport {
        scope: scope.to_string(),
        scope_key: scope_key.to_string(),
        profile_name: profile_name.to_string(),
        sample_count: samples.len(),
        validation_method,
        out_of_fold_sample_count,
        rmse_usd,
        mae_fraction,
        training_rmse_usd,
        training_mae_fraction,
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

fn select_best_candidate(
    profile_name: &str,
    samples: &[FitSample],
    valuation_year: i64,
) -> FitResult<(FitCandidate, f64, f64)> {
    let mut best: Option<(FitCandidate, f64, f64)> = None;
    for age_decay_rate in [0.015, 0.025, 0.035, 0.045, 0.060, 0.080, 0.105] {
        for long_run_residual_fraction in [0.12, 0.18, 0.24, 0.30, 0.38, 0.46, 0.56] {
            for new_to_used_discount_fraction in [0.0, 0.04, 0.08, 0.12, 0.16] {
                for component_baseline in [0.35, 0.45, 0.50, 0.60, 0.70] {
                    // The old family-wide floor was based on the most expensive
                    // price point in a broad model family. Keep it disabled: it
                    // is not a generation-specific current-market anchor.
                    for replacement_floor_fraction in [0.0] {
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
    Ok((best, rmse_usd, mae_fraction))
}

fn grouped_out_of_fold_score(
    profile_name: &str,
    samples: &[FitSample],
    valuation_year: i64,
) -> FitResult<Option<(usize, f64, f64)>> {
    let group_keys = samples
        .iter()
        .map(|sample| sample.duplicate_group_key.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if group_keys.len() < 3 {
        return Ok(None);
    }

    let fold_count = if group_keys.len() < 10 {
        group_keys.len()
    } else {
        5
    };
    let mut squared_error_sum = 0.0;
    let mut absolute_fraction_sum = 0.0;
    let mut prediction_count = 0_usize;
    for fold_index in 0..fold_count {
        let held_out_groups = group_keys
            .iter()
            .enumerate()
            .filter(|(index, _)| index % fold_count == fold_index)
            .map(|(_, key)| key)
            .collect::<BTreeSet<_>>();
        let training = samples
            .iter()
            .filter(|sample| !held_out_groups.contains(&sample.duplicate_group_key))
            .cloned()
            .collect::<Vec<_>>();
        let held_out = samples
            .iter()
            .filter(|sample| held_out_groups.contains(&sample.duplicate_group_key))
            .collect::<Vec<_>>();
        if training.len() < 2 || held_out.is_empty() {
            continue;
        }
        let fold_profile_name = format!("{profile_name}:oof:{fold_index}");
        let (candidate, _, _) =
            select_best_candidate(&fold_profile_name, &training, valuation_year)?;
        for sample in held_out {
            let estimated = estimate_sample_value(&candidate, sample, valuation_year)?;
            let error = estimated - sample.asking_price_usd;
            squared_error_sum += error * error;
            absolute_fraction_sum += (error / sample.asking_price_usd.max(1.0)).abs();
            prediction_count += 1;
        }
    }
    if prediction_count == 0 {
        return Ok(None);
    }
    Ok(Some((
        prediction_count,
        (squared_error_sum / prediction_count as f64).sqrt(),
        absolute_fraction_sum / prediction_count as f64,
    )))
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
        .zip(known_component_time(
            sample.engine_hours,
            &sample.engine_time_basis,
            sample.engine_time_confidence.as_deref(),
        ))
        .map(
            |((tbo_hours, overhaul_cost_usd), hours_since_overhaul)| TimedComponent {
                name: "engine".to_string(),
                hours_since_overhaul,
                tbo_hours,
                overhaul_cost_usd,
                value_reference_year: sample
                    .engine_overhaul_cost_value_reference_year
                    .unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR),
                valuation_year,
                average_inflation_rate: sample.average_inflation_rate,
                count: sample.engine_count,
                baseline_life_fraction: candidate.engine_baseline_life_fraction,
            },
        );
    let propeller = sample
        .propeller_tbo_hours
        .zip(sample.propeller_overhaul_cost_usd)
        .zip(known_component_time(
            sample.propeller_hours,
            &sample.propeller_time_basis,
            sample.propeller_time_confidence.as_deref(),
        ))
        .map(
            |((tbo_hours, overhaul_cost_usd), hours_since_overhaul)| TimedComponent {
                name: "propeller".to_string(),
                hours_since_overhaul,
                tbo_hours,
                overhaul_cost_usd,
                value_reference_year: sample
                    .propeller_overhaul_cost_value_reference_year
                    .unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR),
                valuation_year,
                average_inflation_rate: sample.average_inflation_rate,
                count: sample.propeller_count,
                baseline_life_fraction: candidate.propeller_baseline_life_fraction,
            },
        );
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
            installed_value_contribution_usd: component.installed_value_contribution_usd,
            replacement_cost_usd: component.replacement_cost_usd,
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

fn known_component_time(hours: Option<f64>, basis: &str, confidence: Option<&str>) -> Option<f64> {
    if !matches!(confidence, Some("high" | "medium")) {
        return None;
    }
    match basis {
        "SNEW" | "SMOH" | "SFOH" | "SPOH" => {
            hours.filter(|hours| hours.is_finite() && *hours >= 0.0)
        }
        _ => None,
    }
}

async fn fit_samples(db: &AppDb, _valuation_year: i64) -> FitResult<(Vec<FitSample>, i64)> {
    let snapshot_id = newest_snapshot_id(db).await?;
    require_snapshot_faa_admission(db, snapshot_id)
        .await
        .map_err(|error| FitError::Model(error.to_string()))?;
    let rows = query_as_all!(
        db,
        FitListingRow,
        r#"
        SELECT
          listing.id AS listing_id,
          snapshot_row.duplicate_group_key,
          model.id AS aircraft_model_id,
          listing.model_year,
          listing.asking_price_usd,
          listing.airframe_hours,
          listing.engine_hours,
          listing.engine_time_basis,
          listing.engine_time_confidence,
          listing.propeller_hours,
          listing.propeller_time_basis,
          listing.propeller_time_confidence,
          price_point.purchase_price_new_usd,
          price_point.purchase_price_reference_year,
          spec.average_inflation_rate,
          spec.engine_count,
          COALESCE(installed_engine.tbo_hours, spec.engine_tbo_hours, engine_model.tbo_hours) AS engine_tbo_hours,
          COALESCE(installed_engine.overhaul_cost_usd, spec.engine_overhaul_cost_usd, engine_model.overhaul_cost_usd) AS engine_overhaul_cost_usd,
          COALESCE(installed_engine.value_reference_year, engine_model.value_reference_year) AS engine_overhaul_cost_value_reference_year,
          spec.propeller_count,
          COALESCE(installed_propeller.tbo_hours, spec.propeller_tbo_hours, prop_model.tbo_hours) AS propeller_tbo_hours,
          COALESCE(installed_propeller.overhaul_cost_usd, spec.propeller_overhaul_cost_usd, prop_model.overhaul_cost_usd) AS propeller_overhaul_cost_usd,
          COALESCE(installed_propeller.value_reference_year, prop_model.value_reference_year) AS propeller_overhaul_cost_value_reference_year
        FROM aircraft_sale_listings listing
        JOIN valuation_snapshot_rows snapshot_row
          ON snapshot_row.source_listing_id = listing.id
         AND snapshot_row.inclusion_flag = TRUE
        JOIN valuation_snapshots snapshot
          ON snapshot.id = snapshot_row.snapshot_id
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_model_spec_versions spec
          ON spec.aircraft_model_id = model.id
         AND spec.aircraft_model_variant_id = variant.id
        LEFT JOIN engine_models engine_model
          ON engine_model.id = spec.engine_model_id
        LEFT JOIN engine_models installed_engine
          ON installed_engine.id = listing.installed_engine_model_id
         AND listing.installed_engine_confidence = 'high'
         AND installed_engine.source_confidence = 'high'
         AND installed_engine.evidence_kind = 'authoritative_reference'
         AND installed_engine.is_valuation_eligible = TRUE
        LEFT JOIN propeller_models prop_model
          ON prop_model.id = spec.propeller_model_id
        LEFT JOIN propeller_models installed_propeller
          ON installed_propeller.id = listing.installed_propeller_model_id
         AND listing.installed_propeller_confidence = 'high'
         AND installed_propeller.source_confidence = 'high'
         AND installed_propeller.evidence_kind = 'authoritative_reference'
         AND installed_propeller.is_valuation_eligible = TRUE
        JOIN aircraft_model_variant_price_points price_point
          ON price_point.aircraft_model_variant_id = variant.id
         AND price_point.model_year = listing.model_year
        WHERE snapshot.id = ?
          AND listing.ingestion_state = 'ready'
          AND listing.status = 'active'
          AND listing.currency = 'USD'
          AND listing.asking_price_usd > 0
          AND price_point.is_valuation_eligible = TRUE
          AND price_point.evidence_kind = 'direct_model_year'
          AND price_point.source_confidence = 'high'
          AND spec.configuration_scope = 'factory_default'
          AND spec.is_valuation_eligible = TRUE
          AND spec.source_confidence = 'high'
          AND spec.evidence_kind = 'authoritative_reference'
          AND snapshot.feature_schema_version = ?
          AND spec.id = (
            SELECT latest.id
            FROM aircraft_model_spec_versions latest
            WHERE latest.aircraft_model_id = model.id
              AND latest.aircraft_model_variant_id = variant.id
              AND latest.configuration_scope = 'factory_default'
              AND latest.is_valuation_eligible = TRUE
              AND latest.source_confidence = 'high'
              AND latest.evidence_kind = 'authoritative_reference'
            ORDER BY latest.effective_from DESC, latest.id DESC
            LIMIT 1
          )
        ORDER BY listing.id
        "#,
        snapshot_id,
        FEATURE_SCHEMA_VERSION as i64
    )?;
    let mut avionics_by_listing = group_fit_avionics_rows(fit_avionics_rows(db).await?);
    let mut default_avionics_by_listing =
        group_fit_avionics_rows(fit_default_avionics_rows(db).await?);
    let suite_memberships = fit_suite_memberships(db).await?;
    let samples = rows
        .into_iter()
        .map(|row| {
            let listing_avionics_rows = avionics_by_listing
                .remove(&row.listing_id)
                .unwrap_or_default();
            let default_avionics_rows = default_avionics_by_listing
                .remove(&row.listing_id)
                .unwrap_or_default();
            let default_avionics =
                effective_fit_avionics(&default_avionics_rows, &[], &suite_memberships);
            let avionics = effective_fit_avionics(
                &default_avionics_rows,
                &listing_avionics_rows,
                &suite_memberships,
            );
            let value_reference_year = row.purchase_price_reference_year;
            let purchase_price_new_usd = fit_airframe_basis_excluding_default_avionics(
                row.purchase_price_new_usd,
                &default_avionics,
                value_reference_year,
                row.average_inflation_rate,
            );
            FitSample {
                duplicate_group_key: row.duplicate_group_key,
                model_id: row.aircraft_model_id,
                model_year: row.model_year,
                asking_price_usd: row.asking_price_usd,
                airframe_hours: row.airframe_hours,
                engine_hours: row.engine_hours,
                engine_time_basis: row.engine_time_basis,
                engine_time_confidence: row.engine_time_confidence,
                propeller_hours: row.propeller_hours,
                propeller_time_basis: row.propeller_time_basis,
                propeller_time_confidence: row.propeller_time_confidence,
                value_reference_year,
                purchase_price_new_usd,
                replacement_floor_basis_usd: None,
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
        .collect();
    Ok((samples, snapshot_id))
}

async fn newest_snapshot_id(db: &AppDb) -> FitResult<i64> {
    let sql = "SELECT id FROM valuation_snapshots ORDER BY id DESC LIMIT 1";
    let snapshot_id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_optional(pool)
                .await?
        }
    };
    snapshot_id.ok_or_else(|| {
        FitError::Model(
            "FAA-admitted depreciation fitting requires a valuation snapshot".to_string(),
        )
    })
}

fn group_fit_avionics_rows(rows: Vec<FitAvionicsRow>) -> BTreeMap<i64, Vec<FitAvionicsRow>> {
    let mut grouped = BTreeMap::new();
    for row in rows {
        grouped
            .entry(row.listing_id)
            .or_insert_with(Vec::new)
            .push(row);
    }
    grouped
}

fn effective_fit_avionics(
    factory_defaults: &[FitAvionicsRow],
    listing_deltas: &[FitAvionicsRow],
    suite_memberships: &[AvionicsSuiteMembership],
) -> Vec<FitAvionicsComponent> {
    let to_link = |row: &FitAvionicsRow| AvionicsConfigurationLink {
        avionics_model_id: row.avionics_model_id,
        quantity: row.quantity,
        configuration_action: row.configuration_action.clone(),
        replaces_avionics_model_id: row.replaces_avionics_model_id,
        source_confidence: row.source_confidence.clone(),
        valuation_scope: row.valuation_scope.clone(),
    };
    let quantities = resolve_avionics_configuration(
        &factory_defaults.iter().map(to_link).collect::<Vec<_>>(),
        &listing_deltas.iter().map(to_link).collect::<Vec<_>>(),
        suite_memberships,
    );
    let rows_by_id = factory_defaults
        .iter()
        .chain(listing_deltas)
        .map(|row| (row.avionics_model_id, row))
        .collect::<BTreeMap<_, _>>();
    quantities
        .into_iter()
        .filter_map(|(avionics_model_id, quantity)| {
            let row = rows_by_id.get(&avionics_model_id)?;
            Some(FitAvionicsComponent {
                name: format!("{} {}", row.manufacturer, row.model),
                quantity,
                introduced_year: row.introduced_year?,
                installed_value_contribution_usd: row.installed_value_contribution_usd?,
                replacement_cost_usd: row.replacement_cost_usd?,
                value_reference_year: row.value_reference_year?,
            })
        })
        .collect()
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
            installed_value_contribution_usd: component.installed_value_contribution_usd,
            replacement_cost_usd: component.replacement_cost_usd,
            quantity: component.quantity,
            profile: profile.clone(),
        })
        .collect::<Vec<_>>();
    let default_avionics_basis = avionics_replacement_basis(&components).unwrap_or_default();
    (model_year_purchase_price_new_usd - default_avionics_basis)
        .max(model_year_purchase_price_new_usd * 0.2)
}

async fn fit_avionics_rows(db: &AppDb) -> FitResult<Vec<FitAvionicsRow>> {
    Ok(query_as_all!(
        db,
        FitAvionicsRow,
        r#"
        SELECT
          link.aircraft_sale_listing_id AS listing_id,
          model.id AS avionics_model_id,
          mfr.name AS manufacturer,
          model.name AS model,
          link.quantity,
          model.introduced_year,
          model.estimated_unit_value_usd AS installed_value_contribution_usd,
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
        WHERE link.source = 'listing'
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
          AND model.value_basis = 'installed_contribution'
          AND model.value_source IS NOT NULL
          AND TRIM(model.value_source) <> ''
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
          model.id AS avionics_model_id,
          mfr.name AS manufacturer,
          model.name AS model,
          default_avionics.quantity,
          model.introduced_year,
          model.estimated_unit_value_usd AS installed_value_contribution_usd,
          model.replacement_cost_usd,
          model.value_reference_year,
          model.valuation_scope,
          'installed' AS configuration_action,
          NULL AS replaces_avionics_model_id,
          default_avionics.source_confidence
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variant_default_avionics default_avionics
          ON default_avionics.aircraft_model_variant_id = listing.aircraft_model_variant_id
         AND default_avionics.model_year = listing.model_year
        JOIN avionics_models model
          ON model.id = default_avionics.avionics_model_id
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        WHERE default_avionics.source_confidence = 'high'
          AND model.catalog_status = 'approved'
          AND default_avionics.quantity > 0
          AND TRIM(default_avionics.source_url) <> ''
          AND LOWER(default_avionics.source_url) NOT LIKE '%/listing/%'
          AND LOWER(default_avionics.source_url) NOT LIKE '%/listings/%'
          AND LOWER(default_avionics.source_url) NOT LIKE '%/aircraft-for-sale/%'
          AND LOWER(default_avionics.source_url) NOT LIKE '%/classifieds/%'
          AND model.value_basis = 'installed_contribution'
          AND model.value_source IS NOT NULL
          AND TRIM(model.value_source) <> ''
        ORDER BY listing.id, default_avionics.id
        "#
    )?)
}

async fn fit_suite_memberships(db: &AppDb) -> FitResult<Vec<AvionicsSuiteMembership>> {
    let rows = query_as_all!(
        db,
        FitSuiteRow,
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
    snapshot_id: i64,
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
           AND listing.ingestion_state = 'ready'
           AND listing.status = 'active'
           AND listing.currency = 'USD'
          JOIN valuation_snapshot_rows snapshot_row
            ON snapshot_row.source_listing_id = listing.id
           AND snapshot_row.snapshot_id = ?
           AND snapshot_row.inclusion_flag = TRUE
          GROUP BY model.id
          HAVING COUNT(listing.id) >= ?
        )
        "#,
        generic_profile_id,
        snapshot_id,
        min_model_samples
    )?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::{
        fit_avionics_rows, fit_default_avionics_rows, fit_suite_memberships,
        grouped_out_of_fold_score, known_component_time, select_best_candidate, FitSample,
    };
    use crate::aircraft::AvionicsSuiteMembership;
    use crate::db::{AppDb, DatabaseBackend};

    async fn insert_fit_test_avionics_model(
        db: &AppDb,
        manufacturer_id: i64,
        type_id: i64,
        name: &str,
        normalized_name: &str,
        identifier: &str,
        approved: bool,
    ) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let model_id = if approved {
            let normalized_identifier = identifier
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            sqlx::query_scalar(
                r#"
                INSERT INTO avionics_models (
                  avionics_manufacturer_id, name, normalized_name,
                  manufacturer_identifier_kind, manufacturer_identifier,
                  normalized_manufacturer_identifier, identity_source_url,
                  identity_source_title, identity_evidence_text, identity_evidence_kind,
                  identity_confidence, catalog_reviewed_at, introduced_year,
                  estimated_unit_value_usd, value_basis, replacement_cost_usd,
                  value_reference_year, value_source
                ) VALUES (
                  ?, ?, ?, 'manufacturer_model_number', ?, ?,
                  'https://www.garmin.com/aviation/test-product/',
                  'Garmin test product',
                  'Manufacturer reference identifies this exact test product.',
                  'authoritative_reference', 'very_high', CURRENT_TIMESTAMP, 2020,
                  10000, 'installed_contribution', 20000, 2026,
                  'authoritative test fixture'
                ) RETURNING id
                "#,
            )
            .bind(manufacturer_id)
            .bind(name)
            .bind(normalized_name)
            .bind(identifier)
            .bind(normalized_identifier)
            .fetch_one(pool)
            .await
            .unwrap()
        } else {
            sqlx::query_scalar(
                r#"
                INSERT INTO avionics_models (
                  avionics_manufacturer_id, name, normalized_name,
                  introduced_year, estimated_unit_value_usd, value_basis,
                  replacement_cost_usd, value_reference_year, value_source
                ) VALUES (
                  ?, ?, ?, 2020, 99999, 'installed_contribution',
                  120000, 2026, 'legacy unreviewed fixture'
                ) RETURNING id
                "#,
            )
            .bind(manufacturer_id)
            .bind(name)
            .bind(normalized_name)
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
        if approved {
            sqlx::query("UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?")
                .bind(model_id)
                .execute(pool)
                .await
                .unwrap();
        }
        model_id
    }

    fn sample(group: &str, model_year: i64, asking_price_usd: f64) -> FitSample {
        FitSample {
            duplicate_group_key: group.to_string(),
            model_id: 1,
            model_year,
            asking_price_usd,
            airframe_hours: 2_000.0,
            engine_hours: None,
            engine_time_basis: "unknown".to_string(),
            engine_time_confidence: None,
            propeller_hours: None,
            propeller_time_basis: "unknown".to_string(),
            propeller_time_confidence: None,
            value_reference_year: model_year,
            purchase_price_new_usd: 100_000.0,
            replacement_floor_basis_usd: None,
            average_inflation_rate: 0.025,
            engine_count: 1,
            engine_tbo_hours: None,
            engine_overhaul_cost_usd: None,
            engine_overhaul_cost_value_reference_year: None,
            propeller_count: 1,
            propeller_tbo_hours: None,
            propeller_overhaul_cost_usd: None,
            propeller_overhaul_cost_value_reference_year: None,
            avionics: Vec::new(),
        }
    }

    #[test]
    fn legacy_component_adjustments_require_a_known_time_basis() {
        assert_eq!(
            known_component_time(Some(500.0), "SMOH", Some("high")),
            Some(500.0)
        );
        assert_eq!(
            known_component_time(Some(500.0), "SMOH", Some("medium")),
            Some(500.0)
        );
        assert_eq!(known_component_time(Some(500.0), "SMOH", Some("low")), None);
        assert_eq!(
            known_component_time(Some(500.0), "unknown", Some("high")),
            None
        );
        assert_eq!(known_component_time(None, "SNEW", Some("high")), None);
        assert_eq!(known_component_time(Some(-1.0), "SPOH", Some("high")), None);
    }

    #[test]
    fn legacy_fit_uses_grouped_out_of_fold_validation_and_no_family_floor() {
        let samples = vec![
            sample("serial:A", 1975, 120_000.0),
            sample("serial:B", 1985, 150_000.0),
            sample("serial:C", 1995, 190_000.0),
        ];
        let score = grouped_out_of_fold_score("test", &samples, 2026)
            .unwrap()
            .expect("three physical aircraft produce validation");
        assert_eq!(score.0, samples.len());
        assert!(score.1.is_finite());
        assert!(score.2.is_finite());

        let (candidate, _, _) = select_best_candidate("test", &samples, 2026).unwrap();
        assert_eq!(candidate.profile.replacement_floor_fraction, 0.0);
    }

    #[tokio::test]
    async fn legacy_unreviewed_avionics_are_excluded_from_fit_inputs() {
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
        let approved_id = insert_fit_test_avionics_model(
            &db,
            manufacturer_id,
            type_id,
            "Approved Display",
            "approved display",
            "APPROVED-DISPLAY-1",
            true,
        )
        .await;
        let approved_component_id = insert_fit_test_avionics_model(
            &db,
            manufacturer_id,
            type_id,
            "Approved Component",
            "approved component",
            "APPROVED-COMPONENT-1",
            true,
        )
        .await;
        let unreviewed_id = insert_fit_test_avionics_model(
            &db,
            manufacturer_id,
            type_id,
            "Legacy Guess",
            "legacy guess",
            "",
            false,
        )
        .await;

        for trigger in [
            "aircraft_sale_listing_avionics_approved_insert",
            "aircraft_model_variant_default_avionics_approved_insert",
            "avionics_suite_components_approved_insert",
        ] {
            sqlx::query(&format!("DROP TRIGGER {trigger}"))
                .execute(pool)
                .await
                .unwrap();
        }
        for model_id in [approved_id, unreviewed_id] {
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
        .bind(approved_id)
        .bind(approved_component_id)
        .bind(approved_id)
        .bind(unreviewed_id)
        .execute(pool)
        .await
        .unwrap();

        assert_eq!(
            fit_avionics_rows(&db)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.avionics_model_id)
                .collect::<Vec<_>>(),
            vec![approved_id]
        );
        assert_eq!(
            fit_default_avionics_rows(&db)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.avionics_model_id)
                .collect::<Vec<_>>(),
            vec![approved_id]
        );
        assert_eq!(
            fit_suite_memberships(&db).await.unwrap(),
            vec![AvionicsSuiteMembership {
                suite_model_id: approved_id,
                component_model_id: approved_component_id,
                quantity: 1,
            }]
        );
    }
}
