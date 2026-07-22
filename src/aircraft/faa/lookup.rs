use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};

use super::{AircraftGrounding, AircraftReference, EngineReference, Snapshot};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialMatch {
    /// The listing supplied no serial, so FAA remains the source of truth.
    NotProvided,
    /// FAA permissibly left the manufacturer serial blank.
    RegistryUnavailable,
    /// Trimmed source strings are exactly equal.
    RawExact,
    /// Only the conservative alphanumeric comparison keys are equal.
    NormalizedOnly,
    /// Both sources supplied serials and their comparison keys differ.
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotApplicableReason {
    MissingRegistration,
    ForeignRegistration,
    InvalidNNumber,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LookupOutcome {
    NotApplicable {
        supplied_registration: Option<String>,
        reason: NotApplicableReason,
    },
    NoSnapshot,
    NotFound {
        snapshot: Snapshot,
        n_number: String,
    },
    NotCovered {
        snapshot: Snapshot,
        n_number: String,
    },
    /// Defensive result for corrupted or externally modified databases. The
    /// managed schema enforces one N-number per snapshot.
    Ambiguous {
        snapshot: Snapshot,
        n_number: String,
        match_count: usize,
    },
    Found {
        grounding: AircraftGrounding,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    MissingRegistration,
    NonNRegistration,
    InvalidNNumber,
    RegistrySnapshotUnavailable,
    RegistrationNotFound,
    RegistrationNotCovered,
    AmbiguousRegistration,
    SerialConflict,
}

/// Shared strict admission result. Only an exact current FAA N-number match
/// without a serial conflict may cross a listing-backed trust boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "eligibility", rename_all = "snake_case")]
pub enum Eligibility {
    Eligible {
        grounding: AircraftGrounding,
    },
    Blocked {
        reason: BlockReason,
        n_number: Option<String>,
        snapshot_id: Option<i64>,
    },
}

/// Normalizes a U.S. registration conservatively and validates the FAA N-number
/// shape. Whitespace and hyphens are accepted presentation separators; other
/// punctuation is rejected rather than mechanically erased.
pub fn normalize_n_number(value: &str) -> Option<String> {
    let mut compact = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            compact.push(character.to_ascii_uppercase());
        } else if character.is_ascii_whitespace() || character == '-' {
            continue;
        } else {
            return None;
        }
    }

    let suffix = compact.strip_prefix('N')?;
    if suffix.is_empty() || suffix.len() > 5 {
        return None;
    }
    if !suffix
        .as_bytes()
        .first()
        .is_some_and(|byte| (b'1'..=b'9').contains(byte))
    {
        return None;
    }
    let mut letters = 0usize;
    let mut encountered_letter = false;
    for (index, character) in suffix.chars().enumerate() {
        if character.is_ascii_digit() {
            if encountered_letter || (index == 0 && character == '0') {
                return None;
            }
        } else if character.is_ascii_uppercase() && character != 'I' && character != 'O' {
            encountered_letter = true;
            letters += 1;
            if letters > 2 {
                return None;
            }
        } else {
            return None;
        }
    }
    Some(compact)
}

/// Serial comparison deliberately does not invent manufacturer-specific serial
/// semantics. It only removes ASCII punctuation/spacing and folds case.
pub fn normalize_serial_key(value: &str) -> Option<String> {
    let key: String = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_uppercase())
        .collect();
    (!key.is_empty()).then_some(key)
}

pub async fn lookup_current(
    db: &AppDb,
    registration: Option<&str>,
    observed_serial: Option<&str>,
) -> Result<LookupOutcome> {
    let registration = match classify_registration(registration) {
        Ok(registration) => registration,
        Err(outcome) => return Ok(outcome),
    };
    let Some(latest_release) = latest_snapshot(db).await? else {
        return Ok(LookupOutcome::NoSnapshot);
    };
    let Some(snapshot) = covering_projection(db, &latest_release, &registration).await? else {
        return Ok(LookupOutcome::NotCovered {
            snapshot: latest_release,
            n_number: registration,
        });
    };
    let coverage = coverage_status(db, snapshot.id, &registration).await?;
    if coverage.as_deref() == Some("absent") {
        return Ok(LookupOutcome::NotFound {
            snapshot,
            n_number: registration,
        });
    }
    if coverage.as_deref() != Some("matched") {
        // The covering query and immutable schema make this unreachable unless
        // the database was modified outside the application.
        return Ok(LookupOutcome::Ambiguous {
            snapshot,
            n_number: registration,
            match_count: 0,
        });
    }

    let sql = db.sql(
        r#"
        SELECT
          registry.n_number,
          registry.manufacturer_serial_raw,
          registry.manufacturer_serial_key,
          registry.aircraft_code,
          registry.engine_code,
          registry.year_manufactured,
          registry.source_record_sha256,
          aircraft.aircraft_code AS reference_aircraft_code,
          aircraft.manufacturer_name AS aircraft_manufacturer_name,
          aircraft.model_name AS aircraft_model_name,
          aircraft.aircraft_type_code,
          aircraft.engine_type_code AS aircraft_engine_type_code,
          aircraft.category_code,
          aircraft.certification_indicator_code,
          aircraft.engine_count,
          aircraft.seat_count,
          aircraft.weight_class_code,
          aircraft.cruise_speed_mph,
          aircraft.type_certificate_data_sheet,
          aircraft.type_certificate_holder,
          engine.engine_code AS reference_engine_code,
          engine.manufacturer_name AS engine_manufacturer_name,
          engine.model_name AS engine_model_name,
          engine.engine_type_code,
          engine.horsepower,
          engine.thrust_pounds
        FROM faa_registry_aircraft registry
        LEFT JOIN faa_registry_aircraft_references aircraft
          ON aircraft.snapshot_id = registry.snapshot_id
         AND aircraft.aircraft_code = registry.aircraft_code
        LEFT JOIN faa_registry_engine_references engine
          ON engine.snapshot_id = registry.snapshot_id
         AND engine.engine_code = registry.engine_code
        WHERE registry.snapshot_id = ? AND registry.n_number = ?
        LIMIT 2
        "#,
    );
    let matches = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, GroundingRow>(&sql)
                .bind(snapshot.id)
                .bind(&registration)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, GroundingRow>(&sql)
                .bind(snapshot.id)
                .bind(&registration)
                .fetch_all(pool)
                .await?
        }
    };

    if matches.is_empty() {
        return Ok(LookupOutcome::NotFound {
            snapshot,
            n_number: registration,
        });
    }
    if matches.len() > 1 {
        return Ok(LookupOutcome::Ambiguous {
            snapshot,
            n_number: registration,
            match_count: matches.len(),
        });
    }

    let row = matches.into_iter().next().expect("one checked FAA match");
    let serial_match = compare_serials(observed_serial, row.manufacturer_serial_raw.as_deref());
    let aircraft = match row.reference_aircraft_code {
        Some(aircraft_code) => Some(AircraftReference {
            aircraft_code,
            manufacturer_name: row.aircraft_manufacturer_name,
            model_name: row.aircraft_model_name,
            aircraft_type_code: row.aircraft_type_code,
            engine_type_code: row.aircraft_engine_type_code,
            category_code: row.category_code,
            certification_indicator_code: row.certification_indicator_code,
            engine_count: checked_u16(row.engine_count, "FAA engine count")?,
            seat_count: checked_u16(row.seat_count, "FAA seat count")?,
            weight_class_code: row.weight_class_code,
            cruise_speed_mph: checked_u16(row.cruise_speed_mph, "FAA cruise speed")?,
            type_certificate_data_sheet: row.type_certificate_data_sheet,
            type_certificate_holder: row.type_certificate_holder,
        }),
        None => None,
    };
    let engine = match row.reference_engine_code {
        Some(engine_code) => Some(EngineReference {
            engine_code,
            manufacturer_name: row.engine_manufacturer_name,
            model_name: row.engine_model_name,
            engine_type_code: row.engine_type_code,
            horsepower: checked_u32(row.horsepower, "FAA horsepower")?,
            thrust_pounds: checked_u32(row.thrust_pounds, "FAA thrust")?,
        }),
        None => None,
    };
    let year_manufactured = checked_u16(row.year_manufactured, "FAA year manufactured")?;

    Ok(LookupOutcome::Found {
        grounding: AircraftGrounding {
            snapshot,
            n_number: row.n_number,
            manufacturer_serial_raw: row.manufacturer_serial_raw,
            manufacturer_serial_key: row.manufacturer_serial_key,
            aircraft_code: row.aircraft_code,
            engine_code: row.engine_code,
            source_record_sha256: row.source_record_sha256,
            year_manufactured,
            aircraft,
            engine,
            serial_match,
        },
    })
}

/// Converts a lookup into the mandatory FAA admission decision. The decision
/// itself is pure; callers reject mutations or exclude retained rows.
pub fn require_eligible(outcome: LookupOutcome) -> Eligibility {
    match outcome {
        LookupOutcome::Found { grounding } if grounding.serial_match != SerialMatch::Conflict => {
            Eligibility::Eligible { grounding }
        }
        LookupOutcome::Found { grounding } => Eligibility::Blocked {
            reason: BlockReason::SerialConflict,
            n_number: Some(grounding.n_number),
            snapshot_id: Some(grounding.snapshot.id),
        },
        LookupOutcome::NotApplicable {
            supplied_registration,
            reason,
        } => Eligibility::Blocked {
            reason: match reason {
                NotApplicableReason::MissingRegistration => BlockReason::MissingRegistration,
                NotApplicableReason::ForeignRegistration => BlockReason::NonNRegistration,
                NotApplicableReason::InvalidNNumber => BlockReason::InvalidNNumber,
            },
            n_number: supplied_registration,
            snapshot_id: None,
        },
        LookupOutcome::NoSnapshot => Eligibility::Blocked {
            reason: BlockReason::RegistrySnapshotUnavailable,
            n_number: None,
            snapshot_id: None,
        },
        LookupOutcome::NotFound { snapshot, n_number } => Eligibility::Blocked {
            reason: BlockReason::RegistrationNotFound,
            n_number: Some(n_number),
            snapshot_id: Some(snapshot.id),
        },
        LookupOutcome::NotCovered { snapshot, n_number } => Eligibility::Blocked {
            reason: BlockReason::RegistrationNotCovered,
            n_number: Some(n_number),
            snapshot_id: Some(snapshot.id),
        },
        LookupOutcome::Ambiguous {
            snapshot, n_number, ..
        } => Eligibility::Blocked {
            reason: BlockReason::AmbiguousRegistration,
            n_number: Some(n_number),
            snapshot_id: Some(snapshot.id),
        },
    }
}

fn classify_registration(registration: Option<&str>) -> Result<String, LookupOutcome> {
    let Some(value) = registration
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(LookupOutcome::NotApplicable {
            supplied_registration: None,
            reason: NotApplicableReason::MissingRegistration,
        });
    };
    if let Some(normalized) = normalize_n_number(value) {
        return Ok(normalized);
    }
    let starts_like_n_number = value
        .chars()
        .find(|character| !character.is_ascii_whitespace() && *character != '-')
        .is_some_and(|character| character.eq_ignore_ascii_case(&'N'));
    Err(LookupOutcome::NotApplicable {
        supplied_registration: Some(value.to_string()),
        reason: if starts_like_n_number {
            NotApplicableReason::InvalidNNumber
        } else {
            NotApplicableReason::ForeignRegistration
        },
    })
}

fn compare_serials(observed: Option<&str>, registry: Option<&str>) -> SerialMatch {
    let observed = observed.map(str::trim).filter(|value| !value.is_empty());
    let registry = registry.map(str::trim).filter(|value| !value.is_empty());
    let Some(observed) = observed else {
        return SerialMatch::NotProvided;
    };
    let Some(registry) = registry else {
        return SerialMatch::RegistryUnavailable;
    };
    if observed == registry {
        return SerialMatch::RawExact;
    }
    match (
        normalize_serial_key(observed),
        normalize_serial_key(registry),
    ) {
        (Some(observed), Some(registry)) if observed == registry => SerialMatch::NormalizedOnly,
        _ => SerialMatch::Conflict,
    }
}

async fn latest_snapshot(db: &AppDb) -> Result<Option<Snapshot>> {
    let sql = db.sql(
        r#"
        SELECT id, evidence_source_id, snapshot_date, source_url, archive_sha256,
               source_manifest_sha256, target_set_sha256
        FROM faa_registry_snapshots
        ORDER BY snapshot_date DESC, id DESC
        LIMIT 1
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, SnapshotRow>(&sql)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, SnapshotRow>(&sql)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(row.map(Into::into))
}

async fn coverage_status(db: &AppDb, snapshot_id: i64, n_number: &str) -> Result<Option<String>> {
    let sql = db.sql(
        r#"
        SELECT lookup_status
        FROM faa_registry_coverage
        WHERE snapshot_id = ? AND n_number = ?
        "#,
    );
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => sqlx::query_scalar::<_, String>(&sql)
            .bind(snapshot_id)
            .bind(n_number)
            .fetch_optional(pool)
            .await
            .map_err(Into::into),
        DatabaseBackend::Postgres(pool) => sqlx::query_scalar::<_, String>(&sql)
            .bind(snapshot_id)
            .bind(n_number)
            .fetch_optional(pool)
            .await
            .map_err(Into::into),
    }
}

async fn covering_projection(
    db: &AppDb,
    latest_release: &Snapshot,
    n_number: &str,
) -> Result<Option<Snapshot>> {
    let sql = db.sql(
        r#"
        SELECT snapshot.id, snapshot.evidence_source_id, snapshot.snapshot_date, snapshot.source_url,
               snapshot.archive_sha256, snapshot.source_manifest_sha256,
               snapshot.target_set_sha256
        FROM faa_registry_snapshots snapshot
        JOIN faa_registry_coverage coverage
          ON coverage.snapshot_id = snapshot.id
         AND coverage.n_number = ?
        WHERE snapshot.snapshot_date = ? AND snapshot.archive_sha256 = ?
        ORDER BY (
          SELECT count(*) FROM faa_registry_coverage target
          WHERE target.snapshot_id = snapshot.id
        ) DESC, snapshot.id DESC
        LIMIT 1
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, SnapshotRow>(&sql)
                .bind(n_number)
                .bind(&latest_release.snapshot_date)
                .bind(&latest_release.archive_sha256)
                .fetch_optional(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, SnapshotRow>(&sql)
                .bind(n_number)
                .bind(&latest_release.snapshot_date)
                .bind(&latest_release.archive_sha256)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(row.map(Into::into))
}

fn checked_u16(value: Option<i64>, label: &str) -> Result<Option<u16>> {
    value
        .map(|value| u16::try_from(value).with_context(|| format!("{label} is out of range")))
        .transpose()
}

fn checked_u32(value: Option<i64>, label: &str) -> Result<Option<u32>> {
    value
        .map(|value| u32::try_from(value).with_context(|| format!("{label} is out of range")))
        .transpose()
}

#[derive(Debug, FromRow)]
struct SnapshotRow {
    id: i64,
    evidence_source_id: i64,
    snapshot_date: String,
    source_url: String,
    archive_sha256: String,
    source_manifest_sha256: String,
    target_set_sha256: String,
}

impl From<SnapshotRow> for Snapshot {
    fn from(row: SnapshotRow) -> Self {
        Self {
            id: row.id,
            evidence_source_id: row.evidence_source_id,
            snapshot_date: row.snapshot_date,
            source_url: row.source_url,
            archive_sha256: row.archive_sha256,
            source_manifest_sha256: row.source_manifest_sha256,
            target_set_sha256: row.target_set_sha256,
        }
    }
}

#[derive(Debug, FromRow)]
struct GroundingRow {
    n_number: String,
    manufacturer_serial_raw: Option<String>,
    manufacturer_serial_key: Option<String>,
    aircraft_code: String,
    engine_code: Option<String>,
    year_manufactured: Option<i64>,
    source_record_sha256: String,
    reference_aircraft_code: Option<String>,
    aircraft_manufacturer_name: Option<String>,
    aircraft_model_name: Option<String>,
    aircraft_type_code: Option<String>,
    aircraft_engine_type_code: Option<String>,
    category_code: Option<String>,
    certification_indicator_code: Option<String>,
    engine_count: Option<i64>,
    seat_count: Option<i64>,
    weight_class_code: Option<String>,
    cruise_speed_mph: Option<i64>,
    type_certificate_data_sheet: Option<String>,
    type_certificate_holder: Option<String>,
    reference_engine_code: Option<String>,
    engine_manufacturer_name: Option<String>,
    engine_model_name: Option<String>,
    engine_type_code: Option<String>,
    horsepower: Option<i64>,
    thrust_pounds: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n_number_normalization_is_conservative() {
        assert_eq!(normalize_n_number(" n-123 ab ").as_deref(), Some("N123AB"));
        assert_eq!(normalize_n_number("N1").as_deref(), Some("N1"));
        assert_eq!(normalize_n_number("N99999").as_deref(), Some("N99999"));
        assert_eq!(normalize_n_number("N1234Z").as_deref(), Some("N1234Z"));
        assert_eq!(normalize_n_number("N123ZZ").as_deref(), Some("N123ZZ"));
        assert_eq!(normalize_n_number("N1A").as_deref(), Some("N1A"));
        assert_eq!(normalize_n_number("N1AA").as_deref(), Some("N1AA"));
        assert_eq!(normalize_n_number("C-GABC"), None);
        assert_eq!(normalize_n_number("NAB"), None);
        assert_eq!(normalize_n_number("N0AB"), None);
        assert_eq!(normalize_n_number("N0123"), None);
        assert_eq!(normalize_n_number("N12I"), None);
        assert_eq!(normalize_n_number("N12O"), None);
        assert_eq!(normalize_n_number("N12@3"), None);
        assert_eq!(normalize_n_number("N123ABC"), None);
        assert_eq!(normalize_n_number("N12345A"), None);
        assert_eq!(normalize_n_number("N12A3"), None);
    }

    #[test]
    fn serial_grades_distinguish_raw_normalized_and_conflict() {
        assert_eq!(
            compare_serials(None, Some("182-123")),
            SerialMatch::NotProvided
        );
        assert_eq!(
            compare_serials(Some("182-123"), None),
            SerialMatch::RegistryUnavailable
        );
        assert_eq!(
            compare_serials(Some("182-123"), Some("182-123")),
            SerialMatch::RawExact
        );
        assert_eq!(
            compare_serials(Some("182123"), Some("182-123")),
            SerialMatch::NormalizedOnly
        );
        assert_eq!(
            compare_serials(Some("182-124"), Some("182-123")),
            SerialMatch::Conflict
        );
    }

    #[test]
    fn strict_gate_blocks_every_unresolved_condition() {
        let snapshot = Snapshot {
            id: 7,
            evidence_source_id: 11,
            snapshot_date: "2026-07-20".to_string(),
            source_url: "https://www.faa.gov/example".to_string(),
            archive_sha256: "a".repeat(64),
            source_manifest_sha256: "b".repeat(64),
            target_set_sha256: "c".repeat(64),
        };
        let grounding = AircraftGrounding {
            snapshot: snapshot.clone(),
            n_number: "N123AB".to_string(),
            manufacturer_serial_raw: Some("123".to_string()),
            manufacturer_serial_key: Some("123".to_string()),
            aircraft_code: "0000001".to_string(),
            engine_code: None,
            source_record_sha256: "d".repeat(64),
            year_manufactured: Some(2006),
            aircraft: None,
            engine: None,
            serial_match: SerialMatch::Conflict,
        };
        for (outcome, expected_reason) in [
            (
                LookupOutcome::NotApplicable {
                    supplied_registration: None,
                    reason: NotApplicableReason::MissingRegistration,
                },
                BlockReason::MissingRegistration,
            ),
            (
                LookupOutcome::NotApplicable {
                    supplied_registration: Some("C-GABC".to_string()),
                    reason: NotApplicableReason::ForeignRegistration,
                },
                BlockReason::NonNRegistration,
            ),
            (
                LookupOutcome::NotApplicable {
                    supplied_registration: Some("N12@3".to_string()),
                    reason: NotApplicableReason::InvalidNNumber,
                },
                BlockReason::InvalidNNumber,
            ),
            (
                LookupOutcome::NoSnapshot,
                BlockReason::RegistrySnapshotUnavailable,
            ),
            (
                LookupOutcome::NotFound {
                    snapshot: snapshot.clone(),
                    n_number: "N123AB".to_string(),
                },
                BlockReason::RegistrationNotFound,
            ),
            (
                LookupOutcome::NotCovered {
                    snapshot: snapshot.clone(),
                    n_number: "N123AB".to_string(),
                },
                BlockReason::RegistrationNotCovered,
            ),
            (
                LookupOutcome::Ambiguous {
                    snapshot: snapshot.clone(),
                    n_number: "N123AB".to_string(),
                    match_count: 2,
                },
                BlockReason::AmbiguousRegistration,
            ),
            (
                LookupOutcome::Found { grounding },
                BlockReason::SerialConflict,
            ),
        ] {
            assert!(matches!(
                require_eligible(outcome),
                Eligibility::Blocked { reason, .. } if reason == expected_reason
            ));
        }
    }
}
