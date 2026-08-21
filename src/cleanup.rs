use serde::Serialize;

use crate::db::{AppDb, DatabaseBackend};

macro_rules! execute_query_count {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|result| result.rows_affected())
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|result| result.rows_affected())
            }
        }
    }};
}

#[derive(Debug)]
pub enum CleanupError {
    Database(String),
}

impl std::fmt::Display for CleanupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CleanupError::Database(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for CleanupError {}

impl From<sqlx::Error> for CleanupError {
    fn from(error: sqlx::Error) -> Self {
        CleanupError::Database(error.to_string())
    }
}

type CleanupResult<T> = Result<T, CleanupError>;

#[derive(Clone, Debug, Default, Serialize)]
pub struct OrphanCleanupReport {
    pub aircraft_model_variants: u64,
    pub aircraft_models: u64,
    pub aircraft_manufacturers: u64,
    pub avionics_models: u64,
    pub avionics_manufacturers: u64,
    pub avionics_types: u64,
    pub engine_models: u64,
    pub engine_manufacturers: u64,
    pub propeller_models: u64,
    pub propeller_manufacturers: u64,
}

pub async fn cleanup_orphan_records(db: &AppDb) -> CleanupResult<OrphanCleanupReport> {
    let mut report = OrphanCleanupReport::default();

    report.aircraft_model_variants = execute_query_count!(
        db,
        r#"
        DELETE FROM aircraft_model_variants
        WHERE NOT EXISTS (
          SELECT 1
          FROM aircraft_sale_listings listing
          WHERE listing.aircraft_model_variant_id = aircraft_model_variants.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM rental_aircraft_offerings offering
          WHERE offering.aircraft_model_variant_id = aircraft_model_variants.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_valuation_compatibility_projections projection
          WHERE projection.aircraft_model_variant_id =
            aircraft_model_variants.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
          WHERE placeholder.aircraft_model_variant_id =
            aircraft_model_variants.id
        )
        "#
    )?;

    report.aircraft_models = execute_query_count!(
        db,
        r#"
        DELETE FROM aircraft_models
        WHERE NOT EXISTS (
          SELECT 1
          FROM aircraft_model_variants variant
          WHERE variant.aircraft_model_id = aircraft_models.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_valuation_compatibility_projections projection
          JOIN aircraft_model_variants variant
            ON variant.id = projection.aircraft_model_variant_id
          WHERE variant.aircraft_model_id = aircraft_models.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
          WHERE placeholder.aircraft_model_id = aircraft_models.id
        )
        "#
    )?;

    report.aircraft_manufacturers = execute_query_count!(
        db,
        r#"
        DELETE FROM aircraft_manufacturers
        WHERE NOT EXISTS (
          SELECT 1
          FROM aircraft_models model
          WHERE model.aircraft_manufacturer_id = aircraft_manufacturers.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_valuation_compatibility_projections projection
          JOIN aircraft_model_variants variant
            ON variant.id = projection.aircraft_model_variant_id
          JOIN aircraft_models model
            ON model.id = variant.aircraft_model_id
          WHERE model.aircraft_manufacturer_id = aircraft_manufacturers.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
          WHERE placeholder.aircraft_manufacturer_id =
            aircraft_manufacturers.id
        )
        "#
    )?;

    // Catalog candidates, manufacturer spellings, and capability rows can be
    // referenced by pending-review payloads and evidence-backed identity
    // decisions that are not represented by a simple foreign key. Their
    // lifecycle belongs to listing review or explicit catalog consolidation,
    // never this generic relational-orphan sweep.

    report.engine_models = execute_query_count!(
        db,
        r#"
        DELETE FROM engine_models
        WHERE NOT EXISTS (
          SELECT 1
          FROM aircraft_sale_listings listing
          WHERE listing.installed_engine_model_id = engine_models.id
        )
        "#
    )?;

    report.engine_manufacturers = execute_query_count!(
        db,
        r#"
        DELETE FROM engine_manufacturers
        WHERE NOT EXISTS (
          SELECT 1
          FROM engine_models model
          WHERE model.engine_manufacturer_id = engine_manufacturers.id
        )
        "#
    )?;

    report.propeller_models = execute_query_count!(
        db,
        r#"
        DELETE FROM propeller_models
        WHERE NOT EXISTS (
          SELECT 1
          FROM aircraft_sale_listings listing
          WHERE listing.installed_propeller_model_id = propeller_models.id
        )
        "#
    )?;

    report.propeller_manufacturers = execute_query_count!(
        db,
        r#"
        DELETE FROM propeller_manufacturers
        WHERE NOT EXISTS (
          SELECT 1
          FROM propeller_models model
          WHERE model.propeller_manufacturer_id = propeller_manufacturers.id
        )
        "#
    )?;

    Ok(report)
}

#[cfg(test)]
mod tests {
    use crate::db::{AppDb, DatabaseBackend};

    use super::cleanup_orphan_records;

    #[tokio::test]
    async fn cleanup_removes_unreferenced_aircraft_hierarchy() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let manufacturer_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Cleanup Maker', 'cleanup maker') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) VALUES (?, 'Cleanup Model', 'cleanup model') RETURNING id",
        )
        .bind(manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) VALUES (?, 'Cleanup Variant', 'cleanup variant')",
        )
        .bind(model_id)
        .execute(pool)
        .await
        .unwrap();

        let report = cleanup_orphan_records(&db).await.unwrap();
        assert_eq!(report.aircraft_model_variants, 1);
        assert_eq!(report.aircraft_models, 1);
        assert_eq!(report.aircraft_manufacturers, 1);
    }
}
