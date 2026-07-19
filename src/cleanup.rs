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
    pub aircraft_model_variant_default_avionics: u64,
    pub aircraft_model_variant_price_points: u64,
    pub aircraft_model_spec_versions: u64,
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

    report.aircraft_model_variant_default_avionics = execute_query_count!(
        db,
        r#"
        DELETE FROM aircraft_model_variant_default_avionics
        WHERE aircraft_model_variant_id IN (
          SELECT variant.id
          FROM aircraft_model_variants variant
          WHERE NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listings listing
            WHERE listing.aircraft_model_variant_id = variant.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM rental_aircraft_offerings offering
            WHERE offering.aircraft_model_variant_id = variant.id
          )
        )
        "#
    )?;

    report.aircraft_model_variant_price_points = execute_query_count!(
        db,
        r#"
        DELETE FROM aircraft_model_variant_price_points
        WHERE aircraft_model_variant_id IN (
          SELECT variant.id
          FROM aircraft_model_variants variant
          WHERE NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listings listing
            WHERE listing.aircraft_model_variant_id = variant.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM rental_aircraft_offerings offering
            WHERE offering.aircraft_model_variant_id = variant.id
          )
        )
        "#
    )?;

    report.aircraft_model_spec_versions = execute_query_count!(
        db,
        r#"
        DELETE FROM aircraft_model_spec_versions
        WHERE aircraft_model_variant_id IN (
          SELECT variant.id
          FROM aircraft_model_variants variant
          WHERE NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listings listing
            WHERE listing.aircraft_model_variant_id = variant.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM rental_aircraft_offerings offering
            WHERE offering.aircraft_model_variant_id = variant.id
          )
        )
        "#
    )?;

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
          FROM aircraft_model_spec_versions spec
          WHERE spec.aircraft_model_variant_id = aircraft_model_variants.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_model_variant_price_points price_point
          WHERE price_point.aircraft_model_variant_id = aircraft_model_variants.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_model_variant_default_avionics default_avionics
          WHERE default_avionics.aircraft_model_variant_id = aircraft_model_variants.id
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
          FROM aircraft_model_spec_versions spec
          WHERE spec.aircraft_model_id = aircraft_models.id
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
        "#
    )?;

    report.avionics_models = execute_query_count!(
        db,
        r#"
        DELETE FROM avionics_models
        WHERE NOT EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_avionics listing_link
          WHERE listing_link.avionics_model_id = avionics_models.id
        )
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_model_variant_default_avionics default_link
          WHERE default_link.avionics_model_id = avionics_models.id
        )
        "#
    )?;

    report.avionics_manufacturers = execute_query_count!(
        db,
        r#"
        DELETE FROM avionics_manufacturers
        WHERE NOT EXISTS (
          SELECT 1
          FROM avionics_models model
          WHERE model.avionics_manufacturer_id = avionics_manufacturers.id
        )
        "#
    )?;

    report.avionics_types = execute_query_count!(
        db,
        r#"
        DELETE FROM avionics_types
        WHERE NOT EXISTS (
          SELECT 1
          FROM avionics_models model
          WHERE model.avionics_type_id = avionics_types.id
        )
        "#
    )?;

    report.engine_models = execute_query_count!(
        db,
        r#"
        DELETE FROM engine_models
        WHERE NOT EXISTS (
          SELECT 1
          FROM aircraft_model_spec_versions spec
          WHERE spec.engine_model_id = engine_models.id
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
          FROM aircraft_model_spec_versions spec
          WHERE spec.propeller_model_id = propeller_models.id
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::db::AppDb;

    use super::*;

    macro_rules! execute_query {
        ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {{
            let sql = $db.sql($sql);
            match $db.backend() {
                DatabaseBackend::Sqlite(pool) => {
                    sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|_| ())
                }
                DatabaseBackend::Postgres(pool) => {
                    sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|_| ())
                }
            }
        }};
    }

    macro_rules! query_scalar_one {
        ($db:expr, $ty:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
            let sql = $db.sql($sql);
            match $db.backend() {
                DatabaseBackend::Sqlite(pool) => {
                    sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_one(pool).await
                }
                DatabaseBackend::Postgres(pool) => {
                    sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_one(pool).await
                }
            }
        }};
    }

    #[tokio::test]
    async fn cleanup_removes_unreferenced_aircraft_and_component_rows() {
        let (db, path) = test_db().await;
        seed_unreferenced_aircraft_graph(&db).await;

        let report = cleanup_orphan_records(&db)
            .await
            .expect("cleanup should succeed");

        assert_eq!(report.aircraft_model_variants, 1);
        assert_eq!(report.aircraft_models, 1);
        assert_eq!(report.aircraft_manufacturers, 1);
        assert_eq!(report.aircraft_model_spec_versions, 1);
        assert_eq!(report.aircraft_model_variant_price_points, 1);
        assert_eq!(report.aircraft_model_variant_default_avionics, 1);
        assert_eq!(report.avionics_models, 1);
        assert_eq!(report.avionics_manufacturers, 1);
        assert_eq!(report.avionics_types, 1);
        assert_eq!(report.engine_models, 1);
        assert_eq!(report.engine_manufacturers, 1);
        assert_eq!(report.propeller_models, 1);
        assert_eq!(report.propeller_manufacturers, 1);

        assert_eq!(table_count(&db, "aircraft_model_variants").await, 0);
        assert_eq!(table_count(&db, "avionics_models").await, 0);
        assert_eq!(table_count(&db, "engine_models").await, 0);
        assert_eq!(table_count(&db, "propeller_models").await, 0);

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn cleanup_keeps_rows_referenced_by_listing_roots() {
        let (db, path) = test_db().await;
        seed_referenced_aircraft_graph(&db).await;

        let report = cleanup_orphan_records(&db)
            .await
            .expect("cleanup should succeed");

        assert_eq!(report.aircraft_model_variants, 0);
        assert_eq!(report.aircraft_models, 0);
        assert_eq!(report.aircraft_manufacturers, 0);
        assert_eq!(report.avionics_models, 0);
        assert_eq!(report.avionics_manufacturers, 0);
        assert_eq!(report.avionics_types, 0);
        assert_eq!(table_count(&db, "aircraft_model_variants").await, 1);
        assert_eq!(table_count(&db, "avionics_models").await, 1);

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    async fn test_db() -> (AppDb, std::path::PathBuf) {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aircost-cleanup-{}-{unique_suffix}.sqlite3",
            std::process::id()
        ));
        let database_url = format!("sqlite://{}", path.to_string_lossy());
        let db = AppDb::connect(&database_url)
            .await
            .expect("test database should initialize");
        (db, path)
    }

    async fn seed_unreferenced_aircraft_graph(db: &AppDb) {
        let aircraft_manufacturer_id = insert_named(db, "aircraft_manufacturers", "Cessna").await;
        let aircraft_model_id = insert_aircraft_model(db, aircraft_manufacturer_id).await;
        let variant_id = insert_aircraft_variant(db, aircraft_model_id).await;
        let avionics_model_id = insert_avionics_model(db).await;
        let engine_model_id = insert_engine_model(db).await;
        let propeller_model_id = insert_propeller_model(db).await;

        execute_query!(
            db,
            r#"
            INSERT INTO aircraft_model_spec_versions (
              aircraft_model_id,
              aircraft_model_variant_id,
              effective_from,
              engine_model_id,
              propeller_model_id
            )
            VALUES (?, ?, '2026-01-01', ?, ?)
            "#,
            aircraft_model_id,
            variant_id,
            engine_model_id,
            propeller_model_id
        )
        .expect("spec version should seed");
        execute_query!(
            db,
            r#"
            INSERT INTO aircraft_model_variant_price_points (
              aircraft_model_variant_id,
              model_year,
              purchase_price_new_usd,
              purchase_price_reference_year,
              source_url,
              source_title,
              source_notes,
              source_confidence
            )
            VALUES (?, 2023, 700000, 2023, 'https://example.test', 'fixture', 'fixture', 'high')
            "#,
            variant_id
        )
        .expect("price point should seed");
        execute_query!(
            db,
            r#"
            INSERT INTO aircraft_model_variant_default_avionics (
              aircraft_model_variant_id,
              model_year,
              avionics_model_id,
              quantity,
              source_url,
              source_title,
              source_notes,
              source_confidence
            )
            VALUES (?, 2023, ?, 1, 'https://example.test', 'fixture', 'fixture', 'high')
            "#,
            variant_id,
            avionics_model_id
        )
        .expect("default avionics should seed");
    }

    async fn seed_referenced_aircraft_graph(db: &AppDb) {
        let aircraft_manufacturer_id = insert_named(db, "aircraft_manufacturers", "Cessna").await;
        let aircraft_model_id = insert_aircraft_model(db, aircraft_manufacturer_id).await;
        let variant_id = insert_aircraft_variant(db, aircraft_model_id).await;
        let avionics_model_id = insert_avionics_model(db).await;
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let listing_id = query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id,
              created_by_user_id,
              model_year,
              asking_price_usd,
              airframe_hours,
              engine_hours,
              propeller_hours
            )
            VALUES (?, ?, 2023, 700000, 10, 10, 10)
            RETURNING id
            "#,
            variant_id,
            user.id
        )
        .expect("listing should seed");
        execute_query!(
            db,
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id,
              avionics_model_id
            )
            VALUES (?, ?)
            "#,
            listing_id,
            avionics_model_id
        )
        .expect("listing avionics should seed");
    }

    async fn insert_named(db: &AppDb, table: &str, name: &str) -> i64 {
        let sql = format!("INSERT INTO {table} (name, normalized_name) VALUES (?, ?) RETURNING id");
        query_scalar_one!(db, i64, &sql, name, name.to_ascii_lowercase())
            .expect("named row should seed")
    }

    async fn insert_aircraft_model(db: &AppDb, aircraft_manufacturer_id: i64) -> i64 {
        query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO aircraft_models (
              aircraft_manufacturer_id,
              name,
              normalized_name
            )
            VALUES (?, '182 SKYLANE', '182 skylane')
            RETURNING id
            "#,
            aircraft_manufacturer_id
        )
        .expect("aircraft model should seed")
    }

    async fn insert_aircraft_variant(db: &AppDb, aircraft_model_id: i64) -> i64 {
        query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO aircraft_model_variants (
              aircraft_model_id,
              name,
              normalized_name
            )
            VALUES (?, '182T', '182t')
            RETURNING id
            "#,
            aircraft_model_id
        )
        .expect("aircraft variant should seed")
    }

    async fn insert_avionics_model(db: &AppDb) -> i64 {
        let manufacturer_id = insert_named(db, "avionics_manufacturers", "Garmin").await;
        let type_id = insert_named(db, "avionics_types", "Integrated Flight Deck").await;
        query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id,
              avionics_type_id,
              name,
              normalized_name
            )
            VALUES (?, ?, 'G1000 NXi', 'g1000 nxi')
            RETURNING id
            "#,
            manufacturer_id,
            type_id
        )
        .expect("avionics model should seed")
    }

    async fn insert_engine_model(db: &AppDb) -> i64 {
        let manufacturer_id = insert_named(db, "engine_manufacturers", "Lycoming").await;
        query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO engine_models (
              engine_manufacturer_id,
              name,
              normalized_name
            )
            VALUES (?, 'IO-540-AB1A5', 'io 540 ab1a5')
            RETURNING id
            "#,
            manufacturer_id
        )
        .expect("engine model should seed")
    }

    async fn insert_propeller_model(db: &AppDb) -> i64 {
        let manufacturer_id = insert_named(db, "propeller_manufacturers", "McCauley").await;
        query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO propeller_models (
              propeller_manufacturer_id,
              name,
              normalized_name
            )
            VALUES (?, '3 Blade', '3 blade')
            RETURNING id
            "#,
            manufacturer_id
        )
        .expect("propeller model should seed")
    }

    async fn table_count(db: &AppDb, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        query_scalar_one!(db, i64, &sql).expect("table count should succeed")
    }
}
