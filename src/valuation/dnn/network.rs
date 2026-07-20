use burn::module::{Initializer, Module, Param};
use burn::nn::{Dropout, DropoutConfig, Embedding, EmbeddingConfig, Linear, LinearConfig};
use burn::prelude::{Backend, Int, Tensor, TensorData};
use burn::tensor::activation::{sigmoid, silu, softmax, softplus};
use serde::{Deserialize, Serialize};

use crate::valuation::ValuationError;

use super::features::{EncodedInput, RESIDUAL_NUMERIC_FEATURES};
use super::DnnCapacity;

pub const MAX_DNN_PARAMETERS: usize = 10_000;
const CONTEXT_DIMENSIONS: [usize; 4] = [3, 3, 4, 4];
const PROJECTED_CONTEXT_DIMENSION: usize = 8;
const EQUIPMENT_EMBEDDING_DIMENSION: usize = 4;
const AGE_CATEGORY_DELTA_BOUND: f64 = 0.25;
pub const HOURS_EFFECT_BOUND: f64 = 0.35;
pub const COMPONENT_EFFECT_BOUND: f64 = 0.25;
pub const RESIDUAL_EFFECT_BOUND: f64 = 0.30;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DnnArchitectureConfig {
    pub capacity: DnnCapacity,
    pub category_count: usize,
    pub manufacturer_count: usize,
    pub model_count: usize,
    pub variant_count: usize,
    pub equipment_bucket_count: usize,
    pub maximum_equipment_items: usize,
    pub component_branch_enabled: bool,
    pub equipment_branch_enabled: bool,
    pub residual_feature_count: usize,
}

impl DnnArchitectureConfig {
    pub fn parameter_count(&self) -> usize {
        // Anchor, total age drop, four age mixture logits, and shared hours slope.
        let mut count = 7;
        if self.capacity.includes_shared() {
            count += self.category_count
                + self.manufacturer_count
                + self.model_count
                + self.variant_count;
        }
        if self.capacity.includes_context() {
            count += self.category_count * CONTEXT_DIMENSIONS[0]
                + self.manufacturer_count * CONTEXT_DIMENSIONS[1]
                + self.model_count * CONTEXT_DIMENSIONS[2]
                + self.variant_count * CONTEXT_DIMENSIONS[3];
            count += self.category_count * 4;
            count += 14 * PROJECTED_CONTEXT_DIMENSION + PROJECTED_CONTEXT_DIMENSION;
            let residual_input = PROJECTED_CONTEXT_DIMENSION
                + self.residual_feature_count
                + usize::from(self.equipment_branch_enabled) * EQUIPMENT_EMBEDDING_DIMENSION;
            count += residual_input * 24 + 24;
            count += 24 * 12 + 12;
            count += 12 + 1;
        }
        if self.equipment_branch_enabled {
            count += self.equipment_bucket_count * EQUIPMENT_EMBEDDING_DIMENSION;
        }
        if self.component_branch_enabled {
            count += 10;
        }
        count
    }

    pub fn validate(&self) -> Result<(), ValuationError> {
        if self.residual_feature_count != RESIDUAL_NUMERIC_FEATURES {
            return Err(ValuationError::Fit(format!(
                "DNN residual schema has {} fields; expected {}",
                self.residual_feature_count, RESIDUAL_NUMERIC_FEATURES
            )));
        }
        if self.parameter_count() > MAX_DNN_PARAMETERS {
            return Err(ValuationError::Fit(format!(
                "DNN architecture has {} parameters, exceeding the {} parameter limit",
                self.parameter_count(),
                MAX_DNN_PARAMETERS
            )));
        }
        if self.equipment_branch_enabled && !self.capacity.includes_full() {
            return Err(ValuationError::Fit(
                "equipment embeddings require full DNN capacity".to_string(),
            ));
        }
        if self.component_branch_enabled && !self.capacity.includes_full() {
            return Err(ValuationError::Fit(
                "component splines require full DNN capacity".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Module, Debug)]
pub struct TinyValuationNet<B: Backend> {
    pub global_anchor: Param<Tensor<B, 1>>,
    pub raw_total_age_drop: Param<Tensor<B, 1>>,
    pub raw_global_age_mix: Param<Tensor<B, 1>>,
    pub raw_beta_hours: Param<Tensor<B, 1>>,
    pub category_scalar: Option<Embedding<B>>,
    pub manufacturer_scalar: Option<Embedding<B>>,
    pub model_scalar: Option<Embedding<B>>,
    pub variant_scalar: Option<Embedding<B>>,
    pub category_context: Option<Embedding<B>>,
    pub manufacturer_context: Option<Embedding<B>>,
    pub model_context: Option<Embedding<B>>,
    pub variant_context: Option<Embedding<B>>,
    pub context_projection: Option<Linear<B>>,
    pub category_age_delta: Option<Embedding<B>>,
    pub equipment_embedding: Option<Embedding<B>>,
    pub component_raw_weights: Option<Param<Tensor<B, 2>>>,
    pub residual_hidden_1: Option<Linear<B>>,
    pub residual_hidden_2: Option<Linear<B>>,
    pub residual_output: Option<Linear<B>>,
    pub dropout_1: Dropout,
    pub dropout_2: Dropout,
}

pub struct EncodedBatch<B: Backend> {
    category: Tensor<B, 2, Int>,
    manufacturer: Tensor<B, 2, Int>,
    model: Tensor<B, 2, Int>,
    variant: Tensor<B, 2, Int>,
    age_basis: Tensor<B, 2>,
    hours_residual: Tensor<B, 2>,
    residual_features: Tensor<B, 2>,
    component_features: Tensor<B, 2>,
    equipment_indices: Tensor<B, 2, Int>,
    equipment_mask: Tensor<B, 2>,
}

pub struct ForwardTensors<B: Backend> {
    pub log_value: Tensor<B, 2>,
    pub global_anchor: Tensor<B, 2>,
    pub identity_offset: Tensor<B, 2>,
    pub age_effect: Tensor<B, 2>,
    pub hours_effect: Tensor<B, 2>,
    pub component_effect: Tensor<B, 2>,
    pub residual: Tensor<B, 2>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ForwardBreakdown {
    pub log_value: f64,
    pub global_anchor: f64,
    pub identity_offset: f64,
    pub age_effect: f64,
    pub hours_effect: f64,
    pub component_effect: f64,
    pub residual: f64,
}

impl<B: Backend> TinyValuationNet<B> {
    pub fn new(
        config: &DnnArchitectureConfig,
        initial_anchor: f64,
        initial_age_weights: [f64; 4],
        initial_hours_beta: f64,
        device: &B::Device,
    ) -> Result<Self, ValuationError> {
        config.validate()?;
        let total_drop = initial_age_weights.iter().sum::<f64>().clamp(1e-4, 2.2999);
        let raw_total_drop = (total_drop / (2.3 - total_drop)).ln();
        let raw_mix = initial_age_weights.map(|weight| (weight.max(1e-6) / total_drop).ln() as f32);
        let raw_hours = inverse_softplus((-initial_hours_beta).max(1e-6));
        let zero_embedding = |count: usize, dimensions: usize| {
            EmbeddingConfig::new(count.max(1), dimensions)
                .with_initializer(Initializer::Zeros)
                .init(device)
        };
        let zero_linear = |input: usize, output: usize| {
            LinearConfig::new(input, output)
                .with_initializer(Initializer::Zeros)
                .init(device)
        };
        let small_linear =
            |input: usize, output: usize| LinearConfig::new(input, output).init(device);

        let shared = config.capacity.includes_shared();
        let contextual = config.capacity.includes_context();
        let residual_input = PROJECTED_CONTEXT_DIMENSION
            + config.residual_feature_count
            + usize::from(config.equipment_branch_enabled) * EQUIPMENT_EMBEDDING_DIMENSION;
        Ok(Self {
            global_anchor: Param::from_data([initial_anchor as f32], device),
            raw_total_age_drop: Param::from_data([raw_total_drop as f32], device),
            raw_global_age_mix: Param::from_data(raw_mix, device),
            raw_beta_hours: Param::from_data([raw_hours as f32], device),
            category_scalar: shared.then(|| zero_embedding(config.category_count, 1)),
            manufacturer_scalar: shared.then(|| zero_embedding(config.manufacturer_count, 1)),
            model_scalar: shared.then(|| zero_embedding(config.model_count, 1)),
            variant_scalar: shared.then(|| zero_embedding(config.variant_count, 1)),
            category_context: contextual
                .then(|| zero_embedding(config.category_count, CONTEXT_DIMENSIONS[0])),
            manufacturer_context: contextual
                .then(|| zero_embedding(config.manufacturer_count, CONTEXT_DIMENSIONS[1])),
            model_context: contextual
                .then(|| zero_embedding(config.model_count, CONTEXT_DIMENSIONS[2])),
            variant_context: contextual
                .then(|| zero_embedding(config.variant_count, CONTEXT_DIMENSIONS[3])),
            context_projection: contextual.then(|| small_linear(14, PROJECTED_CONTEXT_DIMENSION)),
            category_age_delta: contextual.then(|| zero_embedding(config.category_count, 4)),
            equipment_embedding: config.equipment_branch_enabled.then(|| {
                zero_embedding(config.equipment_bucket_count, EQUIPMENT_EMBEDDING_DIMENSION)
            }),
            component_raw_weights: config
                .component_branch_enabled
                .then(|| Param::from_data(TensorData::full([2, 5], -20.0_f32), device)),
            residual_hidden_1: contextual.then(|| small_linear(residual_input, 24)),
            residual_hidden_2: contextual.then(|| small_linear(24, 12)),
            residual_output: contextual.then(|| zero_linear(12, 1)),
            dropout_1: DropoutConfig::new(0.10).init(),
            dropout_2: DropoutConfig::new(0.10).init(),
        })
    }

    pub fn forward(&self, batch: EncodedBatch<B>) -> ForwardTensors<B> {
        let batch_size = batch.age_basis.shape().dims::<2>()[0];
        let global_anchor = self
            .global_anchor
            .val()
            .reshape([1, 1])
            .repeat_dim(0, batch_size);
        let identity_offset = self.identity_offset(&batch, batch_size);

        let total_drop = sigmoid(self.raw_total_age_drop.val())
            .mul_scalar(2.3)
            .reshape([1, 1]);
        let global_mix = self.raw_global_age_mix.val().reshape([1, 4]);
        let mix_logits = match &self.category_age_delta {
            Some(delta) => {
                let delta = delta
                    .forward(batch.category.clone())
                    .squeeze_dim::<2>(1)
                    .clamp(-AGE_CATEGORY_DELTA_BOUND, AGE_CATEGORY_DELTA_BOUND)
                    * batch.category.clone().greater_elem(0).float();
                global_mix.repeat_dim(0, batch_size) + delta
            }
            None => global_mix.repeat_dim(0, batch_size),
        };
        let age_weights = softmax(mix_logits, 1) * total_drop;
        let age_effect = -(batch.age_basis.clone() * age_weights).sum_dim(1);

        let beta_hours = -softplus(self.raw_beta_hours.val(), 1.0).reshape([1, 1]);
        let hours_effect = (batch.hours_residual.clone() * beta_hours)
            .clamp(-HOURS_EFFECT_BOUND, HOURS_EFFECT_BOUND);
        let component_effect = match &self.component_raw_weights {
            Some(raw_weights) => {
                let direction = Tensor::<B, 2>::from_data(
                    TensorData::new(
                        vec![0.0_f32, -1.0, -1.0, -1.0, 1.0, 0.0, -1.0, -1.0, -1.0, 1.0],
                        [2, 5],
                    ),
                    &batch.component_features.device(),
                );
                let weights = softplus(raw_weights.val(), 1.0) * direction;
                (batch.component_features.clone() * weights.reshape([1, 10]))
                    .sum_dim(1)
                    .clamp(-COMPONENT_EFFECT_BOUND, COMPONENT_EFFECT_BOUND)
            }
            None => Tensor::zeros([batch_size, 1], &batch.age_basis.device()),
        };

        let residual = self.residual(&batch, batch_size);
        let log_value = global_anchor.clone()
            + identity_offset.clone()
            + age_effect.clone()
            + hours_effect.clone()
            + component_effect.clone()
            + residual.clone();
        ForwardTensors {
            log_value,
            global_anchor,
            identity_offset,
            age_effect,
            hours_effect,
            component_effect,
            residual,
        }
    }

    pub fn predict_one(
        &self,
        encoded: &EncodedInput,
        device: &B::Device,
    ) -> Result<ForwardBreakdown, ValuationError> {
        let output = self.forward(EncodedBatch::from_inputs(
            std::slice::from_ref(encoded),
            device,
        )?);
        let scalar = |tensor: Tensor<B, 2>| -> Result<f64, ValuationError> {
            tensor
                .to_data()
                .to_vec::<f32>()
                .map_err(|error| ValuationError::InvalidArtifact(error.to_string()))
                .and_then(|values| {
                    values.first().copied().map(f64::from).ok_or_else(|| {
                        ValuationError::InvalidArtifact("empty DNN output".to_string())
                    })
                })
        };
        let result = ForwardBreakdown {
            log_value: scalar(output.log_value)?,
            global_anchor: scalar(output.global_anchor)?,
            identity_offset: scalar(output.identity_offset)?,
            age_effect: scalar(output.age_effect)?,
            hours_effect: scalar(output.hours_effect)?,
            component_effect: scalar(output.component_effect)?,
            residual: scalar(output.residual)?,
        };
        if !result.log_value.is_finite() {
            return Err(ValuationError::InvalidArtifact(
                "DNN returned a nonfinite smoke prediction".to_string(),
            ));
        }
        Ok(result)
    }

    fn identity_offset(&self, batch: &EncodedBatch<B>, batch_size: usize) -> Tensor<B, 2> {
        let mut result = Tensor::zeros([batch_size, 1], &batch.age_basis.device());
        for (embedding, indices) in [
            (&self.category_scalar, &batch.category),
            (&self.manufacturer_scalar, &batch.manufacturer),
            (&self.model_scalar, &batch.model),
            (&self.variant_scalar, &batch.variant),
        ] {
            if let Some(embedding) = embedding {
                let known = indices.clone().greater_elem(0).float();
                result = result + embedding.forward(indices.clone()).squeeze_dim::<2>(1) * known;
            }
        }
        result
    }

    fn residual(&self, batch: &EncodedBatch<B>, batch_size: usize) -> Tensor<B, 2> {
        let Some(context_projection) = &self.context_projection else {
            return Tensor::zeros([batch_size, 1], &batch.age_basis.device());
        };
        let context = Tensor::cat(
            vec![
                self.category_context
                    .as_ref()
                    .expect("context architecture")
                    .forward(batch.category.clone())
                    .squeeze_dim::<2>(1)
                    * batch.category.clone().greater_elem(0).float(),
                self.manufacturer_context
                    .as_ref()
                    .expect("context architecture")
                    .forward(batch.manufacturer.clone())
                    .squeeze_dim::<2>(1)
                    * batch.manufacturer.clone().greater_elem(0).float(),
                self.model_context
                    .as_ref()
                    .expect("context architecture")
                    .forward(batch.model.clone())
                    .squeeze_dim::<2>(1)
                    * batch.model.clone().greater_elem(0).float(),
                self.variant_context
                    .as_ref()
                    .expect("context architecture")
                    .forward(batch.variant.clone())
                    .squeeze_dim::<2>(1)
                    * batch.variant.clone().greater_elem(0).float(),
            ],
            1,
        );
        let context = context_projection.forward(context);
        let mut residual_inputs = vec![context, batch.residual_features.clone()];
        if let Some(equipment_embedding) = &self.equipment_embedding {
            let embedded = equipment_embedding.forward(batch.equipment_indices.clone());
            let mask = batch.equipment_mask.clone().unsqueeze_dim::<3>(2);
            let denominator = batch
                .equipment_mask
                .clone()
                .sum_dim(1)
                .clamp(1.0, f64::INFINITY);
            let pooled = (embedded * mask).sum_dim(1).squeeze_dim::<2>(1) / denominator;
            residual_inputs.push(pooled);
        }
        let input = Tensor::cat(residual_inputs, 1);
        let hidden = self.dropout_1.forward(silu(
            self.residual_hidden_1
                .as_ref()
                .expect("residual architecture")
                .forward(input),
        ));
        let hidden = self.dropout_2.forward(silu(
            self.residual_hidden_2
                .as_ref()
                .expect("residual architecture")
                .forward(hidden),
        ));
        self.residual_output
            .as_ref()
            .expect("residual architecture")
            .forward(hidden)
            .tanh()
            .mul_scalar(RESIDUAL_EFFECT_BOUND)
    }
}

impl<B: Backend> EncodedBatch<B> {
    pub fn from_inputs(
        inputs: &[EncodedInput],
        device: &B::Device,
    ) -> Result<Self, ValuationError> {
        let Some(first) = inputs.first() else {
            return Err(ValuationError::Fit(
                "cannot build an empty DNN batch".to_string(),
            ));
        };
        let batch = inputs.len();
        let maximum_equipment_items = first.equipment_indices.len();
        if inputs.iter().any(|input| {
            input.equipment_indices.len() != maximum_equipment_items
                || input.equipment_mask.len() != maximum_equipment_items
        }) {
            return Err(ValuationError::Fit(
                "encoded DNN equipment shapes differ".to_string(),
            ));
        }
        let indices = |values: Vec<i64>| {
            Tensor::<B, 2, Int>::from_data(TensorData::new(values, [batch, 1]), device)
        };
        let floats = |values: Vec<f32>, width: usize| {
            Tensor::<B, 2>::from_data(TensorData::new(values, [batch, width]), device)
        };
        let mut component_features = vec![0.0_f32; batch * 10];
        for (row_index, input) in inputs.iter().enumerate() {
            for (type_index, components) in [
                input.engine_components.as_slice(),
                input.propeller_components.as_slice(),
            ]
            .into_iter()
            .enumerate()
            {
                let total_count = components
                    .iter()
                    .map(|value| value.count)
                    .sum::<f32>()
                    .max(1.0);
                for component in components {
                    if component.basis_index < 5 {
                        component_features
                            [row_index * 10 + type_index * 5 + component.basis_index] +=
                            component.log_hours * component.count / total_count;
                    }
                }
            }
        }
        Ok(Self {
            category: indices(
                inputs
                    .iter()
                    .map(|input| input.category_index as i64)
                    .collect(),
            ),
            manufacturer: indices(
                inputs
                    .iter()
                    .map(|input| input.manufacturer_index as i64)
                    .collect(),
            ),
            model: indices(
                inputs
                    .iter()
                    .map(|input| input.model_index as i64)
                    .collect(),
            ),
            variant: indices(
                inputs
                    .iter()
                    .map(|input| input.variant_index as i64)
                    .collect(),
            ),
            age_basis: floats(inputs.iter().flat_map(|input| input.age_basis).collect(), 4),
            hours_residual: floats(inputs.iter().map(|input| input.hours_residual).collect(), 1),
            residual_features: floats(
                inputs
                    .iter()
                    .flat_map(|input| input.residual_features)
                    .collect(),
                RESIDUAL_NUMERIC_FEATURES,
            ),
            component_features: floats(component_features, 10),
            equipment_indices: Tensor::<B, 2, Int>::from_data(
                TensorData::new(
                    inputs
                        .iter()
                        .flat_map(|input| input.equipment_indices.iter().map(|index| *index as i64))
                        .collect(),
                    [batch, maximum_equipment_items],
                ),
                device,
            ),
            equipment_mask: floats(
                inputs
                    .iter()
                    .flat_map(|input| input.equipment_mask.iter().copied())
                    .collect(),
                maximum_equipment_items,
            ),
        })
    }
}

fn inverse_softplus(value: f64) -> f64 {
    value.exp_m1().ln()
}

#[cfg(test)]
mod tests {
    use burn::backend::Flex;
    use burn::module::Module;

    use super::super::features::EncodedComponent;
    use super::*;
    use crate::valuation::ComponentTimeBasis;

    fn config(capacity: DnnCapacity) -> DnnArchitectureConfig {
        DnnArchitectureConfig {
            capacity,
            category_count: 2,
            manufacturer_count: 4,
            model_count: 6,
            variant_count: 8,
            equipment_bucket_count: 128,
            maximum_equipment_items: 16,
            component_branch_enabled: capacity.includes_full(),
            equipment_branch_enabled: capacity.includes_full(),
            residual_feature_count: RESIDUAL_NUMERIC_FEATURES,
        }
    }

    #[test]
    fn every_capacity_stays_inside_the_parameter_budget() {
        for capacity in [
            DnnCapacity::PriorOnly,
            DnnCapacity::Shared,
            DnnCapacity::Contextual,
            DnnCapacity::Full,
        ] {
            let config = config(capacity);
            assert!(config.parameter_count() <= MAX_DNN_PARAMETERS);
            let model = TinyValuationNet::<Flex>::new(
                &config,
                11.0,
                [0.08, 0.18, 0.42, 0.52],
                -0.02,
                &Default::default(),
            )
            .unwrap();
            assert_eq!(config.parameter_count(), model.num_params());
        }
    }

    fn encoded(age: f32, hours_residual: f32) -> EncodedInput {
        EncodedInput {
            category_index: 0,
            manufacturer_index: 0,
            model_index: 0,
            variant_index: 0,
            age_years: age,
            age_basis: [1.0, 5.0, 15.0, 40.0].map(|tau| 1.0 - (-age / tau).exp()),
            hours_residual,
            residual_features: [0.0; RESIDUAL_NUMERIC_FEATURES],
            engine_components: vec![],
            propeller_components: vec![],
            equipment_indices: vec![0; 16],
            equipment_mask: vec![0.0; 16],
        }
    }

    #[test]
    fn age_and_above_typical_hours_are_monotone_and_tower_bounds_hold() {
        let device = Default::default();
        let model = TinyValuationNet::<Flex>::new(
            &config(DnnCapacity::Full),
            12.0,
            [0.08, 0.18, 0.42, 0.52],
            -0.02,
            &device,
        )
        .unwrap();
        let mut previous = f64::INFINITY;
        for age in 0..=300 {
            let prediction = model
                .predict_one(&encoded(age as f32 / 10.0, 0.0), &device)
                .unwrap();
            assert!(prediction.age_effect <= previous + 1e-6);
            assert!(prediction.age_effect >= -2.3 - 1e-6);
            assert!(prediction.residual.abs() <= RESIDUAL_EFFECT_BOUND + 1e-6);
            assert!(prediction.component_effect.abs() <= COMPONENT_EFFECT_BOUND + 1e-6);
            previous = prediction.age_effect;
        }
        let typical = model.predict_one(&encoded(20.0, 0.0), &device).unwrap();
        let high = model.predict_one(&encoded(20.0, 10.0), &device).unwrap();
        assert!(high.log_value <= typical.log_value);
        assert!(high.hours_effect >= -HOURS_EFFECT_BOUND - 1e-6);
    }

    #[test]
    fn component_order_and_masked_equipment_padding_do_not_change_prediction() {
        let device = Default::default();
        let model = TinyValuationNet::<Flex>::new(
            &config(DnnCapacity::Full),
            12.0,
            [0.08, 0.18, 0.42, 0.52],
            -0.02,
            &device,
        )
        .unwrap();
        let component = |hours| EncodedComponent {
            log_hours: hours,
            count: 1.0,
            basis_index: ComponentTimeBasis::SinceOverhaul as usize,
        };
        let mut first = encoded(20.0, 0.0);
        first.engine_components = vec![component(4.0), component(6.0)];
        first.equipment_indices[0] = 4;
        first.equipment_mask[0] = 1.0;
        let mut second = first.clone();
        second.engine_components.reverse();
        second.equipment_indices[8] = 97;
        second.equipment_mask[8] = 0.0;
        let left = model.predict_one(&first, &device).unwrap();
        let right = model.predict_one(&second, &device).unwrap();
        assert!((left.log_value - right.log_value).abs() < 1e-6);
    }
}
