//! Database projection of immutable, published aircraft reference profiles.
//!
//! A listing never selects an arbitrary "latest" reference row. The current
//! FAA-backed aircraft assignment, exact model year, registration market and
//! normalized serial must select exactly one published immutable version.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::aircraft::catalog::{
    aircraft_serial_sort_key, normalize_aircraft_serial_retrieval_key,
    AIRCRAFT_SERIAL_SORT_KEY_VERSION,
};
use crate::db::{AppDb, DatabaseBackend};

#[derive(Debug)]
pub enum ReferencePublicationError {
    NotBuilding(i64),
    InvalidDraft(String),
    Database(sqlx::Error),
}

impl std::fmt::Display for ReferencePublicationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotBuilding(version_id) => write!(
                formatter,
                "reference configuration version {version_id} is not building"
            ),
            Self::InvalidDraft(message) => formatter.write_str(message),
            Self::Database(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ReferencePublicationError {}

impl From<sqlx::Error> for ReferencePublicationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReferenceGap {
    pub code: String,
    pub message: String,
}

impl ReferenceGap {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PublishedReferenceConfiguration {
    pub version_id: i64,
    pub configuration_id: i64,
    pub display_name: String,
    pub model_year: i64,
    pub market_codes: Vec<String>,
    pub price_usd: f64,
    pub price_reference_year: i64,
    pub avionics_count: i64,
    pub engine_count: i64,
    pub propeller_count: i64,
    pub feature_count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ListingReferenceStatus {
    pub listing_id: i64,
    pub ready: bool,
    pub published: Option<PublishedReferenceConfiguration>,
    pub gaps: Vec<ReferenceGap>,
    pub building_version_count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceConfigurationIdentityDraft {
    pub aircraft_model_family_id: i64,
    pub aircraft_designation_id: i64,
    pub aircraft_generation_id: Option<i64>,
    pub tier_package_id: Option<i64>,
    pub display_name: String,
    pub approval_decision_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceApplicabilityDraft {
    pub aircraft_market_id: i64,
    pub applies_to_all_serials: bool,
    pub aircraft_serial_number_scheme_id: Option<i64>,
    pub serial_prefix: Option<String>,
    pub serial_from_display: Option<String>,
    pub serial_to_display: Option<String>,
    pub evidence_claim_id: i64,
}

#[derive(Clone, Debug)]
struct CanonicalReferenceApplicability {
    aircraft_market_id: i64,
    applies_to_all_serials: bool,
    aircraft_serial_number_scheme_id: Option<i64>,
    serial_prefix: Option<String>,
    serial_from_display: Option<String>,
    serial_to_display: Option<String>,
    serial_from_sort_key: Option<String>,
    serial_to_sort_key: Option<String>,
    evidence_claim_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferencePriceDraft {
    /// Exact nominal MSRP printed by the cited primary source. This is never
    /// an inflation-adjusted or otherwise normalized value.
    pub direct_cited_amount_usd: f64,
    pub direct_cited_nominal_dollar_year: i64,
    pub evidence_claim_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceDollarNormalizationDraft {
    pub source_nominal_dollar_year: i64,
    pub target_nominal_dollar_year: i64,
    pub official_index_series: String,
    pub source_index_value: f64,
    pub target_index_value: f64,
    pub normalization_factor: f64,
    pub evidence_claim_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceComponentDraft {
    pub catalog_id: i64,
    pub quantity: i64,
    pub included_in_tier: bool,
    pub evidence_claim_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceFeatureValueDraft {
    Boolean(bool),
    Number(f64),
    Text(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceFeatureDraft {
    pub aircraft_feature_definition_id: i64,
    pub value: ReferenceFeatureValueDraft,
    pub evidence_claim_id: i64,
}

/// Minimal normalized output of the grounded Search -> URL Context ->
/// tools-disabled structure/adjudication pipeline. It contains database IDs
/// admitted by approved decisions and exact validated primary-source claims;
/// no provider response, search transcript, or URL-context dossier is stored.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedReferenceVersionDraft {
    pub identity: ReferenceConfigurationIdentityDraft,
    pub model_year: i64,
    pub revision: i64,
    pub supersedes_version_id: Option<i64>,
    pub profile_approval_decision_id: i64,
    pub applicability: Vec<ReferenceApplicabilityDraft>,
    pub price: ReferencePriceDraft,
    pub dollar_normalization: Option<ReferenceDollarNormalizationDraft>,
    pub avionics: Vec<ReferenceComponentDraft>,
    pub engines: Vec<ReferenceComponentDraft>,
    pub propellers: Vec<ReferenceComponentDraft>,
    pub features: Vec<ReferenceFeatureDraft>,
    /// One validated primary claim for the completeness of each fact set.
    pub avionics_set_evidence_claim_id: i64,
    pub engines_set_evidence_claim_id: i64,
    pub propellers_set_evidence_claim_id: i64,
    pub features_set_evidence_claim_id: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NormalizedReferencePrice {
    pub direct_cited_amount_usd: f64,
    pub source_nominal_dollar_year: i64,
    pub target_nominal_dollar_year: i64,
    pub normalization_factor: f64,
    pub normalized_amount_usd: f64,
    pub official_normalization_fact_id: Option<i64>,
    pub official_index_series: Option<String>,
    pub source_index_value: Option<f64>,
    pub target_index_value: Option<f64>,
    pub evidence_claim_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublishedReferenceVersionIds {
    pub configuration_id: i64,
    pub version_id: i64,
}

#[derive(Debug, FromRow)]
struct ListingIdentityRow {
    model_year: i64,
    listing_registration_number: Option<String>,
    listing_serial_number: Option<String>,
    faa_n_number: Option<String>,
    faa_serial_key: Option<String>,
    assignment_is_current: bool,
    aircraft_designation_id: Option<i64>,
    aircraft_generation_id: Option<i64>,
    aircraft_factory_package_id: Option<i64>,
}

#[derive(Clone, Debug, FromRow)]
struct CandidateScopeRow {
    version_id: i64,
    configuration_id: i64,
    display_name: String,
    model_year: i64,
    market_code: String,
    applies_to_all_serials: bool,
    serial_prefix: Option<String>,
    serial_from_sort_key: Option<String>,
    serial_to_sort_key: Option<String>,
    full_price_count: i64,
    price_usd: Option<f64>,
    price_reference_year: Option<i64>,
    avionics_count: i64,
    engine_count: i64,
    propeller_count: i64,
    feature_count: i64,
    avionics_complete: bool,
    engines_complete: bool,
    propellers_complete: bool,
    features_complete: bool,
}

#[cfg(test)]
struct CandidateSnapshotInterlock {
    snapshot_selected: tokio::sync::oneshot::Sender<()>,
    resume: tokio::sync::oneshot::Receiver<()>,
}

#[derive(Debug, FromRow)]
struct DollarNormalizationRow {
    id: i64,
    index_series: String,
    source_index_value: f64,
    target_index_value: f64,
    normalization_factor: f64,
    evidence_claim_id: i64,
}

#[derive(Debug, FromRow)]
struct SerialSchemeRow {
    normalization_version: String,
    validation_pattern: String,
}

macro_rules! query_as_optional {
    ($db:expr, $row:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_optional(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_optional(pool).await
            }
        }
    }};
}

macro_rules! query_as_all {
    ($db:expr, $row:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_all(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_as::<_, $row>(&sql)$(.bind($bind))*.fetch_all(pool).await
            }
        }
    }};
}

macro_rules! query_scalar_one {
    ($db:expr, $ty:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_one(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_one(pool).await
            }
        }
    }};
}

/// Resolve the one published factory profile usable by a listing.
///
/// The `US` market follows from the mandatory N-registration admission policy;
/// `GLOBAL` scopes also apply. If both select different versions, resolution is
/// ambiguous and fails closed.
pub async fn listing_reference_status(
    db: &AppDb,
    listing_id: i64,
) -> Result<ListingReferenceStatus, sqlx::Error> {
    listing_reference_status_impl(
        db,
        listing_id,
        #[cfg(test)]
        None,
    )
    .await
}

async fn listing_reference_status_impl(
    db: &AppDb,
    listing_id: i64,
    #[cfg(test)] interlock: Option<CandidateSnapshotInterlock>,
) -> Result<ListingReferenceStatus, sqlx::Error> {
    let identity = query_as_optional!(
        db,
        ListingIdentityRow,
        r#"
        SELECT listing.model_year,
               listing.registration_number AS listing_registration_number,
               listing.serial_number AS listing_serial_number,
               assignment.faa_n_number,
               faa_aircraft.manufacturer_serial_key AS faa_serial_key,
               CASE WHEN assignment.faa_registry_snapshot_id = (
                 SELECT coverage.snapshot_id
                 FROM faa_registry_coverage coverage
                 JOIN faa_registry_snapshots coverage_snapshot
                   ON coverage_snapshot.id = coverage.snapshot_id
                 WHERE coverage.n_number = assignment.faa_n_number
                 ORDER BY coverage_snapshot.snapshot_date DESC, coverage.snapshot_id DESC
                 LIMIT 1
               ) THEN TRUE ELSE FALSE END AS assignment_is_current,
               assignment.aircraft_designation_id,
               assignment.aircraft_generation_id,
               assignment.aircraft_factory_package_id
        FROM aircraft_sale_listings listing
        LEFT JOIN aircraft_sale_listing_current_identity_assignments current_assignment
          ON current_assignment.aircraft_sale_listing_id = listing.id
        LEFT JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = current_assignment.identity_assignment_id
         AND assignment.aircraft_sale_listing_id = listing.id
        LEFT JOIN faa_registry_aircraft faa_aircraft
          ON faa_aircraft.snapshot_id = assignment.faa_registry_snapshot_id
         AND faa_aircraft.n_number = assignment.faa_n_number
         AND faa_aircraft.source_record_sha256 = assignment.faa_source_record_sha256
        WHERE listing.id = ?
        "#,
        listing_id
    )?;
    let Some(identity) = identity else {
        return Ok(ListingReferenceStatus {
            listing_id,
            ready: false,
            published: None,
            gaps: vec![ReferenceGap::new(
                "listing_not_found",
                "listing does not exist",
            )],
            building_version_count: 0,
        });
    };
    let building_version_count = if let Some(designation_id) = identity.aircraft_designation_id {
        query_scalar_one!(
            db,
            i64,
            r#"
            SELECT COUNT(*)
            FROM aircraft_reference_configuration_versions version
            JOIN aircraft_reference_configurations configuration
              ON configuration.id = version.aircraft_reference_configuration_id
            WHERE configuration.aircraft_designation_id = ?
              AND configuration.aircraft_generation_id IS NOT DISTINCT FROM ?
              AND configuration.tier_package_id IS NOT DISTINCT FROM ?
              AND version.model_year = ?
              AND version.publication_state = 'building'
            "#,
            designation_id,
            identity.aircraft_generation_id,
            identity.aircraft_factory_package_id,
            identity.model_year
        )?
    } else {
        0
    };

    let mut gaps = Vec::new();
    let Some(designation_id) = identity.aircraft_designation_id else {
        gaps.push(ReferenceGap::new(
            "canonical_identity_assignment_missing",
            "listing has no current FAA-backed canonical aircraft assignment",
        ));
        return Ok(status_with_gaps(listing_id, building_version_count, gaps));
    };
    if !identity.assignment_is_current {
        gaps.push(ReferenceGap::new(
            "canonical_identity_assignment_not_current",
            "canonical aircraft assignment is not bound to the newest FAA coverage for its N-number",
        ));
        return Ok(status_with_gaps(listing_id, building_version_count, gaps));
    }
    let Some(faa_n_number) = identity.faa_n_number.as_deref() else {
        gaps.push(ReferenceGap::new(
            "reference_market_unresolved",
            "canonical aircraft assignment has no exact FAA N-number",
        ));
        return Ok(status_with_gaps(listing_id, building_version_count, gaps));
    };
    if identity
        .listing_registration_number
        .as_deref()
        .is_some_and(|value| !value.trim().eq_ignore_ascii_case(faa_n_number))
    {
        gaps.push(ReferenceGap::new(
            "listing_registration_conflicts_with_faa",
            "listing registration conflicts with its current canonical FAA assignment",
        ));
        return Ok(status_with_gaps(listing_id, building_version_count, gaps));
    }
    let Some(serial) = identity
        .faa_serial_key
        .as_deref()
        .map(normalize_aircraft_serial_retrieval_key)
        .filter(|value| !value.is_empty())
    else {
        gaps.push(ReferenceGap::new(
            "reference_serial_unresolved",
            "listing has no normalized FAA serial for reference applicability",
        ));
        return Ok(status_with_gaps(listing_id, building_version_count, gaps));
    };
    if identity
        .listing_serial_number
        .as_deref()
        .map(normalize_aircraft_serial_retrieval_key)
        .is_some_and(|value| value != serial)
    {
        gaps.push(ReferenceGap::new(
            "listing_serial_conflicts_with_faa",
            "listing serial conflicts with its current canonical FAA assignment",
        ));
        return Ok(status_with_gaps(listing_id, building_version_count, gaps));
    }
    let serial_sort_key = aircraft_serial_sort_key(&serial);

    let scopes = query_as_all!(
        db,
        CandidateScopeRow,
        r#"
        SELECT version.id AS version_id,
               configuration.id AS configuration_id,
               configuration.display_name,
               version.model_year,
               market.code AS market_code,
               scope.applies_to_all_serials,
               scope.serial_prefix,
               scope.serial_from_sort_key,
               scope.serial_to_sort_key,
               (SELECT COUNT(*) FROM aircraft_reference_prices price
                JOIN curation_evidence_claims claim
                  ON claim.id = price.evidence_claim_id
                WHERE price.aircraft_reference_configuration_version_id = version.id
                  AND price.price_kind = 'equipped_msrp'
                  AND price.currency = 'USD'
                  AND price.evidence_kind = 'direct_model_year'
                  AND price.configuration_basis = 'full_standard_configuration'
                  AND claim.claim_kind = 'price') AS full_price_count,
               (SELECT price.amount FROM aircraft_reference_prices price
                WHERE price.aircraft_reference_configuration_version_id = version.id
                  AND price.price_kind = 'equipped_msrp'
                  AND price.currency = 'USD'
                  AND price.evidence_kind = 'direct_model_year'
                  AND price.configuration_basis = 'full_standard_configuration'
                ORDER BY price.id LIMIT 1) AS price_usd,
               (SELECT price.price_reference_year
                FROM aircraft_reference_prices price
                WHERE price.aircraft_reference_configuration_version_id = version.id
                  AND price.price_kind = 'equipped_msrp'
                  AND price.currency = 'USD'
                  AND price.evidence_kind = 'direct_model_year'
                  AND price.configuration_basis = 'full_standard_configuration'
                ORDER BY price.id LIMIT 1) AS price_reference_year,
               (SELECT COUNT(*) FROM aircraft_reference_avionics fact
                WHERE fact.aircraft_reference_configuration_version_id = version.id)
                  AS avionics_count,
               (SELECT CAST(COALESCE(SUM(fact.quantity), 0) AS BIGINT)
                FROM aircraft_reference_engines fact
                WHERE fact.aircraft_reference_configuration_version_id = version.id)
                  AS engine_count,
               (SELECT CAST(COALESCE(SUM(fact.quantity), 0) AS BIGINT)
                FROM aircraft_reference_propellers fact
                WHERE fact.aircraft_reference_configuration_version_id = version.id)
                  AS propeller_count,
               (SELECT COUNT(*) FROM aircraft_reference_features fact
                WHERE fact.aircraft_reference_configuration_version_id = version.id)
                  AS feature_count,
               EXISTS (
                 SELECT 1 FROM aircraft_reference_fact_set_attestations attestation
                 WHERE attestation.aircraft_reference_configuration_version_id = version.id
                   AND attestation.fact_set_kind = 'avionics'
               ) AS avionics_complete,
               EXISTS (
                 SELECT 1 FROM aircraft_reference_fact_set_attestations attestation
                 WHERE attestation.aircraft_reference_configuration_version_id = version.id
                   AND attestation.fact_set_kind = 'engines'
               ) AS engines_complete,
               EXISTS (
                 SELECT 1 FROM aircraft_reference_fact_set_attestations attestation
                 WHERE attestation.aircraft_reference_configuration_version_id = version.id
                   AND attestation.fact_set_kind = 'propellers'
               ) AS propellers_complete,
               EXISTS (
                 SELECT 1 FROM aircraft_reference_fact_set_attestations attestation
                 WHERE attestation.aircraft_reference_configuration_version_id = version.id
                   AND attestation.fact_set_kind = 'features'
               ) AS features_complete
        FROM aircraft_reference_configuration_versions version
        JOIN aircraft_reference_configurations configuration
          ON configuration.id = version.aircraft_reference_configuration_id
        JOIN aircraft_reference_applicability_scopes scope
          ON scope.aircraft_reference_configuration_version_id = version.id
        JOIN aircraft_markets market ON market.id = scope.aircraft_market_id
        WHERE configuration.aircraft_designation_id = ?
          AND configuration.aircraft_generation_id IS NOT DISTINCT FROM ?
          AND configuration.tier_package_id IS NOT DISTINCT FROM ?
          AND version.model_year = ?
          AND version.publication_state = 'published'
          AND market.code IN ('GLOBAL', 'US')
        ORDER BY version.id, scope.id
        "#,
        designation_id,
        identity.aircraft_generation_id,
        identity.aircraft_factory_package_id,
        identity.model_year
    )?;
    #[cfg(test)]
    if let Some(interlock) = interlock {
        interlock
            .snapshot_selected
            .send(())
            .expect("reference-status interlock receiver must remain open");
        interlock
            .resume
            .await
            .expect("reference-status interlock sender must remain open");
    }
    let mut candidates = BTreeMap::<i64, (CandidateScopeRow, BTreeSet<String>)>::new();
    for scope in scopes {
        if !scope_matches_serial(&scope, &serial, &serial_sort_key) {
            continue;
        }
        candidates
            .entry(scope.version_id)
            .and_modify(|(_, markets)| {
                markets.insert(scope.market_code.clone());
            })
            .or_insert_with(|| {
                let mut markets = BTreeSet::new();
                markets.insert(scope.market_code.clone());
                (scope, markets)
            });
    }
    if candidates.is_empty() {
        gaps.push(ReferenceGap::new(
            "published_reference_configuration_missing",
            "no published profile matches the canonical identity, model year, market, and serial",
        ));
        return Ok(status_with_gaps(listing_id, building_version_count, gaps));
    }
    if candidates.len() != 1 {
        gaps.push(ReferenceGap::new(
            "published_reference_configuration_ambiguous",
            format!("{} published profiles match this listing", candidates.len()),
        ));
        return Ok(status_with_gaps(listing_id, building_version_count, gaps));
    }
    let (_, (candidate, markets)) = candidates.into_iter().next().expect("one candidate");
    if candidate.full_price_count != 1 {
        gaps.push(ReferenceGap::new(
            "full_standard_configuration_price_missing",
            "published profile must contain exactly one direct USD full-configuration price",
        ));
    }
    if !candidate.engines_complete {
        gaps.push(ReferenceGap::new(
            "factory_engine_configuration_missing",
            "published profile has no factory engine configuration",
        ));
    }
    if !candidate.propellers_complete {
        gaps.push(ReferenceGap::new(
            "factory_propeller_configuration_missing",
            "published profile has no factory propeller configuration",
        ));
    }
    if !candidate.avionics_complete {
        gaps.push(ReferenceGap::new(
            "factory_avionics_configuration_missing",
            "published profile has no curated factory avionics configuration",
        ));
    }
    if !candidate.features_complete {
        gaps.push(ReferenceGap::new(
            "factory_feature_configuration_missing",
            "published profile has no curated material feature configuration",
        ));
    }
    if !gaps.is_empty() {
        return Ok(status_with_gaps(listing_id, building_version_count, gaps));
    }
    Ok(ListingReferenceStatus {
        listing_id,
        ready: true,
        published: Some(PublishedReferenceConfiguration {
            version_id: candidate.version_id,
            configuration_id: candidate.configuration_id,
            display_name: candidate.display_name,
            model_year: candidate.model_year,
            market_codes: markets.into_iter().collect(),
            price_usd: candidate
                .price_usd
                .expect("one complete price has an amount"),
            price_reference_year: candidate
                .price_reference_year
                .expect("one complete price has a reference year"),
            avionics_count: candidate.avionics_count,
            engine_count: candidate.engine_count,
            propeller_count: candidate.propeller_count,
            feature_count: candidate.feature_count,
        }),
        gaps,
        building_version_count,
    })
}

/// Convert a direct cited nominal MSRP into the requested market year's
/// dollars using one immutable, validated regulator-backed index fact.
/// Same-year amounts are identity conversions and need no stored fact.
pub async fn normalized_reference_price(
    db: &AppDb,
    reference: &PublishedReferenceConfiguration,
    target_nominal_dollar_year: i64,
) -> Result<Result<NormalizedReferencePrice, ReferenceGap>, sqlx::Error> {
    if reference.price_reference_year == target_nominal_dollar_year {
        return Ok(Ok(NormalizedReferencePrice {
            direct_cited_amount_usd: reference.price_usd,
            source_nominal_dollar_year: reference.price_reference_year,
            target_nominal_dollar_year,
            normalization_factor: 1.0,
            normalized_amount_usd: reference.price_usd,
            official_normalization_fact_id: None,
            official_index_series: None,
            source_index_value: None,
            target_index_value: None,
            evidence_claim_id: None,
        }));
    }
    let fact = query_as_optional!(
        db,
        DollarNormalizationRow,
        r#"
        SELECT fact.id, fact.index_series, fact.source_index_value,
               fact.target_index_value, fact.normalization_factor,
               fact.evidence_claim_id
        FROM official_dollar_normalization_facts fact
        JOIN curation_evidence_claims claim
          ON claim.id = fact.evidence_claim_id
        JOIN curation_evidence_sources source
          ON source.id = claim.evidence_source_id
        WHERE fact.source_year = ?
          AND fact.target_year = ?
          AND claim.validation_status = 'validated'
          AND claim.claim_kind IN ('price', 'specification')
          AND source.source_tier = 'regulator_primary'
        "#,
        reference.price_reference_year,
        target_nominal_dollar_year
    )?;
    let Some(fact) = fact else {
        return Ok(Err(ReferenceGap::new(
            "reference_price_dollar_normalization_missing",
            format!(
                "published direct factory MSRP is expressed in {} nominal dollars, but the valuation market year is {}; no validated official dollar-normalization fact is published",
                reference.price_reference_year, target_nominal_dollar_year
            ),
        )));
    };
    let normalized_amount_usd = reference.price_usd * fact.normalization_factor;
    if !normalized_amount_usd.is_finite() || normalized_amount_usd <= 0.0 {
        return Ok(Err(ReferenceGap::new(
            "reference_price_dollar_normalization_invalid",
            "the approved official dollar-normalization fact produced an invalid amount",
        )));
    }
    Ok(Ok(NormalizedReferencePrice {
        direct_cited_amount_usd: reference.price_usd,
        source_nominal_dollar_year: reference.price_reference_year,
        target_nominal_dollar_year,
        normalization_factor: fact.normalization_factor,
        normalized_amount_usd,
        official_normalization_fact_id: Some(fact.id),
        official_index_series: Some(fact.index_series),
        source_index_value: Some(fact.source_index_value),
        target_index_value: Some(fact.target_index_value),
        evidence_claim_id: Some(fact.evidence_claim_id),
    }))
}

/// Atomically publish one fully assembled immutable version.
///
/// All facts must already have been admitted through the existing approved
/// evidence/decision foreign keys. Database publication triggers recheck the
/// exact-year full-configuration price, all four completed fact sets, primary
/// evidence, approved component identities and non-overlapping applicability.
/// A failed check rolls back without changing the building version.
pub async fn publish_reference_version(
    db: &AppDb,
    version_id: i64,
) -> Result<(), ReferencePublicationError> {
    let sqlite_select = db.sql(
        r#"
        SELECT aircraft_reference_configuration_id, model_year, revision,
               supersedes_version_id
        FROM aircraft_reference_configuration_versions
        WHERE id = ? AND publication_state = 'building'
        "#,
    );
    let postgres_select = db.sql(
        r#"
        SELECT aircraft_reference_configuration_id, model_year, revision,
               supersedes_version_id
        FROM aircraft_reference_configuration_versions
        WHERE id = ? AND publication_state = 'building'
        FOR UPDATE
        "#,
    );
    let supersede = db.sql(
        r#"
        UPDATE aircraft_reference_configuration_versions
        SET publication_state = 'superseded', superseded_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND aircraft_reference_configuration_id = ?
          AND model_year = ?
          AND publication_state = 'published'
          AND revision = (? - 1)
        "#,
    );
    let publish = db.sql(
        r#"
        UPDATE aircraft_reference_configuration_versions
        SET publication_state = 'published', published_at = CURRENT_TIMESTAMP
        WHERE id = ? AND publication_state = 'building'
        "#,
    );

    macro_rules! publish_locked {
        ($transaction:expr, $select:expr) => {{
            let version = sqlx::query_as::<_, (i64, i64, i64, Option<i64>)>($select)
                .bind(version_id)
                .fetch_optional(&mut **$transaction)
                .await?;
            let Some((configuration_id, model_year, revision, predecessor_id)) = version else {
                return Err(ReferencePublicationError::NotBuilding(version_id));
            };
            if let Some(predecessor_id) = predecessor_id {
                let affected = sqlx::query(&supersede)
                    .bind(predecessor_id)
                    .bind(configuration_id)
                    .bind(model_year)
                    .bind(revision)
                    .execute(&mut **$transaction)
                    .await?
                    .rows_affected();
                if affected != 1 {
                    return Err(ReferencePublicationError::InvalidDraft(format!(
                        "reference version {version_id} predecessor {predecessor_id} is not the published lower revision of the same configuration and model year"
                    )));
                }
            }
            if sqlx::query(&publish)
                .bind(version_id)
                .execute(&mut **$transaction)
                .await?
                .rows_affected()
                != 1
            {
                return Err(ReferencePublicationError::NotBuilding(version_id));
            }
            Ok::<(), ReferencePublicationError>(())
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            publish_locked!(&mut transaction, &sqlite_select)?;
            transaction.commit().await?;
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await?;
            publish_locked!(&mut transaction, &postgres_select)?;
            transaction.commit().await?;
        }
    }
    Ok(())
}

/// Atomically create/reuse the shared hierarchy configuration, assemble one
/// immutable exact-year version from normalized approved facts, and publish
/// it through the database completeness/overlap gates.
pub async fn assemble_and_publish_reference_version(
    db: &AppDb,
    draft: &ApprovedReferenceVersionDraft,
) -> Result<PublishedReferenceVersionIds, ReferencePublicationError> {
    assemble_reference_version(db, draft, true).await
}

/// Exercise the exact production assembly and publication gates in a database
/// transaction, then roll it back. This is the dry-run path used by the admin
/// workflow; it validates every decision, catalog and evidence foreign key and
/// every publication invariant without retaining a building or published row.
pub async fn preview_reference_version(
    db: &AppDb,
    draft: &ApprovedReferenceVersionDraft,
) -> Result<PublishedReferenceVersionIds, ReferencePublicationError> {
    assemble_reference_version(db, draft, false).await
}

async fn assemble_reference_version(
    db: &AppDb,
    draft: &ApprovedReferenceVersionDraft,
    apply: bool,
) -> Result<PublishedReferenceVersionIds, ReferencePublicationError> {
    validate_write_draft(draft)?;
    let applicability = canonicalize_applicability(db, draft).await?;
    let select_configuration = db.sql(
        r#"
        SELECT id FROM aircraft_reference_configurations
        WHERE aircraft_model_family_id = ?
          AND aircraft_designation_id = ?
          AND aircraft_generation_id IS NOT DISTINCT FROM ?
          AND tier_package_id IS NOT DISTINCT FROM ?
        "#,
    );
    let insert_configuration = db.sql(
        r#"
        INSERT INTO aircraft_reference_configurations (
          aircraft_model_family_id, aircraft_designation_id,
          aircraft_generation_id, tier_package_id, configuration_kind,
          display_name, approval_decision_id
        ) VALUES (?, ?, ?, ?, ?, ?, ?)
        RETURNING id
        "#,
    );
    let insert_version = db.sql(
        r#"
        INSERT INTO aircraft_reference_configuration_versions (
          aircraft_reference_configuration_id, model_year, revision,
          supersedes_version_id, approval_decision_id
        ) VALUES (?, ?, ?, ?, ?)
        RETURNING id
        "#,
    );
    let insert_scope = db.sql(
        r#"
        INSERT INTO aircraft_reference_applicability_scopes (
          aircraft_reference_configuration_version_id, aircraft_market_id,
          applies_to_all_serials, aircraft_serial_number_scheme_id,
          serial_prefix, serial_from_display, serial_to_display,
          serial_from_sort_key, serial_to_sort_key, evidence_claim_id
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    );
    let insert_price = db.sql(
        r#"
        INSERT INTO aircraft_reference_prices (
          aircraft_reference_configuration_version_id, price_kind, amount,
          currency, price_reference_year, configuration_basis,
          evidence_kind, evidence_claim_id
        ) VALUES (?, 'equipped_msrp', ?, 'USD', ?,
                  'full_standard_configuration', 'direct_model_year', ?)
        "#,
    );
    let insert_avionics = db.sql(
        "INSERT INTO aircraft_reference_avionics (aircraft_reference_configuration_version_id, avionics_model_id, quantity, equipment_role, evidence_claim_id) VALUES (?, ?, ?, ?, ?)",
    );
    let insert_engine = db.sql(
        "INSERT INTO aircraft_reference_engines (aircraft_reference_configuration_version_id, aircraft_engine_catalog_model_id, quantity, equipment_role, evidence_claim_id) VALUES (?, ?, ?, ?, ?)",
    );
    let insert_propeller = db.sql(
        "INSERT INTO aircraft_reference_propellers (aircraft_reference_configuration_version_id, aircraft_propeller_catalog_model_id, quantity, equipment_role, evidence_claim_id) VALUES (?, ?, ?, ?, ?)",
    );
    let insert_feature = db.sql(
        "INSERT INTO aircraft_reference_features (aircraft_reference_configuration_version_id, aircraft_feature_definition_id, boolean_value, number_value, text_value, evidence_claim_id) VALUES (?, ?, ?, ?, ?, ?)",
    );
    let insert_attestation = db.sql(
        "INSERT INTO aircraft_reference_fact_set_attestations (aircraft_reference_configuration_version_id, fact_set_kind, evidence_claim_id) VALUES (?, ?, ?)",
    );
    let insert_normalization = db.sql(
        r#"
        INSERT INTO official_dollar_normalization_facts (
          source_year, target_year, index_series, source_index_value,
          target_index_value, normalization_factor, evidence_claim_id
        ) VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (source_year, target_year) DO NOTHING
        "#,
    );
    let select_normalization = db.sql(
        r#"
        SELECT id, index_series, source_index_value, target_index_value,
               normalization_factor, evidence_claim_id
        FROM official_dollar_normalization_facts
        WHERE source_year = ? AND target_year = ?
        "#,
    );
    let publish = db.sql(
        "UPDATE aircraft_reference_configuration_versions SET publication_state = 'published', published_at = CURRENT_TIMESTAMP WHERE id = ? AND publication_state = 'building'",
    );
    let supersede = db.sql(
        r#"
        UPDATE aircraft_reference_configuration_versions
        SET publication_state = 'superseded', superseded_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND aircraft_reference_configuration_id = ?
          AND model_year = ?
          AND publication_state = 'published'
          AND revision = (? - 1)
        "#,
    );

    macro_rules! assemble {
        ($transaction:expr) => {{
            let mut configuration_id = sqlx::query_scalar::<_, i64>(&select_configuration)
                .bind(draft.identity.aircraft_model_family_id)
                .bind(draft.identity.aircraft_designation_id)
                .bind(draft.identity.aircraft_generation_id)
                .bind(draft.identity.tier_package_id)
                .fetch_optional(&mut **$transaction)
                .await?;
            if configuration_id.is_none() {
                configuration_id = Some(
                    sqlx::query_scalar::<_, i64>(&insert_configuration)
                        .bind(draft.identity.aircraft_model_family_id)
                        .bind(draft.identity.aircraft_designation_id)
                        .bind(draft.identity.aircraft_generation_id)
                        .bind(draft.identity.tier_package_id)
                        .bind(if draft.identity.tier_package_id.is_some() {
                            "tier"
                        } else {
                            "base"
                        })
                        .bind(draft.identity.display_name.trim())
                        .bind(draft.identity.approval_decision_id)
                        .fetch_one(&mut **$transaction)
                        .await?,
                );
            }
            let configuration_id = configuration_id.expect("configuration selected or inserted");
            if let Some(normalization) = &draft.dollar_normalization {
                sqlx::query(&insert_normalization)
                    .bind(normalization.source_nominal_dollar_year)
                    .bind(normalization.target_nominal_dollar_year)
                    .bind(normalization.official_index_series.trim())
                    .bind(normalization.source_index_value)
                    .bind(normalization.target_index_value)
                    .bind(normalization.normalization_factor)
                    .bind(normalization.evidence_claim_id)
                    .execute(&mut **$transaction)
                    .await?;
                let stored = sqlx::query_as::<_, DollarNormalizationRow>(&select_normalization)
                    .bind(normalization.source_nominal_dollar_year)
                    .bind(normalization.target_nominal_dollar_year)
                    .fetch_one(&mut **$transaction)
                    .await?;
                if stored.index_series != normalization.official_index_series.trim()
                    || !normalization_values_match(
                        stored.source_index_value,
                        normalization.source_index_value,
                    )
                    || !normalization_values_match(
                        stored.target_index_value,
                        normalization.target_index_value,
                    )
                    || !normalization_values_match(
                        stored.normalization_factor,
                        normalization.normalization_factor,
                    )
                    || stored.evidence_claim_id != normalization.evidence_claim_id
                {
                    return Err(ReferencePublicationError::InvalidDraft(format!(
                        "official dollar-normalization fact for {} to {} conflicts with the immutable published fact",
                        normalization.source_nominal_dollar_year,
                        normalization.target_nominal_dollar_year
                    )));
                }
            }
            let version_id = sqlx::query_scalar::<_, i64>(&insert_version)
                .bind(configuration_id)
                .bind(draft.model_year)
                .bind(draft.revision)
                .bind(draft.supersedes_version_id)
                .bind(draft.profile_approval_decision_id)
                .fetch_one(&mut **$transaction)
                .await?;
            for scope in &applicability {
                sqlx::query(&insert_scope)
                    .bind(version_id)
                    .bind(scope.aircraft_market_id)
                    .bind(scope.applies_to_all_serials)
                    .bind(scope.aircraft_serial_number_scheme_id)
                    .bind(scope.serial_prefix.as_deref())
                    .bind(scope.serial_from_display.as_deref())
                    .bind(scope.serial_to_display.as_deref())
                    .bind(scope.serial_from_sort_key.as_deref())
                    .bind(scope.serial_to_sort_key.as_deref())
                    .bind(scope.evidence_claim_id)
                    .execute(&mut **$transaction)
                    .await?;
            }
            sqlx::query(&insert_price)
                .bind(version_id)
                .bind(draft.price.direct_cited_amount_usd)
                .bind(draft.price.direct_cited_nominal_dollar_year)
                .bind(draft.price.evidence_claim_id)
                .execute(&mut **$transaction)
                .await?;
            for (statement, components) in [
                (&insert_avionics, &draft.avionics),
                (&insert_engine, &draft.engines),
                (&insert_propeller, &draft.propellers),
            ] {
                for component in components {
                    sqlx::query(statement)
                        .bind(version_id)
                        .bind(component.catalog_id)
                        .bind(component.quantity)
                        .bind(if component.included_in_tier {
                            "included_in_tier"
                        } else {
                            "standard"
                        })
                        .bind(component.evidence_claim_id)
                        .execute(&mut **$transaction)
                        .await?;
                }
            }
            for feature in &draft.features {
                let (boolean_value, number_value, text_value) = match &feature.value {
                    ReferenceFeatureValueDraft::Boolean(value) => (Some(*value), None, None),
                    ReferenceFeatureValueDraft::Number(value) => (None, Some(*value), None),
                    ReferenceFeatureValueDraft::Text(value) => (None, None, Some(value.as_str())),
                };
                sqlx::query(&insert_feature)
                    .bind(version_id)
                    .bind(feature.aircraft_feature_definition_id)
                    .bind(boolean_value)
                    .bind(number_value)
                    .bind(text_value)
                    .bind(feature.evidence_claim_id)
                    .execute(&mut **$transaction)
                    .await?;
            }
            for (kind, claim_id) in [
                ("avionics", draft.avionics_set_evidence_claim_id),
                ("engines", draft.engines_set_evidence_claim_id),
                ("propellers", draft.propellers_set_evidence_claim_id),
                ("features", draft.features_set_evidence_claim_id),
            ] {
                sqlx::query(&insert_attestation)
                    .bind(version_id)
                    .bind(kind)
                    .bind(claim_id)
                    .execute(&mut **$transaction)
                    .await?;
            }
            if let Some(predecessor_id) = draft.supersedes_version_id {
                let affected = sqlx::query(&supersede)
                    .bind(predecessor_id)
                    .bind(configuration_id)
                    .bind(draft.model_year)
                    .bind(draft.revision)
                    .execute(&mut **$transaction)
                    .await?
                    .rows_affected();
                if affected != 1 {
                    return Err(ReferencePublicationError::InvalidDraft(format!(
                        "reference predecessor {predecessor_id} is not the published lower revision of configuration {configuration_id} for model year {}",
                        draft.model_year
                    )));
                }
            }
            if sqlx::query(&publish)
                .bind(version_id)
                .execute(&mut **$transaction)
                .await?
                .rows_affected()
                != 1
            {
                return Err(ReferencePublicationError::NotBuilding(version_id));
            }
            Ok::<PublishedReferenceVersionIds, ReferencePublicationError>(
                PublishedReferenceVersionIds {
                    configuration_id,
                    version_id,
                },
            )
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            let ids = assemble!(&mut transaction)?;
            if apply {
                transaction.commit().await?;
            } else {
                transaction.rollback().await?;
            }
            Ok(ids)
        }
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await?;
            let ids = assemble!(&mut transaction)?;
            if apply {
                transaction.commit().await?;
            } else {
                transaction.rollback().await?;
            }
            Ok(ids)
        }
    }
}

async fn canonicalize_applicability(
    db: &AppDb,
    draft: &ApprovedReferenceVersionDraft,
) -> Result<Vec<CanonicalReferenceApplicability>, ReferencePublicationError> {
    let mut canonical = Vec::with_capacity(draft.applicability.len());
    for scope in &draft.applicability {
        if scope.aircraft_market_id <= 0 || scope.evidence_claim_id <= 0 {
            return Err(ReferencePublicationError::InvalidDraft(
                "reference applicability requires positive market and evidence IDs".to_string(),
            ));
        }
        if scope.applies_to_all_serials {
            if scope.aircraft_serial_number_scheme_id.is_some()
                || scope.serial_prefix.is_some()
                || scope.serial_from_display.is_some()
                || scope.serial_to_display.is_some()
            {
                return Err(ReferencePublicationError::InvalidDraft(
                    "all-serial applicability cannot carry a serial scheme, prefix, or range"
                        .to_string(),
                ));
            }
            canonical.push(CanonicalReferenceApplicability {
                aircraft_market_id: scope.aircraft_market_id,
                applies_to_all_serials: true,
                aircraft_serial_number_scheme_id: None,
                serial_prefix: None,
                serial_from_display: None,
                serial_to_display: None,
                serial_from_sort_key: None,
                serial_to_sort_key: None,
                evidence_claim_id: scope.evidence_claim_id,
            });
            continue;
        }
        let scheme_id = scope.aircraft_serial_number_scheme_id.ok_or_else(|| {
            ReferencePublicationError::InvalidDraft(
                "bounded serial applicability requires a declared serial scheme".to_string(),
            )
        })?;
        let scheme = query_as_optional!(
            db,
            SerialSchemeRow,
            r#"
            SELECT scheme.normalization_version, scheme.validation_pattern
            FROM aircraft_serial_number_schemes scheme
            JOIN aircraft_model_families family
              ON family.aircraft_make_id = scheme.aircraft_make_id
            WHERE scheme.id = ? AND family.id = ?
            "#,
            scheme_id,
            draft.identity.aircraft_model_family_id
        )?
        .ok_or_else(|| {
            ReferencePublicationError::InvalidDraft(format!(
                "serial scheme {scheme_id} does not belong to the reference aircraft make"
            ))
        })?;
        if scheme.normalization_version != AIRCRAFT_SERIAL_SORT_KEY_VERSION {
            return Err(ReferencePublicationError::InvalidDraft(format!(
                "serial scheme {scheme_id} uses unsupported normalization version {}",
                scheme.normalization_version
            )));
        }
        let validation = Regex::new(&scheme.validation_pattern).map_err(|error| {
            ReferencePublicationError::InvalidDraft(format!(
                "serial scheme {scheme_id} has an invalid validation pattern: {error}"
            ))
        })?;
        let from_display = scope
            .serial_from_display
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ReferencePublicationError::InvalidDraft(
                    "bounded serial applicability requires a lower display value".to_string(),
                )
            })?;
        let to_display = scope
            .serial_to_display
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ReferencePublicationError::InvalidDraft(
                    "bounded serial applicability requires an upper display value".to_string(),
                )
            })?;
        let from_serial = normalize_aircraft_serial_retrieval_key(from_display);
        let to_serial = normalize_aircraft_serial_retrieval_key(to_display);
        if from_serial.is_empty() || to_serial.is_empty() {
            return Err(ReferencePublicationError::InvalidDraft(
                "serial range display values must normalize to alphanumeric serials".to_string(),
            ));
        }
        let full_match = |value: &str| {
            validation
                .find(value)
                .is_some_and(|matched| matched.start() == 0 && matched.end() == value.len())
        };
        if !full_match(&from_serial) || !full_match(&to_serial) {
            return Err(ReferencePublicationError::InvalidDraft(format!(
                "serial range does not satisfy declared scheme {scheme_id}"
            )));
        }
        let prefix = scope
            .serial_prefix
            .as_deref()
            .map(normalize_aircraft_serial_retrieval_key)
            .filter(|value| !value.is_empty());
        let from_sort_key = aircraft_serial_sort_key(&from_serial);
        let to_sort_key = aircraft_serial_sort_key(&to_serial);
        if prefix.as_deref().is_some_and(|prefix| {
            !from_serial.starts_with(prefix) || !to_serial.starts_with(prefix)
        }) || from_sort_key > to_sort_key
        {
            return Err(ReferencePublicationError::InvalidDraft(
                "serial prefix/range is inconsistent or reversed after canonicalization"
                    .to_string(),
            ));
        }
        canonical.push(CanonicalReferenceApplicability {
            aircraft_market_id: scope.aircraft_market_id,
            applies_to_all_serials: false,
            aircraft_serial_number_scheme_id: Some(scheme_id),
            serial_prefix: prefix,
            serial_from_display: Some(from_serial),
            serial_to_display: Some(to_serial),
            serial_from_sort_key: Some(from_sort_key),
            serial_to_sort_key: Some(to_sort_key),
            evidence_claim_id: scope.evidence_claim_id,
        });
    }
    Ok(canonical)
}

fn normalization_values_match(left: f64, right: f64) -> bool {
    (left - right).abs() <= 0.000000001
}

fn validate_write_draft(
    draft: &ApprovedReferenceVersionDraft,
) -> Result<(), ReferencePublicationError> {
    let ids = [
        draft.identity.aircraft_model_family_id,
        draft.identity.aircraft_designation_id,
        draft.identity.approval_decision_id,
        draft.profile_approval_decision_id,
        draft.price.evidence_claim_id,
        draft.avionics_set_evidence_claim_id,
        draft.engines_set_evidence_claim_id,
        draft.propellers_set_evidence_claim_id,
        draft.features_set_evidence_claim_id,
    ];
    if ids.into_iter().any(|id| id <= 0)
        || draft.identity.display_name.trim().is_empty()
        || !(1900..=2200).contains(&draft.model_year)
        || !(1900..=2200).contains(&draft.price.direct_cited_nominal_dollar_year)
        || draft.revision < 1
        || (draft.revision == 1) != draft.supersedes_version_id.is_none()
        || draft
            .supersedes_version_id
            .is_some_and(|predecessor_id| predecessor_id <= 0)
        || !draft.price.direct_cited_amount_usd.is_finite()
        || draft.price.direct_cited_amount_usd <= 0.0
        || draft.applicability.is_empty()
    {
        return Err(ReferencePublicationError::InvalidDraft(
            "reference draft has invalid identity, year, revision, price, or applicability"
                .to_string(),
        ));
    }
    if draft
        .avionics
        .iter()
        .chain(&draft.engines)
        .chain(&draft.propellers)
        .any(|component| {
            component.catalog_id <= 0
                || component.quantity <= 0
                || component.evidence_claim_id <= 0
        })
        || draft.features.iter().any(|feature| {
            feature.aircraft_feature_definition_id <= 0
                || feature.evidence_claim_id <= 0
                || matches!(&feature.value, ReferenceFeatureValueDraft::Text(value) if value.trim().is_empty())
                || matches!(&feature.value, ReferenceFeatureValueDraft::Number(value) if !value.is_finite())
        })
    {
        return Err(ReferencePublicationError::InvalidDraft(
            "reference draft has an invalid component or feature fact".to_string(),
        ));
    }
    if let Some(normalization) = &draft.dollar_normalization {
        let derived_factor = normalization.target_index_value / normalization.source_index_value;
        if normalization.source_nominal_dollar_year != draft.price.direct_cited_nominal_dollar_year
            || !(1900..=2200).contains(&normalization.source_nominal_dollar_year)
            || !(1900..=2200).contains(&normalization.target_nominal_dollar_year)
            || normalization.source_nominal_dollar_year == normalization.target_nominal_dollar_year
            || normalization.official_index_series.trim().is_empty()
            || normalization.evidence_claim_id <= 0
            || !normalization.source_index_value.is_finite()
            || normalization.source_index_value <= 0.0
            || !normalization.target_index_value.is_finite()
            || normalization.target_index_value <= 0.0
            || !normalization.normalization_factor.is_finite()
            || normalization.normalization_factor <= 0.0
            || !normalization_values_match(normalization.normalization_factor, derived_factor)
        {
            return Err(ReferencePublicationError::InvalidDraft(
                "dollar normalization must use the cited source year, a distinct valid target year, positive official index values, their exact factor, and validated evidence"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn status_with_gaps(
    listing_id: i64,
    building_version_count: i64,
    gaps: Vec<ReferenceGap>,
) -> ListingReferenceStatus {
    ListingReferenceStatus {
        listing_id,
        ready: false,
        published: None,
        gaps,
        building_version_count,
    }
}

fn scope_matches_serial(
    scope: &CandidateScopeRow,
    normalized_serial: &str,
    serial_sort_key: &str,
) -> bool {
    if scope.applies_to_all_serials {
        return true;
    }
    let prefix_matches = scope
        .serial_prefix
        .as_deref()
        .is_none_or(|prefix| normalized_serial.starts_with(prefix));
    prefix_matches
        && scope
            .serial_from_sort_key
            .as_deref()
            .is_some_and(|from| serial_sort_key >= from)
        && scope
            .serial_to_sort_key
            .as_deref()
            .is_some_and(|to| serial_sort_key <= to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avionics::manufacturer::ensure_test_manufacturer_identity;

    fn scope(version_id: i64, from: Option<&str>, to: Option<&str>) -> CandidateScopeRow {
        CandidateScopeRow {
            version_id,
            configuration_id: 1,
            display_name: "Test profile".to_string(),
            model_year: 2020,
            market_code: "US".to_string(),
            applies_to_all_serials: from.is_none(),
            serial_prefix: None,
            serial_from_sort_key: from.map(aircraft_serial_sort_key),
            serial_to_sort_key: to.map(aircraft_serial_sort_key),
            full_price_count: 1,
            price_usd: Some(500_000.0),
            price_reference_year: Some(2020),
            avionics_count: 0,
            engine_count: 1,
            propeller_count: 1,
            feature_count: 0,
            avionics_complete: true,
            engines_complete: true,
            propellers_complete: true,
            features_complete: true,
        }
    }

    fn matches(scope: &CandidateScopeRow, serial: &str) -> bool {
        let normalized = normalize_aircraft_serial_retrieval_key(serial);
        scope_matches_serial(scope, &normalized, &aircraft_serial_sort_key(&normalized))
    }

    #[test]
    fn serial_applicability_uses_inclusive_natural_order() {
        let bounded = scope(1, Some("SR-100"), Some("SR-1000"));
        assert!(matches(&bounded, "SR100"));
        assert!(matches(&bounded, "SR999"));
        assert!(matches(&bounded, "SR1000"));
        assert!(!matches(&bounded, "SR9"));
        assert!(!matches(&bounded, "SR1001"));

        let variable_width = scope(1, Some("SR-9"), Some("SR-100"));
        assert!(matches(&variable_width, "SR9"));
        assert!(matches(&variable_width, "SR10"));
        assert!(matches(&variable_width, "SR100"));
    }

    #[test]
    fn different_matching_versions_remain_ambiguous() {
        let scopes = [scope(1, None, None), scope(2, None, None)];
        let matching = scopes
            .iter()
            .filter(|scope| matches(scope, "SR150"))
            .map(|scope| scope.version_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(matching, BTreeSet::from([1, 2]));
    }

    #[tokio::test]
    async fn listing_reference_status_keeps_candidate_and_facts_snapshot_coherent_during_publish() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("reference-status-race.sqlite3");
        let database_url = format!("sqlite://{}", database_path.display());
        let db = AppDb::connect(&database_url).await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::raw_sql(include_str!(
            "../../../tests/schema/aircraft_reference_catalog.sqlite.sql"
        ))
        .execute(pool)
        .await
        .unwrap();
        let predecessor_price: f64 = sqlx::query_scalar(
            "SELECT amount FROM aircraft_reference_prices \
             WHERE aircraft_reference_configuration_version_id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();

        let replacement_decision_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_identity_decisions (
              resolution_case_id, entity_kind, decision_action, decision_status,
              decision_payload_json, deterministic_validation_json,
              deterministic_validation_passed, rationale, decided_at
            ) VALUES (
              1, 'reference_profile', 'approve_new', 'approved', '{}', '{}',
              TRUE, 'race-safe replacement', '2026-08-21'
            ) RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_identity_decision_claims \
             (decision_id, evidence_claim_id, evidence_role) \
             VALUES (?, 1, 'identity')",
        )
        .bind(replacement_decision_id)
        .execute(pool)
        .await
        .unwrap();
        let replacement_version_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_reference_configuration_versions (
              aircraft_reference_configuration_id, model_year, revision,
              supersedes_version_id, approval_decision_id
            ) VALUES (1, 2020, 2, 1, ?) RETURNING id
            "#,
        )
        .bind(replacement_decision_id)
        .fetch_one(pool)
        .await
        .unwrap();
        for statement in [
            "INSERT INTO aircraft_reference_applicability_scopes (aircraft_reference_configuration_version_id, aircraft_market_id, applies_to_all_serials, evidence_claim_id) SELECT ?, aircraft_market_id, applies_to_all_serials, evidence_claim_id FROM aircraft_reference_applicability_scopes WHERE aircraft_reference_configuration_version_id = 1",
            "INSERT INTO aircraft_reference_prices (aircraft_reference_configuration_version_id, price_kind, amount, currency, price_reference_year, configuration_basis, evidence_kind, evidence_claim_id) SELECT ?, price_kind, amount + 1000, currency, price_reference_year, configuration_basis, evidence_kind, evidence_claim_id FROM aircraft_reference_prices WHERE aircraft_reference_configuration_version_id = 1",
            "INSERT INTO aircraft_reference_engines (aircraft_reference_configuration_version_id, aircraft_engine_catalog_model_id, quantity, equipment_role, evidence_claim_id) SELECT ?, aircraft_engine_catalog_model_id, quantity, equipment_role, evidence_claim_id FROM aircraft_reference_engines WHERE aircraft_reference_configuration_version_id = 1",
            "INSERT INTO aircraft_reference_propellers (aircraft_reference_configuration_version_id, aircraft_propeller_catalog_model_id, quantity, equipment_role, evidence_claim_id) SELECT ?, aircraft_propeller_catalog_model_id, quantity, equipment_role, evidence_claim_id FROM aircraft_reference_propellers WHERE aircraft_reference_configuration_version_id = 1",
            "INSERT INTO aircraft_reference_fact_set_attestations (aircraft_reference_configuration_version_id, fact_set_kind, evidence_claim_id) SELECT ?, fact_set_kind, evidence_claim_id FROM aircraft_reference_fact_set_attestations WHERE aircraft_reference_configuration_version_id = 1",
        ] {
            sqlx::query(statement)
                .bind(replacement_version_id)
                .execute(pool)
                .await
                .unwrap();
        }

        sqlx::raw_sql(
            r#"
            INSERT INTO curation_evidence_sources (
              source_url, source_title, source_domain, source_tier,
              content_sha256, retrieved_at
            ) VALUES (
              'https://www.faa.gov/reference-status-race.zip',
              'FAA reference status race fixture', 'faa.gov',
              'regulator_primary',
              '0000000000000000000000000000000000000000000000000000000000000000',
              '2026-08-21'
            );
            INSERT INTO faa_registry_snapshots (
              evidence_source_id, snapshot_date, source_url, archive_sha256,
              source_manifest_sha256, target_set_sha256,
              master_member_name, master_member_sha256,
              aircraft_member_name, aircraft_member_sha256,
              engine_member_name, engine_member_sha256, record_hash_domain
            ) SELECT id, '2026-08-21', source_url, content_sha256,
              '1111111111111111111111111111111111111111111111111111111111111111',
              '2222222222222222222222222222222222222222222222222222222222222222',
              'MASTER.txt',
              '3333333333333333333333333333333333333333333333333333333333333333',
              'ACFTREF.txt',
              '4444444444444444444444444444444444444444444444444444444444444444',
              'ENGINE.txt',
              '5555555555555555555555555555555555555555555555555555555555555555',
              'aircost-faa-master-retained-aircraft-projection-v1'
            FROM curation_evidence_sources
            WHERE source_title = 'FAA reference status race fixture';
            INSERT INTO faa_registry_aircraft (
              snapshot_id, n_number, manufacturer_serial_raw,
              manufacturer_serial_key, aircraft_code, year_manufactured,
              source_record_sha256
            ) VALUES (
              1, 'N123AB', 'SR100', 'SR100', 'SR22', 2020,
              'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
            );
            INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status)
            VALUES (1, 'N123AB', 'matched');
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, registration_number, serial_number,
              airframe_hours
            ) SELECT placeholder.aircraft_model_variant_id, user.id,
                'https://listing.test/reference-status-race', 2020, 500000,
                'N123AB', 'SR100', 100
              FROM aircraft_sale_listing_pending_compatibility_placeholder placeholder
              JOIN users user ON user.auth_subject = 'schema-test'
              WHERE placeholder.singleton_id = 1;
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        let assignment_trigger_names = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_schema \
             WHERE type = 'trigger' \
               AND tbl_name IN ( \
                 'aircraft_sale_listing_identity_assignments', \
                 'aircraft_sale_listing_current_identity_assignments' \
               )",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        for trigger_name in assignment_trigger_names {
            let quoted_name = trigger_name.replace('"', "\"\"");
            sqlx::query(&format!("DROP TRIGGER \"{quoted_name}\""))
                .execute(pool)
                .await
                .unwrap();
        }
        sqlx::raw_sql(
            r#"
            INSERT INTO aircraft_sale_listing_identity_assignments (
              aircraft_sale_listing_id, aircraft_make_id,
              aircraft_model_family_id, aircraft_designation_id,
              aircraft_generation_id, aircraft_factory_package_id,
              identity_decision_id, identity_evidence_claim_id,
              faa_registry_snapshot_id, faa_n_number, faa_source_record_sha256
            ) VALUES (
              1, 1, 1, 1, 1, 1, 3, 1, 1, 'N123AB',
              'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
            );
            INSERT INTO aircraft_sale_listing_current_identity_assignments (
              aircraft_sale_listing_id, identity_assignment_id
            ) VALUES (1, 1);
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let (snapshot_selected, wait_for_snapshot) = tokio::sync::oneshot::channel();
        let (resume, wait_for_publish) = tokio::sync::oneshot::channel();
        let status_db = db.clone();
        let status_task = tokio::spawn(async move {
            listing_reference_status_impl(
                &status_db,
                1,
                Some(CandidateSnapshotInterlock {
                    snapshot_selected,
                    resume: wait_for_publish,
                }),
            )
            .await
        });
        wait_for_snapshot.await.unwrap();
        publish_reference_version(&db, replacement_version_id)
            .await
            .unwrap();
        resume.send(()).unwrap();

        let during_publish = status_task.await.unwrap().unwrap();
        assert!(during_publish.ready, "{:?}", during_publish.gaps);
        let during_publish_profile = during_publish.published.unwrap();
        assert_eq!(during_publish_profile.version_id, 1);
        assert_eq!(during_publish_profile.price_usd, predecessor_price);
        let after_publish = listing_reference_status(&db, 1).await.unwrap();
        assert!(after_publish.ready, "{:?}", after_publish.gaps);
        let after_publish_profile = after_publish.published.unwrap();
        assert_eq!(after_publish_profile.version_id, replacement_version_id);
        assert_eq!(after_publish_profile.price_usd, predecessor_price + 1_000.0);
    }

    #[tokio::test]
    async fn production_dry_run_rolls_back_and_apply_publishes_exact_fact_counts() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let source_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO curation_evidence_sources (
              source_url, source_title, source_domain, source_tier, retrieved_at
            ) VALUES (
              'https://manufacturer.example/reference', 'Factory reference',
              'manufacturer.example', 'manufacturer_primary', '2026-08-19'
            ) RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let claim_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO curation_evidence_claims (
              evidence_source_id, claim_kind, subject_text, predicate_text,
              object_text, quoted_evidence, validation_status, validated_at
            ) VALUES (?, 'identity', 'test aircraft', 'defines',
              'factory configuration',
              'Primary factory source identifies the exact configuration and components.',
              'validated', '2026-08-19')
            RETURNING id
            "#,
        )
        .bind(source_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let applicability_claim_id: i64 = sqlx::query_scalar(
            "INSERT INTO curation_evidence_claims (evidence_source_id, claim_kind, subject_text, predicate_text, object_text, quoted_evidence, validation_status, validated_at) VALUES (?, 'applicability', 'test aircraft', 'applies in', 'GLOBAL', 'Primary source defines global applicability.', 'validated', '2026-08-19') RETURNING id",
        )
        .bind(source_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let price_claim_id: i64 = sqlx::query_scalar(
            "INSERT INTO curation_evidence_claims (evidence_source_id, claim_kind, subject_text, predicate_text, object_text, quoted_evidence, validation_status, validated_at) VALUES (?, 'price', 'test aircraft', 'equipped MSRP', '500000 USD', 'Primary source states the equipped MSRP.', 'validated', '2026-08-19') RETURNING id",
        )
        .bind(source_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let factory_claim_id: i64 = sqlx::query_scalar(
            "INSERT INTO curation_evidence_claims (evidence_source_id, claim_kind, subject_text, predicate_text, object_text, quoted_evidence, validation_status, validated_at) VALUES (?, 'standard_equipment', 'test aircraft', 'includes', 'factory equipment', 'Primary source defines the complete standard equipment.', 'validated', '2026-08-19') RETURNING id",
        )
        .bind(source_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let official_index_source_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO curation_evidence_sources (
              source_url, source_title, source_domain, source_tier, retrieved_at
            ) VALUES (
              'https://www.bls.gov/cpi/test-series', 'Official CPI test series',
              'bls.gov', 'regulator_primary', '2026-08-19'
            ) RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let official_index_claim_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO curation_evidence_claims (
              evidence_source_id, claim_kind, subject_text, predicate_text,
              object_text, quoted_evidence, validation_status, validated_at
            ) VALUES (?, 'price', 'official CPI test series', 'reports index values',
              '2020=250; 2026=300',
              'Official government series reports index values 250 and 300.',
              'validated', '2026-08-19')
            RETURNING id
            "#,
        )
        .bind(official_index_source_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let observation_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_identity_observations (
              observed_make, observed_family, observed_designation, model_year,
              exact_source_evidence, observation_sha256
            ) VALUES ('Test Aircraft', 'Model 1', 'Model 1', 2026,
              '2026 Test Aircraft Model 1', 'reference-publisher-observation')
            RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let case_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_identity_resolution_cases (
              observation_id, resolution_scope, job_fingerprint, catalog_revision
            ) VALUES (?, 'reference_profile', 'reference-publisher-job', 'test-catalog')
            RETURNING id
            "#,
        )
        .bind(observation_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut decisions = BTreeMap::new();
        for entity_kind in [
            "make",
            "family",
            "designation",
            "reference_configuration",
            "reference_profile",
            "serial_scheme",
            "engine_model",
            "propeller_model",
        ] {
            let decision_id: i64 = sqlx::query_scalar(
                r#"
                INSERT INTO aircraft_identity_decisions (
                  resolution_case_id, entity_kind, decision_action,
                  decision_status, decision_payload_json,
                  deterministic_validation_json, deterministic_validation_passed,
                  rationale, decided_at
                ) VALUES (?, ?, 'approve_new', 'approved', '{}', '{}', TRUE,
                  'approved test reference', '2026-08-19')
                RETURNING id
                "#,
            )
            .bind(case_id)
            .bind(entity_kind)
            .fetch_one(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO aircraft_identity_decision_claims (decision_id, evidence_claim_id, evidence_role) VALUES (?, ?, 'identity')",
            )
            .bind(decision_id)
            .bind(claim_id)
            .execute(pool)
            .await
            .unwrap();
            decisions.insert(entity_kind, decision_id);
        }
        let make_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_makes (name, normalized_name, approval_decision_id) VALUES ('Test Aircraft', 'test aircraft', ?) RETURNING id",
        )
        .bind(decisions["make"])
        .fetch_one(pool)
        .await
        .unwrap();
        let serial_scheme_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_serial_number_schemes (
              aircraft_make_id, name, normalization_version,
              validation_pattern, approval_decision_id
            ) VALUES (?, 'Test serials', ?, '^[A-Z]{2}[0-9]+$', ?)
            RETURNING id
            "#,
        )
        .bind(make_id)
        .bind(AIRCRAFT_SERIAL_SORT_KEY_VERSION)
        .bind(decisions["serial_scheme"])
        .fetch_one(pool)
        .await
        .unwrap();
        let family_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_model_families (aircraft_make_id, name, normalized_name, approval_decision_id) VALUES (?, 'Model 1', 'model 1', ?) RETURNING id",
        )
        .bind(make_id)
        .bind(decisions["family"])
        .fetch_one(pool)
        .await
        .unwrap();
        let designation_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_designations (aircraft_model_family_id, official_designation, normalized_official_designation, display_name, approval_decision_id) VALUES (?, 'Model 1', 'model 1', 'Model 1', ?) RETURNING id",
        )
        .bind(family_id)
        .bind(decisions["designation"])
        .fetch_one(pool)
        .await
        .unwrap();
        let engine_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_engine_catalog_models (
              manufacturer_name, normalized_manufacturer_name, model_name,
              normalized_model_name, identifier_authority,
              normalized_identifier_authority, identifier_kind,
              authoritative_identifier, normalized_authoritative_identifier,
              approval_decision_id, identity_evidence_claim_id
            ) VALUES ('Engine Maker', 'engine maker', 'E-1', 'e-1',
              'Engine Maker', 'engine maker', 'manufacturer_model_code',
              'E-1', 'e-1', ?, ?) RETURNING id
            "#,
        )
        .bind(decisions["engine_model"])
        .bind(claim_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let propeller_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_propeller_catalog_models (
              manufacturer_name, normalized_manufacturer_name, model_name,
              normalized_model_name, identifier_authority,
              normalized_identifier_authority, identifier_kind,
              authoritative_identifier, normalized_authoritative_identifier,
              approval_decision_id, identity_evidence_claim_id
            ) VALUES ('Prop Maker', 'prop maker', 'P-1', 'p-1',
              'Prop Maker', 'prop maker', 'manufacturer_model_code',
              'P-1', 'p-1', ?, ?) RETURNING id
            "#,
        )
        .bind(decisions["propeller_model"])
        .bind(claim_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let avionics_manufacturer_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Avionics Maker', 'avionics maker') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        ensure_test_manufacturer_identity(&db, avionics_manufacturer_id)
            .await
            .unwrap();
        let avionics_type_id: i64 = sqlx::query_scalar(
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Navigator', 'navigator') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let avionics_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO avionics_models (
              avionics_manufacturer_id, name, normalized_name,
              manufacturer_identifier_kind, manufacturer_identifier,
              normalized_manufacturer_identifier, identity_source_url,
              identity_source_title, identity_evidence_text,
              identity_evidence_kind, identity_confidence, catalog_reviewed_at,
              introduced_year, estimated_unit_value_usd, value_basis,
              replacement_cost_usd, value_reference_year, value_source
            ) VALUES (?, 'Navigator 1', 'navigator 1',
              'manufacturer_model_number', 'NAV-1', 'nav1',
              'https://manufacturer.example/nav-1', 'Navigator 1',
              'Manufacturer source identifies Navigator 1.',
              'authoritative_reference', 'very_high', CURRENT_TIMESTAMP,
              2026, 10000, 'installed_contribution', 15000, 2026,
              'manufacturer reference') RETURNING id
            "#,
        )
        .bind(avionics_manufacturer_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(avionics_id)
        .bind(avionics_type_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE avionics_models SET catalog_status = 'approved', verification_method = 'automated', verified_by_user_id = NULL WHERE id = ?",
        )
            .bind(avionics_id)
            .execute(pool)
            .await
            .unwrap();
        let market_id: i64 =
            sqlx::query_scalar("SELECT id FROM aircraft_markets WHERE code = 'GLOBAL'")
                .fetch_one(pool)
                .await
                .unwrap();
        let draft = ApprovedReferenceVersionDraft {
            identity: ReferenceConfigurationIdentityDraft {
                aircraft_model_family_id: family_id,
                aircraft_designation_id: designation_id,
                aircraft_generation_id: None,
                tier_package_id: None,
                display_name: "2026 Model 1 standard".to_string(),
                approval_decision_id: decisions["reference_configuration"],
            },
            model_year: 2026,
            revision: 1,
            supersedes_version_id: None,
            profile_approval_decision_id: decisions["reference_profile"],
            applicability: vec![ReferenceApplicabilityDraft {
                aircraft_market_id: market_id,
                applies_to_all_serials: true,
                aircraft_serial_number_scheme_id: None,
                serial_prefix: None,
                serial_from_display: None,
                serial_to_display: None,
                evidence_claim_id: applicability_claim_id,
            }],
            price: ReferencePriceDraft {
                direct_cited_amount_usd: 500_000.0,
                direct_cited_nominal_dollar_year: 2020,
                evidence_claim_id: price_claim_id,
            },
            dollar_normalization: Some(ReferenceDollarNormalizationDraft {
                source_nominal_dollar_year: 2020,
                target_nominal_dollar_year: 2026,
                official_index_series: "BLS CPI test series".to_string(),
                source_index_value: 250.0,
                target_index_value: 300.0,
                normalization_factor: 1.2,
                evidence_claim_id: official_index_claim_id,
            }),
            avionics: vec![ReferenceComponentDraft {
                catalog_id: avionics_id,
                quantity: 2,
                included_in_tier: false,
                evidence_claim_id: factory_claim_id,
            }],
            engines: vec![ReferenceComponentDraft {
                catalog_id: engine_id,
                quantity: 1,
                included_in_tier: false,
                evidence_claim_id: factory_claim_id,
            }],
            propellers: vec![ReferenceComponentDraft {
                catalog_id: propeller_id,
                quantity: 1,
                included_in_tier: false,
                evidence_claim_id: factory_claim_id,
            }],
            features: vec![],
            avionics_set_evidence_claim_id: factory_claim_id,
            engines_set_evidence_claim_id: factory_claim_id,
            propellers_set_evidence_claim_id: factory_claim_id,
            features_set_evidence_claim_id: factory_claim_id,
        };

        let mut typographic_overlap = draft.clone();
        typographic_overlap.applicability = vec![
            ReferenceApplicabilityDraft {
                aircraft_market_id: market_id,
                applies_to_all_serials: false,
                aircraft_serial_number_scheme_id: Some(serial_scheme_id),
                serial_prefix: Some("SR-".to_string()),
                serial_from_display: Some("SR-100".to_string()),
                serial_to_display: Some("SR-100".to_string()),
                evidence_claim_id: claim_id,
            },
            ReferenceApplicabilityDraft {
                aircraft_market_id: market_id,
                applies_to_all_serials: false,
                aircraft_serial_number_scheme_id: Some(serial_scheme_id),
                serial_prefix: Some("SR".to_string()),
                serial_from_display: Some("SR100".to_string()),
                serial_to_display: Some("SR100".to_string()),
                evidence_claim_id: claim_id,
            },
        ];
        let canonical_scopes = canonicalize_applicability(&db, &typographic_overlap)
            .await
            .unwrap();
        assert_eq!(
            canonical_scopes[0].serial_from_sort_key,
            canonical_scopes[1].serial_from_sort_key
        );
        preview_reference_version(&db, &typographic_overlap)
            .await
            .expect_err("typographic-equivalent serial ranges must overlap after canonicalization");

        let mut caller_supplied_key = serde_json::to_value(&typographic_overlap).unwrap();
        caller_supplied_key["applicability"][0]["serial_from_sort_key"] =
            serde_json::Value::String("SR100".to_string());
        serde_json::from_value::<ApprovedReferenceVersionDraft>(caller_supplied_key)
            .expect_err("callers cannot provide mechanical serial sort keys");

        let before: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aircraft_reference_configuration_versions")
                .fetch_one(pool)
                .await
                .unwrap();
        preview_reference_version(&db, &draft).await.unwrap();
        let after_preview: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aircraft_reference_configuration_versions")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(after_preview, before, "dry run must roll back every row");

        let ids = assemble_and_publish_reference_version(&db, &draft)
            .await
            .unwrap();
        let state: String = sqlx::query_scalar(
            "SELECT publication_state FROM aircraft_reference_configuration_versions WHERE id = ?",
        )
        .bind(ids.version_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(state, "published");
        let normalized = normalized_reference_price(
            &db,
            &PublishedReferenceConfiguration {
                version_id: ids.version_id,
                configuration_id: ids.configuration_id,
                display_name: draft.identity.display_name.clone(),
                model_year: draft.model_year,
                market_codes: vec!["GLOBAL".to_string()],
                price_usd: draft.price.direct_cited_amount_usd,
                price_reference_year: draft.price.direct_cited_nominal_dollar_year,
                avionics_count: 1,
                engine_count: 1,
                propeller_count: 1,
                feature_count: 0,
            },
            2026,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(normalized.normalization_factor, 1.2);
        assert_eq!(normalized.normalized_amount_usd, 600_000.0);
        assert!(normalized.official_normalization_fact_id.is_some());
        for (table, expected) in [
            ("aircraft_reference_avionics", 1_i64),
            ("aircraft_reference_engines", 1),
            ("aircraft_reference_propellers", 1),
            ("aircraft_reference_features", 0),
            ("aircraft_reference_fact_set_attestations", 4),
        ] {
            let count: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {table} WHERE aircraft_reference_configuration_version_id = ?"
            ))
            .bind(ids.version_id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(count, expected, "unexpected {table} count");
        }
        let engine_quantity: i64 = sqlx::query_scalar(
            "SELECT SUM(quantity) FROM aircraft_reference_engines WHERE aircraft_reference_configuration_version_id = ?",
        )
        .bind(ids.version_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let propeller_quantity: i64 = sqlx::query_scalar(
            "SELECT SUM(quantity) FROM aircraft_reference_propellers WHERE aircraft_reference_configuration_version_id = ?",
        )
        .bind(ids.version_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let avionics_quantity: i64 = sqlx::query_scalar(
            "SELECT SUM(quantity) FROM aircraft_reference_avionics WHERE aircraft_reference_configuration_version_id = ?",
        )
        .bind(ids.version_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            (engine_quantity, propeller_quantity, avionics_quantity),
            (1, 1, 2)
        );

        let replacement_decision_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_identity_decisions (
              resolution_case_id, entity_kind, decision_action,
              decision_status, decision_payload_json,
              deterministic_validation_json, deterministic_validation_passed,
              rationale, decided_at
            ) VALUES (?, 'reference_profile', 'approve_new', 'approved', '{}',
              '{}', TRUE, 'approved correction', '2026-08-19')
            RETURNING id
            "#,
        )
        .bind(case_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_identity_decision_claims (decision_id, evidence_claim_id, evidence_role) VALUES (?, ?, 'identity')",
        )
        .bind(replacement_decision_id)
        .bind(claim_id)
        .execute(pool)
        .await
        .unwrap();
        let mut replacement = draft.clone();
        replacement.revision = 2;
        replacement.supersedes_version_id = Some(ids.version_id);
        replacement.profile_approval_decision_id = replacement_decision_id;
        replacement.price.direct_cited_amount_usd = 510_000.0;

        let replacement_ids = assemble_and_publish_reference_version(&db, &replacement)
            .await
            .unwrap();
        let states = sqlx::query_as::<_, (i64, String)>(
            "SELECT id, publication_state FROM aircraft_reference_configuration_versions WHERE id IN (?, ?) ORDER BY id",
        )
        .bind(ids.version_id)
        .bind(replacement_ids.version_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(
            states,
            vec![
                (ids.version_id, "superseded".to_string()),
                (replacement_ids.version_id, "published".to_string()),
            ]
        );

        let weak_source_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO curation_evidence_sources (
              source_url, source_title, source_domain, source_tier, retrieved_at
            ) VALUES ('https://secondary.example/reference', 'Secondary price',
              'secondary.example', 'recognized_secondary', '2026-08-19')
            RETURNING id
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let weak_price_claim_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO curation_evidence_claims (
              evidence_source_id, claim_kind, subject_text, predicate_text,
              object_text, quoted_evidence, validation_status, validated_at
            ) VALUES (?, 'identity', 'test aircraft', 'lists price', '$520000',
              'Secondary source lists a price.', 'validated', '2026-08-19')
            RETURNING id
            "#,
        )
        .bind(weak_source_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let failed_decision_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_identity_decisions (
              resolution_case_id, entity_kind, decision_action,
              decision_status, decision_payload_json,
              deterministic_validation_json, deterministic_validation_passed,
              rationale, decided_at
            ) VALUES (?, 'reference_profile', 'approve_new', 'approved', '{}',
              '{}', TRUE, 'approved attempted correction', '2026-08-19')
            RETURNING id
            "#,
        )
        .bind(case_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_identity_decision_claims (decision_id, evidence_claim_id, evidence_role) VALUES (?, ?, 'identity')",
        )
        .bind(failed_decision_id)
        .bind(claim_id)
        .execute(pool)
        .await
        .unwrap();
        let mut skipped_revision = replacement.clone();
        skipped_revision.revision = 4;
        skipped_revision.supersedes_version_id = Some(replacement_ids.version_id);
        skipped_revision.profile_approval_decision_id = failed_decision_id;
        assemble_and_publish_reference_version(&db, &skipped_revision)
            .await
            .expect_err("a correction must name the exact prior revision");
        let retained_after_skip: String = sqlx::query_scalar(
            "SELECT publication_state FROM aircraft_reference_configuration_versions WHERE id = ?",
        )
        .bind(replacement_ids.version_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained_after_skip, "published");

        let mut wrong_kind = replacement.clone();
        wrong_kind.revision = 3;
        wrong_kind.supersedes_version_id = Some(replacement_ids.version_id);
        wrong_kind.profile_approval_decision_id = failed_decision_id;
        wrong_kind.price.evidence_claim_id = claim_id;
        assemble_and_publish_reference_version(&db, &wrong_kind)
            .await
            .expect_err("a primary identity claim must not be accepted as price evidence");

        let mut rejected = replacement.clone();
        rejected.revision = 3;
        rejected.supersedes_version_id = Some(replacement_ids.version_id);
        rejected.profile_approval_decision_id = failed_decision_id;
        rejected.price.evidence_claim_id = weak_price_claim_id;

        assemble_and_publish_reference_version(&db, &rejected)
            .await
            .expect_err("a non-primary price claim must reject publication");
        let retained_state: String = sqlx::query_scalar(
            "SELECT publication_state FROM aircraft_reference_configuration_versions WHERE id = ?",
        )
        .bind(replacement_ids.version_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let rejected_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_reference_configuration_versions WHERE aircraft_reference_configuration_id = ? AND model_year = 2026 AND revision = 3",
        )
        .bind(replacement_ids.configuration_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(retained_state, "published");
        assert_eq!(
            rejected_count, 0,
            "failed correction must roll back as a unit"
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_publishes_and_selects_null_and_non_null_configuration_dimensions() {
        use sqlx::postgres::PgPoolOptions;

        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../../../schema/postgres.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../tests/schema/aircraft_reference_catalog.postgres.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let db = AppDb::connect(&database_url).await.unwrap();

        let null_configuration_decision: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_identity_decisions (
              resolution_case_id, entity_kind, decision_action, decision_status,
              decision_payload_json, deterministic_validation_json,
              deterministic_validation_passed, rationale, decided_at
            ) VALUES (1, 'reference_configuration', 'approve_new', 'approved',
              '{}', '{}', TRUE, 'null-dimension configuration', '2026-08-20')
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let null_profile_decision: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_identity_decisions (
              resolution_case_id, entity_kind, decision_action, decision_status,
              decision_payload_json, deterministic_validation_json,
              deterministic_validation_passed, rationale, decided_at
            ) VALUES (1, 'reference_profile', 'approve_new', 'approved',
              '{}', '{}', TRUE, 'null-dimension profile', '2026-08-20')
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let non_null_profile_decision: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_identity_decisions (
              resolution_case_id, entity_kind, decision_action, decision_status,
              decision_payload_json, deterministic_validation_json,
              deterministic_validation_passed, rationale, decided_at
            ) VALUES (1, 'reference_profile', 'approve_new', 'approved',
              '{}', '{}', TRUE, 'non-null-dimension profile', '2026-08-20')
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        for decision_id in [
            null_configuration_decision,
            null_profile_decision,
            non_null_profile_decision,
        ] {
            sqlx::query(
                "INSERT INTO aircraft_identity_decision_claims \
                 (decision_id, evidence_claim_id, evidence_role) \
                 VALUES ($1, 1, 'identity')",
            )
            .bind(decision_id)
            .execute(&pool)
            .await
            .unwrap();
        }

        let draft = |model_year,
                     generation_id,
                     tier_package_id,
                     configuration_decision,
                     profile_decision,
                     display_name: &str| ApprovedReferenceVersionDraft {
            identity: ReferenceConfigurationIdentityDraft {
                aircraft_model_family_id: 1,
                aircraft_designation_id: 1,
                aircraft_generation_id: generation_id,
                tier_package_id,
                display_name: display_name.to_string(),
                approval_decision_id: configuration_decision,
            },
            model_year,
            revision: 1,
            supersedes_version_id: None,
            profile_approval_decision_id: profile_decision,
            applicability: vec![ReferenceApplicabilityDraft {
                aircraft_market_id: 1,
                applies_to_all_serials: true,
                aircraft_serial_number_scheme_id: None,
                serial_prefix: None,
                serial_from_display: None,
                serial_to_display: None,
                evidence_claim_id: 2,
            }],
            price: ReferencePriceDraft {
                direct_cited_amount_usd: 800_000.0,
                direct_cited_nominal_dollar_year: model_year,
                evidence_claim_id: 3,
            },
            dollar_normalization: None,
            avionics: vec![],
            engines: vec![ReferenceComponentDraft {
                catalog_id: 1,
                quantity: 1,
                included_in_tier: tier_package_id.is_some(),
                evidence_claim_id: 4,
            }],
            propellers: vec![ReferenceComponentDraft {
                catalog_id: 1,
                quantity: 1,
                included_in_tier: tier_package_id.is_some(),
                evidence_claim_id: 4,
            }],
            features: vec![],
            avionics_set_evidence_claim_id: 4,
            engines_set_evidence_claim_id: 4,
            propellers_set_evidence_claim_id: 4,
            features_set_evidence_claim_id: 4,
        };

        let non_null_ids = assemble_and_publish_reference_version(
            &db,
            &draft(
                2024,
                Some(1),
                Some(1),
                8,
                non_null_profile_decision,
                "SR22 G6 GTS",
            ),
        )
        .await
        .unwrap();
        assert_eq!(non_null_ids.configuration_id, 1);
        let null_ids = assemble_and_publish_reference_version(
            &db,
            &draft(
                2023,
                None,
                None,
                null_configuration_decision,
                null_profile_decision,
                "SR22 base",
            ),
        )
        .await
        .unwrap();
        assert_ne!(null_ids.configuration_id, non_null_ids.configuration_id);

        sqlx::raw_sql(
            r#"
            SET session_replication_role = replica;
            INSERT INTO curation_evidence_sources (
              source_url, source_title, source_domain, source_tier, retrieved_at
            ) VALUES (
              'https://www.faa.gov/test/reference-nullness', 'FAA test source',
              'faa.gov', 'regulator_primary', '2026-08-20'
            );
            INSERT INTO faa_registry_snapshots (
              evidence_source_id, snapshot_date, source_url, archive_sha256,
              source_manifest_sha256, target_set_sha256,
              master_member_name, master_member_sha256,
              aircraft_member_name, aircraft_member_sha256,
              engine_member_name, engine_member_sha256, record_hash_domain
            ) VALUES (
              3, '2026-08-20', 'https://www.faa.gov/test/reference-nullness',
              repeat('1',64), repeat('2',64), repeat('3',64),
              'MASTER.txt', repeat('4',64), 'ACFTREF.txt', repeat('5',64),
              'ENGINE.txt', repeat('6',64),
              'aircost-faa-master-retained-aircraft-projection-v1'
            );
            INSERT INTO faa_registry_aircraft (
              snapshot_id, n_number, manufacturer_serial_raw,
              manufacturer_serial_key, aircraft_code, year_manufactured,
              source_record_sha256
            ) VALUES
              (1, 'N100AA', 'SR100', 'SR100', 'TEST', 2024, repeat('a',64)),
              (1, 'N200AA', 'SR200', 'SR200', 'TEST', 2023, repeat('b',64));
            INSERT INTO faa_registry_coverage (snapshot_id, n_number, lookup_status)
            VALUES (1, 'N100AA', 'matched'), (1, 'N200AA', 'matched');
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, registration_number, serial_number,
              airframe_hours
            ) SELECT aircraft_model_variant_id, 1,
                'https://listing.test/non-null-reference', 2024, 500000,
                'N100AA', 'SR100', 100
              FROM aircraft_sale_listing_pending_compatibility_placeholder
              WHERE singleton_id=1;
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, registration_number, serial_number,
              airframe_hours
            ) SELECT aircraft_model_variant_id, 1,
                'https://listing.test/null-reference', 2023, 400000,
                'N200AA', 'SR200', 200
              FROM aircraft_sale_listing_pending_compatibility_placeholder
              WHERE singleton_id=1;
            INSERT INTO aircraft_sale_listing_identity_assignments (
              aircraft_sale_listing_id, aircraft_make_id,
              aircraft_model_family_id, aircraft_designation_id,
              aircraft_generation_id, aircraft_factory_package_id,
              identity_decision_id, identity_evidence_claim_id,
              faa_registry_snapshot_id, faa_n_number, faa_source_record_sha256
            ) VALUES
              (1, 1, 1, 1, 1, 1, 3, 1, 1, 'N100AA', repeat('a',64)),
              (2, 1, 1, 1, NULL, NULL, 3, 1, 1, 'N200AA', repeat('b',64));
            INSERT INTO aircraft_sale_listing_current_identity_assignments (
              aircraft_sale_listing_id, identity_assignment_id
            ) VALUES (1, 1), (2, 2);
            SET session_replication_role = origin;
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let non_null_status = listing_reference_status(&db, 1).await.unwrap();
        assert!(non_null_status.ready, "{:?}", non_null_status.gaps);
        assert_eq!(
            non_null_status.published.unwrap().version_id,
            non_null_ids.version_id
        );
        let null_status = listing_reference_status(&db, 2).await.unwrap();
        assert!(null_status.ready, "{:?}", null_status.gaps);
        assert_eq!(
            null_status.published.unwrap().version_id,
            null_ids.version_id
        );
    }
}
