//! Provider-free installation of the reviewed catalog seed for clean replay.

use anyhow::{bail, Result};

use crate::catalog::projection::seed::{prepare_verified_catalog, seed_prepared_verified_catalog};
use crate::db::{database_urls_equal, AppDb};

pub use crate::catalog::projection::seed::CatalogSeedReport;

pub struct SeedVerifiedCatalogRequest<'request> {
    pub source_database_url: &'request str,
    pub target_database_url: &'request str,
    pub expected_fingerprint_sha256: Option<&'request str>,
    pub apply: bool,
}

pub async fn seed_replay_verified_catalog(
    request: SeedVerifiedCatalogRequest<'_>,
) -> Result<CatalogSeedReport> {
    if database_urls_equal(request.source_database_url, request.target_database_url).await? {
        bail!("catalog seed source and target databases must be different");
    }

    let source = AppDb::connect_diagnostic(request.source_database_url).await?;
    let prepared =
        prepare_verified_catalog(&source, request.expected_fingerprint_sha256, request.apply)
            .await?;

    // Applying migrations initializes a missing SQLite target, so this writable
    // connection must remain strictly after source projection authentication.
    let target = if request.apply {
        AppDb::connect(request.target_database_url).await?
    } else {
        AppDb::connect_diagnostic(request.target_database_url).await?
    };
    seed_prepared_verified_catalog(&prepared, &target).await
}
