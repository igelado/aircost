use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::valuation::{
    ComponentObservation, ComponentTimeBasis, TrainingListing, ValuationError, ValuationQuery,
};

use super::DnnCapacity;

pub const FEATURE_SCHEMA_VERSION: u32 = crate::valuation::FEATURE_SCHEMA_VERSION;
pub const DEFAULT_EQUIPMENT_BUCKETS: usize = 128;
pub const DEFAULT_MAX_EQUIPMENT_ITEMS: usize = 16;
pub const RESIDUAL_NUMERIC_FEATURES: usize = 7;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct IdentityVocabulary {
    /// Index zero is always unknown.
    pub entries: BTreeMap<String, usize>,
}

impl IdentityVocabulary {
    fn from_values(
        values: impl Iterator<Item = Option<String>>,
        minimum_count: usize,
        maximum_entries: usize,
    ) -> Self {
        let mut counts = BTreeMap::<String, usize>::new();
        for value in values.flatten() {
            *counts.entry(value).or_default() += 1;
        }
        let mut supported = counts
            .into_iter()
            .filter(|(_, count)| *count >= minimum_count)
            .collect::<Vec<_>>();
        supported.sort_by(|(left_key, left_count), (right_key, right_count)| {
            right_count
                .cmp(left_count)
                .then_with(|| left_key.cmp(right_key))
        });
        let mut retained = supported
            .into_iter()
            .take(maximum_entries)
            .map(|(key, _)| key)
            .collect::<Vec<_>>();
        retained.sort();
        let entries = retained
            .into_iter()
            .enumerate()
            .map(|(index, value)| (value, index + 1))
            .collect();
        Self { entries }
    }

    pub fn index(&self, value: Option<&str>) -> usize {
        value
            .and_then(|value| self.entries.get(value))
            .copied()
            .unwrap_or(0)
    }

    pub fn len_with_unknown(&self) -> usize {
        self.entries.len() + 1
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RobustNumericScale {
    pub median: f64,
    pub scale: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FeatureEncoderV1 {
    pub feature_schema_version: u32,
    pub category_vocabulary: IdentityVocabulary,
    pub manufacturer_vocabulary: IdentityVocabulary,
    pub model_vocabulary: IdentityVocabulary,
    pub variant_vocabulary: IdentityVocabulary,
    pub numeric_field_order: Vec<String>,
    pub numeric_scales: BTreeMap<String, RobustNumericScale>,
    pub missingness_field_order: Vec<String>,
    pub equipment_hash_seed: u64,
    pub equipment_bucket_count: usize,
    pub maximum_equipment_items: usize,
    pub component_time_basis_vocabulary: Vec<ComponentTimeBasis>,
    pub expected_log_hours_intercept: f64,
    pub expected_log_hours_age_slope: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EncodedComponent {
    pub log_hours: f32,
    pub count: f32,
    pub basis_index: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EncodedInput {
    pub category_index: usize,
    pub manufacturer_index: usize,
    pub model_index: usize,
    pub variant_index: usize,
    pub age_years: f32,
    pub age_basis: [f32; 4],
    pub hours_residual: f32,
    pub residual_features: [f32; RESIDUAL_NUMERIC_FEATURES],
    pub engine_components: Vec<EncodedComponent>,
    pub propeller_components: Vec<EncodedComponent>,
    pub equipment_indices: Vec<usize>,
    pub equipment_mask: Vec<f32>,
}

impl FeatureEncoderV1 {
    pub fn fit(
        rows: &[TrainingListing],
        capacity: DnnCapacity,
        equipment_hash_seed: u64,
    ) -> Result<Self, ValuationError> {
        if rows.is_empty() {
            return Err(ValuationError::Fit(
                "a DNN feature encoder needs at least one listing".to_string(),
            ));
        }
        for row in rows {
            row.validate()?;
        }
        let unique_groups = rows
            .iter()
            .map(|row| row.duplicate_group_key.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        let context_minimum = if capacity.includes_context() && unique_groups >= 50 {
            2
        } else {
            1
        };
        let maximums = if capacity.includes_context() {
            [128, 256, 384, 384]
        } else {
            [1_500; 4]
        };
        let category_vocabulary = IdentityVocabulary::from_values(
            rows.iter().map(|row| row.category_key.clone()),
            context_minimum,
            maximums[0],
        );
        let manufacturer_vocabulary = IdentityVocabulary::from_values(
            rows.iter().map(|row| id_key(Some(row.manufacturer_id))),
            context_minimum,
            maximums[1],
        );
        let model_vocabulary = IdentityVocabulary::from_values(
            rows.iter().map(|row| id_key(Some(row.model_id))),
            context_minimum,
            maximums[2],
        );
        let variant_vocabulary = IdentityVocabulary::from_values(
            rows.iter().map(|row| id_key(Some(row.variant_id))),
            context_minimum,
            maximums[3],
        );

        let ages = rows
            .iter()
            .map(|row| (row.snapshot_year - row.model_year).max(0) as f64)
            .collect::<Vec<_>>();
        let hours = rows
            .iter()
            .filter_map(|row| row.airframe_hours.map(|hours| hours.ln_1p()))
            .collect::<Vec<_>>();
        let (expected_log_hours_intercept, expected_log_hours_age_slope) = robust_hours_trend(rows);
        let numeric_scales = BTreeMap::from([
            ("age_years".to_string(), robust_scale(&ages)),
            ("log1p_airframe_hours".to_string(), robust_scale(&hours)),
            (
                "technical_field_count".to_string(),
                robust_scale(
                    &rows
                        .iter()
                        .map(|row| (row.technical_field_count as f64).ln_1p())
                        .collect::<Vec<_>>(),
                ),
            ),
        ]);

        Ok(Self {
            feature_schema_version: FEATURE_SCHEMA_VERSION,
            category_vocabulary,
            manufacturer_vocabulary,
            model_vocabulary,
            variant_vocabulary,
            numeric_field_order: vec![
                "hours_residual".to_string(),
                "technical_field_count".to_string(),
                "equipment_count".to_string(),
                "modification_count".to_string(),
            ],
            numeric_scales,
            missingness_field_order: vec![
                "airframe_hours_missing".to_string(),
                "engine_time_missing".to_string(),
                "propeller_time_missing".to_string(),
            ],
            equipment_hash_seed,
            equipment_bucket_count: DEFAULT_EQUIPMENT_BUCKETS,
            maximum_equipment_items: DEFAULT_MAX_EQUIPMENT_ITEMS,
            component_time_basis_vocabulary: vec![
                ComponentTimeBasis::Unknown,
                ComponentTimeBasis::SinceNew,
                ComponentTimeBasis::SinceOverhaul,
                ComponentTimeBasis::SinceInspection,
                ComponentTimeBasis::TimeRemaining,
            ],
            expected_log_hours_intercept,
            expected_log_hours_age_slope,
        })
    }

    pub fn encode(&self, query: &ValuationQuery) -> Result<EncodedInput, ValuationError> {
        if self.feature_schema_version != FEATURE_SCHEMA_VERSION {
            return Err(ValuationError::InvalidArtifact(format!(
                "unsupported DNN feature schema version {}",
                self.feature_schema_version
            )));
        }
        let age = query.age_years()?;
        let age_basis = [1.0, 5.0, 15.0, 40.0].map(|tau| (1.0 - (-age / tau).exp()) as f32);
        let airframe_missing = query.airframe_hours.is_none();
        let hours_residual = query.airframe_hours.map_or(0.0, |hours| {
            hours.ln_1p()
                - (self.expected_log_hours_intercept + self.expected_log_hours_age_slope * age)
        });
        let engines_missing = query
            .engine_times
            .iter()
            .all(|value| value.time_hours.is_none());
        let propellers_missing = query
            .propeller_times
            .iter()
            .all(|value| value.time_hours.is_none());
        let known = normalize(
            (query.technical_field_count as f64).ln_1p(),
            self.numeric_scales.get("technical_field_count"),
        );
        let residual_features = [
            airframe_missing as u8 as f32,
            engines_missing as u8 as f32,
            propellers_missing as u8 as f32,
            known as f32,
            (query.equipment_tokens.len() as f64).ln_1p() as f32,
            0.0,
            0.0,
        ];

        let mut equipment_indices = vec![0; self.maximum_equipment_items];
        let mut equipment_mask = vec![0.0; self.maximum_equipment_items];
        for (index, token) in query
            .equipment_tokens
            .iter()
            .take(self.maximum_equipment_items)
            .enumerate()
        {
            equipment_indices[index] = self.equipment_bucket(token);
            equipment_mask[index] = 1.0;
        }
        let manufacturer_key = id_key(query.manufacturer_id);
        let model_key = id_key(query.model_id);
        let variant_key = id_key(query.variant_id);
        Ok(EncodedInput {
            category_index: self
                .category_vocabulary
                .index(query.category_key.as_deref()),
            manufacturer_index: self
                .manufacturer_vocabulary
                .index(manufacturer_key.as_deref()),
            model_index: self.model_vocabulary.index(model_key.as_deref()),
            variant_index: self.variant_vocabulary.index(variant_key.as_deref()),
            age_years: age as f32,
            age_basis,
            hours_residual: hours_residual as f32,
            residual_features,
            engine_components: self.encode_components(&query.engine_times),
            propeller_components: self.encode_components(&query.propeller_times),
            equipment_indices,
            equipment_mask,
        })
    }

    pub fn encoded_source_features(&self) -> Vec<&'static str> {
        vec![
            "category_key",
            "manufacturer_id",
            "model_id",
            "variant_id",
            "model_year",
            "valuation_year",
            "airframe_hours",
            "engine_times",
            "propeller_times",
            "equipment_tokens",
            "technical_field_count",
        ]
    }

    fn equipment_bucket(&self, token: &str) -> usize {
        if self.equipment_bucket_count <= 2 || token.trim().is_empty() {
            return 1;
        }
        let mut hasher = Sha256::new();
        hasher.update(self.equipment_hash_seed.to_le_bytes());
        hasher.update(token.trim().to_lowercase().as_bytes());
        let hash = hasher.finalize();
        let prefix = u64::from_le_bytes(hash[..8].try_into().expect("SHA-256 prefix"));
        2 + prefix as usize % (self.equipment_bucket_count - 2)
    }

    fn encode_components(&self, components: &[ComponentObservation]) -> Vec<EncodedComponent> {
        components
            .iter()
            .filter_map(|component| {
                component.time_hours.map(|hours| EncodedComponent {
                    log_hours: hours.max(0.0).ln_1p() as f32,
                    count: component.count.max(1) as f32,
                    basis_index: self
                        .component_time_basis_vocabulary
                        .iter()
                        .position(|basis| *basis == component.basis)
                        .unwrap_or(0),
                })
            })
            .collect()
    }
}

fn id_key(id: Option<i64>) -> Option<String> {
    id.map(|id| format!("id:{id}"))
}

fn robust_hours_trend(rows: &[TrainingListing]) -> (f64, f64) {
    let points = rows
        .iter()
        .filter_map(|row| {
            row.airframe_hours.map(|hours| {
                (
                    (row.snapshot_year - row.model_year).max(0) as f64,
                    hours.ln_1p(),
                )
            })
        })
        .collect::<Vec<_>>();
    if points.is_empty() {
        return (0.0, 0.0);
    }
    let median_age = median(points.iter().map(|point| point.0).collect());
    let median_hours = median(points.iter().map(|point| point.1).collect());
    let mut slopes = Vec::new();
    for left in 0..points.len() {
        for right in left + 1..points.len() {
            let age_delta = points[right].0 - points[left].0;
            if age_delta.abs() > 1e-9 {
                slopes.push((points[right].1 - points[left].1) / age_delta);
            }
        }
    }
    let slope = median(slopes).clamp(0.0, 1.0);
    (median_hours - slope * median_age, slope)
}

fn robust_scale(values: &[f64]) -> RobustNumericScale {
    if values.is_empty() {
        return RobustNumericScale {
            median: 0.0,
            scale: 1.0,
        };
    }
    let median_value = median(values.to_vec());
    let deviation = median(
        values
            .iter()
            .map(|value| (value - median_value).abs())
            .collect(),
    );
    RobustNumericScale {
        median: median_value,
        scale: (1.4826 * deviation).max(1e-6),
    }
}

fn normalize(value: f64, scale: Option<&RobustNumericScale>) -> f64 {
    scale.map_or(value, |scale| {
        ((value - scale.median) / scale.scale).clamp(-5.0, 5.0)
    })
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    if values.len() % 2 == 0 {
        (values[values.len() / 2 - 1] + values[values.len() / 2]) * 0.5
    } else {
        values[values.len() / 2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing() -> TrainingListing {
        TrainingListing {
            listing_id: 42,
            duplicate_group_key: "N42".to_string(),
            snapshot_year: 2026,
            asking_price_usd: 123_456.0,
            category_key: None,
            manufacturer_id: 10,
            model_id: 20,
            variant_id: 30,
            model_year: 2006,
            airframe_hours: Some(2_000.0),
            engine_times: vec![],
            propeller_times: vec![],
            equipment_tokens: vec!["Garmin G1000".to_string()],
            valuation_facts: vec![],
            technical_field_count: 7,
        }
    }

    #[test]
    fn encoded_contract_contains_only_listing_features_and_no_target_or_source_artifacts() {
        let row = listing();
        let encoder = FeatureEncoderV1::fit(&[row.clone()], DnnCapacity::PriorOnly, 7).unwrap();
        let encoded = encoder.encode(&row.query()).unwrap();
        let json = serde_json::to_string(&encoded).unwrap();
        assert!(!json.contains("123456"));
        let fields = encoder.encoded_source_features();
        for forbidden in [
            "asking_price_usd",
            "source_url",
            "source_site",
            "financing",
            "seller_contact",
        ] {
            assert!(!fields.contains(&forbidden));
        }
    }

    #[test]
    fn padding_has_a_zero_mask_and_stable_hashes_are_repeatable() {
        let row = listing();
        let encoder = FeatureEncoderV1::fit(&[row.clone()], DnnCapacity::Full, 91).unwrap();
        let first = encoder.encode(&row.query()).unwrap();
        let second = encoder.encode(&row.query()).unwrap();
        assert_eq!(first.equipment_indices, second.equipment_indices);
        assert_eq!(first.equipment_mask[0], 1.0);
        assert!(first.equipment_mask[1..].iter().all(|mask| *mask == 0.0));
    }
}
