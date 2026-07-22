//! Read-only projections for the authenticated avionics catalog inspector.
//!
//! The catalog is global. Listing-derived counts and occurrences are scoped to
//! the caller using the same rule as the listings API: verified listings are
//! public, while an unverified listing is visible only to its creator.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;
const MIN_CATALOG_YEAR: i64 = 1900;
const MAX_CATALOG_YEAR: i64 = 2200;

// Keep this predicate identical for summary counts and detail-row eligibility.
// The named aliases are established by both queries below.
const VALUATION_ELIGIBLE_LISTING_LINK_PREDICATE: &str = r#"
    listing.ingestion_state = 'ready'
    AND link.source = 'listing'
    AND link.source_confidence = 'high'
    AND link.configuration_action IN ('installed', 'replaces', 'removes')
    AND installed_model.catalog_status = 'approved'
    AND (
      link.replaces_avionics_model_id IS NULL
      OR replacement_model.catalog_status = 'approved'
    )
"#;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct AvionicsCatalogQuery {
    pub search: Option<String>,
    pub status: Option<String>,
    pub capability: Option<String>,
    pub completeness: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsCatalogPage {
    pub items: Vec<AvionicsCatalogSummary>,
    pub total: u64,
    pub limit: u32,
    pub offset: u64,
}

/// Stable response contract shared by the catalog grid and detail header.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsCatalogSummary {
    pub id: i64,
    pub display_name: String,
    pub manufacturer: AvionicsManufacturer,
    pub name: String,
    pub stable_identifier: Option<AvionicsStableIdentifier>,
    pub capabilities: Vec<AvionicsCapability>,
    pub catalog: AvionicsCatalogState,
    pub introduced_year: Option<i64>,
    pub discontinued_year: Option<i64>,
    pub valuation: AvionicsValuation,
    pub usage: AvionicsUsageCounts,
    pub completeness: AvionicsCompleteness,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsManufacturer {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsStableIdentifier {
    pub kind: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsCapability {
    pub id: i64,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsCatalogState {
    pub status: String,
    pub identity_confidence: Option<String>,
    pub reviewed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsValuation {
    pub installed_contribution_usd: Option<f64>,
    pub replacement_cost_usd: Option<f64>,
    pub reference_year: Option<i64>,
    pub source: Option<String>,
    pub basis: String,
    pub scope: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsUsageCounts {
    pub visible_listings: i64,
    pub valuation_eligible_listings: i64,
    pub legacy_defaults: i64,
    pub reference_configurations: i64,
    pub suite_relationships: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsCompleteness {
    pub complete: bool,
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsCatalogDetail {
    pub summary: AvionicsCatalogSummary,
    pub identity_evidence: AvionicsIdentityEvidence,
    pub suite_components: Vec<AvionicsSuiteRelationship>,
    pub suite_memberships: Vec<AvionicsSuiteRelationship>,
    pub listing_occurrences: Vec<AvionicsListingOccurrence>,
    pub legacy_defaults: Vec<AvionicsLegacyDefaultUsage>,
    pub reference_configurations: Vec<AvionicsReferenceConfigurationUsage>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsIdentityEvidence {
    pub source_url: Option<String>,
    pub source_title: Option<String>,
    pub evidence_text: Option<String>,
    pub evidence_kind: String,
    pub confidence: Option<String>,
    pub reviewed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsSuiteRelationship {
    pub model_id: i64,
    pub display_name: String,
    pub stable_identifier: Option<AvionicsStableIdentifier>,
    pub quantity: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsListingOccurrence {
    pub listing_id: i64,
    pub model_year: i64,
    pub aircraft: String,
    pub registration_number: Option<String>,
    pub serial_number: Option<String>,
    pub source_url: Option<String>,
    pub is_verified: bool,
    pub ingestion_state: String,
    pub ingestion_error: Option<String>,
    pub occurrence_role: String,
    pub configuration_action: String,
    pub quantity: i64,
    pub source: String,
    pub source_notes: Option<String>,
    pub source_confidence: Option<String>,
    pub valuation_eligible: bool,
    pub valuation_blockers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsLegacyDefaultUsage {
    pub id: i64,
    pub aircraft_model_variant_id: i64,
    pub model_year: i64,
    pub aircraft: String,
    pub quantity: i64,
    pub source_url: String,
    pub source_title: String,
    pub source_notes: String,
    pub source_confidence: String,
}

/// `aircraft_reference_avionics` rows are append-only/immutable in the schema.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsReferenceConfigurationUsage {
    pub id: i64,
    pub configuration_id: i64,
    pub configuration_version_id: i64,
    pub display_name: String,
    pub configuration_kind: String,
    pub aircraft_make: String,
    pub aircraft_family: String,
    pub aircraft_designation: String,
    pub aircraft_generation: Option<String>,
    pub tier_package: Option<String>,
    pub model_year: i64,
    pub revision: i64,
    pub publication_state: String,
    pub quantity: i64,
    pub equipment_role: String,
    pub evidence_claim_id: i64,
    pub evidence_validation_status: String,
    pub evidence_source_url: String,
    pub evidence_source_title: String,
    pub evidence_source_tier: String,
    pub immutable: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsInspectorOptions {
    pub statuses: Vec<AvionicsFilterOption>,
    pub capabilities: Vec<AvionicsFilterOption>,
    pub completeness: Vec<AvionicsFilterOption>,
    pub default_limit: u32,
    pub max_limit: u32,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AvionicsFilterOption {
    pub value: String,
    pub label: String,
    pub count: i64,
}

#[derive(Debug)]
pub enum AvionicsInspectionError {
    Validation(String),
    NotFound(String),
    Database(String),
}

impl Display for AvionicsInspectionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(message) | Self::NotFound(message) | Self::Database(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl Error for AvionicsInspectionError {}

impl From<sqlx::Error> for AvionicsInspectionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

type InspectionResult<T> = Result<T, AvionicsInspectionError>;

#[derive(Debug)]
struct ValidatedQuery {
    search: Option<String>,
    status: Option<String>,
    capability: Option<String>,
    completeness: Option<bool>,
    limit: u32,
    offset: u64,
}

impl AvionicsCatalogQuery {
    fn validate(self) -> InspectionResult<ValidatedQuery> {
        let search = nonempty(self.search);
        if search.as_ref().is_some_and(|value| value.len() > 200) {
            return Err(AvionicsInspectionError::Validation(
                "search must not exceed 200 characters".to_string(),
            ));
        }
        let status = nonempty(self.status).map(|value| value.to_ascii_lowercase());
        if status
            .as_ref()
            .is_some_and(|value| !matches!(value.as_str(), "unreviewed" | "approved" | "rejected"))
        {
            return Err(AvionicsInspectionError::Validation(
                "status must be unreviewed, approved, or rejected".to_string(),
            ));
        }
        let capability = nonempty(self.capability).map(|value| value.to_ascii_lowercase());
        let completeness = match nonempty(self.completeness)
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
        {
            None => None,
            Some("complete") => Some(true),
            Some("incomplete") => Some(false),
            Some(_) => {
                return Err(AvionicsInspectionError::Validation(
                    "completeness must be complete or incomplete".to_string(),
                ));
            }
        };
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 || limit > MAX_LIMIT {
            return Err(AvionicsInspectionError::Validation(format!(
                "limit must be between 1 and {MAX_LIMIT}"
            )));
        }
        Ok(ValidatedQuery {
            search,
            status,
            capability,
            completeness,
            limit,
            offset: self.offset.unwrap_or(0),
        })
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn escape_like_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' | '%' | '_' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    escaped
}

fn is_blank(value: Option<&str>) -> bool {
    value.is_none_or(|value| value.trim().is_empty())
}

fn stable_identifier(
    kind: Option<String>,
    value: Option<String>,
) -> Option<AvionicsStableIdentifier> {
    match (kind, value) {
        (Some(kind), Some(value)) if !kind.trim().is_empty() && !value.trim().is_empty() => {
            Some(AvionicsStableIdentifier { kind, value })
        }
        _ => None,
    }
}

#[derive(Clone, Debug, FromRow)]
struct RawSummary {
    id: i64,
    manufacturer_id: i64,
    manufacturer_name: String,
    name: String,
    catalog_status: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    identity_source_url: Option<String>,
    identity_source_title: Option<String>,
    identity_evidence_text: Option<String>,
    identity_evidence_kind: String,
    identity_confidence: Option<String>,
    catalog_reviewed_at: Option<String>,
    introduced_year: Option<i64>,
    discontinued_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
    value_basis: String,
    replacement_cost_usd: Option<f64>,
    value_reference_year: Option<i64>,
    value_source: Option<String>,
    valuation_scope: String,
    canonical_capability_count: i64,
    visible_listing_count: i64,
    valuation_eligible_listing_count: i64,
    legacy_default_count: i64,
    reference_configuration_count: i64,
    suite_relationship_count: i64,
    approved_suite_component_count: i64,
}

#[derive(Clone, Debug, FromRow)]
struct CapabilityRow {
    model_id: i64,
    id: i64,
    name: String,
    normalized_name: String,
}

fn summary_sql() -> String {
    format!(
        r#"
    SELECT
      model.id,
      manufacturer.id AS manufacturer_id,
      manufacturer.name AS manufacturer_name,
      model.name,
      model.catalog_status,
      model.manufacturer_identifier_kind,
      model.manufacturer_identifier,
      model.identity_source_url,
      model.identity_source_title,
      model.identity_evidence_text,
      model.identity_evidence_kind,
      model.identity_confidence,
      model.catalog_reviewed_at,
      model.introduced_year,
      model.discontinued_year,
      model.estimated_unit_value_usd,
      model.value_basis,
      model.replacement_cost_usd,
      model.value_reference_year,
      model.value_source,
      model.valuation_scope,
      (SELECT COUNT(*)
        FROM avionics_model_types capability_link
        JOIN avionics_types capability
          ON capability.id = capability_link.avionics_type_id
        WHERE capability_link.avionics_model_id = model.id
          AND lower(trim(capability.normalized_name)) <> 'unknown'
      ) AS canonical_capability_count,
      (SELECT COUNT(DISTINCT listing_link.aircraft_sale_listing_id)
        FROM aircraft_sale_listing_avionics listing_link
        JOIN aircraft_sale_listings listing
          ON listing.id = listing_link.aircraft_sale_listing_id
        WHERE (listing_link.avionics_model_id = model.id
          OR listing_link.replaces_avionics_model_id = model.id)
          AND (listing.is_verified = TRUE OR listing.created_by_user_id = ?)
      ) AS visible_listing_count,
      (SELECT COUNT(DISTINCT link.aircraft_sale_listing_id)
        FROM aircraft_sale_listing_avionics link
        JOIN aircraft_sale_listings listing
          ON listing.id = link.aircraft_sale_listing_id
        JOIN avionics_models installed_model
          ON installed_model.id = link.avionics_model_id
        LEFT JOIN avionics_models replacement_model
          ON replacement_model.id = link.replaces_avionics_model_id
        WHERE (link.avionics_model_id = model.id
          OR link.replaces_avionics_model_id = model.id)
          AND (listing.is_verified = TRUE OR listing.created_by_user_id = ?)
          AND {VALUATION_ELIGIBLE_LISTING_LINK_PREDICATE}
      ) AS valuation_eligible_listing_count,
      (SELECT COUNT(*) FROM aircraft_model_variant_default_avionics default_link
        WHERE default_link.avionics_model_id = model.id) AS legacy_default_count,
      (SELECT COUNT(*) FROM aircraft_reference_avionics reference_link
        WHERE reference_link.avionics_model_id = model.id) AS reference_configuration_count,
      (SELECT COUNT(*) FROM avionics_suite_components suite_link
        WHERE suite_link.suite_model_id = model.id
           OR suite_link.component_model_id = model.id) AS suite_relationship_count,
      (SELECT COUNT(*)
        FROM avionics_suite_components suite_component_link
        JOIN avionics_models suite_component
          ON suite_component.id = suite_component_link.component_model_id
        WHERE suite_component_link.suite_model_id = model.id
          AND suite_component.catalog_status = 'approved'
      ) AS approved_suite_component_count
    FROM avionics_models model
    JOIN avionics_manufacturers manufacturer
      ON manufacturer.id = model.avionics_manufacturer_id
    WHERE (? IS NULL OR model.id = ?)
      AND (
        ? IS NULL
        OR lower(manufacturer.name) LIKE ? ESCAPE '\'
        OR lower(model.name) LIKE ? ESCAPE '\'
        OR lower(COALESCE(model.manufacturer_identifier, '')) LIKE ? ESCAPE '\'
      )
      AND (? IS NULL OR model.catalog_status = ?)
      AND (
        ? IS NULL OR EXISTS (
          SELECT 1
          FROM avionics_model_types filtered_capability_link
          JOIN avionics_types filtered_capability
            ON filtered_capability.id = filtered_capability_link.avionics_type_id
          WHERE filtered_capability_link.avionics_model_id = model.id
            AND (lower(filtered_capability.name) = ?
              OR filtered_capability.normalized_name = ?)
        )
      )
    ORDER BY lower(manufacturer.name), lower(model.name), model.id
"#
    )
}

async fn load_raw_summaries(
    db: &AppDb,
    user_id: i64,
    model_id: Option<i64>,
    query: &ValidatedQuery,
) -> InspectionResult<Vec<RawSummary>> {
    let summary_sql = summary_sql();
    let sql = db.sql(&summary_sql);
    let search_pattern = query
        .search
        .as_ref()
        .map(|value| format!("%{}%", escape_like_literal(&value.to_ascii_lowercase())));
    macro_rules! bind_summary_query {
        ($query_builder:expr) => {
            $query_builder
                .bind(user_id)
                .bind(user_id)
                .bind(model_id)
                .bind(model_id)
                .bind(search_pattern.as_deref())
                .bind(search_pattern.as_deref())
                .bind(search_pattern.as_deref())
                .bind(search_pattern.as_deref())
                .bind(query.status.as_deref())
                .bind(query.status.as_deref())
                .bind(query.capability.as_deref())
                .bind(query.capability.as_deref())
                .bind(query.capability.as_deref())
        };
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            Ok(bind_summary_query!(sqlx::query_as::<_, RawSummary>(&sql))
                .fetch_all(pool)
                .await?)
        }
        DatabaseBackend::Postgres(pool) => {
            Ok(bind_summary_query!(sqlx::query_as::<_, RawSummary>(&sql))
                .fetch_all(pool)
                .await?)
        }
    }
}

async fn load_capabilities(
    db: &AppDb,
    model_id: Option<i64>,
) -> InspectionResult<BTreeMap<i64, Vec<AvionicsCapability>>> {
    let sql = db.sql(
        r#"
        SELECT
          membership.avionics_model_id AS model_id,
          capability.id,
          capability.name,
          capability.normalized_name
        FROM avionics_model_types membership
        JOIN avionics_types capability ON capability.id = membership.avionics_type_id
        WHERE (? IS NULL OR membership.avionics_model_id = ?)
        ORDER BY lower(capability.name), capability.id
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, CapabilityRow>(&sql)
                .bind(model_id)
                .bind(model_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, CapabilityRow>(&sql)
                .bind(model_id)
                .bind(model_id)
                .fetch_all(pool)
                .await?
        }
    };
    let mut capabilities = BTreeMap::new();
    for row in rows {
        capabilities
            .entry(row.model_id)
            .or_insert_with(Vec::new)
            .push(AvionicsCapability {
                id: row.id,
                name: row.name,
                value: row.normalized_name,
            });
    }
    Ok(capabilities)
}

fn completeness_blockers(row: &RawSummary) -> Vec<String> {
    let mut blockers = Vec::new();
    if row.catalog_status != "approved" {
        blockers.push("catalog_not_approved".to_string());
    }
    if is_blank(row.manufacturer_identifier_kind.as_deref())
        || is_blank(row.manufacturer_identifier.as_deref())
    {
        blockers.push("missing_stable_identifier".to_string());
    }
    if is_blank(row.identity_source_url.as_deref())
        || is_blank(row.identity_source_title.as_deref())
        || is_blank(row.identity_evidence_text.as_deref())
        || row.identity_evidence_kind != "authoritative_reference"
    {
        blockers.push("missing_authoritative_identity_evidence".to_string());
    }
    if row.identity_confidence.as_deref() != Some("very_high") {
        blockers.push("identity_confidence_not_very_high".to_string());
    }
    if row.canonical_capability_count == 0 {
        blockers.push("missing_capability".to_string());
    }
    match row.introduced_year {
        None => blockers.push("missing_introduced_year".to_string()),
        Some(year) if !(MIN_CATALOG_YEAR..=MAX_CATALOG_YEAR).contains(&year) => {
            blockers.push("invalid_introduced_year".to_string())
        }
        Some(_) => {}
    }
    if let Some(year) = row.discontinued_year {
        if !(MIN_CATALOG_YEAR..=MAX_CATALOG_YEAR).contains(&year) {
            blockers.push("invalid_discontinued_year".to_string());
        } else if row.introduced_year.is_some_and(|introduced| {
            (MIN_CATALOG_YEAR..=MAX_CATALOG_YEAR).contains(&introduced) && year < introduced
        }) {
            blockers.push("discontinued_before_introduced".to_string());
        }
    }
    match row.estimated_unit_value_usd {
        None => blockers.push("missing_installed_contribution".to_string()),
        Some(value) if !value.is_finite() || value < 0.0 => {
            blockers.push("invalid_installed_contribution".to_string())
        }
        Some(_) => {}
    }
    match row.replacement_cost_usd {
        None => blockers.push("missing_replacement_cost".to_string()),
        Some(replacement) if !replacement.is_finite() || replacement < 0.0 => {
            blockers.push("invalid_replacement_cost".to_string());
        }
        Some(replacement)
            if row
                .estimated_unit_value_usd
                .is_some_and(|installed| installed.is_finite() && replacement < installed) =>
        {
            blockers.push("replacement_cost_below_installed_contribution".to_string());
        }
        Some(_) => {}
    }
    match row.value_reference_year {
        None => blockers.push("missing_value_reference_year".to_string()),
        Some(year) if !(1900..=2200).contains(&year) => {
            blockers.push("invalid_value_reference_year".to_string())
        }
        Some(_) => {}
    }
    if row
        .value_source
        .as_deref()
        .is_none_or(|source| source.trim().is_empty())
    {
        blockers.push("missing_value_source".to_string());
    }
    if row.value_basis != "installed_contribution" {
        blockers.push("value_basis_not_installed_contribution".to_string());
    }
    if row.valuation_scope == "integrated_suite" && row.approved_suite_component_count == 0 {
        blockers.push("integrated_suite_missing_approved_component".to_string());
    }
    blockers
}

fn summary_from_raw(
    row: RawSummary,
    capabilities: Vec<AvionicsCapability>,
) -> AvionicsCatalogSummary {
    let blockers = completeness_blockers(&row);
    AvionicsCatalogSummary {
        id: row.id,
        display_name: format!("{} {}", row.manufacturer_name, row.name),
        manufacturer: AvionicsManufacturer {
            id: row.manufacturer_id,
            name: row.manufacturer_name,
        },
        name: row.name,
        stable_identifier: stable_identifier(
            row.manufacturer_identifier_kind,
            row.manufacturer_identifier,
        ),
        capabilities,
        catalog: AvionicsCatalogState {
            status: row.catalog_status,
            identity_confidence: row.identity_confidence,
            reviewed_at: row.catalog_reviewed_at,
        },
        introduced_year: row.introduced_year,
        discontinued_year: row.discontinued_year,
        valuation: AvionicsValuation {
            installed_contribution_usd: row.estimated_unit_value_usd,
            replacement_cost_usd: row.replacement_cost_usd,
            reference_year: row.value_reference_year,
            source: row.value_source,
            basis: row.value_basis,
            scope: row.valuation_scope,
        },
        usage: AvionicsUsageCounts {
            visible_listings: row.visible_listing_count,
            valuation_eligible_listings: row.valuation_eligible_listing_count,
            legacy_defaults: row.legacy_default_count,
            reference_configurations: row.reference_configuration_count,
            suite_relationships: row.suite_relationship_count,
        },
        completeness: AvionicsCompleteness {
            complete: blockers.is_empty(),
            blockers,
        },
    }
}

pub async fn list_avionics_catalog(
    db: &AppDb,
    user_id: i64,
    query: AvionicsCatalogQuery,
) -> InspectionResult<AvionicsCatalogPage> {
    let query = query.validate()?;
    let raw = load_raw_summaries(db, user_id, None, &query).await?;
    let mut capabilities = load_capabilities(db, None).await?;
    let mut items = raw
        .into_iter()
        .map(|row| {
            let row_capabilities = capabilities.remove(&row.id).unwrap_or_default();
            summary_from_raw(row, row_capabilities)
        })
        .filter(|item| {
            query
                .completeness
                .is_none_or(|complete| item.completeness.complete == complete)
        })
        .collect::<Vec<_>>();
    let total = items.len() as u64;
    let start = usize::try_from(query.offset)
        .unwrap_or(usize::MAX)
        .min(items.len());
    let end = start.saturating_add(query.limit as usize).min(items.len());
    let items = items.drain(start..end).collect();
    Ok(AvionicsCatalogPage {
        items,
        total,
        limit: query.limit,
        offset: query.offset,
    })
}

pub async fn avionics_catalog_options(
    db: &AppDb,
    user_id: i64,
) -> InspectionResult<AvionicsInspectorOptions> {
    #[derive(FromRow)]
    struct OptionRow {
        value: String,
        label: String,
        count: i64,
    }

    let status_sql = db.sql(
        r#"
        SELECT catalog_status AS value, catalog_status AS label, COUNT(*) AS count
        FROM avionics_models
        GROUP BY catalog_status
        ORDER BY catalog_status
        "#,
    );
    let capability_sql = db.sql(
        r#"
        SELECT capability.normalized_name AS value, capability.name AS label,
          COUNT(DISTINCT membership.avionics_model_id) AS count
        FROM avionics_types capability
        LEFT JOIN avionics_model_types membership
          ON membership.avionics_type_id = capability.id
        GROUP BY capability.id, capability.normalized_name, capability.name
        ORDER BY lower(capability.name), capability.id
        "#,
    );
    let (statuses, capability_rows) = match db.backend() {
        DatabaseBackend::Sqlite(pool) => (
            sqlx::query_as::<_, OptionRow>(&status_sql)
                .fetch_all(pool)
                .await?,
            sqlx::query_as::<_, OptionRow>(&capability_sql)
                .fetch_all(pool)
                .await?,
        ),
        DatabaseBackend::Postgres(pool) => (
            sqlx::query_as::<_, OptionRow>(&status_sql)
                .fetch_all(pool)
                .await?,
            sqlx::query_as::<_, OptionRow>(&capability_sql)
                .fetch_all(pool)
                .await?,
        ),
    };
    let unfiltered = AvionicsCatalogQuery::default().validate()?;
    let all_models = load_raw_summaries(db, user_id, None, &unfiltered).await?;
    let complete_count = all_models
        .iter()
        .filter(|row| completeness_blockers(row).is_empty())
        .count() as i64;
    let incomplete_count = all_models.len() as i64 - complete_count;
    let status_counts = statuses
        .into_iter()
        .map(|row| (row.value, row.count))
        .collect::<BTreeMap<_, _>>();
    let statuses = [
        ("unreviewed", "Unreviewed"),
        ("approved", "Approved"),
        ("rejected", "Rejected"),
    ]
    .into_iter()
    .map(|(value, label)| AvionicsFilterOption {
        value: value.to_string(),
        label: label.to_string(),
        count: status_counts.get(value).copied().unwrap_or(0),
    })
    .collect();
    let map = |row: OptionRow| AvionicsFilterOption {
        value: row.value,
        label: row.label,
        count: row.count,
    };
    Ok(AvionicsInspectorOptions {
        statuses,
        capabilities: capability_rows.into_iter().map(map).collect(),
        completeness: vec![
            AvionicsFilterOption {
                value: "complete".to_string(),
                label: "Complete".to_string(),
                count: complete_count,
            },
            AvionicsFilterOption {
                value: "incomplete".to_string(),
                label: "Incomplete".to_string(),
                count: incomplete_count,
            },
        ],
        default_limit: DEFAULT_LIMIT,
        max_limit: MAX_LIMIT,
    })
}

#[derive(Clone, Debug, FromRow)]
struct RelatedModelRow {
    model_id: i64,
    manufacturer_name: String,
    model_name: String,
    manufacturer_identifier_kind: Option<String>,
    manufacturer_identifier: Option<String>,
    quantity: i64,
}

fn suite_relationship(row: RelatedModelRow) -> AvionicsSuiteRelationship {
    AvionicsSuiteRelationship {
        model_id: row.model_id,
        display_name: format!("{} {}", row.manufacturer_name, row.model_name),
        stable_identifier: stable_identifier(
            row.manufacturer_identifier_kind,
            row.manufacturer_identifier,
        ),
        quantity: row.quantity,
    }
}

async fn load_suite_relationships(
    db: &AppDb,
    model_id: i64,
    components: bool,
) -> InspectionResult<Vec<AvionicsSuiteRelationship>> {
    let sql = if components {
        r#"
        SELECT component.id AS model_id, manufacturer.name AS manufacturer_name,
          component.name AS model_name, component.manufacturer_identifier_kind,
          component.manufacturer_identifier, membership.quantity
        FROM avionics_suite_components membership
        JOIN avionics_models component ON component.id = membership.component_model_id
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = component.avionics_manufacturer_id
        WHERE membership.suite_model_id = ?
        ORDER BY lower(manufacturer.name), lower(component.name), component.id
        "#
    } else {
        r#"
        SELECT suite.id AS model_id, manufacturer.name AS manufacturer_name,
          suite.name AS model_name, suite.manufacturer_identifier_kind,
          suite.manufacturer_identifier, membership.quantity
        FROM avionics_suite_components membership
        JOIN avionics_models suite ON suite.id = membership.suite_model_id
        JOIN avionics_manufacturers manufacturer
          ON manufacturer.id = suite.avionics_manufacturer_id
        WHERE membership.component_model_id = ?
        ORDER BY lower(manufacturer.name), lower(suite.name), suite.id
        "#
    };
    let sql = db.sql(sql);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, RelatedModelRow>(&sql)
                .bind(model_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, RelatedModelRow>(&sql)
                .bind(model_id)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows.into_iter().map(suite_relationship).collect())
}

#[derive(Clone, Debug, FromRow)]
struct ListingOccurrenceRow {
    listing_id: i64,
    model_year: i64,
    manufacturer_name: String,
    model_name: String,
    variant_name: String,
    registration_number: Option<String>,
    serial_number: Option<String>,
    source_url: Option<String>,
    is_verified: bool,
    ingestion_state: String,
    ingestion_error: Option<String>,
    occurrence_role: String,
    configuration_action: String,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    installed_model_status: String,
    replacement_model_status: Option<String>,
    valuation_eligible: bool,
}

fn listing_valuation_blockers(row: &ListingOccurrenceRow) -> Vec<String> {
    let mut blockers = Vec::new();
    if row.ingestion_state != "ready" {
        blockers.push("listing_not_ready".to_string());
    }
    if row.source != "listing" {
        blockers.push("source_not_listing".to_string());
    }
    if row.source_confidence.as_deref() != Some("high") {
        blockers.push("source_confidence_not_high".to_string());
    }
    if !matches!(
        row.configuration_action.as_str(),
        "installed" | "replaces" | "removes"
    ) {
        blockers.push("configuration_action_not_valuation_eligible".to_string());
    }
    if row.installed_model_status != "approved" {
        blockers.push("installed_model_not_approved".to_string());
    }
    if row
        .replacement_model_status
        .as_deref()
        .is_some_and(|status| status != "approved")
    {
        blockers.push("replacement_model_not_approved".to_string());
    }
    blockers
}

async fn load_listing_occurrences(
    db: &AppDb,
    user_id: i64,
    model_id: i64,
) -> InspectionResult<Vec<AvionicsListingOccurrence>> {
    let occurrence_sql = format!(
        r#"
        SELECT listing.id AS listing_id, listing.model_year,
          manufacturer.name AS manufacturer_name, aircraft_model.name AS model_name,
          variant.name AS variant_name, listing.registration_number, listing.serial_number,
          listing.source_url, listing.is_verified, listing.ingestion_state,
          listing.ingestion_error,
          CASE WHEN link.avionics_model_id = ? THEN 'catalog_model'
               ELSE 'replaced_model' END AS occurrence_role,
          link.configuration_action, link.quantity, link.source, link.source_notes,
          link.source_confidence, installed_model.catalog_status AS installed_model_status,
          replacement_model.catalog_status AS replacement_model_status,
          CASE WHEN {VALUATION_ELIGIBLE_LISTING_LINK_PREDICATE}
            THEN TRUE ELSE FALSE END AS valuation_eligible
        FROM aircraft_sale_listing_avionics link
        JOIN aircraft_sale_listings listing ON listing.id = link.aircraft_sale_listing_id
        JOIN avionics_models installed_model ON installed_model.id = link.avionics_model_id
        LEFT JOIN avionics_models replacement_model
          ON replacement_model.id = link.replaces_avionics_model_id
        JOIN aircraft_model_variants variant ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models aircraft_model ON aircraft_model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = aircraft_model.aircraft_manufacturer_id
        WHERE (link.avionics_model_id = ? OR link.replaces_avionics_model_id = ?)
          AND (listing.is_verified = TRUE OR listing.created_by_user_id = ?)
        ORDER BY listing.model_year DESC, listing.id DESC, link.id
        "#
    );
    let sql = db.sql(&occurrence_sql);
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ListingOccurrenceRow>(&sql)
                .bind(model_id)
                .bind(model_id)
                .bind(model_id)
                .bind(user_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ListingOccurrenceRow>(&sql)
                .bind(model_id)
                .bind(model_id)
                .bind(model_id)
                .bind(user_id)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows
        .into_iter()
        .map(|row| {
            let valuation_blockers = listing_valuation_blockers(&row);
            debug_assert_eq!(row.valuation_eligible, valuation_blockers.is_empty());
            AvionicsListingOccurrence {
                listing_id: row.listing_id,
                model_year: row.model_year,
                aircraft: format!(
                    "{} {} {}",
                    row.manufacturer_name, row.model_name, row.variant_name
                ),
                registration_number: row.registration_number,
                serial_number: row.serial_number,
                source_url: row.source_url,
                is_verified: row.is_verified,
                ingestion_state: row.ingestion_state,
                ingestion_error: row.ingestion_error,
                occurrence_role: row.occurrence_role,
                configuration_action: row.configuration_action,
                quantity: row.quantity,
                source: row.source,
                source_notes: row.source_notes,
                source_confidence: row.source_confidence,
                valuation_eligible: row.valuation_eligible,
                valuation_blockers,
            }
        })
        .collect())
}

#[derive(Clone, Debug, FromRow)]
struct LegacyDefaultRow {
    id: i64,
    aircraft_model_variant_id: i64,
    model_year: i64,
    manufacturer_name: String,
    model_name: String,
    variant_name: String,
    quantity: i64,
    source_url: String,
    source_title: String,
    source_notes: String,
    source_confidence: String,
}

async fn load_legacy_defaults(
    db: &AppDb,
    model_id: i64,
) -> InspectionResult<Vec<AvionicsLegacyDefaultUsage>> {
    let sql = db.sql(
        r#"
        SELECT default_link.id, default_link.aircraft_model_variant_id,
          default_link.model_year, manufacturer.name AS manufacturer_name,
          aircraft_model.name AS model_name, variant.name AS variant_name,
          default_link.quantity, default_link.source_url, default_link.source_title,
          default_link.source_notes, default_link.source_confidence
        FROM aircraft_model_variant_default_avionics default_link
        JOIN aircraft_model_variants variant
          ON variant.id = default_link.aircraft_model_variant_id
        JOIN aircraft_models aircraft_model ON aircraft_model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = aircraft_model.aircraft_manufacturer_id
        WHERE default_link.avionics_model_id = ?
        ORDER BY default_link.model_year DESC, lower(manufacturer.name),
          lower(aircraft_model.name), lower(variant.name), default_link.id
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, LegacyDefaultRow>(&sql)
                .bind(model_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, LegacyDefaultRow>(&sql)
                .bind(model_id)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows
        .into_iter()
        .map(|row| AvionicsLegacyDefaultUsage {
            id: row.id,
            aircraft_model_variant_id: row.aircraft_model_variant_id,
            model_year: row.model_year,
            aircraft: format!(
                "{} {} {}",
                row.manufacturer_name, row.model_name, row.variant_name
            ),
            quantity: row.quantity,
            source_url: row.source_url,
            source_title: row.source_title,
            source_notes: row.source_notes,
            source_confidence: row.source_confidence,
        })
        .collect())
}

#[derive(Clone, Debug, FromRow)]
struct ReferenceUsageRow {
    id: i64,
    configuration_id: i64,
    configuration_version_id: i64,
    display_name: String,
    configuration_kind: String,
    aircraft_make: String,
    aircraft_family: String,
    aircraft_designation: String,
    aircraft_generation: Option<String>,
    tier_package: Option<String>,
    model_year: i64,
    revision: i64,
    publication_state: String,
    quantity: i64,
    equipment_role: String,
    evidence_claim_id: i64,
    evidence_validation_status: String,
    evidence_source_url: String,
    evidence_source_title: String,
    evidence_source_tier: String,
}

async fn load_reference_configurations(
    db: &AppDb,
    model_id: i64,
) -> InspectionResult<Vec<AvionicsReferenceConfigurationUsage>> {
    let sql = db.sql(
        r#"
        SELECT reference_link.id, configuration.id AS configuration_id,
          version.id AS configuration_version_id, configuration.display_name,
          configuration.configuration_kind, make.name AS aircraft_make,
          family.name AS aircraft_family, designation.display_name AS aircraft_designation,
          generation.name AS aircraft_generation, package.name AS tier_package,
          version.model_year, version.revision, version.publication_state,
          reference_link.quantity, reference_link.equipment_role,
          reference_link.evidence_claim_id,
          claim.validation_status AS evidence_validation_status,
          source.source_url AS evidence_source_url,
          source.source_title AS evidence_source_title,
          source.source_tier AS evidence_source_tier
        FROM aircraft_reference_avionics reference_link
        JOIN aircraft_reference_configuration_versions version
          ON version.id = reference_link.aircraft_reference_configuration_version_id
        JOIN aircraft_reference_configurations configuration
          ON configuration.id = version.aircraft_reference_configuration_id
        JOIN aircraft_model_families family
          ON family.id = configuration.aircraft_model_family_id
        JOIN aircraft_makes make ON make.id = family.aircraft_make_id
        JOIN aircraft_designations designation
          ON designation.id = configuration.aircraft_designation_id
        LEFT JOIN aircraft_generations generation
          ON generation.id = configuration.aircraft_generation_id
        LEFT JOIN aircraft_factory_packages package
          ON package.id = configuration.tier_package_id
        JOIN curation_evidence_claims claim
          ON claim.id = reference_link.evidence_claim_id
        JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
        WHERE reference_link.avionics_model_id = ?
        ORDER BY version.model_year DESC, lower(make.name), lower(family.name),
          lower(configuration.display_name), version.revision DESC
        "#,
    );
    let rows = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, ReferenceUsageRow>(&sql)
                .bind(model_id)
                .fetch_all(pool)
                .await?
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, ReferenceUsageRow>(&sql)
                .bind(model_id)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows
        .into_iter()
        .map(|row| AvionicsReferenceConfigurationUsage {
            id: row.id,
            configuration_id: row.configuration_id,
            configuration_version_id: row.configuration_version_id,
            display_name: row.display_name,
            configuration_kind: row.configuration_kind,
            aircraft_make: row.aircraft_make,
            aircraft_family: row.aircraft_family,
            aircraft_designation: row.aircraft_designation,
            aircraft_generation: row.aircraft_generation,
            tier_package: row.tier_package,
            model_year: row.model_year,
            revision: row.revision,
            publication_state: row.publication_state,
            quantity: row.quantity,
            equipment_role: row.equipment_role,
            evidence_claim_id: row.evidence_claim_id,
            evidence_validation_status: row.evidence_validation_status,
            evidence_source_url: row.evidence_source_url,
            evidence_source_title: row.evidence_source_title,
            evidence_source_tier: row.evidence_source_tier,
            immutable: true,
        })
        .collect())
}

pub async fn get_avionics_catalog_detail(
    db: &AppDb,
    user_id: i64,
    model_id: i64,
) -> InspectionResult<AvionicsCatalogDetail> {
    let query = AvionicsCatalogQuery::default().validate()?;
    let mut raw = load_raw_summaries(db, user_id, Some(model_id), &query).await?;
    let row = raw.pop().ok_or_else(|| {
        AvionicsInspectionError::NotFound("avionics catalog entry not found".to_string())
    })?;
    let evidence = AvionicsIdentityEvidence {
        source_url: row.identity_source_url.clone(),
        source_title: row.identity_source_title.clone(),
        evidence_text: row.identity_evidence_text.clone(),
        evidence_kind: row.identity_evidence_kind.clone(),
        confidence: row.identity_confidence.clone(),
        reviewed_at: row.catalog_reviewed_at.clone(),
    };
    let mut capabilities = load_capabilities(db, Some(model_id)).await?;
    let summary = summary_from_raw(row, capabilities.remove(&model_id).unwrap_or_default());
    let (suite_components, suite_memberships, listing_occurrences, legacy_defaults, references) = tokio::try_join!(
        load_suite_relationships(db, model_id, true),
        load_suite_relationships(db, model_id, false),
        load_listing_occurrences(db, user_id, model_id),
        load_legacy_defaults(db, model_id),
        load_reference_configurations(db, model_id),
    )?;
    Ok(AvionicsCatalogDetail {
        summary,
        identity_evidence: evidence,
        suite_components,
        suite_memberships,
        listing_occurrences,
        legacy_defaults,
        reference_configurations: references,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        avionics_catalog_options, completeness_blockers, get_avionics_catalog_detail,
        list_avionics_catalog, AvionicsCatalogQuery, RawSummary,
    };
    use crate::db::{AppDb, DatabaseBackend};

    fn incomplete_row() -> RawSummary {
        RawSummary {
            id: 1,
            manufacturer_id: 1,
            manufacturer_name: "Example".to_string(),
            name: "Unit".to_string(),
            catalog_status: "unreviewed".to_string(),
            manufacturer_identifier_kind: None,
            manufacturer_identifier: None,
            identity_source_url: None,
            identity_source_title: None,
            identity_evidence_text: None,
            identity_evidence_kind: "unreviewed".to_string(),
            identity_confidence: None,
            catalog_reviewed_at: None,
            introduced_year: None,
            discontinued_year: None,
            estimated_unit_value_usd: None,
            value_basis: "unreviewed".to_string(),
            replacement_cost_usd: None,
            value_reference_year: None,
            value_source: None,
            valuation_scope: "unit".to_string(),
            canonical_capability_count: 0,
            visible_listing_count: 0,
            valuation_eligible_listing_count: 0,
            legacy_default_count: 0,
            reference_configuration_count: 0,
            suite_relationship_count: 0,
            approved_suite_component_count: 0,
        }
    }

    fn complete_row() -> RawSummary {
        let mut row = incomplete_row();
        row.catalog_status = "approved".to_string();
        row.manufacturer_identifier_kind = Some("sku".to_string());
        row.manufacturer_identifier = Some("TEST-1".to_string());
        row.identity_source_url = Some("https://manufacturer.example/test-1".to_string());
        row.identity_source_title = Some("Data sheet".to_string());
        row.identity_evidence_text = Some("Product identity".to_string());
        row.identity_evidence_kind = "authoritative_reference".to_string();
        row.identity_confidence = Some("very_high".to_string());
        row.canonical_capability_count = 1;
        row.introduced_year = Some(2020);
        row.estimated_unit_value_usd = Some(4_000.0);
        row.replacement_cost_usd = Some(9_000.0);
        row.value_reference_year = Some(2026);
        row.value_source = Some("Manufacturer price list".to_string());
        row.value_basis = "installed_contribution".to_string();
        row
    }

    #[test]
    fn completeness_names_each_missing_pipeline_requirement() {
        assert_eq!(
            completeness_blockers(&incomplete_row()),
            vec![
                "catalog_not_approved",
                "missing_stable_identifier",
                "missing_authoritative_identity_evidence",
                "identity_confidence_not_very_high",
                "missing_capability",
                "missing_introduced_year",
                "missing_installed_contribution",
                "missing_replacement_cost",
                "missing_value_reference_year",
                "missing_value_source",
                "value_basis_not_installed_contribution",
            ]
        );
    }

    #[test]
    fn completeness_reports_invalid_values_and_empty_integrated_suites() {
        let mut row = complete_row();
        row.estimated_unit_value_usd = Some(-1.0);
        row.replacement_cost_usd = Some(-2.0);
        row.value_reference_year = Some(1800);
        row.value_source = Some("  ".to_string());
        row.value_basis = "replacement_cost".to_string();
        row.valuation_scope = "integrated_suite".to_string();

        assert_eq!(
            completeness_blockers(&row),
            vec![
                "invalid_installed_contribution",
                "invalid_replacement_cost",
                "invalid_value_reference_year",
                "missing_value_source",
                "value_basis_not_installed_contribution",
                "integrated_suite_missing_approved_component",
            ]
        );
    }

    #[test]
    fn completeness_rejects_blank_evidence_nonfinite_money_and_implausible_years() {
        let mut row = complete_row();
        row.manufacturer_identifier_kind = Some(" ".to_string());
        row.manufacturer_identifier = Some("\t".to_string());
        row.identity_source_url = Some(" ".to_string());
        row.identity_source_title = Some("\n".to_string());
        row.identity_evidence_text = Some("".to_string());
        row.introduced_year = Some(1899);
        row.discontinued_year = Some(2201);
        row.estimated_unit_value_usd = Some(f64::NAN);
        row.replacement_cost_usd = Some(f64::INFINITY);

        assert_eq!(
            completeness_blockers(&row),
            vec![
                "missing_stable_identifier",
                "missing_authoritative_identity_evidence",
                "invalid_introduced_year",
                "invalid_discontinued_year",
                "invalid_installed_contribution",
                "invalid_replacement_cost",
            ]
        );

        let mut reversed_years = complete_row();
        reversed_years.discontinued_year = Some(2019);
        assert_eq!(
            completeness_blockers(&reversed_years),
            vec!["discontinued_before_introduced"]
        );
    }

    async fn execute(db: &AppDb, sql: &str) {
        match db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(sql).execute(pool).await.unwrap();
            }
            DatabaseBackend::Postgres(_) => unreachable!("test uses SQLite"),
        }
    }

    async fn scalar(db: &AppDb, sql: &str) -> i64 {
        match db.backend() {
            DatabaseBackend::Sqlite(pool) => sqlx::query_scalar(sql).fetch_one(pool).await.unwrap(),
            DatabaseBackend::Postgres(_) => unreachable!("test uses SQLite"),
        }
    }

    async fn fixture() -> (AppDb, i64, i64, i64) {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let current_user = db.current_user(None).await.unwrap();
        execute(
            &db,
            "INSERT INTO users (email, display_name, auth_provider, auth_subject) VALUES ('other@example.test', 'Other', 'test', 'other-subject')",
        )
        .await;
        let other_user = scalar(
            &db,
            "SELECT id FROM users WHERE email = 'other@example.test'",
        )
        .await;
        execute(
            &db,
            "INSERT INTO avionics_manufacturers (name, normalized_name) VALUES ('Inspector Test', 'inspector test')",
        )
        .await;
        execute(
            &db,
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Inspector Capability', 'inspector capability')",
        )
        .await;
        execute(
            &db,
            r#"INSERT INTO avionics_models (
                avionics_manufacturer_id, name, normalized_name,
                manufacturer_identifier_kind, manufacturer_identifier,
                normalized_manufacturer_identifier, identity_source_url,
                identity_source_title, identity_evidence_text, identity_evidence_kind,
                identity_confidence, introduced_year, estimated_unit_value_usd,
                value_basis, replacement_cost_usd, value_reference_year, value_source
              ) VALUES (
                (SELECT id FROM avionics_manufacturers WHERE normalized_name = 'inspector test'),
                'Visible Unit', 'visible unit', 'manufacturer_model_number', 'VISIBLE-1',
                'visible 1', 'https://manufacturer.example/visible-1', 'Visible Unit data sheet',
                'Manufacturer identifies the VISIBLE-1 product.', 'authoritative_reference',
                'very_high', 2019, 4000, 'installed_contribution', 9000, 2026,
                'Manufacturer price list'
              )"#,
        )
        .await;
        let avionics_id = scalar(
            &db,
            "SELECT id FROM avionics_models WHERE normalized_name = 'visible unit'",
        )
        .await;
        execute(
            &db,
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) SELECT (SELECT id FROM avionics_models WHERE normalized_name = 'visible unit'), (SELECT id FROM avionics_types WHERE normalized_name = 'inspector capability')",
        )
        .await;
        execute(
            &db,
            "UPDATE avionics_models SET catalog_status = 'approved', catalog_reviewed_at = CURRENT_TIMESTAMP WHERE id = (SELECT id FROM avionics_models WHERE normalized_name = 'visible unit')",
        )
        .await;
        execute(
            &db,
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Inspector Aircraft', 'inspector aircraft')",
        )
        .await;
        execute(
            &db,
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) SELECT id, 'Model', 'model' FROM aircraft_manufacturers WHERE normalized_name = 'inspector aircraft'",
        )
        .await;
        execute(
            &db,
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) SELECT id, 'Variant', 'variant' FROM aircraft_models WHERE normalized_name = 'model' AND aircraft_manufacturer_id = (SELECT id FROM aircraft_manufacturers WHERE normalized_name = 'inspector aircraft')",
        )
        .await;
        let variant_id = scalar(
            &db,
            "SELECT variant.id FROM aircraft_model_variants variant JOIN aircraft_models model ON model.id = variant.aircraft_model_id JOIN aircraft_manufacturers manufacturer ON manufacturer.id = model.aircraft_manufacturer_id WHERE manufacturer.normalized_name = 'inspector aircraft'",
        )
        .await;
        for (email, verified, registration) in [
            ("developer@localhost", 1, "N100IT"),
            ("developer@localhost", 0, "N101IT"),
            ("other@example.test", 0, "N102IT"),
        ] {
            let sql = format!(
                "INSERT INTO aircraft_sale_listings (aircraft_model_variant_id, created_by_user_id, is_verified, source_url, model_year, asking_price_usd, airframe_hours, registration_number) VALUES ({variant_id}, (SELECT id FROM users WHERE email = '{email}'), {verified}, 'https://listing.example/{registration}', 2020, 100000, 1000, '{registration}')"
            );
            execute(&db, &sql).await;
        }
        execute(
            &db,
            &format!(
                "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id) SELECT id, {avionics_id} FROM aircraft_sale_listings WHERE registration_number IN ('N100IT', 'N101IT', 'N102IT')"
            ),
        )
        .await;
        execute(
            &db,
            "UPDATE aircraft_sale_listings SET ingestion_state = 'ready', ingestion_completed_at = CURRENT_TIMESTAMP WHERE registration_number = 'N100IT'",
        )
        .await;
        execute(
            &db,
            "UPDATE aircraft_sale_listing_avionics SET source_confidence = 'high' WHERE aircraft_sale_listing_id = (SELECT id FROM aircraft_sale_listings WHERE registration_number = 'N100IT')",
        )
        .await;
        (db, current_user.id, other_user, avionics_id)
    }

    #[tokio::test]
    async fn catalog_and_detail_hide_other_users_unverified_listings() {
        let (db, current_user_id, other_user_id, avionics_id) = fixture().await;
        let page = list_avionics_catalog(
            &db,
            current_user_id,
            AvionicsCatalogQuery {
                search: Some("VISIBLE-1".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].usage.visible_listings, 2);
        assert_eq!(page.items[0].usage.valuation_eligible_listings, 1);
        assert!(page.items[0].completeness.complete);
        assert_eq!(page.items[0].capabilities[0].name, "Inspector Capability");

        let current_detail = get_avionics_catalog_detail(&db, current_user_id, avionics_id)
            .await
            .unwrap();
        assert_eq!(current_detail.listing_occurrences.len(), 2);
        let eligible = current_detail
            .listing_occurrences
            .iter()
            .find(|occurrence| occurrence.registration_number.as_deref() == Some("N100IT"))
            .unwrap();
        assert_eq!(eligible.ingestion_state, "ready");
        assert!(eligible.ingestion_error.is_none());
        assert!(eligible.valuation_eligible);
        assert!(eligible.valuation_blockers.is_empty());
        let observed_only = current_detail
            .listing_occurrences
            .iter()
            .find(|occurrence| occurrence.registration_number.as_deref() == Some("N101IT"))
            .unwrap();
        assert!(!observed_only.valuation_eligible);
        assert_eq!(
            observed_only.valuation_blockers,
            vec!["listing_not_ready", "source_confidence_not_high"]
        );
        assert!(current_detail
            .listing_occurrences
            .iter()
            .all(|occurrence| occurrence.registration_number.as_deref() != Some("N102IT")));

        let other_detail = get_avionics_catalog_detail(&db, other_user_id, avionics_id)
            .await
            .unwrap();
        assert_eq!(other_detail.listing_occurrences.len(), 2);
        assert!(other_detail
            .listing_occurrences
            .iter()
            .all(|occurrence| occurrence.registration_number.as_deref() != Some("N101IT")));
    }

    #[tokio::test]
    async fn filters_pagination_and_options_are_stable() {
        let (db, current_user_id, _, _) = fixture().await;
        let page = list_avionics_catalog(
            &db,
            current_user_id,
            AvionicsCatalogQuery {
                status: Some("approved".to_string()),
                capability: Some("inspector capability".to_string()),
                completeness: Some("complete".to_string()),
                limit: Some(1),
                offset: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 1);
        let options = avionics_catalog_options(&db, current_user_id)
            .await
            .unwrap();
        assert!(options
            .capabilities
            .iter()
            .any(|option| option.value == "inspector capability" && option.count == 1));
        assert_eq!(options.statuses.len(), 3);
        assert!(options
            .statuses
            .iter()
            .any(|option| option.value == "rejected" && option.count == 0));
        assert!(options
            .completeness
            .iter()
            .any(|option| option.value == "complete" && option.count >= 1));
    }

    #[tokio::test]
    async fn search_treats_like_metacharacters_as_literals() {
        let (db, current_user_id, _, _) = fixture().await;
        for sql in [
            r#"INSERT INTO avionics_models (avionics_manufacturer_id, name, normalized_name)
               SELECT id, 'Percent % Unit', 'percent % unit'
               FROM avionics_manufacturers WHERE normalized_name = 'inspector test'"#,
            r#"INSERT INTO avionics_models (avionics_manufacturer_id, name, normalized_name)
               SELECT id, 'Underscore _ Unit', 'underscore _ unit'
               FROM avionics_manufacturers WHERE normalized_name = 'inspector test'"#,
            r#"INSERT INTO avionics_models (avionics_manufacturer_id, name, normalized_name)
               SELECT id, 'Backslash \ Unit', 'backslash \ unit'
               FROM avionics_manufacturers WHERE normalized_name = 'inspector test'"#,
        ] {
            execute(&db, sql).await;
        }

        for (search, expected_name) in [
            ("%", "Percent % Unit"),
            ("_", "Underscore _ Unit"),
            ("\\", "Backslash \\ Unit"),
        ] {
            let page = list_avionics_catalog(
                &db,
                current_user_id,
                AvionicsCatalogQuery {
                    search: Some(search.to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(page.total, 1, "literal search for {search:?}");
            assert_eq!(page.items[0].name, expected_name);
        }
    }

    #[tokio::test]
    async fn legacy_unknown_type_does_not_satisfy_canonical_capability_completeness() {
        let (db, current_user_id, _, _) = fixture().await;
        execute(
            &db,
            "INSERT INTO avionics_types (name, normalized_name) VALUES ('Unknown', 'unknown') ON CONFLICT (normalized_name) DO NOTHING",
        )
        .await;
        execute(
            &db,
            r#"INSERT INTO avionics_models (
                avionics_manufacturer_id, name, normalized_name,
                manufacturer_identifier_kind, manufacturer_identifier,
                normalized_manufacturer_identifier, identity_source_url,
                identity_source_title, identity_evidence_text, identity_evidence_kind,
                identity_confidence, introduced_year, estimated_unit_value_usd,
                value_basis, replacement_cost_usd, value_reference_year, value_source
              ) VALUES (
                (SELECT id FROM avionics_manufacturers WHERE normalized_name = 'inspector test'),
                'Legacy Unknown Unit', 'legacy unknown unit', 'sku', 'UNKNOWN-1',
                'unknown 1', 'https://manufacturer.example/unknown-1', 'Unknown unit sheet',
                'Manufacturer identity evidence.', 'authoritative_reference', 'very_high',
                2018, 1000, 'installed_contribution', 2000, 2026, 'Price list'
              )"#,
        )
        .await;
        execute(
            &db,
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) SELECT model.id, capability.id FROM avionics_models model CROSS JOIN avionics_types capability WHERE model.normalized_name = 'legacy unknown unit' AND capability.normalized_name = 'unknown'",
        )
        .await;
        execute(
            &db,
            "UPDATE avionics_models SET catalog_status = 'approved', catalog_reviewed_at = CURRENT_TIMESTAMP WHERE normalized_name = 'legacy unknown unit'",
        )
        .await;

        let page = list_avionics_catalog(
            &db,
            current_user_id,
            AvionicsCatalogQuery {
                search: Some("Legacy Unknown Unit".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].capabilities[0].value, "unknown");
        assert_eq!(
            page.items[0].completeness.blockers,
            vec!["missing_capability"]
        );
    }

    #[tokio::test]
    async fn invalid_filters_and_missing_details_are_typed() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let error = list_avionics_catalog(
            &db,
            user.id,
            AvionicsCatalogQuery {
                limit: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            super::AvionicsInspectionError::Validation(_)
        ));
        let error = get_avionics_catalog_detail(&db, user.id, i64::MAX)
            .await
            .unwrap_err();
        assert!(matches!(error, super::AvionicsInspectionError::NotFound(_)));
    }
}
