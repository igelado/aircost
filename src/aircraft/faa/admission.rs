//! Fail-closed admission for listing-backed aircraft work.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use super::{lookup_current, require_eligible, AircraftGrounding, BlockReason, Eligibility};
use crate::aircraft::identity::{require_listing_identity_assignment, IdentityAssignmentError};
use crate::db::{AppDb, DatabaseBackend};
use crate::html::clean::listing_body_contains_exact_structurally_visible_text_span;

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

/// A source serial that disagreed with the current FAA MASTER row and was
/// replaced only after the same N-number was admitted with the FAA serial.
/// The observed value remains source evidence; callers may use the corrected
/// value in a working materialization copy but must not rewrite the source
/// extraction checkpoint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FaaSerialCorrection {
    pub observed_serial_number: String,
    pub corrected_serial_number: String,
}

/// Exact current-FAA admission for a source observation, with an optional
/// narrow serial correction. No other FAA rejection is softened by this path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceAircraftAdmission {
    pub grounding: AircraftGrounding,
    pub serial_correction: Option<FaaSerialCorrection>,
}

impl SourceAircraftAdmission {
    pub fn effective_serial_number(&self) -> Option<&str> {
        self.serial_correction
            .as_ref()
            .map(|correction| correction.corrected_serial_number.as_str())
            .or(self.grounding.manufacturer_serial_raw.as_deref())
    }
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

/// Admit one raw source identity, correcting only an explicit serial conflict
/// from the exact current FAA row for the supplied N-number.
///
/// This operation is provider-free and performs no writes. A caller that uses
/// the corrected value must retain the source extraction unchanged and record
/// the returned correction at its materialization boundary.
pub async fn admit_aircraft_source_identity(
    db: &AppDb,
    registration: Option<&str>,
    serial: Option<&str>,
    retained_source: Option<&str>,
) -> Result<SourceAircraftAdmission, AircraftAdmissionError> {
    match require_aircraft_admission(db, registration, serial).await {
        Ok(grounding) => Ok(SourceAircraftAdmission {
            grounding,
            serial_correction: None,
        }),
        Err(
            conflict @ AircraftAdmissionError::Rejected {
                reason: BlockReason::SerialConflict,
                ..
            },
        ) => {
            let Some(observed_serial_number) = serial
                .map(str::trim)
                .filter(|serial| !serial.is_empty())
                .map(str::to_string)
            else {
                return Err(conflict);
            };
            let Some(observed_registration) = registration
                .map(str::trim)
                .filter(|registration| !registration.is_empty())
            else {
                return Err(conflict);
            };
            let Some(retained_source) = retained_source else {
                return Err(conflict);
            };
            if !listing_body_contains_exact_structurally_visible_text_span(
                retained_source,
                observed_registration,
            ) || !listing_body_contains_exact_structurally_visible_text_span(
                retained_source,
                &observed_serial_number,
            ) {
                return Err(conflict);
            }
            let registration_grounding = require_aircraft_admission(db, registration, None).await?;
            let Some(corrected_serial_number) = registration_grounding
                .manufacturer_serial_raw
                .as_deref()
                .map(str::trim)
                .filter(|serial| !serial.is_empty())
                .map(str::to_string)
            else {
                return Err(conflict);
            };
            if !is_narrow_serial_typo(&observed_serial_number, &corrected_serial_number) {
                return Err(conflict);
            }
            let grounding = require_aircraft_admission(
                db,
                Some(&registration_grounding.n_number),
                Some(&corrected_serial_number),
            )
            .await?;
            Ok(SourceAircraftAdmission {
                grounding,
                serial_correction: Some(FaaSerialCorrection {
                    observed_serial_number,
                    corrected_serial_number,
                }),
            })
        }
        Err(error) => Err(error),
    }
}

/// The automatic source correction is deliberately limited to one internal
/// transcription edit. Both normalized serials must be substantial, retain
/// the same first and last two characters, and differ by exactly one inserted,
/// deleted, or substituted character, or one adjacent transposition. Prefix
/// and suffix edits remain manual because they are more likely to identify a
/// different aircraft than a typographical error.
fn is_narrow_serial_typo(observed: &str, registry: &str) -> bool {
    let Some(observed) = super::normalize_serial_key(observed) else {
        return false;
    };
    let Some(registry) = super::normalize_serial_key(registry) else {
        return false;
    };
    let observed = observed.as_bytes();
    let registry = registry.as_bytes();
    if observed == registry
        || observed.len() < 5
        || registry.len() < 5
        || observed[..2] != registry[..2]
        || observed[observed.len() - 2..] != registry[registry.len() - 2..]
        || observed.len().abs_diff(registry.len()) > 1
    {
        return false;
    }
    if observed.len() == registry.len() {
        let differences = observed
            .iter()
            .zip(registry)
            .enumerate()
            .filter_map(|(index, (left, right))| (left != right).then_some(index))
            .collect::<Vec<_>>();
        return differences.len() == 1
            || (differences.len() == 2
                && differences[1] == differences[0] + 1
                && observed[differences[0]] == registry[differences[1]]
                && observed[differences[1]] == registry[differences[0]]);
    }
    let (shorter, longer) = if observed.len() < registry.len() {
        (observed, registry)
    } else {
        (registry, observed)
    };
    let mismatch = shorter
        .iter()
        .zip(longer)
        .position(|(left, right)| left != right)
        .unwrap_or(shorter.len());
    shorter[mismatch..] == longer[mismatch + 1..]
}

/// Load registration and serial for an existing listing and require only the
/// raw current FAA match. This is the pre-curation boundary: it intentionally
/// does not treat legacy product labels as authoritative.
pub async fn require_listing_faa_admission(
    db: &AppDb,
    listing_id: i64,
) -> Result<AircraftGrounding, AircraftAdmissionError> {
    let sql = db.sql(
        r#"
        SELECT
          listing.registration_number,
          listing.serial_number
        FROM aircraft_sale_listings listing
        WHERE listing.id = ?
        "#,
    );
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

/// Published/valuation admission requires both the raw FAA match and the
/// current immutable curated identity assignment bound to that exact release.
pub async fn require_listing_admission(
    db: &AppDb,
    listing_id: i64,
) -> Result<AircraftGrounding, AircraftAdmissionError> {
    let grounding = require_listing_faa_admission(db, listing_id).await?;
    require_listing_identity_assignment(db, listing_id, &grounding)
        .await
        .map_err(|error| identity_assignment_admission_error(listing_id, &grounding, error))?;
    Ok(grounding)
}

fn identity_assignment_admission_error(
    listing_id: i64,
    grounding: &AircraftGrounding,
    error: IdentityAssignmentError,
) -> AircraftAdmissionError {
    match error {
        IdentityAssignmentError::Missing(_) => AircraftAdmissionError::Rejected {
            listing_id: Some(listing_id),
            reason: BlockReason::CanonicalIdentityAssignmentMissing,
            n_number: Some(grounding.n_number.clone()),
            snapshot_id: Some(grounding.snapshot.id),
        },
        IdentityAssignmentError::Mismatch { .. } => AircraftAdmissionError::Rejected {
            listing_id: Some(listing_id),
            reason: BlockReason::CanonicalIdentityAssignmentMismatch,
            n_number: Some(grounding.n_number.clone()),
            snapshot_id: Some(grounding.snapshot.id),
        },
        IdentityAssignmentError::ListingNotFound(_) => {
            AircraftAdmissionError::ListingNotFound { listing_id }
        }
        IdentityAssignmentError::Database(message) => AircraftAdmissionError::LookupFailed {
            listing_id: Some(listing_id),
            message,
        },
    }
}

/// Audit every stored listing, or an exact requested subset, through the same
/// strict policy used for mutation admission. A requested listing that no
/// longer exists is excluded rather than silently disappearing. Database
/// lookup failures abort the audit, so callers fail closed.
pub async fn audit_listing_admission(
    db: &AppDb,
    listing_ids: Option<&BTreeSet<i64>>,
) -> Result<ListingAdmissionReport, AircraftAdmissionError> {
    let sql = db.sql("SELECT id, serial_number FROM aircraft_sale_listings ORDER BY id");
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
    for row in rows {
        if listing_ids.is_some_and(|ids| !ids.contains(&row.id)) {
            continue;
        }
        found_ids.insert(row.id);
        report.evaluated_count += 1;
        let admission = require_listing_admission(db, row.id).await;
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
        BlockReason::RegistryAircraftIdentityUnavailable => {
            "registry_aircraft_identity_unavailable"
        }
        BlockReason::AircraftManufacturerMismatch => "aircraft_manufacturer_mismatch",
        BlockReason::AircraftModelMismatch => "aircraft_model_mismatch",
        BlockReason::CanonicalIdentityAssignmentMissing => "canonical_identity_assignment_missing",
        BlockReason::CanonicalIdentityAssignmentMismatch => {
            "canonical_identity_assignment_mismatch"
        }
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
    serial_number: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aircraft::faa::{
        store_release, AircraftRecord, AircraftReference, MemberProvenance, Release,
        ReleaseMetadata, TargetCoverage,
    };

    fn release(n_number: &str, serial: &str) -> Release {
        Release {
            metadata: ReleaseMetadata::official("2026-08-19", "a".repeat(64)),
            source_manifest_sha256: "b".repeat(64),
            target_set_sha256: "c".repeat(64),
            master: MemberProvenance {
                member_name: "MASTER.txt".into(),
                sha256: "d".repeat(64),
            },
            aircraft_reference: MemberProvenance {
                member_name: "ACFTREF.txt".into(),
                sha256: "e".repeat(64),
            },
            engine_reference: MemberProvenance {
                member_name: "ENGINE.txt".into(),
                sha256: "f".repeat(64),
            },
            coverage: vec![TargetCoverage {
                n_number: n_number.into(),
                matched: true,
            }],
            aircraft: vec![AircraftRecord {
                n_number: n_number.into(),
                manufacturer_serial_raw: Some(serial.into()),
                manufacturer_serial_key: super::super::normalize_serial_key(serial),
                aircraft_code: "2072738".into(),
                engine_code: None,
                year_manufactured: Some(2020),
                source_record_sha256: "1".repeat(64),
            }],
            aircraft_references: vec![AircraftReference {
                aircraft_code: "2072738".into(),
                manufacturer_name: Some("CESSNA".into()),
                model_name: Some("182T".into()),
                aircraft_type_code: None,
                engine_type_code: None,
                category_code: None,
                certification_indicator_code: None,
                engine_count: Some(1),
                seat_count: Some(4),
                weight_class_code: None,
                cruise_speed_mph: None,
                type_certificate_data_sheet: Some("3A13".into()),
                type_certificate_holder: Some("Textron Aviation Inc.".into()),
            }],
            engine_references: Vec::new(),
        }
    }

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

    #[test]
    fn automatic_serial_typo_is_one_internal_edit_only() {
        assert!(is_narrow_serial_typo("1823006", "18283006"));
        assert!(is_narrow_serial_typo("18280306", "18283006"));
        assert!(is_narrow_serial_typo("18283106", "18283006"));
        assert!(!is_narrow_serial_typo("X8283006", "18283006"));
        assert!(!is_narrow_serial_typo("18283009", "18283006"));
        assert!(!is_narrow_serial_typo("1823009", "18283006"));
        assert!(!is_narrow_serial_typo("WRONG-SERIAL", "18283006"));
    }

    #[tokio::test]
    async fn source_correction_requires_exact_visible_registration_and_serial() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        store_release(&db, &release("N482TW", "18283006"))
            .await
            .unwrap();

        let wrong_registration = admit_aircraft_source_identity(
            &db,
            Some("N482TW"),
            Some("1823006"),
            Some("Registration N482TX; serial 1823006"),
        )
        .await
        .expect_err("a model-supplied N-number absent from retained source must stay rejected");
        assert_eq!(
            wrong_registration.block_reason(),
            Some(&BlockReason::SerialConflict)
        );

        let missing_serial = admit_aircraft_source_identity(
            &db,
            Some("N482TW"),
            Some("1823006"),
            Some("Registration N482TW; serial unavailable"),
        )
        .await
        .expect_err("the observed serial must also be exact retained-source text");
        assert_eq!(
            missing_serial.block_reason(),
            Some(&BlockReason::SerialConflict)
        );

        let unrelated_serial = admit_aircraft_source_identity(
            &db,
            Some("N482TW"),
            Some("99999999"),
            Some("Registration N482TW; serial 99999999"),
        )
        .await
        .expect_err("an unrelated serial is not an automatic typo correction");
        assert_eq!(
            unrelated_serial.block_reason(),
            Some(&BlockReason::SerialConflict)
        );
    }
}
