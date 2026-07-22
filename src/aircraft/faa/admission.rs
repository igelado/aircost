//! Fail-closed admission for listing-backed aircraft work.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};

use super::{lookup_current, require_eligible, AircraftGrounding, BlockReason, Eligibility};

/// A listing or raw aircraft observation that cannot be admitted through the
/// current FAA projection. Rejections preserve the source listing; callers
/// decide whether to report, quarantine, or skip it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AircraftAdmissionError {
    Rejected {
        listing_id: Option<i64>,
        reason: BlockReason,
        n_number: Option<String>,
        snapshot_id: Option<i64>,
    },
    LookupFailed {
        listing_id: Option<i64>,
        message: String,
    },
    ListingNotFound {
        listing_id: i64,
    },
}

/// One deterministic audit of stored listing identities against the current
/// FAA release. This is the shared bulk boundary for datasets that must report
/// exclusions without mutating or deleting their source listings.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ListingAdmissionReport {
    pub evaluated_count: usize,
    pub admitted_count: usize,
    pub excluded_count: usize,
    pub exclusions: BTreeMap<String, usize>,
    admitted_listing_ids: BTreeSet<i64>,
    admitted_evidence: BTreeMap<i64, ListingAdmissionEvidence>,
    excluded_listing_reasons: BTreeMap<i64, String>,
}

impl ListingAdmissionReport {
    pub fn is_admitted(&self, listing_id: i64) -> bool {
        self.admitted_listing_ids.contains(&listing_id)
    }

    pub fn exclusion_reason(&self, listing_id: i64) -> Option<&str> {
        self.excluded_listing_reasons
            .get(&listing_id)
            .map(String::as_str)
    }

    pub fn admission_evidence(&self, listing_id: i64) -> Option<&ListingAdmissionEvidence> {
        self.admitted_evidence.get(&listing_id)
    }
}

/// Minimal immutable evidence needed to prove that a frozen dataset row still
/// represents the same admitted aircraft under the same FAA projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListingAdmissionEvidence {
    pub n_number: String,
    pub observed_serial_key: Option<String>,
    pub faa_snapshot_id: i64,
    pub faa_snapshot_date: String,
    pub faa_archive_sha256: String,
    pub faa_source_record_sha256: String,
}

impl AircraftAdmissionError {
    /// The deterministic FAA policy reason, when the lookup completed and the
    /// aircraft was explicitly rejected.
    pub fn block_reason(&self) -> Option<&BlockReason> {
        match self {
            Self::Rejected { reason, .. } => Some(reason),
            Self::LookupFailed { .. } | Self::ListingNotFound { .. } => None,
        }
    }

    pub fn listing_id(&self) -> Option<i64> {
        match self {
            Self::Rejected { listing_id, .. } | Self::LookupFailed { listing_id, .. } => {
                *listing_id
            }
            Self::ListingNotFound { listing_id } => Some(*listing_id),
        }
    }

    fn with_listing_id(self, listing_id: i64) -> Self {
        match self {
            Self::Rejected {
                reason,
                n_number,
                snapshot_id,
                ..
            } => Self::Rejected {
                listing_id: Some(listing_id),
                reason,
                n_number,
                snapshot_id,
            },
            Self::LookupFailed { message, .. } => Self::LookupFailed {
                listing_id: Some(listing_id),
                message,
            },
            Self::ListingNotFound { .. } => Self::ListingNotFound { listing_id },
        }
    }
}

impl fmt::Display for AircraftAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected {
                listing_id,
                reason,
                n_number,
                snapshot_id,
            } => {
                write!(formatter, "FAA aircraft admission rejected")?;
                if let Some(listing_id) = listing_id {
                    write!(formatter, " for listing {listing_id}")?;
                }
                write!(formatter, ": {}", block_reason_code(reason))?;
                if let Some(n_number) = n_number {
                    write!(formatter, " (N-number {n_number}")?;
                    if let Some(snapshot_id) = snapshot_id {
                        write!(formatter, ", snapshot {snapshot_id}")?;
                    }
                    write!(formatter, ")")?;
                } else if let Some(snapshot_id) = snapshot_id {
                    write!(formatter, " (snapshot {snapshot_id})")?;
                }
                Ok(())
            }
            Self::LookupFailed {
                listing_id,
                message,
            } => {
                write!(formatter, "FAA aircraft admission lookup failed")?;
                if let Some(listing_id) = listing_id {
                    write!(formatter, " for listing {listing_id}")?;
                }
                write!(formatter, ": {message}")
            }
            Self::ListingNotFound { listing_id } => {
                write!(
                    formatter,
                    "listing {listing_id} was not found for FAA admission"
                )
            }
        }
    }
}

impl std::error::Error for AircraftAdmissionError {}

/// Require a raw registration/serial observation to match the newest stored
/// FAA release and its target coverage before any listing-backed processing.
pub async fn require_aircraft_admission(
    db: &AppDb,
    registration: Option<&str>,
    serial: Option<&str>,
) -> Result<AircraftGrounding, AircraftAdmissionError> {
    let outcome = lookup_current(db, registration, serial)
        .await
        .map_err(|error| AircraftAdmissionError::LookupFailed {
            listing_id: None,
            message: error.to_string(),
        })?;
    match require_eligible(outcome) {
        Eligibility::Eligible { grounding } => Ok(grounding),
        Eligibility::Blocked {
            reason,
            n_number,
            snapshot_id,
        } => Err(AircraftAdmissionError::Rejected {
            listing_id: None,
            reason,
            n_number,
            snapshot_id,
        }),
    }
}

/// Load the current canonical registration and serial for an existing listing,
/// then apply the same fail-closed admission policy as new listing ingestion.
pub async fn require_listing_admission(
    db: &AppDb,
    listing_id: i64,
) -> Result<AircraftGrounding, AircraftAdmissionError> {
    let sql = db
        .sql("SELECT registration_number, serial_number FROM aircraft_sale_listings WHERE id = ?");
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ListingIdentityRow>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ListingIdentityRow>(&sql)
                .bind(listing_id)
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|error| AircraftAdmissionError::LookupFailed {
        listing_id: Some(listing_id),
        message: error.to_string(),
    })?
    .ok_or(AircraftAdmissionError::ListingNotFound { listing_id })?;

    require_aircraft_admission(
        db,
        row.registration_number.as_deref(),
        row.serial_number.as_deref(),
    )
    .await
    .map_err(|error| error.with_listing_id(listing_id))
}

/// Audit every stored listing, or an exact requested subset, through the same
/// strict policy used for mutation admission. A requested listing that no
/// longer exists is excluded rather than silently disappearing. Database
/// lookup failures abort the audit, so callers fail closed.
pub async fn audit_listing_admission(
    db: &AppDb,
    listing_ids: Option<&BTreeSet<i64>>,
) -> Result<ListingAdmissionReport, AircraftAdmissionError> {
    let sql = db.sql(
        "SELECT id, registration_number, serial_number FROM aircraft_sale_listings ORDER BY id",
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ListingAdmissionRow>(&sql)
                .fetch_all(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ListingAdmissionRow>(&sql)
                .fetch_all(pool)
                .await
        }
    }
    .map_err(|error| AircraftAdmissionError::LookupFailed {
        listing_id: None,
        message: error.to_string(),
    })?;

    let mut report = ListingAdmissionReport::default();
    let mut found_ids = BTreeSet::new();
    let mut cache = HashMap::<
        (Option<String>, Option<String>),
        Result<AircraftGrounding, AircraftAdmissionError>,
    >::new();
    for row in rows {
        if listing_ids.is_some_and(|ids| !ids.contains(&row.id)) {
            continue;
        }
        found_ids.insert(row.id);
        report.evaluated_count += 1;
        let key = (row.registration_number.clone(), row.serial_number.clone());
        let admission = if let Some(cached) = cache.get(&key) {
            cached.clone()
        } else {
            let result = require_aircraft_admission(
                db,
                row.registration_number.as_deref(),
                row.serial_number.as_deref(),
            )
            .await;
            cache.insert(key, result.clone());
            result
        };
        match admission {
            Ok(grounding) => {
                report.admitted_count += 1;
                report.admitted_listing_ids.insert(row.id);
                report.admitted_evidence.insert(
                    row.id,
                    ListingAdmissionEvidence {
                        n_number: grounding.n_number,
                        observed_serial_key: row
                            .serial_number
                            .as_deref()
                            .and_then(super::normalize_serial_key),
                        faa_snapshot_id: grounding.snapshot.id,
                        faa_snapshot_date: grounding.snapshot.snapshot_date,
                        faa_archive_sha256: grounding.snapshot.archive_sha256,
                        faa_source_record_sha256: grounding.source_record_sha256,
                    },
                );
            }
            Err(AircraftAdmissionError::Rejected { reason, .. }) => {
                let code = block_reason_code(&reason).to_string();
                report.excluded_count += 1;
                *report.exclusions.entry(code.clone()).or_default() += 1;
                report.excluded_listing_reasons.insert(row.id, code);
            }
            Err(error) => return Err(error.with_listing_id(row.id)),
        }
    }

    if let Some(requested_ids) = listing_ids {
        for listing_id in requested_ids.difference(&found_ids) {
            let code = "listing_not_found".to_string();
            report.evaluated_count += 1;
            report.excluded_count += 1;
            *report.exclusions.entry(code.clone()).or_default() += 1;
            report.excluded_listing_reasons.insert(*listing_id, code);
        }
    }
    debug_assert_eq!(
        report.evaluated_count,
        report.admitted_count + report.excluded_count
    );
    let releases = report
        .admitted_evidence
        .values()
        .map(|evidence| {
            (
                evidence.faa_snapshot_date.as_str(),
                evidence.faa_archive_sha256.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    if releases.len() > 1 {
        return Err(AircraftAdmissionError::LookupFailed {
            listing_id: None,
            message: "FAA current release changed during the bulk listing admission audit; retry the operation"
                .to_string(),
        });
    }
    Ok(report)
}

pub fn block_reason_code(reason: &BlockReason) -> &'static str {
    match reason {
        BlockReason::MissingRegistration => "missing_registration",
        BlockReason::NonNRegistration => "non_n_registration",
        BlockReason::InvalidNNumber => "invalid_n_number",
        BlockReason::RegistrySnapshotUnavailable => "registry_snapshot_unavailable",
        BlockReason::RegistrationNotFound => "registration_not_found",
        BlockReason::RegistrationNotCovered => "registration_not_covered",
        BlockReason::AmbiguousRegistration => "ambiguous_registration",
        BlockReason::SerialConflict => "serial_conflict",
    }
}

#[derive(Debug, FromRow)]
struct ListingIdentityRow {
    registration_number: Option<String>,
    serial_number: Option<String>,
}

#[derive(Debug, FromRow)]
struct ListingAdmissionRow {
    id: i64,
    registration_number: Option<String>,
    serial_number: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn raw_admission_rejects_when_no_registry_snapshot_exists() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");

        let error = require_aircraft_admission(&db, Some("N123AB"), Some("182-123"))
            .await
            .expect_err("FAA admission requires a stored current snapshot");

        assert_eq!(
            error.block_reason(),
            Some(&BlockReason::RegistrySnapshotUnavailable)
        );
        assert_eq!(error.listing_id(), None);
        assert!(error.to_string().contains("registry_snapshot_unavailable"));
    }
}
