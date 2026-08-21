//! Isolated conversion of the one attested legacy SQLite source used by the
//! clean-replay rebuild.
//!
//! This is an administrative conversion boundary, not a runtime compatibility
//! path. Unknown source shapes fail closed and the source is always opened
//! read-only.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::TrustedCaptureManifest;

pub const LEGACY_SQLITE_SCHEMA_OBJECT_COUNT: usize = 575;
pub const LEGACY_SQLITE_SCHEMA_SHA3_256: &str =
    "527552c4fbe674eaca5de3a1228bfcde0fd99f05c7f2924f28ffdba687ec5957";
pub const LEGACY_SCHEMA_RECEIPT_COUNT: usize = 19;
pub const LEGACY_SCHEMA_RECEIPT_SHA3_256: &str =
    "7081e6098b4e11367b7a371301c4b6ff1e916104174d65906a723c41284dd639";
pub const LEGACY_FAA_ARCHIVE_SHA256: &str =
    "14885735825e5f46babdac8bf851c77c7ce7b104ae0f86395ef594e6e467c724";

pub struct PrepareLegacyReplaySourceRequest<'a> {
    pub source_database: &'a str,
    pub manifest: &'a TrustedCaptureManifest,
    pub faa_archive: &'a Path,
    pub expected_faa_archive_sha256: &'a str,
    pub output: &'a Path,
    pub apply: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrepareLegacyReplaySourceReport {
    pub dry_run: bool,
    pub provider_calls: u64,
    pub source_schema_object_count: usize,
    pub source_schema_sha3_256: String,
    pub source_receipt_count: usize,
    pub source_receipt_sha3_256: String,
    pub manifest_sha256: String,
    pub capture_count: usize,
    pub n_number_count: usize,
    pub faa_archive_sha256: String,
    pub faa_snapshot_date: String,
    pub catalog_fingerprint_sha256: String,
    pub applied_rows: usize,
    pub output_created: bool,
}

pub async fn prepare_legacy_replay_source(
    _request: PrepareLegacyReplaySourceRequest<'_>,
) -> Result<PrepareLegacyReplaySourceReport> {
    anyhow::bail!("legacy replay-source bridge is not yet initialized")
}
