//! Experimental tiny monotone neural additive valuation model.

mod artifact;
mod features;
mod network;
mod store;
mod train;

pub use artifact::{DnnArtifactMetadataV1, DnnArtifactV1, MemberArtifact};
pub use features::{EncodedInput, FeatureEncoderV1, IdentityVocabulary};
pub use network::{DnnArchitectureConfig, ForwardBreakdown, TinyValuationNet};
pub use store::{
    activate_dnn_model_version, evaluate_candidate_gates, load_active_dnn_model, load_dnn_artifact,
    persist_dnn_candidate, structural_baseline_config, structural_baseline_id,
    validate_dnn_model_version, DnnStoredMetricsV1,
};
pub use train::{
    evaluate_activation_gates, evaluate_dnn, fit_dnn, fit_dnn_candidate, ActivationGateReport,
    DnnFitConfig, DnnFitReport, TrainingSchedule,
};

use std::sync::Arc;

use burn::backend::Flex;
use serde::{Deserialize, Serialize};

use crate::valuation::{
    DepreciationPoint, SupportGrade, ValuationBreakdown, ValuationError, ValuationEstimate,
    ValuationModel, ValuationQuery,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DnnCapacity {
    PriorOnly,
    Shared,
    Contextual,
    Full,
}

impl DnnCapacity {
    pub fn for_sample_size(deduplicated_listings: usize) -> Self {
        match deduplicated_listings {
            0..=9 => Self::PriorOnly,
            10..=49 => Self::Shared,
            50..=199 => Self::Contextual,
            _ => Self::Full,
        }
    }

    pub fn includes_shared(self) -> bool {
        self >= Self::Shared
    }

    pub fn includes_context(self) -> bool {
        self >= Self::Contextual
    }

    pub fn includes_full(self) -> bool {
        self >= Self::Full
    }
}

pub struct DnnValuationModel {
    metadata: DnnArtifactMetadataV1,
    members: Vec<TinyValuationNet<Flex>>,
    fallback: Option<Arc<dyn ValuationModel>>,
}

impl DnnValuationModel {
    pub fn load(
        artifact: &DnnArtifactV1,
        fallback: Option<Arc<dyn ValuationModel>>,
    ) -> Result<Self, ValuationError> {
        let device = Default::default();
        let members = artifact.load_members::<Flex>(&device)?;
        Ok(Self {
            metadata: artifact.metadata.clone(),
            members,
            fallback,
        })
    }

    pub fn metadata(&self) -> &DnnArtifactMetadataV1 {
        &self.metadata
    }

    fn member_predictions(
        &self,
        query: &ValuationQuery,
    ) -> Result<Vec<(usize, ForwardBreakdown)>, ValuationError> {
        let encoded = self.metadata.encoder.encode(query)?;
        let device = Default::default();
        let predictions = self
            .members
            .iter()
            .enumerate()
            .filter_map(
                |(index, model)| match model.predict_one(&encoded, &device) {
                    Ok(prediction) if prediction.log_value.is_finite() => Some((index, prediction)),
                    Ok(_) => None,
                    Err(error) => {
                        eprintln!("DNN ensemble member failed at runtime: {error}");
                        None
                    }
                },
            )
            .collect::<Vec<_>>();
        if predictions.is_empty() {
            return Err(ValuationError::InvalidArtifact(
                "all DNN ensemble members failed".to_string(),
            ));
        }
        Ok(predictions)
    }

    fn support(&self, query: &ValuationQuery) -> SupportGrade {
        let model_count = query
            .model_id
            .and_then(|id| self.metadata.group_counts.models.get(&id))
            .copied()
            .unwrap_or(0);
        let manufacturer_count = query
            .manufacturer_id
            .and_then(|id| self.metadata.group_counts.manufacturers.get(&id))
            .copied()
            .unwrap_or(0);
        if model_count >= 5 {
            SupportGrade::High
        } else if model_count >= 2 || manufacturer_count >= 5 {
            SupportGrade::Medium
        } else {
            SupportGrade::Low
        }
    }

    fn utilization(&self, query: &ValuationQuery) -> f64 {
        query
            .model_id
            .and_then(|id| self.metadata.utilization_rates.models.get(&id))
            .copied()
            .unwrap_or(self.metadata.utilization_rates.global_hours_per_year)
            .max(0.0)
    }
}

impl ValuationModel for DnnValuationModel {
    fn model_version_id(&self) -> i64 {
        self.metadata.model_version_id
    }

    fn model_kind(&self) -> &'static str {
        "dnn"
    }

    fn snapshot_id(&self) -> i64 {
        self.metadata.snapshot_id
    }

    fn estimate(&self, query: &ValuationQuery) -> Result<ValuationEstimate, ValuationError> {
        let current_predictions = match self.member_predictions(query) {
            Ok(predictions) => predictions,
            Err(error) => {
                if let Some(fallback) = &self.fallback {
                    return fallback.estimate(query);
                }
                return Err(error);
            }
        };
        let current_log_values = current_predictions
            .iter()
            .map(|(_, prediction)| prediction.log_value)
            .collect::<Vec<_>>();
        let current_log = median(current_log_values.clone());
        let estimated_value = current_log.exp();
        if !estimated_value.is_finite() || estimated_value <= 0.0 {
            return Err(ValuationError::InvalidArtifact(
                "DNN produced a nonfinite or nonpositive value".to_string(),
            ));
        }
        let support = self.support(query);
        let calibrated_error = self.metadata.error_bands.q80(support);
        let instability = current_log_values
            .iter()
            .map(|value| (value - current_log).abs())
            .fold(0.0_f64, f64::max);
        let reported_error = calibrated_error.max(instability);
        let breakdown = median_breakdown(
            &current_predictions
                .iter()
                .map(|(_, prediction)| *prediction)
                .collect::<Vec<_>>(),
        );
        let utilization = self.utilization(query);
        let mut depreciation = Vec::with_capacity(31);
        let mut previous_value = estimated_value;
        for horizon in 0_i64..=30 {
            let future_value = if horizon == 0 {
                estimated_value
            } else {
                let mut future_query = query.clone();
                future_query.valuation_year += horizon as i64;
                future_query.airframe_hours = query
                    .airframe_hours
                    .map(|hours| hours + utilization * horizon as f64);
                let encoded_future = self.metadata.encoder.encode(&future_query)?;
                let device = Default::default();
                let member_future_logs = current_predictions
                    .iter()
                    .filter_map(|(index, current)| {
                        self.members[*index]
                            .predict_one(&encoded_future, &device)
                            .ok()
                            .filter(|future| future.log_value.is_finite())
                            .map(|future| {
                                current.log_value + future.age_effect - current.age_effect
                                    + future.hours_effect
                                    - current.hours_effect
                            })
                    })
                    .collect::<Vec<_>>();
                if member_future_logs.is_empty() {
                    if let Some(fallback) = &self.fallback {
                        return fallback.estimate(query);
                    }
                    return Err(ValuationError::InvalidArtifact(format!(
                        "all DNN ensemble members failed at horizon {horizon}"
                    )));
                }
                median(member_future_logs).exp()
            };
            if !future_value.is_finite() || future_value <= 0.0 {
                return Err(ValuationError::InvalidArtifact(format!(
                    "DNN depreciation is invalid at horizon {horizon}"
                )));
            }
            if future_value > previous_value * (1.0 + self.metadata.floating_point_tolerance) {
                return Err(ValuationError::InvalidArtifact(format!(
                    "DNN depreciation increased at horizon {horizon}"
                )));
            }
            previous_value = future_value;
            let horizon_error = reported_error * (1.0 + 0.015 * horizon as f64);
            depreciation.push(DepreciationPoint {
                horizon_years: horizon,
                valuation_year: query.valuation_year + horizon,
                age_years: query.age() + horizon as f64,
                airframe_hours: query
                    .airframe_hours
                    .map(|hours| hours + utilization * horizon as f64),
                estimated_value_usd: future_value,
                low_value_usd: future_value * (-horizon_error).exp(),
                high_value_usd: future_value * horizon_error.exp(),
                depreciation_usd: estimated_value - future_value,
                depreciation_fraction: 1.0 - future_value / estimated_value,
                one_year_depreciation_fraction: 0.0,
                estimated_error_fraction: horizon_error.exp() - 1.0,
                support,
            });
        }
        for index in 0..depreciation.len().saturating_sub(1) {
            depreciation[index].one_year_depreciation_fraction = 1.0
                - depreciation[index + 1].estimated_value_usd
                    / depreciation[index].estimated_value_usd;
        }
        if depreciation.len() > 1 {
            let last = depreciation.len() - 1;
            depreciation[last].one_year_depreciation_fraction =
                depreciation[last - 1].one_year_depreciation_fraction;
        }
        Ok(ValuationEstimate {
            estimated_value_usd: estimated_value,
            low_value_usd: estimated_value * (-reported_error).exp(),
            high_value_usd: estimated_value * reported_error.exp(),
            estimated_error_fraction: reported_error.exp() - 1.0,
            support,
            model_version_id: self.metadata.model_version_id,
            snapshot_id: self.metadata.snapshot_id,
            model_kind: "dnn".to_string(),
            breakdown,
            depreciation,
        })
    }
}

fn median_breakdown(predictions: &[ForwardBreakdown]) -> ValuationBreakdown {
    let values =
        |select: fn(&ForwardBreakdown) -> f64| median(predictions.iter().map(select).collect());
    ValuationBreakdown {
        global_anchor_usd: values(|value| value.global_anchor).exp(),
        age_factor: values(|value| value.age_effect).exp(),
        expected_airframe_hours: 0.0,
        hours_residual: 0.0,
        hours_factor: values(|value| value.hours_effect).exp(),
        category_factor: 1.0,
        manufacturer_factor: values(|value| value.identity_offset).exp(),
        model_factor: 1.0,
        variant_factor: 1.0,
        optional_features_factor: values(|value| value.component_effect + value.residual).exp(),
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::valuation::{TrainingListing, ValuationModel};

    fn one_listing() -> TrainingListing {
        TrainingListing {
            listing_id: 1,
            duplicate_group_key: "N123".to_string(),
            snapshot_year: 2026,
            asking_price_usd: 180_000.0,
            category_key: None,
            manufacturer_id: 1,
            model_id: 2,
            variant_id: 3,
            model_year: 2006,
            airframe_hours: Some(2_000.0),
            engine_times: vec![],
            propeller_times: vec![],
            equipment_tokens: vec!["Garmin GNS 430W".to_string()],
            technical_field_count: 6,
        }
    }

    #[test]
    fn prior_only_artifact_round_trips_and_serves_unknown_identity_with_monotone_curve() {
        let row = one_listing();
        let config = DnnFitConfig {
            model_version_id: 12,
            snapshot_id: 9,
            baseline_model_version_id: 11,
            maximum_epochs: 1,
            ..DnnFitConfig::default()
        };
        let artifact = fit_dnn(&[row.clone()], &config).unwrap();
        assert_eq!(artifact.metadata.capacity, DnnCapacity::PriorOnly);
        let model = DnnValuationModel::load(&artifact, None).unwrap();
        let estimate = model.estimate(&row.query()).unwrap();
        assert!((estimate.estimated_value_usd / row.asking_price_usd - 1.0).abs() < 1e-4);
        assert_eq!(estimate.support, SupportGrade::Low);
        assert_eq!(estimate.depreciation.len(), 31);
        assert!(estimate.depreciation.windows(2).all(|points| {
            points[1].estimated_value_usd <= points[0].estimated_value_usd * 1.0001
        }));

        let mut unknown = row.query();
        unknown.manufacturer_id = Some(999);
        unknown.model_id = Some(999);
        unknown.variant_id = Some(999);
        let unknown_estimate = model.estimate(&unknown).unwrap();
        assert!(unknown_estimate.estimated_value_usd.is_finite());
        assert!(unknown_estimate.estimated_value_usd > 0.0);
    }

    #[test]
    fn corrupted_member_hash_prevents_loading() {
        let row = one_listing();
        let mut artifact = fit_dnn(
            &[row],
            &DnnFitConfig {
                maximum_epochs: 1,
                ..DnnFitConfig::default()
            },
        )
        .unwrap();
        artifact.members[0].bytes[0] ^= 0xff;
        assert!(DnnValuationModel::load(&artifact, None).is_err());
    }
}
