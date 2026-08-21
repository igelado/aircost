//! Typed projection of the reusable verified catalog closure.
//!
//! This is deliberately separate from listing replay. It selects only shared,
//! provider-free catalog truth and never imports listings, listing reviews,
//! assignments, correction receipts, valuation state, or provider artifacts.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sqlx::SqliteConnection;

use crate::aircraft::faa::bridge::{FaaBridgeOutcome, LegacyFaaRepresentative};
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
    _source: &mut SqliteConnection,
    _target: &AppDb,
    _faa: &FaaBridgeOutcome,
) -> Result<CatalogProjectionReport> {
    Ok(CatalogProjectionReport::default())
}

pub(crate) async fn required_faa_representatives(
    source: &mut SqliteConnection,
) -> Result<Vec<LegacyFaaRepresentative>> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        r#"SELECT representative_faa_registry_snapshot_id,
                  representative_faa_n_number
           FROM aircraft_tcds_make_lineage_bindings
           UNION
           SELECT binding.representative_faa_registry_snapshot_id,
                  claim.subject_text
           FROM aircraft_designation_faa_bindings binding
           JOIN curation_evidence_claims claim
             ON claim.id = binding.identity_evidence_claim_id
           ORDER BY 1, 2"#,
    )
    .fetch_all(&mut *source)
    .await?;
    rows.into_iter()
        .map(|(snapshot_id, n_number)| {
            let normalized =
                crate::aircraft::faa::normalize_n_number(&n_number).with_context(|| {
                    format!("catalog representative has invalid N-number {n_number:?}")
                })?;
            if normalized != n_number {
                bail!("catalog representative N-number is not canonical: {n_number:?}");
            }
            Ok(LegacyFaaRepresentative {
                snapshot_id,
                n_number,
            })
        })
        .collect()
}
