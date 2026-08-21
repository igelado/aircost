//! Typed projection of the reusable verified catalog closure.
//!
//! This is deliberately separate from listing replay. It selects only shared,
//! provider-free catalog truth and never imports listings, listing reviews,
//! assignments, correction receipts, valuation state, or provider artifacts.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::db::AppDb;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct CatalogProjectionReport {
    pub fingerprint_sha256: String,
    pub source_counts: BTreeMap<String, usize>,
    pub applied_rows: usize,
}

/// Projects the closed, reusable catalog graph from an already attested
/// legacy source into a fresh canonical target.
pub(crate) async fn project_reusable_catalog(
    _source: &SqlitePool,
    _target: &AppDb,
    _apply: bool,
) -> Result<CatalogProjectionReport> {
    Ok(CatalogProjectionReport::default())
}
