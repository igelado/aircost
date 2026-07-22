use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};

use super::comparable::{ComparableConfig, ComparableModel};
use super::dataset::{newest_snapshot, require_snapshot_faa_admission, sha256_hex};
use super::structural::{StructuralFitConfig, StructuralModel};
use super::types::{StructuralArtifactV1, SupportGrade, ValuationError};
use super::validation::{FoldPrediction, ValidationReport, VALIDATION_EVIDENCE_VERSION};
use super::ValuationModel;

pub const STRUCTURAL_ARTIFACT_NAME: &str = "structural-v1.json";
pub const STRUCTURAL_ARTIFACT_MEDIA_TYPE: &str = "application/vnd.aircost.structural+json";

const MINIMUM_COMPARABLE_GROUPS: usize = 5;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServingValuationState {
    Calibrated,
    ComparableFallback,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ServingValuationStatus {
    pub state: ServingValuationState,
    pub calibrated: bool,
    pub listing_only_available: bool,
    pub model_kind: Option<String>,
    pub model_version_id: Option<i64>,
    pub snapshot_id: Option<i64>,
    pub warnings: Vec<String>,
}

pub struct LoadedServingValuation {
    pub model: Option<Arc<dyn ValuationModel>>,
    pub status: ServingValuationStatus,
}

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
    ensure_current_snapshot_schema(db, snapshot_id).await?;
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
    if model_kind(db, model_version_id).await?.as_deref() == Some("dnn") {
        #[cfg(feature = "dnn")]
        return super::dnn::validate_dnn_model_version(db, model_version_id).await;
        #[cfg(not(feature = "dnn"))]
        return Err(ValuationError::InvalidArtifact(
            "DNN validation requires rebuilding with --features dnn".to_string(),
        ));
    }
    validate_structural_model_version(db, model_version_id).await
}

pub(crate) async fn validate_structural_model_version(
    db: &AppDb,
    model_version_id: i64,
) -> Result<StoredModelValidation, ValuationError> {
    let row = load_artifact_row(db, model_version_id).await?;
    ensure_current_snapshot_schema(db, row.snapshot_id).await?;
    validate_stored_row(&row)
}

async fn ensure_current_snapshot_schema(
    db: &AppDb,
    snapshot_id: i64,
) -> Result<(), ValuationError> {
    let sql = db.sql("SELECT feature_schema_version FROM valuation_snapshots WHERE id = ?");
    let version = match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_scalar::<_, i64>(&sql)
            .bind(snapshot_id)
            .fetch_optional(pool)
            .await?
            .map(|version| version as u32),
        DatabaseBackend::Postgres(pool) => sqlx::query_scalar::<_, i32>(&sql)
            .bind(snapshot_id)
            .fetch_optional(pool)
            .await?
            .map(|version| version as u32),
    }
    .ok_or_else(|| ValuationError::Database(format!("snapshot {snapshot_id} not found")))?;
    if version != crate::valuation::FEATURE_SCHEMA_VERSION {
        return Err(ValuationError::ValidationGate(format!(
            "snapshot {snapshot_id} feature schema {version} does not match current schema {}",
            crate::valuation::FEATURE_SCHEMA_VERSION
        )));
    }
    require_snapshot_faa_admission(db, snapshot_id).await?;
    Ok(())
}

pub async fn activate_model_version(
    db: &AppDb,
    model_version_id: i64,
) -> Result<StoredModelValidation, ValuationError> {
    if model_kind(db, model_version_id).await?.as_deref() == Some("dnn") {
        #[cfg(feature = "dnn")]
        return super::dnn::activate_dnn_model_version(db, model_version_id).await;
        #[cfg(not(feature = "dnn"))]
        return Err(ValuationError::InvalidArtifact(
            "DNN activation requires rebuilding with --features dnn".to_string(),
        ));
    }
    let preflight = validate_structural_model_version(db, model_version_id).await?;
    if !preflight.activation_gates_pass {
        return Err(ValuationError::ValidationGate(
            "candidate did not pass current activation gates and evidence requirements".to_string(),
        ));
    }
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
        "UPDATE valuation_model_versions SET state = 'retired' WHERE model_kind IN ('structural', 'dnn') AND state = 'active'",
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
            sqlx::query(&retire).execute(&mut *transaction).await?;
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
            sqlx::query(&retire).execute(&mut *transaction).await?;
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

pub async fn load_serving_valuation(db: &AppDb) -> Result<LoadedServingValuation, ValuationError> {
    let mut warnings = Vec::new();
    #[cfg(feature = "dnn")]
    match super::dnn::load_active_dnn_model(db).await {
        Ok(Some(model)) => {
            let status = calibrated_status(model.as_ref(), warnings);
            return Ok(LoadedServingValuation {
                model: Some(model),
                status,
            });
        }
        Ok(None) => {}
        Err(error) => {
            let warning = format!(
                "active DNN artifact rejected; using the approved structural fallback: {error}"
            );
            eprintln!("{warning}");
            warnings.push(warning);
        }
    }
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
        let loaded = async {
            let row = load_artifact_row(db, model_version_id).await?;
            ensure_current_snapshot_schema(db, row.snapshot_id).await?;
            let validation = validate_stored_row(&row)?;
            if !validation.activation_gates_pass {
                return Err(ValuationError::ValidationGate(format!(
                    "active structural model {model_version_id} does not satisfy current activation evidence requirements"
                )));
            }
            let report: ValidationReport = serde_json::from_str(&row.metrics_json)?;
            let artifact: StructuralArtifactV1 = serde_json::from_slice(&row.artifact_bytes)?;
            let model: Arc<dyn ValuationModel> =
                Arc::new(StructuralModel::new(model_version_id, artifact)?);
            Ok::<_, ValuationError>((model, report.scope_warnings))
        }
        .await;
        match loaded {
            Ok((model, scope_warnings)) => {
                warnings.extend(scope_warnings);
                let status = calibrated_status(model.as_ref(), warnings);
                return Ok(LoadedServingValuation {
                    model: Some(model),
                    status,
                });
            }
            Err(error) => {
                let warning = format!(
                    "active structural model {model_version_id} was rejected; trying a comparable fallback: {error}"
                );
                eprintln!("{warning}");
                warnings.push(warning);
            }
        }
    }
    let snapshot = match newest_snapshot(db).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warnings.push(format!(
                "newest valuation snapshot was rejected for serving: {error}"
            ));
            None
        }
    };
    if let Some((snapshot_id, rows)) = snapshot {
        let unique_groups = rows
            .iter()
            .map(|row| row.duplicate_group_key.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let rows_valid = rows.iter().all(|row| row.validate().is_ok());
        if unique_groups >= MINIMUM_COMPARABLE_GROUPS && rows_valid {
            let model: Arc<dyn ValuationModel> = Arc::new(ComparableModel::new(
                0,
                snapshot_id,
                rows,
                ComparableConfig::default(),
            )?);
            warnings.push(
                "No approved model artifact is active; estimates use an uncalibrated adjusted-comparable snapshot fallback."
                    .to_string(),
            );
            return Ok(LoadedServingValuation {
                model: Some(model),
                status: ServingValuationStatus {
                    state: ServingValuationState::ComparableFallback,
                    calibrated: false,
                    listing_only_available: true,
                    model_kind: Some("comparable".to_string()),
                    model_version_id: None,
                    snapshot_id: Some(snapshot_id),
                    warnings,
                },
            });
        }
        warnings.push(format!(
            "Snapshot {snapshot_id} is not eligible for comparable serving: it has {unique_groups} unique valid aircraft groups; at least {MINIMUM_COMPARABLE_GROUPS} are required."
        ));
    }
    warnings.push(
        "No approved valuation artifact or eligible comparable snapshot is available; primary market estimates are disabled."
            .to_string(),
    );
    Ok(LoadedServingValuation {
        model: None,
        status: ServingValuationStatus {
            state: ServingValuationState::Unavailable,
            calibrated: false,
            listing_only_available: false,
            model_kind: None,
            model_version_id: None,
            snapshot_id: None,
            warnings,
        },
    })
}

fn calibrated_status(model: &dyn ValuationModel, warnings: Vec<String>) -> ServingValuationStatus {
    ServingValuationStatus {
        state: ServingValuationState::Calibrated,
        calibrated: true,
        listing_only_available: true,
        model_kind: Some(model.model_kind().to_string()),
        model_version_id: Some(model.model_version_id()),
        snapshot_id: Some(model.snapshot_id()),
        warnings,
    }
}

pub async fn load_structural_model_version(
    db: &AppDb,
    model_version_id: i64,
) -> Result<Arc<dyn ValuationModel>, ValuationError> {
    let row = load_artifact_row(db, model_version_id).await?;
    ensure_current_snapshot_schema(db, row.snapshot_id).await?;
    validate_stored_row(&row)?;
    let artifact: StructuralArtifactV1 = serde_json::from_slice(&row.artifact_bytes)?;
    Ok(Arc::new(StructuralModel::new(model_version_id, artifact)?))
}

async fn model_kind(db: &AppDb, model_version_id: i64) -> Result<Option<String>, ValuationError> {
    let sql = db.sql("SELECT model_kind FROM valuation_model_versions WHERE id = ?");
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query_scalar::<_, String>(&sql)
            .bind(model_version_id)
            .fetch_optional(pool)
            .await?),
        DatabaseBackend::Postgres(pool) => Ok(sqlx::query_scalar::<_, String>(&sql)
            .bind(model_version_id)
            .fetch_optional(pool)
            .await?),
    }
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
    let current_evidence_pass = report.validation_evidence_version == VALIDATION_EVIDENCE_VERSION
        && report.calibration_aircraft_count > 0
        && report.evaluation_aircraft_count > 0
        && report.comparable_shadow_evidence
        && (!report.leave_one_model_out_required || report.leave_one_model_out_evidence);
    Ok(StoredModelValidation {
        model_version_id: row.model_version_id,
        model_kind: row.model_kind.clone(),
        state: row.state.clone(),
        snapshot_id: row.snapshot_id,
        artifact_sha256: row.sha256.clone(),
        artifact_valid: true,
        activation_gates_pass: report.activation_gates_pass && current_evidence_pass,
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
                model_id: index % 2 + 1,
                variant_id: index % 2 + 1,
                model_year: 2010 + index,
                snapshot_year: 2026,
                asking_price_usd: 150_000.0 + index as f64 * 10_000.0,
                airframe_hours: Some(1000.0 - index as f64 * 100.0),
                engine_times: vec![],
                propeller_times: vec![],
                equipment_tokens: vec![],
                valuation_facts: vec![],
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
            ) VALUES (
              '2026-07-20', lower(hex(randomblob(32))),
              '{"faa_admission":{"schema_version":1,"included_listings":{}}}',
              ?, 3, 0
            )
            RETURNING id
            "#,
        )
        .bind(crate::valuation::FEATURE_SCHEMA_VERSION as i64)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn insert_snapshot_rows(db: &AppDb, snapshot_id: i64, rows: &[TrainingListing]) {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        for row in rows {
            sqlx::query(
                r#"
                INSERT INTO valuation_snapshot_rows (
                  snapshot_id, source_listing_id, duplicate_group_key, inclusion_flag,
                  exclusion_reason, feature_json, target_price_usd, row_sha256
                ) VALUES (?, ?, ?, TRUE, NULL, ?, ?, ?)
                "#,
            )
            .bind(snapshot_id)
            .bind(row.listing_id)
            .bind(&row.duplicate_group_key)
            .bind(serde_json::to_string(row).unwrap())
            .bind(row.asking_price_usd)
            .bind(format!("hash-{}", row.listing_id))
            .execute(pool)
            .await
            .unwrap();
        }
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

    #[tokio::test]
    async fn serving_status_is_explicit_when_valuation_is_unavailable() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let serving = load_serving_valuation(&db).await.unwrap();
        assert!(serving.model.is_none());
        assert_eq!(serving.status.state, ServingValuationState::Unavailable);
        assert!(!serving.status.calibrated);
        assert!(!serving.status.listing_only_available);
        assert!(!serving.status.warnings.is_empty());
    }

    #[tokio::test]
    async fn ungrounded_legacy_snapshot_cannot_become_a_comparable_fallback() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let snapshot_id = snapshot_id(&db).await;
        insert_snapshot_rows(&db, snapshot_id, &rows()[..2]).await;
        let serving = load_serving_valuation(&db).await.unwrap();
        assert!(serving.model.is_none());
        assert_eq!(serving.status.state, ServingValuationState::Unavailable);
        assert!(serving
            .status
            .warnings
            .iter()
            .any(|warning| warning.contains("FAA-ineligible")
                && warning.contains("listing_not_found=2")));
    }

    #[tokio::test]
    async fn stale_feature_snapshot_cannot_become_a_comparable_fallback() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let snapshot_id = snapshot_id(&db).await;
        let mut snapshot_rows = rows();
        for listing_id in 4..=5 {
            let mut row = snapshot_rows[0].clone();
            row.listing_id = listing_id;
            row.duplicate_group_key = format!("group-{listing_id}");
            snapshot_rows.push(row);
        }
        insert_snapshot_rows(&db, snapshot_id, &snapshot_rows).await;
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("UPDATE valuation_snapshots SET feature_schema_version = ? WHERE id = ?")
            .bind(crate::valuation::FEATURE_SCHEMA_VERSION.saturating_sub(1) as i64)
            .bind(snapshot_id)
            .execute(pool)
            .await
            .unwrap();
        let config = StructuralFitConfig::default();
        let mut artifact = fit_structural(&snapshot_rows, &config).unwrap();
        artifact.snapshot_id = snapshot_id;
        let report = validate_structural(&snapshot_rows, &config).unwrap();
        assert!(
            persist_structural_candidate(&db, snapshot_id, &artifact, &report, &config)
                .await
                .is_err()
        );
        let serving = load_serving_valuation(&db).await.unwrap();
        assert!(serving.model.is_none());
        assert_eq!(serving.status.state, ServingValuationState::Unavailable);
        assert!(serving
            .status
            .warnings
            .iter()
            .any(|warning| warning.contains("feature schema")));
    }

    #[tokio::test]
    async fn serving_status_identifies_active_model_and_snapshot() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let snapshot_id = snapshot_id(&db).await;
        let (model_version_id, _, _) = candidate(&db, snapshot_id).await;
        activate_model_version(&db, model_version_id).await.unwrap();
        let serving = load_serving_valuation(&db).await.unwrap();
        assert!(serving.model.is_some());
        assert_eq!(serving.status.state, ServingValuationState::Calibrated);
        assert_eq!(serving.status.model_version_id, Some(model_version_id));
        assert_eq!(serving.status.snapshot_id, Some(snapshot_id));
    }

    #[tokio::test]
    async fn pre_hardening_validation_evidence_cannot_be_activated() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let snapshot_id = snapshot_id(&db).await;
        let config = StructuralFitConfig::default();
        let mut artifact = fit_structural(&rows(), &config).unwrap();
        artifact.snapshot_id = snapshot_id;
        let mut report = validate_structural(&rows(), &config).unwrap();
        report.activation_gates_pass = true;
        report.gate_reasons.clear();
        report.validation_evidence_version = 0;
        report.calibration_aircraft_count = 0;
        report.evaluation_aircraft_count = 0;
        report.comparable_shadow_evidence = false;
        let model_version_id =
            persist_structural_candidate(&db, snapshot_id, &artifact, &report, &config)
                .await
                .unwrap();
        let validation = validate_model_version(&db, model_version_id).await.unwrap();
        assert!(!validation.activation_gates_pass);
        assert!(activate_model_version(&db, model_version_id).await.is_err());
    }

    #[tokio::test]
    async fn rejected_active_model_degrades_to_explicit_unavailable_status() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let snapshot_id = snapshot_id(&db).await;
        let (model_version_id, _, _) = candidate(&db, snapshot_id).await;
        activate_model_version(&db, model_version_id).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        sqlx::query("UPDATE valuation_model_versions SET metrics_json = '{}' WHERE id = ?")
            .bind(model_version_id)
            .execute(pool)
            .await
            .unwrap();
        let serving = load_serving_valuation(&db).await.unwrap();
        assert!(serving.model.is_none());
        assert_eq!(serving.status.state, ServingValuationState::Unavailable);
        assert!(serving
            .status
            .warnings
            .iter()
            .any(|warning| warning.contains("active structural model")));
    }

    #[test]
    fn postgres_schema_uses_bytea_and_cascades_candidate_children() {
        let schema = include_str!("../../schema/postgres.sql");
        assert!(schema.contains("artifact_bytes BYTEA NOT NULL"));
        assert!(schema.matches("ON DELETE CASCADE").count() >= 3);
        assert!(schema.contains("INTO locked_catalog_status"));
        assert!(schema.contains("WHERE model.id = OLD.avionics_model_id\n  FOR UPDATE"));
    }
}
