use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;

use crate::aircraft::enrich_aircraft_spec_for_listing_if_missing;
use crate::aircraft::faa::{
    normalize_serial_key, require_aircraft_admission, require_listing_admission,
    AircraftAdmissionError, AircraftGrounding,
};
use crate::avionics::catalog::{
    resolve_avionics_identity, ApprovedAvionicsIdentity, AvionicsIdentityOutcome,
    AvionicsIdentityRequest,
};
use crate::avionics::{
    enrich_listing_avionics_metadata, enrich_model_year_avionics_and_price_point_for_listing,
};
use crate::cleanup::{cleanup_orphan_records, CleanupError};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{
    optional_f64, optional_i64, optional_string, GeminiListingExtractor,
    ModelFamilyConfirmationContext, VariantConfirmationContext, VariantLabelCorrectionContext,
    VariantNormalizationCandidate, VariantNormalizationContext, VariantNormalizationExample,
};
use crate::models::{
    is_plausible_asking_price_usd, AircraftSummary, ListingPreview, ListingValuationFact,
    ParsedAvionics, ParsedAvionicsReference, ParsedInstalledComponent, SaleListing,
};
use crate::normalize::{
    is_usable_avionics_label, normalize_avionics_manufacturer_name, normalize_avionics_model_name,
    normalize_name,
};

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
    Ingestion { listing_id: i64, message: String },
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
            ListingStoreError::Ingestion {
                listing_id,
                message,
            } => write!(formatter, "listing {listing_id} was quarantined: {message}"),
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
    engine_hours: Option<f64>,
    engine_time_basis: String,
    engine_time_evidence: Option<String>,
    engine_time_confidence: Option<String>,
    propeller_hours: Option<f64>,
    propeller_time_basis: String,
    propeller_time_evidence: Option<String>,
    propeller_time_confidence: Option<String>,
    installed_engine_model_id: Option<i64>,
    installed_engine: Option<ParsedInstalledComponent>,
    installed_engine_evidence_text: Option<String>,
    installed_engine_confidence: Option<String>,
    installed_propeller_model_id: Option<i64>,
    installed_propeller: Option<ParsedInstalledComponent>,
    installed_propeller_evidence_text: Option<String>,
    installed_propeller_confidence: Option<String>,
    avionics: Vec<ListingAvionicsValue>,
    valuation_facts: Vec<ListingValuationFact>,
}

#[derive(Clone, Debug)]
struct ListingAvionicsValue {
    avionics_model_id: Option<i64>,
    manufacturer: String,
    model: String,
    avionics_types: Vec<String>,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    configuration_action: String,
    replaces: Option<ParsedAvionicsReference>,
    replaces_avionics_model_id: Option<i64>,
}

impl ListingAvionicsValue {
    fn from_parsed(item: ParsedAvionics) -> Self {
        Self {
            avionics_model_id: None,
            manufacturer: item.manufacturer,
            model: item.model,
            avionics_types: item.avionics_types,
            quantity: item.quantity,
            source: "listing".to_string(),
            source_notes: item.source_evidence_text,
            source_confidence: item.source_confidence,
            configuration_action: item.configuration_action,
            replaces: item.replaces,
            replaces_avionics_model_id: None,
        }
    }
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
    ingestion_state: String,
    ingestion_error: Option<String>,
    ingestion_completed_at: Option<String>,
    registration_number: Option<String>,
    serial_number: Option<String>,
    airframe_hours: f64,
    engine_hours: Option<f64>,
    engine_time_basis: String,
    engine_time_evidence: Option<String>,
    engine_time_confidence: Option<String>,
    propeller_hours: Option<f64>,
    propeller_time_basis: String,
    propeller_time_evidence: Option<String>,
    propeller_time_confidence: Option<String>,
    installed_engine_model_id: Option<i64>,
    installed_engine_source_url: Option<String>,
    installed_engine_evidence_text: Option<String>,
    installed_engine_confidence: Option<String>,
    installed_propeller_model_id: Option<i64>,
    installed_propeller_source_url: Option<String>,
    installed_propeller_evidence_text: Option<String>,
    installed_propeller_confidence: Option<String>,
    created_at: String,
    updated_at: String,
    aircraft_manufacturer: String,
    aircraft_model: String,
    aircraft_variant: String,
}

#[derive(Debug, FromRow)]
struct ListingFactRow {
    fact_kind: String,
    fact_value: String,
    evidence_text: String,
    source_url: Option<String>,
    source_confidence: String,
}

#[derive(Debug, FromRow)]
struct InstalledComponentIdentityRow {
    manufacturer: String,
    model: String,
}

#[derive(Debug, FromRow)]
struct ParsedAvionicsRow {
    avionics_model_id: i64,
    manufacturer: String,
    model: String,
    quantity: i64,
    configuration_action: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    replaces_avionics_model_id: Option<i64>,
    replaces_manufacturer: Option<String>,
    replaces_model: Option<String>,
}

#[derive(Debug, FromRow)]
struct AvionicsCapabilityRow {
    avionics_model_id: i64,
    avionics_type: String,
}

#[derive(Debug, FromRow)]
struct ListingOwnerRow {
    created_by_user_id: i64,
    is_verified: bool,
}

#[derive(Clone, Debug, FromRow)]
struct MissingIdentitySourceCandidateRow {
    id: i64,
    serial_number: Option<String>,
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
struct ListingAdmissionRow {
    listing_id: i64,
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
    let missing_identity_source_candidate = match values.source_url.as_deref() {
        Some(source_url) => {
            unverified_listing_for_missing_identity_source(db, user_id, source_url).await?
        }
        None => None,
    };
    let admission_serial = serial_evidence_for_identity_repair_admission(
        values.serial_number.as_deref(),
        missing_identity_source_candidate
            .as_ref()
            .and_then(|candidate| candidate.serial_number.as_deref()),
    )?;
    let grounding = require_aircraft_admission(
        db,
        values.registration_number.as_deref(),
        admission_serial.as_deref(),
    )
    .await
    .map_err(listing_admission_error)?;
    apply_faa_grounding_identity(&mut values, &grounding);
    let identity_repair_listing_id = match (
        values.source_url.as_deref(),
        missing_identity_source_candidate.as_ref(),
    ) {
        (Some(source_url), Some(candidate)) => {
            persist_faa_identity_for_missing_identity_source(
                db, user_id, source_url, candidate, &grounding,
            )
            .await?
        }
        _ => None,
    };
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

    // Prefer the exact source row repaired above. Looking it up again by tail
    // could select a different, newer listing if the user has retained more
    // than one observation for the same aircraft.
    if let Some(listing_id) = identity_repair_listing_id {
        emit_listing_progress(progress, "saving_listing", "Repairing existing listing");
        update_listing_values(db, listing_id, &values, true).await?;
        emit_listing_progress(
            progress,
            "refreshing_estimates",
            "Refreshing valuation inputs",
        );
        finalize_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref())
            .await?;
        return get_listing(db, user_id, listing_id).await;
    }

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
            finalize_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref())
                .await?;
            return get_listing(db, user_id, listing_id).await;
        }
    }

    if let Some(source_url) = values.source_url.as_deref() {
        if let Some(listing_id) =
            unverified_listing_id_for_missing_identity_source(db, user_id, source_url).await?
        {
            emit_listing_progress(progress, "saving_listing", "Repairing existing listing");
            update_listing_values(db, listing_id, &values, true).await?;
            emit_listing_progress(
                progress,
                "refreshing_estimates",
                "Refreshing valuation inputs",
            );
            finalize_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref())
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
        mark_listing_incomplete(db, listing_id).await?;
        finalize_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref())
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
    finalize_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref()).await?;
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
    // Preflight the complete bounded run before calling Gemini or mutating any
    // model. A later ungrounded group must not leave a partially healed batch.
    for model_group in &model_groups {
        require_model_listings_faa_admitted(
            db,
            &model_group.aircraft_manufacturer,
            &model_group.aircraft_model,
        )
        .await?;
    }
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
    require_model_listings_faa_admitted(db, manufacturer, model).await?;
    normalize_variants_for_model_after_admission(db, extractor, manufacturer, model, apply).await
}

async fn normalize_variants_for_model_after_admission(
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

async fn require_model_listings_faa_admitted(
    db: &AppDb,
    manufacturer: &str,
    model: &str,
) -> StoreResult<()> {
    let manufacturer_key = normalize_name(manufacturer);
    let model_key = normalize_name(model);
    let listings = query_as_all!(
        db,
        ListingAdmissionRow,
        r#"
        SELECT listing.id AS listing_id
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
        JOIN aircraft_manufacturers manufacturer
          ON manufacturer.id = model.aircraft_manufacturer_id
        WHERE manufacturer.normalized_name = ?
          AND model.normalized_name = ?
        ORDER BY listing.id
        "#,
        manufacturer_key.as_str(),
        model_key.as_str()
    )?;
    for listing in listings {
        require_listing_admission(db, listing.listing_id)
            .await
            .map_err(listing_admission_error)?;
    }
    Ok(())
}

fn listing_admission_error(error: AircraftAdmissionError) -> ListingStoreError {
    let message = error.to_string();
    match error {
        AircraftAdmissionError::Rejected { .. } => ListingStoreError::Validation(message),
        AircraftAdmissionError::LookupFailed { .. }
        | AircraftAdmissionError::ListingNotFound { .. } => ListingStoreError::State(message),
    }
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
    let grounding = require_aircraft_admission(
        db,
        values.registration_number.as_deref(),
        values.serial_number.as_deref(),
    )
    .await
    .map_err(listing_admission_error)?;
    apply_faa_grounding_identity(&mut values, &grounding);
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
    finalize_listing_ingestion(db, listing_id, extractor, None).await?;
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
    let installed_engine_model_id = resolve_installed_engine_model_id(db, values).await?;
    let installed_propeller_model_id = resolve_installed_propeller_model_id(db, values).await?;
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
          ingestion_state,
          registration_number,
          serial_number,
          airframe_hours,
          engine_hours,
          engine_time_basis,
          engine_time_evidence,
          engine_time_confidence,
          propeller_hours,
          propeller_time_basis,
          propeller_time_evidence,
          propeller_time_confidence,
          installed_engine_model_id,
          installed_engine_source_url,
          installed_engine_evidence_text,
          installed_engine_confidence,
          installed_propeller_model_id,
          installed_propeller_source_url,
          installed_propeller_evidence_text,
          installed_propeller_confidence
        )
        VALUES (?, ?, FALSE, ?, ?, ?, ?, CURRENT_TIMESTAMP, ?, 'incomplete', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        values.engine_time_basis.as_str(),
        values.engine_time_evidence.as_deref(),
        values.engine_time_confidence.as_deref(),
        values.propeller_hours,
        values.propeller_time_basis.as_str(),
        values.propeller_time_evidence.as_deref(),
        values.propeller_time_confidence.as_deref(),
        installed_engine_model_id,
        installed_engine_model_id.and(values.source_url.as_deref()),
        values.installed_engine_evidence_text.as_deref(),
        values.installed_engine_confidence.as_deref(),
        installed_propeller_model_id,
        installed_propeller_model_id.and(values.source_url.as_deref()),
        values.installed_propeller_evidence_text.as_deref(),
        values.installed_propeller_confidence.as_deref()
    )?;

    if let Err(error) = replace_listing_avionics(db, listing_id, &values.avionics).await {
        return quarantine_after_error(db, listing_id, error).await;
    }
    if let Err(error) = replace_listing_facts(db, listing_id, values).await {
        return quarantine_after_error(db, listing_id, error).await;
    }
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
    let installed_engine_model_id = resolve_installed_engine_model_id(db, values).await?;
    let installed_propeller_model_id = resolve_installed_propeller_model_id(db, values).await?;
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
              ingestion_state = 'incomplete',
              ingestion_error = NULL,
              ingestion_completed_at = NULL,
              registration_number = ?,
              serial_number = ?,
              airframe_hours = ?,
              engine_hours = ?,
              engine_time_basis = ?,
              engine_time_evidence = ?,
              engine_time_confidence = ?,
              propeller_hours = ?,
              propeller_time_basis = ?,
              propeller_time_evidence = ?,
              propeller_time_confidence = ?,
              installed_engine_model_id = ?,
              installed_engine_source_url = ?,
              installed_engine_evidence_text = ?,
              installed_engine_confidence = ?,
              installed_propeller_model_id = ?,
              installed_propeller_source_url = ?,
              installed_propeller_evidence_text = ?,
              installed_propeller_confidence = ?,
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
        values.engine_time_basis.as_str(),
        values.engine_time_evidence.as_deref(),
        values.engine_time_confidence.as_deref(),
        values.propeller_hours,
        values.propeller_time_basis.as_str(),
        values.propeller_time_evidence.as_deref(),
        values.propeller_time_confidence.as_deref(),
        installed_engine_model_id,
        installed_engine_model_id.and(values.source_url.as_deref()),
        values.installed_engine_evidence_text.as_deref(),
        values.installed_engine_confidence.as_deref(),
        installed_propeller_model_id,
        installed_propeller_model_id.and(values.source_url.as_deref()),
        values.installed_propeller_evidence_text.as_deref(),
        values.installed_propeller_confidence.as_deref(),
        listing_id
    )?;
    if let Err(error) = replace_listing_avionics(db, listing_id, &values.avionics).await {
        return quarantine_after_error(db, listing_id, error).await;
    }
    if let Err(error) = replace_listing_facts(db, listing_id, values).await {
        return quarantine_after_error(db, listing_id, error).await;
    }
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
        engine_hours: parsed.engine_hours,
        engine_time_basis: parsed.engine_time_basis.clone(),
        engine_time_evidence: parsed.engine_time_evidence.clone(),
        engine_time_confidence: parsed.engine_time_confidence.clone(),
        propeller_hours: parsed.propeller_hours,
        propeller_time_basis: parsed.propeller_time_basis.clone(),
        propeller_time_evidence: parsed.propeller_time_evidence.clone(),
        propeller_time_confidence: parsed.propeller_time_confidence.clone(),
        installed_engine_model_id: None,
        installed_engine: parsed.installed_engine.clone(),
        installed_engine_evidence_text: parsed
            .installed_engine
            .as_ref()
            .map(|component| component.evidence_text.clone()),
        installed_engine_confidence: parsed
            .installed_engine
            .as_ref()
            .map(|component| component.confidence.clone()),
        installed_propeller_model_id: None,
        installed_propeller: parsed.installed_propeller.clone(),
        installed_propeller_evidence_text: parsed
            .installed_propeller
            .as_ref()
            .map(|component| component.evidence_text.clone()),
        installed_propeller_confidence: parsed
            .installed_propeller
            .as_ref()
            .map(|component| component.confidence.clone()),
        avionics: parsed
            .avionics
            .clone()
            .into_iter()
            .map(ListingAvionicsValue::from_parsed)
            .collect(),
        valuation_facts: parsed.valuation_facts.clone(),
    };
    validate_listing_values(&values)?;
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
    let listing_context = listing_context
        .map(listing_context_excerpt)
        .unwrap_or_default();
    let mut resolved: Vec<ListingAvionicsValue> = Vec::new();
    let mut unresolved = Vec::new();

    for item in values.avionics.clone() {
        let Some(extractor) = extractor else {
            unresolved.push(format!(
                "{} {} (Gemini identity resolver unavailable)",
                item.manufacturer, item.model
            ));
            continue;
        };
        let identity_request = listing_avionics_identity_request(
            values,
            source_url,
            &listing_context,
            &item.manufacturer,
            &item.model,
            &item.avionics_types,
            item.quantity,
        );
        let identity = match resolve_avionics_identity(db, extractor, &identity_request)
            .await
            .map_err(|error| {
                ListingStoreError::State(format!(
                    "avionics identity resolution failed for {} {}: {error}",
                    item.manufacturer, item.model
                ))
            })? {
            AvionicsIdentityOutcome::Approved(identity) => identity,
            AvionicsIdentityOutcome::Rejected { .. } => continue,
            AvionicsIdentityOutcome::Unresolved { reason } => {
                unresolved.push(format!("{} {} ({reason})", item.manufacturer, item.model));
                continue;
            }
        };
        let resolved_item = listing_avionics_value_from_catalog(&item, &identity);
        let Some(resolved_item) = resolve_listing_avionics_replacement(
            db,
            values,
            resolved_item,
            extractor,
            source_url,
            &listing_context,
            &mut unresolved,
        )
        .await?
        else {
            continue;
        };
        resolved.push(resolved_item);
    }

    if !unresolved.is_empty() {
        unresolved.sort();
        unresolved.dedup();
        return Err(ListingStoreError::Validation(format!(
            "unresolved avionics catalog mappings: {}",
            unresolved.join(", ")
        )));
    }
    values.avionics = coalesce_resolved_listing_avionics(resolved)?;
    Ok(())
}

fn listing_avionics_identity_request(
    values: &ListingValues,
    source_url: Option<&str>,
    listing_context: &str,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
    quantity: i64,
) -> AvionicsIdentityRequest {
    AvionicsIdentityRequest {
        aircraft_manufacturer: values.manufacturer.clone(),
        aircraft_model: values.model.clone(),
        aircraft_variant: values.variant.clone(),
        model_year: values.model_year,
        source_url: source_url.unwrap_or("").to_string(),
        listing_context: listing_context.to_string(),
        requires_listing_evidence: true,
        manufacturer: manufacturer.to_string(),
        model: model.to_string(),
        avionics_types: avionics_types.to_vec(),
        quantity: quantity.max(1),
    }
}

async fn resolve_listing_avionics_replacement(
    db: &AppDb,
    values: &ListingValues,
    mut item: ListingAvionicsValue,
    extractor: &GeminiListingExtractor,
    source_url: Option<&str>,
    listing_context: &str,
    unresolved: &mut Vec<String>,
) -> StoreResult<Option<ListingAvionicsValue>> {
    if item.configuration_action == "installed" {
        if item.replaces.is_some() || item.replaces_avionics_model_id.is_some() {
            return Err(ListingStoreError::Validation(format!(
                "installed avionics cannot also declare a replacement target: {} {}",
                item.manufacturer, item.model
            )));
        }
        return Ok(Some(item));
    }
    let Some(replaced) = item.replaces.as_ref() else {
        return Err(ListingStoreError::Validation(format!(
            "avionics action {} requires a concrete replacement target: {} {}",
            item.configuration_action, item.manufacturer, item.model
        )));
    };
    let request = listing_avionics_identity_request(
        values,
        source_url,
        listing_context,
        &replaced.manufacturer,
        &replaced.model,
        &replaced.avionics_types,
        1,
    );
    let identity = match resolve_avionics_identity(db, extractor, &request)
        .await
        .map_err(|error| {
            ListingStoreError::State(format!(
                "replacement avionics identity resolution failed for {} {}: {error}",
                replaced.manufacturer, replaced.model
            ))
        })? {
        AvionicsIdentityOutcome::Approved(identity) => identity,
        AvionicsIdentityOutcome::Rejected { reason }
        | AvionicsIdentityOutcome::Unresolved { reason } => {
            unresolved.push(format!(
                "replacement {} {} ({reason})",
                replaced.manufacturer, replaced.model
            ));
            return Ok(None);
        }
    };
    item.replaces = Some(ParsedAvionicsReference {
        manufacturer: identity.manufacturer.clone(),
        model: identity.model.clone(),
        avionics_types: identity.avionics_types.clone(),
    });
    item.replaces_avionics_model_id = Some(identity.id);
    Ok(Some(item))
}

fn listing_avionics_value_from_catalog(
    original: &ListingAvionicsValue,
    identity: &ApprovedAvionicsIdentity,
) -> ListingAvionicsValue {
    let identity_notes = format!(
        "Curated catalog id {}; {} Evidence: {} — {}",
        identity.id, identity.reason, identity.evidence_title, identity.evidence
    );
    let source_notes = original
        .source_notes
        .as_deref()
        .map(|notes| format!("{notes} {identity_notes}"))
        .unwrap_or(identity_notes);
    ListingAvionicsValue {
        avionics_model_id: Some(identity.id),
        manufacturer: identity.manufacturer.clone(),
        model: identity.model.clone(),
        avionics_types: identity.avionics_types.clone(),
        quantity: original.quantity.max(1),
        source: original.source.clone(),
        source_notes: Some(source_notes),
        // Product identity and installation evidence are independent. A
        // grounded catalog match must never upgrade a weak listing mention.
        source_confidence: original.source_confidence.clone(),
        configuration_action: original.configuration_action.clone(),
        replaces: original.replaces.clone(),
        replaces_avionics_model_id: original.replaces_avionics_model_id,
    }
}

fn merged_avionics_types(left: &[String], right: &[String]) -> Vec<String> {
    let mut merged = left.to_vec();
    for avionics_type in right {
        if !merged
            .iter()
            .any(|known| normalize_name(known) == normalize_name(avionics_type))
        {
            merged.push(avionics_type.clone());
        }
    }
    merged.sort_by_key(|value| normalize_name(value));
    merged
}

fn merge_duplicate_listing_avionics(
    existing: &mut ListingAvionicsValue,
    incoming: &ListingAvionicsValue,
) -> StoreResult<()> {
    let existing_model_id = existing.avionics_model_id.filter(|id| *id > 0);
    let incoming_model_id = incoming.avionics_model_id.filter(|id| *id > 0);
    if existing_model_id.is_none() || existing_model_id != incoming_model_id {
        return Err(ListingStoreError::State(
            "only rows for the same resolved avionics catalog product can be coalesced".to_string(),
        ));
    }
    if normalize_avionics_manufacturer_name(&existing.manufacturer)
        != normalize_avionics_manufacturer_name(&incoming.manufacturer)
        || normalize_avionics_model_name(&existing.model)
            != normalize_avionics_model_name(&incoming.model)
    {
        return Err(ListingStoreError::Validation(format!(
            "catalog avionics model {} was paired with conflicting canonical identities",
            existing_model_id.expect("resolved catalog id checked above")
        )));
    }

    let replacement_semantics_match = match existing.configuration_action.as_str() {
        "installed" => {
            incoming.configuration_action == "installed"
                && existing.replaces.is_none()
                && incoming.replaces.is_none()
                && existing.replaces_avionics_model_id.is_none()
                && incoming.replaces_avionics_model_id.is_none()
        }
        "replaces" | "removes" => {
            incoming.configuration_action == existing.configuration_action
                && matches!(
                    (
                        existing.replaces_avionics_model_id,
                        incoming.replaces_avionics_model_id
                    ),
                    (Some(existing_target), Some(incoming_target))
                        if existing_target > 0 && existing_target == incoming_target
                )
                && matching_avionics_reference(
                    existing.replaces.as_ref(),
                    incoming.replaces.as_ref(),
                )
        }
        _ => false,
    };
    if !replacement_semantics_match {
        return Err(ListingStoreError::Validation(format!(
            "catalog avionics model {} has conflicting installation actions or replacement targets",
            existing_model_id.expect("resolved catalog id checked above")
        )));
    }

    // Multiple capability mentions describe one physical unit, not additive
    // quantities. Preserve all evidence, and let the weakest mention govern
    // confidence so a strong duplicate cannot upgrade a weak one.
    existing.quantity = existing.quantity.max(incoming.quantity);
    existing.avionics_types =
        merged_avionics_types(&existing.avionics_types, &incoming.avionics_types);
    existing.source_notes = merged_source_notes(
        existing.source_notes.as_deref(),
        incoming.source_notes.as_deref(),
    );
    existing.source_confidence = conservative_source_confidence(
        existing.source_confidence.as_deref(),
        incoming.source_confidence.as_deref(),
    )?;
    Ok(())
}

fn matching_avionics_reference(
    left: Option<&ParsedAvionicsReference>,
    right: Option<&ParsedAvionicsReference>,
) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    normalize_avionics_manufacturer_name(&left.manufacturer)
        == normalize_avionics_manufacturer_name(&right.manufacturer)
        && normalize_avionics_model_name(&left.model) == normalize_avionics_model_name(&right.model)
        && canonical_avionics_types(&left.avionics_types)
            == canonical_avionics_types(&right.avionics_types)
}

fn merged_source_notes(left: Option<&str>, right: Option<&str>) -> Option<String> {
    let mut notes = Vec::new();
    for note in [left, right].into_iter().flatten() {
        for line in note.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if !notes.contains(&line) {
                notes.push(line);
            }
        }
    }
    (!notes.is_empty()).then(|| notes.join("\n"))
}

fn conservative_source_confidence(
    left: Option<&str>,
    right: Option<&str>,
) -> StoreResult<Option<String>> {
    fn rank(confidence: &str) -> Option<u8> {
        match confidence {
            "low" => Some(0),
            "medium" => Some(1),
            "high" => Some(2),
            _ => None,
        }
    }

    for confidence in [left, right].into_iter().flatten() {
        if rank(confidence).is_none() {
            return Err(ListingStoreError::Validation(format!(
                "invalid avionics source confidence while coalescing duplicates: {confidence}"
            )));
        }
    }
    let (Some(left), Some(right)) = (left, right) else {
        return Ok(None);
    };
    let left_rank = rank(left).expect("confidence values checked above");
    let right_rank = rank(right).expect("confidence values checked above");
    Ok(Some(
        if left_rank <= right_rank { left } else { right }.to_string(),
    ))
}

fn coalesce_resolved_listing_avionics(
    avionics: impl IntoIterator<Item = ListingAvionicsValue>,
) -> StoreResult<Vec<ListingAvionicsValue>> {
    let mut coalesced: Vec<ListingAvionicsValue> = Vec::new();
    let mut seen = HashMap::<i64, usize>::new();
    for item in avionics {
        let avionics_model_id = item.avionics_model_id.filter(|id| *id > 0).ok_or_else(|| {
            ListingStoreError::Validation(format!(
                "avionics must resolve to a catalog id before persistence: {} {}",
                item.manufacturer, item.model
            ))
        })?;
        if let Some(index) = seen.get(&avionics_model_id).copied() {
            merge_duplicate_listing_avionics(&mut coalesced[index], &item)?;
        } else {
            seen.insert(avionics_model_id, coalesced.len());
            coalesced.push(item);
        }
    }
    Ok(coalesced)
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
    let intersection = left_tokens.intersection(right_tokens).count();
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
        engine_time_basis: listing.engine_time_basis.clone(),
        engine_time_evidence: listing.engine_time_evidence.clone(),
        engine_time_confidence: listing.engine_time_confidence.clone(),
        propeller_hours: listing.propeller_hours,
        propeller_time_basis: listing.propeller_time_basis.clone(),
        propeller_time_evidence: listing.propeller_time_evidence.clone(),
        propeller_time_confidence: listing.propeller_time_confidence.clone(),
        installed_engine_model_id: listing.installed_engine_model_id,
        installed_engine: None,
        installed_engine_evidence_text: listing.installed_engine_evidence_text.clone(),
        installed_engine_confidence: listing.installed_engine_confidence.clone(),
        installed_propeller_model_id: listing.installed_propeller_model_id,
        installed_propeller: None,
        installed_propeller_evidence_text: listing.installed_propeller_evidence_text.clone(),
        installed_propeller_confidence: listing.installed_propeller_confidence.clone(),
        avionics: listing
            .avionics
            .clone()
            .into_iter()
            .map(ListingAvionicsValue::from_parsed)
            .collect(),
        valuation_facts: listing.valuation_facts.clone(),
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
            "engine_hours" => values.engine_hours = optional_f64(Some(value)),
            "engine_time_basis" => {
                values.engine_time_basis = component_time_basis_from_value(value, key)?
            }
            "engine_time_evidence" => values.engine_time_evidence = optional_string(Some(value)),
            "engine_time_confidence" => {
                values.engine_time_confidence = optional_confidence_from_value(value, key)?
            }
            "propeller_hours" => values.propeller_hours = optional_f64(Some(value)),
            "propeller_time_basis" => {
                values.propeller_time_basis = component_time_basis_from_value(value, key)?
            }
            "propeller_time_evidence" => {
                values.propeller_time_evidence = optional_string(Some(value))
            }
            "propeller_time_confidence" => {
                values.propeller_time_confidence = optional_confidence_from_value(value, key)?
            }
            "installed_engine" => {
                values.installed_engine = installed_component_from_value(value, key)?;
                values.installed_engine_model_id = None;
                values.installed_engine_evidence_text = values
                    .installed_engine
                    .as_ref()
                    .map(|component| component.evidence_text.clone());
                values.installed_engine_confidence = values
                    .installed_engine
                    .as_ref()
                    .map(|component| component.confidence.clone());
            }
            "installed_propeller" => {
                values.installed_propeller = installed_component_from_value(value, key)?;
                values.installed_propeller_model_id = None;
                values.installed_propeller_evidence_text = values
                    .installed_propeller
                    .as_ref()
                    .map(|component| component.evidence_text.clone());
                values.installed_propeller_confidence = values
                    .installed_propeller
                    .as_ref()
                    .map(|component| component.confidence.clone());
            }
            "registration_number" => values.registration_number = optional_string(Some(value)),
            "serial_number" => values.serial_number = optional_string(Some(value)),
            "status" => {
                values.status = optional_string(Some(value)).unwrap_or_else(|| "active".to_string())
            }
            "source_url" => values.source_url = optional_string(Some(value)),
            "avionics" => values.avionics = avionics_from_value(value),
            "valuation_facts" => values.valuation_facts = valuation_facts_from_value(value)?,
            _ => {
                return Err(ListingStoreError::Validation(format!(
                    "unsupported listing field: {key}"
                )))
            }
        }
    }
    validate_listing_values(values)?;
    Ok(())
}

fn installed_component_from_value(
    value: &Value,
    field_name: &str,
) -> StoreResult<Option<ParsedInstalledComponent>> {
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or_else(|| {
        ListingStoreError::Validation(format!("{field_name} must be an object or null"))
    })?;
    let confidence = required_string_from_value(
        object.get("confidence").unwrap_or(&Value::Null),
        &format!("{field_name}.confidence"),
    )?;
    if !matches!(confidence.as_str(), "high" | "medium" | "low") {
        return Err(ListingStoreError::Validation(format!(
            "{field_name}.confidence must be high, medium, or low"
        )));
    }
    Ok(Some(ParsedInstalledComponent {
        manufacturer: required_string_from_value(
            object.get("manufacturer").unwrap_or(&Value::Null),
            &format!("{field_name}.manufacturer"),
        )?,
        model: required_string_from_value(
            object.get("model").unwrap_or(&Value::Null),
            &format!("{field_name}.model"),
        )?,
        evidence_text: required_string_from_value(
            object.get("evidence_text").unwrap_or(&Value::Null),
            &format!("{field_name}.evidence_text"),
        )?,
        confidence,
    }))
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
            let avionics_types = avionics_types_from_object(object);
            Some(ListingAvionicsValue::from_parsed(ParsedAvionics {
                manufacturer,
                model,
                avionics_types,
                quantity: optional_i64(object.get("quantity")).unwrap_or(1),
                configuration_action: optional_string(object.get("configuration_action"))
                    .unwrap_or_else(|| "installed".to_string()),
                replaces: parsed_avionics_reference(object.get("replaces")),
                source_evidence_text: optional_string(object.get("source_evidence_text")),
                source_confidence: optional_string(object.get("source_confidence")),
            }))
        })
        .collect()
}

fn parsed_avionics_reference(value: Option<&Value>) -> Option<ParsedAvionicsReference> {
    let object = value?.as_object()?;
    Some(ParsedAvionicsReference {
        manufacturer: optional_string(object.get("manufacturer"))?,
        model: optional_string(object.get("model"))?,
        avionics_types: avionics_types_from_object(object),
    })
}

fn avionics_types_from_object(object: &serde_json::Map<String, Value>) -> Vec<String> {
    string_array(object.get("types"))
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn valuation_facts_from_value(value: &Value) -> StoreResult<Vec<ListingValuationFact>> {
    let Some(items) = value.as_array() else {
        return Err(ListingStoreError::Validation(
            "valuation_facts must be an array".to_string(),
        ));
    };
    items
        .iter()
        .map(|item| {
            let object = item.as_object().ok_or_else(|| {
                ListingStoreError::Validation("each valuation fact must be an object".to_string())
            })?;
            Ok(ListingValuationFact {
                kind: required_string_from_value(
                    object.get("kind").unwrap_or(&Value::Null),
                    "valuation_facts.kind",
                )?,
                value: required_string_from_value(
                    object.get("value").unwrap_or(&Value::Null),
                    "valuation_facts.value",
                )?,
                evidence_text: required_string_from_value(
                    object.get("evidence_text").unwrap_or(&Value::Null),
                    "valuation_facts.evidence_text",
                )?,
                source_url: optional_string(object.get("source_url")),
                confidence: required_string_from_value(
                    object.get("confidence").unwrap_or(&Value::Null),
                    "valuation_facts.confidence",
                )?,
            })
        })
        .collect()
}

fn component_time_basis_from_value(value: &Value, field_name: &str) -> StoreResult<String> {
    let value = optional_string(Some(value)).unwrap_or_else(|| "unknown".to_string());
    if matches!(
        value.as_str(),
        "SNEW" | "SMOH" | "SFOH" | "SPOH" | "unknown"
    ) {
        Ok(value)
    } else {
        Err(ListingStoreError::Validation(format!(
            "{field_name} must be SNEW, SMOH, SFOH, SPOH, or unknown"
        )))
    }
}

fn optional_confidence_from_value(value: &Value, field_name: &str) -> StoreResult<Option<String>> {
    let value = optional_string(Some(value));
    if value
        .as_deref()
        .is_none_or(|value| matches!(value, "high" | "medium" | "low"))
    {
        Ok(value)
    } else {
        Err(ListingStoreError::Validation(format!(
            "{field_name} must be high, medium, low, or null"
        )))
    }
}

fn validate_listing_values(values: &ListingValues) -> StoreResult<()> {
    if !is_plausible_asking_price_usd(values.asking_price_usd) {
        return Err(ListingStoreError::Validation(
            "asking_price_usd must be between 1000 and 250000000".to_string(),
        ));
    }
    if !values.airframe_hours.is_finite() || !(0.0..=100_000.0).contains(&values.airframe_hours) {
        return Err(ListingStoreError::Validation(
            "airframe_hours must be between 0 and 100000".to_string(),
        ));
    }
    validate_component_time(
        "engine",
        values.engine_hours,
        &values.engine_time_basis,
        values.engine_time_evidence.as_deref(),
        values.engine_time_confidence.as_deref(),
    )?;
    validate_installed_component(
        "engine",
        values.source_url.as_deref(),
        values.installed_engine_model_id,
        values.installed_engine.as_ref(),
        values.installed_engine_evidence_text.as_deref(),
        values.installed_engine_confidence.as_deref(),
    )?;
    validate_installed_component(
        "propeller",
        values.source_url.as_deref(),
        values.installed_propeller_model_id,
        values.installed_propeller.as_ref(),
        values.installed_propeller_evidence_text.as_deref(),
        values.installed_propeller_confidence.as_deref(),
    )?;
    validate_component_time(
        "propeller",
        values.propeller_hours,
        &values.propeller_time_basis,
        values.propeller_time_evidence.as_deref(),
        values.propeller_time_confidence.as_deref(),
    )?;
    let allowed_fact_kinds = [
        "restoration",
        "damage_history",
        "log_completeness",
        "paint_condition",
        "interior_condition",
        "engine_conversion",
        "airframe_conversion",
        "major_modification",
    ];
    for fact in &values.valuation_facts {
        if !allowed_fact_kinds.contains(&fact.kind.as_str())
            || fact.value.trim().is_empty()
            || fact.evidence_text.trim().is_empty()
            || (fact.source_url.is_none() && values.source_url.is_none())
            || !matches!(fact.confidence.as_str(), "high" | "medium" | "low")
        {
            return Err(ListingStoreError::Validation(format!(
                "invalid source-backed valuation fact: {}",
                fact.kind
            )));
        }
    }
    for item in &values.avionics {
        if canonical_avionics_types(&item.avionics_types).is_empty() {
            return Err(ListingStoreError::Validation(format!(
                "avionics capability types are required for {} {}",
                item.manufacturer, item.model
            )));
        }
        if !matches!(
            item.configuration_action.as_str(),
            "installed" | "replaces" | "removes"
        ) {
            return Err(ListingStoreError::Validation(format!(
                "invalid avionics configuration action: {}",
                item.configuration_action
            )));
        }
        if matches!(item.configuration_action.as_str(), "replaces" | "removes")
            && item.replaces.is_none()
            && item.replaces_avionics_model_id.is_none()
        {
            return Err(ListingStoreError::Validation(format!(
                "avionics action {} requires a concrete replaces target",
                item.configuration_action
            )));
        }
        if item.configuration_action == "installed"
            && (item.replaces.is_some() || item.replaces_avionics_model_id.is_some())
        {
            return Err(ListingStoreError::Validation(
                "installed avionics cannot declare a replacement target".to_string(),
            ));
        }
        if item
            .replaces
            .as_ref()
            .is_some_and(|replaced| canonical_avionics_types(&replaced.avionics_types).is_empty())
        {
            return Err(ListingStoreError::Validation(
                "replacement avionics capability types are required".to_string(),
            ));
        }
        if item.source_notes.is_none() != item.source_confidence.is_none()
            || item
                .source_confidence
                .as_deref()
                .is_some_and(|value| !matches!(value, "high" | "medium" | "low"))
        {
            return Err(ListingStoreError::Validation(
                "avionics evidence and confidence must be supplied together".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_installed_component(
    component_name: &str,
    listing_source_url: Option<&str>,
    model_id: Option<i64>,
    component: Option<&ParsedInstalledComponent>,
    evidence: Option<&str>,
    confidence: Option<&str>,
) -> StoreResult<()> {
    let present = model_id.is_some() || component.is_some();
    if present
        && (listing_source_url.is_none()
            || evidence.is_none_or(str::is_empty)
            || !confidence.is_some_and(|value| matches!(value, "high" | "medium" | "low")))
    {
        return Err(ListingStoreError::Validation(format!(
            "installed {component_name} requires source URL, evidence, and confidence"
        )));
    }
    if !present && (evidence.is_some() || confidence.is_some()) {
        return Err(ListingStoreError::Validation(format!(
            "installed {component_name} evidence cannot exist without a component model"
        )));
    }
    Ok(())
}

fn validate_component_time(
    component: &str,
    hours: Option<f64>,
    basis: &str,
    evidence: Option<&str>,
    confidence: Option<&str>,
) -> StoreResult<()> {
    if hours.is_some_and(|hours| !hours.is_finite() || !(0.0..=100_000.0).contains(&hours)) {
        return Err(ListingStoreError::Validation(format!(
            "{component}_hours must be null or between 0 and 100000"
        )));
    }
    if !matches!(basis, "SNEW" | "SMOH" | "SFOH" | "SPOH" | "unknown") {
        return Err(ListingStoreError::Validation(format!(
            "{component}_time_basis is invalid"
        )));
    }
    if hours.is_none() && basis != "unknown" {
        return Err(ListingStoreError::Validation(format!(
            "{component}_time_basis must be unknown when hours are missing"
        )));
    }
    if evidence.is_none() != confidence.is_none() {
        return Err(ListingStoreError::Validation(format!(
            "{component} time evidence and confidence must be provided together"
        )));
    }
    if confidence.is_some_and(|value| !matches!(value, "high" | "medium" | "low")) {
        return Err(ListingStoreError::Validation(format!(
            "{component}_time_confidence is invalid"
        )));
    }
    Ok(())
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

async fn unverified_listing_id_for_missing_identity_source(
    db: &AppDb,
    user_id: i64,
    source_url: &str,
) -> StoreResult<Option<i64>> {
    Ok(
        unverified_listing_for_missing_identity_source(db, user_id, source_url)
            .await?
            .map(|candidate| candidate.id),
    )
}

async fn unverified_listing_for_missing_identity_source(
    db: &AppDb,
    user_id: i64,
    source_url: &str,
) -> StoreResult<Option<MissingIdentitySourceCandidateRow>> {
    Ok(query_as_optional!(
        db,
        MissingIdentitySourceCandidateRow,
        r#"
        SELECT id, serial_number
        FROM aircraft_sale_listings
        WHERE created_by_user_id = ?
          AND is_verified = FALSE
          AND source_url = ?
          AND (registration_number IS NULL OR TRIM(registration_number) = '')
        ORDER BY added_at DESC, id DESC
        LIMIT 1
        "#,
        user_id,
        source_url
    )?)
}

fn serial_evidence_for_identity_repair_admission(
    extracted_serial: Option<&str>,
    retained_serial: Option<&str>,
) -> StoreResult<Option<String>> {
    let extracted_serial = extracted_serial
        .map(str::trim)
        .filter(|serial| !serial.is_empty());
    let retained_serial = retained_serial
        .map(str::trim)
        .filter(|serial| !serial.is_empty());
    match (extracted_serial, retained_serial) {
        (Some(extracted), Some(retained)) => {
            let same_serial = extracted == retained
                || matches!(
                    (normalize_serial_key(extracted), normalize_serial_key(retained)),
                    (Some(extracted_key), Some(retained_key)) if extracted_key == retained_key
                );
            if !same_serial {
                return Err(ListingStoreError::Validation(
                    "cannot repair aircraft identity; extracted serial conflicts with the retained same-source serial"
                        .to_string(),
                ));
            }
            Ok(Some(retained.to_string()))
        }
        (Some(extracted), None) => Ok(Some(extracted.to_string())),
        (None, Some(retained)) => Ok(Some(retained.to_string())),
        (None, None) => Ok(None),
    }
}

/// Persist regulator-primary identity before fallible aircraft/avionics
/// enrichment. This deliberately changes no ingestion or listing metadata: a
/// quarantined legacy row remains quarantined until the complete ingestion
/// workflow succeeds.
///
/// The conditional update is a compare-and-set. It cannot overwrite identity
/// populated by a concurrent worker, and it avoids introducing an obvious
/// duplicate when another unverified row for this user already has the same
/// canonical N-number. If another worker completed the same source repair, the
/// follow-up read returns that exact row so the caller can continue safely.
async fn persist_faa_identity_for_missing_identity_source(
    db: &AppDb,
    user_id: i64,
    source_url: &str,
    candidate: &MissingIdentitySourceCandidateRow,
    grounding: &AircraftGrounding,
) -> StoreResult<Option<i64>> {
    let faa_serial = grounding
        .manufacturer_serial_raw
        .as_deref()
        .map(str::trim)
        .filter(|serial| !serial.is_empty());
    let repaired_id = query_scalar_optional!(
        db,
        i64,
        r#"
        UPDATE aircraft_sale_listings
        SET
          registration_number = ?,
          serial_number = COALESCE(?, serial_number)
        WHERE id = ?
          AND created_by_user_id = ?
          AND is_verified = FALSE
          AND source_url = ?
          AND (
            registration_number IS NULL
            OR TRIM(registration_number) = ''
          )
          AND (
            serial_number = ?
            OR (serial_number IS NULL AND ? IS NULL)
          )
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listings duplicate
            WHERE duplicate.id <> aircraft_sale_listings.id
              AND duplicate.created_by_user_id = ?
              AND duplicate.is_verified = FALSE
              AND UPPER(TRIM(duplicate.registration_number)) = UPPER(?)
          )
        RETURNING id
        "#,
        grounding.n_number.as_str(),
        faa_serial,
        candidate.id,
        user_id,
        source_url,
        candidate.serial_number.as_deref(),
        candidate.serial_number.as_deref(),
        user_id,
        grounding.n_number.as_str()
    )?;
    if repaired_id.is_some() {
        return Ok(repaired_id);
    }

    // A racing worker may have performed the same compare-and-set between our
    // FAA lookup and update. Continue only when the exact source now carries
    // the exact admitted identity and expected post-repair serial; a different
    // identity or changed retained serial is never overwritten.
    let expected_serial = faa_serial
        .map(ToOwned::to_owned)
        .or_else(|| candidate.serial_number.clone());
    let concurrently_repaired_id = query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT id
        FROM aircraft_sale_listings
        WHERE id = ?
          AND created_by_user_id = ?
          AND is_verified = FALSE
          AND source_url = ?
          AND UPPER(TRIM(registration_number)) = UPPER(?)
          AND (
            serial_number = ?
            OR (serial_number IS NULL AND ? IS NULL)
          )
        "#,
        candidate.id,
        user_id,
        source_url,
        grounding.n_number.as_str(),
        expected_serial.as_deref(),
        expected_serial.as_deref()
    )?;
    if concurrently_repaired_id.is_some() {
        return Ok(concurrently_repaired_id);
    }

    let competing_listing_id = query_scalar_optional!(
        db,
        i64,
        r#"
        SELECT id
        FROM aircraft_sale_listings
        WHERE id <> ?
          AND created_by_user_id = ?
          AND is_verified = FALSE
          AND UPPER(TRIM(registration_number)) = UPPER(?)
        ORDER BY added_at DESC, id DESC
        LIMIT 1
        "#,
        candidate.id,
        user_id,
        grounding.n_number.as_str()
    )?;
    if let Some(competing_listing_id) = competing_listing_id {
        return Err(ListingStoreError::State(format!(
            "cannot repair listing {} from source {source_url}; canonical registration {} already belongs to unverified listing {competing_listing_id}",
            candidate.id, grounding.n_number
        )));
    }

    Err(ListingStoreError::State(format!(
        "cannot repair listing {} from source {source_url}; retained identity changed during FAA admission",
        candidate.id
    )))
}

fn apply_faa_grounding_identity(values: &mut ListingValues, grounding: &AircraftGrounding) {
    values.registration_number = Some(grounding.n_number.clone());
    if let Some(faa_serial) = grounding
        .manufacturer_serial_raw
        .as_deref()
        .map(str::trim)
        .filter(|serial| !serial.is_empty())
    {
        values.serial_number = Some(faa_serial.to_string());
    }
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
          AND l.ingestion_state = 'ready'
        ORDER BY l.added_at DESC, l.id DESC
        "#,
        registration_number
    )?;
    for row in rows {
        let listing = listing_from_row(db, row).await?;
        if listing_matches_values(db, &listing, values).await? {
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

async fn listing_matches_values(
    db: &AppDb,
    listing: &SaleListing,
    values: &ListingValues,
) -> StoreResult<bool> {
    for (left, right) in [
        (&listing.aircraft.manufacturer, &values.manufacturer),
        (&listing.aircraft.model, &values.model),
        (&listing.aircraft.variant, &values.variant),
    ] {
        if normalize_name(left) != normalize_name(right) {
            return Ok(false);
        }
    }

    let scalar_fields_match = values_match_i64(listing.model_year, values.model_year)
        && values_match_f64(listing.asking_price_usd, values.asking_price_usd)
        && values_match_text(Some(&listing.currency), Some(&values.currency))
        && values_match_f64(listing.airframe_hours, values.airframe_hours)
        && values_match_optional_f64(listing.engine_hours, values.engine_hours)
        && values_match_text(
            Some(&listing.engine_time_basis),
            Some(&values.engine_time_basis),
        )
        && values_match_text(
            listing.engine_time_evidence.as_deref(),
            values.engine_time_evidence.as_deref(),
        )
        && values_match_text(
            listing.engine_time_confidence.as_deref(),
            values.engine_time_confidence.as_deref(),
        )
        && values_match_optional_f64(listing.propeller_hours, values.propeller_hours)
        && values_match_text(
            Some(&listing.propeller_time_basis),
            Some(&values.propeller_time_basis),
        )
        && values_match_text(
            listing.propeller_time_evidence.as_deref(),
            values.propeller_time_evidence.as_deref(),
        )
        && values_match_text(
            listing.propeller_time_confidence.as_deref(),
            values.propeller_time_confidence.as_deref(),
        )
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
        && canonical_valuation_facts(&listing.valuation_facts)
            == canonical_valuation_facts(&values.valuation_facts);
    if !scalar_fields_match {
        return Ok(false);
    }

    Ok(
        canonical_listing_engine(db, listing).await? == canonical_values_engine(db, values).await?
            && canonical_listing_propeller(db, listing).await?
                == canonical_values_propeller(db, values).await?,
    )
}

type CanonicalAvionics = (
    String,
    String,
    Vec<String>,
    i64,
    String,
    Option<(String, String, Vec<String>)>,
    String,
    String,
);

fn canonical_parsed_avionics(value: &[ParsedAvionics]) -> Vec<CanonicalAvionics> {
    let mut canonical = value
        .iter()
        .map(|item| {
            (
                normalize_name(&item.manufacturer),
                normalize_avionics_model_name(&item.model),
                canonical_avionics_types(&item.avionics_types),
                item.quantity.max(1),
                item.configuration_action.clone(),
                item.replaces.as_ref().map(|replaced| {
                    (
                        normalize_name(&replaced.manufacturer),
                        normalize_avionics_model_name(&replaced.model),
                        canonical_avionics_types(&replaced.avionics_types),
                    )
                }),
                normalize_name(item.source_evidence_text.as_deref().unwrap_or("")),
                normalize_name(item.source_confidence.as_deref().unwrap_or("")),
            )
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    canonical.sort();
    canonical
}

fn canonical_avionics(value: &[ListingAvionicsValue]) -> Vec<CanonicalAvionics> {
    let mut canonical = value
        .iter()
        .map(|item| {
            (
                normalize_name(&item.manufacturer),
                normalize_avionics_model_name(&item.model),
                canonical_avionics_types(&item.avionics_types),
                item.quantity.max(1),
                item.configuration_action.clone(),
                item.replaces.as_ref().map(|replaced| {
                    (
                        normalize_name(&replaced.manufacturer),
                        normalize_avionics_model_name(&replaced.model),
                        canonical_avionics_types(&replaced.avionics_types),
                    )
                }),
                normalize_name(item.source_notes.as_deref().unwrap_or("")),
                normalize_name(item.source_confidence.as_deref().unwrap_or("")),
            )
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    canonical.sort();
    canonical
}

fn canonical_avionics_types(avionics_types: &[String]) -> Vec<String> {
    let mut values = avionics_types
        .iter()
        .map(|value| normalize_name(value))
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    values.sort();
    values
}

type CanonicalInstalledComponent = (String, String, String, String);

fn canonical_installed_component(
    identity: Option<InstalledComponentIdentityRow>,
    evidence_text: Option<&str>,
    source_confidence: Option<&str>,
) -> Option<CanonicalInstalledComponent> {
    identity.map(|identity| {
        (
            normalize_name(&identity.manufacturer),
            normalize_name(&identity.model),
            normalize_name(evidence_text.unwrap_or("")),
            normalize_name(source_confidence.unwrap_or("")),
        )
    })
}

async fn engine_identity(
    db: &AppDb,
    model_id: Option<i64>,
) -> StoreResult<Option<InstalledComponentIdentityRow>> {
    let Some(model_id) = model_id else {
        return Ok(None);
    };
    Ok(query_as_optional!(
        db,
        InstalledComponentIdentityRow,
        r#"
        SELECT manufacturer.name AS manufacturer, model.name AS model
        FROM engine_models model
        JOIN engine_manufacturers manufacturer
          ON manufacturer.id = model.engine_manufacturer_id
        WHERE model.id = ?
        "#,
        model_id
    )?)
}

async fn propeller_identity(
    db: &AppDb,
    model_id: Option<i64>,
) -> StoreResult<Option<InstalledComponentIdentityRow>> {
    let Some(model_id) = model_id else {
        return Ok(None);
    };
    Ok(query_as_optional!(
        db,
        InstalledComponentIdentityRow,
        r#"
        SELECT manufacturer.name AS manufacturer, model.name AS model
        FROM propeller_models model
        JOIN propeller_manufacturers manufacturer
          ON manufacturer.id = model.propeller_manufacturer_id
        WHERE model.id = ?
        "#,
        model_id
    )?)
}

async fn canonical_listing_engine(
    db: &AppDb,
    listing: &SaleListing,
) -> StoreResult<Option<CanonicalInstalledComponent>> {
    Ok(canonical_installed_component(
        engine_identity(db, listing.installed_engine_model_id).await?,
        listing.installed_engine_evidence_text.as_deref(),
        listing.installed_engine_confidence.as_deref(),
    ))
}

async fn canonical_values_engine(
    db: &AppDb,
    values: &ListingValues,
) -> StoreResult<Option<CanonicalInstalledComponent>> {
    let identity = match &values.installed_engine {
        Some(component) => Some(InstalledComponentIdentityRow {
            manufacturer: component.manufacturer.clone(),
            model: component.model.clone(),
        }),
        None => engine_identity(db, values.installed_engine_model_id).await?,
    };
    Ok(canonical_installed_component(
        identity,
        values.installed_engine_evidence_text.as_deref(),
        values.installed_engine_confidence.as_deref(),
    ))
}

async fn canonical_listing_propeller(
    db: &AppDb,
    listing: &SaleListing,
) -> StoreResult<Option<CanonicalInstalledComponent>> {
    Ok(canonical_installed_component(
        propeller_identity(db, listing.installed_propeller_model_id).await?,
        listing.installed_propeller_evidence_text.as_deref(),
        listing.installed_propeller_confidence.as_deref(),
    ))
}

async fn canonical_values_propeller(
    db: &AppDb,
    values: &ListingValues,
) -> StoreResult<Option<CanonicalInstalledComponent>> {
    let identity = match &values.installed_propeller {
        Some(component) => Some(InstalledComponentIdentityRow {
            manufacturer: component.manufacturer.clone(),
            model: component.model.clone(),
        }),
        None => propeller_identity(db, values.installed_propeller_model_id).await?,
    };
    Ok(canonical_installed_component(
        identity,
        values.installed_propeller_evidence_text.as_deref(),
        values.installed_propeller_confidence.as_deref(),
    ))
}

type CanonicalValuationFact = (String, String, String, String);

fn canonical_valuation_facts(value: &[ListingValuationFact]) -> Vec<CanonicalValuationFact> {
    let mut canonical = value
        .iter()
        .map(|fact| {
            (
                normalize_name(&fact.kind),
                normalize_name(&fact.value),
                normalize_name(&fact.evidence_text),
                normalize_name(&fact.confidence),
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
    enrich_aircraft_spec_for_listing_if_missing(db, extractor, listing_id, listing_text)
        .await
        .map_err(|error| {
            ListingStoreError::State(format!("aircraft specification enrichment failed: {error}"))
        })?;

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

    if let Ok(Some(identity)) = listing_aircraft_identity(db, listing_id).await {
        mark_valuation_snapshot_stale_best_effort(db, identity.aircraft_model_id).await;
    }
    let _ = cleanup_orphan_records(db).await;
    Ok(())
}

async fn finalize_listing_ingestion(
    db: &AppDb,
    listing_id: i64,
    extractor: Option<&GeminiListingExtractor>,
    listing_text: Option<&str>,
) -> StoreResult<()> {
    match complete_listing_ingestion(db, listing_id, extractor, listing_text).await {
        Ok(()) => {
            mark_listing_ready(db, listing_id).await?;
            Ok(())
        }
        Err(error) => quarantine_after_error(db, listing_id, error).await,
    }
}

async fn mark_listing_incomplete(db: &AppDb, listing_id: i64) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'incomplete',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        listing_id
    )?;
    Ok(())
}

async fn mark_listing_ready(db: &AppDb, listing_id: i64) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'ready',
            ingestion_error = NULL,
            ingestion_completed_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        listing_id
    )?;
    Ok(())
}

async fn quarantine_after_error<T>(
    db: &AppDb,
    listing_id: i64,
    error: ListingStoreError,
) -> StoreResult<T> {
    let message = error.to_string();
    execute_query!(
        db,
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'quarantined',
            ingestion_error = ?,
            ingestion_completed_at = NULL,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        message.as_str(),
        listing_id
    )?;
    Err(ListingStoreError::Ingestion {
        listing_id,
        message,
    })
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
            model.catalog_status <> 'approved'
            OR model.introduced_year IS NULL
            OR model.estimated_unit_value_usd IS NULL
            OR model.estimated_unit_value_usd < 0
            OR model.value_basis <> 'installed_contribution'
            OR model.replacement_cost_usd IS NULL
            OR model.replacement_cost_usd < model.estimated_unit_value_usd
            OR model.value_reference_year IS NULL
            OR model.value_reference_year < 1900
            OR model.value_reference_year > 2200
            OR model.value_source IS NULL
            OR TRIM(model.value_source) = ''
            OR (
              model.valuation_scope = 'integrated_suite'
              AND NOT EXISTS (
                SELECT 1
                FROM avionics_suite_components membership
                JOIN avionics_models component
                  ON component.id = membership.component_model_id
                WHERE membership.suite_model_id = model.id
                  AND component.catalog_status = 'approved'
              )
            )
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
                AND price_point.purchase_price_reference_year = price_point.model_year
                AND price_point.source_confidence = 'high'
                AND price_point.evidence_kind = 'direct_model_year'
                AND price_point.is_valuation_eligible = TRUE
            )
            OR NOT EXISTS (
              SELECT 1
              FROM aircraft_model_variant_default_avionics default_avionics
              JOIN avionics_models model
                ON model.id = default_avionics.avionics_model_id
              WHERE default_avionics.aircraft_model_variant_id = listing.aircraft_model_variant_id
                AND default_avionics.model_year = listing.model_year
                AND default_avionics.source_confidence = 'high'
                AND default_avionics.quantity > 0
                AND TRIM(default_avionics.source_url) <> ''
                AND LOWER(default_avionics.source_url) NOT LIKE '%/listing/%'
                AND LOWER(default_avionics.source_url) NOT LIKE '%/listings/%'
                AND LOWER(default_avionics.source_url) NOT LIKE '%/aircraft-for-sale/%'
                AND LOWER(default_avionics.source_url) NOT LIKE '%/classifieds/%'
                AND model.catalog_status = 'approved'
                AND model.introduced_year IS NOT NULL
                AND model.estimated_unit_value_usd >= 0
                AND model.value_basis = 'installed_contribution'
                AND model.replacement_cost_usd >= model.estimated_unit_value_usd
                AND model.value_reference_year BETWEEN 1900 AND 2200
                AND model.value_source IS NOT NULL
                AND TRIM(model.value_source) <> ''
                AND (
                  model.valuation_scope <> 'integrated_suite'
                  OR EXISTS (
                    SELECT 1
                    FROM avionics_suite_components membership
                    JOIN avionics_models component
                      ON component.id = membership.component_model_id
                    WHERE membership.suite_model_id = model.id
                      AND component.catalog_status = 'approved'
                  )
                )
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

fn values_match_optional_f64(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => values_match_f64(left, right),
        (None, None) => true,
        _ => false,
    }
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

#[cfg(test)]
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
    let model_id = query_scalar_one!(
        db,
        i64,
        r#"
        INSERT INTO avionics_models (
          avionics_manufacturer_id,
          name,
          normalized_name
        )
        VALUES (?, ?, ?)
        RETURNING id
        "#,
        manufacturer_id,
        model,
        normalized_model.as_str()
    )?;
    execute_query!(
        db,
        "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        model_id,
        type_id,
    )?;
    Ok(model_id)
}

async fn resolve_installed_engine_model_id(
    db: &AppDb,
    values: &ListingValues,
) -> StoreResult<Option<i64>> {
    if values.installed_engine_model_id.is_some() {
        return Ok(values.installed_engine_model_id);
    }
    let Some(component) = &values.installed_engine else {
        return Ok(None);
    };
    let manufacturer_id =
        ensure_named_row(db, "engine_manufacturers", &component.manufacturer).await?;
    let normalized_model = normalize_name(&component.model);
    execute_query!(
        db,
        r#"
        INSERT INTO engine_models (
          engine_manufacturer_id, name, normalized_name,
          source_url, source_title, source_confidence, evidence_kind, is_valuation_eligible
        ) VALUES (?, ?, ?, ?, 'sale listing installed engine evidence', ?, 'listing_only', FALSE)
        ON CONFLICT (engine_manufacturer_id, normalized_name) DO NOTHING
        "#,
        manufacturer_id,
        component.model.as_str(),
        normalized_model.as_str(),
        values.source_url.as_deref(),
        component.confidence.as_str()
    )?;
    Ok(Some(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT id FROM engine_models
        WHERE engine_manufacturer_id = ? AND normalized_name = ?
        "#,
        manufacturer_id,
        normalized_model.as_str()
    )?))
}

async fn resolve_installed_propeller_model_id(
    db: &AppDb,
    values: &ListingValues,
) -> StoreResult<Option<i64>> {
    if values.installed_propeller_model_id.is_some() {
        return Ok(values.installed_propeller_model_id);
    }
    let Some(component) = &values.installed_propeller else {
        return Ok(None);
    };
    let manufacturer_id =
        ensure_named_row(db, "propeller_manufacturers", &component.manufacturer).await?;
    let normalized_model = normalize_name(&component.model);
    execute_query!(
        db,
        r#"
        INSERT INTO propeller_models (
          propeller_manufacturer_id, name, normalized_name,
          source_url, source_title, source_confidence, evidence_kind, is_valuation_eligible
        ) VALUES (?, ?, ?, ?, 'sale listing installed propeller evidence', ?, 'listing_only', FALSE)
        ON CONFLICT (propeller_manufacturer_id, normalized_name) DO NOTHING
        "#,
        manufacturer_id,
        component.model.as_str(),
        normalized_model.as_str(),
        values.source_url.as_deref(),
        component.confidence.as_str()
    )?;
    Ok(Some(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT id FROM propeller_models
        WHERE propeller_manufacturer_id = ? AND normalized_name = ?
        "#,
        manufacturer_id,
        normalized_model.as_str()
    )?))
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

async fn validated_catalog_avionics_model_id(
    db: &AppDb,
    avionics_model_id: i64,
    manufacturer: &str,
    model: &str,
    avionics_types: &[String],
) -> StoreResult<i64> {
    let manufacturer = normalize_avionics_manufacturer_name(manufacturer);
    let model = normalize_avionics_model_name(model);
    let matching_rows = query_scalar_one!(
        db,
        i64,
        r#"
        SELECT COUNT(*)
        FROM avionics_models model
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        WHERE model.id = ?
          AND model.catalog_status = 'approved'
          AND mfr.normalized_name = ?
          AND model.normalized_name = ?
        "#,
        avionics_model_id,
        manufacturer.as_str(),
        model.as_str()
    )?;
    if matching_rows != 1 {
        return Err(ListingStoreError::Validation(format!(
            "avionics catalog id {avionics_model_id} does not match its canonical identity"
        )));
    }
    let stored_types = catalog_avionics_types(db, avionics_model_id).await?;
    if canonical_avionics_types(avionics_types) != canonical_avionics_types(&stored_types) {
        return Err(ListingStoreError::Validation(format!(
            "avionics catalog id {avionics_model_id} capability set does not match its canonical identity"
        )));
    }
    Ok(avionics_model_id)
}

async fn catalog_avionics_types(db: &AppDb, avionics_model_id: i64) -> StoreResult<Vec<String>> {
    Ok(query_as_all!(
        db,
        AvionicsCapabilityRow,
        r#"
        SELECT membership.avionics_model_id, avionics_type.name AS avionics_type
        FROM avionics_model_types membership
        JOIN avionics_types avionics_type
          ON avionics_type.id = membership.avionics_type_id
        WHERE membership.avionics_model_id = ?
        ORDER BY avionics_type.normalized_name
        "#,
        avionics_model_id
    )?
    .into_iter()
    .map(|row| row.avionics_type)
    .collect())
}

async fn replace_listing_avionics(
    db: &AppDb,
    listing_id: i64,
    avionics: &[ListingAvionicsValue],
) -> StoreResult<()> {
    struct PreparedListingAvionics {
        avionics_model_id: i64,
        quantity: i64,
        source: String,
        source_notes: Option<String>,
        source_confidence: Option<String>,
        configuration_action: String,
        replaces_avionics_model_id: Option<i64>,
    }

    // Coalesce by physical catalog product before validation and persistence.
    // This is deliberately repeated at the storage boundary so no caller can
    // accidentally delegate conflict resolution to the database upsert.
    let avionics = coalesce_resolved_listing_avionics(
        avionics
            .iter()
            .filter(|item| is_usable_avionics_label(&item.manufacturer, &item.model))
            .cloned(),
    )?;

    // Validate the entire replacement set before touching existing links.
    // The transaction below then makes trigger/race failures all-or-nothing.
    let mut prepared = Vec::new();
    for item in &avionics {
        let avionics_model_id = validated_catalog_avionics_model_id(
            db,
            item.avionics_model_id.ok_or_else(|| {
                ListingStoreError::Validation(format!(
                    "avionics must resolve to a catalog id before persistence: {} {}",
                    item.manufacturer, item.model
                ))
            })?,
            &item.manufacturer,
            &item.model,
            &item.avionics_types,
        )
        .await?;
        let replaces_avionics_model_id = match item.configuration_action.as_str() {
            "installed" if item.replaces.is_none() && item.replaces_avionics_model_id.is_none() => {
                None
            }
            "replaces" | "removes" => {
                let replaced = item.replaces.as_ref().ok_or_else(|| {
                    ListingStoreError::Validation(
                        "replacement/removal avionics requires a canonical catalog identity"
                            .to_string(),
                    )
                })?;
                Some(
                    validated_catalog_avionics_model_id(
                        db,
                        item.replaces_avionics_model_id.ok_or_else(|| {
                            ListingStoreError::Validation(
                                "replacement/removal avionics must resolve to a catalog id"
                                    .to_string(),
                            )
                        })?,
                        &replaced.manufacturer,
                        &replaced.model,
                        &replaced.avionics_types,
                    )
                    .await?,
                )
            }
            _ => {
                return Err(ListingStoreError::Validation(format!(
                    "invalid catalog-backed avionics action: {}",
                    item.configuration_action
                )))
            }
        };
        prepared.push(PreparedListingAvionics {
            avionics_model_id,
            quantity: item.quantity.max(1),
            source: item.source.clone(),
            source_notes: item.source_notes.clone(),
            source_confidence: item.source_confidence.clone(),
            configuration_action: item.configuration_action.clone(),
            replaces_avionics_model_id,
        });
    }

    let delete_sql =
        db.sql("DELETE FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?");
    let insert_sql = db.sql(
        r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id,
              avionics_model_id,
              quantity,
              source,
              source_notes,
              source_confidence,
              configuration_action,
              replaces_avionics_model_id
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
    );
    macro_rules! replace_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            sqlx::query(&delete_sql)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            for item in &prepared {
                sqlx::query(&insert_sql)
                    .bind(listing_id)
                    .bind(item.avionics_model_id)
                    .bind(item.quantity)
                    .bind(item.source.as_str())
                    .bind(item.source_notes.as_deref())
                    .bind(item.source_confidence.as_deref())
                    .bind(item.configuration_action.as_str())
                    .bind(item.replaces_avionics_model_id)
                    .execute(&mut *transaction)
                    .await?;
            }
            transaction.commit().await?;
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => replace_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => replace_in_transaction!(pool),
    }
    Ok(())
}

async fn replace_listing_facts(
    db: &AppDb,
    listing_id: i64,
    values: &ListingValues,
) -> StoreResult<()> {
    execute_query!(
        db,
        "DELETE FROM aircraft_sale_listing_facts WHERE aircraft_sale_listing_id = ?",
        listing_id
    )?;
    for fact in &values.valuation_facts {
        execute_query!(
            db,
            r#"
            INSERT INTO aircraft_sale_listing_facts (
              aircraft_sale_listing_id,
              fact_kind,
              fact_value,
              evidence_text,
              source_url,
              source_confidence
            ) VALUES (?, ?, ?, ?, ?, ?)
            "#,
            listing_id,
            fact.kind.as_str(),
            fact.value.as_str(),
            fact.evidence_text.as_str(),
            fact.source_url.as_deref().or(values.source_url.as_deref()),
            fact.confidence.as_str()
        )?;
    }
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
        engine_time_basis: row.engine_time_basis,
        engine_time_evidence: row.engine_time_evidence,
        engine_time_confidence: row.engine_time_confidence,
        propeller_hours: row.propeller_hours,
        propeller_time_basis: row.propeller_time_basis,
        propeller_time_evidence: row.propeller_time_evidence,
        propeller_time_confidence: row.propeller_time_confidence,
        installed_engine_model_id: row.installed_engine_model_id,
        installed_engine_source_url: row.installed_engine_source_url,
        installed_engine_evidence_text: row.installed_engine_evidence_text,
        installed_engine_confidence: row.installed_engine_confidence,
        installed_propeller_model_id: row.installed_propeller_model_id,
        installed_propeller_source_url: row.installed_propeller_source_url,
        installed_propeller_evidence_text: row.installed_propeller_evidence_text,
        installed_propeller_confidence: row.installed_propeller_confidence,
        ingestion_state: row.ingestion_state,
        ingestion_error: row.ingestion_error,
        ingestion_completed_at: row.ingestion_completed_at,
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
        valuation_facts: listing_facts(db, listing_id).await?,
    })
}

async fn listing_avionics(db: &AppDb, listing_id: i64) -> StoreResult<Vec<ParsedAvionics>> {
    let capabilities = listing_avionics_capabilities(db, listing_id).await?;
    let rows = query_as_all!(
        db,
        ParsedAvionicsRow,
        r#"
        SELECT
          model.id AS avionics_model_id,
          mfr.name AS manufacturer,
          model.name AS model,
          link.quantity,
          link.configuration_action,
          link.source_notes,
          link.source_confidence,
          replaces_model.id AS replaces_avionics_model_id,
          replaces_mfr.name AS replaces_manufacturer,
          replaces_model.name AS replaces_model
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_models model
          ON model.id = link.avionics_model_id
        JOIN avionics_manufacturers mfr
          ON mfr.id = model.avionics_manufacturer_id
        LEFT JOIN avionics_models replaces_model
          ON replaces_model.id = link.replaces_avionics_model_id
        LEFT JOIN avionics_manufacturers replaces_mfr
          ON replaces_mfr.id = replaces_model.avionics_manufacturer_id
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
            avionics_types: capabilities
                .get(&row.avionics_model_id)
                .cloned()
                .unwrap_or_default(),
            quantity: row.quantity,
            configuration_action: row.configuration_action,
            replaces: match (
                row.replaces_avionics_model_id,
                row.replaces_manufacturer,
                row.replaces_model,
            ) {
                (Some(avionics_model_id), Some(manufacturer), Some(model)) => {
                    Some(ParsedAvionicsReference {
                        manufacturer,
                        model,
                        avionics_types: capabilities
                            .get(&avionics_model_id)
                            .cloned()
                            .unwrap_or_default(),
                    })
                }
                _ => None,
            },
            source_evidence_text: row.source_notes,
            source_confidence: row.source_confidence,
        })
        .collect())
}

async fn listing_avionics_capabilities(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<HashMap<i64, Vec<String>>> {
    let rows = query_as_all!(
        db,
        AvionicsCapabilityRow,
        r#"
        SELECT
          membership.avionics_model_id,
          avionics_type.name AS avionics_type
        FROM avionics_model_types membership
        JOIN avionics_types avionics_type
          ON avionics_type.id = membership.avionics_type_id
        WHERE EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_avionics link
          WHERE link.aircraft_sale_listing_id = ?
            AND (
              link.avionics_model_id = membership.avionics_model_id
              OR link.replaces_avionics_model_id = membership.avionics_model_id
            )
        )
        ORDER BY
          membership.avionics_model_id,
          avionics_type.normalized_name
        "#,
        listing_id
    )?;
    let mut capabilities = HashMap::new();
    for row in rows {
        capabilities
            .entry(row.avionics_model_id)
            .or_insert_with(Vec::new)
            .push(row.avionics_type);
    }
    Ok(capabilities)
}

async fn listing_facts(db: &AppDb, listing_id: i64) -> StoreResult<Vec<ListingValuationFact>> {
    let rows = query_as_all!(
        db,
        ListingFactRow,
        r#"
        SELECT fact_kind, fact_value, evidence_text, source_url, source_confidence
        FROM aircraft_sale_listing_facts
        WHERE aircraft_sale_listing_id = ?
        ORDER BY fact_kind, id
        "#,
        listing_id
    )?;
    Ok(rows
        .into_iter()
        .map(|row| ListingValuationFact {
            kind: row.fact_kind,
            value: row.fact_value,
            evidence_text: row.evidence_text,
            source_url: row.source_url,
            confidence: row.source_confidence,
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
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use crate::aircraft::faa::{parse_release, store_release, ReleaseMetadata, ReleaseReaders};
    use crate::avionics::catalog::ApprovedAvionicsIdentity;
    use crate::db::{AppDb, DatabaseBackend};
    use crate::extract::preview_manual_listing;
    use crate::models::ParsedAvionics;

    use super::{
        coalesce_resolved_listing_avionics, listing_avionics_value_from_catalog, model_similarity,
        replace_listing_avionics, resolve_listing_avionics_values, variant_label_issues,
        variant_normalization_groups_from_response, ListingAvionicsValue, ListingValues,
        ModelVariantRow, MODEL_SIMILARITY_CONFIRMATION_THRESHOLD,
    };

    const FAA_AIRCRAFT_REFERENCE: &str = "CODE,MFR,MODEL,TYPE-ACFT,TYPE-ENG,AC-CAT,BUILD-CERT-IND,NO-ENG,NO-SEATS,AC-WEIGHT,SPEED,TC-DATA-SHEET,TC-DATA-HOLDER\n2072738,CESSNA AIRCRAFT CO,182T,4,1,1,0,01,004,CLASS 1,0145,3A13,TEXTRON AVIATION INC\n";
    const FAA_ENGINE_REFERENCE: &str =
        "CODE,MFR,MODEL,TYPE,HORSEPOWER,THRUST\n41528,LYCOMING,IO-540-AB1A5,1,00230,000000\n";

    async fn seed_faa_aircraft(db: &AppDb, n_number: &str, serial: &str) {
        let suffix = n_number
            .strip_prefix('N')
            .expect("test FAA N-number must include N prefix");
        let master = format!(
            "N-NUMBER,SERIAL NUMBER,MFR MDL CODE,ENG MFR MDL,YEAR MFR\n{suffix},{serial},2072738,41528,2023\n"
        );
        let digest_seed = n_number.bytes().fold(0_u64, |state, byte| {
            state.wrapping_mul(31).wrapping_add(u64::from(byte))
        });
        let release = parse_release(
            ReleaseMetadata::official("2026-07-20", format!("{digest_seed:064x}")),
            ReleaseReaders::new(
                Cursor::new(master),
                Cursor::new(FAA_AIRCRAFT_REFERENCE),
                Cursor::new(FAA_ENGINE_REFERENCE),
            ),
            [n_number],
        )
        .expect("test FAA release should parse");
        store_release(db, &release)
            .await
            .expect("test FAA release should store");
    }

    async fn seed_blank_identity_listing(db: &AppDb, user_id: i64, source_url: &str) -> i64 {
        let variant_id = super::ensure_aircraft_model_variant(db, "Cessna", "182", "182T")
            .await
            .expect("variant should seed");
        query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, is_verified,
              source_url, model_year, asking_price_usd, currency, status,
              ingestion_state, ingestion_error, registration_number, serial_number,
              airframe_hours
            )
            VALUES (?, ?, FALSE, ?, 2023, 525000, 'USD', 'active',
                    'quarantined', 'legacy identity is missing', NULL, NULL, 400)
            RETURNING id
            "#,
            variant_id,
            user_id,
            source_url
        )
        .expect("legacy listing should seed")
    }

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
    fn installed_component_requires_a_listing_source_url() {
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.source_url = None;
        values.installed_engine = Some(crate::models::ParsedInstalledComponent {
            manufacturer: "Continental".to_string(),
            model: "IO-550-D".to_string(),
            evidence_text: "Continental IO-550-D installed".to_string(),
            confidence: "high".to_string(),
        });
        values.installed_engine_evidence_text = Some("Continental IO-550-D installed".to_string());
        values.installed_engine_confidence = Some("high".to_string());

        let error = super::validate_listing_values(&values)
            .expect_err("unsourced installed component must be rejected");
        assert!(error.to_string().contains("requires source URL"));
    }

    #[tokio::test]
    async fn unavailable_classifier_cannot_assign_even_exact_looking_avionics() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        super::ensure_avionics_model(&db, "Garmin", "GTX 345R", "Transponder")
            .await
            .expect("known catalog model should seed");
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![
            ListingAvionicsValue::from_parsed(parsed_avionics("GTX 345R")),
            ListingAvionicsValue::from_parsed(parsed_avionics("Imaginary 999")),
        ];

        let error = resolve_listing_avionics_values(
            &db,
            &mut values,
            None,
            Some("https://example.com/listing"),
            None,
        )
        .await
        .expect_err("unknown equipment requires a classifier or curation");
        assert!(error.to_string().contains("Imaginary 999"));
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM avionics_models WHERE normalized_name = 'imaginary999'"
            )
            .expect("unknown model count should load"),
            0
        );

        let error = resolve_listing_avionics_values(
            &db,
            &mut values,
            None,
            Some("https://example.com/listing"),
            None,
        )
        .await
        .expect_err("string equality is retrieval help, not identity proof");
        assert!(error
            .to_string()
            .contains("Gemini identity resolver unavailable"));
    }

    #[tokio::test]
    async fn persistence_rejects_free_form_replacement_target() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let model_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GTX 345R", "Transponder")
                .await
                .expect("known catalog model should seed");
        let mut candidate = approved_avionics_identity();
        candidate.id = model_id;
        let mut item = listing_avionics_value_from_catalog(
            &ListingAvionicsValue::from_parsed(parsed_avionics("GTX 345R")),
            &candidate,
        );
        item.configuration_action = "replaces".to_string();
        item.replaces = Some(crate::models::ParsedAvionicsReference {
            manufacturer: "Unknown Maker".to_string(),
            model: "Imaginary 999".to_string(),
            avionics_types: vec!["Transponder".to_string()],
        });
        item.replaces_avionics_model_id = None;

        let error = replace_listing_avionics(&db, 999999, &[item])
            .await
            .expect_err("raw replacement identity must not be created");
        assert!(error.to_string().contains("must resolve to a catalog id"));
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM avionics_models WHERE normalized_name = 'imaginary999'"
            )
            .expect("unknown model count should load"),
            0
        );
    }

    #[tokio::test]
    async fn listing_read_exposes_all_types_once_for_one_physical_product() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        sqlx::query(
            "INSERT INTO aircraft_manufacturers (name, normalized_name) VALUES ('Cessna', 'cessna')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_models (aircraft_manufacturer_id, name, normalized_name) VALUES (1, '182', '182')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_model_variants (aircraft_model_id, name, normalized_name) VALUES (1, '182T', '182t')",
        )
        .execute(pool)
        .await
        .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listings (aircraft_model_variant_id, created_by_user_id, model_year, asking_price_usd, airframe_hours) VALUES (1, 1, 2020, 300000, 1000) RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let avionics_model_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GNX 375", "GPS")
                .await
                .unwrap();
        let transponder_type_id = super::ensure_named_row(&db, "avionics_types", "Transponder")
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
        )
        .bind(avionics_model_id)
        .bind(transponder_type_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO aircraft_sale_listing_avionics (aircraft_sale_listing_id, avionics_model_id, source_confidence) VALUES (?, ?, 'high')",
        )
        .bind(listing_id)
        .bind(avionics_model_id)
        .execute(pool)
        .await
        .unwrap();

        let avionics = super::listing_avionics(&db, listing_id).await.unwrap();

        assert_eq!(avionics.len(), 1, "capabilities must not duplicate a unit");
        assert_eq!(avionics[0].model, "GNX 375");
        assert_eq!(avionics[0].avionics_types, vec!["GPS", "Transponder"]);
    }

    #[test]
    fn gnx_duplicate_capability_mentions_coalesce_without_creating_extra_units() {
        let gps = resolved_avionics_value(
            375,
            &["GPS"],
            "installed",
            None,
            Some("GNX 375 GPS navigator installed"),
            Some("high"),
            1,
        );
        let transponder = resolved_avionics_value(
            375,
            &["Transponder"],
            "installed",
            None,
            Some("GNX 375 Mode S transponder installed"),
            Some("medium"),
            2,
        );

        let coalesced = coalesce_resolved_listing_avionics([gps, transponder])
            .expect("identical installation semantics should coalesce");

        assert_eq!(coalesced.len(), 1);
        assert_eq!(coalesced[0].quantity, 2, "quantity uses max, not sum");
        assert_eq!(coalesced[0].avionics_types, vec!["GPS", "Transponder"]);
        assert_eq!(
            coalesced[0].source_notes.as_deref(),
            Some("GNX 375 GPS navigator installed\nGNX 375 Mode S transponder installed")
        );
        assert_eq!(
            coalesced[0].source_confidence.as_deref(),
            Some("medium"),
            "the weaker duplicate evidence must govern"
        );
    }

    #[test]
    fn duplicate_catalog_product_with_conflicting_action_is_rejected() {
        let installed = resolved_avionics_value(
            375,
            &["GPS"],
            "installed",
            None,
            Some("GNX 375 installed"),
            Some("high"),
            1,
        );
        let replaces = resolved_avionics_value(
            375,
            &["Transponder"],
            "replaces",
            Some(327),
            Some("GNX 375 replaces GTX 327"),
            Some("high"),
            1,
        );

        let error = coalesce_resolved_listing_avionics([installed, replaces])
            .expect_err("conflicting configuration actions must fail closed");

        assert!(error
            .to_string()
            .contains("conflicting installation actions"));
    }

    #[test]
    fn duplicate_catalog_product_with_different_replacement_targets_is_rejected() {
        let replaces_gtx_327 = resolved_avionics_value(
            375,
            &["GPS"],
            "replaces",
            Some(327),
            Some("GNX 375 replaces GTX 327"),
            Some("high"),
            1,
        );
        let replaces_gtx_330 = resolved_avionics_value(
            375,
            &["Transponder"],
            "replaces",
            Some(330),
            Some("GNX 375 replaces GTX 330"),
            Some("high"),
            1,
        );

        let error = coalesce_resolved_listing_avionics([replaces_gtx_327, replaces_gtx_330])
            .expect_err("different replacement targets must fail closed");

        assert!(error.to_string().contains("replacement targets"));
    }

    #[test]
    fn duplicate_catalog_product_cannot_spoof_a_shared_replacement_id() {
        let replaces_gtx_327 = resolved_avionics_value(
            375,
            &["GPS"],
            "replaces",
            Some(327),
            Some("GNX 375 replaces GTX 327"),
            Some("high"),
            1,
        );
        let mut conflicting_reference = resolved_avionics_value(
            375,
            &["Transponder"],
            "replaces",
            Some(327),
            Some("GNX 375 replaces a different unit"),
            Some("high"),
            1,
        );
        conflicting_reference
            .replaces
            .as_mut()
            .expect("replacement reference should exist")
            .model = "GTX 330".to_string();

        let error = coalesce_resolved_listing_avionics([replaces_gtx_327, conflicting_reference])
            .expect_err("a shared numeric id must not hide conflicting target evidence");

        assert!(error.to_string().contains("replacement targets"));
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

    fn approved_avionics_identity() -> ApprovedAvionicsIdentity {
        ApprovedAvionicsIdentity {
            id: 42,
            manufacturer: "Garmin".to_string(),
            model: "GTX 345R".to_string(),
            avionics_types: vec!["Transponder".to_string()],
            manufacturer_identifier_kind: "manufacturer_part_number".to_string(),
            manufacturer_identifier: "011-03520-00".to_string(),
            evidence_url: "https://static.garmin.com/manuals/gtx345r.pdf".to_string(),
            evidence_title: "GTX 345R installation manual".to_string(),
            evidence: "The manual identifies the model and part number.".to_string(),
            reason: "Authoritative manufacturer manual.".to_string(),
        }
    }

    async fn ensure_approved_test_avionics_model(
        db: &AppDb,
        manufacturer: &str,
        model: &str,
        avionics_type: &str,
    ) -> super::StoreResult<i64> {
        let id = super::ensure_avionics_model(db, manufacturer, model, avionics_type).await?;
        let identifier = format!("TEST-{id}");
        let normalized_identifier = format!("test{id}");
        execute_query!(
            db,
            r#"
            UPDATE avionics_models
            SET catalog_status = 'approved',
                manufacturer_identifier_kind = 'manufacturer_part_number',
                manufacturer_identifier = ?,
                normalized_manufacturer_identifier = ?,
                identity_source_url = 'https://manufacturer.example/manuals/test.pdf',
                identity_source_title = 'Manufacturer test manual',
                identity_evidence_text = 'The manufacturer manual identifies this test product and part number.',
                identity_evidence_kind = 'authoritative_reference',
                identity_confidence = 'very_high',
                catalog_reviewed_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            identifier.as_str(),
            normalized_identifier.as_str(),
            id
        )?;
        Ok(id)
    }

    fn parsed_avionics(model: &str) -> ParsedAvionics {
        ParsedAvionics {
            manufacturer: "Garmin".to_string(),
            model: model.to_string(),
            avionics_types: vec!["Transponder".to_string()],
            quantity: 1,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some(format!("{model} installed")),
            source_confidence: Some("high".to_string()),
        }
    }

    fn resolved_avionics_value(
        avionics_model_id: i64,
        avionics_types: &[&str],
        configuration_action: &str,
        replaces_avionics_model_id: Option<i64>,
        source_notes: Option<&str>,
        source_confidence: Option<&str>,
        quantity: i64,
    ) -> ListingAvionicsValue {
        ListingAvionicsValue {
            avionics_model_id: Some(avionics_model_id),
            manufacturer: "Garmin".to_string(),
            model: "GNX 375".to_string(),
            avionics_types: avionics_types
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            quantity,
            source: "listing".to_string(),
            source_notes: source_notes.map(ToString::to_string),
            source_confidence: source_confidence.map(ToString::to_string),
            configuration_action: configuration_action.to_string(),
            replaces: replaces_avionics_model_id.map(|target| {
                crate::models::ParsedAvionicsReference {
                    manufacturer: "Garmin".to_string(),
                    model: format!("GTX {target}"),
                    avionics_types: vec!["Transponder".to_string()],
                }
            }),
            replaces_avionics_model_id,
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
            engine_hours: Some(357.0),
            engine_time_basis: "SNEW".to_string(),
            engine_time_evidence: Some("357 hours since new".to_string()),
            engine_time_confidence: Some("high".to_string()),
            propeller_hours: Some(357.0),
            propeller_time_basis: "SNEW".to_string(),
            propeller_time_evidence: Some("357 hours since new".to_string()),
            propeller_time_confidence: Some("high".to_string()),
            installed_engine_model_id: None,
            installed_engine: None,
            installed_engine_evidence_text: None,
            installed_engine_confidence: None,
            installed_propeller_model_id: None,
            installed_propeller: None,
            installed_propeller_evidence_text: None,
            installed_propeller_confidence: None,
            avionics: Vec::new(),
            valuation_facts: Vec::new(),
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
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let variant_id =
            super::ensure_aircraft_model_variant(&db, "Cessna", "182 Skylane", "182T Skylane")
                .await
                .expect("variant should seed");
        let avionics_model_id = ensure_approved_test_avionics_model(
            &db,
            "Garmin",
            "G1000 NXi",
            "Integrated Flight Deck",
        )
        .await
        .expect("avionics model should seed");
        execute_query!(
            &db,
            r#"
            UPDATE avionics_models
            SET introduced_year = 2017,
                estimated_unit_value_usd = 50000,
                value_basis = 'installed_contribution',
                replacement_cost_usd = 65000,
                value_reference_year = 2026,
                value_source = 'gemini',
                valuation_scope = 'unit'
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
              source_confidence,
              evidence_kind,
              is_valuation_eligible
            )
            VALUES (
              ?, 2023, 699000, 2023, 'https://example.test', 'test', 'test fixture',
              'high', 'direct_model_year', TRUE
            )
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
        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182 Skylane",
            "variant": "182T Skylane",
            "model_year": 2023,
            "asking_price_usd": 699000,
            "currency": "USD",
            "airframe_hours": 357,
            "status": "active",
            "registration_number": "N123T",
            "serial_number": "TESTSERIAL",
            "installed_engine": {
                "manufacturer": "Lycoming",
                "model": "IO-540-AB1A5",
                "evidence_text": "Lycoming IO-540-AB1A5 installed",
                "confidence": "high"
            },
            "valuation_facts": [{
                "kind": "engine_conversion",
                "value": "Air Plains 300 HP conversion",
                "evidence_text": "Air Plains 300 HP engine conversion",
                "confidence": "high"
            }],
            "avionics": []
        }));
        preview.source_url = Some("https://example.test/listing".to_string());

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
        assert_eq!(listing.registration_number.as_deref(), Some("N123T"));
        assert_eq!(listing.engine_hours, None);
        assert_eq!(listing.propeller_hours, None);
        assert!(listing.installed_engine_model_id.is_some());
        assert_eq!(listing.valuation_facts.len(), 1);
        assert_eq!(listing.ingestion_state, "ready");
        assert!(listing.ingestion_error.is_none());
        assert!(listing.ingestion_completed_at.is_some());

        let values = super::values_from_listing(&listing);
        assert!(super::listing_matches_values(&db, &listing, &values)
            .await
            .expect("same evidence-backed listing should match"));

        let mut changed_fact = values.clone();
        changed_fact.valuation_facts[0].value = "Air Plains 310 HP conversion".to_string();
        assert!(!super::listing_matches_values(&db, &listing, &changed_fact)
            .await
            .expect("fact comparison should run"));

        let mut changed_engine = values;
        changed_engine.installed_engine_model_id = None;
        changed_engine.installed_engine = Some(crate::models::ParsedInstalledComponent {
            manufacturer: "Continental".to_string(),
            model: "IO-550-D".to_string(),
            evidence_text: "Continental IO-550-D installed".to_string(),
            confidence: "high".to_string(),
        });
        changed_engine.installed_engine_evidence_text =
            Some("Continental IO-550-D installed".to_string());
        assert!(
            !super::listing_matches_values(&db, &listing, &changed_engine)
                .await
                .expect("installed engine comparison should run")
        );

        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn non_n_listing_is_rejected_before_model_work_or_existing_row_update() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let variant_id = super::ensure_aircraft_model_variant(&db, "Cessna", "182", "182T")
            .await
            .expect("existing aircraft identity should seed");
        let existing_listing_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, currency, status, registration_number,
              serial_number, airframe_hours
            ) VALUES (?, ?, 'https://example.test/foreign', 2022, 250000, 'USD', 'active',
                      'C-GABC', 'FOREIGN-1', 500)
            RETURNING id
            "#,
            variant_id,
            user.id
        )
        .expect("existing foreign listing should seed");
        let catalog_counts_before = query_as_optional!(
            &db,
            (i64, i64, i64),
            r#"
            SELECT
              (SELECT count(*) FROM aircraft_manufacturers),
              (SELECT count(*) FROM aircraft_models),
              (SELECT count(*) FROM aircraft_model_variants)
            "#
        )
        .expect("catalog counts should load")
        .expect("count query returns one row");

        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2022,
            "asking_price_usd": 999000,
            "currency": "USD",
            "airframe_hours": 500,
            "status": "active",
            "registration_number": "C-GABC",
            "serial_number": "FOREIGN-1",
            "avionics": [{
                "manufacturer": "Imaginary",
                "model": "Model 9000",
                "types": ["GPS"],
                "source_evidence_text": "Imaginary Model 9000 installed",
                "source_confidence": "high"
            }]
        }));
        preview.source_url = Some("https://example.test/foreign".to_string());

        let error = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect_err("non-N registration must fail before model-assisted work");
        assert!(matches!(error, super::ListingStoreError::Validation(_)));
        assert!(error.to_string().contains("non_n_registration"));
        assert_eq!(
            query_scalar_one!(
                &db,
                f64,
                "SELECT asking_price_usd FROM aircraft_sale_listings WHERE id = ?",
                existing_listing_id
            )
            .expect("existing price should load"),
            250000.0
        );
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT count(*) FROM aircraft_sale_listings")
                .expect("listing count should load"),
            1
        );
        let legacy_error = super::require_model_listings_faa_admitted(&db, "Cessna", "182")
            .await
            .expect_err("legacy variant curation must preflight every source listing");
        assert!(legacy_error.to_string().contains("non_n_registration"));
        let update_error = super::update_listing(
            &db,
            user.id,
            existing_listing_id,
            &json!({"asking_price_usd": 888000}),
            None,
        )
        .await
        .expect_err("an existing non-N listing must not reach update normalization");
        assert!(update_error.to_string().contains("non_n_registration"));
        assert_eq!(
            query_scalar_one!(
                &db,
                f64,
                "SELECT asking_price_usd FROM aircraft_sale_listings WHERE id = ?",
                existing_listing_id
            )
            .expect("existing price should remain unchanged"),
            250000.0
        );
        assert_eq!(
            query_as_optional!(
                &db,
                (i64, i64, i64),
                r#"
                SELECT
                  (SELECT count(*) FROM aircraft_manufacturers),
                  (SELECT count(*) FROM aircraft_models),
                  (SELECT count(*) FROM aircraft_model_variants)
                "#
            )
            .expect("catalog counts should reload")
            .expect("count query returns one row"),
            catalog_counts_before
        );
    }

    #[tokio::test]
    async fn uncovered_n_number_is_rejected_before_model_work_or_insert() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N111AA", "SERIAL-111").await;
        let catalog_counts_before = query_as_optional!(
            &db,
            (i64, i64, i64),
            r#"
            SELECT
              (SELECT count(*) FROM aircraft_manufacturers),
              (SELECT count(*) FROM aircraft_models),
              (SELECT count(*) FROM aircraft_model_variants)
            "#
        )
        .expect("catalog counts should load")
        .expect("count query returns one row");
        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Test Aircraft",
            "model": "Model 2",
            "variant": "Variant B",
            "model_year": 2021,
            "asking_price_usd": 150000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "N222BB",
            "serial_number": "SERIAL-222",
            "avionics": []
        }));
        preview.source_url = Some("https://example.test/uncovered".to_string());

        let error = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect_err("an N-number outside current projection must fail closed");
        assert!(error.to_string().contains("registration_not_covered"));
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT count(*) FROM aircraft_sale_listings")
                .expect("listing count should load"),
            0
        );
        assert_eq!(
            query_as_optional!(
                &db,
                (i64, i64, i64),
                r#"
                SELECT
                  (SELECT count(*) FROM aircraft_manufacturers),
                  (SELECT count(*) FROM aircraft_models),
                  (SELECT count(*) FROM aircraft_model_variants)
                "#
            )
            .expect("catalog counts should reload")
            .expect("count query returns one row"),
            catalog_counts_before
        );
    }

    #[tokio::test]
    async fn source_reprocessing_repairs_blank_identity_with_canonical_faa_values() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let variant_id = super::ensure_aircraft_model_variant(&db, "Cessna", "182", "182T")
            .await
            .expect("variant should seed");
        let source_url = "https://example.test/listing/identity-recovery";
        let legacy_listing_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, is_verified,
              source_url, model_year, asking_price_usd, currency, status,
              ingestion_state, ingestion_error, registration_number, serial_number,
              airframe_hours
            )
            VALUES (?, ?, FALSE, ?, 2023, 525000, 'USD', 'active',
                    'quarantined', 'legacy identity is missing', NULL, NULL, 400)
            RETURNING id
            "#,
            variant_id,
            user.id,
            source_url
        )
        .expect("legacy listing should seed");

        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2023,
            "asking_price_usd": 525000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "n-123t",
            "serial_number": null,
            "avionics": []
        }));
        preview.source_url = Some(source_url.to_string());

        let _ = super::create_listing(&db, user.id, &preview, None, None).await;

        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT count(*) FROM aircraft_sale_listings")
                .expect("listing count should load"),
            1
        );
        let identity = query_as_optional!(
            &db,
            (String, String),
            "SELECT registration_number, serial_number FROM aircraft_sale_listings WHERE id = ?",
            legacy_listing_id
        )
        .expect("identity should load")
        .expect("legacy listing should remain");
        assert_eq!(identity.0, "N123T");
        assert_eq!(identity.1, "TESTSERIAL");
    }

    #[tokio::test]
    async fn source_identity_repair_survives_downstream_avionics_failure() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let source_url = "https://example.test/listing/identity-before-enrichment";
        let listing_id = seed_blank_identity_listing(&db, user.id, source_url).await;
        let before = super::get_listing(&db, user.id, listing_id)
            .await
            .expect("legacy listing should load");

        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2023,
            "asking_price_usd": 525000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "n-123t",
            "serial_number": null,
            "avionics": []
        }));
        preview.source_url = Some(source_url.to_string());
        preview.parsed_listing.avionics = vec![parsed_avionics("Imaginary 999")];

        let error = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect_err("unavailable avionics resolver should fail downstream ingestion");
        assert!(error
            .to_string()
            .contains("Gemini identity resolver unavailable"));

        let after = super::get_listing(&db, user.id, listing_id)
            .await
            .expect("repaired listing should remain");
        let mut expected = before;
        expected.registration_number = Some("N123T".to_string());
        expected.serial_number = Some("TESTSERIAL".to_string());
        assert_eq!(after, expected);
        assert_eq!(after.ingestion_state, "quarantined");
        assert_eq!(
            after.ingestion_error.as_deref(),
            Some("legacy identity is missing")
        );
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT count(*) FROM aircraft_sale_listings")
                .expect("listing count should load"),
            1
        );
    }

    #[tokio::test]
    async fn conflicting_retained_serial_blocks_source_identity_repair() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let source_url = "https://example.test/listing/conflicting-retained-serial";
        let listing_id = seed_blank_identity_listing(&db, user.id, source_url).await;
        execute_query!(
            &db,
            "UPDATE aircraft_sale_listings SET serial_number = 'CONFLICTING-SERIAL' WHERE id = ?",
            listing_id
        )
        .expect("conflicting retained serial should seed");
        let before = super::get_listing(&db, user.id, listing_id)
            .await
            .expect("legacy listing should load");

        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2023,
            "asking_price_usd": 525000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "N123T",
            "serial_number": null,
            "avionics": []
        }));
        preview.source_url = Some(source_url.to_string());

        let error = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect_err("retained serial must participate in FAA admission");
        assert!(error.to_string().contains("serial_conflict"));
        assert_eq!(
            super::get_listing(&db, user.id, listing_id)
                .await
                .expect("conflicting legacy listing should remain"),
            before
        );
    }

    #[tokio::test]
    async fn changed_retained_serial_fails_identity_compare_and_set_closed() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let source_url = "https://example.test/listing/racing-retained-serial";
        let listing_id = seed_blank_identity_listing(&db, user.id, source_url).await;
        let candidate =
            super::unverified_listing_for_missing_identity_source(&db, user.id, source_url)
                .await
                .expect("candidate lookup should succeed")
                .expect("blank source candidate should exist");
        let grounding = crate::aircraft::faa::require_aircraft_admission(
            &db,
            Some("N123T"),
            candidate.serial_number.as_deref(),
        )
        .await
        .expect("blank retained serial should pass FAA admission");

        execute_query!(
            &db,
            "UPDATE aircraft_sale_listings SET serial_number = 'CHANGED-DURING-ADMISSION' WHERE id = ?",
            listing_id
        )
        .expect("concurrent serial change should be simulated");
        let before_repair = super::get_listing(&db, user.id, listing_id)
            .await
            .expect("changed listing should load");

        let error = super::persist_faa_identity_for_missing_identity_source(
            &db, user.id, source_url, &candidate, &grounding,
        )
        .await
        .expect_err("stale retained evidence must fail closed");
        assert!(error
            .to_string()
            .contains("retained identity changed during FAA admission"));
        assert_eq!(
            super::get_listing(&db, user.id, listing_id)
                .await
                .expect("stale candidate listing should remain"),
            before_repair
        );
    }

    #[tokio::test]
    async fn failed_faa_admission_does_not_mutate_same_source_blank_listing() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let source_url = "https://example.test/listing/rejected-identity";
        let listing_id = seed_blank_identity_listing(&db, user.id, source_url).await;
        let before = super::get_listing(&db, user.id, listing_id)
            .await
            .expect("legacy listing should load");

        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2023,
            "asking_price_usd": 525000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "N999ZZ",
            "serial_number": null,
            "avionics": []
        }));
        preview.source_url = Some(source_url.to_string());

        let error = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect_err("an uncovered N-number must fail FAA admission");
        assert!(error.to_string().contains("registration_not_covered"));
        assert_eq!(
            super::get_listing(&db, user.id, listing_id)
                .await
                .expect("legacy listing should remain"),
            before
        );
    }

    #[tokio::test]
    async fn different_source_does_not_receive_admitted_identity() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let listing_id = seed_blank_identity_listing(
            &db,
            user.id,
            "https://example.test/listing/original-source",
        )
        .await;
        let before = super::get_listing(&db, user.id, listing_id)
            .await
            .expect("legacy listing should load");

        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2023,
            "asking_price_usd": 525000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "N123T",
            "serial_number": null,
            "avionics": []
        }));
        preview.source_url = Some("https://example.test/listing/different-source".to_string());
        preview.parsed_listing.avionics = vec![parsed_avionics("Imaginary 999")];

        let error = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect_err("downstream failure should prevent a new listing insert");
        assert!(error
            .to_string()
            .contains("Gemini identity resolver unavailable"));
        assert_eq!(
            super::get_listing(&db, user.id, listing_id)
                .await
                .expect("unrelated source listing should remain"),
            before
        );
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT count(*) FROM aircraft_sale_listings")
                .expect("listing count should load"),
            1
        );
    }

    #[tokio::test]
    async fn source_identity_repair_does_not_duplicate_an_existing_unverified_tail() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let variant_id = super::ensure_aircraft_model_variant(&db, "Cessna", "182", "182T")
            .await
            .expect("variant should seed");
        let existing_tail_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, is_verified,
              source_url, model_year, asking_price_usd, currency, status,
              ingestion_state, ingestion_error, registration_number, serial_number,
              airframe_hours
            )
            VALUES (?, ?, FALSE, 'https://example.test/listing/existing-tail',
                    2023, 525000, 'USD', 'active', 'quarantined',
                    'awaiting enrichment', 'N123T', 'TESTSERIAL', 400)
            RETURNING id
            "#,
            variant_id,
            user.id
        )
        .expect("existing tail listing should seed");
        let blank_source = "https://example.test/listing/blank-duplicate";
        let blank_id = seed_blank_identity_listing(&db, user.id, blank_source).await;
        let existing_before = super::get_listing(&db, user.id, existing_tail_id)
            .await
            .expect("existing tail listing should load");
        let blank_before = super::get_listing(&db, user.id, blank_id)
            .await
            .expect("blank listing should load");

        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2023,
            "asking_price_usd": 525000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "N123T",
            "serial_number": null,
            "avionics": []
        }));
        preview.source_url = Some(blank_source.to_string());

        let error = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect_err("competing retained tail must block source identity repair");
        assert!(error
            .to_string()
            .contains("already belongs to unverified listing"));
        assert!(error.to_string().contains(&existing_tail_id.to_string()));
        assert_eq!(
            super::get_listing(&db, user.id, existing_tail_id)
                .await
                .expect("existing tail listing should remain"),
            existing_before
        );
        assert_eq!(
            super::get_listing(&db, user.id, blank_id)
                .await
                .expect("blank source listing should remain"),
            blank_before
        );
    }

    #[tokio::test]
    async fn readiness_requires_valuation_grade_avionics_price_and_suite_membership() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let variant_id = super::ensure_aircraft_model_variant(&db, "Test", "Readiness", "Suite")
            .await
            .expect("variant should seed");
        let suite_id = ensure_approved_test_avionics_model(
            &db,
            "Garmin",
            "Test Integrated Suite",
            "Integrated Flight Deck",
        )
        .await
        .expect("suite should seed");
        let component_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "Test Display", "Display")
                .await
                .expect("component should seed");
        let listing_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, currency, status, airframe_hours
            ) VALUES (?, ?, 'https://example.test/readiness', 2024, 500000, 'USD', 'active', 100)
            RETURNING id
            "#,
            variant_id,
            user.id
        )
        .expect("listing should seed");
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, source, source_notes,
              source_confidence
            ) VALUES (?, ?, 'listing', 'explicitly installed suite', 'high')
            "#,
            listing_id,
            suite_id
        )
        .expect("listing avionics should seed");
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_model_variant_price_points (
              aircraft_model_variant_id, model_year, purchase_price_new_usd,
              purchase_price_reference_year, source_url, source_title,
              source_notes, source_confidence
            ) VALUES (?, 2024, 500000, 2024, 'https://example.test', 'test', 'legacy', 'high')
            "#,
            variant_id
        )
        .expect("legacy price should seed");
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_model_variant_default_avionics (
              aircraft_model_variant_id, model_year, avionics_model_id, quantity,
              source_url, source_title, source_notes, source_confidence
            ) VALUES (?, 2024, ?, 1, 'https://example.test', 'test', 'default suite', 'high')
            "#,
            variant_id,
            suite_id
        )
        .expect("default avionics should seed");

        assert_eq!(
            super::listing_missing_avionics_metadata_count(&db, listing_id)
                .await
                .expect("missing metadata should count"),
            1
        );
        assert!(
            super::listing_needs_model_year_price_or_default_avionics(&db, listing_id)
                .await
                .expect("legacy records should be incomplete")
        );

        execute_query!(
            &db,
            r#"
            UPDATE avionics_models
            SET introduced_year = 2020,
                estimated_unit_value_usd = 40000,
                value_basis = 'installed_contribution',
                replacement_cost_usd = 55000,
                value_reference_year = 2026,
                value_source = 'gemini',
                valuation_scope = 'integrated_suite'
            WHERE id = ?
            "#,
            suite_id
        )
        .expect("rich suite metadata should seed");
        execute_query!(
            &db,
            r#"
            UPDATE aircraft_model_variant_price_points
            SET evidence_kind = 'direct_model_year', is_valuation_eligible = TRUE
            WHERE aircraft_model_variant_id = ? AND model_year = 2024
            "#,
            variant_id
        )
        .expect("price should become eligible");

        assert_eq!(
            super::listing_missing_avionics_metadata_count(&db, listing_id)
                .await
                .expect("suite membership should still be required"),
            1
        );
        assert!(
            super::listing_needs_model_year_price_or_default_avionics(&db, listing_id)
                .await
                .expect("default suite should still be incomplete")
        );

        execute_query!(
            &db,
            "INSERT INTO avionics_suite_components (suite_model_id, component_model_id, quantity) VALUES (?, ?, 1)",
            suite_id,
            component_id
        )
        .expect("suite membership should seed");
        assert_eq!(
            super::listing_missing_avionics_metadata_count(&db, listing_id)
                .await
                .expect("rich suite should be complete"),
            0
        );
        assert!(
            !super::listing_needs_model_year_price_or_default_avionics(&db, listing_id)
                .await
                .expect("valuation-grade records should be complete")
        );
    }

    #[tokio::test]
    async fn failed_completion_quarantines_staged_listing() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123QT", "QTEST").await;
        let preview = preview_manual_listing(&json!({
            "manufacturer": "Test Aircraft",
            "model": "Model 1",
            "variant": "Variant A",
            "model_year": 2020,
            "asking_price_usd": 125000,
            "currency": "USD",
            "airframe_hours": 500,
            "status": "active",
            "registration_number": "N123QT",
            "serial_number": "QTEST",
            "avionics": []
        }));

        let error = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect_err("missing required enrichment should quarantine the row");
        let super::ListingStoreError::Ingestion { listing_id, .. } = error else {
            panic!("expected an ingestion error")
        };
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects sqlite")
        };
        let state: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT ingestion_state, ingestion_error, ingestion_completed_at FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .expect("quarantined listing should remain queryable");
        assert_eq!(state.0, "quarantined");
        assert!(state.1.is_some());
        assert!(state.2.is_none());
    }
}
