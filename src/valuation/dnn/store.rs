use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::valuation::store::StoredModelValidation;
use crate::valuation::validation::{FoldPrediction, ValidationMetrics, ValidationReport};
use crate::valuation::{StructuralFitConfig, SupportGrade, ValuationError};

use super::artifact::{sha256_hex, DnnArtifactMetadataV1, DnnArtifactV1, MemberArtifact};
use super::train::{evaluate_activation_gates, ActivationGateReport, DnnFitReport};
use super::DnnValuationModel;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct DnnStoredMetricsV1 {
    pub dnn_metrics: ValidationMetrics,
    pub activation_gates: ActivationGateReport,
    pub baseline_model_version_id: i64,
}

#[derive(FromRow)]
struct BaselineRow {
    snapshot_id: i64,
    metrics_json: String,
}

#[derive(FromRow)]
struct StoredFoldRow {
    fold_id: String,
    duplicate_group_key: String,
    source_listing_id: i64,
    actual_price_usd: f64,
    predicted_price_usd: f64,
    log_error: f64,
    absolute_percentage_error: f64,
    support_grade: String,
}

pub async fn structural_baseline_id(db: &AppDb, snapshot_id: i64) -> Result<i64, ValuationError> {
    let sql = db.sql(
        r#"
        SELECT id
        FROM valuation_model_versions
        WHERE snapshot_id = ? AND model_kind = 'structural'
        ORDER BY id DESC
        LIMIT 1
        "#,
    );
    let id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(snapshot_id)
                .fetch_optional(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(snapshot_id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|error| ValuationError::Database(error.to_string()))?;
    id.ok_or_else(|| {
        ValuationError::Fit(format!(
            "snapshot {snapshot_id} has no structural baseline model version"
        ))
    })
}

pub async fn structural_baseline_config(
    db: &AppDb,
    model_version_id: i64,
) -> Result<StructuralFitConfig, ValuationError> {
    let sql = db.sql(
        "SELECT configuration_json FROM valuation_model_versions WHERE id = ? AND model_kind = 'structural'",
    );
    let json = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, String>(&sql)
                .bind(model_version_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, String>(&sql)
                .bind(model_version_id)
                .fetch_optional(pool)
                .await?
        }
    }
    .ok_or_else(|| {
        ValuationError::InvalidArtifact(format!(
            "structural baseline model {model_version_id} does not exist"
        ))
    })?;
    Ok(serde_json::from_str(&json)?)
}

#[derive(FromRow)]
struct StoredArtifact {
    artifact_name: String,
    artifact_bytes: Vec<u8>,
    sha256: String,
}

#[derive(FromRow)]
struct DnnVersionRow {
    model_version_id: i64,
    snapshot_id: i64,
    model_kind: String,
    state: String,
    artifact_format_version: i32,
    metrics_json: String,
}

pub async fn evaluate_candidate_gates(
    db: &AppDb,
    report: &DnnFitReport,
) -> Result<DnnStoredMetricsV1, ValuationError> {
    let baseline_id = report.artifact.metadata.baseline_model_version_id;
    let baseline_sql = db.sql(
        r#"
        SELECT snapshot_id, metrics_json
        FROM valuation_model_versions
        WHERE id = ? AND model_kind = 'structural'
        "#,
    );
    let baseline = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, BaselineRow>(&baseline_sql)
                .bind(baseline_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, BaselineRow>(&baseline_sql)
                .bind(baseline_id)
                .fetch_optional(pool)
                .await?
        }
    }
    .ok_or_else(|| {
        ValuationError::ValidationGate(format!(
            "structural baseline model {baseline_id} does not exist"
        ))
    })?;
    if baseline.snapshot_id != report.artifact.metadata.snapshot_id {
        return Err(ValuationError::ValidationGate(
            "DNN and structural baseline must use the same snapshot".to_string(),
        ));
    }
    let baseline_report: ValidationReport = serde_json::from_str(&baseline.metrics_json)?;
    let predictions_sql = db.sql(
        r#"
        SELECT fold_id, duplicate_group_key, source_listing_id,
               actual_price_usd, predicted_price_usd, log_error,
               absolute_percentage_error, support_grade
        FROM valuation_fold_predictions
        WHERE model_version_id = ?
        ORDER BY fold_id, duplicate_group_key, source_listing_id
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, StoredFoldRow>(&predictions_sql)
                .bind(baseline_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, StoredFoldRow>(&predictions_sql)
                .bind(baseline_id)
                .fetch_all(pool)
                .await?
        }
    };
    let structural_predictions = rows
        .into_iter()
        .map(stored_fold_prediction)
        .collect::<Result<Vec<_>, _>>()?;
    let bootstrap_improvements = paired_bootstrap_improvements(
        &report.fold_predictions,
        &structural_predictions,
        report
            .artifact
            .metadata
            .member_seeds
            .first()
            .copied()
            .unwrap_or(1),
    );
    let gates = evaluate_activation_gates(
        &report.fold_predictions,
        &structural_predictions,
        &report.metrics,
        &baseline_report.structural_metrics,
        report.artifact.validate().is_ok(),
        &bootstrap_improvements,
    );
    Ok(DnnStoredMetricsV1 {
        dnn_metrics: report.metrics.clone(),
        activation_gates: gates,
        baseline_model_version_id: baseline_id,
    })
}

fn stored_fold_prediction(row: StoredFoldRow) -> Result<FoldPrediction, ValuationError> {
    let support = match row.support_grade.as_str() {
        "low" => SupportGrade::Low,
        "medium" => SupportGrade::Medium,
        "high" => SupportGrade::High,
        other => {
            return Err(ValuationError::InvalidArtifact(format!(
                "invalid stored support grade: {other}"
            )))
        }
    };
    let signed = row.predicted_price_usd / row.actual_price_usd - 1.0;
    Ok(FoldPrediction {
        fold_id: row.fold_id,
        duplicate_group_key: row.duplicate_group_key,
        listing_id: row.source_listing_id,
        manufacturer_id: 0,
        model_id: 0,
        variant_id: 0,
        actual_price_usd: row.actual_price_usd,
        predicted_price_usd: row.predicted_price_usd,
        log_error: row.log_error,
        absolute_percentage_error: row.absolute_percentage_error,
        signed_percentage_error: signed,
        support,
    })
}

fn paired_bootstrap_improvements(
    dnn: &[FoldPrediction],
    structural: &[FoldPrediction],
    seed: u64,
) -> Vec<bool> {
    let structural_by_key = structural
        .iter()
        .map(|prediction| {
            (
                (
                    prediction.duplicate_group_key.as_str(),
                    prediction.listing_id,
                ),
                prediction.absolute_percentage_error,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let paired = dnn
        .iter()
        .filter_map(|prediction| {
            structural_by_key
                .get(&(
                    prediction.duplicate_group_key.as_str(),
                    prediction.listing_id,
                ))
                .map(|structural_error| {
                    (
                        prediction.duplicate_group_key.as_str(),
                        prediction.absolute_percentage_error,
                        *structural_error,
                    )
                })
        })
        .collect::<Vec<_>>();
    if paired.is_empty() {
        return Vec::new();
    }
    (0..21_u64)
        .map(|sample| {
            let mut dnn_errors = Vec::with_capacity(paired.len());
            let mut structural_errors = Vec::with_capacity(paired.len());
            for draw in 0..paired.len() {
                let digest = Sha256::digest(
                    format!("dnn-paired-bootstrap:{seed}:{sample}:{draw}").as_bytes(),
                );
                let index = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"))
                    as usize
                    % paired.len();
                dnn_errors.push(paired[index].1);
                structural_errors.push(paired[index].2);
            }
            median(dnn_errors) < median(structural_errors)
        })
        .collect()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

pub async fn persist_dnn_candidate(
    db: &AppDb,
    report: &mut DnnFitReport,
) -> Result<i64, ValuationError> {
    report.artifact.validate()?;
    report
        .artifact
        .load_members::<burn::backend::Flex>(&Default::default())?;
    let stored_metrics = evaluate_candidate_gates(db, report).await?;
    let metrics_json = serde_json::to_string(&stored_metrics)?;
    let configuration_json = serde_json::to_string(&report.artifact.metadata.architecture)
        .map_err(|error| ValuationError::Serialization(error.to_string()))?;
    let metadata = &report.artifact.metadata;
    let insert_model_sql = db.sql(
        r#"
        INSERT INTO valuation_model_versions (
          snapshot_id, model_kind, artifact_format_version, state,
          metrics_json, configuration_json
        ) VALUES (?, 'dnn', ?, 'candidate', ?, ?)
        RETURNING id
        "#,
    );
    let insert_artifact_sql = db.sql(
        r#"
        INSERT INTO valuation_model_artifacts (
          model_version_id, artifact_name, artifact_bytes, sha256, media_type
        ) VALUES (?, ?, ?, ?, ?)
        "#,
    );
    let insert_prediction_sql = db.sql(
        r#"
        INSERT INTO valuation_fold_predictions (
          model_version_id, fold_id, duplicate_group_key, source_listing_id,
          actual_price_usd, predicted_price_usd, log_error,
          absolute_percentage_error, support_grade
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    );
    macro_rules! persist_with_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let model_version_id = sqlx::query_scalar::<_, i64>(&insert_model_sql)
                .bind(metadata.snapshot_id)
                .bind(i64::from(metadata.artifact_format_version))
                .bind(&metrics_json)
                .bind(&configuration_json)
                .fetch_one(&mut *transaction)
                .await?;
            report.artifact.metadata.model_version_id = model_version_id;
            let metadata_bytes = report.artifact.metadata_json()?;
            sqlx::query(&insert_artifact_sql)
                .bind(model_version_id)
                .bind("metadata.json")
                .bind(&metadata_bytes)
                .bind(sha256_hex(&metadata_bytes))
                .bind("application/json")
                .execute(&mut *transaction)
                .await?;
            for member in &report.artifact.members {
                sqlx::query(&insert_artifact_sql)
                    .bind(model_version_id)
                    .bind(&member.name)
                    .bind(&member.bytes)
                    .bind(sha256_hex(&member.bytes))
                    .bind("application/x-safetensors")
                    .execute(&mut *transaction)
                    .await?;
            }
            for prediction in &report.fold_predictions {
                let support = format!("{:?}", prediction.support).to_lowercase();
                sqlx::query(&insert_prediction_sql)
                    .bind(model_version_id)
                    .bind(&prediction.fold_id)
                    .bind(&prediction.duplicate_group_key)
                    .bind(prediction.listing_id)
                    .bind(prediction.actual_price_usd)
                    .bind(prediction.predicted_price_usd)
                    .bind(prediction.log_error)
                    .bind(prediction.absolute_percentage_error)
                    .bind(&support)
                    .execute(&mut *transaction)
                    .await?;
            }
            transaction.commit().await?;
            Ok(model_version_id)
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => persist_with_transaction!(pool),
        DatabaseBackend::Postgres(pool) => persist_with_transaction!(pool),
    }
}

pub async fn load_dnn_artifact(
    db: &AppDb,
    model_version_id: i64,
) -> Result<DnnArtifactV1, ValuationError> {
    let sql = db.sql(
        r#"
        SELECT artifact_name, artifact_bytes, sha256
        FROM valuation_model_artifacts
        WHERE model_version_id = ?
        ORDER BY artifact_name
        "#,
    );
    let artifacts = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, StoredArtifact>(&sql)
                .bind(model_version_id)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, StoredArtifact>(&sql)
                .bind(model_version_id)
                .fetch_all(pool)
                .await
        }
    }
    .map_err(|error| ValuationError::Database(error.to_string()))?;
    build_artifact(artifacts).map(|(artifact, _)| artifact)
}

fn build_artifact(
    artifacts: Vec<StoredArtifact>,
) -> Result<(DnnArtifactV1, String), ValuationError> {
    let metadata_row = artifacts
        .iter()
        .find(|artifact| artifact.artifact_name == "metadata.json")
        .ok_or_else(|| {
            ValuationError::InvalidArtifact("DNN metadata.json is missing".to_string())
        })?;
    if sha256_hex(&metadata_row.artifact_bytes) != metadata_row.sha256 {
        return Err(ValuationError::InvalidArtifact(
            "DNN metadata.json hash mismatch".to_string(),
        ));
    }
    let metadata = serde_json::from_slice::<DnnArtifactMetadataV1>(&metadata_row.artifact_bytes)
        .map_err(|error| ValuationError::InvalidArtifact(error.to_string()))?;
    let metadata_hash = metadata_row.sha256.clone();
    let members = artifacts
        .into_iter()
        .filter(|artifact| artifact.artifact_name != "metadata.json")
        .map(|artifact| MemberArtifact {
            name: artifact.artifact_name,
            sha256: artifact.sha256,
            bytes: artifact.artifact_bytes,
        })
        .collect();
    let artifact = DnnArtifactV1 { metadata, members };
    artifact.validate()?;
    Ok((artifact, metadata_hash))
}

async fn load_version_row(
    db: &AppDb,
    model_version_id: i64,
) -> Result<DnnVersionRow, ValuationError> {
    let sql = db.sql(
        r#"
        SELECT id AS model_version_id, snapshot_id, model_kind, state,
               artifact_format_version, metrics_json
        FROM valuation_model_versions
        WHERE id = ?
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, DnnVersionRow>(&sql)
                .bind(model_version_id)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, DnnVersionRow>(&sql)
                .bind(model_version_id)
                .fetch_optional(pool)
                .await?
        }
    };
    row.ok_or_else(|| ValuationError::Database("valuation model not found".to_string()))
}

async fn baseline_exists(
    db: &AppDb,
    baseline_model_version_id: i64,
    snapshot_id: i64,
) -> Result<bool, ValuationError> {
    let sql = db.sql(
        r#"
        SELECT COUNT(*)
        FROM valuation_model_versions
        WHERE id = ? AND snapshot_id = ? AND model_kind = 'structural'
        "#,
    );
    let count = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(baseline_model_version_id)
                .bind(snapshot_id)
                .fetch_one(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(baseline_model_version_id)
                .bind(snapshot_id)
                .fetch_one(pool)
                .await?
        }
    };
    Ok(count == 1)
}

fn validate_dnn_record(
    row: &DnnVersionRow,
    artifact: &DnnArtifactV1,
    metadata_hash: String,
    baseline_exists: bool,
) -> Result<StoredModelValidation, ValuationError> {
    if row.model_kind != "dnn" || row.artifact_format_version != 1 {
        return Err(ValuationError::InvalidArtifact(
            "unsupported DNN model kind or artifact format".to_string(),
        ));
    }
    if artifact.metadata.model_version_id != row.model_version_id
        || artifact.metadata.snapshot_id != row.snapshot_id
    {
        return Err(ValuationError::InvalidArtifact(
            "DNN metadata does not match its model-version record".to_string(),
        ));
    }
    if !baseline_exists {
        return Err(ValuationError::InvalidArtifact(
            "DNN structural fallback is missing or uses a different snapshot".to_string(),
        ));
    }
    artifact.load_members::<burn::backend::Flex>(&Default::default())?;
    let metrics: DnnStoredMetricsV1 = serde_json::from_str(&row.metrics_json)?;
    if metrics.baseline_model_version_id != artifact.metadata.baseline_model_version_id {
        return Err(ValuationError::InvalidArtifact(
            "DNN metrics and metadata reference different structural baselines".to_string(),
        ));
    }
    Ok(StoredModelValidation {
        model_version_id: row.model_version_id,
        model_kind: row.model_kind.clone(),
        state: row.state.clone(),
        snapshot_id: row.snapshot_id,
        artifact_sha256: metadata_hash,
        artifact_valid: true,
        activation_gates_pass: metrics.activation_gates.eligible_for_activation,
    })
}

pub async fn validate_dnn_model_version(
    db: &AppDb,
    model_version_id: i64,
) -> Result<StoredModelValidation, ValuationError> {
    let row = load_version_row(db, model_version_id).await?;
    let artifacts = load_artifact_rows(db, model_version_id).await?;
    let (artifact, metadata_hash) = build_artifact(artifacts)?;
    let has_baseline = baseline_exists(
        db,
        artifact.metadata.baseline_model_version_id,
        artifact.metadata.snapshot_id,
    )
    .await?;
    validate_dnn_record(&row, &artifact, metadata_hash, has_baseline)
}

async fn load_artifact_rows(
    db: &AppDb,
    model_version_id: i64,
) -> Result<Vec<StoredArtifact>, ValuationError> {
    let sql = db.sql(
        r#"
        SELECT artifact_name, artifact_bytes, sha256
        FROM valuation_model_artifacts
        WHERE model_version_id = ?
        ORDER BY artifact_name
        "#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_as::<_, StoredArtifact>(&sql)
            .bind(model_version_id)
            .fetch_all(pool)
            .await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_as::<_, StoredArtifact>(&sql)
            .bind(model_version_id)
            .fetch_all(pool)
            .await?),
    }
}

pub async fn activate_dnn_model_version(
    db: &AppDb,
    model_version_id: i64,
) -> Result<StoredModelValidation, ValuationError> {
    let version_sql = db.sql(
        r#"
        SELECT id AS model_version_id, snapshot_id, model_kind, state,
               artifact_format_version, metrics_json
        FROM valuation_model_versions
        WHERE id = ?
        "#,
    );
    let artifacts_sql = db.sql(
        r#"
        SELECT artifact_name, artifact_bytes, sha256
        FROM valuation_model_artifacts
        WHERE model_version_id = ?
        ORDER BY artifact_name
        "#,
    );
    let baseline_sql = db.sql(
        r#"
        SELECT COUNT(*)
        FROM valuation_model_versions
        WHERE id = ? AND snapshot_id = ? AND model_kind = 'structural' AND state = 'active'
        "#,
    );
    let retire_sql = db.sql(
        "UPDATE valuation_model_versions SET state = 'retired' WHERE model_kind = 'dnn' AND state = 'active'",
    );
    let activate_sql = db.sql(
        "UPDATE valuation_model_versions SET state = 'active' WHERE id = ? AND state = 'candidate'",
    );
    macro_rules! activate_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let mut row = sqlx::query_as::<_, DnnVersionRow>(&version_sql)
                .bind(model_version_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| ValuationError::Database("valuation model not found".to_string()))?;
            let artifacts = sqlx::query_as::<_, StoredArtifact>(&artifacts_sql)
                .bind(model_version_id)
                .fetch_all(&mut *transaction)
                .await?;
            let (artifact, metadata_hash) = build_artifact(artifacts)?;
            let has_baseline = sqlx::query_scalar::<_, i64>(&baseline_sql)
                .bind(artifact.metadata.baseline_model_version_id)
                .bind(artifact.metadata.snapshot_id)
                .fetch_one(&mut *transaction)
                .await?
                == 1;
            let validation = validate_dnn_record(&row, &artifact, metadata_hash, has_baseline)?;
            if row.state != "candidate" {
                return Err(ValuationError::ValidationGate(
                    "only a candidate DNN can be activated".to_string(),
                ));
            }
            if !validation.activation_gates_pass {
                return Err(ValuationError::ValidationGate(
                    "DNN candidate did not pass paired activation gates".to_string(),
                ));
            }
            sqlx::query(&retire_sql).execute(&mut *transaction).await?;
            let changed = sqlx::query(&activate_sql)
                .bind(model_version_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if changed != 1 {
                return Err(ValuationError::ValidationGate(
                    "only a candidate DNN can be activated".to_string(),
                ));
            }
            transaction.commit().await?;
            row.state = "active".to_string();
            Ok(StoredModelValidation {
                state: row.state,
                ..validation
            })
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => activate_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => activate_in_transaction!(pool),
    }
}

pub async fn load_active_dnn_model(
    db: &AppDb,
) -> Result<Option<Arc<dyn crate::valuation::ValuationModel>>, ValuationError> {
    let sql =
        "SELECT id FROM valuation_model_versions WHERE model_kind = 'dnn' AND state = 'active' ORDER BY id DESC LIMIT 1";
    let model_version_id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_optional(pool)
                .await?
        }
    };
    let Some(model_version_id) = model_version_id else {
        return Ok(None);
    };
    validate_dnn_model_version(db, model_version_id).await?;
    let artifact = load_dnn_artifact(db, model_version_id).await?;
    let fallback = crate::valuation::store::load_structural_model_version(
        db,
        artifact.metadata.baseline_model_version_id,
    )
    .await?;
    Ok(Some(Arc::new(DnnValuationModel::load(
        &artifact,
        Some(fallback),
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::valuation::store::{activate_model_version, persist_structural_candidate};
    use crate::valuation::validation::validate_structural;
    use crate::valuation::{fit_structural, StructuralFitConfig, TrainingListing};

    fn rows() -> Vec<TrainingListing> {
        (0..3)
            .map(|index| TrainingListing {
                listing_id: index + 1,
                duplicate_group_key: format!("dnn-group-{index}"),
                category_key: None,
                manufacturer_id: 1,
                model_id: index + 1,
                variant_id: index + 1,
                model_year: 2010 + index,
                snapshot_year: 2026,
                asking_price_usd: 150_000.0 + index as f64 * 12_000.0,
                airframe_hours: Some(900.0 + index as f64 * 100.0),
                engine_times: vec![],
                propeller_times: vec![],
                equipment_tokens: vec![],
                technical_field_count: 3,
            })
            .collect()
    }

    async fn snapshot_id(db: &AppDb) -> i64 {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO valuation_snapshots (
              capture_time, input_sha256, selection_policy_json,
              feature_schema_version, included_count, excluded_count
            ) VALUES ('2026-07-20', lower(hex(randomblob(32))), '{}', 1, 3, 0)
            RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn candidate_round_trip_activation_and_corruption_checks() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let snapshot_id = snapshot_id(&db).await;
        let structural_config = StructuralFitConfig::default();
        let mut structural = fit_structural(&rows(), &structural_config).unwrap();
        structural.snapshot_id = snapshot_id;
        let mut structural_report = validate_structural(&rows(), &structural_config).unwrap();
        structural_report.activation_gates_pass = true;
        structural_report.gate_reasons.clear();
        let baseline_id = persist_structural_candidate(
            &db,
            snapshot_id,
            &structural,
            &structural_report,
            &structural_config,
        )
        .await
        .unwrap();
        activate_model_version(&db, baseline_id).await.unwrap();

        let mut dnn_report = super::super::fit_dnn_candidate(
            &rows(),
            &super::super::DnnFitConfig {
                snapshot_id,
                baseline_model_version_id: baseline_id,
                maximum_epochs: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let dnn_id = persist_dnn_candidate(&db, &mut dnn_report).await.unwrap();
        let validation = validate_dnn_model_version(&db, dnn_id).await.unwrap();
        assert_eq!(validation.model_kind, "dnn");
        assert_eq!(validation.snapshot_id, snapshot_id);

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let metrics_json = sqlx::query_scalar::<_, String>(
            "SELECT metrics_json FROM valuation_model_versions WHERE id = ?",
        )
        .bind(dnn_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut metrics: DnnStoredMetricsV1 = serde_json::from_str(&metrics_json).unwrap();
        metrics.activation_gates = ActivationGateReport {
            median_error_gate: true,
            paired_win_fraction: 1.0,
            paired_win_gate: true,
            q80_error_gate: true,
            constraint_and_artifact_gate: true,
            bootstrap_direction_gate: true,
            eligible_for_activation: true,
        };
        sqlx::query("UPDATE valuation_model_versions SET metrics_json = ? WHERE id = ?")
            .bind(serde_json::to_string(&metrics).unwrap())
            .bind(dnn_id)
            .execute(pool)
            .await
            .unwrap();
        let activated = activate_dnn_model_version(&db, dnn_id).await.unwrap();
        assert_eq!(activated.state, "active");
        let model = load_active_dnn_model(&db).await.unwrap().unwrap();
        assert!(model
            .estimate(&rows()[0].as_query())
            .unwrap()
            .estimated_value_usd
            .is_finite());

        sqlx::query(
            "UPDATE valuation_model_artifacts SET artifact_bytes = ? WHERE model_version_id = ? AND artifact_name = 'member-00.safetensors'",
        )
        .bind(b"corrupt".as_slice())
        .bind(dnn_id)
        .execute(pool)
        .await
        .unwrap();
        assert!(validate_dnn_model_version(&db, dnn_id).await.is_err());
    }
}
