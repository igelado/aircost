use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{
    AircraftPricePointContext, AvionicsMetadataContext, AvionicsNormalizationCandidate,
    AvionicsNormalizationContext, DefaultAvionicsContext, GeminiListingExtractor,
};
use crate::normalize::{normalize_avionics_model_name, normalize_name};

const DEFAULT_VALUE_REFERENCE_YEAR: i64 = 2026;

macro_rules! execute_query {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|_| ())
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|_| ())
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

macro_rules! query_scalar_optional {
    ($db:expr, $ty:ty, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_optional(pool).await
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query_scalar::<_, $ty>(&sql)$(.bind($bind))*.fetch_optional(pool).await
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

#[derive(Debug)]
pub enum AvionicsStoreError {
    Database(String),
    Model(String),
}

impl std::fmt::Display for AvionicsStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AvionicsStoreError::Database(message) | AvionicsStoreError::Model(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl std::error::Error for AvionicsStoreError {}

impl From<sqlx::Error> for AvionicsStoreError {
    fn from(error: sqlx::Error) -> Self {
        AvionicsStoreError::Database(error.to_string())
    }
}

impl From<anyhow::Error> for AvionicsStoreError {
    fn from(error: anyhow::Error) -> Self {
        AvionicsStoreError::Model(error.to_string())
    }
}

type StoreResult<T> = Result<T, AvionicsStoreError>;

#[derive(Debug, FromRow)]
struct AvionicsModelReferenceRow {
    id: i64,
    manufacturer: String,
    model: String,
    avionics_type: String,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
}

#[derive(Clone, Debug, FromRow)]
struct AvionicsModelNormalizeRow {
    id: i64,
    avionics_manufacturer_id: i64,
    avionics_type_id: i64,
    name: String,
    normalized_name: String,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
    value_reference_year: Option<i64>,
    value_source: Option<String>,
}

#[derive(Debug, FromRow)]
struct AvionicsListingLinkRow {
    aircraft_sale_listing_id: i64,
    quantity: i64,
}

#[derive(Debug, FromRow)]
struct AvionicsDefaultProfileLinkRow {
    aircraft_model_variant_id: i64,
    model_year: i64,
    quantity: i64,
    source_url: String,
    source_title: String,
    source_notes: String,
    source_confidence: String,
}

#[derive(Clone, Debug, FromRow)]
struct AvionicsNormalizationInputRow {
    id: i64,
    manufacturer: String,
    avionics_type: String,
    model: String,
    normalized_model: String,
    listing_count: i64,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
}

#[derive(Debug, FromRow)]
struct AircraftModelYearProfileRow {
    aircraft_model_variant_id: i64,
    manufacturer: String,
    model: String,
    variant: String,
    model_year: i64,
    source_url: Option<String>,
    listing_count: i64,
}

#[derive(Debug, FromRow)]
struct NearbyAircraftPricePointRow {
    variant: String,
    model_year: i64,
    purchase_price_new_usd: f64,
    purchase_price_reference_year: i64,
    source_title: String,
    source_confidence: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsEnrichmentReport {
    pub applied: bool,
    pub value_reference_year: i64,
    pub items: Vec<AvionicsEnrichmentItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsEnrichmentItem {
    pub avionics_model_id: i64,
    pub manufacturer: String,
    pub model: String,
    pub avionics_type: String,
    pub previous_introduced_year: Option<i64>,
    pub previous_estimated_unit_value_usd: Option<f64>,
    pub introduced_year: i64,
    pub estimated_unit_value_usd: f64,
    pub confidence: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsNormalizationReport {
    pub applied: bool,
    pub items: Vec<AvionicsNormalizationItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsNormalizationItem {
    pub canonical_model_id: i64,
    pub canonical_name: String,
    pub canonical_normalized_name: String,
    pub source_model_ids: Vec<i64>,
    pub source_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsModelYearProfileReport {
    pub applied: bool,
    pub value_reference_year: i64,
    pub items: Vec<AvionicsModelYearProfileItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsModelYearProfileItem {
    pub aircraft_model_variant_id: i64,
    pub manufacturer: String,
    pub model: String,
    pub variant: String,
    pub model_year: i64,
    pub listing_count: i64,
    pub purchase_price_new_usd: f64,
    pub purchase_price_reference_year: i64,
    pub price_source_url: String,
    pub price_source_title: String,
    pub price_source_notes: String,
    pub price_source_confidence: String,
    pub avionics: Vec<AvionicsModelYearProfileAvionicsItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvionicsModelYearProfileAvionicsItem {
    pub avionics_model_id: i64,
    pub manufacturer: String,
    pub model: String,
    pub avionics_type: String,
    pub quantity: i64,
    pub introduced_year: i64,
    pub estimated_unit_value_usd: f64,
    pub confidence: String,
    pub source_url: String,
    pub source_title: String,
    pub notes: String,
}

pub async fn normalize_avionics_models(
    db: &AppDb,
    apply: bool,
) -> StoreResult<AvionicsNormalizationReport> {
    let rows = avionics_models_for_normalization(db).await?;
    let mut groups: BTreeMap<(i64, i64, String), Vec<AvionicsModelNormalizeRow>> = BTreeMap::new();
    for row in rows {
        let canonical = normalize_avionics_model_name(&row.name);
        groups
            .entry((
                row.avionics_manufacturer_id,
                row.avionics_type_id,
                canonical,
            ))
            .or_default()
            .push(row);
    }

    let mut items = Vec::new();
    for ((_manufacturer_id, _avionics_type_id, canonical_normalized_name), rows) in groups {
        let needs_normalization = rows.len() > 1
            || rows
                .iter()
                .any(|row| row.normalized_name != canonical_normalized_name);
        if !needs_normalization {
            continue;
        }
        let canonical = rows
            .iter()
            .min_by_key(|row| {
                (
                    row.introduced_year.is_none() || row.estimated_unit_value_usd.is_none(),
                    row.id,
                )
            })
            .expect("normalization group is not empty")
            .clone();
        let item = AvionicsNormalizationItem {
            canonical_model_id: canonical.id,
            canonical_name: canonical.name.clone(),
            canonical_normalized_name: canonical_normalized_name.clone(),
            source_model_ids: rows.iter().map(|row| row.id).collect(),
            source_names: rows.iter().map(|row| row.name.clone()).collect(),
        };
        if apply {
            apply_avionics_normalization_group(db, &canonical, &canonical_normalized_name, &rows)
                .await?;
        }
        items.push(item);
    }

    Ok(AvionicsNormalizationReport {
        applied: apply,
        items,
    })
}

pub async fn enrich_missing_avionics_metadata(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    limit: i64,
    value_reference_year: Option<i64>,
    refresh_existing: bool,
) -> StoreResult<AvionicsEnrichmentReport> {
    if limit < 1 {
        return Err(AvionicsStoreError::Model(
            "limit must be at least 1".to_string(),
        ));
    }
    let value_reference_year = value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR);
    let rows = avionics_models_to_enrich(db, limit, refresh_existing).await?;
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let response = extractor
            .estimate_avionics_metadata(&AvionicsMetadataContext {
                manufacturer: &row.manufacturer,
                model: &row.model,
                avionics_type: &row.avionics_type,
                value_reference_year,
            })
            .await?;
        let item = enrichment_item_from_response(&row, &response)?;
        if apply {
            update_avionics_metadata(db, &item, value_reference_year, refresh_existing).await?;
        }
        items.push(item);
    }

    Ok(AvionicsEnrichmentReport {
        applied: apply,
        value_reference_year,
        items,
    })
}

pub async fn enrich_listing_avionics_metadata(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    listing_id: i64,
    value_reference_year: Option<i64>,
    refresh_existing: bool,
) -> StoreResult<AvionicsEnrichmentReport> {
    let value_reference_year = value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR);
    let rows = listing_avionics_models_to_enrich(db, listing_id, refresh_existing).await?;
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let response = extractor
            .estimate_avionics_metadata(&AvionicsMetadataContext {
                manufacturer: &row.manufacturer,
                model: &row.model,
                avionics_type: &row.avionics_type,
                value_reference_year,
            })
            .await?;
        let item = enrichment_item_from_response(&row, &response)?;
        if apply {
            update_avionics_metadata(db, &item, value_reference_year, refresh_existing).await?;
        }
        items.push(item);
    }

    Ok(AvionicsEnrichmentReport {
        applied: apply,
        value_reference_year,
        items,
    })
}

pub async fn curate_avionics_models_with_gemini(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    limit: i64,
) -> StoreResult<AvionicsNormalizationReport> {
    if limit < 1 {
        return Err(AvionicsStoreError::Model(
            "limit must be at least 1".to_string(),
        ));
    }
    let rows = avionics_models_for_gemini_normalization(db, limit).await?;
    let mut groups: BTreeMap<(String, String), Vec<AvionicsNormalizationInputRow>> =
        BTreeMap::new();
    for row in rows {
        groups
            .entry((row.manufacturer.clone(), row.avionics_type.clone()))
            .or_default()
            .push(row);
    }

    let mut items = Vec::new();
    for ((manufacturer, avionics_type), rows) in groups {
        if rows.len() < 2 {
            continue;
        }
        let context = AvionicsNormalizationContext {
            manufacturer,
            avionics_type,
            models: rows
                .iter()
                .map(|row| AvionicsNormalizationCandidate {
                    id: row.id,
                    model: row.model.clone(),
                    normalized_model: row.normalized_model.clone(),
                    listing_count: row.listing_count,
                    introduced_year: row.introduced_year,
                    estimated_unit_value_usd: row.estimated_unit_value_usd,
                })
                .collect(),
        };
        let response = extractor.normalize_avionics_model_labels(&context).await?;
        let response_groups = avionics_normalization_groups_from_response(&response, &rows)?;
        for response_group in response_groups {
            if response_group.rows.len() < 2
                && response_group.rows.iter().all(|row| {
                    normalize_avionics_model_name(&row.model)
                        == normalize_avionics_model_name(&response_group.canonical_model)
                })
            {
                continue;
            }
            let canonical_normalized_name =
                normalize_avionics_model_name(&response_group.canonical_model);
            let normalize_rows = avionics_normalize_rows_for_ids(
                db,
                &response_group
                    .rows
                    .iter()
                    .map(|row| row.id)
                    .collect::<Vec<_>>(),
            )
            .await?;
            let mut normalize_rows = normalize_rows;
            let manufacturer_id = normalize_rows
                .first()
                .map(|row| row.avionics_manufacturer_id)
                .ok_or_else(|| {
                    AvionicsStoreError::Model(
                        "Gemini avionics normalization group had no rows".to_string(),
                    )
                })?;
            let avionics_type_id = normalize_rows
                .first()
                .map(|row| row.avionics_type_id)
                .expect("normalization group has a first row");
            for collision in avionics_normalize_rows_for_manufacturer_normalized(
                db,
                manufacturer_id,
                avionics_type_id,
                &canonical_normalized_name,
            )
            .await?
            {
                if !normalize_rows.iter().any(|row| row.id == collision.id) {
                    normalize_rows.push(collision);
                }
            }
            let canonical_row = normalize_rows
                .iter()
                .min_by_key(|row| {
                    (
                        row.introduced_year.is_none() || row.estimated_unit_value_usd.is_none(),
                        row.id,
                    )
                })
                .expect("normalization response group is not empty")
                .clone();
            let item = AvionicsNormalizationItem {
                canonical_model_id: canonical_row.id,
                canonical_name: response_group.canonical_model.clone(),
                canonical_normalized_name: canonical_normalized_name.clone(),
                source_model_ids: normalize_rows.iter().map(|row| row.id).collect(),
                source_names: normalize_rows.iter().map(|row| row.name.clone()).collect(),
            };
            if apply {
                apply_avionics_normalization_group(
                    db,
                    &canonical_row,
                    &canonical_normalized_name,
                    &normalize_rows,
                )
                .await?;
                execute_query!(
                    db,
                    r#"
                    UPDATE avionics_models
                    SET name = ?, updated_at = CURRENT_TIMESTAMP
                    WHERE id = ?
                    "#,
                    response_group.canonical_model.as_str(),
                    canonical_row.id
                )?;
            }
            items.push(item);
        }
    }

    Ok(AvionicsNormalizationReport {
        applied: apply,
        items,
    })
}

pub async fn enrich_model_year_avionics_and_price_points(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    limit: i64,
    value_reference_year: Option<i64>,
    refresh_existing: bool,
) -> StoreResult<AvionicsModelYearProfileReport> {
    if limit < 1 {
        return Err(AvionicsStoreError::Model(
            "limit must be at least 1".to_string(),
        ));
    }
    let value_reference_year = value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR);
    let rows = aircraft_model_year_profiles_to_enrich(db, limit, refresh_existing).await?;
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let nearby_price_points =
            nearby_model_family_price_points(db, row.aircraft_model_variant_id, row.model_year)
                .await?;
        let response = extractor
            .estimate_default_aircraft_avionics(&DefaultAvionicsContext {
                manufacturer: &row.manufacturer,
                model: &row.model,
                variant: &row.variant,
                model_year: row.model_year,
                value_reference_year,
                source_url: row.source_url.as_deref(),
                nearby_price_points: &nearby_price_points,
            })
            .await?;
        let mut item = model_year_profile_item_from_response(&row, &response)?;
        if apply {
            upsert_model_year_price_point(db, &item).await?;
            for avionics in &mut item.avionics {
                upsert_default_avionics_profile_item(
                    db,
                    row.aircraft_model_variant_id,
                    row.model_year,
                    avionics,
                )
                .await?;
            }
        }
        items.push(item);
    }

    Ok(AvionicsModelYearProfileReport {
        applied: apply,
        value_reference_year,
        items,
    })
}

pub async fn enrich_model_year_avionics_and_price_point_for_listing(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    listing_id: i64,
    value_reference_year: Option<i64>,
    refresh_existing: bool,
) -> StoreResult<Option<AvionicsModelYearProfileItem>> {
    let value_reference_year = value_reference_year.unwrap_or(DEFAULT_VALUE_REFERENCE_YEAR);
    let Some(row) =
        aircraft_model_year_profile_for_listing(db, listing_id, refresh_existing).await?
    else {
        return Ok(None);
    };
    let nearby_price_points =
        nearby_model_family_price_points(db, row.aircraft_model_variant_id, row.model_year).await?;
    let response = extractor
        .estimate_default_aircraft_avionics(&DefaultAvionicsContext {
            manufacturer: &row.manufacturer,
            model: &row.model,
            variant: &row.variant,
            model_year: row.model_year,
            value_reference_year,
            source_url: row.source_url.as_deref(),
            nearby_price_points: &nearby_price_points,
        })
        .await?;
    let mut item = model_year_profile_item_from_response(&row, &response)?;
    if apply {
        upsert_model_year_price_point(db, &item).await?;
        for avionics in &mut item.avionics {
            upsert_default_avionics_profile_item(
                db,
                row.aircraft_model_variant_id,
                row.model_year,
                avionics,
            )
            .await?;
        }
    }
    Ok(Some(item))
}

async fn avionics_models_for_normalization(
    db: &AppDb,
) -> StoreResult<Vec<AvionicsModelNormalizeRow>> {
    Ok(query_as_all!(
        db,
        AvionicsModelNormalizeRow,
        r#"
        SELECT
          id,
          avionics_manufacturer_id,
          avionics_type_id,
          name,
          normalized_name,
          introduced_year,
          estimated_unit_value_usd,
          value_reference_year,
          value_source
        FROM avionics_models
        ORDER BY id
        "#
    )?)
}

async fn avionics_models_for_gemini_normalization(
    db: &AppDb,
    limit: i64,
) -> StoreResult<Vec<AvionicsNormalizationInputRow>> {
    Ok(query_as_all!(
        db,
        AvionicsNormalizationInputRow,
        r#"
        SELECT
          model.id,
          mfr.name AS manufacturer,
          avionics_type.name AS avionics_type,
          model.name AS model,
          model.normalized_name AS normalized_model,
          COUNT(link.id) AS listing_count,
          model.introduced_year,
          model.estimated_unit_value_usd
        FROM avionics_models model
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        JOIN avionics_types avionics_type
          ON avionics_type.id = model.avionics_type_id
        LEFT JOIN aircraft_sale_listing_avionics link
          ON link.avionics_model_id = model.id
        GROUP BY
          model.id,
          mfr.name,
          avionics_type.name,
          model.name,
          model.normalized_name,
          model.introduced_year,
          model.estimated_unit_value_usd
        ORDER BY mfr.name, avionics_type.name, listing_count DESC, model.name
        LIMIT ?
        "#,
        limit
    )?)
}

struct AvionicsResponseNormalizationGroup {
    canonical_model: String,
    rows: Vec<AvionicsNormalizationInputRow>,
}

fn avionics_normalization_groups_from_response(
    response: &Value,
    rows: &[AvionicsNormalizationInputRow],
) -> StoreResult<Vec<AvionicsResponseNormalizationGroup>> {
    let groups = response
        .get("groups")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AvionicsStoreError::Model(
                "Gemini avionics normalization response missing groups".to_string(),
            )
        })?;
    let mut remaining = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    remaining.sort_unstable();
    let mut seen = Vec::new();
    let mut output = Vec::new();

    for group in groups {
        let canonical_model = required_string(group, "canonical_model")?;
        let source_ids = group
            .get("source_ids")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AvionicsStoreError::Model(
                    "Gemini avionics normalization group missing source_ids".to_string(),
                )
            })?
            .iter()
            .map(|value| {
                value.as_i64().ok_or_else(|| {
                    AvionicsStoreError::Model(
                        "Gemini avionics normalization source_ids must be integers".to_string(),
                    )
                })
            })
            .collect::<StoreResult<Vec<_>>>()?;
        let mut group_rows = Vec::new();
        for source_id in source_ids {
            if !remaining.binary_search(&source_id).is_ok() {
                return Err(AvionicsStoreError::Model(format!(
                    "Gemini avionics normalization source id not in input: {source_id}"
                )));
            }
            if seen.contains(&source_id) {
                return Err(AvionicsStoreError::Model(format!(
                    "Gemini avionics normalization source id repeated: {source_id}"
                )));
            }
            seen.push(source_id);
            let row = rows
                .iter()
                .find(|row| row.id == source_id)
                .expect("source id was checked against input")
                .clone();
            group_rows.push(row);
        }
        if !group_rows.is_empty() {
            output.push(AvionicsResponseNormalizationGroup {
                canonical_model,
                rows: group_rows,
            });
        }
    }

    seen.sort_unstable();
    if seen != remaining {
        return Err(AvionicsStoreError::Model(
            "Gemini avionics normalization did not cover every input id exactly once".to_string(),
        ));
    }
    Ok(output)
}

async fn avionics_normalize_rows_for_ids(
    db: &AppDb,
    ids: &[i64],
) -> StoreResult<Vec<AvionicsModelNormalizeRow>> {
    let id_set = ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    Ok(avionics_models_for_normalization(db)
        .await?
        .into_iter()
        .filter(|row| id_set.contains(&row.id))
        .collect())
}

async fn avionics_normalize_rows_for_manufacturer_normalized(
    db: &AppDb,
    manufacturer_id: i64,
    avionics_type_id: i64,
    normalized_name: &str,
) -> StoreResult<Vec<AvionicsModelNormalizeRow>> {
    Ok(query_as_all!(
        db,
        AvionicsModelNormalizeRow,
        r#"
        SELECT
          id,
          avionics_manufacturer_id,
          avionics_type_id,
          name,
          normalized_name,
          introduced_year,
          estimated_unit_value_usd,
          value_reference_year,
          value_source
        FROM avionics_models
        WHERE avionics_manufacturer_id = ?
          AND avionics_type_id = ?
          AND normalized_name = ?
        ORDER BY id
        "#,
        manufacturer_id,
        avionics_type_id,
        normalized_name
    )?)
}

async fn aircraft_model_year_profiles_to_enrich(
    db: &AppDb,
    limit: i64,
    refresh_existing: bool,
) -> StoreResult<Vec<AircraftModelYearProfileRow>> {
    let predicate = if refresh_existing {
        "1 = 1"
    } else {
        r#"
        (
          NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_price_points price_point
            WHERE price_point.aircraft_model_variant_id = variant.id
              AND price_point.model_year = listing.model_year
          )
          OR NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_default_avionics default_avionics
            WHERE default_avionics.aircraft_model_variant_id = variant.id
              AND default_avionics.model_year = listing.model_year
          )
        )
        "#
    };
    let sql = format!(
        r#"
        SELECT
          variant.id AS aircraft_model_variant_id,
          mfr.name AS manufacturer,
          model.name AS model,
          variant.name AS variant,
          listing.model_year,
          MIN(listing.source_url) AS source_url,
          COUNT(listing.id) AS listing_count
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE {predicate}
        GROUP BY
          variant.id,
          mfr.name,
          model.name,
          variant.name,
          listing.model_year
        ORDER BY listing_count DESC, mfr.name, model.name, variant.name, listing.model_year
        LIMIT ?
        "#
    );
    Ok(query_as_all!(db, AircraftModelYearProfileRow, &sql, limit)?)
}

async fn aircraft_model_year_profile_for_listing(
    db: &AppDb,
    listing_id: i64,
    refresh_existing: bool,
) -> StoreResult<Option<AircraftModelYearProfileRow>> {
    let predicate = if refresh_existing {
        "1 = 1"
    } else {
        r#"
        (
          NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_price_points price_point
            WHERE price_point.aircraft_model_variant_id = variant.id
              AND price_point.model_year = listing.model_year
          )
          OR NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_default_avionics default_avionics
            WHERE default_avionics.aircraft_model_variant_id = variant.id
              AND default_avionics.model_year = listing.model_year
          )
        )
        "#
    };
    let sql = format!(
        r#"
        SELECT
          variant.id AS aircraft_model_variant_id,
          mfr.name AS manufacturer,
          model.name AS model,
          variant.name AS variant,
          listing.model_year,
          listing.source_url,
          1 AS listing_count
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE listing.id = ?
          AND {predicate}
        LIMIT 1
        "#
    );
    Ok(
        query_as_all!(db, AircraftModelYearProfileRow, &sql, listing_id)?
            .into_iter()
            .next(),
    )
}

async fn nearby_model_family_price_points(
    db: &AppDb,
    aircraft_model_variant_id: i64,
    model_year: i64,
) -> StoreResult<Vec<AircraftPricePointContext>> {
    let rows = query_as_all!(
        db,
        NearbyAircraftPricePointRow,
        r#"
        SELECT
          variant.name AS variant,
          price_point.model_year,
          price_point.purchase_price_new_usd,
          price_point.purchase_price_reference_year,
          price_point.source_title,
          price_point.source_confidence
        FROM aircraft_model_variant_price_points price_point
        JOIN aircraft_model_variants variant
          ON variant.id = price_point.aircraft_model_variant_id
        WHERE variant.aircraft_model_id = (
            SELECT source_variant.aircraft_model_id
            FROM aircraft_model_variants source_variant
            WHERE source_variant.id = ?
          )
          AND NOT (
            price_point.aircraft_model_variant_id = ?
            AND price_point.model_year = ?
          )
          AND price_point.model_year BETWEEN ? AND ?
        ORDER BY ABS(price_point.model_year - ?), price_point.model_year
        LIMIT 8
        "#,
        aircraft_model_variant_id,
        aircraft_model_variant_id,
        model_year,
        model_year - 5,
        model_year + 5,
        model_year
    )?;
    Ok(rows
        .into_iter()
        .map(|row| AircraftPricePointContext {
            variant: row.variant,
            model_year: row.model_year,
            purchase_price_new_usd: row.purchase_price_new_usd,
            purchase_price_reference_year: row.purchase_price_reference_year,
            source_title: row.source_title,
            source_confidence: row.source_confidence,
        })
        .collect())
}

fn model_year_profile_item_from_response(
    row: &AircraftModelYearProfileRow,
    response: &Value,
) -> StoreResult<AvionicsModelYearProfileItem> {
    let purchase_price_new_usd = required_min_f64(response, "purchase_price_new_usd", 10_000.0)?;
    let purchase_price_reference_year = required_year(response, "purchase_price_reference_year")?;
    let price_source_url = required_string(response, "price_source_url")?;
    if !(price_source_url.starts_with("https://") || price_source_url.starts_with("http://")) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini model-year profile price_source_url must be http(s): {price_source_url}"
        )));
    }
    if row.source_url.as_deref() == Some(price_source_url.as_str())
        || looks_like_used_listing_url(&price_source_url)
    {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini model-year profile price_source_url must cite a new-price reference, not an ordinary listing URL: {price_source_url}"
        )));
    }
    let avionics = response
        .get("avionics")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AvionicsStoreError::Model(
                "Gemini model-year profile response missing avionics".to_string(),
            )
        })?
        .iter()
        .map(default_avionics_item_from_response)
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(AvionicsModelYearProfileItem {
        aircraft_model_variant_id: row.aircraft_model_variant_id,
        manufacturer: row.manufacturer.clone(),
        model: row.model.clone(),
        variant: row.variant.clone(),
        model_year: row.model_year,
        listing_count: row.listing_count,
        purchase_price_new_usd,
        purchase_price_reference_year,
        price_source_url,
        price_source_title: required_string(response, "price_source_title")?,
        price_source_notes: required_string(response, "price_source_notes")?,
        price_source_confidence: required_string(response, "price_source_confidence")?,
        avionics,
    })
}

fn looks_like_used_listing_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("/listing/for-sale/") || lower.contains("/listings/for-sale/")
}

fn default_avionics_item_from_response(
    value: &Value,
) -> StoreResult<AvionicsModelYearProfileAvionicsItem> {
    let quantity = required_i64(value, "quantity")?.max(1);
    let introduced_year = required_year(value, "introduced_year")?;
    let estimated_unit_value_usd = required_min_f64(value, "estimated_unit_value_usd", 0.0)?;
    let source_url = required_string(value, "source_url")?;
    if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini default avionics source_url must be http(s): {source_url}"
        )));
    }
    Ok(AvionicsModelYearProfileAvionicsItem {
        avionics_model_id: 0,
        manufacturer: required_string(value, "manufacturer")?,
        model: required_string(value, "model")?,
        avionics_type: required_string(value, "type")?,
        quantity,
        introduced_year,
        estimated_unit_value_usd,
        confidence: required_string(value, "confidence")?,
        source_url,
        source_title: required_string(value, "source_title")?,
        notes: required_string(value, "notes")?,
    })
}

async fn upsert_model_year_price_point(
    db: &AppDb,
    item: &AvionicsModelYearProfileItem,
) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
        INSERT INTO aircraft_model_variant_price_points (
          aircraft_model_variant_id,
          model_year,
          purchase_price_new_usd,
          purchase_price_reference_year,
          source_url,
          source_title,
          source_notes,
          source_confidence
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (aircraft_model_variant_id, model_year) DO UPDATE SET
          purchase_price_new_usd = excluded.purchase_price_new_usd,
          purchase_price_reference_year = excluded.purchase_price_reference_year,
          source_url = excluded.source_url,
          source_title = excluded.source_title,
          source_notes = excluded.source_notes,
          source_confidence = excluded.source_confidence,
          updated_at = CURRENT_TIMESTAMP
        "#,
        item.aircraft_model_variant_id,
        item.model_year,
        item.purchase_price_new_usd,
        item.purchase_price_reference_year,
        item.price_source_url.as_str(),
        item.price_source_title.as_str(),
        item.price_source_notes.as_str(),
        item.price_source_confidence.as_str(),
    )?;
    Ok(())
}

async fn upsert_default_avionics_profile_item(
    db: &AppDb,
    aircraft_model_variant_id: i64,
    model_year: i64,
    item: &mut AvionicsModelYearProfileAvionicsItem,
) -> StoreResult<()> {
    let avionics_model_id =
        ensure_avionics_model(db, &item.manufacturer, &item.model, &item.avionics_type).await?;
    item.avionics_model_id = avionics_model_id;
    update_avionics_model_metadata(
        db,
        avionics_model_id,
        item.introduced_year,
        item.estimated_unit_value_usd,
        DEFAULT_VALUE_REFERENCE_YEAR,
        "gemini-grounded",
    )
    .await?;
    execute_query!(
        db,
        r#"
        INSERT INTO aircraft_model_variant_default_avionics (
          aircraft_model_variant_id,
          model_year,
          avionics_model_id,
          quantity,
          source_url,
          source_title,
          source_notes,
          source_confidence
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (aircraft_model_variant_id, model_year, avionics_model_id) DO UPDATE SET
          quantity = excluded.quantity,
          source_url = excluded.source_url,
          source_title = excluded.source_title,
          source_notes = excluded.source_notes,
          source_confidence = excluded.source_confidence,
          updated_at = CURRENT_TIMESTAMP
        "#,
        aircraft_model_variant_id,
        model_year,
        avionics_model_id,
        item.quantity,
        item.source_url.as_str(),
        item.source_title.as_str(),
        item.notes.as_str(),
        item.confidence.as_str(),
    )?;
    Ok(())
}

async fn apply_avionics_normalization_group(
    db: &AppDb,
    canonical: &AvionicsModelNormalizeRow,
    canonical_normalized_name: &str,
    rows: &[AvionicsModelNormalizeRow],
) -> StoreResult<()> {
    for row in rows.iter().filter(|row| row.id != canonical.id) {
        let links = avionics_listing_links_for_model(db, row.id).await?;
        for link in links {
            upsert_listing_avionics_link(
                db,
                link.aircraft_sale_listing_id,
                canonical.id,
                link.quantity,
            )
            .await?;
        }
        execute_query!(
            db,
            "DELETE FROM aircraft_sale_listing_avionics WHERE avionics_model_id = ?",
            row.id
        )?;

        let default_links = avionics_default_profile_links_for_model(db, row.id).await?;
        for link in default_links {
            upsert_default_avionics_profile_link(db, canonical.id, &link).await?;
        }
        execute_query!(
            db,
            "DELETE FROM aircraft_model_variant_default_avionics WHERE avionics_model_id = ?",
            row.id
        )?;

        execute_query!(db, "DELETE FROM avionics_models WHERE id = ?", row.id)?;
    }

    let introduced_year = canonical
        .introduced_year
        .or_else(|| rows.iter().find_map(|row| row.introduced_year));
    let estimated_unit_value_usd = canonical
        .estimated_unit_value_usd
        .or_else(|| rows.iter().find_map(|row| row.estimated_unit_value_usd));
    let value_reference_year = canonical
        .value_reference_year
        .or_else(|| rows.iter().find_map(|row| row.value_reference_year));
    let value_source = canonical
        .value_source
        .clone()
        .or_else(|| rows.iter().find_map(|row| row.value_source.clone()));

    execute_query!(
        db,
        r#"
        UPDATE avionics_models
        SET
          normalized_name = ?,
          introduced_year = COALESCE(introduced_year, ?),
          estimated_unit_value_usd = COALESCE(estimated_unit_value_usd, ?),
          value_reference_year = COALESCE(value_reference_year, ?),
          value_source = COALESCE(value_source, ?),
          updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        canonical_normalized_name,
        introduced_year,
        estimated_unit_value_usd,
        value_reference_year,
        value_source.as_deref(),
        canonical.id
    )?;
    Ok(())
}

async fn avionics_listing_links_for_model(
    db: &AppDb,
    avionics_model_id: i64,
) -> StoreResult<Vec<AvionicsListingLinkRow>> {
    Ok(query_as_all!(
        db,
        AvionicsListingLinkRow,
        r#"
        SELECT aircraft_sale_listing_id, quantity
        FROM aircraft_sale_listing_avionics
        WHERE avionics_model_id = ?
        "#,
        avionics_model_id
    )?)
}

async fn avionics_default_profile_links_for_model(
    db: &AppDb,
    avionics_model_id: i64,
) -> StoreResult<Vec<AvionicsDefaultProfileLinkRow>> {
    Ok(query_as_all!(
        db,
        AvionicsDefaultProfileLinkRow,
        r#"
        SELECT
          aircraft_model_variant_id,
          model_year,
          quantity,
          source_url,
          source_title,
          source_notes,
          source_confidence
        FROM aircraft_model_variant_default_avionics
        WHERE avionics_model_id = ?
        "#,
        avionics_model_id
    )?)
}

async fn upsert_listing_avionics_link(
    db: &AppDb,
    listing_id: i64,
    avionics_model_id: i64,
    quantity: i64,
) -> StoreResult<()> {
    let existing_quantity = query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT quantity
        FROM aircraft_sale_listing_avionics
        WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?
        "#,
        listing_id,
        avionics_model_id
    )?;
    match existing_quantity {
        Some(existing_quantity) => {
            execute_query!(
                db,
                r#"
                UPDATE aircraft_sale_listing_avionics
                SET quantity = ?, updated_at = CURRENT_TIMESTAMP
                WHERE aircraft_sale_listing_id = ? AND avionics_model_id = ?
                "#,
                existing_quantity.max(quantity),
                listing_id,
                avionics_model_id
            )?;
        }
        None => {
            execute_query!(
                db,
                r#"
                INSERT INTO aircraft_sale_listing_avionics (
                  aircraft_sale_listing_id,
                  avionics_model_id,
                  quantity
                )
                VALUES (?, ?, ?)
                "#,
                listing_id,
                avionics_model_id,
                quantity.max(1)
            )?;
        }
    }
    Ok(())
}

async fn upsert_default_avionics_profile_link(
    db: &AppDb,
    avionics_model_id: i64,
    link: &AvionicsDefaultProfileLinkRow,
) -> StoreResult<()> {
    let existing_quantity = query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT quantity
        FROM aircraft_model_variant_default_avionics
        WHERE aircraft_model_variant_id = ?
          AND model_year = ?
          AND avionics_model_id = ?
        "#,
        link.aircraft_model_variant_id,
        link.model_year,
        avionics_model_id
    )?;
    match existing_quantity {
        Some(existing_quantity) => {
            execute_query!(
                db,
                r#"
                UPDATE aircraft_model_variant_default_avionics
                SET
                  quantity = ?,
                  source_url = ?,
                  source_title = ?,
                  source_notes = ?,
                  source_confidence = ?,
                  updated_at = CURRENT_TIMESTAMP
                WHERE aircraft_model_variant_id = ?
                  AND model_year = ?
                  AND avionics_model_id = ?
                "#,
                existing_quantity.max(link.quantity).max(1),
                link.source_url.as_str(),
                link.source_title.as_str(),
                link.source_notes.as_str(),
                link.source_confidence.as_str(),
                link.aircraft_model_variant_id,
                link.model_year,
                avionics_model_id
            )?;
        }
        None => {
            execute_query!(
                db,
                r#"
                INSERT INTO aircraft_model_variant_default_avionics (
                  aircraft_model_variant_id,
                  model_year,
                  avionics_model_id,
                  quantity,
                  source_url,
                  source_title,
                  source_notes,
                  source_confidence
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                link.aircraft_model_variant_id,
                link.model_year,
                avionics_model_id,
                link.quantity.max(1),
                link.source_url.as_str(),
                link.source_title.as_str(),
                link.source_notes.as_str(),
                link.source_confidence.as_str()
            )?;
        }
    }
    Ok(())
}

async fn avionics_models_to_enrich(
    db: &AppDb,
    limit: i64,
    refresh_existing: bool,
) -> StoreResult<Vec<AvionicsModelReferenceRow>> {
    if refresh_existing {
        Ok(query_as_all!(
            db,
            AvionicsModelReferenceRow,
            r#"
            SELECT
              model.id,
              mfr.name AS manufacturer,
              model.name AS model,
              avionics_type.name AS avionics_type,
              model.introduced_year,
              model.estimated_unit_value_usd
            FROM avionics_models model
            JOIN avionics_manufacturers mfr
              ON mfr.id = model.avionics_manufacturer_id
            JOIN avionics_types avionics_type
              ON avionics_type.id = model.avionics_type_id
            ORDER BY model.id
            LIMIT ?
            "#,
            limit
        )?)
    } else {
        Ok(query_as_all!(
            db,
            AvionicsModelReferenceRow,
            r#"
            SELECT
              model.id,
              mfr.name AS manufacturer,
              model.name AS model,
              avionics_type.name AS avionics_type,
              model.introduced_year,
              model.estimated_unit_value_usd
            FROM avionics_models model
            JOIN avionics_manufacturers mfr
              ON mfr.id = model.avionics_manufacturer_id
            JOIN avionics_types avionics_type
              ON avionics_type.id = model.avionics_type_id
            WHERE model.introduced_year IS NULL
               OR model.estimated_unit_value_usd IS NULL
               OR model.value_reference_year IS NULL
            ORDER BY model.id
            LIMIT ?
            "#,
            limit
        )?)
    }
}

async fn listing_avionics_models_to_enrich(
    db: &AppDb,
    listing_id: i64,
    refresh_existing: bool,
) -> StoreResult<Vec<AvionicsModelReferenceRow>> {
    if refresh_existing {
        Ok(query_as_all!(
            db,
            AvionicsModelReferenceRow,
            r#"
            SELECT DISTINCT
              model.id,
              mfr.name AS manufacturer,
              model.name AS model,
              avionics_type.name AS avionics_type,
              model.introduced_year,
              model.estimated_unit_value_usd
            FROM aircraft_sale_listing_avionics link
            JOIN avionics_models model
              ON model.id = link.avionics_model_id
            JOIN avionics_manufacturers mfr
              ON mfr.id = model.avionics_manufacturer_id
            JOIN avionics_types avionics_type
              ON avionics_type.id = model.avionics_type_id
            WHERE link.aircraft_sale_listing_id = ?
            ORDER BY model.id
            "#,
            listing_id
        )?)
    } else {
        Ok(query_as_all!(
            db,
            AvionicsModelReferenceRow,
            r#"
            SELECT DISTINCT
              model.id,
              mfr.name AS manufacturer,
              model.name AS model,
              avionics_type.name AS avionics_type,
              model.introduced_year,
              model.estimated_unit_value_usd
            FROM aircraft_sale_listing_avionics link
            JOIN avionics_models model
              ON model.id = link.avionics_model_id
            JOIN avionics_manufacturers mfr
              ON mfr.id = model.avionics_manufacturer_id
            JOIN avionics_types avionics_type
              ON avionics_type.id = model.avionics_type_id
            WHERE link.aircraft_sale_listing_id = ?
              AND (
                model.introduced_year IS NULL
                OR model.estimated_unit_value_usd IS NULL
                OR model.value_reference_year IS NULL
              )
            ORDER BY model.id
            "#,
            listing_id
        )?)
    }
}

fn enrichment_item_from_response(
    row: &AvionicsModelReferenceRow,
    response: &Value,
) -> StoreResult<AvionicsEnrichmentItem> {
    let introduced_year = response
        .get("introduced_year")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            AvionicsStoreError::Model(
                "Gemini avionics response missing introduced_year".to_string(),
            )
        })?;
    if !(1940..=2100).contains(&introduced_year) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response introduced_year out of range: {introduced_year}"
        )));
    }
    let estimated_unit_value_usd = response
        .get("estimated_unit_value_usd")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            AvionicsStoreError::Model(
                "Gemini avionics response missing estimated_unit_value_usd".to_string(),
            )
        })?;
    if estimated_unit_value_usd < 0.0 {
        return Err(AvionicsStoreError::Model(
            "Gemini avionics response estimated_unit_value_usd must be non-negative".to_string(),
        ));
    }
    let confidence = response
        .get("confidence")
        .and_then(Value::as_str)
        .unwrap_or("low")
        .trim()
        .to_string();
    Ok(AvionicsEnrichmentItem {
        avionics_model_id: row.id,
        manufacturer: row.manufacturer.clone(),
        model: row.model.clone(),
        avionics_type: row.avionics_type.clone(),
        previous_introduced_year: row.introduced_year,
        previous_estimated_unit_value_usd: row.estimated_unit_value_usd,
        introduced_year,
        estimated_unit_value_usd,
        confidence,
    })
}

async fn update_avionics_metadata(
    db: &AppDb,
    item: &AvionicsEnrichmentItem,
    value_reference_year: i64,
    overwrite_existing: bool,
) -> StoreResult<()> {
    let value_source = "gemini";
    if overwrite_existing {
        execute_query!(
            db,
            r#"
            UPDATE avionics_models
            SET
              introduced_year = ?,
              estimated_unit_value_usd = ?,
              value_reference_year = ?,
              value_source = ?,
              updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            item.introduced_year,
            item.estimated_unit_value_usd,
            value_reference_year,
            value_source,
            item.avionics_model_id
        )?;
    } else {
        execute_query!(
            db,
            r#"
            UPDATE avionics_models
            SET
              introduced_year = COALESCE(introduced_year, ?),
              estimated_unit_value_usd = COALESCE(estimated_unit_value_usd, ?),
              value_reference_year = ?,
              value_source = ?,
              updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            item.introduced_year,
            item.estimated_unit_value_usd,
            value_reference_year,
            value_source,
            item.avionics_model_id
        )?;
    }
    Ok(())
}

async fn ensure_avionics_model(
    db: &AppDb,
    manufacturer: &str,
    model: &str,
    avionics_type: &str,
) -> StoreResult<i64> {
    let manufacturer_id = ensure_named_row(db, "avionics_manufacturers", manufacturer).await?;
    let type_id = ensure_named_row(db, "avionics_types", avionics_type).await?;
    let normalized_model = normalize_avionics_model_name(model);
    execute_query!(
        db,
        r#"
        INSERT INTO avionics_models (
          avionics_manufacturer_id,
          avionics_type_id,
          name,
          normalized_name
        )
        VALUES (?, ?, ?, ?)
        ON CONFLICT (avionics_manufacturer_id, avionics_type_id, normalized_name) DO NOTHING
        "#,
        manufacturer_id,
        type_id,
        model.trim(),
        normalized_model.as_str()
    )?;
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT id
        FROM avionics_models
        WHERE avionics_manufacturer_id = ?
          AND avionics_type_id = ?
          AND normalized_name = ?
        "#,
        manufacturer_id,
        type_id,
        normalized_model.as_str()
    )?)
}

async fn ensure_named_row(db: &AppDb, table: &str, name: &str) -> StoreResult<i64> {
    let normalized_name = normalize_name(name);
    let insert_sql = format!(
        "INSERT INTO {table} (name, normalized_name) VALUES (?, ?) ON CONFLICT (normalized_name) DO NOTHING"
    );
    execute_query!(db, &insert_sql, name.trim(), normalized_name.as_str())?;
    let select_sql = format!("SELECT id FROM {table} WHERE normalized_name = ?");
    Ok(query_scalar_one!(
        db,
        i64,
        &select_sql,
        normalized_name.as_str()
    )?)
}

async fn update_avionics_model_metadata(
    db: &AppDb,
    avionics_model_id: i64,
    introduced_year: i64,
    estimated_unit_value_usd: f64,
    value_reference_year: i64,
    value_source: &str,
) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
        UPDATE avionics_models
        SET
          introduced_year = COALESCE(introduced_year, ?),
          estimated_unit_value_usd = COALESCE(estimated_unit_value_usd, ?),
          value_reference_year = COALESCE(value_reference_year, ?),
          value_source = COALESCE(value_source, ?),
          updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        introduced_year,
        estimated_unit_value_usd,
        value_reference_year,
        value_source,
        avionics_model_id
    )?;
    Ok(())
}

fn required_string(value: &Value, field: &str) -> StoreResult<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            AvionicsStoreError::Model(format!(
                "Gemini avionics response missing required string field {field}"
            ))
        })
}

fn required_i64(value: &Value, field: &str) -> StoreResult<i64> {
    value.get(field).and_then(Value::as_i64).ok_or_else(|| {
        AvionicsStoreError::Model(format!(
            "Gemini avionics response missing required integer field {field}"
        ))
    })
}

fn required_year(value: &Value, field: &str) -> StoreResult<i64> {
    let year = required_i64(value, field)?;
    if !(1900..=2100).contains(&year) {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response {field} out of range: {year}"
        )));
    }
    Ok(year)
}

fn required_min_f64(value: &Value, field: &str, minimum: f64) -> StoreResult<f64> {
    let number = value.get(field).and_then(Value::as_f64).ok_or_else(|| {
        AvionicsStoreError::Model(format!(
            "Gemini avionics response missing required number field {field}"
        ))
    })?;
    if number < minimum {
        return Err(AvionicsStoreError::Model(format!(
            "Gemini avionics response {field} below minimum {minimum}: {number}"
        )));
    }
    Ok(number)
}
