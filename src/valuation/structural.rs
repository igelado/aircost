use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::types::{
    DepreciationPoint, ErrorBands, GroupCounts, HoursTrend, IdentityOffsets, StructuralArtifactV1,
    SupportGrade, TrainingListing, UtilizationRates, ValuationBreakdown, ValuationError,
    ValuationEstimate, ValuationQuery,
};
use super::{ValuationModel, FEATURE_SCHEMA_VERSION};

const AGE_FLOOR_MIN: f64 = 0.10;
const AGE_FLOOR_MAX: f64 = 0.70;
const AGE_DECAY_MIN: f64 = 0.01;
const AGE_DECAY_MAX: f64 = 0.25;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct StructuralFitConfig {
    pub initial_age_floor: f64,
    pub initial_age_decay: f64,
    pub huber_delta: f64,
    pub manufacturer_ridge: f64,
    pub model_ridge: f64,
    pub variant_ridge: f64,
    pub hours_ridge: f64,
    pub age_prior_weight: f64,
    pub enable_equipment_count: bool,
}

impl Default for StructuralFitConfig {
    fn default() -> Self {
        Self {
            initial_age_floor: 0.28,
            initial_age_decay: 0.05,
            huber_delta: 0.20,
            manufacturer_ridge: 3.0,
            model_ridge: 7.0,
            variant_ridge: 14.0,
            hours_ridge: 12.0,
            age_prior_weight: 2.0,
            enable_equipment_count: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StructuralModel {
    model_version_id: i64,
    artifact: StructuralArtifactV1,
}

impl StructuralModel {
    pub fn new(
        model_version_id: i64,
        artifact: StructuralArtifactV1,
    ) -> Result<Self, ValuationError> {
        artifact.validate()?;
        Ok(Self {
            model_version_id,
            artifact,
        })
    }

    pub fn artifact(&self) -> &StructuralArtifactV1 {
        &self.artifact
    }

    fn predict_value(
        &self,
        query: &ValuationQuery,
    ) -> Result<(f64, ValuationBreakdown), ValuationError> {
        query.validate()?;
        let artifact = &self.artifact;
        let age_factor = age_factor(artifact.age_floor, artifact.age_decay, query.age());
        let expected_log_hours = artifact
            .expected_hours
            .expected_log_hours(query.age(), query.category_key.as_deref());
        let expected_hours = expected_log_hours.exp_m1().max(0.0);
        let hours_residual = query
            .airframe_hours
            .map(|hours| hours.ln_1p() - expected_log_hours)
            .unwrap_or_default();
        let category_offset = query
            .category_key
            .as_ref()
            .and_then(|key| artifact.identity_offsets.categories.get(key))
            .copied()
            .unwrap_or_default();
        let manufacturer_offset = query
            .manufacturer_id
            .and_then(|id| artifact.identity_offsets.manufacturers.get(&id))
            .copied()
            .unwrap_or_default();
        let model_offset = query
            .model_id
            .and_then(|id| artifact.identity_offsets.models.get(&id))
            .copied()
            .unwrap_or_default();
        let variant_offset = query
            .variant_id
            .and_then(|id| artifact.identity_offsets.variants.get(&id))
            .copied()
            .unwrap_or_default();
        let optional_offset = artifact
            .optional_feature_coefficients
            .get("equipment_count_log1p")
            .copied()
            .unwrap_or_default()
            * (query.equipment_tokens.len() as f64).ln_1p()
            + artifact
                .optional_feature_coefficients
                .get("airframe_hours_missing")
                .copied()
                .unwrap_or_default()
                * f64::from(query.airframe_hours.is_none());
        let hours_offset = artifact.beta_hours * hours_residual;
        let log_value = artifact.global_log_anchor
            + age_factor.ln()
            + category_offset
            + manufacturer_offset
            + model_offset
            + variant_offset
            + hours_offset
            + optional_offset;
        let value = log_value.exp();
        if !value.is_finite() || value <= 0.0 {
            return Err(ValuationError::InvalidArtifact(
                "prediction is not finite and positive".to_string(),
            ));
        }
        Ok((
            value,
            ValuationBreakdown {
                global_anchor_usd: artifact.global_log_anchor.exp(),
                age_factor,
                expected_airframe_hours: expected_hours,
                hours_residual,
                hours_factor: hours_offset.exp(),
                category_factor: category_offset.exp(),
                manufacturer_factor: manufacturer_offset.exp(),
                model_factor: model_offset.exp(),
                variant_factor: variant_offset.exp(),
                optional_features_factor: optional_offset.exp(),
            },
        ))
    }

    fn support(&self, query: &ValuationQuery) -> SupportGrade {
        support_for_counts(&self.artifact.group_counts, query)
    }

    fn q80(&self, query: &ValuationQuery, support: SupportGrade) -> f64 {
        let bands = &self.artifact.error_bands;
        query
            .variant_id
            .and_then(|id| bands.variants.get(&id))
            .filter(|band| band.residual_count >= 10)
            .or_else(|| {
                query
                    .model_id
                    .and_then(|id| bands.models.get(&id))
                    .filter(|band| band.residual_count >= 10)
            })
            .or_else(|| {
                query
                    .manufacturer_id
                    .and_then(|id| bands.manufacturers.get(&id))
                    .filter(|band| band.residual_count >= 10)
            })
            .or_else(|| {
                bands
                    .by_support
                    .get(&support)
                    .filter(|band| band.residual_count >= 10)
            })
            .unwrap_or(&bands.global)
            .q80_abs_log_error
            .max(0.01)
    }

    fn utilization(&self, query: &ValuationQuery) -> f64 {
        query
            .model_id
            .and_then(|id| self.artifact.utilization_rates.models.get(&id))
            .or_else(|| {
                query
                    .manufacturer_id
                    .and_then(|id| self.artifact.utilization_rates.manufacturers.get(&id))
            })
            .copied()
            .unwrap_or(self.artifact.utilization_rates.global_hours_per_year)
            .max(0.0)
    }
}

impl ValuationModel for StructuralModel {
    fn model_version_id(&self) -> i64 {
        self.model_version_id
    }

    fn model_kind(&self) -> &'static str {
        "structural"
    }

    fn snapshot_id(&self) -> i64 {
        self.artifact.snapshot_id
    }

    fn estimate(&self, query: &ValuationQuery) -> Result<ValuationEstimate, ValuationError> {
        let (value_now, breakdown) = self.predict_value(query)?;
        let support = self.support(query);
        let base_q80 = self.q80(query, support);
        let utilization = self.utilization(query);
        let mut depreciation = Vec::with_capacity(31);
        for horizon in 0..=30_i64 {
            let mut future = query.clone();
            future.valuation_year += horizon;
            future.airframe_hours = query
                .airframe_hours
                .map(|hours| hours + utilization * horizon as f64);
            let (value, _) = self.predict_value(&future)?;
            let mut next = future.clone();
            next.valuation_year += 1;
            next.airframe_hours = future.airframe_hours.map(|hours| hours + utilization);
            let next_value = self.predict_value(&next)?.0;
            let q80 = base_q80 * (1.0 + 0.0125 * horizon as f64);
            let error_multiplier = q80.exp();
            depreciation.push(DepreciationPoint {
                horizon_years: horizon,
                valuation_year: future.valuation_year,
                age_years: future.age(),
                airframe_hours: future.airframe_hours,
                estimated_value_usd: value,
                low_value_usd: value / error_multiplier,
                high_value_usd: value * error_multiplier,
                depreciation_usd: (value_now - value).max(0.0),
                depreciation_fraction: ((value_now - value) / value_now).max(0.0),
                one_year_depreciation_fraction: ((value - next_value) / value).max(0.0),
                estimated_error_fraction: error_multiplier - 1.0,
                support,
            });
        }
        let error_multiplier = base_q80.exp();
        Ok(ValuationEstimate {
            estimated_value_usd: value_now,
            low_value_usd: value_now / error_multiplier,
            high_value_usd: value_now * error_multiplier,
            estimated_error_fraction: error_multiplier - 1.0,
            support,
            model_kind: self.model_kind().to_string(),
            model_version_id: self.model_version_id,
            snapshot_id: self.artifact.snapshot_id,
            breakdown,
            depreciation,
        })
    }
}

#[derive(Clone, Debug)]
enum DesignColumn {
    Global,
    Manufacturer(i64),
    Model(i64),
    Variant(i64),
    Hours,
    HoursMissing,
    EquipmentCount,
}

pub fn fit_structural(
    rows: &[TrainingListing],
    config: &StructuralFitConfig,
) -> Result<StructuralArtifactV1, ValuationError> {
    if rows.is_empty() {
        return Err(ValuationError::EmptySnapshot);
    }
    validate_training_rows(rows)?;
    let expected_hours = fit_hours_trend(rows);
    let columns = build_columns(rows, config);
    let mut floor = config.initial_age_floor.clamp(AGE_FLOOR_MIN, AGE_FLOOR_MAX);
    let mut decay = config.initial_age_decay.clamp(AGE_DECAY_MIN, AGE_DECAY_MAX);
    let mut floor_step = 0.10;
    let mut decay_step = 0.035;
    for _ in 0..10 {
        let mut best = (
            objective(rows, &expected_hours, &columns, config, floor, decay)?,
            floor,
            decay,
        );
        for candidate_floor in [floor - floor_step, floor + floor_step]
            .into_iter()
            .map(|value| value.clamp(AGE_FLOOR_MIN, AGE_FLOOR_MAX))
        {
            let score = objective(
                rows,
                &expected_hours,
                &columns,
                config,
                candidate_floor,
                decay,
            )?;
            if score < best.0 {
                best = (score, candidate_floor, decay);
            }
        }
        floor = best.1;
        for candidate_decay in [decay - decay_step, decay + decay_step]
            .into_iter()
            .map(|value| value.clamp(AGE_DECAY_MIN, AGE_DECAY_MAX))
        {
            let score = objective(
                rows,
                &expected_hours,
                &columns,
                config,
                floor,
                candidate_decay,
            )?;
            if score < best.0 {
                best = (score, floor, candidate_decay);
            }
        }
        decay = best.2;
        floor_step *= 0.5;
        decay_step *= 0.5;
    }
    let coefficients = fit_coefficients(rows, &expected_hours, &columns, config, floor, decay)?.0;
    let mut global_log_anchor = 0.0;
    let mut identity_offsets = IdentityOffsets::default();
    let mut beta_hours = 0.0;
    let mut optional_feature_coefficients = BTreeMap::new();
    for (column, coefficient) in columns.iter().zip(coefficients) {
        match column {
            DesignColumn::Global => global_log_anchor = coefficient,
            DesignColumn::Manufacturer(id) => {
                identity_offsets.manufacturers.insert(*id, coefficient);
            }
            DesignColumn::Model(id) => {
                identity_offsets.models.insert(*id, coefficient);
            }
            DesignColumn::Variant(id) => {
                identity_offsets.variants.insert(*id, coefficient);
            }
            DesignColumn::Hours => beta_hours = coefficient.min(0.0),
            DesignColumn::HoursMissing => {
                optional_feature_coefficients.insert(
                    "airframe_hours_missing".to_string(),
                    coefficient.clamp(-0.20, 0.20),
                );
            }
            DesignColumn::EquipmentCount => {
                optional_feature_coefficients.insert(
                    "equipment_count_log1p".to_string(),
                    coefficient.clamp(-0.10, 0.10),
                );
            }
        }
    }
    let artifact = StructuralArtifactV1 {
        snapshot_id: 0,
        snapshot_year: rows[0].snapshot_year,
        global_log_anchor,
        age_floor: floor,
        age_decay: decay,
        expected_hours,
        beta_hours,
        identity_offsets,
        optional_feature_coefficients,
        group_counts: count_groups(rows),
        error_bands: ErrorBands::default(),
        utilization_rates: fit_utilization_rates(rows),
        feature_schema_version: FEATURE_SCHEMA_VERSION,
    };
    artifact.validate()?;

    let median_price = median(rows.iter().map(|row| row.asking_price_usd).collect());
    let anchor = artifact.global_log_anchor.exp();
    if anchor < median_price / 100.0 || anchor > median_price * 100.0 {
        return Err(ValuationError::InvalidArtifact(
            "global anchor cannot reproduce the training price scale".to_string(),
        ));
    }
    Ok(artifact)
}

fn validate_training_rows(rows: &[TrainingListing]) -> Result<(), ValuationError> {
    let snapshot_year = rows[0].snapshot_year;
    let mut groups = BTreeSet::new();
    for row in rows {
        if row.snapshot_year != snapshot_year
            || row.model_year < 1850
            || row.model_year > row.snapshot_year
            || !row.asking_price_usd.is_finite()
            || row.asking_price_usd <= 0.0
            || row
                .airframe_hours
                .is_some_and(|hours| !hours.is_finite() || hours < 0.0)
        {
            return Err(ValuationError::Fit(format!(
                "invalid training row {}",
                row.listing_id
            )));
        }
        if !groups.insert(&row.duplicate_group_key) {
            return Err(ValuationError::Fit(format!(
                "duplicate group {} appears more than once in prepared rows",
                row.duplicate_group_key
            )));
        }
    }
    Ok(())
}

fn build_columns(rows: &[TrainingListing], config: &StructuralFitConfig) -> Vec<DesignColumn> {
    let manufacturers: BTreeSet<_> = rows.iter().map(|row| row.manufacturer_id).collect();
    let models: BTreeSet<_> = rows.iter().map(|row| row.model_id).collect();
    let variants: BTreeSet<_> = rows.iter().map(|row| row.variant_id).collect();
    let mut columns = vec![DesignColumn::Global];
    columns.extend(manufacturers.into_iter().map(DesignColumn::Manufacturer));
    columns.extend(models.into_iter().map(DesignColumn::Model));
    columns.extend(variants.into_iter().map(DesignColumn::Variant));
    columns.push(DesignColumn::Hours);
    if rows.iter().any(|row| row.airframe_hours.is_none()) {
        columns.push(DesignColumn::HoursMissing);
    }
    if config.enable_equipment_count
        && rows
            .iter()
            .filter(|row| !row.equipment_tokens.is_empty())
            .count()
            >= 5
    {
        columns.push(DesignColumn::EquipmentCount);
    }
    columns
}

fn objective(
    rows: &[TrainingListing],
    expected_hours: &HoursTrend,
    columns: &[DesignColumn],
    config: &StructuralFitConfig,
    floor: f64,
    decay: f64,
) -> Result<f64, ValuationError> {
    let (_, loss) = fit_coefficients(rows, expected_hours, columns, config, floor, decay)?;
    let floor_prior = (floor - config.initial_age_floor) / 0.20;
    let decay_prior = (decay - config.initial_age_decay) / 0.08;
    Ok(loss + config.age_prior_weight * (floor_prior * floor_prior + decay_prior * decay_prior))
}

fn fit_coefficients(
    rows: &[TrainingListing],
    expected_hours: &HoursTrend,
    columns: &[DesignColumn],
    config: &StructuralFitConfig,
    floor: f64,
    decay: f64,
) -> Result<(Vec<f64>, f64), ValuationError> {
    let x: Vec<Vec<f64>> = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| design_value(column, row, expected_hours))
                .collect()
        })
        .collect();
    let y: Vec<f64> = rows
        .iter()
        .map(|row| row.asking_price_usd.ln() - age_factor(floor, decay, row.age()).ln())
        .collect();
    let ridge: Vec<f64> = columns
        .iter()
        .map(|column| match column {
            DesignColumn::Global => 0.0,
            DesignColumn::Manufacturer(_) => config.manufacturer_ridge,
            DesignColumn::Model(_) => config.model_ridge,
            DesignColumn::Variant(_) => config.variant_ridge,
            DesignColumn::Hours => config.hours_ridge,
            DesignColumn::HoursMissing => 20.0,
            DesignColumn::EquipmentCount => 20.0,
        })
        .collect();
    let mut coefficients = vec![0.0; columns.len()];
    let mut weights = vec![1.0; rows.len()];
    for _ in 0..20 {
        let next = solve_ridge(&x, &y, &weights, &ridge)?;
        let residuals: Vec<f64> = x
            .iter()
            .zip(&y)
            .map(|(features, target)| target - dot(features, &next))
            .collect();
        weights = residuals
            .iter()
            .map(|residual| huber_weight(*residual, config.huber_delta))
            .collect();
        let change = next
            .iter()
            .zip(&coefficients)
            .map(|(left, right)| (left - right).abs())
            .fold(0.0, f64::max);
        coefficients = next;
        if change < 1e-8 {
            break;
        }
    }
    if let Some(index) = columns
        .iter()
        .position(|column| matches!(column, DesignColumn::Hours))
    {
        coefficients[index] = coefficients[index].min(0.0);
    }
    let loss = x
        .iter()
        .zip(y)
        .map(|(features, target)| {
            huber_loss(target - dot(features, &coefficients), config.huber_delta)
        })
        .sum::<f64>()
        / rows.len() as f64;
    Ok((coefficients, loss))
}

fn design_value(column: &DesignColumn, row: &TrainingListing, trend: &HoursTrend) -> f64 {
    match column {
        DesignColumn::Global => 1.0,
        DesignColumn::Manufacturer(id) => f64::from(row.manufacturer_id == *id),
        DesignColumn::Model(id) => f64::from(row.model_id == *id),
        DesignColumn::Variant(id) => f64::from(row.variant_id == *id),
        DesignColumn::Hours => row
            .airframe_hours
            .map(|hours| {
                hours.ln_1p() - trend.expected_log_hours(row.age(), row.category_key.as_deref())
            })
            .unwrap_or_default(),
        DesignColumn::HoursMissing => f64::from(row.airframe_hours.is_none()),
        DesignColumn::EquipmentCount => (row.equipment_tokens.len() as f64).ln_1p(),
    }
}

fn solve_ridge(
    x: &[Vec<f64>],
    y: &[f64],
    weights: &[f64],
    ridge: &[f64],
) -> Result<Vec<f64>, ValuationError> {
    let size = ridge.len();
    let mut normal = vec![vec![0.0; size]; size];
    let mut rhs = vec![0.0; size];
    for ((features, target), weight) in x.iter().zip(y).zip(weights) {
        for column in 0..size {
            rhs[column] += weight * features[column] * target;
            for other in 0..=column {
                normal[column][other] += weight * features[column] * features[other];
            }
        }
    }
    for column in 0..size {
        normal[column][column] += ridge[column] + 1e-9;
        let lower_values: Vec<_> = normal[column].iter().take(column).copied().collect();
        for (row, value) in normal.iter_mut().take(column).zip(lower_values) {
            row[column] = value;
        }
    }
    cholesky_solve(&normal, &rhs)
}

fn cholesky_solve(matrix: &[Vec<f64>], rhs: &[f64]) -> Result<Vec<f64>, ValuationError> {
    let size = rhs.len();
    let mut lower = vec![vec![0.0; size]; size];
    for row in 0..size {
        for column in 0..=row {
            let previous = (0..column)
                .map(|index| lower[row][index] * lower[column][index])
                .sum::<f64>();
            if row == column {
                let diagonal = matrix[row][row] - previous;
                if diagonal <= 0.0 || !diagonal.is_finite() {
                    return Err(ValuationError::Fit(
                        "ridge system is not positive definite".to_string(),
                    ));
                }
                lower[row][column] = diagonal.sqrt();
            } else {
                lower[row][column] = (matrix[row][column] - previous) / lower[column][column];
            }
        }
    }
    let mut intermediate = vec![0.0; size];
    for row in 0..size {
        let previous = (0..row)
            .map(|column| lower[row][column] * intermediate[column])
            .sum::<f64>();
        intermediate[row] = (rhs[row] - previous) / lower[row][row];
    }
    let mut solution = vec![0.0; size];
    for row in (0..size).rev() {
        let following = ((row + 1)..size)
            .map(|column| lower[column][row] * solution[column])
            .sum::<f64>();
        solution[row] = (intermediate[row] - following) / lower[row][row];
    }
    Ok(solution)
}

fn fit_hours_trend(rows: &[TrainingListing]) -> HoursTrend {
    let observations: Vec<(f64, f64)> = rows
        .iter()
        .filter_map(|row| row.airframe_hours.map(|hours| (row.age(), hours.ln_1p())))
        .collect();
    if observations.is_empty() {
        return HoursTrend {
            intercept: 0.0,
            age_slope: 0.0,
            category_adjustments: BTreeMap::new(),
        };
    }
    let mut weights = vec![1.0; observations.len()];
    let mut fit = (
        median(observations.iter().map(|(_, value)| *value).collect()),
        0.0,
    );
    for _ in 0..20 {
        let next = weighted_line_fit(&observations, &weights);
        let residuals: Vec<f64> = observations
            .iter()
            .map(|(age, value)| value - next.0 - next.1 * age)
            .collect();
        let scale = median(residuals.iter().map(|value| value.abs()).collect()).max(0.10);
        weights = residuals
            .into_iter()
            .map(|residual| huber_weight(residual, 1.345 * scale))
            .collect();
        if (next.0 - fit.0).abs() + (next.1 - fit.1).abs() < 1e-8 {
            fit = next;
            break;
        }
        fit = next;
    }
    HoursTrend {
        intercept: fit.0.max(0.0),
        age_slope: fit.1.max(0.0),
        category_adjustments: BTreeMap::new(),
    }
}

fn weighted_line_fit(observations: &[(f64, f64)], weights: &[f64]) -> (f64, f64) {
    let sum_weight = weights.iter().sum::<f64>().max(1e-9);
    let mean_x = observations
        .iter()
        .zip(weights)
        .map(|((x, _), weight)| x * weight)
        .sum::<f64>()
        / sum_weight;
    let mean_y = observations
        .iter()
        .zip(weights)
        .map(|((_, y), weight)| y * weight)
        .sum::<f64>()
        / sum_weight;
    let covariance = observations
        .iter()
        .zip(weights)
        .map(|((x, y), weight)| weight * (x - mean_x) * (y - mean_y))
        .sum::<f64>();
    let variance = observations
        .iter()
        .zip(weights)
        .map(|((x, _), weight)| weight * (x - mean_x).powi(2))
        .sum::<f64>();
    let slope = if variance > 1e-9 {
        covariance / variance
    } else {
        0.0
    };
    (mean_y - slope * mean_x, slope)
}

fn count_groups(rows: &[TrainingListing]) -> GroupCounts {
    let mut counts = GroupCounts {
        total: rows.len(),
        ..GroupCounts::default()
    };
    for row in rows {
        if let Some(category) = &row.category_key {
            *counts.categories.entry(category.clone()).or_default() += 1;
        }
        *counts.manufacturers.entry(row.manufacturer_id).or_default() += 1;
        *counts.models.entry(row.model_id).or_default() += 1;
        *counts.variants.entry(row.variant_id).or_default() += 1;
    }
    counts
}

fn fit_utilization_rates(rows: &[TrainingListing]) -> UtilizationRates {
    let rates: Vec<f64> = rows
        .iter()
        .filter_map(|row| row.airframe_hours.map(|hours| hours / row.age().max(1.0)))
        .filter(|rate| rate.is_finite() && *rate >= 0.0)
        .collect();
    let global = if rates.is_empty() { 0.0 } else { median(rates) };
    let mut manufacturer_values: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    let mut model_values: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for row in rows {
        if let Some(hours) = row.airframe_hours {
            let rate = hours / row.age().max(1.0);
            manufacturer_values
                .entry(row.manufacturer_id)
                .or_default()
                .push(rate);
            model_values.entry(row.model_id).or_default().push(rate);
        }
    }
    let shrink = |values: Vec<f64>| {
        let count = values.len() as f64;
        (median(values) * count + global * 5.0) / (count + 5.0)
    };
    UtilizationRates {
        global_hours_per_year: global,
        manufacturers: manufacturer_values
            .into_iter()
            .map(|(id, values)| (id, shrink(values)))
            .collect(),
        models: model_values
            .into_iter()
            .map(|(id, values)| (id, shrink(values)))
            .collect(),
    }
}

pub(crate) fn support_for_counts(counts: &GroupCounts, query: &ValuationQuery) -> SupportGrade {
    let model_count = query
        .model_id
        .and_then(|id| counts.models.get(&id))
        .copied()
        .unwrap_or_default();
    let manufacturer_count = query
        .manufacturer_id
        .and_then(|id| counts.manufacturers.get(&id))
        .copied()
        .unwrap_or_default();
    if model_count >= 5 {
        SupportGrade::High
    } else if model_count >= 2 || manufacturer_count >= 5 {
        SupportGrade::Medium
    } else {
        SupportGrade::Low
    }
}

pub(crate) fn age_factor(floor: f64, decay: f64, age: f64) -> f64 {
    crate::depreciation::listing_only_age_residual_fraction(age, floor, decay)
}

fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn huber_weight(residual: f64, delta: f64) -> f64 {
    if residual.abs() <= delta || residual == 0.0 {
        1.0
    } else {
        delta / residual.abs()
    }
}

fn huber_loss(residual: f64, delta: f64) -> f64 {
    if residual.abs() <= delta {
        0.5 * residual * residual
    } else {
        delta * (residual.abs() - 0.5 * delta)
    }
}

pub(crate) fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return 0.0;
    }
    if values.len().is_multiple_of(2) {
        (values[values.len() / 2 - 1] + values[values.len() / 2]) / 2.0
    } else {
        values[values.len() / 2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::valuation::ValuationModel;

    fn synthetic_rows() -> Vec<TrainingListing> {
        let floor = 0.30;
        let decay = 0.06;
        (0..36)
            .map(|index| {
                let age = index as f64;
                TrainingListing {
                    listing_id: index + 1,
                    duplicate_group_key: format!("aircraft-{index}"),
                    category_key: None,
                    manufacturer_id: 1,
                    model_id: if index < 18 { 1 } else { 2 },
                    variant_id: if index < 18 { 1 } else { 2 },
                    model_year: 2026 - index,
                    snapshot_year: 2026,
                    asking_price_usd: 500_000.0 * age_factor(floor, decay, age),
                    airframe_hours: Some(80.0 * age),
                    engine_times: vec![],
                    propeller_times: vec![],
                    equipment_tokens: vec![],
                    technical_field_count: 4,
                }
            })
            .collect()
    }

    #[test]
    fn recovers_synthetic_shared_age_curve() {
        let artifact = fit_structural(&synthetic_rows(), &StructuralFitConfig::default()).unwrap();
        assert!((artifact.age_floor - 0.30).abs() < 0.16, "{artifact:?}");
        assert!((artifact.age_decay - 0.06).abs() < 0.035, "{artifact:?}");
    }

    #[test]
    fn estimates_decrease_with_age_and_extra_hours() {
        let artifact = fit_structural(&synthetic_rows(), &StructuralFitConfig::default()).unwrap();
        let model = StructuralModel::new(7, artifact.clone()).unwrap();
        let query = synthetic_rows()[8].as_query();
        let estimate = model.estimate(&query).unwrap();
        assert!(estimate
            .depreciation
            .windows(2)
            .all(|pair| pair[1].estimated_value_usd <= pair[0].estimated_value_usd + 0.01));
        let mut high_hours = query.clone();
        high_hours.airframe_hours = query.airframe_hours.map(|hours| hours * 2.0 + 100.0);
        assert!(
            model.estimate(&high_hours).unwrap().estimated_value_usd
                <= estimate.estimated_value_usd + 0.01
        );
        assert!(age_factor(artifact.age_floor, artifact.age_decay, 10_000.0) >= artifact.age_floor);
    }

    #[test]
    fn unknown_identity_and_missing_fields_still_produce_value() {
        let artifact = fit_structural(&synthetic_rows(), &StructuralFitConfig::default()).unwrap();
        let model = StructuralModel::new(8, artifact).unwrap();
        let query = ValuationQuery {
            category_key: None,
            manufacturer_id: None,
            model_id: None,
            variant_id: None,
            model_year: 2010,
            valuation_year: 2026,
            airframe_hours: None,
            engine_times: vec![],
            propeller_times: vec![],
            equipment_tokens: vec![],
        };
        let estimate = model.estimate(&query).unwrap();
        assert!(estimate.estimated_value_usd.is_finite());
        assert!(estimate.estimated_value_usd > 0.0);
        assert_eq!(estimate.support, SupportGrade::Low);
    }

    #[test]
    fn artifact_round_trip_does_not_change_prediction() {
        let artifact = fit_structural(&synthetic_rows(), &StructuralFitConfig::default()).unwrap();
        let encoded = serde_json::to_vec(&artifact).unwrap();
        let decoded: StructuralArtifactV1 = serde_json::from_slice(&encoded).unwrap();
        let query = synthetic_rows()[10].as_query();
        let before = StructuralModel::new(1, artifact)
            .unwrap()
            .estimate(&query)
            .unwrap();
        let after = StructuralModel::new(1, decoded)
            .unwrap()
            .estimate(&query)
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn unknown_identity_falls_back_through_the_documented_hierarchy() {
        let mut artifact =
            fit_structural(&synthetic_rows(), &StructuralFitConfig::default()).unwrap();
        artifact.identity_offsets.manufacturers.insert(10, 0.10);
        artifact.identity_offsets.models.insert(20, 0.20);
        artifact.identity_offsets.variants.insert(30, 0.30);
        let model = StructuralModel::new(1, artifact).unwrap();
        let base_query = ValuationQuery {
            category_key: None,
            manufacturer_id: None,
            model_id: None,
            variant_id: None,
            model_year: 2010,
            valuation_year: 2026,
            airframe_hours: None,
            engine_times: vec![],
            propeller_times: vec![],
            equipment_tokens: vec![],
        };
        let global = model.estimate(&base_query).unwrap().estimated_value_usd;
        let mut manufacturer_query = base_query.clone();
        manufacturer_query.manufacturer_id = Some(10);
        let manufacturer = model
            .estimate(&manufacturer_query)
            .unwrap()
            .estimated_value_usd;
        let mut model_query = manufacturer_query.clone();
        model_query.model_id = Some(20);
        let model_value = model.estimate(&model_query).unwrap().estimated_value_usd;
        let mut variant_query = model_query.clone();
        variant_query.variant_id = Some(30);
        let variant = model.estimate(&variant_query).unwrap().estimated_value_usd;
        assert!((manufacturer / global - 0.10_f64.exp()).abs() < 1e-10);
        assert!((model_value / manufacturer - 0.20_f64.exp()).abs() < 1e-10);
        assert!((variant / model_value - 0.30_f64.exp()).abs() < 1e-10);
    }
}
