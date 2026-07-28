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
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_valuation_compatibility_projections projection
            WHERE projection.aircraft_model_variant_id = variant.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
            WHERE placeholder.aircraft_model_variant_id = variant.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_default_avionics_candidates candidate
            WHERE candidate.aircraft_model_variant_id = variant.id
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
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_valuation_compatibility_projections projection
            WHERE projection.aircraft_model_variant_id = variant.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
            WHERE placeholder.aircraft_model_variant_id = variant.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_default_avionics_candidates candidate
            WHERE candidate.aircraft_model_variant_id = variant.id
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
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_valuation_compatibility_projections projection
            WHERE projection.aircraft_model_variant_id = variant.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
            WHERE placeholder.aircraft_model_variant_id = variant.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_default_avionics_candidates candidate
            WHERE candidate.aircraft_model_variant_id = variant.id
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
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_model_variant_default_avionics_candidates candidate
          WHERE candidate.aircraft_model_variant_id = aircraft_model_variants.id
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

    use crate::avionics::manufacturer::ensure_test_manufacturer_identity;
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
    async fn cleanup_removes_unreferenced_aircraft_rows_but_keeps_approved_catalog_entries() {
        let (db, path) = test_db().await;
        let graph = seed_unreferenced_aircraft_graph(&db).await;

        let report = cleanup_orphan_records(&db)
            .await
            .expect("cleanup should succeed");

        assert_eq!(report.aircraft_model_variants, 1);
        assert_eq!(report.aircraft_models, 1);
        assert_eq!(report.aircraft_manufacturers, 1);
        assert_eq!(report.aircraft_model_spec_versions, 1);
        assert_eq!(report.aircraft_model_variant_price_points, 1);
        assert_eq!(report.aircraft_model_variant_default_avionics, 1);
        assert_eq!(report.avionics_models, 0);
        assert_eq!(report.avionics_manufacturers, 0);
        assert_eq!(report.avionics_types, 0);
        assert_eq!(report.engine_models, 1);
        assert_eq!(report.engine_manufacturers, 1);
        assert_eq!(report.propeller_models, 1);
        assert_eq!(report.propeller_manufacturers, 1);

        assert!(
            !row_exists(
                &db,
                "aircraft_model_variants",
                graph.aircraft_model_variant_id
            )
            .await
        );
        assert!(!row_exists(&db, "aircraft_models", graph.aircraft_model_id).await);
        assert!(
            !row_exists(
                &db,
                "aircraft_manufacturers",
                graph.aircraft_manufacturer_id
            )
            .await
        );
        assert_eq!(table_count(&db, "avionics_models").await, 1);
        assert_eq!(table_count(&db, "engine_models").await, 0);
        assert_eq!(table_count(&db, "propeller_models").await, 0);

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn cleanup_keeps_rows_referenced_by_listing_roots() {
        let (db, path) = test_db().await;
        let graph = seed_referenced_aircraft_graph(&db).await;

        let report = cleanup_orphan_records(&db)
            .await
            .expect("cleanup should succeed");

        assert_eq!(report.aircraft_model_variants, 0);
        assert_eq!(report.aircraft_models, 0);
        assert_eq!(report.aircraft_manufacturers, 0);
        assert_eq!(report.avionics_models, 0);
        assert_eq!(report.avionics_manufacturers, 0);
        assert_eq!(report.avionics_types, 0);
        assert!(
            row_exists(
                &db,
                "aircraft_model_variants",
                graph.aircraft_model_variant_id
            )
            .await
        );
        assert_eq!(table_count(&db, "avionics_models").await, 1);

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn cleanup_keeps_hierarchy_and_product_referenced_only_by_pending_default_claim() {
        let (db, path) = test_db().await;
        let aircraft_manufacturer_id =
            insert_named(&db, "aircraft_manufacturers", "Pending Cessna").await;
        let aircraft_model_id = insert_aircraft_model(&db, aircraft_manufacturer_id).await;
        let aircraft_model_variant_id = insert_aircraft_variant(&db, aircraft_model_id).await;
        let avionics_manufacturer_id =
            insert_named(&db, "avionics_manufacturers", "Pending Garmin").await;
        let avionics_type_id = insert_named(&db, "avionics_types", "Pending Navigation").await;
        let avionics_model_id = insert_unreviewed_avionics_model(
            &db,
            avionics_manufacturer_id,
            avionics_type_id,
            "Pending GIA",
            "pending gia",
        )
        .await;
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_model_variant_default_avionics_candidates (
              aircraft_model_variant_id,
              model_year,
              avionics_model_id,
              quantity,
              source_url,
              source_title,
              source_notes,
              source_confidence,
              pending_reason
            ) VALUES (
              ?, 2010, ?, 2,
              'https://example.test/pending-default',
              'Pending factory equipment claim',
              'Exact imported pending claim',
              'high',
              'factory_default_claim_unverified'
            )
            "#,
            aircraft_model_variant_id,
            avionics_model_id
        )
        .expect("pending default claim should seed");

        let report = cleanup_orphan_records(&db)
            .await
            .expect("cleanup should preserve pending default ownership");

        assert_eq!(report.aircraft_model_variants, 0);
        assert_eq!(report.aircraft_models, 0);
        assert_eq!(report.aircraft_manufacturers, 0);
        assert_eq!(report.avionics_models, 0);
        assert!(row_exists(&db, "aircraft_model_variants", aircraft_model_variant_id).await);
        assert!(row_exists(&db, "aircraft_models", aircraft_model_id).await);
        assert!(row_exists(&db, "aircraft_manufacturers", aircraft_manufacturer_id).await);
        assert!(row_exists(&db, "avionics_models", avionics_model_id).await);
        assert_eq!(
            table_count(&db, "aircraft_model_variant_default_avionics_candidates").await,
            1
        );

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn cleanup_keeps_the_schema_owned_pending_compatibility_placeholder() {
        let (db, path) = test_db().await;
        let aircraft_manufacturer_id = query_scalar_one!(
            &db,
            i64,
            "SELECT aircraft_manufacturer_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1"
        )
        .expect("pending placeholder manufacturer should exist");
        let aircraft_model_id = query_scalar_one!(
            &db,
            i64,
            "SELECT aircraft_model_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1"
        )
        .expect("pending placeholder model should exist");
        let aircraft_model_variant_id = query_scalar_one!(
            &db,
            i64,
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listing_pending_compatibility_placeholder WHERE singleton_id = 1"
        )
        .expect("pending placeholder variant should exist");
        let avionics_model_id = insert_avionics_model(&db).await;
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_model_spec_versions (
              aircraft_model_id, aircraft_model_variant_id, effective_from
            ) VALUES (?, ?, '2026-01-01')
            "#,
            aircraft_model_id,
            aircraft_model_variant_id
        )
        .expect("placeholder spec version should seed");
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_model_variant_price_points (
              aircraft_model_variant_id, model_year,
              purchase_price_new_usd, purchase_price_reference_year,
              source_url, source_title, source_notes, source_confidence
            ) VALUES (?, 2026, 1, 2026, 'https://example.test',
                      'placeholder fixture', 'placeholder fixture', 'high')
            "#,
            aircraft_model_variant_id
        )
        .expect("placeholder price point should seed");
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_model_variant_default_avionics (
              aircraft_model_variant_id, model_year, avionics_model_id,
              quantity, source_url, source_title, source_notes,
              source_confidence
            ) VALUES (?, 2026, ?, 1, 'https://example.test',
                      'placeholder fixture', 'placeholder fixture', 'high')
            "#,
            aircraft_model_variant_id,
            avionics_model_id
        )
        .expect("placeholder default avionics should seed");

        let report = cleanup_orphan_records(&db)
            .await
            .expect("cleanup should preserve schema-owned compatibility rows");

        assert_eq!(report.aircraft_model_variant_default_avionics, 0);
        assert_eq!(report.aircraft_model_variant_price_points, 0);
        assert_eq!(report.aircraft_model_spec_versions, 0);
        assert_eq!(report.aircraft_model_variants, 0);
        assert_eq!(report.aircraft_models, 0);
        assert_eq!(report.aircraft_manufacturers, 0);
        assert!(row_exists(&db, "aircraft_model_variants", aircraft_model_variant_id).await);
        assert!(row_exists(&db, "aircraft_models", aircraft_model_id).await);
        assert!(row_exists(&db, "aircraft_manufacturers", aircraft_manufacturer_id).await);

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn cleanup_keeps_every_row_owned_by_a_valuation_compatibility_projection() {
        let (db, path) = test_db().await;
        let graph = seed_unreferenced_aircraft_graph(&db).await;

        // Projection admission is covered by the aircraft-identity schema.
        // This unit test isolates orphan ownership by replacing the guarded
        // table with its one cleanup-relevant key.
        execute_query!(
            &db,
            "DROP TABLE aircraft_valuation_compatibility_projections"
        )
        .expect("projection fixture should replace the guarded table");
        execute_query!(
            &db,
            r#"
            CREATE TABLE aircraft_valuation_compatibility_projections (
              aircraft_model_variant_id INTEGER PRIMARY KEY
            )
            "#
        )
        .expect("projection fixture table should seed");
        execute_query!(
            &db,
            "INSERT INTO aircraft_valuation_compatibility_projections (aircraft_model_variant_id) VALUES (?)",
            graph.aircraft_model_variant_id
        )
        .expect("projection ownership should seed");

        let report = cleanup_orphan_records(&db)
            .await
            .expect("cleanup should preserve projection-owned compatibility rows");

        assert_eq!(report.aircraft_model_variant_default_avionics, 0);
        assert_eq!(report.aircraft_model_variant_price_points, 0);
        assert_eq!(report.aircraft_model_spec_versions, 0);
        assert_eq!(report.aircraft_model_variants, 0);
        assert_eq!(report.aircraft_models, 0);
        assert_eq!(report.aircraft_manufacturers, 0);
        assert!(
            row_exists(
                &db,
                "aircraft_model_variants",
                graph.aircraft_model_variant_id
            )
            .await
        );
        assert!(row_exists(&db, "aircraft_models", graph.aircraft_model_id).await);
        assert!(
            row_exists(
                &db,
                "aircraft_manufacturers",
                graph.aircraft_manufacturer_id
            )
            .await
        );

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn cleanup_keeps_unreviewed_models_used_only_by_a_suite_membership() {
        let (db, path) = test_db().await;
        let manufacturer_id = insert_named(&db, "avionics_manufacturers", "Garmin").await;
        let type_id = insert_named(&db, "avionics_types", "Flight Display").await;
        let suite_id = insert_unreviewed_avionics_model(
            &db,
            manufacturer_id,
            type_id,
            "Test Integrated Suite",
            "test integrated suite",
        )
        .await;
        let component_id = insert_unreviewed_avionics_model(
            &db,
            manufacturer_id,
            type_id,
            "Test Display",
            "test display",
        )
        .await;
        // A migrated database may preserve suite links created before the
        // approved-only trigger existed.
        execute_query!(
            &db,
            "DROP TRIGGER avionics_suite_components_approved_insert"
        )
        .expect("legacy fixture should disable the fresh-schema insert guard");
        execute_query!(
            &db,
            r#"
            INSERT INTO avionics_suite_components (
              suite_model_id, component_model_id, quantity
            ) VALUES (?, ?, 2)
            "#,
            suite_id,
            component_id
        )
        .expect("suite membership should seed");

        let report = cleanup_orphan_records(&db)
            .await
            .expect("cleanup should succeed");

        assert_eq!(report.avionics_models, 0);
        assert_eq!(table_count(&db, "avionics_models").await, 2);
        assert_eq!(table_count(&db, "avionics_suite_components").await, 1);

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

    #[derive(Clone, Copy, Debug)]
    struct SeededAircraftGraph {
        aircraft_manufacturer_id: i64,
        aircraft_model_id: i64,
        aircraft_model_variant_id: i64,
    }

    async fn seed_unreferenced_aircraft_graph(db: &AppDb) -> SeededAircraftGraph {
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
        SeededAircraftGraph {
            aircraft_manufacturer_id,
            aircraft_model_id,
            aircraft_model_variant_id: variant_id,
        }
    }

    async fn seed_referenced_aircraft_graph(db: &AppDb) -> SeededAircraftGraph {
        let aircraft_manufacturer_id = insert_named(db, "aircraft_manufacturers", "Cessna").await;
        let aircraft_model_id = insert_aircraft_model(db, aircraft_manufacturer_id).await;
        let variant_id = insert_aircraft_variant(db, aircraft_model_id).await;
        let avionics_model_id = insert_avionics_model(db).await;
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        // Reproduce a retained listing row from before the compatibility
        // projection contract was installed. New application writes cannot
        // create this shape, but cleanup must never delete hierarchy rows
        // still referenced by a migrated legacy listing.
        execute_query!(
            db,
            "DROP TRIGGER listing_insert_requires_aircraft_projection_or_placeholder"
        )
        .expect("legacy listing fixture should bypass only the new-write guard");
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
        SeededAircraftGraph {
            aircraft_manufacturer_id,
            aircraft_model_id,
            aircraft_model_variant_id: variant_id,
        }
    }

    async fn insert_named(db: &AppDb, table: &str, name: &str) -> i64 {
        let sql = format!("INSERT INTO {table} (name, normalized_name) VALUES (?, ?) RETURNING id");
        query_scalar_one!(db, i64, &sql, name, name.to_ascii_lowercase())
            .expect("named row should seed")
    }

    async fn insert_aircraft_model(db: &AppDb, aircraft_manufacturer_id: i64) -> i64 {
        let model_id = query_scalar_one!(
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
        .expect("aircraft model should seed");
        model_id
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
        let model_id = query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id,
              name,
              normalized_name,
              manufacturer_identifier_kind,
              manufacturer_identifier,
              normalized_manufacturer_identifier,
              identity_source_url,
              identity_source_title,
              identity_evidence_text,
              identity_evidence_kind,
              identity_confidence,
              catalog_reviewed_at
            )
            VALUES (
              ?, 'G1000 NXi', 'g1000 nxi',
              'manufacturer_model_number', 'G1000 NXi', 'g1000nxi',
              'https://www.garmin.com/aviation/g1000-nxi/',
              'Garmin G1000 NXi',
              'Manufacturer reference identifies the G1000 NXi product.',
              'authoritative_reference', 'very_high', CURRENT_TIMESTAMP
            )
            RETURNING id
            "#,
            manufacturer_id
        )
        .expect("avionics model should seed");
        execute_query!(
            db,
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
            model_id,
            type_id
        )
        .expect("avionics capability should seed");
        ensure_test_manufacturer_identity(db, manufacturer_id)
            .await
            .expect("avionics manufacturer identity should seed");
        execute_query!(
            db,
            "UPDATE avionics_models SET catalog_status = 'approved' WHERE id = ?",
            model_id
        )
        .expect("avionics model should be approved after capability assignment");
        model_id
    }

    async fn insert_unreviewed_avionics_model(
        db: &AppDb,
        manufacturer_id: i64,
        type_id: i64,
        name: &str,
        normalized_name: &str,
    ) -> i64 {
        let model_id = query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id,
              name,
              normalized_name
            )
            VALUES (?, ?, ?)
            RETURNING id
            "#,
            manufacturer_id,
            name,
            normalized_name
        )
        .expect("unreviewed avionics model should seed");
        execute_query!(
            db,
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
            model_id,
            type_id
        )
        .expect("unreviewed avionics capability should seed");
        model_id
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

    async fn row_exists(db: &AppDb, table: &str, id: i64) -> bool {
        let sql = format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = ?)");
        query_scalar_one!(db, i64, &sql, id).expect("row existence should load") != 0
    }
}
