use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ComponentObservation {
    pub time_hours: Option<f64>,
    pub basis: ComponentTimeBasis,
    #[serde(default)]
    pub source_basis: Option<String>,
    #[serde(default)]
    pub evidence_text: Option<String>,
    #[serde(default)]
    pub source_confidence: Option<String>,
    #[serde(default = "one_component")]
    pub count: i64,
}

pub fn source_backed_component_observation(
    time_hours: Option<f64>,
    source_basis: &str,
    evidence_text: Option<&str>,
    source_confidence: Option<&str>,
    count: i64,
) -> ComponentObservation {
    let parsed_basis = match source_basis {
        "SNEW" => ComponentTimeBasis::SinceNew,
        "SMOH" | "SFOH" | "SPOH" => ComponentTimeBasis::SinceOverhaul,
        _ => ComponentTimeBasis::Unknown,
    };
    let is_source_backed = parsed_basis != ComponentTimeBasis::Unknown
        && evidence_text.is_some_and(|value| !value.trim().is_empty())
        && matches!(source_confidence, Some("high" | "medium"))
        && time_hours.is_some_and(|hours| hours.is_finite() && hours >= 0.0);

    ComponentObservation {
        time_hours: is_source_backed.then_some(time_hours).flatten(),
        basis: if is_source_backed {
            parsed_basis
        } else {
            ComponentTimeBasis::Unknown
        },
        source_basis: Some(source_basis.to_string()),
        evidence_text: evidence_text.map(str::to_string),
        source_confidence: source_confidence.map(str::to_string),
        count,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SourceBackedValuationFact {
    pub kind: String,
    pub value: String,
    pub evidence_text: String,
    pub source_url: Option<String>,
    pub confidence: String,
}

fn one_component() -> i64 {
    1
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComponentTimeBasis {
    SinceNew,
    SinceOverhaul,
    SinceInspection,
    TimeRemaining,
    #[default]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::{source_backed_component_observation, ComponentTimeBasis};

    #[test]
    fn only_source_backed_known_component_times_reach_valuation() {
        let trusted = source_backed_component_observation(
            Some(420.0),
            "SMOH",
            Some("420 hours SMOH"),
            Some("medium"),
            1,
        );
        assert_eq!(trusted.time_hours, Some(420.0));
        assert_eq!(trusted.basis, ComponentTimeBasis::SinceOverhaul);

        for source_basis in ["SFOH", "SPOH"] {
            let overhaul = source_backed_component_observation(
                Some(420.0),
                source_basis,
                Some("420 hours since overhaul"),
                Some("high"),
                1,
            );
            assert_eq!(overhaul.basis, ComponentTimeBasis::SinceOverhaul);
        }

        for rejected in [
            source_backed_component_observation(
                Some(420.0),
                "unknown",
                Some("420 engine hours"),
                Some("high"),
                1,
            ),
            source_backed_component_observation(Some(420.0), "SMOH", None, Some("high"), 1),
            source_backed_component_observation(
                Some(420.0),
                "SMOH",
                Some("possibly 420 SMOH"),
                Some("low"),
                1,
            ),
        ] {
            assert_eq!(rejected.time_hours, None);
            assert_eq!(rejected.basis, ComponentTimeBasis::Unknown);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ValuationQuery {
    pub category_key: Option<String>,
    pub manufacturer_id: Option<i64>,
    pub model_id: Option<i64>,
    pub variant_id: Option<i64>,
    pub model_year: i64,
    pub valuation_year: i64,
    pub airframe_hours: Option<f64>,
    #[serde(default)]
    pub engine_times: Vec<ComponentObservation>,
    #[serde(default)]
    pub propeller_times: Vec<ComponentObservation>,
    #[serde(default)]
    pub equipment_tokens: Vec<String>,
    #[serde(default)]
    pub technical_field_count: u32,
}

impl ValuationQuery {
    pub fn validate(&self) -> Result<(), ValuationError> {
        if !(1850..=self.valuation_year).contains(&self.model_year) {
            return Err(ValuationError::InvalidQuery(format!(
                "model year {} is outside 1850..={}",
                self.model_year, self.valuation_year
            )));
        }
        if self
            .airframe_hours
            .is_some_and(|hours| !hours.is_finite() || hours < 0.0)
        {
            return Err(ValuationError::InvalidQuery(
                "airframe hours must be finite and non-negative".to_string(),
            ));
        }
        for component in self.engine_times.iter().chain(&self.propeller_times) {
            if component.count < 1
                || component
                    .time_hours
                    .is_some_and(|hours| !hours.is_finite() || hours < 0.0)
            {
                return Err(ValuationError::InvalidQuery(
                    "component observations must have positive counts and non-negative hours"
                        .to_string(),
                ));
            }
        }
        Ok(())
    }

    pub fn age(&self) -> f64 {
        (self.valuation_year - self.model_year).max(0) as f64
    }

    pub fn age_years(&self) -> Result<f64, ValuationError> {
        self.validate()?;
        Ok(self.age())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "lowercase")]
pub enum SupportGrade {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ValuationEstimate {
    pub estimated_value_usd: f64,
    pub low_value_usd: f64,
    pub high_value_usd: f64,
    pub estimated_error_fraction: f64,
    pub support: SupportGrade,
    pub model_kind: String,
    pub model_version_id: i64,
    pub snapshot_id: i64,
    pub breakdown: ValuationBreakdown,
    pub depreciation: Vec<DepreciationPoint>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ValuationBreakdown {
    pub global_anchor_usd: f64,
    pub age_factor: f64,
    pub expected_airframe_hours: f64,
    pub hours_residual: f64,
    pub hours_factor: f64,
    pub category_factor: f64,
    pub manufacturer_factor: f64,
    pub model_factor: f64,
    pub variant_factor: f64,
    pub optional_features_factor: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct DepreciationPoint {
    pub horizon_years: i64,
    pub valuation_year: i64,
    pub age_years: f64,
    pub airframe_hours: Option<f64>,
    pub estimated_value_usd: f64,
    pub low_value_usd: f64,
    pub high_value_usd: f64,
    pub depreciation_usd: f64,
    pub depreciation_fraction: f64,
    pub one_year_depreciation_fraction: f64,
    pub estimated_error_fraction: f64,
    pub support: SupportGrade,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TrainingListing {
    pub listing_id: i64,
    pub duplicate_group_key: String,
    pub category_key: Option<String>,
    pub manufacturer_id: i64,
    pub model_id: i64,
    pub variant_id: i64,
    pub model_year: i64,
    pub snapshot_year: i64,
    pub asking_price_usd: f64,
    pub airframe_hours: Option<f64>,
    #[serde(default)]
    pub engine_times: Vec<ComponentObservation>,
    #[serde(default)]
    pub propeller_times: Vec<ComponentObservation>,
    #[serde(default)]
    pub equipment_tokens: Vec<String>,
    #[serde(default)]
    pub valuation_facts: Vec<SourceBackedValuationFact>,
    #[serde(default)]
    pub technical_field_count: u32,
}

impl TrainingListing {
    pub fn age(&self) -> f64 {
        (self.snapshot_year - self.model_year).max(0) as f64
    }

    pub fn as_query(&self) -> ValuationQuery {
        ValuationQuery {
            category_key: self.category_key.clone(),
            manufacturer_id: Some(self.manufacturer_id),
            model_id: Some(self.model_id),
            variant_id: Some(self.variant_id),
            model_year: self.model_year,
            valuation_year: self.snapshot_year,
            airframe_hours: self.airframe_hours,
            engine_times: self.engine_times.clone(),
            propeller_times: self.propeller_times.clone(),
            equipment_tokens: self.equipment_tokens.clone(),
            technical_field_count: self.technical_field_count,
        }
    }

    pub fn query(&self) -> ValuationQuery {
        self.as_query()
    }

    pub fn validate(&self) -> Result<(), ValuationError> {
        if !self.asking_price_usd.is_finite() || self.asking_price_usd <= 0.0 {
            return Err(ValuationError::Fit(format!(
                "listing {} has an invalid USD asking price",
                self.listing_id
            )));
        }
        self.as_query().validate()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct HoursTrend {
    pub intercept: f64,
    pub age_slope: f64,
    pub category_adjustments: BTreeMap<String, f64>,
}

impl HoursTrend {
    pub fn expected_log_hours(&self, age: f64, category: Option<&str>) -> f64 {
        let category = category
            .and_then(|key| self.category_adjustments.get(key))
            .copied()
            .unwrap_or_default();
        (self.intercept + self.age_slope * age.max(0.0) + category).max(0.0)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct IdentityOffsets {
    pub categories: BTreeMap<String, f64>,
    pub manufacturers: BTreeMap<i64, f64>,
    pub models: BTreeMap<i64, f64>,
    pub variants: BTreeMap<i64, f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct GroupCounts {
    pub total: usize,
    pub categories: BTreeMap<String, usize>,
    pub manufacturers: BTreeMap<i64, usize>,
    pub models: BTreeMap<i64, usize>,
    pub variants: BTreeMap<i64, usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ErrorBand {
    pub median_abs_log_error: f64,
    pub q80_abs_log_error: f64,
    pub residual_count: usize,
}

impl Default for ErrorBand {
    fn default() -> Self {
        Self {
            median_abs_log_error: 1.35_f64.ln(),
            q80_abs_log_error: 1.55_f64.ln(),
            residual_count: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ErrorBands {
    pub global: ErrorBand,
    pub by_support: BTreeMap<SupportGrade, ErrorBand>,
    pub manufacturers: BTreeMap<i64, ErrorBand>,
    pub models: BTreeMap<i64, ErrorBand>,
    pub variants: BTreeMap<i64, ErrorBand>,
}

impl ErrorBands {
    pub fn q80(&self, support: SupportGrade) -> f64 {
        self.by_support
            .get(&support)
            .unwrap_or(&self.global)
            .q80_abs_log_error
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct UtilizationRates {
    pub global_hours_per_year: f64,
    pub manufacturers: BTreeMap<i64, f64>,
    pub models: BTreeMap<i64, f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct StructuralArtifactV1 {
    pub snapshot_id: i64,
    pub snapshot_year: i64,
    pub global_log_anchor: f64,
    pub age_floor: f64,
    pub age_decay: f64,
    pub expected_hours: HoursTrend,
    pub beta_hours: f64,
    pub identity_offsets: IdentityOffsets,
    pub optional_feature_coefficients: BTreeMap<String, f64>,
    pub group_counts: GroupCounts,
    pub error_bands: ErrorBands,
    pub utilization_rates: UtilizationRates,
    pub feature_schema_version: u32,
}

impl StructuralArtifactV1 {
    pub fn validate(&self) -> Result<(), ValuationError> {
        if self.feature_schema_version != crate::valuation::FEATURE_SCHEMA_VERSION {
            return Err(ValuationError::InvalidArtifact(format!(
                "structural artifact feature schema {} does not match current schema {}",
                self.feature_schema_version,
                crate::valuation::FEATURE_SCHEMA_VERSION
            )));
        }
        if !(0.10..=0.70).contains(&self.age_floor)
            || !(0.01..=0.25).contains(&self.age_decay)
            || !self.global_log_anchor.is_finite()
            || !self.beta_hours.is_finite()
            || self.beta_hours > 1e-12
            || !self.expected_hours.intercept.is_finite()
            || !self.expected_hours.age_slope.is_finite()
        {
            return Err(ValuationError::InvalidArtifact(
                "artifact contains invalid core parameters".to_string(),
            ));
        }
        let all_finite = self
            .identity_offsets
            .categories
            .values()
            .chain(self.identity_offsets.manufacturers.values())
            .chain(self.identity_offsets.models.values())
            .chain(self.identity_offsets.variants.values())
            .chain(self.optional_feature_coefficients.values())
            .all(|value| value.is_finite());
        if !all_finite {
            return Err(ValuationError::InvalidArtifact(
                "artifact contains non-finite coefficients".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ValuationError {
    EmptySnapshot,
    InvalidQuery(String),
    InvalidArtifact(String),
    Fit(String),
    Database(String),
    Serialization(String),
    ValidationGate(String),
}

impl fmt::Display for ValuationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySnapshot => write!(formatter, "valuation snapshot has no included rows"),
            Self::InvalidQuery(message)
            | Self::InvalidArtifact(message)
            | Self::Fit(message)
            | Self::Database(message)
            | Self::Serialization(message)
            | Self::ValidationGate(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for ValuationError {}

impl From<sqlx::Error> for ValuationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

impl From<serde_json::Error> for ValuationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
    }
}
