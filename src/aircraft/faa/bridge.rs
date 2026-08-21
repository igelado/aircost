//! One-purpose FAA translation for the frozen replay-source bridge.
//!
//! The bridge never copies or mechanically rehashes a legacy FAA projection.
//! It parses the operator-supplied archive through the current parser, proves
//! that its selected non-PII facts equal the legacy projection, and stores only
//! that parser-owned current-domain release in the fresh target.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sqlx::{FromRow, SqliteConnection};

use crate::db::AppDb;

use super::{
    parse_release_archive, store_release, AircraftRecord, AircraftReference, EngineReference,
    Release,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct FaaBridgeReport {
    pub archive_sha256: String,
    pub snapshot_date: String,
    pub target_count: usize,
    pub matched_count: usize,
    pub stored_snapshot_id: i64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct LegacyFaaRepresentative {
    pub(crate) snapshot_id: i64,
    pub(crate) n_number: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CurrentFaaRepresentative {
    pub(crate) snapshot_id: i64,
    pub(crate) n_number: String,
    pub(crate) source_record_sha256: String,
}

pub(crate) struct FaaBridgeOutcome {
    pub(crate) report: FaaBridgeReport,
    pub(crate) representative_remap: BTreeMap<LegacyFaaRepresentative, CurrentFaaRepresentative>,
    pub(crate) obsolete_hashes: BTreeSet<String>,
}

#[derive(FromRow)]
struct LegacySnapshotRow {
    id: i64,
    source_url: String,
    master_member_name: String,
    master_member_sha256: String,
    aircraft_member_name: String,
    aircraft_member_sha256: String,
    engine_member_name: String,
    engine_member_sha256: String,
    lookup_status: String,
}

#[derive(FromRow)]
struct LegacyAircraftRow {
    n_number: String,
    manufacturer_serial_raw: Option<String>,
    manufacturer_serial_key: Option<String>,
    aircraft_code: String,
    engine_code: Option<String>,
    year_manufactured: Option<i64>,
}

#[derive(FromRow)]
struct LegacyAircraftReferenceRow {
    aircraft_code: String,
    manufacturer_name: Option<String>,
    model_name: Option<String>,
    aircraft_type_code: Option<String>,
    engine_type_code: Option<String>,
    category_code: Option<String>,
    certification_indicator_code: Option<String>,
    engine_count: Option<i64>,
    seat_count: Option<i64>,
    weight_class_code: Option<String>,
    cruise_speed_mph: Option<i64>,
    type_certificate_data_sheet: Option<String>,
    type_certificate_holder: Option<String>,
}

#[derive(FromRow)]
struct LegacyEngineReferenceRow {
    engine_code: String,
    manufacturer_name: Option<String>,
    model_name: Option<String>,
    engine_type_code: Option<String>,
    horsepower: Option<i64>,
    thrust_pounds: Option<i64>,
}

pub(crate) async fn rebuild_faa_projection(
    legacy_source: &mut SqliteConnection,
    target: &AppDb,
    archive: &Path,
    expected_archive_sha256: &str,
    capture_n_numbers: &[String],
    representatives: &[LegacyFaaRepresentative],
) -> Result<FaaBridgeOutcome> {
    if expected_archive_sha256.len() != 64
        || expected_archive_sha256 != expected_archive_sha256.to_ascii_lowercase()
        || !expected_archive_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("--faa-archive-sha256 must be one lowercase SHA-256 digest");
    }
    let n_numbers = capture_n_numbers
        .iter()
        .cloned()
        .chain(representatives.iter().map(|row| row.n_number.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if n_numbers.is_empty() {
        bail!("legacy FAA bridge requires at least one explicit N-number target");
    }
    let archive_path = archive.to_path_buf();
    let targets = n_numbers;
    let release = tokio::task::spawn_blocking(move || -> Result<Release> {
        let file = File::open(&archive_path).with_context(|| {
            format!(
                "could not open historical FAA archive {}",
                archive_path.display()
            )
        })?;
        parse_release_archive(file, &targets)
    })
    .await
    .context("historical FAA archive parser task failed")??;
    let summary = release.summary();
    if summary.archive_sha256 != expected_archive_sha256 {
        bail!(
            "historical FAA archive SHA-256 mismatch: expected {expected_archive_sha256}, parsed {}",
            summary.archive_sha256
        );
    }
    let obsolete_hashes =
        compare_legacy_projection(legacy_source, &release, capture_n_numbers, representatives)
            .await?;
    let stored = store_release(target, &release).await?;
    let current = release
        .aircraft
        .iter()
        .map(|row| (row.n_number.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let representative_remap = representatives
        .iter()
        .map(|legacy| {
            let record = current.get(legacy.n_number.as_str()).with_context(|| {
                format!(
                    "representative {} is absent from the supplied FAA archive",
                    legacy.n_number
                )
            })?;
            Ok((
                legacy.clone(),
                CurrentFaaRepresentative {
                    snapshot_id: stored.snapshot.id,
                    n_number: record.n_number.clone(),
                    source_record_sha256: record.source_record_sha256.clone(),
                },
            ))
        })
        .collect::<Result<_>>()?;
    Ok(FaaBridgeOutcome {
        report: FaaBridgeReport {
            archive_sha256: summary.archive_sha256.to_string(),
            snapshot_date: summary.snapshot_date.to_string(),
            target_count: summary.target_count,
            matched_count: summary.matched_count,
            stored_snapshot_id: stored.snapshot.id,
        },
        representative_remap,
        obsolete_hashes,
    })
}

async fn compare_legacy_projection(
    source: &mut SqliteConnection,
    release: &Release,
    capture_n_numbers: &[String],
    representatives: &[LegacyFaaRepresentative],
) -> Result<BTreeSet<String>> {
    let mut selected = BTreeSet::new();
    for n_number in capture_n_numbers {
        let snapshot_id: i64 = sqlx::query_scalar(
            r#"SELECT snapshot.id
               FROM faa_registry_snapshots snapshot
               JOIN faa_registry_coverage coverage
                 ON coverage.snapshot_id = snapshot.id AND coverage.n_number = ?
               WHERE snapshot.snapshot_date = ? AND snapshot.archive_sha256 = ?
               ORDER BY snapshot.id DESC LIMIT 1"#,
        )
        .bind(n_number)
        .bind(&release.metadata.snapshot_date)
        .bind(&release.metadata.archive_sha256)
        .fetch_optional(&mut *source)
        .await?
        .with_context(|| {
            format!(
                "legacy FAA projection does not cover {n_number} in the supplied historical archive"
            )
        })?;
        selected.insert((snapshot_id, n_number.clone()));
    }
    selected.extend(
        representatives
            .iter()
            .map(|row| (row.snapshot_id, row.n_number.clone())),
    );

    let mut obsolete_hashes = BTreeSet::new();
    for (snapshot_id, n_number) in selected {
        let snapshot = sqlx::query_as::<_, LegacySnapshotRow>(
            r#"SELECT snapshot.id, snapshot.source_url,
                      snapshot.master_member_name, snapshot.master_member_sha256,
                      snapshot.aircraft_member_name, snapshot.aircraft_member_sha256,
                      snapshot.engine_member_name, snapshot.engine_member_sha256,
                      coverage.lookup_status
               FROM faa_registry_snapshots snapshot
               JOIN faa_registry_coverage coverage
                 ON coverage.snapshot_id = snapshot.id
                AND coverage.n_number = ?
               WHERE snapshot.id = ? AND snapshot.snapshot_date = ?
                 AND snapshot.archive_sha256 = ?"#,
        )
        .bind(&n_number)
        .bind(snapshot_id)
        .bind(&release.metadata.snapshot_date)
        .bind(&release.metadata.archive_sha256)
        .fetch_optional(&mut *source)
        .await?
        .with_context(|| {
            format!("legacy FAA snapshot {snapshot_id} does not cover representative {n_number}")
        })?;
        require_snapshot_provenance(&snapshot, release)?;
        let source_manifest_sha256: String = sqlx::query_scalar(
            "SELECT source_manifest_sha256 FROM faa_registry_snapshots WHERE id = ?",
        )
        .bind(snapshot.id)
        .fetch_one(&mut *source)
        .await?;
        obsolete_hashes.insert(source_manifest_sha256);
        let coverage = release
            .coverage
            .iter()
            .find(|row| row.n_number == n_number)
            .context("current FAA parser omitted a requested coverage row")?;
        let expected_status = if coverage.matched {
            "matched"
        } else {
            "absent"
        };
        if snapshot.lookup_status != expected_status {
            bail!(
                "legacy FAA coverage for {} disagrees with the supplied archive",
                n_number
            );
        }
        if !coverage.matched {
            continue;
        }
        let current = release
            .aircraft
            .iter()
            .find(|row| row.n_number == coverage.n_number)
            .context("current FAA parser omitted one matched coverage record")?;
        let legacy = sqlx::query_as::<_, LegacyAircraftRow>(
            r#"SELECT n_number, manufacturer_serial_raw, manufacturer_serial_key,
                      aircraft_code, engine_code, year_manufactured
               FROM faa_registry_aircraft
               WHERE snapshot_id = ? AND n_number = ?"#,
        )
        .bind(snapshot.id)
        .bind(&n_number)
        .fetch_optional(&mut *source)
        .await?
        .context("legacy matched FAA coverage has no retained MASTER projection")?;
        compare_aircraft(&legacy, current)?;
        let old_record_hash: String = sqlx::query_scalar(
            r#"SELECT source_record_sha256 FROM faa_registry_aircraft
               WHERE snapshot_id = ? AND n_number = ?"#,
        )
        .bind(snapshot.id)
        .bind(&n_number)
        .fetch_one(&mut *source)
        .await?;
        obsolete_hashes.insert(old_record_hash);
        compare_aircraft_reference(source, snapshot.id, release, current).await?;
        if current.engine_code.is_some() {
            compare_engine_reference(source, snapshot.id, release, current).await?;
        }
    }
    Ok(obsolete_hashes)
}

fn require_snapshot_provenance(snapshot: &LegacySnapshotRow, release: &Release) -> Result<()> {
    if snapshot.source_url != release.metadata.source_url
        || snapshot.master_member_name != release.master.member_name
        || snapshot.master_member_sha256 != release.master.sha256
        || snapshot.aircraft_member_name != release.aircraft_reference.member_name
        || snapshot.aircraft_member_sha256 != release.aircraft_reference.sha256
        || snapshot.engine_member_name != release.engine_reference.member_name
        || snapshot.engine_member_sha256 != release.engine_reference.sha256
    {
        bail!("legacy FAA snapshot provenance disagrees with the supplied archive");
    }
    Ok(())
}

fn compare_aircraft(legacy: &LegacyAircraftRow, current: &AircraftRecord) -> Result<()> {
    if legacy.n_number != current.n_number
        || legacy.manufacturer_serial_raw != current.manufacturer_serial_raw
        || legacy.manufacturer_serial_key != current.manufacturer_serial_key
        || legacy.aircraft_code != current.aircraft_code
        || legacy.engine_code != current.engine_code
        || optional_u16(legacy.year_manufactured, "year_manufactured")? != current.year_manufactured
    {
        bail!(
            "legacy FAA MASTER projection for {} disagrees with the supplied archive",
            current.n_number
        );
    }
    Ok(())
}

async fn compare_aircraft_reference(
    source: &mut SqliteConnection,
    snapshot_id: i64,
    release: &Release,
    aircraft: &AircraftRecord,
) -> Result<()> {
    let current = release
        .aircraft_references
        .iter()
        .find(|row| row.aircraft_code == aircraft.aircraft_code)
        .context("current FAA release omitted a reachable ACFTREF projection")?;
    let legacy = sqlx::query_as::<_, LegacyAircraftReferenceRow>(
        r#"SELECT aircraft_code, manufacturer_name, model_name,
                  aircraft_type_code, engine_type_code, category_code,
                  certification_indicator_code, engine_count, seat_count,
                  weight_class_code, cruise_speed_mph,
                  type_certificate_data_sheet, type_certificate_holder
           FROM faa_registry_aircraft_references
           WHERE snapshot_id = ? AND aircraft_code = ?"#,
    )
    .bind(snapshot_id)
    .bind(&aircraft.aircraft_code)
    .fetch_optional(&mut *source)
    .await?
    .context("legacy FAA projection omitted a reachable ACFTREF row")?;
    if legacy_aircraft_reference(&legacy)? != *current {
        bail!(
            "legacy FAA ACFTREF projection {} disagrees with the supplied archive",
            aircraft.aircraft_code
        );
    }
    Ok(())
}

fn legacy_aircraft_reference(row: &LegacyAircraftReferenceRow) -> Result<AircraftReference> {
    Ok(AircraftReference {
        aircraft_code: row.aircraft_code.clone(),
        manufacturer_name: row.manufacturer_name.clone(),
        model_name: row.model_name.clone(),
        aircraft_type_code: row.aircraft_type_code.clone(),
        engine_type_code: row.engine_type_code.clone(),
        category_code: row.category_code.clone(),
        certification_indicator_code: row.certification_indicator_code.clone(),
        engine_count: optional_u16(row.engine_count, "engine_count")?,
        seat_count: optional_u16(row.seat_count, "seat_count")?,
        weight_class_code: row.weight_class_code.clone(),
        cruise_speed_mph: optional_u16(row.cruise_speed_mph, "cruise_speed_mph")?,
        type_certificate_data_sheet: row.type_certificate_data_sheet.clone(),
        type_certificate_holder: row.type_certificate_holder.clone(),
    })
}

async fn compare_engine_reference(
    source: &mut SqliteConnection,
    snapshot_id: i64,
    release: &Release,
    aircraft: &AircraftRecord,
) -> Result<()> {
    let engine_code = aircraft
        .engine_code
        .as_deref()
        .context("engine code missing")?;
    let current = release
        .engine_references
        .iter()
        .find(|row| row.engine_code == engine_code)
        .context("current FAA release omitted a reachable ENGINE projection")?;
    let legacy = sqlx::query_as::<_, LegacyEngineReferenceRow>(
        r#"SELECT engine_code, manufacturer_name, model_name, engine_type_code,
                  horsepower, thrust_pounds
           FROM faa_registry_engine_references
           WHERE snapshot_id = ? AND engine_code = ?"#,
    )
    .bind(snapshot_id)
    .bind(engine_code)
    .fetch_optional(&mut *source)
    .await?
    .context("legacy FAA projection omitted a reachable ENGINE row")?;
    if legacy_engine_reference(&legacy)? != *current {
        bail!("legacy FAA ENGINE projection {engine_code} disagrees with the supplied archive");
    }
    Ok(())
}

fn legacy_engine_reference(row: &LegacyEngineReferenceRow) -> Result<EngineReference> {
    Ok(EngineReference {
        engine_code: row.engine_code.clone(),
        manufacturer_name: row.manufacturer_name.clone(),
        model_name: row.model_name.clone(),
        engine_type_code: row.engine_type_code.clone(),
        horsepower: optional_u32(row.horsepower, "horsepower")?,
        thrust_pounds: optional_u32(row.thrust_pounds, "thrust_pounds")?,
    })
}

fn optional_u16(value: Option<i64>, field: &str) -> Result<Option<u16>> {
    value
        .map(|value| u16::try_from(value).with_context(|| format!("legacy FAA {field} overflow")))
        .transpose()
}

fn optional_u32(value: Option<i64>, field: &str) -> Result<Option<u32>> {
    value
        .map(|value| u32::try_from(value).with_context(|| format!("legacy FAA {field} overflow")))
        .transpose()
}
