use std::collections::{BTreeMap, BTreeSet};

use burn::backend::{Autodiff, Flex};
use burn::module::AutodiffModule;
use burn::nn::loss::{HuberLossConfig, Reduction};
use burn::optim::grad_clipping::GradientClippingConfig;
use burn::optim::{AdamWConfig, GradientsParams, Optimizer};
use burn::prelude::{Backend, Tensor, TensorData};
use burn::tensor::Device;
use serde::{Deserialize, Serialize};

use crate::valuation::validation::{
    calibrate_error_bands, grouped_folds, metrics, split_calibration_evaluation, FoldPrediction,
    ValidationMetrics,
};
use crate::valuation::{
    fit_structural, GroupCounts, StructuralArtifactV1, StructuralFitConfig, SupportGrade,
    TrainingListing, UtilizationRates, ValuationError,
};

use super::artifact::{
    serialize_member, DnnArtifactMetadataV1, DnnArtifactV1, DNN_ARTIFACT_FORMAT_VERSION,
    DNN_ENSEMBLE_MEMBERS,
};
use super::features::{FeatureEncoderV1, RESIDUAL_NUMERIC_FEATURES};
use super::network::{DnnArchitectureConfig, EncodedBatch, TinyValuationNet};
use super::DnnCapacity;

type TrainBackend = Autodiff<Flex>;
const INITIAL_AGE_WEIGHTS: [f64; 4] = [0.08, 0.18, 0.42, 0.52];
#[cfg(test)]
const INITIAL_HOURS_BETA: f64 = -0.02;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrainingSchedule {
    pub learning_rate: f64,
    pub weight_decay: f64,
    pub batch_size: usize,
    pub maximum_epochs: usize,
    pub selected_epochs: usize,
    pub dropout_probability: f64,
    pub gradient_norm_clip: f64,
    #[serde(default)]
    pub outer_selected_epochs: Vec<usize>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DnnFitConfig {
    pub model_version_id: i64,
    pub snapshot_id: i64,
    pub baseline_model_version_id: i64,
    pub structural_fit_config: StructuralFitConfig,
    pub member_seeds: Vec<u64>,
    pub validation_seed: u64,
    pub maximum_epochs: usize,
}

impl Default for DnnFitConfig {
    fn default() -> Self {
        Self {
            model_version_id: 0,
            snapshot_id: 0,
            baseline_model_version_id: 0,
            structural_fit_config: StructuralFitConfig::default(),
            member_seeds: vec![11, 23, 37, 53, 71],
            validation_seed: 0x5eed_d11,
            maximum_epochs: 500,
        }
    }
}

impl DnnFitConfig {
    fn validate(&self) -> Result<(), ValuationError> {
        if self.member_seeds.len() != DNN_ENSEMBLE_MEMBERS {
            return Err(ValuationError::Fit(format!(
                "DNN fitting requires exactly {DNN_ENSEMBLE_MEMBERS} member seeds"
            )));
        }
        if self.maximum_epochs == 0 || self.maximum_epochs > 500 {
            return Err(ValuationError::Fit(
                "DNN maximum epochs must be between 1 and 500".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DnnFitReport {
    pub artifact: DnnArtifactV1,
    pub fold_predictions: Vec<FoldPrediction>,
    pub metrics: ValidationMetrics,
}

struct DnnEvaluation {
    fold_predictions: Vec<FoldPrediction>,
    metrics: ValidationMetrics,
    selected_epochs: Vec<usize>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ActivationGateReport {
    pub median_error_gate: bool,
    pub paired_win_fraction: f64,
    pub paired_win_gate: bool,
    pub q80_error_gate: bool,
    pub constraint_and_artifact_gate: bool,
    pub bootstrap_direction_gate: bool,
    pub eligible_for_activation: bool,
}

pub fn evaluate_activation_gates(
    dnn_predictions: &[FoldPrediction],
    structural_predictions: &[FoldPrediction],
    dnn_metrics: &ValidationMetrics,
    structural_metrics: &ValidationMetrics,
    constraint_and_artifact_checks_pass: bool,
    bootstrap_improvements: &[bool],
) -> ActivationGateReport {
    let median_improvement = structural_metrics.median_absolute_percentage_error
        - dnn_metrics.median_absolute_percentage_error;
    let bias_improvement = structural_metrics.mean_signed_percentage_error.abs()
        - dnn_metrics.mean_signed_percentage_error.abs();
    let median_error_gate = median_improvement >= 0.02
        || (bias_improvement >= 0.02
            && dnn_metrics.median_absolute_percentage_error
                <= structural_metrics.median_absolute_percentage_error + 0.01);
    let structural_by_listing = structural_predictions
        .iter()
        .map(|prediction| {
            (
                (
                    prediction.fold_id.as_str(),
                    prediction.duplicate_group_key.as_str(),
                    prediction.listing_id,
                ),
                prediction.absolute_percentage_error(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let paired = dnn_predictions
        .iter()
        .filter_map(|prediction| {
            structural_by_listing
                .get(&(
                    prediction.fold_id.as_str(),
                    prediction.duplicate_group_key.as_str(),
                    prediction.listing_id,
                ))
                .map(|structural_error| prediction.absolute_percentage_error() < *structural_error)
        })
        .collect::<Vec<_>>();
    let paired_win_fraction = if paired.is_empty() {
        0.0
    } else {
        paired.iter().filter(|improved| **improved).count() as f64 / paired.len() as f64
    };
    let paired_win_gate = paired_win_fraction >= 0.60;
    let q80_error_gate = dnn_metrics.q80_absolute_percentage_error
        <= structural_metrics.q80_absolute_percentage_error + 0.05;
    let bootstrap_direction_gate = !bootstrap_improvements.is_empty()
        && bootstrap_improvements
            .iter()
            .filter(|improved| **improved)
            .count()
            * 2
            > bootstrap_improvements.len();
    ActivationGateReport {
        median_error_gate,
        paired_win_fraction,
        paired_win_gate,
        q80_error_gate,
        constraint_and_artifact_gate: constraint_and_artifact_checks_pass,
        bootstrap_direction_gate,
        eligible_for_activation: median_error_gate
            && paired_win_gate
            && q80_error_gate
            && constraint_and_artifact_checks_pass
            && bootstrap_direction_gate,
    }
}

pub fn fit_dnn(
    rows: &[TrainingListing],
    config: &DnnFitConfig,
) -> Result<DnnArtifactV1, ValuationError> {
    let evaluation = if distinct_groups(rows) < 2 {
        None
    } else {
        Some(evaluate_dnn_with_schedule(rows, config)?)
    };
    fit_final_ensemble(
        rows,
        config,
        evaluation
            .as_ref()
            .map(|value| value.fold_predictions.as_slice())
            .unwrap_or_default(),
        evaluation
            .as_ref()
            .map(|value| value.selected_epochs.as_slice())
            .unwrap_or_default(),
    )
}

pub fn fit_dnn_candidate(
    rows: &[TrainingListing],
    config: &DnnFitConfig,
) -> Result<DnnFitReport, ValuationError> {
    let evaluation = if distinct_groups(rows) < 2 {
        DnnEvaluation {
            fold_predictions: vec![],
            metrics: ValidationMetrics::default(),
            selected_epochs: vec![],
        }
    } else {
        evaluate_dnn_with_schedule(rows, config)?
    };
    let artifact = fit_final_ensemble(
        rows,
        config,
        &evaluation.fold_predictions,
        &evaluation.selected_epochs,
    )?;
    Ok(DnnFitReport {
        artifact,
        fold_predictions: evaluation.fold_predictions,
        metrics: evaluation.metrics,
    })
}

pub fn evaluate_dnn(
    rows: &[TrainingListing],
    config: &DnnFitConfig,
) -> Result<(Vec<FoldPrediction>, ValidationMetrics), ValuationError> {
    let evaluation = evaluate_dnn_with_schedule(rows, config)?;
    Ok((evaluation.fold_predictions, evaluation.metrics))
}

fn evaluate_dnn_with_schedule(
    rows: &[TrainingListing],
    config: &DnnFitConfig,
) -> Result<DnnEvaluation, ValuationError> {
    validate_rows(rows)?;
    config.validate()?;
    if distinct_groups(rows) < 2 {
        return Err(ValuationError::Fit(
            "out-of-fold DNN evaluation requires at least two aircraft groups".to_string(),
        ));
    }
    let folds = grouped_folds(rows);
    let device = Default::default();
    let mut predictions = Vec::new();
    let mut selected_epochs = Vec::new();
    for (fold_index, fold) in folds.into_iter().enumerate() {
        let training = fold
            .training_indices
            .iter()
            .map(|index| rows[*index].clone())
            .collect::<Vec<_>>();
        let (capacity, epochs) = select_model_plan(&training, config, &device)?;
        selected_epochs.push(epochs);
        let encoder = FeatureEncoderV1::fit(
            &training,
            capacity,
            config.validation_seed ^ fold_index as u64,
        )?;
        let architecture = architecture_for(&training, &encoder, capacity)?;
        let model = train_member(
            &training,
            &encoder,
            &architecture,
            config.validation_seed ^ fold_index as u64,
            epochs,
            &config.structural_fit_config,
            &device,
        )?;
        for index in fold.held_out_indices {
            let held_out = &rows[index];
            let encoded = encoder.encode(&held_out.query())?;
            let predicted_log = model.predict_one(&encoded, &device)?.log_value;
            let predicted_price = predicted_log.exp();
            if !predicted_price.is_finite() || predicted_price <= 0.0 {
                return Err(ValuationError::Fit(format!(
                    "fold {} produced an invalid prediction",
                    fold.id
                )));
            }
            let ratio = predicted_price / held_out.asking_price_usd;
            predictions.push(FoldPrediction {
                fold_id: fold.id.clone(),
                duplicate_group_key: held_out.duplicate_group_key.clone(),
                listing_id: held_out.listing_id,
                manufacturer_id: held_out.manufacturer_id,
                model_id: held_out.model_id,
                variant_id: held_out.variant_id,
                actual_price_usd: held_out.asking_price_usd,
                predicted_price_usd: predicted_price,
                log_error: ratio.ln().abs(),
                absolute_percentage_error: (ratio - 1.0).abs(),
                signed_percentage_error: ratio - 1.0,
                support: support_for_rows(&training, held_out),
            });
        }
    }
    let (calibration, evaluation) = split_calibration_evaluation(&predictions);
    let error_bands = calibrate_error_bands(&calibration);
    let metrics = metrics(&evaluation, error_bands.global.q80_abs_log_error);
    Ok(DnnEvaluation {
        fold_predictions: predictions,
        metrics,
        selected_epochs,
    })
}

fn fit_final_ensemble(
    rows: &[TrainingListing],
    config: &DnnFitConfig,
    fold_predictions: &[FoldPrediction],
    outer_selected_epochs: &[usize],
) -> Result<DnnArtifactV1, ValuationError> {
    validate_rows(rows)?;
    config.validate()?;
    let device = Default::default();
    let (capacity, full_data_epochs) = select_model_plan(rows, config, &device)?;
    let selected_epochs = median_epoch(outer_selected_epochs).unwrap_or(full_data_epochs);
    let encoder = FeatureEncoderV1::fit(rows, capacity, config.validation_seed)?;
    let architecture = architecture_for(rows, &encoder, capacity)?;
    let mut models = Vec::with_capacity(DNN_ENSEMBLE_MEMBERS);
    let mut members = Vec::with_capacity(DNN_ENSEMBLE_MEMBERS);
    for (index, seed) in config.member_seeds.iter().copied().enumerate() {
        let sample = bootstrap_groups(rows, seed);
        let model = train_member(
            &sample,
            &encoder,
            &architecture,
            seed,
            selected_epochs,
            &config.structural_fit_config,
            &device,
        )?;
        members.push(serialize_member(index, &model)?);
        models.push(model);
    }
    let smoke_input = encoder.encode(&rows[0].query())?;
    let smoke_log_prediction = median(
        models
            .iter()
            .map(|model| {
                model
                    .predict_one(&smoke_input, &device)
                    .map(|value| value.log_value)
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    let member_hashes = members.iter().map(|member| member.sha256.clone()).collect();
    let metadata = DnnArtifactMetadataV1 {
        artifact_format_version: DNN_ARTIFACT_FORMAT_VERSION,
        model_version_id: config.model_version_id,
        snapshot_id: config.snapshot_id,
        baseline_model_version_id: config.baseline_model_version_id,
        capacity,
        architecture: architecture.clone(),
        encoder,
        member_seeds: config.member_seeds.clone(),
        member_hashes,
        parameter_count_per_member: architecture.parameter_count(),
        training_schedule: TrainingSchedule {
            learning_rate: 1e-3,
            weight_decay: 1e-3,
            batch_size: rows.len().min(32),
            maximum_epochs: config.maximum_epochs,
            selected_epochs,
            dropout_probability: 0.10,
            gradient_norm_clip: 1.0,
            outer_selected_epochs: outer_selected_epochs.to_vec(),
        },
        group_counts: group_counts(rows),
        error_bands: {
            let (calibration, _) = split_calibration_evaluation(fold_predictions);
            calibrate_error_bands(&calibration)
        },
        utilization_rates: utilization_rates(rows),
        smoke_input,
        smoke_log_prediction,
        floating_point_tolerance: 1e-4,
    };
    let artifact = DnnArtifactV1 { metadata, members };
    artifact.validate()?;
    // Loading immediately catches missing, extra, or incorrectly shaped tensors.
    artifact.load_members::<Flex>(&device)?;
    Ok(artifact)
}

fn median_epoch(epochs: &[usize]) -> Option<usize> {
    if epochs.is_empty() {
        return None;
    }
    let mut epochs = epochs.to_vec();
    epochs.sort_unstable();
    Some(epochs[epochs.len() / 2])
}

fn train_member(
    rows: &[TrainingListing],
    encoder: &FeatureEncoderV1,
    architecture: &DnnArchitectureConfig,
    seed: u64,
    epochs: usize,
    structural_config: &StructuralFitConfig,
    device: &Device<TrainBackend>,
) -> Result<TinyValuationNet<Flex>, ValuationError> {
    let encoded = rows
        .iter()
        .map(|row| encoder.encode(&row.query()))
        .collect::<Result<Vec<_>, _>>()?;
    let structural = fit_structural(rows, structural_config)?;
    let initial_age_weights = structural_age_weights(&structural);
    let initial_hours_beta = structural.beta_hours;
    let initial_anchor = calibrated_initial_anchor(
        rows,
        encoder,
        structural.global_log_anchor,
        initial_age_weights,
        initial_hours_beta,
    );
    <TrainBackend as Backend>::seed(device, seed);
    let mut model = TinyValuationNet::<TrainBackend>::new(
        architecture,
        initial_anchor,
        initial_age_weights,
        initial_hours_beta,
        device,
    )?;
    if architecture.capacity == DnnCapacity::PriorOnly {
        return Ok(model.valid());
    }
    let labels = Tensor::<TrainBackend, 2>::from_data(
        TensorData::new(
            rows.iter()
                .map(|row| row.asking_price_usd.ln() as f32)
                .collect(),
            [rows.len(), 1],
        ),
        device,
    );
    let huber = HuberLossConfig::new(0.20).init();
    let mut optimizer = AdamWConfig::new()
        .with_weight_decay(1e-3)
        .with_grad_clipping(Some(GradientClippingConfig::Norm(1.0)))
        .init();
    for _ in 0..epochs {
        let output = model.forward(EncodedBatch::from_inputs(&encoded, device)?);
        let identity_penalty = output
            .identity_offset
            .clone()
            .square()
            .mean()
            .mul_scalar(0.01);
        let residual_penalty = output.residual.clone().square().mean().mul_scalar(0.02);
        let loss = huber.forward(output.log_value, labels.clone(), Reduction::Mean)
            + identity_penalty
            + residual_penalty;
        let gradients = GradientsParams::from_grads(loss.backward(), &model);
        model = optimizer.step(1e-3, model, gradients);
    }
    Ok(model.valid())
}

fn architecture_for(
    rows: &[TrainingListing],
    encoder: &FeatureEncoderV1,
    capacity: DnnCapacity,
) -> Result<DnnArchitectureConfig, ValuationError> {
    let comparable_component_values = rows
        .iter()
        .flat_map(|row| row.engine_times.iter().chain(&row.propeller_times))
        .filter(|component| component.time_hours.is_some() && component.basis != Default::default())
        .count();
    let equipment_rows = rows
        .iter()
        .filter(|row| !row.equipment_tokens.is_empty())
        .count();
    let config = DnnArchitectureConfig {
        capacity,
        category_count: encoder.category_vocabulary.len_with_unknown(),
        manufacturer_count: encoder.manufacturer_vocabulary.len_with_unknown(),
        model_count: encoder.model_vocabulary.len_with_unknown(),
        variant_count: encoder.variant_vocabulary.len_with_unknown(),
        equipment_bucket_count: encoder.equipment_bucket_count,
        maximum_equipment_items: encoder.maximum_equipment_items,
        component_branch_enabled: capacity.includes_full() && comparable_component_values >= 10,
        equipment_branch_enabled: capacity.includes_full() && equipment_rows >= 10,
        residual_feature_count: RESIDUAL_NUMERIC_FEATURES,
    };
    config.validate()?;
    Ok(config)
}

fn select_capacity(rows: &[TrainingListing]) -> DnnCapacity {
    let groups = distinct_groups(rows);
    let mut capacity = DnnCapacity::for_sample_size(groups);
    let model_diversity = rows
        .iter()
        .map(|row| row.model_id)
        .collect::<BTreeSet<_>>()
        .len();
    if capacity.includes_context() && model_diversity < 3 {
        capacity = DnnCapacity::Shared;
    }
    capacity
}

fn selected_epoch_budget(rows: &[TrainingListing], configured: usize) -> usize {
    // Tiny folds cannot form a reliable inner validation split. Larger folds use a
    // conservative predeclared budget which is persisted and never reads an outer target.
    if distinct_groups(rows) < 10 {
        configured.min(100)
    } else {
        configured
    }
}

fn select_model_plan(
    rows: &[TrainingListing],
    config: &DnnFitConfig,
    device: &Device<TrainBackend>,
) -> Result<(DnnCapacity, usize), ValuationError> {
    let maximum_capacity = select_capacity(rows);
    if distinct_groups(rows) < 20 || maximum_capacity == DnnCapacity::PriorOnly {
        return Ok((
            maximum_capacity,
            selected_epoch_budget(rows, config.maximum_epochs),
        ));
    }
    let Some(inner_fold) = grouped_folds(rows).into_iter().next() else {
        return Ok((
            maximum_capacity,
            selected_epoch_budget(rows, config.maximum_epochs),
        ));
    };
    let inner_training = inner_fold
        .training_indices
        .iter()
        .map(|index| rows[*index].clone())
        .collect::<Vec<_>>();
    let held_out = inner_fold
        .held_out_indices
        .iter()
        .map(|index| rows[*index].clone())
        .collect::<Vec<_>>();
    if inner_training.is_empty() || held_out.is_empty() {
        return Ok((
            maximum_capacity,
            selected_epoch_budget(rows, config.maximum_epochs),
        ));
    }
    let mut capacities = vec![DnnCapacity::Shared];
    if maximum_capacity.includes_context() {
        capacities.push(DnnCapacity::Contextual);
    }
    if maximum_capacity.includes_full() {
        capacities.push(DnnCapacity::Full);
    }
    let mut epoch_budgets = vec![config.maximum_epochs.min(50), config.maximum_epochs];
    epoch_budgets.sort_unstable();
    epoch_budgets.dedup();
    let mut best = None::<(f64, DnnCapacity, usize)>;
    for capacity in capacities {
        let encoder = FeatureEncoderV1::fit(
            &inner_training,
            capacity,
            config.validation_seed ^ 0x1a2b_3c4d,
        )?;
        let architecture = architecture_for(&inner_training, &encoder, capacity)?;
        if capacity == DnnCapacity::Full
            && !architecture.component_branch_enabled
            && !architecture.equipment_branch_enabled
        {
            continue;
        }
        for epochs in &epoch_budgets {
            let model = train_member(
                &inner_training,
                &encoder,
                &architecture,
                config.validation_seed ^ 0x55aa_7711,
                *epochs,
                &config.structural_fit_config,
                device,
            )?;
            let score = median(
                held_out
                    .iter()
                    .map(|row| {
                        let encoded = encoder.encode(&row.as_query())?;
                        let predicted = model.predict_one(&encoded, device)?.log_value;
                        Ok((predicted - row.asking_price_usd.ln()).abs())
                    })
                    .collect::<Result<Vec<_>, ValuationError>>()?,
            );
            if best
                .as_ref()
                .is_none_or(|(best_score, _, _)| score + 1e-4 < *best_score)
            {
                best = Some((score, capacity, *epochs));
            }
        }
    }
    Ok(best
        .map(|(_, capacity, epochs)| (capacity, epochs))
        .unwrap_or((
            DnnCapacity::Shared,
            selected_epoch_budget(rows, config.maximum_epochs),
        )))
}

#[cfg(test)]
fn initialized_anchor(rows: &[TrainingListing]) -> f64 {
    median(
        rows.iter()
            .map(|row| {
                let age = (row.snapshot_year - row.model_year).max(0) as f64;
                row.asking_price_usd.ln() - initialized_age_effect(age)
            })
            .collect(),
    )
}

#[cfg(test)]
fn initialized_age_effect(age: f64) -> f64 {
    -INITIAL_AGE_WEIGHTS
        .iter()
        .zip([1.0, 5.0, 15.0, 40.0])
        .map(|(weight, tau)| weight * (1.0 - (-age / tau).exp()))
        .sum::<f64>()
}

fn structural_age_weights(artifact: &StructuralArtifactV1) -> [f64; 4] {
    let taus = [1.0_f64, 5.0, 15.0, 40.0];
    let mut weights = INITIAL_AGE_WEIGHTS;
    for iteration in 0..800 {
        let mut gradient = [0.0; 4];
        for age in 0..=60 {
            let age = age as f64;
            let bases = taus.map(|tau| 1.0 - (-age / tau).exp());
            let target = -(artifact.age_floor
                + (1.0 - artifact.age_floor) * (-artifact.age_decay * age).exp())
            .ln();
            let predicted = weights
                .iter()
                .zip(bases)
                .map(|(weight, basis)| weight * basis)
                .sum::<f64>();
            for index in 0..4 {
                gradient[index] += 2.0 * (predicted - target) * bases[index] / 61.0;
            }
        }
        let learning_rate = 0.08 / (1.0 + iteration as f64 / 200.0);
        for index in 0..4 {
            weights[index] = (weights[index] - learning_rate * gradient[index]).max(1e-6);
        }
        let total = weights.iter().sum::<f64>();
        if total > 2.3 {
            for weight in &mut weights {
                *weight *= 2.3 / total;
            }
        }
    }
    weights
}

fn calibrated_initial_anchor(
    rows: &[TrainingListing],
    encoder: &FeatureEncoderV1,
    structural_anchor: f64,
    age_weights: [f64; 4],
    beta_hours: f64,
) -> f64 {
    let corrections = rows
        .iter()
        .map(|row| {
            let age = row.age();
            let age_effect = -age_weights
                .iter()
                .zip([1.0_f64, 5.0, 15.0, 40.0])
                .map(|(weight, tau)| weight * (1.0 - (-age / tau).exp()))
                .sum::<f64>();
            let hours_residual = row.airframe_hours.map_or(0.0, |hours| {
                hours.ln_1p()
                    - (encoder.expected_log_hours_intercept
                        + encoder.expected_log_hours_age_slope * age)
            });
            let hours_effect = (beta_hours * hours_residual).clamp(-0.35, 0.35);
            row.asking_price_usd.ln() - (structural_anchor + age_effect + hours_effect)
        })
        .collect();
    structural_anchor + median(corrections)
}

fn bootstrap_groups(rows: &[TrainingListing], seed: u64) -> Vec<TrainingListing> {
    let mut by_group = BTreeMap::<&str, Vec<&TrainingListing>>::new();
    for row in rows {
        by_group
            .entry(&row.duplicate_group_key)
            .or_default()
            .push(row);
    }
    let groups = by_group.keys().copied().collect::<Vec<_>>();
    let mut state = seed.max(1);
    let mut sampled = Vec::new();
    for draw in 0..groups.len() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let group = groups[state as usize % groups.len()];
        sampled.extend(by_group[group].iter().map(|row| {
            let mut cloned = (*row).clone();
            cloned.duplicate_group_key =
                format!("bootstrap-{seed}-{draw}-{}", cloned.duplicate_group_key);
            cloned
        }));
    }
    sampled
}

fn support_for_rows(training: &[TrainingListing], held_out: &TrainingListing) -> SupportGrade {
    let exact_variant =
        unique_matching_groups(training, |row| row.variant_id == held_out.variant_id);
    let near_variant = unique_matching_groups(training, |row| {
        row.variant_id == held_out.variant_id && feature_distance_is_near(row, held_out)
    });
    let near_model = unique_matching_groups(training, |row| {
        row.model_id == held_out.model_id && feature_distance_is_near(row, held_out)
    });
    let same_manufacturer = unique_matching_groups(training, |row| {
        row.manufacturer_id == held_out.manufacturer_id
    });
    if near_variant >= 5 {
        SupportGrade::High
    } else if near_variant >= 2
        || exact_variant >= 5
        || near_model >= 5
        || (exact_variant >= 2 && same_manufacturer >= 5)
    {
        SupportGrade::Medium
    } else {
        SupportGrade::Low
    }
}

fn feature_distance_is_near(left: &TrainingListing, right: &TrainingListing) -> bool {
    let age_near = (left.age() - right.age()).abs() <= 12.0;
    let hours_near = match (left.airframe_hours, right.airframe_hours) {
        (Some(left), Some(right)) => (left.ln_1p() - right.ln_1p()).abs() <= 2.5_f64.ln(),
        (None, None) => true,
        _ => false,
    };
    age_near && hours_near
}

fn unique_matching_groups(
    rows: &[TrainingListing],
    predicate: impl Fn(&TrainingListing) -> bool,
) -> usize {
    rows.iter()
        .filter(|row| predicate(row))
        .map(|row| row.duplicate_group_key.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

fn group_counts(rows: &[TrainingListing]) -> GroupCounts {
    let mut result = GroupCounts {
        total: distinct_groups(rows),
        ..GroupCounts::default()
    };
    let mut seen = BTreeSet::new();
    for row in rows {
        if !seen.insert(row.duplicate_group_key.as_str()) {
            continue;
        }
        increment(&mut result.categories, row.category_key.clone());
        *result.manufacturers.entry(row.manufacturer_id).or_default() += 1;
        *result.models.entry(row.model_id).or_default() += 1;
        *result.variants.entry(row.variant_id).or_default() += 1;
    }
    result
}

fn increment(counts: &mut BTreeMap<String, usize>, key: Option<String>) {
    if let Some(key) = key {
        *counts.entry(key).or_default() += 1;
    }
}

fn utilization_rates(rows: &[TrainingListing]) -> UtilizationRates {
    let rates = rows
        .iter()
        .filter_map(|row| {
            row.airframe_hours
                .map(|hours| hours / (row.snapshot_year - row.model_year).max(1) as f64)
        })
        .filter(|rate| rate.is_finite() && *rate >= 0.0)
        .collect::<Vec<_>>();
    let global = median(rates);
    let mut grouped = BTreeMap::<i64, Vec<f64>>::new();
    for row in rows {
        if let Some(hours) = row.airframe_hours {
            grouped
                .entry(row.model_id)
                .or_default()
                .push(hours / (row.snapshot_year - row.model_year).max(1) as f64);
        }
    }
    let models = grouped
        .into_iter()
        .filter(|(_, values)| values.len() >= 3)
        .map(|(model, values)| {
            let group_rate = median(values);
            let shrunk = (group_rate * 3.0 + global * 5.0) / 8.0;
            (model, shrunk)
        })
        .collect();
    UtilizationRates {
        global_hours_per_year: global,
        models,
        manufacturers: BTreeMap::new(),
    }
}

fn validate_rows(rows: &[TrainingListing]) -> Result<(), ValuationError> {
    let Some(first) = rows.first() else {
        return Err(ValuationError::Fit(
            "DNN fitting needs at least one current USD listing".to_string(),
        ));
    };
    for row in rows {
        row.validate()?;
        if row.snapshot_year != first.snapshot_year {
            return Err(ValuationError::Fit(
                "all DNN rows must share one snapshot year".to_string(),
            ));
        }
    }
    Ok(())
}

fn distinct_groups(rows: &[TrainingListing]) -> usize {
    rows.iter()
        .map(|row| row.duplicate_group_key.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold_prediction(fold: &str, error: f64) -> FoldPrediction {
        FoldPrediction {
            fold_id: fold.to_string(),
            duplicate_group_key: "aircraft-1".to_string(),
            listing_id: 1,
            manufacturer_id: 1,
            model_id: 1,
            variant_id: 1,
            actual_price_usd: 100.0,
            predicted_price_usd: 100.0 * (1.0 + error),
            log_error: (1.0 + error).ln(),
            absolute_percentage_error: error,
            signed_percentage_error: error,
            support: SupportGrade::Low,
        }
    }

    fn synthetic_rows(count: usize) -> Vec<TrainingListing> {
        (0..count)
            .map(|index| {
                let age = 2.0 + index as f64;
                let log_price = 12.0 + initialized_age_effect(age);
                TrainingListing {
                    listing_id: index as i64 + 1,
                    duplicate_group_key: format!("aircraft-{index}"),
                    snapshot_year: 2026,
                    asking_price_usd: log_price.exp(),
                    category_key: None,
                    manufacturer_id: 1,
                    model_id: (index % 3) as i64 + 1,
                    variant_id: (index % 6) as i64 + 1,
                    model_year: 2024 - index as i64,
                    airframe_hours: Some(age * 100.0),
                    engine_times: vec![],
                    propeller_times: vec![],
                    equipment_tokens: vec![],
                    valuation_facts: vec![],
                    technical_field_count: 5,
                }
            })
            .collect()
    }

    #[test]
    fn sample_size_capacity_is_progressive_and_reduces_for_poor_diversity() {
        assert_eq!(select_capacity(&synthetic_rows(8)), DnnCapacity::PriorOnly);
        assert_eq!(select_capacity(&synthetic_rows(20)), DnnCapacity::Shared);
        assert_eq!(
            select_capacity(&synthetic_rows(60)),
            DnnCapacity::Contextual
        );
        let mut poor = synthetic_rows(60);
        for row in &mut poor {
            row.model_id = 1;
        }
        assert_eq!(select_capacity(&poor), DnnCapacity::Shared);
    }

    #[test]
    fn bootstrap_resamples_complete_groups() {
        let rows = synthetic_rows(12);
        let first = bootstrap_groups(&rows, 17);
        let second = bootstrap_groups(&rows, 17);
        assert_eq!(first, second);
        assert_eq!(first.len(), rows.len());
    }

    #[test]
    fn paired_activation_comparison_keeps_repeated_folds_distinct() {
        let structural = vec![
            fold_prediction("fold-a", 0.10),
            fold_prediction("fold-b", 0.90),
        ];
        let dnn = vec![
            fold_prediction("fold-a", 0.20),
            fold_prediction("fold-b", 0.80),
        ];
        let report = evaluate_activation_gates(
            &dnn,
            &structural,
            &ValidationMetrics::default(),
            &ValidationMetrics::default(),
            true,
            &[true],
        );
        assert_eq!(report.paired_win_fraction, 0.5);
        assert!(!report.paired_win_gate);
    }

    #[test]
    fn seeded_training_improves_synthetic_held_out_loss_and_is_reproducible() {
        let mut rows = synthetic_rows(15);
        for row in &mut rows {
            row.asking_price_usd = 12.0_f64.exp();
            row.manufacturer_id = 1;
            row.model_id = 1;
            row.variant_id = 1;
            row.airframe_hours = None;
        }
        let held_indices = [2_usize, 7, 12];
        let training = rows
            .iter()
            .enumerate()
            .filter(|(index, _)| !held_indices.contains(index))
            .map(|(_, row)| row.clone())
            .collect::<Vec<_>>();
        let held_out = held_indices
            .iter()
            .map(|index| rows[*index].clone())
            .collect::<Vec<_>>();
        let capacity = DnnCapacity::Shared;
        let encoder = FeatureEncoderV1::fit(&training, capacity, 7).unwrap();
        let architecture = architecture_for(&training, &encoder, capacity).unwrap();
        let device = Default::default();
        let initial = TinyValuationNet::<Flex>::new(
            &architecture,
            initialized_anchor(&training),
            INITIAL_AGE_WEIGHTS,
            INITIAL_HOURS_BETA,
            &device,
        )
        .unwrap();
        let initial_error = held_out
            .iter()
            .map(|row| {
                let predicted = initial
                    .predict_one(&encoder.encode(&row.query()).unwrap(), &device)
                    .unwrap()
                    .log_value;
                (predicted - row.asking_price_usd.ln()).abs()
            })
            .sum::<f64>();
        let structural_config = StructuralFitConfig::default();
        let trained = train_member(
            &training,
            &encoder,
            &architecture,
            99,
            40,
            &structural_config,
            &device,
        )
        .unwrap();
        let repeated = train_member(
            &training,
            &encoder,
            &architecture,
            99,
            40,
            &structural_config,
            &device,
        )
        .unwrap();
        let mut trained_error = 0.0;
        for row in held_out {
            let input = encoder.encode(&row.query()).unwrap();
            let first = trained.predict_one(&input, &device).unwrap().log_value;
            let second = repeated.predict_one(&input, &device).unwrap().log_value;
            assert!((first - second).abs() < 1e-5);
            trained_error += (first - row.asking_price_usd.ln()).abs();
        }
        assert!(
            trained_error < initial_error,
            "{trained_error} !< {initial_error}"
        );
    }
}
