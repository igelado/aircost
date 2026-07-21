use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;

use crate::aircraft::enrich_aircraft_spec_for_listing_if_missing;
use crate::avionics::{
    enrich_listing_avionics_metadata, enrich_model_year_avionics_and_price_point_for_listing,
    normalize_avionics_models,
};
use crate::cleanup::{cleanup_orphan_records, CleanupError};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{
    optional_f64, optional_i64, optional_string, AvionicsUnitResolutionCandidate,
    AvionicsUnitResolutionContext, AvionicsUnitResolutionCorrectionContext, GeminiListingExtractor,
    ModelFamilyConfirmationContext, VariantConfirmationContext, VariantLabelCorrectionContext,
    VariantNormalizationCandidate, VariantNormalizationContext, VariantNormalizationExample,
};
use crate::models::{AircraftSummary, ListingPreview, ParsedAvionics, SaleListing};
use crate::normalize::{is_usable_avionics_label, normalize_avionics_model_name, normalize_name};

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

macro_rules! execute_query_count {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {{
        let sql = $db.sql($sql);
        match $db.backend() {
            DatabaseBackend::Sqlite(pool) => {
                sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|result| result.rows_affected())
            }
            DatabaseBackend::Postgres(pool) => {
                sqlx::query(&sql)$(.bind($bind))*.execute(pool).await.map(|result| result.rows_affected())
            }
        }
    }};
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

#[derive(Debug)]
pub enum ListingStoreError {
    Validation(String),
    NotFound(String),
    Permission(String),
    State(String),
    Database(String),
}

impl fmt::Display for ListingStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListingStoreError::Validation(message)
            | ListingStoreError::NotFound(message)
            | ListingStoreError::Permission(message)
            | ListingStoreError::State(message)
            | ListingStoreError::Database(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for ListingStoreError {}

impl From<sqlx::Error> for ListingStoreError {
    fn from(error: sqlx::Error) -> Self {
        ListingStoreError::Database(error.to_string())
    }
}

impl From<anyhow::Error> for ListingStoreError {
    fn from(error: anyhow::Error) -> Self {
        ListingStoreError::Database(error.to_string())
    }
}

impl From<CleanupError> for ListingStoreError {
    fn from(error: CleanupError) -> Self {
        ListingStoreError::Database(error.to_string())
    }
}

type StoreResult<T> = Result<T, ListingStoreError>;
pub type ListingProgressSender = tokio::sync::mpsc::UnboundedSender<Value>;
#[cfg(test)]
const MODEL_SIMILARITY_CONFIRMATION_THRESHOLD: f64 = 0.65;
const MODEL_FAMILY_CANDIDATE_THRESHOLD: f64 = 0.35;
const MODEL_FAMILY_CANDIDATE_LIMIT: usize = 5;
const VARIANT_SIMILARITY_CONFIRMATION_THRESHOLD: f64 = 0.35;
const KNOWN_VARIANT_CANDIDATE_LIMIT: usize = 5;
const AVIONICS_VALUE_REFERENCE_YEAR: i64 = 2026;

#[derive(Clone, Debug)]
struct ListingValues {
    manufacturer: String,
    model: String,
    variant: String,
    source_url: Option<String>,
    model_year: i64,
    asking_price_usd: f64,
    currency: String,
    status: String,
    registration_number: Option<String>,
    serial_number: Option<String>,
    airframe_hours: f64,
    engine_hours: f64,
    propeller_hours: f64,
    avionics: Vec<ListingAvionicsValue>,
}

#[derive(Clone, Debug)]
struct ListingAvionicsValue {
    manufacturer: String,
    model: String,
    avionics_type: String,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    introduced_year: Option<i64>,
    estimated_unit_value_usd: Option<f64>,
    value_reference_year: Option<i64>,
}

impl ListingAvionicsValue {
    fn from_parsed(item: ParsedAvionics) -> Self {
        Self {
            manufacturer: item.manufacturer,
            model: item.model,
            avionics_type: item.avionics_type,
            quantity: item.quantity,
            source: "listing".to_string(),
            source_notes: None,
            introduced_year: None,
            estimated_unit_value_usd: None,
            value_reference_year: None,
        }
    }
}

#[derive(Debug, FromRow)]
struct VerifiedAvionicsModelRow {
    manufacturer: String,
    model: String,
    avionics_type: String,
    introduced_year: i64,
    estimated_unit_value_usd: f64,
    value_reference_year: i64,
}

#[derive(Debug, FromRow)]
struct ListingRow {
    id: i64,
    aircraft_model_id: i64,
    aircraft_model_variant_id: i64,
    created_by_user_id: i64,
    is_verified: bool,
    source_url: Option<String>,
    model_year: i64,
    asking_price_usd: f64,
    currency: String,
    added_at: String,
    status: String,
    registration_number: Option<String>,
    serial_number: Option<String>,
    airframe_hours: f64,
    engine_hours: f64,
    propeller_hours: f64,
    created_at: String,
    updated_at: String,
    aircraft_manufacturer: String,
    aircraft_model: String,
    aircraft_variant: String,
}

#[derive(Debug, FromRow)]
struct ParsedAvionicsRow {
    manufacturer: String,
    model: String,
    avionics_type: String,
    quantity: i64,
}

#[derive(Debug, FromRow)]
struct ListingOwnerRow {
    created_by_user_id: i64,
    is_verified: bool,
}

#[derive(Debug, FromRow)]
struct ListingAircraftIdentityRow {
    aircraft_model_id: i64,
    aircraft_manufacturer: String,
    aircraft_model: String,
}

#[derive(Clone, Debug, FromRow)]
struct AircraftModelCandidateRow {
    name: String,
}

#[derive(Clone, Debug, FromRow)]
struct AircraftVariantCandidateRow {
    model_name: String,
    variant_name: String,
}

#[derive(Debug, FromRow)]
struct ModelVariantRow {
    aircraft_model_id: i64,
    aircraft_manufacturer: String,
    aircraft_model: String,
    variant_id: i64,
    variant_name: String,
    listing_count: i64,
}

#[derive(Debug, FromRow)]
struct AircraftModelGroupRow {
    aircraft_manufacturer: String,
    aircraft_model: String,
}

#[derive(Debug, FromRow)]
struct VariantExampleRow {
    model_year: i64,
    registration_number: Option<String>,
    source_url: Option<String>,
}

#[derive(Debug, FromRow)]
struct VariantPricePointRow {
    id: i64,
    model_year: i64,
}

#[derive(Debug, FromRow)]
struct VariantDefaultAvionicsRow {
    model_year: i64,
    avionics_model_id: i64,
    quantity: i64,
    source_url: String,
    source_title: String,
    source_notes: String,
    source_confidence: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct VariantNormalizationReport {
    pub manufacturer: String,
    pub model: String,
    pub applied: bool,
    pub groups: Vec<VariantNormalizationGroupReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VariantNormalizationGroupReport {
    pub canonical_variant: String,
    pub source_variants: Vec<String>,
    pub rationale: String,
    pub actions: Vec<VariantNormalizationActionReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VariantNormalizationActionReport {
    pub source_variant_id: i64,
    pub source_variant: String,
    pub target_variant_id: Option<i64>,
    pub target_variant: String,
    pub listing_count: i64,
    pub updated_listing_count: u64,
    pub updated_rental_count: u64,
    pub deleted_orphan_variant: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct AircraftModelHealingReport {
    pub applied: bool,
    pub processed_model_count: usize,
    pub models: Vec<VariantNormalizationReport>,
}

pub async fn create_listing(
    db: &AppDb,
    user_id: i64,
    preview: &ListingPreview,
    original_listing: Option<&Value>,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<SaleListing> {
    create_listing_with_progress(db, user_id, preview, original_listing, extractor, None).await
}

pub async fn create_listing_with_progress(
    db: &AppDb,
    user_id: i64,
    preview: &ListingPreview,
    original_listing: Option<&Value>,
    extractor: Option<&GeminiListingExtractor>,
    progress: Option<&ListingProgressSender>,
) -> StoreResult<SaleListing> {
    emit_listing_progress(
        progress,
        "verifying_listing",
        "Verifying extracted listing fields",
    );
    let mut values = values_from_preview(preview, original_listing)?;
    emit_listing_progress(
        progress,
        "normalizing_aircraft",
        "Normalizing aircraft model and variant",
    );
    canonicalize_aircraft_model_and_variant(db, &mut values, preview, extractor).await?;
    emit_listing_progress(
        progress,
        "normalizing_avionics",
        "Normalizing avionics units",
    );
    resolve_listing_avionics_values(
        db,
        &mut values,
        extractor,
        preview.source_url.as_deref(),
        preview.context_text.as_deref(),
    )
    .await?;

    if let Some(registration_number) = &values.registration_number {
        if let Some(listing_id) =
            unverified_listing_id_for_tail(db, user_id, registration_number).await?
        {
            emit_listing_progress(progress, "saving_listing", "Updating existing listing");
            update_listing_values(db, listing_id, &values, true).await?;
            emit_listing_progress(
                progress,
                "refreshing_estimates",
                "Refreshing valuation inputs",
            );
            complete_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref())
                .await?;
            return get_listing(db, user_id, listing_id).await;
        }
    }

    if let Some(listing_id) = matching_verified_listing_id(db, &values).await? {
        emit_listing_progress(progress, "saving_listing", "Refreshing matching listing");
        refresh_listing_timestamp(db, listing_id, values.source_url.as_deref()).await?;
        emit_listing_progress(
            progress,
            "refreshing_estimates",
            "Refreshing valuation inputs",
        );
        complete_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref())
            .await?;
        return get_listing(db, user_id, listing_id).await;
    }

    emit_listing_progress(progress, "saving_listing", "Saving listing");
    let listing_id = insert_listing(db, user_id, &values).await?;
    emit_listing_progress(
        progress,
        "refreshing_estimates",
        "Refreshing valuation inputs",
    );
    complete_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref()).await?;
    get_listing(db, user_id, listing_id).await
}

fn emit_listing_progress(progress: Option<&ListingProgressSender>, stage: &str, message: &str) {
    if let Some(progress) = progress {
        let _ = progress.send(json!({
            "stage": stage,
            "status": "running",
            "message": message,
        }));
    }
}

pub async fn heal_aircraft_models(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    apply: bool,
    limit: i64,
) -> StoreResult<AircraftModelHealingReport> {
    if limit < 1 {
        return Err(ListingStoreError::Validation(
            "limit must be at least 1".to_string(),
        ));
    }
    let model_groups = aircraft_model_groups(db, limit).await?;
    let mut reports = Vec::with_capacity(model_groups.len());
    for model_group in model_groups {
        reports.push(
            normalize_variants_for_model(
                db,
                extractor,
                &model_group.aircraft_manufacturer,
                &model_group.aircraft_model,
                apply,
            )
            .await?,
        );
    }
    Ok(AircraftModelHealingReport {
        applied: apply,
        processed_model_count: reports.len(),
        models: reports,
    })
}

pub async fn normalize_variants_for_model(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    manufacturer: &str,
    model: &str,
    apply: bool,
) -> StoreResult<VariantNormalizationReport> {
    let variants = model_variant_rows(db, manufacturer, model).await?;
    if variants.is_empty() {
        return Err(ListingStoreError::NotFound(format!(
            "no variants found for {manufacturer} {model}"
        )));
    }
    let aircraft_model_id = variants[0].aircraft_model_id;
    let aircraft_manufacturer = variants[0].aircraft_manufacturer.clone();
    let aircraft_model = variants[0].aircraft_model.clone();
    let mut candidates = Vec::with_capacity(variants.len());
    for variant in &variants {
        let examples = variant_examples(db, variant.variant_id).await?;
        candidates.push(VariantNormalizationCandidate {
            variant: variant.variant_name.clone(),
            listing_count: variant.listing_count,
            examples: examples
                .into_iter()
                .map(|example| VariantNormalizationExample {
                    model_year: example.model_year,
                    registration_number: example.registration_number,
                    source_url: example.source_url,
                })
                .collect(),
        });
    }

    let context = VariantNormalizationContext {
        manufacturer: aircraft_manufacturer.clone(),
        model: aircraft_model.clone(),
        variants: candidates,
    };
    let response = extractor.normalize_aircraft_variants(&context).await?;
    let groups = match variant_normalization_groups_from_response(&response, &variants) {
        Ok(groups) => groups,
        Err(error) => {
            let correction_context =
                variant_normalization_correction_context(&response, &variants, &error.to_string());
            let corrected_response = extractor
                .correct_aircraft_variant_normalization(&context, &response, &correction_context)
                .await?;
            variant_normalization_groups_from_response(&corrected_response, &variants)?
        }
    };
    let mut report_groups = Vec::with_capacity(groups.len());

    for group in groups {
        let target_variant_id = if apply {
            Some(
                ensure_aircraft_model_variant(
                    db,
                    &aircraft_manufacturer,
                    &aircraft_model,
                    &group.canonical_variant,
                )
                .await?,
            )
        } else {
            variant_id_for_model(db, aircraft_model_id, &group.canonical_variant).await?
        };

        if let Some(target_variant_id) = target_variant_id {
            if apply {
                update_variant_display_name(db, target_variant_id, &group.canonical_variant)
                    .await?;
            }
        }

        let mut actions = Vec::with_capacity(group.source_variants.len());
        for source_variant in &group.source_variants {
            let source_row = variants
                .iter()
                .find(|variant| variant.variant_name == *source_variant)
                .ok_or_else(|| {
                    ListingStoreError::State(format!(
                        "normalization source variant disappeared: {source_variant}"
                    ))
                })?;
            let (updated_listing_count, updated_rental_count, deleted_orphan_variant) = if apply {
                let target_variant_id = target_variant_id.ok_or_else(|| {
                    ListingStoreError::State(format!(
                        "missing target variant for {}",
                        group.canonical_variant
                    ))
                })?;
                let updated_listings = if source_row.variant_id == target_variant_id {
                    0
                } else {
                    update_listing_variant_references(db, source_row.variant_id, target_variant_id)
                        .await?
                };
                let updated_rentals = if source_row.variant_id == target_variant_id {
                    0
                } else {
                    update_rental_variant_references(db, source_row.variant_id, target_variant_id)
                        .await?
                };
                if source_row.variant_id != target_variant_id {
                    merge_spec_variant_references(db, source_row.variant_id, target_variant_id)
                        .await?;
                    merge_price_point_variant_references(
                        db,
                        source_row.variant_id,
                        target_variant_id,
                    )
                    .await?;
                    merge_default_avionics_variant_references(
                        db,
                        source_row.variant_id,
                        target_variant_id,
                    )
                    .await?;
                }
                let deleted_orphan = delete_orphan_variant(db, source_row.variant_id).await? > 0;
                (updated_listings, updated_rentals, deleted_orphan)
            } else {
                (0, 0, false)
            };

            actions.push(VariantNormalizationActionReport {
                source_variant_id: source_row.variant_id,
                source_variant: source_row.variant_name.clone(),
                target_variant_id,
                target_variant: group.canonical_variant.clone(),
                listing_count: source_row.listing_count,
                updated_listing_count,
                updated_rental_count,
                deleted_orphan_variant,
            });
        }

        report_groups.push(VariantNormalizationGroupReport {
            canonical_variant: group.canonical_variant,
            source_variants: group.source_variants,
            rationale: group.rationale,
            actions,
        });
    }

    if apply {
        cleanup_orphan_records(db).await?;
    }

    Ok(VariantNormalizationReport {
        manufacturer: aircraft_manufacturer,
        model: aircraft_model,
        applied: apply,
        groups: report_groups,
    })
}

pub async fn list_listings(db: &AppDb, user_id: i64) -> StoreResult<Vec<SaleListing>> {
    let rows = query_as_all!(
        db,
        ListingRow,
        r#"
        SELECT
          l.*,
          model.id AS aircraft_model_id,
          mfr.name AS aircraft_manufacturer,
          model.name AS aircraft_model,
          variant.name AS aircraft_variant
        FROM aircraft_sale_listings l
        JOIN aircraft_model_variants variant
          ON variant.id = l.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE l.is_verified = TRUE OR l.created_by_user_id = ?
        ORDER BY l.added_at DESC, l.id DESC
        "#,
        user_id
    )?;
    let mut listings = Vec::with_capacity(rows.len());
    for row in rows {
        listings.push(listing_from_row(db, row).await?);
    }
    Ok(listings)
}

pub async fn get_listing(db: &AppDb, user_id: i64, listing_id: i64) -> StoreResult<SaleListing> {
    let row = query_as_optional!(
        db,
        ListingRow,
        r#"
        SELECT
          l.*,
          model.id AS aircraft_model_id,
          mfr.name AS aircraft_manufacturer,
          model.name AS aircraft_model,
          variant.name AS aircraft_variant
        FROM aircraft_sale_listings l
        JOIN aircraft_model_variants variant
          ON variant.id = l.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE l.id = ? AND (l.is_verified = TRUE OR l.created_by_user_id = ?)
        "#,
        listing_id,
        user_id
    )?;
    match row {
        Some(row) => listing_from_row(db, row).await,
        None => Err(ListingStoreError::NotFound("listing not found".to_string())),
    }
}

pub async fn update_listing(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
    listing: &Value,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<SaleListing> {
    let row = listing_owner_row(db, listing_id).await?;
    assert_user_can_mutate(&row, user_id, "update")?;

    let current = get_listing(db, user_id, listing_id).await?;
    let old_model_id = current.aircraft.aircraft_model_id;
    let mut values = values_from_listing(&current);
    merge_update_fields(&mut values, listing)?;
    let source_url = values.source_url.clone();
    correct_nonconforming_variant_label_with_context(
        &mut values,
        extractor,
        source_url.as_deref(),
        None,
    )
    .await?;
    resolve_listing_avionics_values(db, &mut values, extractor, source_url.as_deref(), None)
        .await?;
    update_listing_values(db, listing_id, &values, false).await?;
    complete_listing_ingestion(db, listing_id, extractor, None).await?;
    let updated = get_listing(db, user_id, listing_id).await?;
    if updated.aircraft.aircraft_model_id != old_model_id {
        mark_valuation_snapshot_stale_best_effort(db, old_model_id).await;
    }
    cleanup_orphan_records(db).await?;
    Ok(updated)
}

pub async fn delete_listing(db: &AppDb, user_id: i64, listing_id: i64) -> StoreResult<()> {
    let row = listing_owner_row(db, listing_id).await?;
    assert_user_can_mutate(&row, user_id, "delete")?;
    let model_id = listing_aircraft_identity(db, listing_id)
        .await?
        .map(|identity| identity.aircraft_model_id);
    detach_submission_and_delete_listing(db, listing_id).await?;
    if let Some(model_id) = model_id {
        mark_valuation_snapshot_stale_best_effort(db, model_id).await;
    }
    cleanup_orphan_records(db).await?;
    Ok(())
}

async fn detach_submission_and_delete_listing(db: &AppDb, listing_id: i64) -> StoreResult<()> {
    let detach_submission = db.sql(
        "UPDATE plugin_submissions SET canonical_listing_id = NULL WHERE canonical_listing_id = ?",
    );
    let delete_listing = db.sql("DELETE FROM aircraft_sale_listings WHERE id = ?");

    macro_rules! execute_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&detach_submission)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query(&delete_listing)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Ok::<(), sqlx::Error>(())
        }};
    }

    match db.backend() {
        DatabaseBackend::Sqlite(pool) => execute_in_transaction!(pool)?,
        DatabaseBackend::Postgres(pool) => execute_in_transaction!(pool)?,
    }
    Ok(())
}

async fn insert_listing(db: &AppDb, user_id: i64, values: &ListingValues) -> StoreResult<i64> {
    let aircraft_model_variant_id =
        ensure_aircraft_model_variant(db, &values.manufacturer, &values.model, &values.variant)
            .await?;
    let listing_id = query_scalar_one!(
        db,
        i64,
        r#"
        INSERT INTO aircraft_sale_listings (
          aircraft_model_variant_id,
          created_by_user_id,
          is_verified,
          source_url,
          model_year,
          asking_price_usd,
          currency,
          added_at,
          status,
          registration_number,
          serial_number,
          airframe_hours,
          engine_hours,
          propeller_hours
        )
        VALUES (?, ?, FALSE, ?, ?, ?, ?, CURRENT_TIMESTAMP, ?, ?, ?, ?, ?, ?)
        RETURNING id
        "#,
        aircraft_model_variant_id,
        user_id,
        values.source_url.as_deref(),
        values.model_year,
        values.asking_price_usd,
        values.currency.as_str(),
        values.status.as_str(),
        values.registration_number.as_deref(),
        values.serial_number.as_deref(),
        values.airframe_hours,
        values.engine_hours,
        values.propeller_hours
    )?;

    replace_listing_avionics(db, listing_id, &values.avionics).await?;
    Ok(listing_id)
}

async fn update_listing_values(
    db: &AppDb,
    listing_id: i64,
    values: &ListingValues,
    update_added_at: bool,
) -> StoreResult<()> {
    let aircraft_model_variant_id =
        ensure_aircraft_model_variant(db, &values.manufacturer, &values.model, &values.variant)
            .await?;
    let added_at_assignment = if update_added_at {
        ", added_at = CURRENT_TIMESTAMP"
    } else {
        ""
    };
    let update_sql = format!(
        r#"
            UPDATE aircraft_sale_listings
            SET
              aircraft_model_variant_id = ?,
              source_url = ?,
              model_year = ?,
              asking_price_usd = ?,
              currency = ?,
              status = ?,
              registration_number = ?,
              serial_number = ?,
              airframe_hours = ?,
              engine_hours = ?,
              propeller_hours = ?,
              updated_at = CURRENT_TIMESTAMP
              {added_at_assignment}
            WHERE id = ?
            "#
    );
    execute_query!(
        db,
        &update_sql,
        aircraft_model_variant_id,
        values.source_url.as_deref(),
        values.model_year,
        values.asking_price_usd,
        values.currency.as_str(),
        values.status.as_str(),
        values.registration_number.as_deref(),
        values.serial_number.as_deref(),
        values.airframe_hours,
        values.engine_hours,
        values.propeller_hours,
        listing_id
    )?;
    replace_listing_avionics(db, listing_id, &values.avionics).await?;
    Ok(())
}

fn values_from_preview(
    preview: &ListingPreview,
    _original_listing: Option<&Value>,
) -> StoreResult<ListingValues> {
    let parsed = &preview.parsed_listing;
    let values = ListingValues {
        manufacturer: required_string(parsed.manufacturer.as_deref(), "manufacturer")?,
        model: required_string(parsed.model.as_deref(), "model")?,
        variant: required_string(parsed.variant.as_deref(), "variant")?,
        source_url: preview.source_url.clone(),
        model_year: required_i64(parsed.model_year, "model_year")?,
        asking_price_usd: required_f64(parsed.asking_price_usd, "asking_price_usd")?,
        currency: parsed.currency.clone(),
        status: parsed.status.clone(),
        registration_number: parsed.registration_number.clone(),
        serial_number: parsed.serial_number.clone(),
        airframe_hours: required_f64(parsed.airframe_hours, "airframe_hours")?,
        engine_hours: required_f64(parsed.engine_hours, "engine_hours")?,
        propeller_hours: required_f64(parsed.propeller_hours, "propeller_hours")?,
        avionics: parsed
            .avionics
            .clone()
            .into_iter()
            .map(ListingAvionicsValue::from_parsed)
            .collect(),
    };
    Ok(values)
}

async fn canonicalize_aircraft_model_and_variant(
    db: &AppDb,
    values: &mut ListingValues,
    preview: &ListingPreview,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<()> {
    correct_nonconforming_variant_label(values, preview, extractor).await?;
    canonicalize_model_family_from_known_candidates(db, values, preview, extractor).await?;
    normalize_variant_label_against_model_family(values, preview, extractor).await?;
    let _ = canonicalize_variant_from_known_candidates(db, values, preview, extractor).await?;
    Ok(())
}

async fn normalize_variant_label_against_model_family(
    values: &mut ListingValues,
    preview: &ListingPreview,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<()> {
    let listing_context = preview.context_text.as_deref().map(listing_context_excerpt);
    normalize_variant_label_with_context(
        values,
        extractor,
        preview.source_url.as_deref(),
        listing_context.as_deref(),
        vec!["normalize variant label to the canonical model/variant split before known-variant matching".to_string()],
    )
    .await
}

async fn correct_nonconforming_variant_label(
    values: &mut ListingValues,
    preview: &ListingPreview,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<()> {
    let listing_context = preview.context_text.as_deref().map(listing_context_excerpt);
    correct_nonconforming_variant_label_with_context(
        values,
        extractor,
        preview.source_url.as_deref(),
        listing_context.as_deref(),
    )
    .await
}

async fn correct_nonconforming_variant_label_with_context(
    values: &mut ListingValues,
    extractor: Option<&GeminiListingExtractor>,
    source_url: Option<&str>,
    listing_context: Option<&str>,
) -> StoreResult<()> {
    let issues = variant_label_issues(values);
    if issues.is_empty() {
        return Ok(());
    }
    normalize_variant_label_with_context(values, extractor, source_url, listing_context, issues)
        .await
}

async fn normalize_variant_label_with_context(
    values: &mut ListingValues,
    extractor: Option<&GeminiListingExtractor>,
    source_url: Option<&str>,
    listing_context: Option<&str>,
    issues: Vec<String>,
) -> StoreResult<()> {
    let Some(extractor) = extractor else {
        return Ok(());
    };
    let context = VariantLabelCorrectionContext {
        manufacturer: &values.manufacturer,
        model: &values.model,
        variant: &values.variant,
        model_year: values.model_year,
        source_url,
        listing_context,
        issues: &issues,
    };
    let response = extractor
        .correct_aircraft_variant_label(&context)
        .await
        .map_err(|error| {
            ListingStoreError::State(format!(
                "Gemini variant label correction failed for '{}': {error:#}",
                values.variant
            ))
        })?;
    let corrected_variant = required_string(
        response.get("corrected_variant").and_then(Value::as_str),
        "corrected_variant",
    )?;
    let corrected_values = ListingValues {
        variant: corrected_variant.clone(),
        ..values.clone()
    };
    let remaining_issues = variant_label_issues(&corrected_values);
    if !remaining_issues.is_empty() {
        return Err(ListingStoreError::Validation(format!(
            "Gemini variant label correction returned non-conforming variant '{corrected_variant}': {}",
            remaining_issues.join("; ")
        )));
    }
    values.variant = corrected_variant;
    Ok(())
}

fn variant_label_issues(values: &ListingValues) -> Vec<String> {
    let mut issues = Vec::new();
    let variant_norm = normalize_name(&values.variant);
    let manufacturer_norm = normalize_name(&values.manufacturer);
    if !manufacturer_norm.is_empty()
        && variant_norm
            .split_whitespace()
            .any(|token| token == manufacturer_norm)
    {
        issues.push("variant contains the aircraft manufacturer name".to_string());
    }
    let model_year = values.model_year.to_string();
    if variant_norm
        .split_whitespace()
        .any(|token| token == model_year)
    {
        issues.push("variant contains the aircraft model year".to_string());
    }
    issues
}

async fn resolve_listing_avionics_values(
    db: &AppDb,
    values: &mut ListingValues,
    extractor: Option<&GeminiListingExtractor>,
    source_url: Option<&str>,
    listing_context: Option<&str>,
) -> StoreResult<()> {
    let Some(extractor) = extractor else {
        values
            .avionics
            .retain(|item| is_usable_avionics_label(&item.manufacturer, &item.model));
        return Ok(());
    };
    let listing_context = listing_context
        .map(listing_context_excerpt)
        .unwrap_or_default();
    let mut resolved = Vec::new();
    let mut seen = HashSet::new();

    for item in values.avionics.clone() {
        if let Some(cached) = existing_verified_avionics_value(db, &item).await? {
            let key = (
                normalize_name(&cached.manufacturer),
                normalize_avionics_model_name(&cached.model),
                normalize_name(&cached.avionics_type),
            );
            if seen.insert(key) {
                resolved.push(cached);
            }
            continue;
        }
        let context = AvionicsUnitResolutionContext {
            aircraft_manufacturer: values.manufacturer.clone(),
            aircraft_model: values.model.clone(),
            aircraft_variant: values.variant.clone(),
            model_year: values.model_year,
            source_url: source_url.unwrap_or("").to_string(),
            listing_context: listing_context.clone(),
            candidate: AvionicsUnitResolutionCandidate {
                manufacturer: item.manufacturer.clone(),
                model: item.model.clone(),
                avionics_type: item.avionics_type.clone(),
                quantity: item.quantity.max(1),
            },
            value_reference_year: AVIONICS_VALUE_REFERENCE_YEAR,
        };
        let (resolution_result, secondary_check_result) = tokio::join!(
            extractor.resolve_avionics_unit(&context),
            extractor.classify_avionics_unit_concreteness(&context)
        );
        let mut response = resolution_result.map_err(|error| {
            ListingStoreError::State(format!(
                "Gemini avionics unit grounding failed for {} {}: {error:#}",
                item.manufacturer, item.model
            ))
        })?;
        let secondary_check = secondary_check_result.map_err(|error| {
            ListingStoreError::State(format!(
                "Gemini avionics concreteness check failed for {} {}: {error:#}",
                item.manufacturer, item.model
            ))
        })?;
        let mut issues = avionics_resolution_review_issues(&context, &response);
        issues.extend(avionics_secondary_check_review_issues(
            &context,
            &response,
            &secondary_check,
        ));
        if !issues.is_empty() {
            response = extractor
                .correct_avionics_unit_resolution(
                    &context,
                    &response,
                    &AvionicsUnitResolutionCorrectionContext {
                        issues,
                        secondary_check: Some(secondary_check),
                    },
                )
                .await
                .map_err(|error| {
                    ListingStoreError::State(format!(
                        "Gemini avionics unit correction failed for {} {}: {error:#}",
                        item.manufacturer, item.model
                    ))
                })?;
            let remaining_issues = avionics_resolution_review_issues(&context, &response);
            if !remaining_issues.is_empty() {
                continue;
            }
        }
        let Some(resolved_item) = listing_avionics_value_from_resolution(&item, &response)? else {
            continue;
        };
        let key = (
            normalize_name(&resolved_item.manufacturer),
            normalize_avionics_model_name(&resolved_item.model),
            normalize_name(&resolved_item.avionics_type),
        );
        if seen.insert(key) {
            resolved.push(resolved_item);
        }
    }

    values.avionics = resolved;
    Ok(())
}

fn avionics_resolution_review_issues(
    context: &AvionicsUnitResolutionContext,
    response: &Value,
) -> Vec<String> {
    let mut issues = Vec::new();
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if status == "reject" {
        return issues;
    }
    if status != "concrete" && status != "factory_default" {
        issues.push("status must be concrete, factory_default, or reject".to_string());
        return issues;
    }

    let manufacturer = response
        .get("manufacturer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let model = response
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let avionics_type = response
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let source_url = response
        .get("source_url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let source_title = response
        .get("source_title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let confidence = response
        .get("confidence")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let notes = response
        .get("notes")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();

    if !is_usable_avionics_label(manufacturer, model) {
        issues.push("manufacturer/model is empty or uses a generic manufacturer label".to_string());
    }
    if source_url.is_empty() || source_title.is_empty() {
        issues.push(
            "concrete or factory_default resolution must include a source_url and source_title"
                .to_string(),
        );
    }
    if confidence == "low" {
        issues.push("low confidence cannot be stored as verified avionics metadata".to_string());
    }
    if manufacturer.contains('(') || manufacturer.contains(')') {
        issues.push(
            "manufacturer contains a parenthetical alias; return the verified avionics maker only"
                .to_string(),
        );
    }

    let manufacturer_norm = normalize_name(manufacturer);
    let aircraft_manufacturer_norm = normalize_name(&context.aircraft_manufacturer);
    if !aircraft_manufacturer_norm.is_empty()
        && (manufacturer_norm == aircraft_manufacturer_norm
            || manufacturer_norm
                .split_whitespace()
                .any(|token| token == aircraft_manufacturer_norm))
    {
        issues.push(
            "manufacturer appears to be the aircraft maker or an aircraft-maker alias, not the avionics maker"
                .to_string(),
        );
    }

    let model_norm = normalize_avionics_model_name(model);
    let type_norm = normalize_name(avionics_type);
    if !type_norm.is_empty()
        && (model_norm == type_norm
            || (!model_has_specific_designator(model)
                && model_norm.ends_with(&format!(" {type_norm}"))))
    {
        issues.push(
            "model appears to be only an equipment class or capability instead of a concrete unit"
                .to_string(),
        );
    }
    if model_norm
        .split_whitespace()
        .any(|token| token == "series" || token == "family")
    {
        issues.push(
            "model appears to be a broad product series/family instead of one exact unit"
                .to_string(),
        );
    }
    if combines_multiple_model_numbers(model) {
        issues.push(
            "model appears to combine multiple possible units; return one exact unit or reject"
                .to_string(),
        );
    }
    let notes_norm = normalize_name(notes);
    if status == "concrete"
        && (notes_norm.contains(" generic ")
            || notes_norm.starts_with("generic ")
            || notes_norm.ends_with(" generic"))
    {
        issues.push("notes describe the candidate as generic while status is concrete".to_string());
    }

    issues
}

fn avionics_secondary_check_review_issues(
    context: &AvionicsUnitResolutionContext,
    response: &Value,
    secondary_check: &Value,
) -> Vec<String> {
    let mut issues = Vec::new();
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if status != "concrete" {
        return issues;
    }

    let classification = secondary_check
        .get("classification")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let confidence = secondary_check
        .get("confidence")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if confidence == "low" {
        return issues;
    }

    let response_manufacturer = response
        .get("manufacturer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let response_model = response
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let same_candidate_label = normalize_name(response_manufacturer)
        == normalize_name(&context.candidate.manufacturer)
        && normalize_avionics_model_name(response_model)
            == normalize_avionics_model_name(&context.candidate.model);

    if same_candidate_label && (classification == "generic" || classification == "ambiguous") {
        issues.push(format!(
            "secondary classifier marked the original candidate as {classification}; do not store the unchanged candidate as concrete unless a source proves it is one exact unit"
        ));
    }
    if same_candidate_label
        && secondary_check
            .get("manufacturer_is_avionics_maker")
            .and_then(Value::as_bool)
            == Some(false)
    {
        issues.push(
            "secondary classifier says the candidate manufacturer is not a verified avionics maker"
                .to_string(),
        );
    }
    if same_candidate_label
        && secondary_check
            .get("model_identifies_single_unit")
            .and_then(Value::as_bool)
            == Some(false)
    {
        issues.push(
            "secondary classifier says the candidate model does not identify one exact unit"
                .to_string(),
        );
    }

    issues
}

fn model_has_specific_designator(value: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '+')
        .any(|token| {
            token.chars().any(|character| character.is_ascii_digit()) || token.contains('+')
        })
}

fn combines_multiple_model_numbers(value: &str) -> bool {
    if !value.contains('/') {
        return false;
    }
    let mut numeric_groups = 0;
    let mut in_digits = false;
    for character in value.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                numeric_groups += 1;
                in_digits = true;
            }
        } else {
            in_digits = false;
        }
    }
    numeric_groups > 1
}

async fn existing_verified_avionics_value(
    db: &AppDb,
    item: &ListingAvionicsValue,
) -> StoreResult<Option<ListingAvionicsValue>> {
    if !is_usable_avionics_label(&item.manufacturer, &item.model) {
        return Ok(None);
    }
    let manufacturer = normalize_name(&item.manufacturer);
    let avionics_type = normalize_name(&item.avionics_type);
    let model = normalize_avionics_model_name(&item.model);
    let row = query_as_optional!(
        db,
        VerifiedAvionicsModelRow,
        r#"
        SELECT
          mfr.name AS manufacturer,
          model.name AS model,
          avionics_type.name AS avionics_type,
          model.introduced_year,
          model.estimated_unit_value_usd,
          model.value_reference_year
        FROM avionics_models model
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        JOIN avionics_types avionics_type
          ON avionics_type.id = model.avionics_type_id
        WHERE mfr.normalized_name = ?
          AND avionics_type.normalized_name = ?
          AND model.normalized_name = ?
          AND model.introduced_year IS NOT NULL
          AND model.estimated_unit_value_usd IS NOT NULL
          AND model.value_reference_year IS NOT NULL
        "#,
        manufacturer.as_str(),
        avionics_type.as_str(),
        model.as_str()
    )?;
    Ok(row.map(|row| ListingAvionicsValue {
        manufacturer: row.manufacturer,
        model: row.model,
        avionics_type: row.avionics_type,
        quantity: item.quantity.max(1),
        source: item.source.clone(),
        source_notes: Some("reused previously grounded avionics metadata".to_string()),
        introduced_year: Some(row.introduced_year),
        estimated_unit_value_usd: Some(row.estimated_unit_value_usd),
        value_reference_year: Some(row.value_reference_year),
    }))
}

fn listing_avionics_value_from_resolution(
    original: &ListingAvionicsValue,
    response: &Value,
) -> StoreResult<Option<ListingAvionicsValue>> {
    let status = required_string(response.get("status").and_then(Value::as_str), "status")?;
    if status == "reject" {
        return Ok(None);
    }
    if status != "concrete" && status != "factory_default" {
        return Err(ListingStoreError::Validation(format!(
            "Gemini avionics resolution returned invalid status: {status}"
        )));
    }

    let manufacturer = required_string(
        response.get("manufacturer").and_then(Value::as_str),
        "manufacturer",
    )?;
    let model = required_string(response.get("model").and_then(Value::as_str), "model")?;
    if !is_usable_avionics_label(&manufacturer, &model) {
        return Ok(None);
    }
    let avionics_type = required_string(response.get("type").and_then(Value::as_str), "type")?;
    let introduced_year = required_i64(
        response.get("introduced_year").and_then(Value::as_i64),
        "introduced_year",
    )?;
    if introduced_year < 1900 || introduced_year > 2100 {
        return Err(ListingStoreError::Validation(format!(
            "Gemini avionics resolution introduced_year out of range: {introduced_year}"
        )));
    }
    let estimated_unit_value_usd = required_f64(
        response
            .get("estimated_unit_value_usd")
            .and_then(Value::as_f64),
        "estimated_unit_value_usd",
    )?;
    if estimated_unit_value_usd < 0.0 {
        return Err(ListingStoreError::Validation(format!(
            "Gemini avionics resolution estimated_unit_value_usd must be non-negative: {estimated_unit_value_usd}"
        )));
    }
    let notes = required_string(response.get("notes").and_then(Value::as_str), "notes")?;
    let source = if status == "factory_default" {
        "factory_default"
    } else {
        "listing"
    };
    let source_notes = if status == "factory_default" {
        Some(format!(
            "Factory default replacement for rejected listing avionics '{} {}': {}",
            original.manufacturer, original.model, notes
        ))
    } else {
        Some(notes)
    };

    Ok(Some(ListingAvionicsValue {
        manufacturer,
        model,
        avionics_type,
        quantity: optional_i64(response.get("quantity"))
            .unwrap_or(original.quantity)
            .max(1),
        source: source.to_string(),
        source_notes,
        introduced_year: Some(introduced_year),
        estimated_unit_value_usd: Some(estimated_unit_value_usd),
        value_reference_year: Some(AVIONICS_VALUE_REFERENCE_YEAR),
    }))
}

async fn canonicalize_variant_from_known_candidates(
    db: &AppDb,
    values: &mut ListingValues,
    preview: &ListingPreview,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<bool> {
    let candidates =
        known_variant_candidates(db, &values.manufacturer, &values.model, &values.variant).await?;
    if candidates.is_empty() {
        return Ok(false);
    }

    for candidate in &candidates {
        if normalize_name(&candidate.variant_name) == normalize_name(&values.variant) {
            values.model = candidate.model_name.clone();
            values.variant = candidate.variant_name.clone();
            return Ok(true);
        }
    }

    let Some(extractor) = extractor else {
        return Ok(false);
    };
    let listing_context = preview.context_text.as_deref().map(listing_context_excerpt);
    for candidate in candidates {
        let context = VariantConfirmationContext {
            manufacturer: &values.manufacturer,
            extracted_model: &values.model,
            extracted_variant: &values.variant,
            candidate_model: &candidate.model_name,
            candidate_variant: &candidate.variant_name,
            source_url: preview.source_url.as_deref(),
            model_year: preview.parsed_listing.model_year,
            listing_context: listing_context.as_deref(),
        };
        if extractor
            .confirm_same_aircraft_variant(&context)
            .await
            .unwrap_or(false)
        {
            values.model = candidate.model_name;
            values.variant = candidate.variant_name;
            return Ok(true);
        }
    }
    Ok(false)
}

async fn canonicalize_model_family_from_known_candidates(
    db: &AppDb,
    values: &mut ListingValues,
    preview: &ListingPreview,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<()> {
    let candidates = known_model_candidates(db, &values.manufacturer, &values.model).await?;
    if candidates.is_empty() {
        return Ok(());
    }

    let Some(extractor) = extractor else {
        for candidate in candidates {
            if normalize_name(&candidate.name) == normalize_name(&values.model) {
                values.model = candidate.name;
                return Ok(());
            }
        }
        return Ok(());
    };

    for candidate in candidates {
        if normalize_name(&candidate.name) == normalize_name(&values.model) {
            values.model = candidate.name;
            return Ok(());
        }

        let listing_context = preview.context_text.as_deref().map(listing_context_excerpt);
        let context = ModelFamilyConfirmationContext {
            manufacturer: &values.manufacturer,
            extracted_model: &values.model,
            extracted_variant: &values.variant,
            candidate_model: &candidate.name,
            source_url: preview.source_url.as_deref(),
            model_year: preview.parsed_listing.model_year,
            listing_context: listing_context.as_deref(),
        };
        if extractor
            .confirm_same_aircraft_model_family(&context)
            .await
            .unwrap_or(false)
        {
            values.model = candidate.name;
            return Ok(());
        }
    }
    Ok(())
}

#[derive(Debug)]
struct VariantNormalizationGroup {
    canonical_variant: String,
    source_variants: Vec<String>,
    rationale: String,
}

fn variant_normalization_correction_context(
    response: &Value,
    variants: &[ModelVariantRow],
    validation_error: &str,
) -> Value {
    let input_variants = variants
        .iter()
        .map(|variant| variant.variant_name.clone())
        .collect::<Vec<_>>();
    let expected = input_variants.iter().cloned().collect::<HashSet<_>>();
    let mut seen_counts = HashMap::<String, usize>::new();
    let mut invalid_source_variants = Vec::new();
    let mut group_count = 0_usize;

    if let Some(groups) = response.get("groups").and_then(Value::as_array) {
        group_count = groups.len();
        for group in groups {
            if let Some(source_variants) = group.get("source_variants").and_then(Value::as_array) {
                for source_variant in source_variants {
                    if let Some(source_variant) = source_variant.as_str() {
                        *seen_counts.entry(source_variant.to_string()).or_default() += 1;
                    } else {
                        invalid_source_variants.push(source_variant.clone());
                    }
                }
            }
        }
    }

    let missing_source_variants = input_variants
        .iter()
        .filter(|variant| !seen_counts.contains_key(*variant))
        .cloned()
        .collect::<Vec<_>>();
    let duplicated_source_variants = seen_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(variant, count)| json!({"variant": variant, "count": count}))
        .collect::<Vec<_>>();
    let unknown_source_variants = seen_counts
        .keys()
        .filter(|variant| !expected.contains(*variant))
        .cloned()
        .collect::<Vec<_>>();

    json!({
        "validation_error": validation_error,
        "group_count": group_count,
        "input_source_variants": input_variants,
        "missing_source_variants": missing_source_variants,
        "duplicated_source_variants": duplicated_source_variants,
        "unknown_source_variants": unknown_source_variants,
        "invalid_source_variants": invalid_source_variants
    })
}

async fn model_variant_rows(
    db: &AppDb,
    manufacturer: &str,
    model: &str,
) -> StoreResult<Vec<ModelVariantRow>> {
    let manufacturer_key = normalize_name(manufacturer);
    let model_key = normalize_name(model);
    Ok(query_as_all!(
        db,
        ModelVariantRow,
        r#"
        SELECT
          model.id AS aircraft_model_id,
          mfr.name AS aircraft_manufacturer,
          model.name AS aircraft_model,
          variant.id AS variant_id,
          variant.name AS variant_name,
          COUNT(l.id) AS listing_count
        FROM aircraft_model_variants variant
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        LEFT JOIN aircraft_sale_listings l
          ON l.aircraft_model_variant_id = variant.id
        WHERE mfr.normalized_name = ? AND model.normalized_name = ?
        GROUP BY
          model.id,
          mfr.name,
          model.name,
          variant.id,
          variant.name
        ORDER BY variant.name
        "#,
        manufacturer_key.as_str(),
        model_key.as_str()
    )?)
}

async fn aircraft_model_groups(db: &AppDb, limit: i64) -> StoreResult<Vec<AircraftModelGroupRow>> {
    Ok(query_as_all!(
        db,
        AircraftModelGroupRow,
        r#"
        SELECT
          mfr.name AS aircraft_manufacturer,
          model.name AS aircraft_model,
          COUNT(l.id) AS listing_count
        FROM aircraft_models model
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        JOIN aircraft_model_variants variant
          ON variant.aircraft_model_id = model.id
        JOIN aircraft_sale_listings l
          ON l.aircraft_model_variant_id = variant.id
        GROUP BY
          mfr.name,
          model.name
        ORDER BY listing_count DESC, mfr.name, model.name
        LIMIT ?
        "#,
        limit
    )?)
}

async fn variant_examples(db: &AppDb, variant_id: i64) -> StoreResult<Vec<VariantExampleRow>> {
    Ok(query_as_all!(
        db,
        VariantExampleRow,
        r#"
        SELECT model_year, registration_number, source_url
        FROM aircraft_sale_listings
        WHERE aircraft_model_variant_id = ?
        ORDER BY model_year DESC, id DESC
        LIMIT 5
        "#,
        variant_id
    )?)
}

fn variant_normalization_groups_from_response(
    response: &Value,
    variants: &[ModelVariantRow],
) -> StoreResult<Vec<VariantNormalizationGroup>> {
    let Some(groups) = response.get("groups").and_then(Value::as_array) else {
        return Err(ListingStoreError::Validation(
            "Gemini variant normalization response missing groups".to_string(),
        ));
    };
    let expected_counts = variants
        .iter()
        .map(|variant| (variant.variant_name.clone(), 1_usize))
        .collect::<HashMap<_, _>>();
    let mut seen_counts: HashMap<String, usize> = HashMap::new();
    let mut parsed_groups = Vec::with_capacity(groups.len());

    for group in groups {
        let canonical_variant = required_string(
            group.get("canonical_variant").and_then(Value::as_str),
            "canonical_variant",
        )?;
        let rationale =
            optional_string(group.get("rationale")).unwrap_or_else(|| "No rationale".to_string());
        let Some(source_values) = group.get("source_variants").and_then(Value::as_array) else {
            return Err(ListingStoreError::Validation(
                "Gemini variant normalization group missing source_variants".to_string(),
            ));
        };
        if source_values.is_empty() {
            return Err(ListingStoreError::Validation(
                "Gemini variant normalization group has no source_variants".to_string(),
            ));
        }

        let mut source_variants = Vec::with_capacity(source_values.len());
        for source_value in source_values {
            let source_variant = required_string(source_value.as_str(), "source_variants item")?;
            if !expected_counts.contains_key(&source_variant) {
                return Err(ListingStoreError::Validation(format!(
                    "Gemini returned unknown source variant: {source_variant}"
                )));
            }
            *seen_counts.entry(source_variant.clone()).or_default() += 1;
            source_variants.push(source_variant);
        }

        parsed_groups.push(VariantNormalizationGroup {
            canonical_variant,
            source_variants,
            rationale,
        });
    }

    let missing = expected_counts
        .keys()
        .filter(|variant| !seen_counts.contains_key(*variant))
        .cloned()
        .collect::<Vec<_>>();
    let duplicated = seen_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(variant, _)| (*variant).to_string())
        .collect::<Vec<_>>();
    if !missing.is_empty() || !duplicated.is_empty() {
        return Err(ListingStoreError::Validation(format!(
            "Gemini variant normalization did not cover source variants exactly once; missing={missing:?}, duplicated={duplicated:?}"
        )));
    }

    Ok(parsed_groups)
}

async fn variant_id_for_model(
    db: &AppDb,
    aircraft_model_id: i64,
    variant: &str,
) -> StoreResult<Option<i64>> {
    let normalized_variant = normalize_name(variant);
    Ok(query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT id
        FROM aircraft_model_variants
        WHERE aircraft_model_id = ? AND normalized_name = ?
        "#,
        aircraft_model_id,
        normalized_variant.as_str()
    )?)
}

async fn update_variant_display_name(
    db: &AppDb,
    variant_id: i64,
    variant: &str,
) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
        UPDATE aircraft_model_variants
        SET name = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        variant,
        variant_id
    )?;
    Ok(())
}

async fn update_listing_variant_references(
    db: &AppDb,
    source_variant_id: i64,
    target_variant_id: i64,
) -> StoreResult<u64> {
    Ok(execute_query_count!(
        db,
        r#"
        UPDATE aircraft_sale_listings
        SET aircraft_model_variant_id = ?, updated_at = CURRENT_TIMESTAMP
        WHERE aircraft_model_variant_id = ?
        "#,
        target_variant_id,
        source_variant_id
    )?)
}

async fn update_rental_variant_references(
    db: &AppDb,
    source_variant_id: i64,
    target_variant_id: i64,
) -> StoreResult<u64> {
    Ok(execute_query_count!(
        db,
        r#"
        UPDATE rental_aircraft_offerings
        SET aircraft_model_variant_id = ?, updated_at = CURRENT_TIMESTAMP
        WHERE aircraft_model_variant_id = ?
        "#,
        target_variant_id,
        source_variant_id
    )?)
}

async fn merge_spec_variant_references(
    db: &AppDb,
    source_variant_id: i64,
    target_variant_id: i64,
) -> StoreResult<()> {
    let target_spec_count = query_scalar_one!(
        db,
        i64,
        "SELECT COUNT(*) FROM aircraft_model_spec_versions WHERE aircraft_model_variant_id = ?",
        target_variant_id
    )?;
    if target_spec_count > 0 {
        execute_query!(
            db,
            "DELETE FROM aircraft_model_spec_versions WHERE aircraft_model_variant_id = ?",
            source_variant_id
        )?;
    } else {
        execute_query!(
            db,
            r#"
            UPDATE aircraft_model_spec_versions
            SET aircraft_model_variant_id = ?, updated_at = CURRENT_TIMESTAMP
            WHERE aircraft_model_variant_id = ?
            "#,
            target_variant_id,
            source_variant_id
        )?;
    }
    Ok(())
}

async fn merge_price_point_variant_references(
    db: &AppDb,
    source_variant_id: i64,
    target_variant_id: i64,
) -> StoreResult<()> {
    let source_rows = query_as_all!(
        db,
        VariantPricePointRow,
        r#"
        SELECT id, model_year
        FROM aircraft_model_variant_price_points
        WHERE aircraft_model_variant_id = ?
        "#,
        source_variant_id
    )?;
    for row in source_rows {
        let target_exists = query_scalar_optional!(
            db,
            i64,
            r#"
            SELECT id
            FROM aircraft_model_variant_price_points
            WHERE aircraft_model_variant_id = ?
              AND model_year = ?
            LIMIT 1
            "#,
            target_variant_id,
            row.model_year
        )?
        .is_some();
        if target_exists {
            execute_query!(
                db,
                "DELETE FROM aircraft_model_variant_price_points WHERE id = ?",
                row.id
            )?;
        } else {
            execute_query!(
                db,
                r#"
                UPDATE aircraft_model_variant_price_points
                SET aircraft_model_variant_id = ?, updated_at = CURRENT_TIMESTAMP
                WHERE id = ?
                "#,
                target_variant_id,
                row.id
            )?;
        }
    }
    Ok(())
}

async fn merge_default_avionics_variant_references(
    db: &AppDb,
    source_variant_id: i64,
    target_variant_id: i64,
) -> StoreResult<()> {
    let source_rows = query_as_all!(
        db,
        VariantDefaultAvionicsRow,
        r#"
        SELECT
          model_year,
          avionics_model_id,
          quantity,
          source_url,
          source_title,
          source_notes,
          source_confidence
        FROM aircraft_model_variant_default_avionics
        WHERE aircraft_model_variant_id = ?
        "#,
        source_variant_id
    )?;
    for row in source_rows {
        let target_quantity = query_scalar_optional!(
            db,
            i64,
            r#"
            SELECT quantity
            FROM aircraft_model_variant_default_avionics
            WHERE aircraft_model_variant_id = ?
              AND model_year = ?
              AND avionics_model_id = ?
            "#,
            target_variant_id,
            row.model_year,
            row.avionics_model_id
        )?;
        match target_quantity {
            Some(target_quantity) => {
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
                    target_quantity.max(row.quantity).max(1),
                    row.source_url.as_str(),
                    row.source_title.as_str(),
                    row.source_notes.as_str(),
                    row.source_confidence.as_str(),
                    target_variant_id,
                    row.model_year,
                    row.avionics_model_id
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
                    target_variant_id,
                    row.model_year,
                    row.avionics_model_id,
                    row.quantity.max(1),
                    row.source_url.as_str(),
                    row.source_title.as_str(),
                    row.source_notes.as_str(),
                    row.source_confidence.as_str()
                )?;
            }
        }
    }
    execute_query!(
        db,
        "DELETE FROM aircraft_model_variant_default_avionics WHERE aircraft_model_variant_id = ?",
        source_variant_id
    )?;
    Ok(())
}

async fn delete_orphan_variant(db: &AppDb, variant_id: i64) -> StoreResult<u64> {
    Ok(execute_query_count!(
        db,
        r#"
        DELETE FROM aircraft_model_variants
        WHERE id = ?
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listings
            WHERE aircraft_model_variant_id = aircraft_model_variants.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM rental_aircraft_offerings
            WHERE aircraft_model_variant_id = aircraft_model_variants.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_model_spec_versions
            WHERE aircraft_model_variant_id = aircraft_model_variants.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_price_points
            WHERE aircraft_model_variant_id = aircraft_model_variants.id
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_model_variant_default_avionics
            WHERE aircraft_model_variant_id = aircraft_model_variants.id
          )
        "#,
        variant_id
    )?)
}

async fn known_variant_candidates(
    db: &AppDb,
    manufacturer: &str,
    model: &str,
    extracted_variant: &str,
) -> StoreResult<Vec<AircraftVariantCandidateRow>> {
    let manufacturer_key = normalize_name(manufacturer);
    let model_key = normalize_avionics_model_name(model);
    let rows = query_as_all!(
        db,
        AircraftVariantCandidateRow,
        r#"
        SELECT
          model.name AS model_name,
          variant.name AS variant_name
        FROM aircraft_model_variants variant
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE mfr.normalized_name = ?
          AND model.normalized_name = ?
        "#,
        manufacturer_key.as_str(),
        model_key.as_str()
    )?;

    let mut scored = rows
        .into_iter()
        .filter_map(|row| {
            let score = model_similarity(extracted_variant, &row.variant_name);
            (score >= VARIANT_SIMILARITY_CONFIRMATION_THRESHOLD).then_some((score, row))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left.variant_name.cmp(&right.variant_name))
    });
    scored.truncate(KNOWN_VARIANT_CANDIDATE_LIMIT);
    Ok(scored.into_iter().map(|(_, row)| row).collect())
}

async fn known_model_candidates(
    db: &AppDb,
    manufacturer: &str,
    extracted_model: &str,
) -> StoreResult<Vec<AircraftModelCandidateRow>> {
    let manufacturer_key = normalize_name(manufacturer);
    let rows = query_as_all!(
        db,
        AircraftModelCandidateRow,
        r#"
        SELECT model.name AS name
        FROM aircraft_models model
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE mfr.normalized_name = ?
        "#,
        manufacturer_key.as_str()
    )?;

    let mut scored = rows
        .into_iter()
        .filter_map(|row| {
            let score = model_similarity(extracted_model, &row.name);
            (score >= MODEL_FAMILY_CANDIDATE_THRESHOLD).then_some((score, row))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left.name.cmp(&right.name))
    });
    scored.truncate(MODEL_FAMILY_CANDIDATE_LIMIT);
    Ok(scored.into_iter().map(|(_, row)| row).collect())
}

fn model_similarity(left: &str, right: &str) -> f64 {
    let left = normalize_name(left);
    let right = normalize_name(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    if left == right {
        return 1.0;
    }
    token_dice_score(&model_tokens(&left), &model_tokens(&right))
        .max(bigram_dice_score(&compact(&left), &compact(&right)))
}

fn token_dice_score(left_tokens: &HashSet<String>, right_tokens: &HashSet<String>) -> f64 {
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let intersection = left_tokens.intersection(&right_tokens).count();
    (2.0 * intersection as f64) / (left_tokens.len() + right_tokens.len()) as f64
}

fn model_tokens(value: &str) -> HashSet<String> {
    value
        .split_whitespace()
        .flat_map(split_alpha_numeric)
        .filter(|token| !token.is_empty())
        .collect()
}

fn split_alpha_numeric(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_kind = None;
    for character in value.chars() {
        let kind = if character.is_ascii_digit() {
            Some(true)
        } else if character.is_ascii_alphabetic() {
            Some(false)
        } else {
            None
        };
        if kind.is_none() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            current_kind = None;
            continue;
        }
        if current_kind.is_some() && current_kind != kind {
            tokens.push(std::mem::take(&mut current));
        }
        current.push(character);
        current_kind = kind;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn bigram_dice_score(left: &str, right: &str) -> f64 {
    let left_bigrams = bigrams(left);
    let right_bigrams = bigrams(right);
    if left_bigrams.is_empty() || right_bigrams.is_empty() {
        return 0.0;
    }
    let intersection = left_bigrams
        .iter()
        .filter(|bigram| right_bigrams.contains(*bigram))
        .count();
    (2.0 * intersection as f64) / (left_bigrams.len() + right_bigrams.len()) as f64
}

fn bigrams(value: &str) -> HashSet<String> {
    let characters = value.chars().collect::<Vec<_>>();
    if characters.len() < 2 {
        return HashSet::new();
    }
    characters
        .windows(2)
        .map(|window| window.iter().collect::<String>())
        .collect()
}

fn compact(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn listing_context_excerpt(value: &str) -> String {
    value
        .split_whitespace()
        .take(900)
        .collect::<Vec<_>>()
        .join(" ")
}

fn values_from_listing(listing: &SaleListing) -> ListingValues {
    ListingValues {
        manufacturer: listing.aircraft.manufacturer.clone(),
        model: listing.aircraft.model.clone(),
        variant: listing.aircraft.variant.clone(),
        source_url: listing.source_url.clone(),
        model_year: listing.model_year,
        asking_price_usd: listing.asking_price_usd,
        currency: listing.currency.clone(),
        status: listing.status.clone(),
        registration_number: listing.registration_number.clone(),
        serial_number: listing.serial_number.clone(),
        airframe_hours: listing.airframe_hours,
        engine_hours: listing.engine_hours,
        propeller_hours: listing.propeller_hours,
        avionics: listing
            .avionics
            .clone()
            .into_iter()
            .map(ListingAvionicsValue::from_parsed)
            .collect(),
    }
}

fn merge_update_fields(values: &mut ListingValues, listing: &Value) -> StoreResult<()> {
    let Some(object) = listing.as_object() else {
        return Err(ListingStoreError::Validation(
            "listing must be a JSON object".to_string(),
        ));
    };
    for (key, value) in object {
        match key.as_str() {
            "manufacturer" => values.manufacturer = required_string_from_value(value, key)?,
            "model" => values.model = required_string_from_value(value, key)?,
            "variant" => values.variant = required_string_from_value(value, key)?,
            "model_year" => values.model_year = required_i64(optional_i64(Some(value)), key)?,
            "asking_price_usd" => {
                values.asking_price_usd = required_f64(optional_f64(Some(value)), key)?
            }
            "currency" => {
                values.currency = optional_string(Some(value)).unwrap_or_else(|| "USD".to_string())
            }
            "airframe_hours" => {
                values.airframe_hours = required_f64(optional_f64(Some(value)), key)?
            }
            "engine_hours" => values.engine_hours = required_f64(optional_f64(Some(value)), key)?,
            "propeller_hours" => {
                values.propeller_hours = required_f64(optional_f64(Some(value)), key)?
            }
            "registration_number" => values.registration_number = optional_string(Some(value)),
            "serial_number" => values.serial_number = optional_string(Some(value)),
            "status" => {
                values.status = optional_string(Some(value)).unwrap_or_else(|| "active".to_string())
            }
            "source_url" => values.source_url = optional_string(Some(value)),
            "avionics" => values.avionics = avionics_from_value(value),
            _ => {
                return Err(ListingStoreError::Validation(format!(
                    "unsupported listing field: {key}"
                )))
            }
        }
    }
    Ok(())
}

fn avionics_from_value(value: &Value) -> Vec<ListingAvionicsValue> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let manufacturer = optional_string(object.get("manufacturer"))?;
            let model = optional_string(object.get("model"))?;
            let avionics_type =
                optional_string(object.get("type")).unwrap_or_else(|| "Unknown".to_string());
            Some(ListingAvionicsValue::from_parsed(ParsedAvionics {
                manufacturer,
                model,
                avionics_type,
                quantity: optional_i64(object.get("quantity")).unwrap_or(1),
            }))
        })
        .collect()
}

async fn unverified_listing_id_for_tail(
    db: &AppDb,
    user_id: i64,
    registration_number: &str,
) -> StoreResult<Option<i64>> {
    Ok(query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT id
        FROM aircraft_sale_listings
        WHERE created_by_user_id = ?
          AND is_verified = FALSE
          AND UPPER(registration_number) = UPPER(?)
        ORDER BY added_at DESC, id DESC
        LIMIT 1
        "#,
        user_id,
        registration_number
    )?)
}

async fn matching_verified_listing_id(
    db: &AppDb,
    values: &ListingValues,
) -> StoreResult<Option<i64>> {
    let Some(registration_number) = &values.registration_number else {
        return Ok(None);
    };
    let rows = query_as_all!(
        db,
        ListingRow,
        r#"
        SELECT
          l.*,
          model.id AS aircraft_model_id,
          mfr.name AS aircraft_manufacturer,
          model.name AS aircraft_model,
          variant.name AS aircraft_variant
        FROM aircraft_sale_listings l
        JOIN aircraft_model_variants variant
          ON variant.id = l.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE UPPER(l.registration_number) = UPPER(?)
          AND l.is_verified = TRUE
        ORDER BY l.added_at DESC, l.id DESC
        "#,
        registration_number
    )?;
    for row in rows {
        let listing = listing_from_row(db, row).await?;
        if listing_matches_values(&listing, values) {
            return Ok(Some(listing.id));
        }
    }
    Ok(None)
}

async fn refresh_listing_timestamp(
    db: &AppDb,
    listing_id: i64,
    source_url: Option<&str>,
) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
            UPDATE aircraft_sale_listings
            SET
              added_at = CURRENT_TIMESTAMP,
              source_url = COALESCE(source_url, ?),
              updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
        source_url,
        listing_id
    )?;
    Ok(())
}

fn listing_matches_values(listing: &SaleListing, values: &ListingValues) -> bool {
    for (left, right) in [
        (&listing.aircraft.manufacturer, &values.manufacturer),
        (&listing.aircraft.model, &values.model),
        (&listing.aircraft.variant, &values.variant),
    ] {
        if normalize_name(left) != normalize_name(right) {
            return false;
        }
    }

    values_match_i64(listing.model_year, values.model_year)
        && values_match_f64(listing.asking_price_usd, values.asking_price_usd)
        && values_match_text(Some(&listing.currency), Some(&values.currency))
        && values_match_f64(listing.airframe_hours, values.airframe_hours)
        && values_match_f64(listing.engine_hours, values.engine_hours)
        && values_match_f64(listing.propeller_hours, values.propeller_hours)
        && values_match_text(Some(&listing.status), Some(&values.status))
        && values_match_text(
            listing.registration_number.as_deref(),
            values.registration_number.as_deref(),
        )
        && values_match_text(
            listing.serial_number.as_deref(),
            values.serial_number.as_deref(),
        )
        && canonical_parsed_avionics(&listing.avionics) == canonical_avionics(&values.avionics)
}

fn canonical_parsed_avionics(value: &[ParsedAvionics]) -> Vec<(String, String, String, i64)> {
    let mut canonical = value
        .iter()
        .map(|item| {
            (
                normalize_name(&item.manufacturer),
                normalize_avionics_model_name(&item.model),
                normalize_name(&item.avionics_type),
                item.quantity.max(1),
            )
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    canonical.sort();
    canonical
}

fn canonical_avionics(value: &[ListingAvionicsValue]) -> Vec<(String, String, String, i64)> {
    let mut canonical = value
        .iter()
        .map(|item| {
            (
                normalize_name(&item.manufacturer),
                normalize_avionics_model_name(&item.model),
                normalize_name(&item.avionics_type),
                item.quantity.max(1),
            )
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    canonical.sort();
    canonical
}

async fn complete_listing_ingestion(
    db: &AppDb,
    listing_id: i64,
    extractor: Option<&GeminiListingExtractor>,
    listing_text: Option<&str>,
) -> StoreResult<()> {
    if let Some(extractor) = extractor {
        let _ = heal_listing_aircraft_variants_if_needed(db, listing_id, extractor).await;
    }
    let _ = normalize_avionics_models(db, true).await;
    let _ =
        enrich_aircraft_spec_for_listing_if_missing(db, extractor, listing_id, listing_text).await;

    if listing_missing_avionics_metadata_count(db, listing_id).await? > 0 {
        let extractor = extractor.ok_or_else(|| {
            ListingStoreError::State(
                "Gemini extractor is not configured; cannot ground missing avionics metadata"
                    .to_string(),
            )
        })?;
        enrich_listing_avionics_metadata(db, extractor, true, listing_id, None, false)
            .await
            .map_err(|error| {
                ListingStoreError::State(format!(
                    "Gemini avionics metadata grounding failed: {error}"
                ))
            })?;
        let remaining = listing_missing_avionics_metadata_count(db, listing_id).await?;
        if remaining > 0 {
            return Err(ListingStoreError::State(format!(
                "Gemini avionics metadata grounding left {remaining} avionics rows incomplete"
            )));
        }
    }

    if listing_needs_model_year_price_or_default_avionics(db, listing_id).await? {
        let extractor = extractor.ok_or_else(|| {
            ListingStoreError::State(
                "Gemini extractor is not configured; cannot ground missing model-year price/default avionics"
                    .to_string(),
            )
        })?;
        enrich_model_year_avionics_and_price_point_for_listing(
            db, extractor, true, listing_id, None, false,
        )
        .await
        .map_err(|error| {
            ListingStoreError::State(format!(
                "Gemini model-year avionics grounding failed: {error}"
            ))
        })?;
        if listing_needs_model_year_price_or_default_avionics(db, listing_id).await? {
            return Err(ListingStoreError::State(
                "Gemini model-year avionics grounding left price/default avionics incomplete"
                    .to_string(),
            ));
        }
    }

    if extractor.is_some() {
        let _ = normalize_avionics_models(db, true).await;
    }
    if let Ok(Some(identity)) = listing_aircraft_identity(db, listing_id).await {
        mark_valuation_snapshot_stale_best_effort(db, identity.aircraft_model_id).await;
    }
    let _ = cleanup_orphan_records(db).await;
    Ok(())
}

async fn listing_missing_avionics_metadata_count(db: &AppDb, listing_id: i64) -> StoreResult<i64> {
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT COUNT(*)
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model
          ON model.id = link.avionics_model_id
        WHERE link.aircraft_sale_listing_id = ?
          AND (
            model.introduced_year IS NULL
            OR model.estimated_unit_value_usd IS NULL
            OR model.value_reference_year IS NULL
          )
        "#,
        listing_id
    )?)
}

async fn listing_needs_model_year_price_or_default_avionics(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<bool> {
    let missing_count = query_scalar_one!(
        db,
        i64,
        r#"
        SELECT COUNT(*)
        FROM aircraft_sale_listings listing
        WHERE listing.id = ?
          AND (
            NOT EXISTS (
              SELECT 1
              FROM aircraft_model_variant_price_points price_point
              WHERE price_point.aircraft_model_variant_id = listing.aircraft_model_variant_id
                AND price_point.model_year = listing.model_year
            )
            OR NOT EXISTS (
              SELECT 1
              FROM aircraft_model_variant_default_avionics default_avionics
              WHERE default_avionics.aircraft_model_variant_id = listing.aircraft_model_variant_id
                AND default_avionics.model_year = listing.model_year
            )
          )
        "#,
        listing_id
    )?;
    Ok(missing_count > 0)
}

async fn heal_listing_aircraft_variants_if_needed(
    db: &AppDb,
    listing_id: i64,
    extractor: &GeminiListingExtractor,
) -> StoreResult<()> {
    let Some(identity) = listing_aircraft_identity(db, listing_id).await? else {
        return Ok(());
    };
    let variants = model_variant_rows(
        db,
        &identity.aircraft_manufacturer,
        &identity.aircraft_model,
    )
    .await?;
    if !model_variants_need_normalization(&variants) {
        return Ok(());
    }
    normalize_variants_for_model(
        db,
        extractor,
        &identity.aircraft_manufacturer,
        &identity.aircraft_model,
        true,
    )
    .await?;
    Ok(())
}

fn model_variants_need_normalization(variants: &[ModelVariantRow]) -> bool {
    let mut normalized_names = HashSet::new();
    variants.iter().any(|variant| {
        let normalized = normalize_name(&variant.variant_name);
        normalized.is_empty() || !normalized_names.insert(normalized)
    })
}

async fn mark_valuation_snapshot_stale_best_effort(db: &AppDb, aircraft_model_id: i64) {
    let sql = db.sql(
        r#"
        INSERT INTO valuation_refresh_state (id, listings_changed_at, reason)
        VALUES (1, CURRENT_TIMESTAMP, ?)
        ON CONFLICT (id) DO UPDATE SET
          listings_changed_at = CURRENT_TIMESTAMP,
          reason = excluded.reason
        "#,
    );
    let reason = format!("listing mutation affected aircraft model {aircraft_model_id}");
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let _ = sqlx::query(&sql).bind(&reason).execute(pool).await;
        }
        DatabaseBackend::Postgres(pool) => {
            let _ = sqlx::query(&sql).bind(&reason).execute(pool).await;
        }
    }
}

fn values_match_i64(left: i64, right: i64) -> bool {
    left == right
}

fn values_match_f64(left: f64, right: f64) -> bool {
    (left - right).abs() <= 0.01
}

fn values_match_text(left: Option<&str>, right: Option<&str>) -> bool {
    left.unwrap_or("").trim() == right.unwrap_or("").trim()
}

async fn listing_owner_row(db: &AppDb, listing_id: i64) -> StoreResult<ListingOwnerRow> {
    let row = query_as_optional!(
        db,
        ListingOwnerRow,
        r#"
        SELECT created_by_user_id, is_verified
        FROM aircraft_sale_listings
        WHERE id = ?
        "#,
        listing_id
    )?;
    row.ok_or_else(|| ListingStoreError::NotFound("listing not found".to_string()))
}

async fn listing_aircraft_identity(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<Option<ListingAircraftIdentityRow>> {
    Ok(query_as_optional!(
        db,
        ListingAircraftIdentityRow,
        r#"
        SELECT
          model.id AS aircraft_model_id,
          mfr.name AS aircraft_manufacturer,
          model.name AS aircraft_model
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers mfr
          ON mfr.id = model.aircraft_manufacturer_id
        WHERE listing.id = ?
        "#,
        listing_id
    )?)
}

fn assert_user_can_mutate(row: &ListingOwnerRow, user_id: i64, action: &str) -> StoreResult<()> {
    if row.created_by_user_id != user_id {
        return Err(ListingStoreError::Permission(format!(
            "cannot {action} a listing owned by another user"
        )));
    }
    if row.is_verified {
        return Err(ListingStoreError::State(format!(
            "cannot {action} an internally verified listing"
        )));
    }
    Ok(())
}

async fn ensure_aircraft_model(db: &AppDb, manufacturer: &str, model: &str) -> StoreResult<i64> {
    let manufacturer_id = ensure_named_row(db, "aircraft_manufacturers", manufacturer).await?;
    let normalized_model = normalize_avionics_model_name(model);
    execute_query!(
        db,
        r#"
        INSERT INTO aircraft_models (
          aircraft_manufacturer_id,
          name,
          normalized_name
        )
        VALUES (?, ?, ?)
        ON CONFLICT (aircraft_manufacturer_id, normalized_name) DO NOTHING
        "#,
        manufacturer_id,
        model,
        normalized_model.as_str()
    )?;
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT id
        FROM aircraft_models
        WHERE aircraft_manufacturer_id = ? AND normalized_name = ?
        "#,
        manufacturer_id,
        normalized_model.as_str()
    )?)
}

async fn ensure_aircraft_model_variant(
    db: &AppDb,
    manufacturer: &str,
    model: &str,
    variant: &str,
) -> StoreResult<i64> {
    let aircraft_model_id = ensure_aircraft_model(db, manufacturer, model).await?;
    let normalized_variant = normalize_name(variant);
    execute_query!(
        db,
        r#"
        INSERT INTO aircraft_model_variants (
          aircraft_model_id,
          name,
          normalized_name
        )
        VALUES (?, ?, ?)
        ON CONFLICT (aircraft_model_id, normalized_name) DO NOTHING
        "#,
        aircraft_model_id,
        variant,
        normalized_variant.as_str()
    )?;
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT id
        FROM aircraft_model_variants
        WHERE aircraft_model_id = ? AND normalized_name = ?
        "#,
        aircraft_model_id,
        normalized_variant.as_str()
    )?)
}

async fn ensure_avionics_model(
    db: &AppDb,
    manufacturer: &str,
    model: &str,
    avionics_type: &str,
) -> StoreResult<i64> {
    if !is_usable_avionics_label(manufacturer, model) {
        return Err(ListingStoreError::Validation(format!(
            "generic avionics labels cannot be stored: {manufacturer} {model}"
        )));
    }
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
        model,
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
    execute_query!(db, &insert_sql, name, normalized_name.as_str())?;
    let select_sql = format!("SELECT id FROM {table} WHERE normalized_name = ?");
    Ok(query_scalar_one!(
        db,
        i64,
        &select_sql,
        normalized_name.as_str()
    )?)
}

async fn replace_listing_avionics(
    db: &AppDb,
    listing_id: i64,
    avionics: &[ListingAvionicsValue],
) -> StoreResult<()> {
    execute_query!(
        db,
        "DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        listing_id
    )?;
    for item in avionics {
        if !is_usable_avionics_label(&item.manufacturer, &item.model) {
            continue;
        }
        let avionics_model_id =
            ensure_avionics_model(db, &item.manufacturer, &item.model, &item.avionics_type).await?;
        if let (Some(introduced_year), Some(estimated_unit_value_usd), Some(value_reference_year)) = (
            item.introduced_year,
            item.estimated_unit_value_usd,
            item.value_reference_year,
        ) {
            update_avionics_model_metadata(
                db,
                avionics_model_id,
                introduced_year,
                estimated_unit_value_usd,
                value_reference_year,
            )
            .await?;
        }
        execute_query!(
            db,
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id,
              avionics_model_id,
              quantity,
              source,
              source_notes
            )
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT (aircraft_sale_listing_id, avionics_model_id)
            DO UPDATE SET
              quantity = EXCLUDED.quantity,
              source = EXCLUDED.source,
              source_notes = EXCLUDED.source_notes,
              updated_at = CURRENT_TIMESTAMP
            "#,
            listing_id,
            avionics_model_id,
            item.quantity.max(1),
            item.source.as_str(),
            item.source_notes.as_deref()
        )?;
    }
    Ok(())
}

async fn update_avionics_model_metadata(
    db: &AppDb,
    avionics_model_id: i64,
    introduced_year: i64,
    estimated_unit_value_usd: f64,
    value_reference_year: i64,
) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
        UPDATE avionics_models
        SET introduced_year = COALESCE(introduced_year, ?),
            estimated_unit_value_usd = COALESCE(estimated_unit_value_usd, ?),
            value_reference_year = COALESCE(value_reference_year, ?),
            value_source = COALESCE(value_source, 'gemini-grounded'),
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        introduced_year,
        estimated_unit_value_usd,
        value_reference_year,
        avionics_model_id
    )?;
    Ok(())
}

async fn listing_from_row(db: &AppDb, row: ListingRow) -> StoreResult<SaleListing> {
    let listing_id = row.id;
    let aircraft_model_id = row.aircraft_model_id;
    let aircraft_model_variant_id = row.aircraft_model_variant_id;
    Ok(SaleListing {
        id: listing_id,
        aircraft_model_id,
        aircraft_model_variant_id,
        created_by_user_id: row.created_by_user_id,
        is_verified: row.is_verified,
        source_url: row.source_url,
        model_year: row.model_year,
        asking_price_usd: row.asking_price_usd,
        currency: row.currency,
        added_at: row.added_at,
        status: row.status,
        registration_number: row.registration_number,
        serial_number: row.serial_number,
        airframe_hours: row.airframe_hours,
        engine_hours: row.engine_hours,
        propeller_hours: row.propeller_hours,
        created_at: row.created_at,
        updated_at: row.updated_at,
        aircraft: AircraftSummary {
            manufacturer: row.aircraft_manufacturer,
            model: row.aircraft_model,
            variant: row.aircraft_variant,
            aircraft_model_id,
            aircraft_model_variant_id,
        },
        avionics: listing_avionics(db, listing_id).await?,
    })
}

async fn listing_avionics(db: &AppDb, listing_id: i64) -> StoreResult<Vec<ParsedAvionics>> {
    let rows = query_as_all!(
        db,
        ParsedAvionicsRow,
        r#"
        SELECT
          mfr.name AS manufacturer,
          model.name AS model,
          avionics_type.name AS avionics_type,
          link.quantity
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model
          ON model.id = link.avionics_model_id
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        JOIN avionics_types avionics_type
          ON avionics_type.id = model.avionics_type_id
        WHERE link.aircraft_sale_listing_id = ?
        ORDER BY link.id
        "#,
        listing_id
    )?;
    Ok(rows
        .into_iter()
        .map(|row| ParsedAvionics {
            manufacturer: row.manufacturer,
            model: row.model,
            avionics_type: row.avionics_type,
            quantity: row.quantity,
        })
        .collect())
}

fn required_string(value: Option<&str>, field_name: &str) -> StoreResult<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            ListingStoreError::Validation(format!(
                "cannot save listing; missing fields: {field_name}"
            ))
        })
}

fn required_string_from_value(value: &Value, field_name: &str) -> StoreResult<String> {
    optional_string(Some(value)).ok_or_else(|| {
        ListingStoreError::Validation(format!("cannot save listing; missing fields: {field_name}"))
    })
}

fn required_i64(value: Option<i64>, field_name: &str) -> StoreResult<i64> {
    value.ok_or_else(|| {
        ListingStoreError::Validation(format!("cannot save listing; missing fields: {field_name}"))
    })
}

fn required_f64(value: Option<f64>, field_name: &str) -> StoreResult<f64> {
    value.ok_or_else(|| {
        ListingStoreError::Validation(format!("cannot save listing; missing fields: {field_name}"))
    })
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use crate::db::{AppDb, DatabaseBackend};
    use crate::extract::{
        preview_manual_listing, AvionicsUnitResolutionCandidate, AvionicsUnitResolutionContext,
    };

    use super::{
        avionics_resolution_review_issues, avionics_secondary_check_review_issues,
        model_similarity, variant_label_issues, variant_normalization_groups_from_response,
        ListingValues, ModelVariantRow, MODEL_SIMILARITY_CONFIRMATION_THRESHOLD,
    };

    #[test]
    fn model_similarity_handles_compact_codes_without_special_cases() {
        assert!(
            model_similarity("T182", "CT182") >= MODEL_SIMILARITY_CONFIRMATION_THRESHOLD,
            "compact model codes should be close enough to ask the model"
        );
        assert!(
            model_similarity("182T", "Turbo 182T Skylane")
                >= MODEL_SIMILARITY_CONFIRMATION_THRESHOLD,
            "marketing words should not hide a shared aircraft model code"
        );
        assert!(
            model_similarity("SR22", "SR22T") >= MODEL_SIMILARITY_CONFIRMATION_THRESHOLD,
            "configuration-changing suffixes should be sent to the model for confirmation"
        );
    }

    #[test]
    fn variant_normalization_response_must_cover_sources_once() {
        let variants = vec![
            model_variant_row(1, "SR22-G6 TURBO", 7),
            model_variant_row(2, "SR22T-G6 GTS", 1),
        ];

        let groups = variant_normalization_groups_from_response(
            &json!({
                "groups": [
                    {
                        "canonical_variant": "SR22-G6 TURBO",
                        "source_variants": ["SR22-G6 TURBO", "SR22T-G6 GTS"],
                        "rationale": "same turbo configuration"
                    }
                ]
            }),
            &variants,
        )
        .expect("complete mapping should parse");

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical_variant, "SR22-G6 TURBO");

        let error = variant_normalization_groups_from_response(
            &json!({
                "groups": [
                    {
                        "canonical_variant": "SR22-G6 TURBO",
                        "source_variants": ["SR22-G6 TURBO"],
                        "rationale": "missing one source"
                    }
                ]
            }),
            &variants,
        )
        .expect_err("missing source variants must be rejected");

        assert!(error
            .to_string()
            .contains("did not cover source variants exactly once"));
    }

    #[test]
    fn variant_label_issues_flag_year_and_manufacturer() {
        let values = listing_values_with_variant("2023 CESSNA 182T SKYLANE");
        let issues = variant_label_issues(&values);

        assert!(issues.iter().any(|issue| issue.contains("manufacturer")));
        assert!(issues.iter().any(|issue| issue.contains("model year")));
    }

    #[test]
    fn variant_label_issues_accept_clean_variant() {
        let values = listing_values_with_variant("182T SKYLANE");
        assert!(variant_label_issues(&values).is_empty());
    }

    #[test]
    fn avionics_resolution_review_flags_structural_generic_indicators() {
        let context =
            avionics_resolution_context("Cessna", "300 Series Audio Panel", "Audio Panel");
        let issues = avionics_resolution_review_issues(
            &context,
            &json!({
                "status": "concrete",
                "manufacturer": "ARC (Cessna)",
                "model": "300 Series Audio Control Panel",
                "type": "Audio Panel",
                "quantity": 1,
                "introduced_year": 1970,
                "estimated_unit_value_usd": 300,
                "confidence": "medium",
                "source_url": "https://example.com",
                "source_title": "Example",
                "notes": "verified"
            }),
        );

        assert!(issues
            .iter()
            .any(|issue| issue.contains("parenthetical alias")));
        assert!(issues.iter().any(|issue| issue.contains("aircraft maker")));
        assert!(issues.iter().any(|issue| issue.contains("series/family")));
    }

    #[test]
    fn avionics_resolution_review_flags_class_and_multi_model_outputs() {
        let class_context = avionics_resolution_context("Cirrus", "ADS-B", "Transponder");
        let class_issues = avionics_resolution_review_issues(
            &class_context,
            &json!({
                "status": "concrete",
                "manufacturer": "Garmin",
                "model": "ADS-B Transponder",
                "type": "Transponder",
                "quantity": 1,
                "introduced_year": 2016,
                "estimated_unit_value_usd": 2000,
                "confidence": "medium",
                "source_url": "https://example.com",
                "source_title": "Example",
                "notes": "verified"
            }),
        );
        assert!(class_issues
            .iter()
            .any(|issue| issue.contains("equipment class")));

        let multi_context = avionics_resolution_context("Cirrus", "GNS 430/530", "NAV/COM");
        let multi_issues = avionics_resolution_review_issues(
            &multi_context,
            &json!({
                "status": "concrete",
                "manufacturer": "Garmin",
                "model": "GNS 430/530 Series",
                "type": "NAV/COM",
                "quantity": 1,
                "introduced_year": 1998,
                "estimated_unit_value_usd": 5000,
                "confidence": "medium",
                "source_url": "https://example.com",
                "source_title": "Example",
                "notes": "verified"
            }),
        );
        assert!(multi_issues
            .iter()
            .any(|issue| issue.contains("series/family")));
        assert!(multi_issues
            .iter()
            .any(|issue| issue.contains("multiple possible units")));
    }

    #[test]
    fn avionics_resolution_review_accepts_concrete_sourced_unit() {
        let context = avionics_resolution_context("Cessna", "GTX 345R", "Transponder");
        let issues = avionics_resolution_review_issues(
            &context,
            &json!({
                "status": "concrete",
                "manufacturer": "Garmin",
                "model": "GTX 345R",
                "type": "Transponder",
                "quantity": 1,
                "introduced_year": 2016,
                "estimated_unit_value_usd": 4500,
                "confidence": "high",
                "source_url": "https://example.com",
                "source_title": "Example",
                "notes": "verified exact unit"
            }),
        );

        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn secondary_avionics_review_flags_unchanged_generic_candidate() {
        let context = avionics_resolution_context("Cessna", "ADS-B Transponder", "Transponder");
        let issues = avionics_secondary_check_review_issues(
            &context,
            &json!({
                "status": "concrete",
                "manufacturer": "Garmin",
                "model": "ADS-B Transponder",
                "type": "Transponder"
            }),
            &json!({
                "classification": "generic",
                "manufacturer_is_avionics_maker": true,
                "model_identifies_single_unit": false,
                "confidence": "high",
                "generic_indicators": ["capability label"],
                "notes": "not one exact model"
            }),
        );

        assert!(issues
            .iter()
            .any(|issue| issue.contains("secondary classifier")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("does not identify one exact unit")));
    }

    #[test]
    fn secondary_avionics_review_allows_corrected_concrete_unit() {
        let context = avionics_resolution_context("Cessna", "ADS-B Transponder", "Transponder");
        let issues = avionics_secondary_check_review_issues(
            &context,
            &json!({
                "status": "concrete",
                "manufacturer": "Garmin",
                "model": "GTX 345R",
                "type": "Transponder"
            }),
            &json!({
                "classification": "generic",
                "manufacturer_is_avionics_maker": true,
                "model_identifies_single_unit": false,
                "confidence": "high",
                "generic_indicators": ["capability label"],
                "notes": "not one exact model"
            }),
        );

        assert!(issues.is_empty(), "{issues:?}");
    }

    fn model_variant_row(
        variant_id: i64,
        variant_name: &str,
        listing_count: i64,
    ) -> ModelVariantRow {
        ModelVariantRow {
            aircraft_model_id: 10,
            aircraft_manufacturer: "Cirrus".to_string(),
            aircraft_model: "SR22".to_string(),
            variant_id,
            variant_name: variant_name.to_string(),
            listing_count,
        }
    }

    fn avionics_resolution_context(
        aircraft_manufacturer: &str,
        model: &str,
        avionics_type: &str,
    ) -> AvionicsUnitResolutionContext {
        AvionicsUnitResolutionContext {
            aircraft_manufacturer: aircraft_manufacturer.to_string(),
            aircraft_model: "182 SKYLANE".to_string(),
            aircraft_variant: "182T".to_string(),
            model_year: 2009,
            source_url: "https://example.com/listing".to_string(),
            listing_context: String::new(),
            candidate: AvionicsUnitResolutionCandidate {
                manufacturer: "Garmin".to_string(),
                model: model.to_string(),
                avionics_type: avionics_type.to_string(),
                quantity: 1,
            },
            value_reference_year: 2026,
        }
    }

    fn listing_values_with_variant(variant: &str) -> ListingValues {
        ListingValues {
            manufacturer: "Cessna".to_string(),
            model: "182 SKYLANE".to_string(),
            variant: variant.to_string(),
            source_url: Some("https://example.com/listing".to_string()),
            model_year: 2023,
            asking_price_usd: 699000.0,
            currency: "USD".to_string(),
            registration_number: Some("N414PK".to_string()),
            serial_number: Some("18283243".to_string()),
            status: "active".to_string(),
            airframe_hours: 357.0,
            engine_hours: 357.0,
            propeller_hours: 357.0,
            avionics: Vec::new(),
        }
    }

    #[tokio::test]
    async fn delete_listing_preserves_and_detaches_plugin_submission() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        execute_query!(&db, "DROP TABLE plugin_submissions")
            .expect("new submission table should be replaceable");
        execute_query!(
            &db,
            r#"
            CREATE TABLE plugin_submissions (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              user_id INTEGER NOT NULL REFERENCES users(id),
              plugin_install_id INTEGER NOT NULL REFERENCES plugin_installs(id),
              source_url TEXT NOT NULL,
              submitted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
              rendered_html TEXT NOT NULL,
              rendered_html_sha256 TEXT NOT NULL,
              signature_base64 TEXT NOT NULL,
              extracted_listing_json TEXT,
              extraction_error TEXT,
              canonical_listing_id INTEGER REFERENCES aircraft_sale_listings(id)
            )
            "#
        )
        .expect("legacy restrictive submission foreign key should seed");
        let variant_id = super::ensure_aircraft_model_variant(&db, "Cessna", "182 Skylane", "182T")
            .await
            .expect("variant should seed");
        let listing_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id,
              created_by_user_id,
              source_url,
              model_year,
              asking_price_usd,
              currency,
              status,
              airframe_hours,
              engine_hours,
              propeller_hours
            )
            VALUES (?, ?, 'https://example.test/listing', 2023, 699000, 'USD', 'active', 357, 357, 357)
            RETURNING id
            "#,
            variant_id,
            user.id
        )
        .expect("listing should seed");
        let install_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO plugin_installs (user_id, public_key_base64)
            VALUES (?, 'test-key')
            RETURNING id
            "#,
            user.id
        )
        .expect("plugin install should seed");
        let submission_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO plugin_submissions (
              user_id,
              plugin_install_id,
              source_url,
              rendered_html,
              rendered_html_sha256,
              signature_base64,
              canonical_listing_id
            )
            VALUES (?, ?, 'https://example.test/listing', '<html></html>', 'hash', 'signature', ?)
            RETURNING id
            "#,
            user.id,
            install_id,
            listing_id
        )
        .expect("plugin submission should seed");

        super::delete_listing(&db, user.id, listing_id)
            .await
            .expect("listing deletion should detach the retained submission");

        let listing_count = query_scalar_one!(
            &db,
            i64,
            "SELECT COUNT(*) FROM aircraft_sale_listings WHERE id = ?",
            listing_id
        )
        .expect("listing count should load");
        let canonical_listing_id = query_scalar_one!(
            &db,
            Option<i64>,
            "SELECT canonical_listing_id FROM plugin_submissions WHERE id = ?",
            submission_id
        )
        .expect("submission should remain queryable");

        assert_eq!(listing_count, 0);
        assert_eq!(canonical_listing_id, None);
    }

    #[tokio::test]
    async fn create_listing_inserts_model_backed_sale_listing() {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aircost-create-listing-{}-{unique_suffix}.sqlite3",
            std::process::id()
        ));
        let database_url = format!("sqlite://{}", path.to_string_lossy());
        let db = AppDb::connect(&database_url)
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let variant_id =
            super::ensure_aircraft_model_variant(&db, "Cessna", "182 Skylane", "182T Skylane")
                .await
                .expect("variant should seed");
        let avionics_model_id =
            super::ensure_avionics_model(&db, "Garmin", "G1000 NXi", "Integrated Flight Deck")
                .await
                .expect("avionics model should seed");
        execute_query!(
            &db,
            r#"
            UPDATE avionics_models
            SET introduced_year = 2017,
                estimated_unit_value_usd = 50000,
                value_reference_year = 2026
            WHERE id = ?
            "#,
            avionics_model_id
        )
        .expect("avionics metadata should seed");
        execute_query!(
            &db,
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
            VALUES (?, 2023, 699000, 2023, 'https://example.test', 'test', 'test fixture', 'high')
            "#,
            variant_id
        )
        .expect("price point should seed");
        execute_query!(
            &db,
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
            VALUES (?, 2023, ?, 1, 'https://example.test', 'test', 'test fixture', 'high')
            "#,
            variant_id,
            avionics_model_id
        )
        .expect("default avionics should seed");
        let preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182 Skylane",
            "variant": "182T Skylane",
            "model_year": 2023,
            "asking_price_usd": 699000,
            "currency": "USD",
            "airframe_hours": 357,
            "engine_hours": 357,
            "propeller_hours": 357,
            "status": "active",
            "registration_number": "NTEST1",
            "serial_number": "TESTSERIAL",
            "avionics": []
        }));

        let listing = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect("listing should insert");

        assert_eq!(
            listing.aircraft_model_id,
            listing.aircraft.aircraft_model_id
        );
        assert_eq!(
            listing.aircraft_model_variant_id,
            listing.aircraft.aircraft_model_variant_id
        );
        assert_eq!(listing.aircraft.manufacturer, "Cessna");
        assert_eq!(listing.aircraft.model, "182 Skylane");
        assert_eq!(listing.aircraft.variant, "182T Skylane");
        assert_eq!(listing.registration_number.as_deref(), Some("NTEST1"));

        drop(db);
        let _ = std::fs::remove_file(path);
    }
}
