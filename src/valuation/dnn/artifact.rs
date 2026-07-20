use burn::prelude::Backend;
use burn::store::{ModuleStore, SafetensorsStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::valuation::{ErrorBands, GroupCounts, UtilizationRates, ValuationError};

use super::features::{EncodedInput, FeatureEncoderV1};
use super::network::{DnnArchitectureConfig, TinyValuationNet};
use super::train::TrainingSchedule;
use super::DnnCapacity;

pub const DNN_ARTIFACT_FORMAT_VERSION: u32 = 1;
pub const DNN_ENSEMBLE_MEMBERS: usize = 5;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DnnArtifactMetadataV1 {
    pub artifact_format_version: u32,
    pub model_version_id: i64,
    pub snapshot_id: i64,
    pub baseline_model_version_id: i64,
    pub capacity: DnnCapacity,
    pub architecture: DnnArchitectureConfig,
    pub encoder: FeatureEncoderV1,
    pub member_seeds: Vec<u64>,
    pub member_hashes: Vec<String>,
    pub parameter_count_per_member: usize,
    pub training_schedule: TrainingSchedule,
    pub group_counts: GroupCounts,
    pub error_bands: ErrorBands,
    pub utilization_rates: UtilizationRates,
    pub smoke_input: EncodedInput,
    pub smoke_log_prediction: f64,
    pub floating_point_tolerance: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MemberArtifact {
    pub name: String,
    pub sha256: String,
    pub bytes: Vec<u8>,
}

impl MemberArtifact {
    pub fn new(index: usize, bytes: Vec<u8>) -> Self {
        Self {
            name: format!("member-{index:02}.safetensors"),
            sha256: sha256_hex(&bytes),
            bytes,
        }
    }

    pub fn verify(&self, expected_name: &str, expected_hash: &str) -> Result<(), ValuationError> {
        if self.name != expected_name {
            return Err(ValuationError::InvalidArtifact(format!(
                "expected DNN member {expected_name}, found {}",
                self.name
            )));
        }
        let actual_hash = sha256_hex(&self.bytes);
        if self.sha256 != actual_hash || expected_hash != actual_hash {
            return Err(ValuationError::InvalidArtifact(format!(
                "hash mismatch for DNN member {}",
                self.name
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DnnArtifactV1 {
    pub metadata: DnnArtifactMetadataV1,
    pub members: Vec<MemberArtifact>,
}

impl DnnArtifactV1 {
    pub fn metadata_json(&self) -> Result<Vec<u8>, ValuationError> {
        serde_json::to_vec_pretty(&self.metadata)
            .map_err(|error| ValuationError::InvalidArtifact(error.to_string()))
    }

    pub fn validate(&self) -> Result<(), ValuationError> {
        if self.metadata.artifact_format_version != DNN_ARTIFACT_FORMAT_VERSION {
            return Err(ValuationError::InvalidArtifact(format!(
                "unsupported DNN artifact format {}",
                self.metadata.artifact_format_version
            )));
        }
        self.metadata.architecture.validate()?;
        if self.metadata.capacity != self.metadata.architecture.capacity {
            return Err(ValuationError::InvalidArtifact(
                "DNN capacity disagrees with architecture".to_string(),
            ));
        }
        if self.metadata.parameter_count_per_member != self.metadata.architecture.parameter_count()
        {
            return Err(ValuationError::InvalidArtifact(
                "DNN metadata parameter count does not match architecture".to_string(),
            ));
        }
        if self.members.len() != DNN_ENSEMBLE_MEMBERS
            || self.metadata.member_seeds.len() != DNN_ENSEMBLE_MEMBERS
            || self.metadata.member_hashes.len() != DNN_ENSEMBLE_MEMBERS
        {
            return Err(ValuationError::InvalidArtifact(format!(
                "DNN artifact must contain exactly {DNN_ENSEMBLE_MEMBERS} members"
            )));
        }
        for (index, member) in self.members.iter().enumerate() {
            member.verify(
                &format!("member-{index:02}.safetensors"),
                &self.metadata.member_hashes[index],
            )?;
        }
        if !self.metadata.smoke_log_prediction.is_finite()
            || self.metadata.floating_point_tolerance <= 0.0
            || !self.metadata.floating_point_tolerance.is_finite()
        {
            return Err(ValuationError::InvalidArtifact(
                "DNN smoke prediction or tolerance is invalid".to_string(),
            ));
        }
        Ok(())
    }

    pub fn load_members<B: Backend>(
        &self,
        device: &B::Device,
    ) -> Result<Vec<TinyValuationNet<B>>, ValuationError> {
        self.validate()?;
        let mut models = Vec::with_capacity(self.members.len());
        for member in &self.members {
            let mut model = TinyValuationNet::new(
                &self.metadata.architecture,
                0.0,
                [0.08, 0.18, 0.42, 0.52],
                -0.02,
                device,
            )?;
            let mut store = SafetensorsStore::from_bytes(Some(member.bytes.clone()));
            let result = store
                .apply_to(&mut model)
                .map_err(|error| ValuationError::InvalidArtifact(error.to_string()))?;
            if !result.is_success()
                || !result.missing.is_empty()
                || !result.unused.is_empty()
                || !result.skipped.is_empty()
            {
                return Err(ValuationError::InvalidArtifact(format!(
                    "invalid tensors in {}: {result}",
                    member.name
                )));
            }
            models.push(model);
        }
        let smoke_predictions = models
            .iter()
            .map(|model| {
                model
                    .predict_one(&self.metadata.smoke_input, device)
                    .map(|prediction| prediction.log_value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let smoke = median(smoke_predictions);
        if (smoke - self.metadata.smoke_log_prediction).abs()
            > self.metadata.floating_point_tolerance
        {
            return Err(ValuationError::InvalidArtifact(format!(
                "DNN known-answer smoke prediction changed: expected {}, got {smoke}",
                self.metadata.smoke_log_prediction
            )));
        }
        Ok(models)
    }
}

pub fn serialize_member<B: Backend>(
    index: usize,
    model: &TinyValuationNet<B>,
) -> Result<MemberArtifact, ValuationError> {
    let mut store = SafetensorsStore::from_bytes(None);
    store
        .collect_from(model)
        .map_err(|error| ValuationError::InvalidArtifact(error.to_string()))?;
    let bytes = store
        .get_bytes()
        .map_err(|error| ValuationError::InvalidArtifact(error.to_string()))?;
    Ok(MemberArtifact::new(index, bytes))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}
