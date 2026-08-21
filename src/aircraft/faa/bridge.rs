//! One-purpose FAA translation for the frozen replay-source bridge.
//!
//! The bridge never copies or mechanically rehashes a legacy FAA projection.
//! It parses the operator-supplied archive through the current parser and
//! stores only that parser-owned release in the fresh target.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::db::AppDb;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct FaaBridgeReport {
    pub archive_sha256: String,
    pub snapshot_date: String,
    pub target_count: usize,
    pub matched_count: usize,
    pub stored: bool,
}

pub(crate) async fn rebuild_faa_projection(
    _legacy_source: &SqlitePool,
    _target: Option<&AppDb>,
    _archive: &Path,
    _expected_archive_sha256: &str,
    _n_numbers: &[String],
    _apply: bool,
) -> Result<FaaBridgeReport> {
    anyhow::bail!("FAA replay-source bridge is not yet initialized")
}
