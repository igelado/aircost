//! Deterministic grounding against privacy-minimized FAA registry snapshots.
//!
//! The FAA registry is authoritative for the fields that its releasable
//! `MASTER`, `ACFTREF`, and `ENGINE` files actually contain. In particular,
//! [`AircraftGrounding::year_manufactured`] is not an aircraft model year.
//! This module does not promote FAA rows into the curated aircraft catalog.

mod admission;
pub(crate) mod bridge;
pub mod drs;
mod import;
mod lookup;
mod store;
mod target;

pub use admission::{
    admit_aircraft_source_identity, audit_listing_admission, block_reason_code,
    require_aircraft_admission, require_listing_admission, require_listing_faa_admission,
    AircraftAdmissionError, FaaSerialCorrection, ListingAdmissionEvidence, ListingAdmissionReport,
    SourceAircraftAdmission,
};
pub use import::parse_release_archive;
#[cfg(test)]
pub(crate) use import::ReleaseFixtureBuilder;
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
/// Domain separator prepended to every retained FAA MASTER projection hash.
/// Stored snapshots must name this exact domain so a record digest is never
/// interpreted without the algorithm and field set that produced it.
pub const AIRCRAFT_RECORD_HASH_DOMAIN: &str = "aircost-faa-master-retained-aircraft-projection-v1";

/// Provenance computed from one FAA release archive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseMetadata {
    /// Calendar date represented by the daily FAA release, in `YYYY-MM-DD`.
    pub snapshot_date: String,
    /// Official download-page URL (or an immutable official archive URL).
    pub source_url: String,
    /// SHA-256 computed from the exact original FAA ZIP bytes before reading
    /// the required members.
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
    /// SHA-256 of only this retained non-PII aircraft projection under a
    /// versioned domain. Exact archive and member hashes bind the source bytes;
    /// discarded owner, address, and other MASTER fields never enter this hash.
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
///
/// All fields are private so downstream safe Rust can persist only a release
/// constructed by this module's archive parser. Tests inside this crate use a
/// separate `cfg(test)` fixture builder that is absent from production builds.
///
/// ```compile_fail
/// use aircost_rs::aircraft::faa::Release;
///
/// let fabricated = Release { /* parser-owned fields are private */ };
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
    metadata: ReleaseMetadata,
    /// Digest over the release metadata and all three member identities/digests.
    source_manifest_sha256: String,
    /// Digest over the sorted, normalized N-numbers intentionally scanned.
    target_set_sha256: String,
    master: MemberProvenance,
    aircraft_reference: MemberProvenance,
    engine_reference: MemberProvenance,
    coverage: Vec<TargetCoverage>,
    aircraft: Vec<AircraftRecord>,
    aircraft_references: Vec<AircraftReference>,
    engine_references: Vec<EngineReference>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ReleaseMemberDigestSummary<'a> {
    pub master: &'a str,
    pub aircraft_reference: &'a str,
    pub engine_reference: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ReleaseSummary<'a> {
    pub snapshot_date: &'a str,
    pub source_url: &'a str,
    pub archive_sha256: &'a str,
    pub source_manifest_sha256: &'a str,
    pub target_set_sha256: &'a str,
    pub record_hash_domain: &'a str,
    pub member_sha256: ReleaseMemberDigestSummary<'a>,
    pub target_count: usize,
    pub matched_count: usize,
    pub absent_count: usize,
    pub aircraft_reference_count: usize,
    pub engine_reference_count: usize,
}

impl Release {
    pub fn summary(&self) -> ReleaseSummary<'_> {
        ReleaseSummary {
            snapshot_date: &self.metadata.snapshot_date,
            source_url: &self.metadata.source_url,
            archive_sha256: &self.metadata.archive_sha256,
            source_manifest_sha256: &self.source_manifest_sha256,
            target_set_sha256: &self.target_set_sha256,
            record_hash_domain: AIRCRAFT_RECORD_HASH_DOMAIN,
            member_sha256: ReleaseMemberDigestSummary {
                master: &self.master.sha256,
                aircraft_reference: &self.aircraft_reference.sha256,
                engine_reference: &self.engine_reference.sha256,
            },
            target_count: self.coverage.len(),
            matched_count: self.aircraft.len(),
            absent_count: self.coverage.iter().filter(|row| !row.matched).count(),
            aircraft_reference_count: self.aircraft_references.len(),
            engine_reference_count: self.engine_references.len(),
        }
    }
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
    /// Exact domain that defines every `source_record_sha256` in this
    /// projection. It participates in snapshot identity and reuse.
    pub record_hash_domain: String,
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
