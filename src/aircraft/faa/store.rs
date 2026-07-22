use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Sqlite, SqlitePool};

use crate::db::{AppDb, DatabaseBackend};

use super::{Release, Snapshot};

const INSERT_BATCH_SIZE: usize = 500;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredSnapshot {
    pub snapshot: Snapshot,
    pub inserted: bool,
    pub target_count: usize,
    pub matched_count: usize,
}

/// Atomically stores one immutable, target-scoped FAA projection.
///
/// Re-importing the exact archive and target set is idempotent. Expanded target
/// sets create another immutable projection of the same release archive.
pub async fn store_release(db: &AppDb, release: &Release) -> Result<StoredSnapshot> {
    validate_release_projection(release)?;
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => store_sqlite(pool, release).await,
        DatabaseBackend::Postgres(pool) => store_postgres(pool, release).await,
    }
}

async fn store_sqlite(pool: &SqlitePool, release: &Release) -> Result<StoredSnapshot> {
    if let Some(existing) = find_sqlite(
        pool,
        &release.metadata.archive_sha256,
        &release.target_set_sha256,
    )
    .await?
    {
        return existing_snapshot(existing, release);
    }

    let mut transaction = pool.begin().await?;
    let evidence_source_id = evidence_source_sqlite(&mut transaction, release).await?;
    let snapshot_id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO faa_registry_snapshots (
          evidence_source_id, snapshot_date, source_url, archive_sha256, source_manifest_sha256,
          target_set_sha256, master_member_name, master_member_sha256,
          aircraft_member_name, aircraft_member_sha256,
          engine_member_name, engine_member_sha256
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (archive_sha256, target_set_sha256) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(evidence_source_id)
    .bind(&release.metadata.snapshot_date)
    .bind(&release.metadata.source_url)
    .bind(&release.metadata.archive_sha256)
    .bind(&release.source_manifest_sha256)
    .bind(&release.target_set_sha256)
    .bind(&release.master.member_name)
    .bind(&release.master.sha256)
    .bind(&release.aircraft_reference.member_name)
    .bind(&release.aircraft_reference.sha256)
    .bind(&release.engine_reference.member_name)
    .bind(&release.engine_reference.sha256)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(snapshot_id) = snapshot_id else {
        transaction.rollback().await?;
        let existing = find_sqlite(
            pool,
            &release.metadata.archive_sha256,
            &release.target_set_sha256,
        )
        .await?
        .context("concurrent FAA snapshot import disappeared")?;
        return existing_snapshot(existing, release);
    };

    insert_aircraft_sqlite(&mut transaction, snapshot_id, release).await?;
    insert_aircraft_references_sqlite(&mut transaction, snapshot_id, release).await?;
    insert_engine_references_sqlite(&mut transaction, snapshot_id, release).await?;
    insert_coverage_sqlite(&mut transaction, snapshot_id, release).await?;
    transaction.commit().await?;
    Ok(stored_snapshot(
        snapshot_id,
        evidence_source_id,
        release,
        true,
    ))
}

async fn store_postgres(pool: &PgPool, release: &Release) -> Result<StoredSnapshot> {
    if let Some(existing) = find_postgres(
        pool,
        &release.metadata.archive_sha256,
        &release.target_set_sha256,
    )
    .await?
    {
        return existing_snapshot(existing, release);
    }

    let mut transaction = pool.begin().await?;
    let evidence_source_id = evidence_source_postgres(&mut transaction, release).await?;
    let snapshot_id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO faa_registry_snapshots (
          evidence_source_id, snapshot_date, source_url, archive_sha256, source_manifest_sha256,
          target_set_sha256, master_member_name, master_member_sha256,
          aircraft_member_name, aircraft_member_sha256,
          engine_member_name, engine_member_sha256
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
        ON CONFLICT (archive_sha256, target_set_sha256) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(evidence_source_id)
    .bind(&release.metadata.snapshot_date)
    .bind(&release.metadata.source_url)
    .bind(&release.metadata.archive_sha256)
    .bind(&release.source_manifest_sha256)
    .bind(&release.target_set_sha256)
    .bind(&release.master.member_name)
    .bind(&release.master.sha256)
    .bind(&release.aircraft_reference.member_name)
    .bind(&release.aircraft_reference.sha256)
    .bind(&release.engine_reference.member_name)
    .bind(&release.engine_reference.sha256)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(snapshot_id) = snapshot_id else {
        transaction.rollback().await?;
        let existing = find_postgres(
            pool,
            &release.metadata.archive_sha256,
            &release.target_set_sha256,
        )
        .await?
        .context("concurrent FAA snapshot import disappeared")?;
        return existing_snapshot(existing, release);
    };

    insert_aircraft_postgres(&mut transaction, snapshot_id, release).await?;
    insert_aircraft_references_postgres(&mut transaction, snapshot_id, release).await?;
    insert_engine_references_postgres(&mut transaction, snapshot_id, release).await?;
    insert_coverage_postgres(&mut transaction, snapshot_id, release).await?;
    transaction.commit().await?;
    Ok(stored_snapshot(
        snapshot_id,
        evidence_source_id,
        release,
        true,
    ))
}

async fn insert_aircraft_references_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    snapshot_id: i64,
    release: &Release,
) -> Result<()> {
    for rows in release.aircraft_references.chunks(INSERT_BATCH_SIZE) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO faa_registry_aircraft_references (snapshot_id, aircraft_code, manufacturer_name, model_name, aircraft_type_code, engine_type_code, category_code, certification_indicator_code, engine_count, seat_count, weight_class_code, cruise_speed_mph, type_certificate_data_sheet, type_certificate_holder) ",
        );
        query.push_values(rows, |mut values, row| {
            values
                .push_bind(snapshot_id)
                .push_bind(&row.aircraft_code)
                .push_bind(&row.manufacturer_name)
                .push_bind(&row.model_name)
                .push_bind(&row.aircraft_type_code)
                .push_bind(&row.engine_type_code)
                .push_bind(&row.category_code)
                .push_bind(&row.certification_indicator_code)
                .push_bind(row.engine_count.map(i64::from))
                .push_bind(row.seat_count.map(i64::from))
                .push_bind(&row.weight_class_code)
                .push_bind(row.cruise_speed_mph.map(i64::from))
                .push_bind(&row.type_certificate_data_sheet)
                .push_bind(&row.type_certificate_holder);
        });
        query.build().execute(&mut **transaction).await?;
    }
    Ok(())
}

async fn insert_engine_references_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    snapshot_id: i64,
    release: &Release,
) -> Result<()> {
    for rows in release.engine_references.chunks(INSERT_BATCH_SIZE) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO faa_registry_engine_references (snapshot_id, engine_code, manufacturer_name, model_name, engine_type_code, horsepower, thrust_pounds) ",
        );
        query.push_values(rows, |mut values, row| {
            values
                .push_bind(snapshot_id)
                .push_bind(&row.engine_code)
                .push_bind(&row.manufacturer_name)
                .push_bind(&row.model_name)
                .push_bind(&row.engine_type_code)
                .push_bind(row.horsepower.map(i64::from))
                .push_bind(row.thrust_pounds.map(i64::from));
        });
        query.build().execute(&mut **transaction).await?;
    }
    Ok(())
}

async fn insert_aircraft_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    snapshot_id: i64,
    release: &Release,
) -> Result<()> {
    for rows in release.aircraft.chunks(INSERT_BATCH_SIZE) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO faa_registry_aircraft (snapshot_id, n_number, manufacturer_serial_raw, manufacturer_serial_key, aircraft_code, engine_code, year_manufactured, source_record_sha256) ",
        );
        query.push_values(rows, |mut values, row| {
            values
                .push_bind(snapshot_id)
                .push_bind(&row.n_number)
                .push_bind(&row.manufacturer_serial_raw)
                .push_bind(&row.manufacturer_serial_key)
                .push_bind(&row.aircraft_code)
                .push_bind(&row.engine_code)
                .push_bind(row.year_manufactured.map(i64::from))
                .push_bind(&row.source_record_sha256);
        });
        query.build().execute(&mut **transaction).await?;
    }
    Ok(())
}

async fn insert_coverage_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    snapshot_id: i64,
    release: &Release,
) -> Result<()> {
    for rows in release.coverage.chunks(INSERT_BATCH_SIZE) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status) ",
        );
        query.push_values(rows, |mut values, row| {
            values
                .push_bind(snapshot_id)
                .push_bind(&row.n_number)
                .push_bind(if row.matched { "matched" } else { "absent" });
        });
        query.build().execute(&mut **transaction).await?;
    }
    Ok(())
}

async fn insert_aircraft_references_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    snapshot_id: i64,
    release: &Release,
) -> Result<()> {
    for rows in release.aircraft_references.chunks(INSERT_BATCH_SIZE) {
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO faa_registry_aircraft_references (snapshot_id, aircraft_code, manufacturer_name, model_name, aircraft_type_code, engine_type_code, category_code, certification_indicator_code, engine_count, seat_count, weight_class_code, cruise_speed_mph, type_certificate_data_sheet, type_certificate_holder) ",
        );
        query.push_values(rows, |mut values, row| {
            values
                .push_bind(snapshot_id)
                .push_bind(&row.aircraft_code)
                .push_bind(&row.manufacturer_name)
                .push_bind(&row.model_name)
                .push_bind(&row.aircraft_type_code)
                .push_bind(&row.engine_type_code)
                .push_bind(&row.category_code)
                .push_bind(&row.certification_indicator_code)
                .push_bind(row.engine_count.map(i64::from))
                .push_bind(row.seat_count.map(i64::from))
                .push_bind(&row.weight_class_code)
                .push_bind(row.cruise_speed_mph.map(i64::from))
                .push_bind(&row.type_certificate_data_sheet)
                .push_bind(&row.type_certificate_holder);
        });
        query.build().execute(&mut **transaction).await?;
    }
    Ok(())
}

async fn insert_engine_references_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    snapshot_id: i64,
    release: &Release,
) -> Result<()> {
    for rows in release.engine_references.chunks(INSERT_BATCH_SIZE) {
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO faa_registry_engine_references (snapshot_id, engine_code, manufacturer_name, model_name, engine_type_code, horsepower, thrust_pounds) ",
        );
        query.push_values(rows, |mut values, row| {
            values
                .push_bind(snapshot_id)
                .push_bind(&row.engine_code)
                .push_bind(&row.manufacturer_name)
                .push_bind(&row.model_name)
                .push_bind(&row.engine_type_code)
                .push_bind(row.horsepower.map(i64::from))
                .push_bind(row.thrust_pounds.map(i64::from));
        });
        query.build().execute(&mut **transaction).await?;
    }
    Ok(())
}

async fn insert_aircraft_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    snapshot_id: i64,
    release: &Release,
) -> Result<()> {
    for rows in release.aircraft.chunks(INSERT_BATCH_SIZE) {
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO faa_registry_aircraft (snapshot_id, n_number, manufacturer_serial_raw, manufacturer_serial_key, aircraft_code, engine_code, year_manufactured, source_record_sha256) ",
        );
        query.push_values(rows, |mut values, row| {
            values
                .push_bind(snapshot_id)
                .push_bind(&row.n_number)
                .push_bind(&row.manufacturer_serial_raw)
                .push_bind(&row.manufacturer_serial_key)
                .push_bind(&row.aircraft_code)
                .push_bind(&row.engine_code)
                .push_bind(row.year_manufactured.map(i64::from))
                .push_bind(&row.source_record_sha256);
        });
        query.build().execute(&mut **transaction).await?;
    }
    Ok(())
}

async fn insert_coverage_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    snapshot_id: i64,
    release: &Release,
) -> Result<()> {
    for rows in release.coverage.chunks(INSERT_BATCH_SIZE) {
        let mut query = QueryBuilder::<Postgres>::new(
            "INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status) ",
        );
        query.push_values(rows, |mut values, row| {
            values
                .push_bind(snapshot_id)
                .push_bind(&row.n_number)
                .push_bind(if row.matched { "matched" } else { "absent" });
        });
        query.build().execute(&mut **transaction).await?;
    }
    Ok(())
}

async fn evidence_source_sqlite(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    release: &Release,
) -> Result<i64> {
    let title = format!(
        "FAA Releasable Aircraft Registry {}",
        release.metadata.snapshot_date
    );
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO curation_evidence_sources (
          source_url, resolved_url, source_title, publisher, source_domain,
          source_tier, content_sha256, retrieved_at
        ) VALUES (?, ?, ?, 'Federal Aviation Administration', 'faa.gov',
                  'regulator_primary', ?, CURRENT_TIMESTAMP)
        ON CONFLICT (source_url, content_sha256) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(&release.metadata.source_url)
    .bind(&release.metadata.source_url)
    .bind(title)
    .bind(&release.metadata.archive_sha256)
    .fetch_optional(&mut **transaction)
    .await?
    {
        return Ok(id);
    }
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM curation_evidence_sources WHERE source_url = ? AND content_sha256 = ?",
    )
    .bind(&release.metadata.source_url)
    .bind(&release.metadata.archive_sha256)
    .fetch_one(&mut **transaction)
    .await
    .map_err(Into::into)
}

async fn evidence_source_postgres(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    release: &Release,
) -> Result<i64> {
    let title = format!(
        "FAA Releasable Aircraft Registry {}",
        release.metadata.snapshot_date
    );
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO curation_evidence_sources (
          source_url, resolved_url, source_title, publisher, source_domain,
          source_tier, content_sha256, retrieved_at
        ) VALUES ($1, $2, $3, 'Federal Aviation Administration', 'faa.gov',
                  'regulator_primary', $4, CURRENT_TIMESTAMP)
        ON CONFLICT (source_url, content_sha256) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(&release.metadata.source_url)
    .bind(&release.metadata.source_url)
    .bind(title)
    .bind(&release.metadata.archive_sha256)
    .fetch_optional(&mut **transaction)
    .await?
    {
        return Ok(id);
    }
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM curation_evidence_sources WHERE source_url = $1 AND content_sha256 = $2",
    )
    .bind(&release.metadata.source_url)
    .bind(&release.metadata.archive_sha256)
    .fetch_one(&mut **transaction)
    .await
    .map_err(Into::into)
}

async fn find_sqlite(
    pool: &SqlitePool,
    archive_sha256: &str,
    target_set_sha256: &str,
) -> Result<Option<StoredRow>> {
    Ok(sqlx::query_as::<_, StoredRow>(
        r#"
        SELECT id, evidence_source_id, snapshot_date, source_url, archive_sha256, source_manifest_sha256,
               target_set_sha256,
               (SELECT count(*) FROM faa_registry_coverage coverage
                WHERE coverage.snapshot_id = snapshot.id) AS target_count,
               (SELECT count(*) FROM faa_registry_aircraft aircraft
                WHERE aircraft.snapshot_id = snapshot.id) AS matched_count
        FROM faa_registry_snapshots snapshot
        WHERE archive_sha256 = ? AND target_set_sha256 = ?
        "#,
    )
    .bind(archive_sha256)
    .bind(target_set_sha256)
    .fetch_optional(pool)
    .await?)
}

async fn find_postgres(
    pool: &PgPool,
    archive_sha256: &str,
    target_set_sha256: &str,
) -> Result<Option<StoredRow>> {
    Ok(sqlx::query_as::<_, StoredRow>(
        r#"
        SELECT id, evidence_source_id, snapshot_date, source_url, archive_sha256, source_manifest_sha256,
               target_set_sha256,
               (SELECT count(*) FROM faa_registry_coverage coverage
                WHERE coverage.snapshot_id = snapshot.id) AS target_count,
               (SELECT count(*) FROM faa_registry_aircraft aircraft
                WHERE aircraft.snapshot_id = snapshot.id) AS matched_count
        FROM faa_registry_snapshots snapshot
        WHERE archive_sha256 = $1 AND target_set_sha256 = $2
        "#,
    )
    .bind(archive_sha256)
    .bind(target_set_sha256)
    .fetch_optional(pool)
    .await?)
}

fn existing_snapshot(row: StoredRow, release: &Release) -> Result<StoredSnapshot> {
    if row.source_manifest_sha256 != release.source_manifest_sha256 {
        bail!("FAA archive projection was already imported with different provenance");
    }
    Ok(StoredSnapshot {
        snapshot: row.snapshot(),
        inserted: false,
        target_count: usize::try_from(row.target_count)?,
        matched_count: usize::try_from(row.matched_count)?,
    })
}

fn stored_snapshot(
    snapshot_id: i64,
    evidence_source_id: i64,
    release: &Release,
    inserted: bool,
) -> StoredSnapshot {
    StoredSnapshot {
        snapshot: Snapshot {
            id: snapshot_id,
            evidence_source_id,
            snapshot_date: release.metadata.snapshot_date.clone(),
            source_url: release.metadata.source_url.clone(),
            archive_sha256: release.metadata.archive_sha256.clone(),
            source_manifest_sha256: release.source_manifest_sha256.clone(),
            target_set_sha256: release.target_set_sha256.clone(),
        },
        inserted,
        target_count: release.coverage.len(),
        matched_count: release.aircraft.len(),
    }
}

fn validate_release_projection(release: &Release) -> Result<()> {
    if release.coverage.is_empty() {
        bail!("FAA release projection has no coverage targets");
    }
    let covered = release
        .coverage
        .iter()
        .map(|coverage| coverage.n_number.as_str())
        .collect::<BTreeSet<_>>();
    if covered.len() != release.coverage.len() {
        bail!("FAA release projection contains duplicate coverage targets");
    }
    let matched = release
        .aircraft
        .iter()
        .map(|aircraft| aircraft.n_number.as_str())
        .collect::<BTreeSet<_>>();
    if matched.len() != release.aircraft.len() {
        bail!("FAA release projection contains duplicate matched N-numbers");
    }
    let declared_matched = release
        .coverage
        .iter()
        .filter(|coverage| coverage.matched)
        .map(|coverage| coverage.n_number.as_str())
        .collect::<BTreeSet<_>>();
    if matched != declared_matched {
        bail!("FAA matched rows do not agree with explicit coverage");
    }
    let reachable_aircraft = release
        .aircraft
        .iter()
        .map(|aircraft| aircraft.aircraft_code.as_str())
        .collect::<BTreeSet<_>>();
    if release
        .aircraft_references
        .iter()
        .any(|reference| !reachable_aircraft.contains(reference.aircraft_code.as_str()))
    {
        bail!("FAA projection contains an unreachable aircraft reference");
    }
    let reachable_engines = release
        .aircraft
        .iter()
        .filter_map(|aircraft| aircraft.engine_code.as_deref())
        .collect::<BTreeSet<_>>();
    if release
        .engine_references
        .iter()
        .any(|reference| !reachable_engines.contains(reference.engine_code.as_str()))
    {
        bail!("FAA projection contains an unreachable engine reference");
    }
    Ok(())
}

#[derive(Debug, FromRow)]
struct StoredRow {
    id: i64,
    evidence_source_id: i64,
    snapshot_date: String,
    source_url: String,
    archive_sha256: String,
    source_manifest_sha256: String,
    target_set_sha256: String,
    target_count: i64,
    matched_count: i64,
}

impl StoredRow {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            id: self.id,
            evidence_source_id: self.evidence_source_id,
            snapshot_date: self.snapshot_date.clone(),
            source_url: self.source_url.clone(),
            archive_sha256: self.archive_sha256.clone(),
            source_manifest_sha256: self.source_manifest_sha256.clone(),
            target_set_sha256: self.target_set_sha256.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::aircraft::faa::{
        lookup_current, parse_release, LookupOutcome, ReleaseMetadata, ReleaseReaders,
    };

    use super::*;

    const MASTER: &str = "N-NUMBER,SERIAL NUMBER,MFR MDL CODE,ENG MFR MDL,YEAR MFR,NAME,STREET,MODE S CODE\n123AB,182-01234,2072738,41528,2006,PRIVATE OWNER,SECRET ADDRESS,50000000\n456,UNRELATED,9999999,99999,1999,OTHER OWNER,OTHER ADDRESS,50000001\n";
    const AIRCRAFT: &str = "CODE,MFR,MODEL,TYPE-ACFT,TYPE-ENG,AC-CAT,BUILD-CERT-IND,NO-ENG,NO-SEATS,AC-WEIGHT,SPEED,TC-DATA-SHEET,TC-DATA-HOLDER\n2072738,CESSNA AIRCRAFT CO,182T,4,1,1,0,01,004,CLASS 1,0145,3A13,TEXTRON AVIATION INC\n9999999,UNRELATED,MODEL,4,1,1,0,01,004,CLASS 1,0100,,\n";
    const ENGINE: &str = "CODE,MFR,MODEL,TYPE,HORSEPOWER,THRUST\n41528,LYCOMING,IO-540-AB1A5,1,00230,000000\n99999,UNRELATED,ENGINE,1,00100,000000\n";

    fn release(targets: &[&str]) -> Release {
        parse_release(
            ReleaseMetadata::official("2026-07-20", "a".repeat(64)),
            ReleaseReaders::new(
                Cursor::new(MASTER),
                Cursor::new(AIRCRAFT),
                Cursor::new(ENGINE),
            ),
            targets,
        )
        .unwrap()
    }

    async fn temporary_db() -> (AppDb, std::path::PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aircost-faa-{}-{nonce}.sqlite3",
            std::process::id()
        ));
        let db = AppDb::connect(&format!("sqlite://{}", path.display()))
            .await
            .unwrap();
        (db, path)
    }

    #[tokio::test]
    async fn stores_target_projection_idempotently_and_supports_expansion() {
        let (db, path) = temporary_db().await;
        let first_release = release(&["N123AB"]);
        let first = store_release(&db, &first_release).await.unwrap();
        assert!(first.inserted);
        assert_eq!(first.target_count, 1);
        assert_eq!(first.matched_count, 1);

        let repeated = store_release(&db, &first_release).await.unwrap();
        assert!(!repeated.inserted);
        assert_eq!(repeated.snapshot.id, first.snapshot.id);

        let expanded_release = release(&["N123AB", "N999ZZ"]);
        let expanded = store_release(&db, &expanded_release).await.unwrap();
        assert!(expanded.inserted);
        assert_ne!(expanded.snapshot.id, first.snapshot.id);
        assert_eq!(expanded.target_count, 2);
        assert_eq!(expanded.matched_count, 1);

        let found = lookup_current(&db, Some("n-123ab"), Some("18201234"))
            .await
            .unwrap();
        match found {
            LookupOutcome::Found { grounding } => {
                assert_eq!(grounding.snapshot.id, expanded.snapshot.id);
                assert_eq!(
                    grounding.aircraft.unwrap().model_name.as_deref(),
                    Some("182T")
                );
                assert_eq!(
                    grounding.engine.unwrap().model_name.as_deref(),
                    Some("IO-540-AB1A5")
                );
                assert_eq!(grounding.year_manufactured, Some(2006));
                assert_eq!(grounding.source_record_sha256.len(), 64);
            }
            other => panic!("expected FAA match, got {other:?}"),
        }
        assert!(matches!(
            lookup_current(&db, Some("N999ZZ"), None).await.unwrap(),
            LookupOutcome::NotFound { .. }
        ));
        assert!(matches!(
            lookup_current(&db, Some("N888ZZ"), None).await.unwrap(),
            LookupOutcome::NotCovered { .. }
        ));

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let registry_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM faa_registry_aircraft")
            .fetch_one(pool)
            .await
            .unwrap();
        let evidence_rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM curation_evidence_sources WHERE source_domain = 'faa.gov'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(registry_rows, 2, "only one matched target per projection");
        assert_eq!(evidence_rows, 1, "same archive reuses exact FAA evidence");
        assert!(
            sqlx::query("UPDATE faa_registry_aircraft SET aircraft_code = 'x'")
                .execute(pool)
                .await
                .is_err()
        );

        drop(db);
        let _ = std::fs::remove_file(path);
    }
}
