//! Deterministic grounding against privacy-minimized FAA registry snapshots.
//!
//! The FAA registry is authoritative for the fields that its releasable
//! `MASTER`, `ACFTREF`, and `ENGINE` files actually contain. In particular,
//! [`AircraftGrounding::year_manufactured`] is not an aircraft model year.
//! This module does not promote FAA rows into the curated aircraft catalog.

mod admission;
mod import;
mod lookup;
mod store;
mod target;

pub use admission::{
    audit_listing_admission, block_reason_code, require_aircraft_admission,
    require_listing_admission, AircraftAdmissionError, ListingAdmissionEvidence,
    ListingAdmissionReport,
};
pub use import::{parse_release, ReleaseReaders};
pub use lookup::{
    lookup_current, normalize_n_number, normalize_serial_key, require_eligible, BlockReason,
    Eligibility, LookupOutcome, NotApplicableReason, SerialMatch,
};
pub use store::{store_release, StoredSnapshot};
pub use target::{
    listing_targets, ExplicitNNumberTargets, FaaImportTargets, ListingTargetCounts, ListingTargets,
    PendingSubmissionTargetCounts,
};

use serde::{Deserialize, Serialize};

pub const RELEASE_SOURCE_URL: &str =
    "https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/releasable_aircraft_download";
pub const MASTER_MEMBER_NAME: &str = "MASTER.txt";
pub const AIRCRAFT_MEMBER_NAME: &str = "ACFTREF.txt";
pub const ENGINE_MEMBER_NAME: &str = "ENGINE.txt";

/// Provenance supplied by the downloader for one FAA release archive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseMetadata {
    /// Calendar date represented by the daily FAA release, in `YYYY-MM-DD`.
    pub snapshot_date: String,
    /// Official download-page URL (or an immutable official archive URL).
    pub source_url: String,
    /// SHA-256 of the original FAA ZIP, before extracting its members.
    pub archive_sha256: String,
}

impl ReleaseMetadata {
    pub fn official(snapshot_date: impl Into<String>, archive_sha256: impl Into<String>) -> Self {
        Self {
            snapshot_date: snapshot_date.into(),
            source_url: RELEASE_SOURCE_URL.to_string(),
            archive_sha256: archive_sha256.into(),
        }
    }
}

/// Digest and archive-member identity retained for each imported source file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemberProvenance {
    pub member_name: String,
    pub sha256: String,
}

/// Privacy-minimized projection of one current FAA `MASTER` row.
///
/// Owner, address, other-name, Mode-S, and other registrant fields are never
/// represented by this type and therefore cannot be persisted by this importer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AircraftRecord {
    /// Canonical U.S. registration including the leading `N`.
    pub n_number: String,
    pub manufacturer_serial_raw: Option<String>,
    pub manufacturer_serial_key: Option<String>,
    /// Opaque seven-character FAA manufacturer/model/series code.
    pub aircraft_code: String,
    /// Opaque FAA engine manufacturer/model code.
    pub engine_code: Option<String>,
    /// FAA `YEAR MFR`; deliberately not named or treated as model year.
    pub year_manufactured: Option<u16>,
    /// SHA-256 of the full logical MASTER CSV record (all parsed fields,
    /// length-delimited in source order). This cites the exact source
    /// observation without retaining its registrant/owner fields.
    pub source_record_sha256: String,
}

/// Non-PII projection of one FAA `ACFTREF` row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AircraftReference {
    pub aircraft_code: String,
    pub manufacturer_name: Option<String>,
    pub model_name: Option<String>,
    pub aircraft_type_code: Option<String>,
    pub engine_type_code: Option<String>,
    pub category_code: Option<String>,
    pub certification_indicator_code: Option<String>,
    pub engine_count: Option<u16>,
    pub seat_count: Option<u16>,
    pub weight_class_code: Option<String>,
    pub cruise_speed_mph: Option<u16>,
    pub type_certificate_data_sheet: Option<String>,
    pub type_certificate_holder: Option<String>,
}

/// Non-PII projection of one FAA `ENGINE` row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EngineReference {
    pub engine_code: String,
    pub manufacturer_name: Option<String>,
    pub model_name: Option<String>,
    pub engine_type_code: Option<String>,
    pub horsepower: Option<u32>,
    pub thrust_pounds: Option<u32>,
}

/// Fully parsed, digest-verified release ready for atomic storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
    pub metadata: ReleaseMetadata,
    /// Digest over the release metadata and all three member identities/digests.
    pub source_manifest_sha256: String,
    /// Digest over the sorted, normalized N-numbers intentionally scanned.
    pub target_set_sha256: String,
    pub master: MemberProvenance,
    pub aircraft_reference: MemberProvenance,
    pub engine_reference: MemberProvenance,
    pub coverage: Vec<TargetCoverage>,
    pub aircraft: Vec<AircraftRecord>,
    pub aircraft_references: Vec<AircraftReference>,
    pub engine_references: Vec<EngineReference>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TargetCoverage {
    pub n_number: String,
    pub matched: bool,
}

/// Immutable identity of a stored FAA snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: i64,
    /// Exact regulator-primary evidence row for this archive digest.
    pub evidence_source_id: i64,
    pub snapshot_date: String,
    pub source_url: String,
    pub archive_sha256: String,
    pub source_manifest_sha256: String,
    pub target_set_sha256: String,
}

/// FAA facts joined deterministically through the opaque aircraft and engine
/// reference codes from `MASTER`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AircraftGrounding {
    pub snapshot: Snapshot,
    pub n_number: String,
    pub manufacturer_serial_raw: Option<String>,
    pub manufacturer_serial_key: Option<String>,
    pub aircraft_code: String,
    pub engine_code: Option<String>,
    pub source_record_sha256: String,
    /// This is FAA `YEAR MFR`, never a model-year substitution.
    pub year_manufactured: Option<u16>,
    pub aircraft: Option<AircraftReference>,
    pub engine: Option<EngineReference>,
    pub serial_match: SerialMatch,
}
