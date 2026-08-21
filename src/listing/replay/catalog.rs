//! Provider-free installation of the reviewed catalog seed for clean replay.

use anyhow::Result;

use crate::catalog::projection::seed::seed_verified_catalog;
use crate::db::AppDb;

pub use crate::catalog::projection::seed::CatalogSeedReport;

pub struct SeedVerifiedCatalogRequest<'request> {
    pub source: &'request AppDb,
    pub target: &'request AppDb,
    pub expected_fingerprint_sha256: &'request str,
    pub apply: bool,
}

pub async fn seed_replay_verified_catalog(
    request: SeedVerifiedCatalogRequest<'_>,
) -> Result<CatalogSeedReport> {
    seed_verified_catalog(
        request.source,
        request.target,
        request.expected_fingerprint_sha256,
        request.apply,
    )
    .await
}
