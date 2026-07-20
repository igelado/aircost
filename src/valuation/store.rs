use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};

use super::comparable::{ComparableConfig, ComparableModel};
use super::dataset::{newest_snapshot, sha256_hex};
use super::structural::{StructuralFitConfig, StructuralModel};
use super::types::{StructuralArtifactV1, SupportGrade, ValuationError};
use super::validation::{FoldPrediction, ValidationReport};
use super::ValuationModel;

pub const STRUCTURAL_ARTIFACT_NAME: &str = "structural-v1.json";
pub const STRUCTURAL_ARTIFACT_MEDIA_TYPE: &str = "application/vnd.aircost.structural+json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct StoredModelValidation {
    pub model_version_id: i64,
    pub model_kind: String,
    pub state: String,
    pub snapshot_id: i64,
    pub artifact_sha256: String,
    pub artifact_valid: bool,
    pub activation_gates_pass: bool,
}

#[derive(Debug, FromRow)]
struct StoredArtifactRow {
    model_version_id: i64,
    snapshot_id: i64,
    model_kind: String,
    state: String,
    artifact_format_version: i32,
    metrics_json: String,
    artifact_bytes: Vec<u8>,
    sha256: String,
}

pub async fn persist_structural_candidate(
    db: &AppDb,
    snapshot_id: i64,
    artifact: &StructuralArtifactV1,
    report: &ValidationReport,
    config: &StructuralFitConfig,
) -> Result<i64, ValuationError> {
    artifact.validate()?;
    if artifact.snapshot_id != snapshot_id {
        return Err(ValuationError::InvalidArtifact(format!(
            "artifact snapshot {} does not match candidate snapshot {snapshot_id}",
            artifact.snapshot_id
        )));
    }
    let metrics_json = serde_json::to_string(report)?;
    let configuration_json = serde_json::to_string(config)?;
    let bytes = serde_json::to_vec(artifact)?;
    let hash = sha256_hex(&bytes);
    let insert_version = db.sql(
        r#"
        INSERT INTO valuation_model_versions (
          snapshot_id, model_kind, artifact_format_version, state,
          metrics_json, configuration_json
        ) VALUES (?, 'structural', 1, 'candidate', ?, ?)
        RETURNING id
        "#,
    );
    let model_version_id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(&insert_version)
                .bind(snapshot_id)
                .bind(&metrics_json)
                .bind(&configuration_json)
                .fetch_one(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(&insert_version)
                .bind(snapshot_id)
                .bind(&metrics_json)
                .bind(&configuration_json)
                .fetch_one(pool)
                .await?
        }
    };
    let insert_artifact = db.sql(
        r#"
        INSERT INTO valuation_model_artifacts (
          model_version_id, artifact_name, artifact_bytes, sha256, media_type
        ) VALUES (?, ?, ?, ?, ?)
        "#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query(&insert_artifact)
                .bind(model_version_id)
                .bind(STRUCTURAL_ARTIFACT_NAME)
                .bind(&bytes)
                .bind(&hash)
                .bind(STRUCTURAL_ARTIFACT_MEDIA_TYPE)
                .execute(pool)
                .await?;
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query(&insert_artifact)
                .bind(model_version_id)
                .bind(STRUCTURAL_ARTIFACT_NAME)
                .bind(&bytes)
                .bind(&hash)
                .bind(STRUCTURAL_ARTIFACT_MEDIA_TYPE)
                .execute(pool)
                .await?;
        }
    }
    persist_fold_predictions(db, model_version_id, &report.fold_predictions).await?;
    Ok(model_version_id)
}

pub async fn persist_fold_predictions(
    db: &AppDb,
    model_version_id: i64,
    predictions: &[FoldPrediction],
) -> Result<(), ValuationError> {
    let insert = db.sql(
        r#"
        INSERT INTO valuation_fold_predictions (
          model_version_id, fold_id, duplicate_group_key, source_listing_id,
          actual_price_usd, predicted_price_usd, log_error,
          absolute_percentage_error, support_grade
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    );
    for prediction in predictions {
        let support = support_name(prediction.support);
        match db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(&insert)
                    .bind(model_version_id)
                    .bind(&prediction.fold_id)
                    .bind(&prediction.duplicate_group_key)
                    .bind(prediction.listing_id)
                    .bind(prediction.actual_price_usd)
                    .bind(prediction.predicted_price_usd)
                    .bind(prediction.log_error)
                    .bind(prediction.absolute_percentage_error)
                    .bind(support)
                    .execute(pool)
                    .await?;
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query(&insert)
                    .bind(model_version_id)
                    .bind(&prediction.fold_id)
                    .bind(&prediction.duplicate_group_key)
                    .bind(prediction.listing_id)
                    .bind(prediction.actual_price_usd)
                    .bind(prediction.predicted_price_usd)
                    .bind(prediction.log_error)
                    .bind(prediction.absolute_percentage_error)
                    .bind(support)
                    .execute(pool)
                    .await?;
            }
        }
    }
    Ok(())
}

pub async fn validate_model_version(
    db: &AppDb,
    model_version_id: i64,
) -> Result<StoredModelValidation, ValuationError> {
    let row = load_artifact_row(db, model_version_id).await?;
    validate_stored_row(&row)
}

pub async fn activate_model_version(
    db: &AppDb,
    model_version_id: i64,
) -> Result<StoredModelValidation, ValuationError> {
    let select = db.sql(
        r#"
        SELECT
          version.id AS model_version_id,
          version.snapshot_id,
          version.model_kind,
          version.state,
          version.artifact_format_version,
          version.metrics_json,
          artifact.artifact_bytes,
          artifact.sha256
        FROM valuation_model_versions version
        JOIN valuation_model_artifacts artifact
          ON artifact.model_version_id = version.id
        WHERE version.id = ? AND artifact.artifact_name = ?
        "#,
    );
    let retire = db.sql(
        "UPDATE valuation_model_versions SET state = 'retired' WHERE model_kind = ? AND state = 'active'",
    );
    let activate = db.sql(
        "UPDATE valuation_model_versions SET state = 'active' WHERE id = ? AND state = 'candidate'",
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            let row = sqlx::query_as::<_, StoredArtifactRow>(&select)
                .bind(model_version_id)
                .bind(STRUCTURAL_ARTIFACT_NAME)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| ValuationError::Database("model artifact not found".to_string()))?;
            validate_activatable(&row)?;
            sqlx::query(&retire)
                .bind(&row.model_kind)
                .execute(&mut *transaction)
                .await?;
            let changed = sqlx::query(&activate)
                .bind(model_version_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if changed != 1 {
                return Err(ValuationError::ValidationGate(
                    "only a candidate model can be activated".to_string(),
                ));
            }
            transaction.commit().await?;
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await?;
            let row = sqlx::query_as::<_, StoredArtifactRow>(&select)
                .bind(model_version_id)
                .bind(STRUCTURAL_ARTIFACT_NAME)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| ValuationError::Database("model artifact not found".to_string()))?;
            validate_activatable(&row)?;
            sqlx::query(&retire)
                .bind(&row.model_kind)
                .execute(&mut *transaction)
                .await?;
            let changed = sqlx::query(&activate)
                .bind(model_version_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if changed != 1 {
                return Err(ValuationError::ValidationGate(
                    "only a candidate model can be activated".to_string(),
                ));
            }
            transaction.commit().await?;
        }
    }
    validate_model_version(db, model_version_id).await
}

pub async fn load_serving_model(
    db: &AppDb,
) -> Result<Option<Arc<dyn ValuationModel>>, ValuationError> {
    let active_id_sql =
        "SELECT id FROM valuation_model_versions WHERE model_kind = 'structural' AND state = 'active' ORDER BY id DESC LIMIT 1";
    let active_id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>(active_id_sql)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_scalar::<_, i64>(active_id_sql)
                .fetch_optional(pool)
                .await?
        }
    };
    if let Some(model_version_id) = active_id {
        let row = load_artifact_row(db, model_version_id).await?;
        validate_stored_row(&row)?;
        let artifact: StructuralArtifactV1 = serde_json::from_slice(&row.artifact_bytes)?;
        return Ok(Some(Arc::new(StructuralModel::new(
            model_version_id,
            artifact,
        )?)));
    }
    if let Some((snapshot_id, rows)) = newest_snapshot(db).await? {
        if !rows.is_empty() {
            return Ok(Some(Arc::new(ComparableModel::new(
                0,
                snapshot_id,
                rows,
                ComparableConfig::default(),
            )?)));
        }
    }
    Ok(None)
}

async fn load_artifact_row(
    db: &AppDb,
    model_version_id: i64,
) -> Result<StoredArtifactRow, ValuationError> {
    let sql = db.sql(
        r#"
        SELECT
          version.id AS model_version_id,
          version.snapshot_id,
          version.model_kind,
          version.state,
          version.artifact_format_version,
          version.metrics_json,
          artifact.artifact_bytes,
          artifact.sha256
        FROM valuation_model_versions version
        JOIN valuation_model_artifacts artifact
          ON artifact.model_version_id = version.id
        WHERE version.id = ? AND artifact.artifact_name = ?
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, StoredArtifactRow>(&sql)
                .bind(model_version_id)
                .bind(STRUCTURAL_ARTIFACT_NAME)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, StoredArtifactRow>(&sql)
                .bind(model_version_id)
                .bind(STRUCTURAL_ARTIFACT_NAME)
                .fetch_optional(pool)
                .await?
        }
    };
    row.ok_or_else(|| ValuationError::Database("model artifact not found".to_string()))
}

fn validate_stored_row(row: &StoredArtifactRow) -> Result<StoredModelValidation, ValuationError> {
    let actual_hash = sha256_hex(&row.artifact_bytes);
    if actual_hash != row.sha256 {
        return Err(ValuationError::InvalidArtifact(format!(
            "artifact hash mismatch for model {}",
            row.model_version_id
        )));
    }
    if row.model_kind != "structural" || row.artifact_format_version != 1 {
        return Err(ValuationError::InvalidArtifact(
            "unsupported model kind or artifact format".to_string(),
        ));
    }
    let artifact: StructuralArtifactV1 = serde_json::from_slice(&row.artifact_bytes)?;
    artifact.validate()?;
    if artifact.snapshot_id != row.snapshot_id {
        return Err(ValuationError::InvalidArtifact(
            "artifact and model version reference different snapshots".to_string(),
        ));
    }
    let report: ValidationReport = serde_json::from_str(&row.metrics_json)?;
    Ok(StoredModelValidation {
        model_version_id: row.model_version_id,
        model_kind: row.model_kind.clone(),
        state: row.state.clone(),
        snapshot_id: row.snapshot_id,
        artifact_sha256: row.sha256.clone(),
        artifact_valid: true,
        activation_gates_pass: report.activation_gates_pass,
    })
}

fn validate_activatable(row: &StoredArtifactRow) -> Result<(), ValuationError> {
    if row.state != "candidate" {
        return Err(ValuationError::ValidationGate(
            "only a candidate model can be activated".to_string(),
        ));
    }
    let validation = validate_stored_row(row)?;
    if !validation.activation_gates_pass {
        return Err(ValuationError::ValidationGate(
            "candidate did not pass activation gates".to_string(),
        ));
    }
    Ok(())
}

fn support_name(support: SupportGrade) -> &'static str {
    match support {
        SupportGrade::Low => "low",
        SupportGrade::Medium => "medium",
        SupportGrade::High => "high",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::valuation::structural::fit_structural;
    use crate::valuation::types::TrainingListing;
    use crate::valuation::validation::{validate_structural, ValidationReport};

    fn rows() -> Vec<TrainingListing> {
        (0..3)
            .map(|index| TrainingListing {
                listing_id: index + 1,
                duplicate_group_key: format!("group-{index}"),
                category_key: None,
                manufacturer_id: 1,
                model_id: 1,
                variant_id: 1,
                model_year: 2010 + index,
                snapshot_year: 2026,
                asking_price_usd: 150_000.0 + index as f64 * 10_000.0,
                airframe_hours: Some(1000.0 - index as f64 * 100.0),
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

    async fn candidate(
        db: &AppDb,
        snapshot_id: i64,
    ) -> (i64, StructuralArtifactV1, ValidationReport) {
        let config = StructuralFitConfig::default();
        let mut artifact = fit_structural(&rows(), &config).unwrap();
        artifact.snapshot_id = snapshot_id;
        let mut report = validate_structural(&rows(), &config).unwrap();
        report.activation_gates_pass = true;
        report.gate_reasons.clear();
        let id = persist_structural_candidate(db, snapshot_id, &artifact, &report, &config)
            .await
            .unwrap();
        (id, artifact, report)
    }

    #[tokio::test]
    async fn activation_is_single_active_and_transactional() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let snapshot_id = snapshot_id(&db).await;
        let (first, _, _) = candidate(&db, snapshot_id).await;
        activate_model_version(&db, first).await.unwrap();
        let (second, _, _) = candidate(&db, snapshot_id).await;
        activate_model_version(&db, second).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let active = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM valuation_model_versions WHERE state = 'active'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let first_state = sqlx::query_scalar::<_, String>(
            "SELECT state FROM valuation_model_versions WHERE id = ?",
        )
        .bind(first)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(active, 1);
        assert_eq!(first_state, "retired");
    }

    #[tokio::test]
    async fn corrupt_artifact_hash_fails_closed() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let snapshot_id = snapshot_id(&db).await;
        let (model_version_id, _, _) = candidate(&db, snapshot_id).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE valuation_model_artifacts SET artifact_bytes = ? WHERE model_version_id = ?",
        )
        .bind(b"corrupt".as_slice())
        .bind(model_version_id)
        .execute(pool)
        .await
        .unwrap();
        let error = validate_model_version(&db, model_version_id)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("hash mismatch"));
    }

    #[test]
    fn postgres_schema_uses_bytea_and_cascades_candidate_children() {
        let schema = include_str!("../../aircost/webapp/schema.postgres.sql");
        assert!(schema.contains("artifact_bytes BYTEA NOT NULL"));
        assert!(schema.matches("ON DELETE CASCADE").count() >= 3);
    }
}
