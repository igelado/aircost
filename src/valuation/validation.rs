use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::comparable::{ComparableConfig, ComparableModel};
use super::structural::{fit_structural, StructuralFitConfig, StructuralModel};
use super::types::{
    ErrorBand, ErrorBands, StructuralArtifactV1, SupportGrade, TrainingListing, ValuationError,
};
use super::ValuationModel;

const GROUPED_FOLD_SEED: &str = "aircost-valuation-folds-v1";
const CALIBRATION_SPLIT_SEED: &str = "aircost-valuation-calibration-v1";
const BASELINE_NON_INFERIORITY_MARGIN: f64 = 0.02;
pub(crate) const VALIDATION_EVIDENCE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Fold {
    pub id: String,
    pub training_indices: Vec<usize>,
    pub held_out_indices: Vec<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct FoldPrediction {
    pub fold_id: String,
    pub duplicate_group_key: String,
    pub listing_id: i64,
    pub manufacturer_id: i64,
    pub model_id: i64,
    pub variant_id: i64,
    pub actual_price_usd: f64,
    pub predicted_price_usd: f64,
    pub log_error: f64,
    pub absolute_percentage_error: f64,
    pub signed_percentage_error: f64,
    pub support: SupportGrade,
}

impl FoldPrediction {
    pub fn absolute_percentage_error(&self) -> f64 {
        self.absolute_percentage_error
    }

    pub fn signed_percentage_error(&self) -> f64 {
        self.signed_percentage_error
    }

    pub fn absolute_log_error(&self) -> f64 {
        self.log_error
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ValidationMetrics {
    pub prediction_count: usize,
    #[serde(default)]
    pub unique_aircraft_count: usize,
    pub median_absolute_percentage_error: f64,
    pub mean_signed_percentage_error: f64,
    pub q80_absolute_percentage_error: f64,
    pub log_rmse: f64,
    pub empirical_interval_coverage: f64,
}

impl ValidationMetrics {
    pub fn from_predictions(predictions: &[FoldPrediction]) -> Result<Self, ValuationError> {
        if predictions.is_empty()
            || predictions.iter().any(|prediction| {
                !prediction.actual_price_usd.is_finite()
                    || prediction.actual_price_usd <= 0.0
                    || !prediction.predicted_price_usd.is_finite()
                    || prediction.predicted_price_usd <= 0.0
            })
        {
            return Err(ValuationError::Fit(
                "metrics need finite positive actual and predicted prices".to_string(),
            ));
        }
        let (calibration, evaluation) = split_calibration_evaluation(predictions);
        let bands = calibrate_error_bands(&calibration);
        Ok(metrics(&evaluation, bands.global.q80_abs_log_error))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct StabilityReport {
    pub refit_count: usize,
    pub leave_one_group_refit_count: usize,
    pub bootstrap_refit_count: usize,
    pub age_floor_min: f64,
    pub age_floor_max: f64,
    pub age_decay_min: f64,
    pub age_decay_max: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ValidationReport {
    #[serde(default)]
    pub validation_evidence_version: u32,
    pub fold_strategy: String,
    pub structural_metrics: ValidationMetrics,
    pub comparable_metrics: ValidationMetrics,
    pub median_baseline_metrics: ValidationMetrics,
    pub leave_one_model_out_metrics: ValidationMetrics,
    #[serde(default)]
    pub calibration_aircraft_count: usize,
    #[serde(default)]
    pub evaluation_aircraft_count: usize,
    #[serde(default)]
    pub comparable_shadow_evidence: bool,
    #[serde(default)]
    pub leave_one_model_out_required: bool,
    #[serde(default)]
    pub leave_one_model_out_evidence: bool,
    #[serde(default)]
    pub scope_warnings: Vec<String>,
    pub error_bands: ErrorBands,
    pub stability: StabilityReport,
    pub activation_gates_pass: bool,
    pub gate_reasons: Vec<String>,
    pub fold_predictions: Vec<FoldPrediction>,
}

pub fn grouped_folds(rows: &[TrainingListing]) -> Vec<Fold> {
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        groups
            .entry(&row.duplicate_group_key)
            .or_default()
            .push(index);
    }
    if groups.len() < 2 {
        return Vec::new();
    }
    if groups.len() < 20 {
        return groups
            .into_iter()
            .enumerate()
            .map(|(fold_index, (_, held_out_indices))| {
                fold_from_test_indices(format!("loao-{fold_index}"), rows.len(), held_out_indices)
            })
            .collect();
    }
    let group_entries: Vec<_> = groups.into_iter().collect();
    let mut folds = Vec::with_capacity(10);
    for repeat in 0..2_u64 {
        for fold_index in 0..5_u64 {
            let held_out_indices = group_entries
                .iter()
                .filter(|(key, _)| stable_group_bucket(key, repeat) == fold_index)
                .flat_map(|(_, indices)| indices.iter().copied())
                .collect();
            folds.push(fold_from_test_indices(
                format!("grouped-5fold-r{repeat}-f{fold_index}"),
                rows.len(),
                held_out_indices,
            ));
        }
    }
    folds
        .into_iter()
        .filter(|fold| !fold.held_out_indices.is_empty() && !fold.training_indices.is_empty())
        .collect()
}

pub fn validate_structural(
    rows: &[TrainingListing],
    config: &StructuralFitConfig,
) -> Result<ValidationReport, ValuationError> {
    if rows.is_empty() {
        return Err(ValuationError::EmptySnapshot);
    }
    let folds = grouped_folds(rows);
    let fold_strategy = if distinct_group_count(rows) < 20 {
        "leave-one-aircraft-out"
    } else {
        "repeated-grouped-five-fold"
    }
    .to_string();
    let structural_predictions = evaluate_structural(rows, config, &folds)?;
    let comparable_predictions = evaluate_comparable(rows, &folds)?;
    let median_predictions = evaluate_median_baseline(rows, &folds);
    let (structural_calibration, structural_evaluation) =
        split_calibration_evaluation(&structural_predictions);
    let (comparable_calibration, comparable_evaluation) =
        split_calibration_evaluation(&comparable_predictions);
    let (median_calibration, median_evaluation) = split_calibration_evaluation(&median_predictions);
    let error_bands = calibrate_error_bands(&structural_calibration);
    let structural_metrics = metrics(&structural_evaluation, error_bands.global.q80_abs_log_error);
    let comparable_metrics = metrics(
        &comparable_evaluation,
        calibrate_error_bands(&comparable_calibration)
            .global
            .q80_abs_log_error,
    );
    let median_baseline_metrics = metrics(
        &median_evaluation,
        calibrate_error_bands(&median_calibration)
            .global
            .q80_abs_log_error,
    );
    let leave_one_model_out_predictions = evaluate_leave_one_model_out(rows, config)?;
    let leave_one_model_out_required = distinct_model_count(rows) >= 2;
    let leave_one_model_out_evidence = !leave_one_model_out_predictions.is_empty();
    let leave_one_model_out_metrics = metrics(
        &leave_one_model_out_predictions,
        calibrate_error_bands(&leave_one_model_out_predictions)
            .global
            .q80_abs_log_error,
    );
    let stability = resampling_stability(rows, config)?;
    let mut gate_reasons = Vec::new();
    let mut scope_warnings = Vec::new();
    if structural_predictions.is_empty() && rows.len() >= 2 {
        gate_reasons.push("no grouped out-of-fold predictions were produced".to_string());
    }
    if leave_one_model_out_required && !leave_one_model_out_evidence {
        gate_reasons.push(
            "leave-one-model-out validation was required but produced no evidence".to_string(),
        );
    } else if !leave_one_model_out_required {
        scope_warnings.push(
            "leave-one-model-out validation is unavailable because the snapshot represents only one model; metrics describe within-model performance only"
                .to_string(),
        );
    }
    comparable_activation_gate_reasons(&structural_metrics, &comparable_metrics, &mut gate_reasons);
    if structural_metrics.prediction_count > 0
        && structural_metrics.median_absolute_percentage_error
            > median_baseline_metrics.median_absolute_percentage_error
        && structural_metrics.mean_signed_percentage_error.abs()
            >= median_baseline_metrics.mean_signed_percentage_error.abs()
    {
        gate_reasons.push(
            "structural model improves neither median error nor signed bias over median baseline"
                .to_string(),
        );
    }
    if structural_metrics.prediction_count > 0
        && !(-0.50..=0.50).contains(&structural_metrics.mean_signed_percentage_error)
    {
        gate_reasons.push("out-of-fold signed bias exceeds the safety bound".to_string());
    }
    if structural_metrics.prediction_count >= 10 {
        if structural_metrics.median_absolute_percentage_error > 0.25 {
            gate_reasons.push("grouped median absolute percentage error exceeds 25%".to_string());
        }
        if !(-0.10..=0.10).contains(&structural_metrics.mean_signed_percentage_error) {
            gate_reasons
                .push("grouped mean signed percentage error is outside -10%..+10%".to_string());
        }
        if structural_metrics.q80_absolute_percentage_error > 0.40 {
            gate_reasons.push("grouped 80th-percentile percentage error exceeds 40%".to_string());
        }
        if !(0.70..=0.90).contains(&structural_metrics.empirical_interval_coverage) {
            gate_reasons.push("empirical range coverage is outside 70%..90%".to_string());
        }
    }
    let comparable_shadow_evidence = comparable_metrics.unique_aircraft_count
        == structural_metrics.unique_aircraft_count
        && comparable_metrics.unique_aircraft_count > 0;
    Ok(ValidationReport {
        validation_evidence_version: VALIDATION_EVIDENCE_VERSION,
        fold_strategy,
        structural_metrics,
        comparable_metrics,
        median_baseline_metrics,
        leave_one_model_out_metrics,
        calibration_aircraft_count: distinct_prediction_group_count(&structural_calibration),
        evaluation_aircraft_count: distinct_prediction_group_count(&structural_evaluation),
        comparable_shadow_evidence,
        leave_one_model_out_required,
        leave_one_model_out_evidence,
        scope_warnings,
        error_bands,
        stability,
        activation_gates_pass: gate_reasons.is_empty(),
        gate_reasons,
        fold_predictions: structural_predictions,
    })
}

fn comparable_activation_gate_reasons(
    structural: &ValidationMetrics,
    comparable: &ValidationMetrics,
    reasons: &mut Vec<String>,
) {
    if structural.prediction_count == 0 || comparable.prediction_count == 0 {
        reasons.push(
            "structural and adjusted-comparable shadow evaluation both need predictions"
                .to_string(),
        );
        return;
    }
    if structural.median_absolute_percentage_error
        > comparable.median_absolute_percentage_error + BASELINE_NON_INFERIORITY_MARGIN
    {
        reasons.push(
            "structural median error is more than 2 percentage points worse than adjusted-comparable"
                .to_string(),
        );
    }
    if structural.q80_absolute_percentage_error
        > comparable.q80_absolute_percentage_error + BASELINE_NON_INFERIORITY_MARGIN
    {
        reasons.push(
            "structural 80th-percentile error is more than 2 percentage points worse than adjusted-comparable"
                .to_string(),
        );
    }
    if structural.mean_signed_percentage_error.abs()
        > comparable.mean_signed_percentage_error.abs() + BASELINE_NON_INFERIORITY_MARGIN
    {
        reasons.push(
            "structural absolute signed bias is more than 2 percentage points worse than adjusted-comparable"
                .to_string(),
        );
    }
}

pub fn fit_validated_structural(
    rows: &[TrainingListing],
    snapshot_id: i64,
    config: &StructuralFitConfig,
) -> Result<(StructuralArtifactV1, ValidationReport), ValuationError> {
    let report = validate_structural(rows, config)?;
    let mut artifact = fit_structural(rows, config)?;
    artifact.snapshot_id = snapshot_id;
    artifact.error_bands = report.error_bands.clone();
    artifact.validate()?;
    Ok((artifact, report))
}

fn evaluate_structural(
    rows: &[TrainingListing],
    config: &StructuralFitConfig,
    folds: &[Fold],
) -> Result<Vec<FoldPrediction>, ValuationError> {
    let mut predictions = Vec::new();
    for fold in folds {
        let training: Vec<_> = fold
            .training_indices
            .iter()
            .map(|index| rows[*index].clone())
            .collect();
        let artifact = fit_structural(&training, config)?;
        let model = StructuralModel::new(0, artifact)?;
        for index in &fold.held_out_indices {
            let row = &rows[*index];
            let estimate = model.estimate(&row.as_query())?;
            predictions.push(prediction(
                fold.id.clone(),
                row,
                estimate.estimated_value_usd,
                estimate.support,
            ));
        }
    }
    Ok(predictions)
}

fn evaluate_comparable(
    rows: &[TrainingListing],
    folds: &[Fold],
) -> Result<Vec<FoldPrediction>, ValuationError> {
    let mut predictions = Vec::new();
    for fold in folds {
        let training: Vec<_> = fold
            .training_indices
            .iter()
            .map(|index| rows[*index].clone())
            .collect();
        let model = ComparableModel::new(0, 0, training, ComparableConfig::default())?;
        for index in &fold.held_out_indices {
            let row = &rows[*index];
            let estimate = model.estimate(&row.as_query())?;
            predictions.push(prediction(
                fold.id.clone(),
                row,
                estimate.estimated_value_usd,
                estimate.support,
            ));
        }
    }
    Ok(predictions)
}

fn evaluate_median_baseline(rows: &[TrainingListing], folds: &[Fold]) -> Vec<FoldPrediction> {
    let mut predictions = Vec::new();
    for fold in folds {
        let training: Vec<_> = fold
            .training_indices
            .iter()
            .map(|index| &rows[*index])
            .collect();
        for index in &fold.held_out_indices {
            let held_out = &rows[*index];
            // Category is not yet source-backed in the listing schema, so the
            // declared category-or-global median baseline is global.
            let prices: Vec<f64> = training.iter().map(|row| row.asking_price_usd).collect();
            let predicted = percentile(prices, 0.5);
            predictions.push(prediction(
                fold.id.clone(),
                held_out,
                predicted,
                SupportGrade::Low,
            ));
        }
    }
    predictions
}

fn evaluate_leave_one_model_out(
    rows: &[TrainingListing],
    config: &StructuralFitConfig,
) -> Result<Vec<FoldPrediction>, ValuationError> {
    let models: BTreeSet<i64> = rows.iter().map(|row| row.model_id).collect();
    if models.len() < 2 {
        return Ok(Vec::new());
    }
    let mut predictions = Vec::new();
    for model_id in models {
        let training: Vec<_> = rows
            .iter()
            .filter(|row| row.model_id != model_id)
            .cloned()
            .collect();
        let artifact = fit_structural(&training, config)?;
        let model = StructuralModel::new(0, artifact)?;
        for row in rows.iter().filter(|row| row.model_id == model_id) {
            let estimate = model.estimate(&row.as_query())?;
            predictions.push(prediction(
                format!("leave-model-{model_id}"),
                row,
                estimate.estimated_value_usd,
                estimate.support,
            ));
        }
    }
    Ok(predictions)
}

fn prediction(
    fold_id: String,
    row: &TrainingListing,
    predicted_price_usd: f64,
    support: SupportGrade,
) -> FoldPrediction {
    let ratio = predicted_price_usd / row.asking_price_usd;
    FoldPrediction {
        fold_id,
        duplicate_group_key: row.duplicate_group_key.clone(),
        listing_id: row.listing_id,
        manufacturer_id: row.manufacturer_id,
        model_id: row.model_id,
        variant_id: row.variant_id,
        actual_price_usd: row.asking_price_usd,
        predicted_price_usd,
        log_error: ratio.ln().abs(),
        absolute_percentage_error: (ratio - 1.0).abs(),
        signed_percentage_error: ratio - 1.0,
        support,
    }
}

pub fn calibrate_error_bands(predictions: &[FoldPrediction]) -> ErrorBands {
    if distinct_prediction_group_count(predictions) < 2 {
        return ErrorBands::default();
    }
    let mut bands = ErrorBands {
        global: band(predictions.iter()),
        ..ErrorBands::default()
    };
    for support in [SupportGrade::Low, SupportGrade::Medium, SupportGrade::High] {
        let matching: Vec<_> = predictions
            .iter()
            .filter(|prediction| prediction.support == support)
            .collect();
        if !matching.is_empty() {
            bands.by_support.insert(support, band(matching.into_iter()));
        }
    }
    bands.manufacturers = grouped_bands(predictions, |prediction| prediction.manufacturer_id);
    bands.models = grouped_bands(predictions, |prediction| prediction.model_id);
    bands.variants = grouped_bands(predictions, |prediction| prediction.variant_id);
    bands
}

fn grouped_bands(
    predictions: &[FoldPrediction],
    key: impl Fn(&FoldPrediction) -> i64,
) -> BTreeMap<i64, ErrorBand> {
    let mut grouped: BTreeMap<i64, Vec<&FoldPrediction>> = BTreeMap::new();
    for prediction in predictions {
        grouped.entry(key(prediction)).or_default().push(prediction);
    }
    grouped
        .into_iter()
        .map(|(key, predictions)| (key, band(predictions.into_iter())))
        .collect()
}

fn band<'a>(predictions: impl Iterator<Item = &'a FoldPrediction>) -> ErrorBand {
    let mut by_aircraft: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for prediction in predictions {
        by_aircraft
            .entry(&prediction.duplicate_group_key)
            .or_default()
            .push(prediction.log_error);
    }
    let errors: Vec<f64> = by_aircraft
        .into_values()
        .map(|values| percentile(values, 0.5))
        .collect();
    ErrorBand {
        median_abs_log_error: percentile(errors.clone(), 0.5),
        q80_abs_log_error: percentile(errors.clone(), 0.8),
        residual_count: errors.len(),
    }
}

pub(crate) fn metrics(predictions: &[FoldPrediction], q80: f64) -> ValidationMetrics {
    if predictions.is_empty() {
        return ValidationMetrics::default();
    }
    let absolute: Vec<f64> = predictions
        .iter()
        .map(|prediction| prediction.absolute_percentage_error)
        .collect();
    let signed_mean = predictions
        .iter()
        .map(|prediction| prediction.signed_percentage_error)
        .sum::<f64>()
        / predictions.len() as f64;
    let log_rmse = (predictions
        .iter()
        .map(|prediction| prediction.log_error.powi(2))
        .sum::<f64>()
        / predictions.len() as f64)
        .sqrt();
    let coverage = predictions
        .iter()
        .filter(|prediction| prediction.log_error <= q80)
        .count() as f64
        / predictions.len() as f64;
    ValidationMetrics {
        prediction_count: predictions.len(),
        unique_aircraft_count: distinct_prediction_group_count(predictions),
        median_absolute_percentage_error: percentile(absolute.clone(), 0.5),
        mean_signed_percentage_error: signed_mean,
        q80_absolute_percentage_error: percentile(absolute, 0.8),
        log_rmse,
        empirical_interval_coverage: coverage,
    }
}

pub(crate) fn split_calibration_evaluation(
    predictions: &[FoldPrediction],
) -> (Vec<FoldPrediction>, Vec<FoldPrediction>) {
    let mut keyed_groups = predictions
        .iter()
        .map(|prediction| prediction.duplicate_group_key.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|key| {
            let digest = Sha256::digest(format!("{CALIBRATION_SPLIT_SEED}:{key}").as_bytes());
            (digest.to_vec(), key)
        })
        .collect::<Vec<_>>();
    keyed_groups.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(right.1)));
    if keyed_groups.len() < 2 {
        return (predictions.to_vec(), predictions.to_vec());
    }
    let calibration_count = keyed_groups.len() / 2;
    let calibration_groups = keyed_groups
        .iter()
        .take(calibration_count)
        .map(|(_, key)| *key)
        .collect::<BTreeSet<_>>();
    let (calibration, evaluation): (Vec<_>, Vec<_>) =
        predictions.iter().cloned().partition(|prediction| {
            calibration_groups.contains(prediction.duplicate_group_key.as_str())
        });
    (calibration, evaluation)
}

fn distinct_prediction_group_count(predictions: &[FoldPrediction]) -> usize {
    predictions
        .iter()
        .map(|prediction| prediction.duplicate_group_key.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

fn resampling_stability(
    rows: &[TrainingListing],
    config: &StructuralFitConfig,
) -> Result<StabilityReport, ValuationError> {
    let groups: BTreeSet<_> = rows
        .iter()
        .map(|row| row.duplicate_group_key.clone())
        .collect();
    if groups.len() < 3 {
        return Ok(StabilityReport::default());
    }
    let all_groups: Vec<_> = groups.into_iter().collect();
    let sampled_groups: Vec<_> = all_groups.iter().take(10).cloned().collect();
    let mut floors = Vec::new();
    let mut decays = Vec::new();
    for group in sampled_groups {
        let training: Vec<_> = rows
            .iter()
            .filter(|row| row.duplicate_group_key != group)
            .cloned()
            .collect();
        let artifact = fit_structural(&training, config)?;
        floors.push(artifact.age_floor);
        decays.push(artifact.age_decay);
    }
    let leave_one_group_refit_count = floors.len();
    for sample in 0..10_u64 {
        let mut training = Vec::with_capacity(rows.len());
        for draw in 0..all_groups.len() {
            let digest = Sha256::digest(format!("aircost-bootstrap-v1:{sample}:{draw}").as_bytes());
            let index = u64::from_be_bytes(
                digest[0..8]
                    .try_into()
                    .expect("SHA-256 prefix is eight bytes"),
            ) as usize
                % all_groups.len();
            for row in rows
                .iter()
                .filter(|row| row.duplicate_group_key == all_groups[index])
            {
                let mut cloned = row.clone();
                cloned.duplicate_group_key =
                    format!("bootstrap-{sample}-{draw}-{}", cloned.duplicate_group_key);
                training.push(cloned);
            }
        }
        let artifact = fit_structural(&training, config)?;
        floors.push(artifact.age_floor);
        decays.push(artifact.age_decay);
    }
    Ok(StabilityReport {
        refit_count: floors.len(),
        leave_one_group_refit_count,
        bootstrap_refit_count: floors.len() - leave_one_group_refit_count,
        age_floor_min: floors.iter().copied().fold(f64::INFINITY, f64::min),
        age_floor_max: floors.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        age_decay_min: decays.iter().copied().fold(f64::INFINITY, f64::min),
        age_decay_max: decays.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    })
}

fn fold_from_test_indices(id: String, row_count: usize, held_out_indices: Vec<usize>) -> Fold {
    let held_out: BTreeSet<_> = held_out_indices.iter().copied().collect();
    Fold {
        id,
        training_indices: (0..row_count)
            .filter(|index| !held_out.contains(index))
            .collect(),
        held_out_indices,
    }
}

fn stable_group_bucket(key: &str, repeat: u64) -> u64 {
    let digest = Sha256::digest(format!("{GROUPED_FOLD_SEED}:{repeat}:{key}").as_bytes());
    u64::from_be_bytes(
        digest[0..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    ) % 5
}

fn distinct_group_count(rows: &[TrainingListing]) -> usize {
    rows.iter()
        .map(|row| &row.duplicate_group_key)
        .collect::<BTreeSet<_>>()
        .len()
}

fn distinct_model_count(rows: &[TrainingListing]) -> usize {
    rows.iter()
        .map(|row| row.model_id)
        .collect::<BTreeSet<_>>()
        .len()
}

fn percentile(mut values: Vec<f64>, percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let index = ((values.len() - 1) as f64 * percentile)
        .round()
        .clamp(0.0, (values.len() - 1) as f64) as usize;
    values[index]
}

pub fn quantile(sorted: &[f64], probability: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let position = probability.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    sorted[lower] + (sorted[upper] - sorted[lower]) * (position - lower as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, group: &str, model: i64, price: f64) -> TrainingListing {
        TrainingListing {
            listing_id: id,
            duplicate_group_key: group.to_string(),
            category_key: None,
            manufacturer_id: 1,
            model_id: model,
            variant_id: model,
            model_year: 2010,
            snapshot_year: 2026,
            asking_price_usd: price,
            airframe_hours: Some(1000.0),
            engine_times: vec![],
            propeller_times: vec![],
            equipment_tokens: vec![],
            valuation_facts: vec![],
            technical_field_count: 3,
        }
    }

    fn fold_prediction(fold: &str, group: &str, error: f64) -> FoldPrediction {
        FoldPrediction {
            fold_id: fold.to_string(),
            duplicate_group_key: group.to_string(),
            listing_id: 1,
            manufacturer_id: 1,
            model_id: 1,
            variant_id: 1,
            actual_price_usd: 100.0,
            predicted_price_usd: 100.0 * error.exp(),
            log_error: error,
            absolute_percentage_error: error.exp() - 1.0,
            signed_percentage_error: error.exp() - 1.0,
            support: SupportGrade::Low,
        }
    }

    #[test]
    fn duplicate_advertisements_never_cross_folds() {
        let rows = vec![
            row(1, "same", 1, 100.0),
            row(2, "same", 1, 100.0),
            row(3, "other", 2, 200.0),
        ];
        for fold in grouped_folds(&rows) {
            let training_groups: BTreeSet<_> = fold
                .training_indices
                .iter()
                .map(|index| &rows[*index].duplicate_group_key)
                .collect();
            let held_out_groups: BTreeSet<_> = fold
                .held_out_indices
                .iter()
                .map(|index| &rows[*index].duplicate_group_key)
                .collect();
            assert!(training_groups.is_disjoint(&held_out_groups));
        }
    }

    #[test]
    fn interval_conversion_is_symmetric_on_log_scale() {
        let q80 = 1.55_f64.ln();
        let estimate = 100_000.0;
        let low = estimate * (-q80).exp();
        let high = estimate * q80.exp();
        assert!((estimate.ln() - low.ln() - (high.ln() - estimate.ln())).abs() < 1e-12);
    }

    #[test]
    fn residual_bands_count_unique_aircraft_not_repeated_folds() {
        let predictions = vec![
            fold_prediction("repeat-1", "aircraft-a", 0.10),
            fold_prediction("repeat-2", "aircraft-a", 0.20),
            fold_prediction("repeat-1", "aircraft-b", 0.30),
            fold_prediction("repeat-2", "aircraft-b", 0.40),
        ];
        let bands = calibrate_error_bands(&predictions);
        assert_eq!(bands.global.residual_count, 2);
    }

    #[test]
    fn calibration_and_evaluation_use_disjoint_aircraft() {
        let predictions = (0..8)
            .flat_map(|aircraft| {
                ["repeat-1", "repeat-2"]
                    .map(move |fold| fold_prediction(fold, &format!("aircraft-{aircraft}"), 0.10))
            })
            .collect::<Vec<_>>();
        let (calibration, evaluation) = split_calibration_evaluation(&predictions);
        let calibration_groups = calibration
            .iter()
            .map(|prediction| prediction.duplicate_group_key.as_str())
            .collect::<BTreeSet<_>>();
        let evaluation_groups = evaluation
            .iter()
            .map(|prediction| prediction.duplicate_group_key.as_str())
            .collect::<BTreeSet<_>>();
        assert!(calibration_groups.is_disjoint(&evaluation_groups));
        assert_eq!(calibration_groups.len() + evaluation_groups.len(), 8);
    }

    #[test]
    fn one_model_snapshot_reports_limited_scope_without_fake_lomo_evidence() {
        let rows = vec![
            row(1, "a", 1, 100_000.0),
            row(2, "b", 1, 110_000.0),
            row(3, "c", 1, 120_000.0),
        ];
        let report = validate_structural(&rows, &StructuralFitConfig::default()).unwrap();
        assert!(!report.leave_one_model_out_required);
        assert!(!report.leave_one_model_out_evidence);
        assert!(!report
            .gate_reasons
            .iter()
            .any(|reason| reason.contains("leave-one-model-out")));
        assert!(report
            .scope_warnings
            .iter()
            .any(|warning| warning.contains("within-model performance only")));
    }

    #[test]
    fn structural_must_remain_close_to_comparable_shadow() {
        let structural = ValidationMetrics {
            prediction_count: 10,
            median_absolute_percentage_error: 0.25,
            ..ValidationMetrics::default()
        };
        let comparable = ValidationMetrics {
            prediction_count: 10,
            median_absolute_percentage_error: 0.10,
            ..ValidationMetrics::default()
        };
        let mut reasons = Vec::new();
        comparable_activation_gate_reasons(&structural, &comparable, &mut reasons);
        assert!(reasons.iter().any(|reason| reason.contains("median error")));
    }

    #[test]
    fn held_out_only_identity_does_not_enter_training_artifact() {
        let rows = vec![row(1, "a", 1, 100_000.0), row(2, "b", 99, 200_000.0)];
        let fold = grouped_folds(&rows)
            .into_iter()
            .find(|fold| fold.held_out_indices == vec![1])
            .unwrap();
        let training: Vec<_> = fold
            .training_indices
            .iter()
            .map(|index| rows[*index].clone())
            .collect();
        let artifact = fit_structural(&training, &StructuralFitConfig::default()).unwrap();
        assert!(!artifact.identity_offsets.models.contains_key(&99));
    }
}
