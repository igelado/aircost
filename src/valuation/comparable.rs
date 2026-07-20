use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::structural::{age_factor, median, support_for_counts};
use super::types::{
    DepreciationPoint, GroupCounts, TrainingListing, ValuationBreakdown, ValuationError,
    ValuationEstimate, ValuationQuery,
};
use super::ValuationModel;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ComparableConfig {
    pub config_version: u32,
    pub age_floor: f64,
    pub age_decay: f64,
    pub beta_hours: f64,
    pub exact_variant_weight: f64,
    pub exact_model_weight: f64,
    pub same_manufacturer_weight: f64,
    pub global_weight: f64,
    pub q80_abs_log_error: f64,
}

impl Default for ComparableConfig {
    fn default() -> Self {
        Self {
            config_version: 1,
            age_floor: 0.28,
            age_decay: 0.05,
            beta_hours: -0.08,
            exact_variant_weight: 16.0,
            exact_model_weight: 8.0,
            same_manufacturer_weight: 3.0,
            global_weight: 1.0,
            q80_abs_log_error: 1.55_f64.ln(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ComparableModel {
    model_version_id: i64,
    snapshot_id: i64,
    rows: Vec<TrainingListing>,
    config: ComparableConfig,
    counts: GroupCounts,
    utilization: f64,
    expected_hours_per_year: f64,
    global_anchor: f64,
}

impl ComparableModel {
    pub fn new(
        model_version_id: i64,
        snapshot_id: i64,
        rows: Vec<TrainingListing>,
        config: ComparableConfig,
    ) -> Result<Self, ValuationError> {
        if rows.is_empty() {
            return Err(ValuationError::EmptySnapshot);
        }
        if !(0.10..=0.70).contains(&config.age_floor)
            || !(0.01..=0.25).contains(&config.age_decay)
            || config.beta_hours > 0.0
        {
            return Err(ValuationError::InvalidArtifact(
                "invalid comparable configuration".to_string(),
            ));
        }
        let counts = count_groups(&rows);
        let rates: Vec<f64> = rows
            .iter()
            .filter_map(|row| row.airframe_hours.map(|hours| hours / row.age().max(1.0)))
            .collect();
        let utilization = median(rates).max(0.0);
        let anchors = rows
            .iter()
            .map(|row| {
                row.asking_price_usd / age_factor(config.age_floor, config.age_decay, row.age())
            })
            .collect();
        Ok(Self {
            model_version_id,
            snapshot_id,
            rows,
            config,
            counts,
            utilization,
            expected_hours_per_year: utilization,
            global_anchor: median(anchors),
        })
    }

    fn predicted_value(&self, query: &ValuationQuery) -> Result<f64, ValuationError> {
        query.validate()?;
        let target_age = query.age();
        let target_hours_adjustment = self.hours_adjustment(target_age, query.airframe_hours);
        let mut by_group: BTreeMap<&str, Vec<(f64, f64)>> = BTreeMap::new();
        for row in &self.rows {
            let similarity_weight = if query.variant_id == Some(row.variant_id) {
                self.config.exact_variant_weight
            } else if query.model_id == Some(row.model_id) {
                self.config.exact_model_weight
            } else if query.manufacturer_id == Some(row.manufacturer_id) {
                self.config.same_manufacturer_weight
            } else {
                self.config.global_weight
            };
            let adjusted = row.asking_price_usd
                * age_factor(self.config.age_floor, self.config.age_decay, target_age)
                / age_factor(self.config.age_floor, self.config.age_decay, row.age())
                * (target_hours_adjustment - self.hours_adjustment(row.age(), row.airframe_hours))
                    .exp();
            by_group
                .entry(&row.duplicate_group_key)
                .or_default()
                .push((adjusted, similarity_weight));
        }
        let mut candidates: Vec<(f64, f64)> = by_group
            .into_values()
            .map(|members| {
                let value = weighted_median(members.clone());
                let weight = members
                    .iter()
                    .map(|(_, weight)| *weight)
                    .fold(0.0, f64::max);
                (value, weight)
            })
            .collect();
        cap_candidate_weights(&mut candidates);
        let value = weighted_median(candidates);
        if !value.is_finite() || value <= 0.0 {
            return Err(ValuationError::InvalidArtifact(
                "comparable estimate is not finite and positive".to_string(),
            ));
        }
        Ok(value)
    }

    fn hours_adjustment(&self, age: f64, hours: Option<f64>) -> f64 {
        let expected = (self.expected_hours_per_year * age.max(1.0)).ln_1p();
        self.config.beta_hours
            * hours
                .map(|value| value.ln_1p() - expected)
                .unwrap_or_default()
    }
}

impl ValuationModel for ComparableModel {
    fn model_version_id(&self) -> i64 {
        self.model_version_id
    }

    fn model_kind(&self) -> &'static str {
        "comparable"
    }

    fn snapshot_id(&self) -> i64 {
        self.snapshot_id
    }

    fn estimate(&self, query: &ValuationQuery) -> Result<ValuationEstimate, ValuationError> {
        let value_now = self.predicted_value(query)?;
        let support = support_for_counts(&self.counts, query);
        let base_q80 = self.config.q80_abs_log_error;
        let mut depreciation = Vec::with_capacity(31);
        for horizon in 0..=30_i64 {
            let mut future = query.clone();
            future.valuation_year += horizon;
            future.airframe_hours = query
                .airframe_hours
                .map(|hours| hours + self.utilization * horizon as f64);
            let value = self.predicted_value(&future)?;
            let mut next = future.clone();
            next.valuation_year += 1;
            next.airframe_hours = future.airframe_hours.map(|hours| hours + self.utilization);
            let next_value = self.predicted_value(&next)?;
            let q80 = base_q80 * (1.0 + 0.0125 * horizon as f64);
            let multiplier = q80.exp();
            depreciation.push(DepreciationPoint {
                horizon_years: horizon,
                valuation_year: future.valuation_year,
                age_years: future.age(),
                airframe_hours: future.airframe_hours,
                estimated_value_usd: value,
                low_value_usd: value / multiplier,
                high_value_usd: value * multiplier,
                depreciation_usd: (value_now - value).max(0.0),
                depreciation_fraction: ((value_now - value) / value_now).max(0.0),
                one_year_depreciation_fraction: ((value - next_value) / value).max(0.0),
                estimated_error_fraction: multiplier - 1.0,
                support,
            });
        }
        let target_expected_hours = self.expected_hours_per_year * query.age().max(1.0);
        let target_hours_adjustment = self.hours_adjustment(query.age(), query.airframe_hours);
        let multiplier = base_q80.exp();
        Ok(ValuationEstimate {
            estimated_value_usd: value_now,
            low_value_usd: value_now / multiplier,
            high_value_usd: value_now * multiplier,
            estimated_error_fraction: multiplier - 1.0,
            support,
            model_kind: self.model_kind().to_string(),
            model_version_id: self.model_version_id,
            snapshot_id: self.snapshot_id,
            breakdown: ValuationBreakdown {
                global_anchor_usd: self.global_anchor,
                age_factor: age_factor(self.config.age_floor, self.config.age_decay, query.age()),
                expected_airframe_hours: target_expected_hours,
                hours_residual: query
                    .airframe_hours
                    .map(|hours| hours.ln_1p() - target_expected_hours.ln_1p())
                    .unwrap_or_default(),
                hours_factor: target_hours_adjustment.exp(),
                category_factor: 1.0,
                manufacturer_factor: 1.0,
                model_factor: 1.0,
                variant_factor: 1.0,
                optional_features_factor: 1.0,
            },
            depreciation,
        })
    }
}

fn count_groups(rows: &[TrainingListing]) -> GroupCounts {
    let mut counts = GroupCounts {
        total: rows.len(),
        ..GroupCounts::default()
    };
    for row in rows {
        *counts.manufacturers.entry(row.manufacturer_id).or_default() += 1;
        *counts.models.entry(row.model_id).or_default() += 1;
        *counts.variants.entry(row.variant_id).or_default() += 1;
    }
    counts
}

fn cap_candidate_weights(candidates: &mut [(f64, f64)]) {
    if candidates.len() <= 1 {
        return;
    }
    let total = candidates.iter().map(|(_, weight)| *weight).sum::<f64>();
    let maximum = total / 2.0;
    for (_, weight) in candidates {
        *weight = weight.min(maximum);
    }
}

fn weighted_median(mut candidates: Vec<(f64, f64)>) -> f64 {
    candidates.sort_by(|left, right| left.0.total_cmp(&right.0));
    let total = candidates.iter().map(|(_, weight)| *weight).sum::<f64>();
    let mut accumulated = 0.0;
    for (value, weight) in &candidates {
        accumulated += weight;
        if accumulated >= total / 2.0 {
            return *value;
        }
    }
    candidates.last().map(|(value, _)| *value).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, price: f64) -> TrainingListing {
        TrainingListing {
            listing_id: id,
            duplicate_group_key: format!("serial-{id}"),
            category_key: None,
            manufacturer_id: 1,
            model_id: 2,
            variant_id: 3,
            model_year: 2015,
            snapshot_year: 2026,
            asking_price_usd: price,
            airframe_hours: Some(900.0),
            engine_times: vec![],
            propeller_times: vec![],
            equipment_tokens: vec![],
            technical_field_count: 4,
        }
    }

    #[test]
    fn one_identical_listing_reproduces_asking_price() {
        let training = row(1, 215_000.0);
        let query = training.as_query();
        let model =
            ComparableModel::new(0, 4, vec![training], ComparableConfig::default()).unwrap();
        let estimate = model.estimate(&query).unwrap();
        assert!((estimate.estimated_value_usd - 215_000.0).abs() < 0.01);
    }

    #[test]
    fn duplicate_group_does_not_gain_duplicate_weight() {
        let mut duplicate = row(2, 1_000_000.0);
        duplicate.duplicate_group_key = "serial-1".to_string();
        let model = ComparableModel::new(
            0,
            4,
            vec![row(1, 100_000.0), duplicate, row(3, 110_000.0)],
            ComparableConfig::default(),
        )
        .unwrap();
        let estimate = model.estimate(&row(3, 0.0).as_query()).unwrap();
        assert!(estimate.estimated_value_usd <= 110_000.0);
    }
}
