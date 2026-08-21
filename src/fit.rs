use serde::Serialize;

use crate::db::AppDb;
use crate::valuation::dataset::{load_snapshot, sha256_hex};
use crate::valuation::store::persist_structural_candidate;
use crate::valuation::types::StructuralArtifactV1;
use crate::valuation::validation::{fit_validated_structural, ValidationReport};
use crate::valuation::StructuralFitConfig;

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
