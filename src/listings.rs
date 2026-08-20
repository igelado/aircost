use std::collections::{HashMap, HashSet};
use std::fmt;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::aircraft::faa::{
    admit_aircraft_source_identity, normalize_serial_key, require_aircraft_admission,
    require_listing_admission, require_listing_faa_admission, AircraftAdmissionError,
    AircraftGrounding, FaaSerialCorrection,
};
use crate::aircraft::identity::{
    ensure_listing_identity_assignment_from_approved_catalog, EnsureIdentityAssignmentOutcome,
};
use crate::aircraft::{enrich_aircraft_spec_for_listing_if_missing, AircraftStoreError};
use crate::avionics::catalog::{
    resolve_avionics_identity, resolve_verified_local_avionics_identity, ApprovedAvionicsIdentity,
    AvionicsIdentityOutcome, AvionicsIdentityRequest, CatalogError,
};
use crate::avionics::reuse::{
    reuse_attestation_is_current_postgres, reuse_attestation_is_current_sqlite,
};
use crate::avionics::{
    enrich_listing_avionics_metadata, enrich_model_year_avionics_and_price_point_for_listing,
    AvionicsStoreError,
};
use crate::cleanup::{cleanup_orphan_records, CleanupError};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{optional_f64, optional_i64, optional_string, GeminiListingExtractor};
use crate::listing::avionics::disposition::{AutomaticOccurrenceDisposition, OccurrenceRole};
use crate::listing::avionics::{
    approved_avionics_product_key, validate_canonical_avionics_actions, CanonicalAvionicsAction,
};
use crate::listing::review::{
    clear_pending_review, replace_pending_review, PendingReviewAspect, ReviewAction,
    ReviewAspectId, ReviewProduct, StableIdentifier, POSTGRES_LISTING_CHILD_LOCK_SQL,
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
    AircraftAdmission(AircraftAdmissionError),
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
            ListingStoreError::AircraftAdmission(error) => write!(formatter, "{error}"),
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListingFinalizationOutcome {
    Ready,
    PendingReference { reason: String },
}

#[derive(Debug)]
pub(crate) struct ListingCreationResult {
    pub(crate) listing: SaleListing,
    pub(crate) occurrence_dispositions: Vec<AutomaticOccurrenceDisposition>,
    pub(crate) source_serial_correction: Option<FaaSerialCorrection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListingCreationMode {
    Ordinary,
    SignedSource,
    CreateOnly,
}

#[derive(Clone, Debug)]
pub(crate) struct SignedSourceListingBinding {
    pub submission_id: i64,
    pub user_id: i64,
    pub plugin_install_id: i64,
    pub install_public_key_base64: String,
    pub install_revoked_at: Option<String>,
    pub source_url: String,
    pub submitted_at: String,
    pub rendered_html: String,
    pub rendered_html_sha256: String,
    pub signature_base64: String,
    pub expected_extracted_listing_json: Option<String>,
    pub expected_extracted_listing_sha256: Option<String>,
    pub expected_extraction_error: Option<String>,
    pub bound_extracted_listing_json: String,
    pub bound_extracted_listing_sha256: String,
}

impl ListingCreationMode {
    fn may_reuse_listing(self) -> bool {
        self != Self::CreateOnly
    }

    fn permits_source_serial_correction(self) -> bool {
        matches!(self, Self::SignedSource | Self::CreateOnly)
    }
}

#[derive(Debug)]
struct ResolvedListingAvionics {
    pending_review_aspects: Vec<PendingReviewAspect>,
    occurrence_dispositions: Vec<AutomaticOccurrenceDisposition>,
}

const AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON: &str =
    "Automated product verification could not complete safely. Confirm or discard this observation manually.";
const AVIONICS_INSTALLATION_EVIDENCE_NOT_HIGH_REASON: &str =
    "The product identity was verified automatically, but the listing does not provide high-confidence evidence that this unit is installed.";
pub(crate) const SOURCE_IDENTITY_RECEIPT_PENDING: &str =
    "source_identity_correction_receipt_pending";

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
struct ListingAvionicsGraphRow {
    subject_manufacturer_identity_id: i64,
    subject_product_key: String,
    quantity: i64,
    configuration_action: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    replaces_avionics_model_id: Option<i64>,
    replacement_manufacturer_identity_id: Option<i64>,
    replacement_product_key: Option<String>,
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
    Ok(create_listing_with_progress_and_occurrence_dispositions(
        db,
        user_id,
        preview,
        original_listing,
        extractor,
        progress,
        ListingCreationMode::Ordinary,
        None,
    )
    .await?
    .listing)
}

pub(crate) async fn create_listing_with_progress_and_occurrence_dispositions(
    db: &AppDb,
    user_id: i64,
    preview: &ListingPreview,
    original_listing: Option<&Value>,
    extractor: Option<&GeminiListingExtractor>,
    progress: Option<&ListingProgressSender>,
    creation_mode: ListingCreationMode,
    signed_source_binding: Option<&SignedSourceListingBinding>,
) -> StoreResult<ListingCreationResult> {
    emit_listing_progress(
        progress,
        "verifying_listing",
        "Verifying extracted listing fields",
    );
    let mut values = values_from_preview(preview, original_listing)?;
    let literal_identity_values = values.clone();
    let mut missing_identity_source_candidate = match (creation_mode, values.source_url.as_deref())
    {
        (ListingCreationMode::CreateOnly, _) => None,
        (_, Some(source_url)) => {
            unverified_listing_for_missing_identity_source(db, user_id, source_url).await?
        }
        (_, None) => None,
    };
    let retained_serial_evidence =
        if creation_mode == ListingCreationMode::SignedSource && values.serial_number.is_some() {
            None
        } else {
            missing_identity_source_candidate
                .as_ref()
                .and_then(|candidate| candidate.serial_number.as_deref())
        };
    let admission_serial = serial_evidence_for_identity_repair_admission(
        values.serial_number.as_deref(),
        retained_serial_evidence,
    )?;
    let (grounding, source_serial_correction) = if creation_mode.permits_source_serial_correction()
    {
        let admission = admit_aircraft_source_identity(
            db,
            values.registration_number.as_deref(),
            admission_serial.as_deref(),
            preview.context_text.as_deref(),
        )
        .await
        .map_err(listing_admission_error)?;
        (admission.grounding, admission.serial_correction)
    } else {
        (
            require_aircraft_admission(
                db,
                values.registration_number.as_deref(),
                admission_serial.as_deref(),
            )
            .await
            .map_err(listing_admission_error)?,
            None,
        )
    };
    let may_reuse_listing = creation_mode.may_reuse_listing() && source_serial_correction.is_none();
    if !may_reuse_listing {
        // A source serial correction is materialized as an isolated observation.
        // No existing row may change before the signed capture and immutable
        // correction receipt have both been persisted.
        missing_identity_source_candidate = None;
    }
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
        "resolving_aircraft",
        "Resolving FAA-backed canonical aircraft identity",
    );
    emit_listing_progress(
        progress,
        "normalizing_avionics",
        "Normalizing avionics units",
    );
    let resolved_avionics = resolve_listing_avionics_values(
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
    if may_reuse_listing {
        if let Some(listing_id) = identity_repair_listing_id {
            emit_listing_progress(progress, "saving_listing", "Repairing existing listing");
            update_listing_values(
                db,
                listing_id,
                &values,
                &literal_identity_values,
                true,
                true,
                false,
            )
            .await?;
            replace_listing_pending_review(
                db,
                listing_id,
                &resolved_avionics.pending_review_aspects,
                false,
            )
            .await?;
            emit_listing_progress(
                progress,
                "refreshing_estimates",
                "Refreshing valuation inputs",
            );
            finalize_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref())
                .await?;
            return Ok(ListingCreationResult {
                listing: get_listing(db, user_id, listing_id).await?,
                occurrence_dispositions: resolved_avionics.occurrence_dispositions,
                source_serial_correction,
            });
        }
    }

    if may_reuse_listing {
        if let Some(registration_number) = &values.registration_number {
            if let Some(listing_id) =
                unverified_listing_id_for_tail(db, user_id, registration_number).await?
            {
                emit_listing_progress(progress, "saving_listing", "Updating existing listing");
                update_listing_values(
                    db,
                    listing_id,
                    &values,
                    &literal_identity_values,
                    true,
                    true,
                    false,
                )
                .await?;
                replace_listing_pending_review(
                    db,
                    listing_id,
                    &resolved_avionics.pending_review_aspects,
                    false,
                )
                .await?;
                emit_listing_progress(
                    progress,
                    "refreshing_estimates",
                    "Refreshing valuation inputs",
                );
                finalize_listing_ingestion(
                    db,
                    listing_id,
                    extractor,
                    preview.context_text.as_deref(),
                )
                .await?;
                return Ok(ListingCreationResult {
                    listing: get_listing(db, user_id, listing_id).await?,
                    occurrence_dispositions: resolved_avionics.occurrence_dispositions,
                    source_serial_correction,
                });
            }
        }
    }

    if may_reuse_listing {
        if let Some(source_url) = values.source_url.as_deref() {
            if let Some(listing_id) =
                unverified_listing_id_for_missing_identity_source(db, user_id, source_url).await?
            {
                emit_listing_progress(progress, "saving_listing", "Repairing existing listing");
                update_listing_values(
                    db,
                    listing_id,
                    &values,
                    &literal_identity_values,
                    true,
                    true,
                    false,
                )
                .await?;
                replace_listing_pending_review(
                    db,
                    listing_id,
                    &resolved_avionics.pending_review_aspects,
                    false,
                )
                .await?;
                emit_listing_progress(
                    progress,
                    "refreshing_estimates",
                    "Refreshing valuation inputs",
                );
                finalize_listing_ingestion(
                    db,
                    listing_id,
                    extractor,
                    preview.context_text.as_deref(),
                )
                .await?;
                return Ok(ListingCreationResult {
                    listing: get_listing(db, user_id, listing_id).await?,
                    occurrence_dispositions: resolved_avionics.occurrence_dispositions,
                    source_serial_correction,
                });
            }
        }
    }

    if may_reuse_listing && resolved_avionics.pending_review_aspects.is_empty() {
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
            return Ok(ListingCreationResult {
                listing: get_listing(db, user_id, listing_id).await?,
                occurrence_dispositions: resolved_avionics.occurrence_dispositions,
                source_serial_correction,
            });
        }
    }

    emit_listing_progress(progress, "saving_listing", "Saving listing");
    let listing_id = insert_listing(
        db,
        user_id,
        &values,
        &literal_identity_values,
        source_serial_correction.is_some(),
        signed_source_binding,
    )
    .await?;
    replace_listing_pending_review(
        db,
        listing_id,
        &resolved_avionics.pending_review_aspects,
        source_serial_correction.is_some(),
    )
    .await?;
    if source_serial_correction.is_some() {
        return Ok(ListingCreationResult {
            listing: get_listing(db, user_id, listing_id).await?,
            occurrence_dispositions: resolved_avionics.occurrence_dispositions,
            source_serial_correction,
        });
    }
    emit_listing_progress(
        progress,
        "refreshing_estimates",
        "Refreshing valuation inputs",
    );
    finalize_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref()).await?;
    Ok(ListingCreationResult {
        listing: get_listing(db, user_id, listing_id).await?,
        occurrence_dispositions: resolved_avionics.occurrence_dispositions,
        source_serial_correction,
    })
}

/// Resume only the durable post-bind state of one corrected signed capture.
/// The listing must still be in the database-enforced receipt gate and must
/// already contain the exact current FAA correction represented by the raw
/// retained extraction. All child projections are deterministic replacements,
/// so a process restart can safely replay them before inserting the receipt.
pub(crate) async fn resume_signed_source_correction_listing(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
    preview: &ListingPreview,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<ListingCreationResult> {
    let mut values = values_from_preview(preview, None)?;
    let literal_identity_values = values.clone();
    let admission = admit_aircraft_source_identity(
        db,
        values.registration_number.as_deref(),
        values.serial_number.as_deref(),
        preview.context_text.as_deref(),
    )
    .await
    .map_err(listing_admission_error)?;
    let source_serial_correction = admission.serial_correction.ok_or_else(|| {
        ListingStoreError::State(
            "receipt-gated signed listing no longer has its exact FAA serial correction"
                .to_string(),
        )
    })?;
    apply_faa_grounding_identity(&mut values, &admission.grounding);
    let current = get_listing(db, user_id, listing_id).await?;
    if current.created_by_user_id != user_id
        || current.source_url != values.source_url
        || current.ingestion_state != "quarantined"
        || current.ingestion_error.as_deref() != Some(SOURCE_IDENTITY_RECEIPT_PENDING)
        || current.registration_number != values.registration_number
        || current.serial_number != values.serial_number
    {
        return Err(ListingStoreError::State(format!(
            "receipt-gated listing {listing_id} changed before deterministic recovery"
        )));
    }
    let resolved_avionics = resolve_listing_avionics_values(
        db,
        &mut values,
        extractor,
        preview.source_url.as_deref(),
        preview.context_text.as_deref(),
    )
    .await?;
    update_listing_values(
        db,
        listing_id,
        &values,
        &literal_identity_values,
        false,
        true,
        true,
    )
    .await?;
    replace_listing_pending_review(
        db,
        listing_id,
        &resolved_avionics.pending_review_aspects,
        true,
    )
    .await?;
    Ok(ListingCreationResult {
        listing: get_listing(db, user_id, listing_id).await?,
        occurrence_dispositions: resolved_avionics.occurrence_dispositions,
        source_serial_correction: Some(source_serial_correction),
    })
}

/// Deterministically rebuild every mutable child projection for an already
/// atomically bound replay listing. The capture binding is only an ownership
/// anchor; callers must not treat it as proof that materialization completed.
pub(crate) async fn resume_bound_replay_listing(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
    preview: &ListingPreview,
    extractor: Option<&GeminiListingExtractor>,
) -> StoreResult<ListingCreationResult> {
    let current = get_listing(db, user_id, listing_id).await?;
    if current.ingestion_error.as_deref() == Some(SOURCE_IDENTITY_RECEIPT_PENDING) {
        return resume_signed_source_correction_listing(
            db, user_id, listing_id, preview, extractor,
        )
        .await;
    }

    let mut values = values_from_preview(preview, None)?;
    let literal_identity_values = values.clone();
    let admission = admit_aircraft_source_identity(
        db,
        values.registration_number.as_deref(),
        values.serial_number.as_deref(),
        preview.context_text.as_deref(),
    )
    .await
    .map_err(listing_admission_error)?;
    if admission.serial_correction.is_some() {
        return Err(ListingStoreError::State(
            "a replay listing requiring an FAA serial correction is missing its receipt gate"
                .to_string(),
        ));
    }
    apply_faa_grounding_identity(&mut values, &admission.grounding);
    if current.created_by_user_id != user_id || current.source_url != values.source_url {
        return Err(ListingStoreError::State(format!(
            "bound replay listing {listing_id} changed before deterministic recovery"
        )));
    }
    let resolved_avionics = resolve_listing_avionics_values(
        db,
        &mut values,
        extractor,
        preview.source_url.as_deref(),
        preview.context_text.as_deref(),
    )
    .await?;
    update_listing_values(
        db,
        listing_id,
        &values,
        &literal_identity_values,
        false,
        true,
        false,
    )
    .await?;
    replace_listing_pending_review(
        db,
        listing_id,
        &resolved_avionics.pending_review_aspects,
        false,
    )
    .await?;
    finalize_listing_ingestion(db, listing_id, extractor, preview.context_text.as_deref()).await?;
    Ok(ListingCreationResult {
        listing: get_listing(db, user_id, listing_id).await?,
        occurrence_dispositions: resolved_avionics.occurrence_dispositions,
        source_serial_correction: None,
    })
}

pub(crate) async fn finalize_signed_source_listing_after_receipt(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
    submission_id: i64,
    extractor: Option<&GeminiListingExtractor>,
    listing_text: Option<&str>,
) -> StoreResult<SaleListing> {
    let receipt_count = query_scalar_one!(
        db,
        i64,
        r#"
        SELECT COUNT(*)
        FROM aircraft_listing_identity_correction_decisions decision
        JOIN plugin_submissions submission
          ON submission.id = decision.plugin_submission_id
        JOIN aircraft_sale_listings listing
          ON listing.id = decision.aircraft_sale_listing_id
        WHERE decision.aircraft_sale_listing_id = ?
          AND decision.plugin_submission_id = ?
          AND decision.correction_kind = 'faa_serial'
          AND decision.rendered_html_sha256 = submission.rendered_html_sha256
          AND submission.user_id = ?
          AND submission.canonical_listing_id = decision.aircraft_sale_listing_id
          AND submission.extraction_error IS NULL
          AND listing.created_by_user_id = submission.user_id
          AND listing.registration_number = decision.corrected_registration_number
          AND listing.serial_number = decision.corrected_serial_number
        "#,
        listing_id,
        submission_id,
        user_id
    )?;
    if receipt_count != 1 {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} has no exact bound source identity correction receipt"
        )));
    }
    finalize_listing_ingestion(db, listing_id, extractor, listing_text).await?;
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

fn listing_admission_error(error: AircraftAdmissionError) -> ListingStoreError {
    ListingStoreError::AircraftAdmission(error)
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

    // Avionics are an explicit PATCH boundary. Re-resolving canonical links
    // during an unrelated edit would perform surprising paid work and could
    // erase a concurrently staged review or change link IDs referenced by its
    // exact coverage. Context changes likewise require a complete avionics
    // replacement so review evidence can be restaged against the new context.
    let explicitly_replaces_avionics = listing
        .as_object()
        .is_some_and(|fields| fields.contains_key("avionics"));
    const AVIONICS_RESOLUTION_CONTEXT_FIELDS: [&str; 7] = [
        "manufacturer",
        "model",
        "variant",
        "model_year",
        "source_url",
        "registration_number",
        "serial_number",
    ];
    let changed_context = listing
        .as_object()
        .into_iter()
        .flat_map(|fields| {
            AVIONICS_RESOLUTION_CONTEXT_FIELDS
                .iter()
                .copied()
                .filter(|field| fields.contains_key(*field))
        })
        .collect::<Vec<_>>();
    if !explicitly_replaces_avionics && !changed_context.is_empty() {
        return Err(ListingStoreError::Validation(format!(
            "changing avionics-resolution context requires an explicit avionics replacement: {}",
            changed_context.join(", ")
        )));
    }

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
    let source_url = values.source_url.clone();
    apply_faa_grounding_identity(&mut values, &grounding);
    let pending_review_aspects = if explicitly_replaces_avionics {
        Some(
            resolve_listing_avionics_values(
                db,
                &mut values,
                extractor,
                source_url.as_deref(),
                None,
            )
            .await?
            .pending_review_aspects,
        )
    } else {
        None
    };
    update_listing_values(
        db,
        listing_id,
        &values,
        &values,
        false,
        explicitly_replaces_avionics,
        false,
    )
    .await?;
    if let Some(pending_review_aspects) = pending_review_aspects {
        replace_listing_pending_review(db, listing_id, &pending_review_aspects, false).await?;
    }
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
    let replay_provenance_count = query_scalar_one!(
        db,
        i64,
        r#"
        SELECT
          (SELECT COUNT(*) FROM listing_replay_run_items
           WHERE resulting_listing_id = ?)
          +
          (SELECT COUNT(*) FROM plugin_submission_materialization_receipts
           WHERE aircraft_sale_listing_id = ?)
        "#,
        listing_id,
        listing_id
    )?;
    if replay_provenance_count != 0 {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} is retained by clean-replay provenance and cannot be deleted"
        )));
    }
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

async fn pending_aircraft_compatibility_variant_id(db: &AppDb) -> StoreResult<i64> {
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT aircraft_model_variant_id
        FROM aircraft_sale_listing_pending_compatibility_placeholder
        WHERE singleton_id = 1
        "#
    )?)
}

async fn insert_listing(
    db: &AppDb,
    user_id: i64,
    values: &ListingValues,
    literal_identity_values: &ListingValues,
    source_identity_receipt_pending: bool,
    signed_source_binding: Option<&SignedSourceListingBinding>,
) -> StoreResult<i64> {
    let aircraft_model_variant_id = pending_aircraft_compatibility_variant_id(db).await?;
    let installed_engine_model_id = resolve_installed_engine_model_id(db, values).await?;
    let installed_propeller_model_id = resolve_installed_propeller_model_id(db, values).await?;
    let insert_sql = db.sql(
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
          ingestion_error,
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
        VALUES (?, ?, FALSE, ?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        RETURNING id
        "#,
    );
    let bind_exact_signed_checkpoint_sql = db.sql(match db.backend() {
        DatabaseBackend::Sqlite(_) => {
            r#"
            UPDATE plugin_submissions
            SET canonical_listing_id = ?, extracted_listing_json = ?, extraction_error = NULL
            WHERE id = ? AND user_id = ? AND plugin_install_id = ?
              AND source_url = ? AND submitted_at = ?
              AND rendered_html = ? AND rendered_html_sha256 = ?
              AND signature_base64 = ?
              AND canonical_listing_id IS NULL
              AND extracted_listing_json IS ?
              AND extraction_error IS ?
              AND EXISTS (
                SELECT 1 FROM plugin_installs exact_install
                WHERE exact_install.id = plugin_submissions.plugin_install_id
                  AND exact_install.user_id = plugin_submissions.user_id
                  AND exact_install.public_key_base64 = ?
                  AND exact_install.revoked_at IS ?
              )
            "#
        }
        DatabaseBackend::Postgres(_) => {
            r#"
            UPDATE plugin_submissions
            SET canonical_listing_id = ?, extracted_listing_json = ?, extraction_error = NULL
            WHERE id = ? AND user_id = ? AND plugin_install_id = ?
              AND source_url = ? AND submitted_at = ?
              AND rendered_html = ? AND rendered_html_sha256 = ?
              AND signature_base64 = ?
              AND canonical_listing_id IS NULL
              AND extracted_listing_json IS NOT DISTINCT FROM ?
              AND extraction_error IS NOT DISTINCT FROM ?
              AND EXISTS (
                SELECT 1 FROM plugin_installs exact_install
                WHERE exact_install.id = plugin_submissions.plugin_install_id
                  AND exact_install.user_id = plugin_submissions.user_id
                  AND exact_install.public_key_base64 = ?
                  AND exact_install.revoked_at IS NOT DISTINCT FROM ?
              )
            "#
        }
    });
    if let Some(binding) = signed_source_binding {
        let expected_sha256 = binding
            .expected_extracted_listing_json
            .as_deref()
            .map(|checkpoint| format!("{:x}", Sha256::digest(checkpoint.as_bytes())));
        if binding.user_id != user_id
            || values.source_url.as_deref() != Some(binding.source_url.as_str())
            || format!("{:x}", Sha256::digest(binding.rendered_html.as_bytes()))
                != binding.rendered_html_sha256
            || expected_sha256 != binding.expected_extracted_listing_sha256
            || format!(
                "{:x}",
                Sha256::digest(binding.bound_extracted_listing_json.as_bytes())
            ) != binding.bound_extracted_listing_sha256
        {
            return Err(ListingStoreError::State(
                "signed source binding does not match its exact capture checkpoint".to_string(),
            ));
        }
    }
    macro_rules! insert_and_bind {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let listing_id = sqlx::query_scalar::<_, i64>(&insert_sql)
                .bind(aircraft_model_variant_id)
                .bind(user_id)
                .bind(values.source_url.as_deref())
                .bind(values.model_year)
                .bind(values.asking_price_usd)
                .bind(values.currency.as_str())
                .bind(signed_source_binding.map(|binding| binding.submitted_at.as_str()))
                .bind(values.status.as_str())
                .bind(if source_identity_receipt_pending {
                    "quarantined"
                } else {
                    "incomplete"
                })
                .bind(source_identity_receipt_pending.then_some(SOURCE_IDENTITY_RECEIPT_PENDING))
                .bind(values.registration_number.as_deref())
                .bind(values.serial_number.as_deref())
                .bind(values.airframe_hours)
                .bind(values.engine_hours)
                .bind(values.engine_time_basis.as_str())
                .bind(values.engine_time_evidence.as_deref())
                .bind(values.engine_time_confidence.as_deref())
                .bind(values.propeller_hours)
                .bind(values.propeller_time_basis.as_str())
                .bind(values.propeller_time_evidence.as_deref())
                .bind(values.propeller_time_confidence.as_deref())
                .bind(installed_engine_model_id)
                .bind(installed_engine_model_id.and(values.source_url.as_deref()))
                .bind(values.installed_engine_evidence_text.as_deref())
                .bind(values.installed_engine_confidence.as_deref())
                .bind(installed_propeller_model_id)
                .bind(installed_propeller_model_id.and(values.source_url.as_deref()))
                .bind(values.installed_propeller_evidence_text.as_deref())
                .bind(values.installed_propeller_confidence.as_deref())
                .fetch_one(&mut *transaction)
                .await?;
            if let Some(binding) = signed_source_binding {
                let bound = sqlx::query(&bind_exact_signed_checkpoint_sql)
                    .bind(listing_id)
                    .bind(binding.bound_extracted_listing_json.as_str())
                    .bind(binding.submission_id)
                    .bind(binding.user_id)
                    .bind(binding.plugin_install_id)
                    .bind(binding.source_url.as_str())
                    .bind(binding.submitted_at.as_str())
                    .bind(binding.rendered_html.as_str())
                    .bind(binding.rendered_html_sha256.as_str())
                    .bind(binding.signature_base64.as_str())
                    .bind(binding.expected_extracted_listing_json.as_deref())
                    .bind(binding.expected_extraction_error.as_deref())
                    .bind(binding.install_public_key_base64.as_str())
                    .bind(binding.install_revoked_at.as_deref())
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                if bound != 1 {
                    return Err(ListingStoreError::State(
                        "signed source changed before its corrected listing could be atomically bound"
                            .to_string(),
                    ));
                }
            }
            transaction.commit().await?;
            Ok::<i64, ListingStoreError>(listing_id)
        }};
    }
    let listing_id = match db.backend() {
        DatabaseBackend::Sqlite(pool) => insert_and_bind!(pool)?,
        DatabaseBackend::Postgres(pool) => insert_and_bind!(pool)?,
    };

    if let Err(error) =
        stage_literal_aircraft_identity_observation(db, listing_id, literal_identity_values).await
    {
        return retain_receipt_gate_or_quarantine(
            db,
            listing_id,
            error,
            source_identity_receipt_pending,
        )
        .await;
    }
    if let Err(error) = replace_listing_avionics(db, listing_id, &values.avionics).await {
        return retain_receipt_gate_or_quarantine(
            db,
            listing_id,
            error,
            source_identity_receipt_pending,
        )
        .await;
    }
    if let Err(error) = replace_listing_facts(db, listing_id, values).await {
        return retain_receipt_gate_or_quarantine(
            db,
            listing_id,
            error,
            source_identity_receipt_pending,
        )
        .await;
    }
    Ok(listing_id)
}

async fn update_listing_values(
    db: &AppDb,
    listing_id: i64,
    values: &ListingValues,
    literal_identity_values: &ListingValues,
    update_added_at: bool,
    replace_avionics_links: bool,
    preserve_source_identity_receipt_gate: bool,
) -> StoreResult<()> {
    let installed_engine_model_id = resolve_installed_engine_model_id(db, values).await?;
    let installed_propeller_model_id = resolve_installed_propeller_model_id(db, values).await?;
    let added_at_assignment = if update_added_at {
        ", added_at = CURRENT_TIMESTAMP"
    } else {
        ""
    };
    let ingestion_assignment = if preserve_source_identity_receipt_gate {
        "ingestion_state = 'quarantined', ingestion_error = 'source_identity_correction_receipt_pending', is_verified = FALSE,"
    } else {
        "ingestion_state = 'incomplete', ingestion_error = NULL,"
    };
    let update_sql = format!(
        r#"
            UPDATE aircraft_sale_listings
            SET
              source_url = ?,
              model_year = ?,
              asking_price_usd = ?,
              currency = ?,
              status = ?,
              {ingestion_assignment}
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
    if let Err(error) =
        stage_literal_aircraft_identity_observation(db, listing_id, literal_identity_values).await
    {
        return retain_receipt_gate_or_quarantine(
            db,
            listing_id,
            error,
            preserve_source_identity_receipt_gate,
        )
        .await;
    }
    if replace_avionics_links {
        if let Err(error) = replace_listing_avionics(db, listing_id, &values.avionics).await {
            return retain_receipt_gate_or_quarantine(
                db,
                listing_id,
                error,
                preserve_source_identity_receipt_gate,
            )
            .await;
        }
    }
    if let Err(error) = replace_listing_facts(db, listing_id, values).await {
        return retain_receipt_gate_or_quarantine(
            db,
            listing_id,
            error,
            preserve_source_identity_receipt_gate,
        )
        .await;
    }
    Ok(())
}

async fn stage_literal_aircraft_identity_observation(
    db: &AppDb,
    listing_id: i64,
    values: &ListingValues,
) -> StoreResult<()> {
    let input_json = json!({
        "observation_kind": "literal_listing_input",
        "manufacturer": values.manufacturer,
        "model": values.model,
        "variant": values.variant,
        "model_year": values.model_year,
        "registration_number": values.registration_number,
        "serial_number": values.serial_number,
    })
    .to_string();
    let observation_sha256 = format!("{:x}", Sha256::digest(format!("{listing_id}:{input_json}")));
    execute_query!(
        db,
        r#"
        INSERT INTO aircraft_listing_identity_input_observations (
          aircraft_sale_listing_id,
          source_url,
          observed_make,
          observed_family,
          observed_designation,
          model_year,
          serial_number,
          registration_number,
          input_json,
          observation_sha256
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (observation_sha256) DO NOTHING
        "#,
        listing_id,
        values.source_url.as_deref(),
        values.manufacturer.as_str(),
        values.model.as_str(),
        values.variant.as_str(),
        values.model_year,
        values.serial_number.as_deref(),
        values.registration_number.as_deref(),
        input_json.as_str(),
        observation_sha256.as_str()
    )?;
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

async fn resolve_listing_avionics_values(
    db: &AppDb,
    values: &mut ListingValues,
    extractor: Option<&GeminiListingExtractor>,
    source_url: Option<&str>,
    listing_context: Option<&str>,
) -> StoreResult<ResolvedListingAvionics> {
    let listing_context = listing_context
        .map(listing_context_excerpt)
        .unwrap_or_default();
    let mut resolved: Vec<ListingAvionicsValue> = Vec::new();
    let mut pending = Vec::new();
    let mut dispositions = Vec::new();

    for (index, item) in values.avionics.clone().into_iter().enumerate() {
        let identity_request = listing_avionics_identity_request(
            values,
            source_url,
            &listing_context,
            &item.manufacturer,
            &item.model,
            &item.avionics_types,
            item.quantity,
        );
        let primary = resolve_listing_avionics_identity(
            db,
            extractor,
            &identity_request,
            item.source_confidence.as_deref(),
        )
        .await;

        match primary {
            ListingAvionicsIdentityResolution::Rejected { .. } => {
                // High-confidence garbage never enters either the canonical
                // catalog or the review queue.
                dispositions.push(AutomaticOccurrenceDisposition::discarded(
                    index,
                    OccurrenceRole::Primary,
                ));
                if item.replaces.is_some() {
                    dispositions.push(AutomaticOccurrenceDisposition::discarded(
                        index,
                        OccurrenceRole::Replacement,
                    ));
                }
            }
            ListingAvionicsIdentityResolution::Pending {
                reason,
                suggested_product,
            } => {
                let replacement = resolve_listing_avionics_replacement(
                    db,
                    values,
                    extractor,
                    source_url,
                    &listing_context,
                    index,
                    &item,
                )
                .await?;
                let (replaces_product_id, replacement_aspect_id) = match &replacement {
                    ListingAvionicsReplacementResolution::None => (None, None),
                    ListingAvionicsReplacementResolution::Approved(identity) => {
                        dispositions.push(AutomaticOccurrenceDisposition::linked(
                            index,
                            OccurrenceRole::Replacement,
                            identity.id,
                        ));
                        (Some(identity.id), None)
                    }
                    ListingAvionicsReplacementResolution::Pending(aspect) => {
                        (None, Some(aspect.id.clone()))
                    }
                };
                pending.push(pending_avionics_aspect(
                    ReviewAspectId::String(format!("avionics:{index}:primary")),
                    &item,
                    reason,
                    suggested_product,
                    replaces_product_id,
                    replacement_aspect_id,
                ));
                if let ListingAvionicsReplacementResolution::Pending(aspect) = replacement {
                    pending.push(*aspect);
                }
            }
            ListingAvionicsIdentityResolution::Approved(identity) => {
                let replacement = resolve_listing_avionics_replacement(
                    db,
                    values,
                    extractor,
                    source_url,
                    &listing_context,
                    index,
                    &item,
                )
                .await?;
                match replacement {
                    ListingAvionicsReplacementResolution::None => {
                        dispositions.push(AutomaticOccurrenceDisposition::linked(
                            index,
                            OccurrenceRole::Primary,
                            identity.id,
                        ));
                        resolved.push(listing_avionics_value_from_catalog(&item, &identity));
                    }
                    ListingAvionicsReplacementResolution::Approved(replaced) => {
                        dispositions.push(AutomaticOccurrenceDisposition::linked(
                            index,
                            OccurrenceRole::Primary,
                            identity.id,
                        ));
                        dispositions.push(AutomaticOccurrenceDisposition::linked(
                            index,
                            OccurrenceRole::Replacement,
                            replaced.id,
                        ));
                        let mut resolved_item =
                            listing_avionics_value_from_catalog(&item, &identity);
                        resolved_item.replaces = Some(ParsedAvionicsReference {
                            manufacturer: replaced.manufacturer,
                            model: replaced.model,
                            avionics_types: replaced.avionics_types,
                        });
                        resolved_item.replaces_avionics_model_id = Some(replaced.id);
                        resolved.push(resolved_item);
                    }
                    ListingAvionicsReplacementResolution::Pending(replacement_aspect) => {
                        pending.push(pending_avionics_aspect(
                            ReviewAspectId::String(format!("avionics:{index}:primary")),
                            &item,
                            format!(
                                "the product identity is verified, but its {} relationship has an unresolved target",
                                item.configuration_action
                            ),
                            Some(review_product_from_identity(&identity)),
                            None,
                            Some(replacement_aspect.id.clone()),
                        ));
                        pending.push(*replacement_aspect);
                    }
                }
            }
        }
    }

    values.avionics = coalesce_resolved_listing_avionics(resolved)?;
    Ok(ResolvedListingAvionics {
        pending_review_aspects: pending,
        occurrence_dispositions: dispositions,
    })
}

enum ListingAvionicsIdentityResolution {
    Approved(ApprovedAvionicsIdentity),
    Rejected {
        reason: String,
    },
    Pending {
        reason: String,
        suggested_product: Option<ReviewProduct>,
    },
}

async fn resolve_listing_avionics_identity(
    db: &AppDb,
    extractor: Option<&GeminiListingExtractor>,
    request: &AvionicsIdentityRequest,
    source_confidence: Option<&str>,
) -> ListingAvionicsIdentityResolution {
    if let Some(extractor) = extractor {
        return listing_avionics_identity_resolution(
            resolve_avionics_identity(db, extractor, request).await,
            source_confidence,
        );
    }

    match resolve_verified_local_avionics_identity(db, request).await {
        Ok(Some(identity)) => listing_avionics_identity_resolution::<CatalogError>(
            Ok(AvionicsIdentityOutcome::Approved(identity)),
            source_confidence,
        ),
        Ok(None) => ListingAvionicsIdentityResolution::Pending {
            reason: AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON.to_string(),
            suggested_product: None,
        },
        Err(error) => listing_avionics_identity_resolution(Err(error), source_confidence),
    }
}

fn listing_avionics_identity_resolution<E>(
    outcome: Result<AvionicsIdentityOutcome, E>,
    source_confidence: Option<&str>,
) -> ListingAvionicsIdentityResolution {
    match outcome {
        Ok(AvionicsIdentityOutcome::Approved(identity)) if source_confidence == Some("high") => {
            ListingAvionicsIdentityResolution::Approved(identity)
        }
        Ok(AvionicsIdentityOutcome::Approved(identity)) => {
            ListingAvionicsIdentityResolution::Pending {
                reason: AVIONICS_INSTALLATION_EVIDENCE_NOT_HIGH_REASON.to_string(),
                suggested_product: Some(review_product_from_identity(&identity)),
            }
        }
        Ok(AvionicsIdentityOutcome::Rejected { reason }) => {
            ListingAvionicsIdentityResolution::Rejected { reason }
        }
        Ok(AvionicsIdentityOutcome::Unresolved { reason }) => {
            ListingAvionicsIdentityResolution::Pending {
                reason,
                suggested_product: None,
            }
        }
        Err(_error) => ListingAvionicsIdentityResolution::Pending {
            // Provider request details remain in usage accounting when a call
            // was attempted. Review payloads retain only a stable, actionable
            // explanation and never expose transport or catalog internals.
            reason: AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON.to_string(),
            suggested_product: None,
        },
    }
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
        authoritative_direct_source_urls: Vec::new(),
        authoritative_identity_anchors: Vec::new(),
        manufacturer: manufacturer.to_string(),
        model: model.to_string(),
        avionics_types: avionics_types.to_vec(),
        quantity: quantity.max(1),
    }
}

enum ListingAvionicsReplacementResolution {
    None,
    Approved(Box<ApprovedAvionicsIdentity>),
    Pending(Box<PendingReviewAspect>),
}

async fn resolve_listing_avionics_replacement(
    db: &AppDb,
    values: &ListingValues,
    extractor: Option<&GeminiListingExtractor>,
    source_url: Option<&str>,
    listing_context: &str,
    index: usize,
    item: &ListingAvionicsValue,
) -> StoreResult<ListingAvionicsReplacementResolution> {
    if item.configuration_action == "installed" {
        if item.replaces.is_some() || item.replaces_avionics_model_id.is_some() {
            return Err(ListingStoreError::Validation(format!(
                "installed avionics cannot also declare a replacement target: {} {}",
                item.manufacturer, item.model
            )));
        }
        return Ok(ListingAvionicsReplacementResolution::None);
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
    match resolve_listing_avionics_identity(
        db,
        extractor,
        &request,
        item.source_confidence.as_deref(),
    )
    .await
    {
        ListingAvionicsIdentityResolution::Approved(identity) => Ok(
            ListingAvionicsReplacementResolution::Approved(Box::new(identity)),
        ),
        ListingAvionicsIdentityResolution::Rejected { reason } => Ok(
            ListingAvionicsReplacementResolution::Pending(Box::new(pending_replacement_aspect(
                index,
                replaced,
                item,
                format!("grounded classification rejected this replacement target: {reason}"),
            ))),
        ),
        ListingAvionicsIdentityResolution::Pending {
            reason,
            suggested_product,
        } => {
            let mut aspect = pending_replacement_aspect(index, replaced, item, reason);
            aspect.suggested_product = suggested_product;
            Ok(ListingAvionicsReplacementResolution::Pending(Box::new(
                aspect,
            )))
        }
    }
}

fn pending_avionics_aspect(
    id: ReviewAspectId,
    item: &ListingAvionicsValue,
    reason: String,
    suggested_product: Option<ReviewProduct>,
    replaces_product_id: Option<i64>,
    replacement_aspect_id: Option<ReviewAspectId>,
) -> PendingReviewAspect {
    PendingReviewAspect {
        id,
        kind: "avionics".to_string(),
        label: format!("{} {}", item.manufacturer, item.model),
        observed_text: avionics_observation_text(
            &item.manufacturer,
            &item.model,
            &item.avionics_types,
            item.quantity,
            &item.configuration_action,
        ),
        required: true,
        reason,
        suggested_product,
        proposed_product: Some(review_product_from_observation(
            &item.manufacturer,
            &item.model,
            &item.avionics_types,
        )),
        allowed_actions: vec![
            ReviewAction::UseVerifiedProduct,
            ReviewAction::CreateVerifiedProduct,
            ReviewAction::Discard,
        ],
        quantity: item.quantity.max(1),
        configuration_action: item.configuration_action.clone(),
        source_evidence_text: item.source_notes.clone(),
        source_confidence: item.source_confidence.clone(),
        replaces_product_id,
        replacement_aspect_id,
        covered_associations: Vec::new(),
        reviewer_correction_association_binding: None,
        reuse_attestation_target_id: None,
    }
}

fn pending_replacement_aspect(
    index: usize,
    replaced: &ParsedAvionicsReference,
    parent: &ListingAvionicsValue,
    reason: String,
) -> PendingReviewAspect {
    PendingReviewAspect {
        id: ReviewAspectId::String(format!("avionics:{index}:replacement")),
        kind: "avionics".to_string(),
        label: format!("{} {}", replaced.manufacturer, replaced.model),
        observed_text: avionics_observation_text(
            &replaced.manufacturer,
            &replaced.model,
            &replaced.avionics_types,
            1,
            "installed",
        ),
        required: true,
        reason,
        suggested_product: None,
        proposed_product: Some(review_product_from_observation(
            &replaced.manufacturer,
            &replaced.model,
            &replaced.avionics_types,
        )),
        allowed_actions: vec![
            ReviewAction::UseVerifiedProduct,
            ReviewAction::CreateVerifiedProduct,
            ReviewAction::Discard,
        ],
        quantity: 1,
        // This aspect supplies another link's target; it is not independently
        // installed as an additional listing row.
        configuration_action: "installed".to_string(),
        source_evidence_text: parent.source_notes.clone(),
        source_confidence: parent.source_confidence.clone(),
        replaces_product_id: None,
        replacement_aspect_id: None,
        covered_associations: Vec::new(),
        reviewer_correction_association_binding: None,
        reuse_attestation_target_id: None,
    }
}

fn review_product_from_observation(
    manufacturer: &str,
    model: &str,
    capabilities: &[String],
) -> ReviewProduct {
    ReviewProduct {
        id: None,
        manufacturer: manufacturer.to_string(),
        model: model.to_string(),
        capabilities: capabilities.to_vec(),
        stable_identifier: None,
        identity_source_url: None,
        identity_source_title: None,
        identity_evidence_text: None,
    }
}

fn review_product_from_identity(identity: &ApprovedAvionicsIdentity) -> ReviewProduct {
    ReviewProduct {
        id: Some(identity.id),
        manufacturer: identity.manufacturer.clone(),
        model: identity.model.clone(),
        capabilities: identity.avionics_types.clone(),
        stable_identifier: Some(StableIdentifier {
            kind: identity.manufacturer_identifier_kind.clone(),
            value: identity.manufacturer_identifier.clone(),
        }),
        identity_source_url: Some(identity.evidence_url.clone()),
        identity_source_title: Some(identity.evidence_title.clone()),
        identity_evidence_text: Some(identity.evidence.clone()),
    }
}

fn avionics_observation_text(
    manufacturer: &str,
    model: &str,
    capabilities: &[String],
    quantity: i64,
    configuration_action: &str,
) -> String {
    format!(
        "{} {} · {} · quantity {} · {}",
        manufacturer,
        model,
        capabilities.join(", "),
        quantity.max(1),
        configuration_action
    )
}

fn listing_avionics_value_from_catalog(
    original: &ListingAvionicsValue,
    identity: &ApprovedAvionicsIdentity,
) -> ListingAvionicsValue {
    ListingAvionicsValue {
        avionics_model_id: Some(identity.id),
        manufacturer: identity.manufacturer.clone(),
        model: identity.model.clone(),
        avionics_types: identity.avionics_types.clone(),
        quantity: original.quantity.max(1),
        source: original.source.clone(),
        // The listing link retains only the listing occurrence. Authoritative
        // product evidence belongs to the single approved catalog identity.
        source_notes: original.source_notes.clone(),
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
            "avionics" => values.avionics = avionics_from_value(value)?,
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

fn avionics_from_value(value: &Value) -> StoreResult<Vec<ListingAvionicsValue>> {
    let items = value
        .as_array()
        .ok_or_else(|| ListingStoreError::Validation("avionics must be an array".to_string()))?;
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let object = item.as_object().ok_or_else(|| {
                ListingStoreError::Validation(format!("avionics[{index}] must be an object"))
            })?;
            let manufacturer = required_string_from_value(
                object.get("manufacturer").unwrap_or(&Value::Null),
                &format!("avionics[{index}].manufacturer"),
            )?;
            let model = required_string_from_value(
                object.get("model").unwrap_or(&Value::Null),
                &format!("avionics[{index}].model"),
            )?;
            let avionics_types = avionics_types_from_object(object);
            Ok(ListingAvionicsValue::from_parsed(ParsedAvionics {
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
        && canonical_valuation_facts(&listing.valuation_facts)
            == canonical_valuation_facts(&values.valuation_facts);
    if !scalar_fields_match {
        return Ok(false);
    }

    if canonical_listing_avionics_graph(db, listing.id).await?
        != canonical_values_avionics_graph(db, &values.avionics).await?
    {
        return Ok(false);
    }

    Ok(
        canonical_listing_engine(db, listing).await? == canonical_values_engine(db, values).await?
            && canonical_listing_propeller(db, listing).await?
                == canonical_values_propeller(db, values).await?,
    )
}

type CanonicalAvionicsGraph = (String, i64, String, Option<String>, String, String);

fn canonicalize_avionics_graph(
    value: impl IntoIterator<Item = CanonicalAvionicsGraph>,
) -> Vec<CanonicalAvionicsGraph> {
    let mut canonical = value
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    canonical.sort();
    canonical
}

async fn canonical_listing_avionics_graph(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<Vec<CanonicalAvionicsGraph>> {
    let rows = query_as_all!(
        db,
        ListingAvionicsGraphRow,
        r#"
        SELECT
          subject.avionics_manufacturer_identity_id
            AS subject_manufacturer_identity_id,
          subject.canonical_product_key AS subject_product_key,
          link.quantity,
          link.configuration_action,
          link.source_notes,
          link.source_confidence,
          link.replaces_avionics_model_id,
          replacement.avionics_manufacturer_identity_id
            AS replacement_manufacturer_identity_id,
          replacement.canonical_product_key AS replacement_product_key
        FROM aircraft_sale_listing_avionics link
        JOIN avionics_approved_product_graph_identities subject
          ON subject.avionics_model_id = link.avionics_model_id
        LEFT JOIN avionics_approved_product_graph_identities replacement
          ON replacement.avionics_model_id = link.replaces_avionics_model_id
        WHERE link.aircraft_sale_listing_id = ?
        "#,
        listing_id
    )?;
    let mut canonical = Vec::with_capacity(rows.len());
    for row in rows {
        let subject_key = approved_avionics_product_key(
            row.subject_manufacturer_identity_id,
            &row.subject_product_key,
        )
        .map_err(ListingStoreError::State)?;
        let replacement_key = match (
            row.replaces_avionics_model_id,
            row.replacement_manufacturer_identity_id,
            row.replacement_product_key,
        ) {
            (None, None, None) => None,
            (Some(_), Some(identity_id), Some(product_key)) => Some(
                approved_avionics_product_key(identity_id, &product_key)
                    .map_err(ListingStoreError::State)?,
            ),
            _ => {
                return Err(ListingStoreError::State(format!(
                    "listing {listing_id} has an avionics replacement without an approved product graph identity"
                )))
            }
        };
        canonical.push((
            subject_key,
            row.quantity.max(1),
            row.configuration_action,
            replacement_key,
            normalize_name(row.source_notes.as_deref().unwrap_or("")),
            normalize_name(row.source_confidence.as_deref().unwrap_or("")),
        ));
    }
    Ok(canonicalize_avionics_graph(canonical))
}

async fn canonical_values_avionics_graph(
    db: &AppDb,
    value: &[ListingAvionicsValue],
) -> StoreResult<Vec<CanonicalAvionicsGraph>> {
    let mut canonical = Vec::with_capacity(value.len());
    for item in value {
        let model_id = item.avionics_model_id.ok_or_else(|| {
            ListingStoreError::Validation(format!(
                "avionics must resolve to an approved product graph before verified-listing refresh: {} {}",
                item.manufacturer, item.model
            ))
        })?;
        let subject_key = approved_catalog_avionics_graph_key(db, model_id).await?;
        let replacement_key = match item.replaces_avionics_model_id {
            Some(replacement_id) => {
                Some(approved_catalog_avionics_graph_key(db, replacement_id).await?)
            }
            None => None,
        };
        canonical.push((
            subject_key,
            item.quantity.max(1),
            item.configuration_action.clone(),
            replacement_key,
            normalize_name(item.source_notes.as_deref().unwrap_or("")),
            normalize_name(item.source_confidence.as_deref().unwrap_or("")),
        ));
    }
    Ok(canonicalize_avionics_graph(canonical))
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
) -> StoreResult<ListingFinalizationOutcome> {
    match enrich_aircraft_spec_for_listing_if_missing(db, extractor, listing_id, listing_text).await
    {
        Ok(()) => {}
        Err(AircraftStoreError::Database(message)) => {
            return Err(ListingStoreError::Database(message));
        }
        Err(AircraftStoreError::NotFound(message)) => {
            return Err(ListingStoreError::NotFound(message));
        }
        Err(AircraftStoreError::Model(message)) => {
            require_listing_admission(db, listing_id)
                .await
                .map_err(listing_admission_error)?;
            return pending_factory_reference(
                db,
                listing_id,
                format!("factory aircraft specification remains pending: {message}"),
            )
            .await;
        }
    }
    if listing_aircraft_spec_is_pending(db, listing_id).await? {
        return pending_factory_reference(
            db,
            listing_id,
            if extractor.is_some() {
                "factory aircraft specification enrichment completed without valuation-ready reference data"
                    .to_string()
            } else {
                "Gemini extractor is not configured; factory aircraft specification remains pending"
                    .to_string()
            },
        )
        .await;
    }

    if listing_missing_avionics_metadata_count(db, listing_id).await? > 0 {
        let Some(extractor) = extractor else {
            return pending_factory_reference(
                db,
                listing_id,
                    "Gemini extractor is not configured; reusable installed-avionics metadata remains pending"
                        .to_string(),
            )
            .await;
        };
        match enrich_listing_avionics_metadata(db, extractor, true, listing_id, None, false).await {
            Ok(_) => {}
            Err(AvionicsStoreError::Database(message)) => {
                return Err(ListingStoreError::Database(message));
            }
            Err(AvionicsStoreError::Model(message)) => {
                require_listing_admission(db, listing_id)
                    .await
                    .map_err(listing_admission_error)?;
                return pending_factory_reference(
                    db,
                    listing_id,
                    format!("reusable installed-avionics metadata remains pending: {message}"),
                )
                .await;
            }
        }
        let remaining = listing_missing_avionics_metadata_count(db, listing_id).await?;
        if remaining > 0 {
            return pending_factory_reference(
                db,
                listing_id,
                format!(
                    "reusable installed-avionics metadata remains pending for {remaining} catalog rows"
                ),
            )
            .await;
        }
    }

    if listing_model_year_factory_reference_is_pending(db, listing_id).await? {
        let Some(extractor) = extractor else {
            return pending_factory_reference(
                db,
                listing_id,
                    "Gemini extractor is not configured; model-year factory reference data remains pending"
                        .to_string(),
            )
            .await;
        };
        match enrich_model_year_avionics_and_price_point_for_listing(
            db, extractor, true, listing_id, None, false,
        )
        .await
        {
            Ok(_) => {}
            Err(AvionicsStoreError::Database(message)) => {
                return Err(ListingStoreError::Database(message));
            }
            Err(AvionicsStoreError::Model(message)) => {
                // The reference workflow repeats FAA admission before doing
                // provider work. If that admission changed concurrently, it
                // remains a listing failure rather than shared reference work.
                require_listing_admission(db, listing_id)
                    .await
                    .map_err(listing_admission_error)?;
                return pending_factory_reference(
                    db,
                    listing_id,
                    format!("model-year factory reference curation remains pending: {message}"),
                )
                .await;
            }
        }
        if listing_model_year_factory_reference_is_pending(db, listing_id).await? {
            return pending_factory_reference(
                db,
                listing_id,
                    "model-year factory reference curation completed without valuation-ready price and default-avionics data"
                        .to_string(),
            )
            .await;
        }
    }

    if let Ok(Some(identity)) = listing_aircraft_identity(db, listing_id).await {
        mark_valuation_snapshot_stale_best_effort(db, identity.aircraft_model_id).await;
    }
    let _ = cleanup_orphan_records(db).await;
    Ok(ListingFinalizationOutcome::Ready)
}

async fn pending_factory_reference(
    db: &AppDb,
    listing_id: i64,
    reason: String,
) -> StoreResult<ListingFinalizationOutcome> {
    if let Ok(Some(identity)) = listing_aircraft_identity(db, listing_id).await {
        mark_valuation_snapshot_stale_best_effort(db, identity.aircraft_model_id).await;
    }
    let _ = cleanup_orphan_records(db).await;
    Ok(ListingFinalizationOutcome::PendingReference { reason })
}

async fn finalize_listing_ingestion(
    db: &AppDb,
    listing_id: i64,
    extractor: Option<&GeminiListingExtractor>,
    listing_text: Option<&str>,
) -> StoreResult<()> {
    if source_identity_receipt_is_pending(db, listing_id).await? {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} is waiting for its immutable source identity correction receipt"
        )));
    }
    // Establish an FAA-backed aircraft assignment independently of avionics
    // review when the approved catalog already contains one exact identity.
    // Missing canonical catalog data is itself review work, not an ingestion
    // failure, when the listing already has a durable pending-review bundle.
    // Raw FAA rejection and database failures still fail closed.
    match prepare_listing_canonical_aircraft_identity(db, listing_id).await {
        Ok(CanonicalAircraftIdentityPreparation::Ready) => {}
        Ok(CanonicalAircraftIdentityPreparation::PendingCuration {
            reason,
            candidate_count,
        }) => {
            if listing_has_pending_review(db, listing_id).await? {
                mark_listing_pending_review(db, listing_id).await?;
                return Ok(());
            }
            return quarantine_after_error(
                db,
                listing_id,
                aircraft_identity_curation_error(listing_id, reason, candidate_count),
            )
            .await;
        }
        Err(error) => return quarantine_after_error(db, listing_id, error).await,
    }
    // Expected avionics ambiguity is a review state, not an ingestion failure.
    // Never run enrichment or promote a listing while the exact current
    // extraction still has a pending review bundle.
    if listing_has_pending_review(db, listing_id).await? {
        mark_listing_pending_review(db, listing_id).await?;
        return Ok(());
    }
    match complete_listing_ingestion(db, listing_id, extractor, listing_text).await {
        Ok(ListingFinalizationOutcome::Ready) => {
            mark_listing_ready(db, listing_id).await?;
            Ok(())
        }
        Ok(ListingFinalizationOutcome::PendingReference { .. }) => {
            mark_listing_pending_factory_reference(db, listing_id).await?;
            Ok(())
        }
        Err(error) => quarantine_after_error(db, listing_id, error).await,
    }
}

async fn source_identity_receipt_is_pending(db: &AppDb, listing_id: i64) -> StoreResult<bool> {
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT COUNT(*)
        FROM aircraft_sale_listings
        WHERE id = ? AND ingestion_error = ?
        "#,
        listing_id,
        SOURCE_IDENTITY_RECEIPT_PENDING
    )? == 1)
}

async fn replace_listing_pending_review(
    db: &AppDb,
    listing_id: i64,
    aspects: &[PendingReviewAspect],
    preserve_source_identity_receipt_gate: bool,
) -> StoreResult<()> {
    match replace_pending_review(db, listing_id, None, aspects).await {
        Ok(_) => Ok(()),
        Err(error) => {
            // Listing links have already been replaced by the caller. A prior
            // bundle may contain exact covered link IDs from before that
            // replacement, so it is no longer safe review evidence. Fail
            // closed by removing it before recording the ingestion failure.
            let clear_error = clear_pending_review(db, listing_id).await.err();
            let detail = match clear_error {
                Some(clear_error) => format!(
                    "could not persist pending listing review: {error}; could not clear stale prior review: {clear_error}"
                ),
                None => format!("could not persist pending listing review: {error}"),
            };
            retain_receipt_gate_or_quarantine(
                db,
                listing_id,
                ListingStoreError::State(detail),
                preserve_source_identity_receipt_gate,
            )
            .await
        }
    }
}

/// Finish a listing after an explicit review transaction has applied every
/// pending decision. Review resolution deliberately leaves the listing
/// incomplete and private: enrichment can make network calls and therefore
/// cannot safely run inside the transaction that replaces avionics links.
/// Only a fully enriched listing with a durable source is published.
pub async fn finalize_reviewed_listing_ingestion(
    db: &AppDb,
    listing_id: i64,
    extractor: Option<&GeminiListingExtractor>,
    listing_text: Option<&str>,
) -> Result<ListingFinalizationOutcome, ListingStoreError> {
    if source_identity_receipt_is_pending(db, listing_id).await? {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} is waiting for its immutable source identity correction receipt"
        )));
    }
    let state = query_as_optional!(
        db,
        (String, bool),
        "SELECT ingestion_state, is_verified FROM aircraft_sale_listings WHERE id = ?",
        listing_id
    )?
    .ok_or_else(|| ListingStoreError::NotFound(format!("listing {listing_id} not found")))?;
    if listing_has_pending_review(db, listing_id).await? {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} still has a pending review"
        )));
    }
    // A response retry after successful publication must be free and
    // idempotent. In particular, do not re-enter any network enrichment path.
    if state.0 == "ready" && state.1 {
        return Ok(ListingFinalizationOutcome::Ready);
    }
    if let Err(error) = ensure_listing_canonical_aircraft_identity(db, listing_id).await {
        return quarantine_after_error(db, listing_id, error).await;
    }
    match complete_listing_ingestion(db, listing_id, extractor, listing_text).await {
        Ok(ListingFinalizationOutcome::Ready) => {
            match mark_reviewed_listing_ready(db, listing_id).await {
                Ok(()) => Ok(ListingFinalizationOutcome::Ready),
                Err(error) => quarantine_after_error(db, listing_id, error).await,
            }
        }
        Ok(pending @ ListingFinalizationOutcome::PendingReference { .. }) => {
            match mark_listing_pending_factory_reference(db, listing_id).await {
                Ok(()) => Ok(pending),
                Err(error) => quarantine_after_error(db, listing_id, error).await,
            }
        }
        Err(error) => quarantine_after_error(db, listing_id, error).await,
    }
}

/// Establish and strictly revalidate the immutable FAA-backed aircraft
/// assignment needed by review and publication workflows.
///
/// Raw FAA admission runs first. Promotion may select only one exact,
/// already-curated catalog identity; this boundary never creates catalog
/// hierarchy from listing prose. The final strict admission check proves that
/// the selected assignment and valuation compatibility projection cite the
/// current FAA record.
pub(crate) async fn ensure_listing_canonical_aircraft_identity(
    db: &AppDb,
    listing_id: i64,
) -> Result<(), ListingStoreError> {
    match prepare_listing_canonical_aircraft_identity(db, listing_id).await? {
        CanonicalAircraftIdentityPreparation::Ready => Ok(()),
        CanonicalAircraftIdentityPreparation::PendingCuration {
            reason,
            candidate_count,
        } => Err(aircraft_identity_curation_error(
            listing_id,
            reason,
            candidate_count,
        )),
    }
}

#[derive(Debug)]
enum CanonicalAircraftIdentityPreparation {
    Ready,
    PendingCuration {
        reason: String,
        candidate_count: usize,
    },
}

fn aircraft_identity_curation_error(
    listing_id: i64,
    reason: String,
    candidate_count: usize,
) -> ListingStoreError {
    ListingStoreError::Validation(format!(
        "listing {listing_id} requires aircraft identity curation: {reason} (exact candidates: {candidate_count})"
    ))
}

async fn prepare_listing_canonical_aircraft_identity(
    db: &AppDb,
    listing_id: i64,
) -> Result<CanonicalAircraftIdentityPreparation, ListingStoreError> {
    let grounding = require_listing_faa_admission(db, listing_id)
        .await
        .map_err(listing_admission_error)?;
    match ensure_listing_identity_assignment_from_approved_catalog(db, listing_id, &grounding)
        .await
        .map_err(|error| ListingStoreError::State(error.to_string()))?
    {
        EnsureIdentityAssignmentOutcome::Current { .. }
        | EnsureIdentityAssignmentOutcome::Assigned { .. } => {}
        EnsureIdentityAssignmentOutcome::PendingCuration {
            reason,
            candidate_count,
        } => {
            return Ok(CanonicalAircraftIdentityPreparation::PendingCuration {
                reason,
                candidate_count,
            });
        }
    }
    require_listing_admission(db, listing_id)
        .await
        .map_err(listing_admission_error)?;
    Ok(CanonicalAircraftIdentityPreparation::Ready)
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

async fn mark_listing_pending_factory_reference(db: &AppDb, listing_id: i64) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = CASE
              WHEN EXISTS (
                SELECT 1
                FROM aircraft_sale_listing_pending_reviews review
                WHERE review.listing_id = aircraft_sale_listings.id
              ) THEN 'pending_review'
              ELSE 'incomplete'
            END,
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            is_verified = FALSE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        listing_id
    )?;
    Ok(())
}

async fn listing_has_pending_review(db: &AppDb, listing_id: i64) -> StoreResult<bool> {
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT COUNT(*)
        FROM aircraft_sale_listing_pending_reviews
        WHERE listing_id = ?
        "#,
        listing_id
    )? > 0)
}

async fn mark_listing_pending_review(db: &AppDb, listing_id: i64) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'pending_review',
            ingestion_error = NULL,
            ingestion_completed_at = NULL,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_reviews review
            WHERE review.listing_id = aircraft_sale_listings.id
          )
        "#,
        listing_id
    )?;
    Ok(())
}

async fn mark_listing_ready(db: &AppDb, listing_id: i64) -> StoreResult<()> {
    let updated = execute_ready_listing_update(
        db,
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'ready',
            ingestion_error = NULL,
            ingestion_completed_at = CURRENT_TIMESTAMP,
            -- Mandatory FAA admission, catalog resolution, and enrichment
            -- have all completed at this point. A durable listing source is
            -- the final publication requirement; source-less manual drafts
            -- remain private even when otherwise complete.
            is_verified = CASE WHEN source_url IS NOT NULL THEN TRUE ELSE FALSE END,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND COALESCE(ingestion_error, '') <> 'source_identity_correction_receipt_pending'
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_reviews review
            WHERE review.listing_id = aircraft_sale_listings.id
          )
        "#,
        listing_id,
    )
    .await?;
    if updated != 1 {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} cannot be published while review work remains or after concurrent deletion"
        )));
    }
    Ok(())
}

async fn mark_reviewed_listing_ready(db: &AppDb, listing_id: i64) -> StoreResult<()> {
    let updated = execute_ready_listing_update(
        db,
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = 'ready',
            ingestion_error = NULL,
            ingestion_completed_at = CURRENT_TIMESTAMP,
            is_verified = TRUE,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
          AND source_url IS NOT NULL
          AND COALESCE(ingestion_error, '') <> 'source_identity_correction_receipt_pending'
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_sale_listing_pending_reviews review
            WHERE review.listing_id = aircraft_sale_listings.id
          )
        "#,
        listing_id,
    )
    .await?;
    if updated != 1 {
        return Err(ListingStoreError::State(format!(
            "reviewed listing {listing_id} cannot be published without a source or while review work remains"
        )));
    }
    Ok(())
}

/// Publish with a post-lock snapshot on Postgres. Child avionics/review writes
/// use `ROW EXCLUSIVE` table locks, so taking these conflicting locks in a
/// separate statement before the ready update closes the READ COMMITTED race
/// where the parent and child triggers could otherwise miss each other's
/// uncommitted changes. SQLite already serializes writers.
async fn execute_ready_listing_update(
    db: &AppDb,
    statement: &str,
    listing_id: i64,
) -> StoreResult<u64> {
    let statement = db.sql(statement);
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => Ok(sqlx::query(&statement)
            .bind(listing_id)
            .execute(pool)
            .await?
            .rows_affected()),
        DatabaseBackend::Postgres(pool) => {
            let mut transaction = pool.begin().await?;
            sqlx::query(POSTGRES_LISTING_CHILD_LOCK_SQL)
                .execute(&mut *transaction)
                .await?;
            let updated = sqlx::query(&statement)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            transaction.commit().await?;
            Ok(updated)
        }
    }
}

async fn quarantine_after_error<T>(
    db: &AppDb,
    listing_id: i64,
    error: ListingStoreError,
) -> StoreResult<T> {
    let message = error.to_string();
    let preserves_admission = matches!(error, ListingStoreError::AircraftAdmission(_));
    execute_query!(
        db,
        r#"
        UPDATE aircraft_sale_listings
        SET ingestion_state = CASE
              WHEN EXISTS (
                SELECT 1
                FROM aircraft_sale_listing_pending_reviews review
                WHERE review.listing_id = aircraft_sale_listings.id
              ) THEN 'pending_review'
              ELSE 'quarantined'
            END,
            ingestion_error = CASE
              WHEN EXISTS (
                SELECT 1
                FROM aircraft_sale_listing_pending_reviews review
                WHERE review.listing_id = aircraft_sale_listings.id
              ) THEN NULL
              ELSE ?
            END,
            ingestion_completed_at = NULL,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
        message.as_str(),
        listing_id
    )?;
    if preserves_admission {
        Err(error)
    } else {
        Err(ListingStoreError::Ingestion {
            listing_id,
            message,
        })
    }
}

async fn retain_receipt_gate_or_quarantine<T>(
    db: &AppDb,
    listing_id: i64,
    error: ListingStoreError,
    preserve_source_identity_receipt_gate: bool,
) -> StoreResult<T> {
    if preserve_source_identity_receipt_gate {
        return if matches!(error, ListingStoreError::AircraftAdmission(_)) {
            Err(error)
        } else {
            Err(ListingStoreError::Ingestion {
                listing_id,
                message: error.to_string(),
            })
        };
    }
    quarantine_after_error(db, listing_id, error).await
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

pub(crate) async fn listing_factory_reference_is_pending(
    db: &AppDb,
    listing_id: i64,
) -> Result<bool, ListingStoreError> {
    if query_scalar_one!(
        db,
        i64,
        "SELECT COUNT(*) FROM aircraft_sale_listings WHERE id = ?",
        listing_id
    )? != 1
    {
        return Err(ListingStoreError::NotFound(format!(
            "listing {listing_id} not found"
        )));
    }
    Ok(listing_aircraft_spec_is_pending(db, listing_id).await?
        || listing_missing_avionics_metadata_count(db, listing_id).await? > 0
        || listing_model_year_factory_reference_is_pending(db, listing_id).await?)
}

async fn listing_aircraft_spec_is_pending(db: &AppDb, listing_id: i64) -> StoreResult<bool> {
    Ok(query_scalar_one!(
        db,
        i64,
        r#"
        SELECT COUNT(*)
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        WHERE listing.id = ?
          AND NOT EXISTS (
            SELECT 1
            FROM aircraft_model_spec_versions spec
            WHERE spec.aircraft_model_id = variant.aircraft_model_id
              AND spec.aircraft_model_variant_id = variant.id
              AND spec.configuration_scope = 'factory_default'
              AND spec.is_valuation_eligible = TRUE
              AND spec.source_confidence = 'high'
              AND spec.evidence_kind = 'authoritative_reference'
          )
        "#,
        listing_id
    )? > 0)
}

async fn listing_model_year_factory_reference_is_pending(
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
        SELECT model.id AS aircraft_model_id
        FROM aircraft_sale_listings listing
        JOIN aircraft_model_variants variant
          ON variant.id = listing.aircraft_model_variant_id
        JOIN aircraft_models model
          ON model.id = variant.aircraft_model_id
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

async fn approved_catalog_avionics_graph_key(
    db: &AppDb,
    avionics_model_id: i64,
) -> StoreResult<String> {
    let (manufacturer_identity_id, product_key) = query_as_optional!(
        db,
        (i64, String),
        r#"
        SELECT avionics_manufacturer_identity_id, canonical_product_key
        FROM avionics_approved_product_graph_identities
        WHERE avionics_model_id = ?
        "#,
        avionics_model_id
    )?
    .ok_or_else(|| {
        ListingStoreError::Validation(format!(
            "approved avionics catalog id {avionics_model_id} has no stable product identity"
        ))
    })?;
    approved_avionics_product_key(manufacturer_identity_id, &product_key)
        .map_err(ListingStoreError::Validation)
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
        canonical_identity_key: String,
        replacement_identity_key: Option<String>,
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
        let canonical_identity_key =
            approved_catalog_avionics_graph_key(db, avionics_model_id).await?;
        let (replaces_avionics_model_id, replacement_identity_key) = match item
            .configuration_action
            .as_str()
        {
            "installed" if item.replaces.is_none() && item.replaces_avionics_model_id.is_none() => {
                (None, None)
            }
            "replaces" | "removes" => {
                let replaced = item.replaces.as_ref().ok_or_else(|| {
                    ListingStoreError::Validation(
                        "replacement/removal avionics requires a canonical catalog identity"
                            .to_string(),
                    )
                })?;
                let replaced_id = validated_catalog_avionics_model_id(
                    db,
                    item.replaces_avionics_model_id.ok_or_else(|| {
                        ListingStoreError::Validation(
                            "replacement/removal avionics must resolve to a catalog id".to_string(),
                        )
                    })?,
                    &replaced.manufacturer,
                    &replaced.model,
                    &replaced.avionics_types,
                )
                .await?;
                (
                    Some(replaced_id),
                    Some(approved_catalog_avionics_graph_key(db, replaced_id).await?),
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
            canonical_identity_key,
            replacement_identity_key,
        });
    }

    let mut canonical_actions = Vec::with_capacity(prepared.len());
    for item in &prepared {
        match item.configuration_action.as_str() {
            "installed" => {}
            "replaces" => {
                let target = item.replaces_avionics_model_id.ok_or_else(|| {
                    ListingStoreError::Validation(
                        "replacement avionics requires a displaced catalog product".to_string(),
                    )
                })?;
                if target == item.avionics_model_id {
                    return Err(ListingStoreError::Validation(format!(
                        "catalog product {} cannot replace itself",
                        item.avionics_model_id
                    )));
                }
            }
            "removes" => {
                let target = item.replaces_avionics_model_id.ok_or_else(|| {
                    ListingStoreError::Validation(
                        "removed avionics requires its displaced catalog product".to_string(),
                    )
                })?;
                if target != item.avionics_model_id {
                    return Err(ListingStoreError::Validation(format!(
                        "removal action must identify catalog product {} as both subject and displaced product",
                        item.avionics_model_id
                    )));
                }
            }
            _ => unreachable!("configuration actions were validated above"),
        }
        canonical_actions.push(CanonicalAvionicsAction::new(
            item.canonical_identity_key.clone(),
            item.configuration_action.clone(),
            item.replacement_identity_key.clone(),
        ));
    }
    validate_canonical_avionics_actions(&canonical_actions)
        .map_err(ListingStoreError::Validation)?;

    let lock_listing_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE aircraft_sale_listings SET updated_at = updated_at WHERE id = ? RETURNING id",
        ),
        DatabaseBackend::Postgres(_) => {
            db.sql("SELECT id FROM aircraft_sale_listings WHERE id = ? FOR UPDATE")
        }
    };
    let lock_reuse_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql("SELECT 1"),
        DatabaseBackend::Postgres(_) => db.sql(
            r#"
            LOCK TABLE
              avionics_models,
              avionics_model_types,
              avionics_types,
              avionics_manufacturers,
              avionics_approved_product_identities,
              avionics_product_reuse_attestations,
              avionics_authoritative_source_origins,
              avionics_authoritative_source_origin_revocations
            IN SHARE ROW EXCLUSIVE MODE
            "#,
        ),
    };
    let lock_listing_children = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql("SELECT 1"),
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_LISTING_CHILD_LOCK_SQL),
    };
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
        ($pool:expr, $reuse_is_current:path) => {{
            let mut transaction = $pool.begin().await?;
            if matches!(db.backend(), DatabaseBackend::Postgres(_)) {
                sqlx::query(&lock_reuse_sql)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(&lock_listing_children)
                    .execute(&mut *transaction)
                    .await?;
            }
            let locked: Option<i64> = sqlx::query_scalar(&lock_listing_sql)
                .bind(listing_id)
                .fetch_optional(&mut *transaction)
                .await?;
            if locked.is_none() {
                return Err(ListingStoreError::Validation(format!(
                    "listing {listing_id} no longer exists"
                )));
            }
            // SQLite's no-op listing UPDATE above acquires the single writer
            // lock. PostgreSQL additionally locks every mutable dependency of
            // the attestation fingerprint before the current-state check.
            if matches!(db.backend(), DatabaseBackend::Sqlite(_)) {
                sqlx::query(&lock_reuse_sql)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(&lock_listing_children)
                    .execute(&mut *transaction)
                    .await?;
            }
            let mut required_attestations = HashSet::new();
            for item in &prepared {
                required_attestations.insert(item.avionics_model_id);
                if let Some(target) = item.replaces_avionics_model_id {
                    required_attestations.insert(target);
                }
            }
            for avionics_model_id in required_attestations {
                if !$reuse_is_current(db, &mut transaction, avionics_model_id).await? {
                    return Err(ListingStoreError::Validation(format!(
                        "avionics catalog id {avionics_model_id} is not eligible for current-policy reuse; ground and re-attest it before linking it to a listing"
                    )));
                }
            }
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
        DatabaseBackend::Sqlite(pool) => {
            replace_in_transaction!(pool, reuse_attestation_is_current_sqlite)
        }
        DatabaseBackend::Postgres(pool) => {
            replace_in_transaction!(pool, reuse_attestation_is_current_postgres)
        }
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
    use sha2::{Digest, Sha256};

    use crate::aircraft::faa::{
        require_listing_faa_admission, store_release, AircraftAdmissionError, BlockReason,
        ReleaseFixtureBuilder, ReleaseMetadata,
    };
    use crate::avionics::catalog::{
        ApprovedAvionicsIdentity, AvionicsIdentityOutcome, CatalogError,
    };
    use crate::avionics::manufacturer::{
        ensure_manufacturer_identity, ManufacturerIdentityEvidence,
    };
    use crate::avionics::reuse::{
        refresh_reuse_attestation_sqlite, reuse_attestation_is_current_sqlite,
    };
    use crate::db::{AppDb, DatabaseBackend};
    use crate::extract::preview_manual_listing;
    use crate::listing::review::{
        stage_pending_review, ListingAssociationRole, PendingReviewAspect, ReviewProduct,
    };
    use crate::models::ParsedAvionics;

    use super::{
        coalesce_resolved_listing_avionics, listing_avionics_identity_resolution,
        listing_avionics_value_from_catalog, replace_listing_avionics,
        resolve_listing_avionics_values, ListingAvionicsIdentityResolution, ListingAvionicsValue,
        ListingValues, AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON,
        AVIONICS_INSTALLATION_EVIDENCE_NOT_HIGH_REASON,
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
        let release = ReleaseFixtureBuilder::from_csv(
            ReleaseMetadata::official("2026-07-20", format!("{digest_seed:064x}")),
            Cursor::new(master),
            Cursor::new(FAA_AIRCRAFT_REFERENCE),
            Cursor::new(FAA_ENGINE_REFERENCE),
            [n_number],
        )
        .expect("test FAA release should parse");
        store_release(db, &release)
            .await
            .expect("test FAA release should store");
    }

    async fn seed_blank_identity_listing(db: &AppDb, user_id: i64, source_url: &str) -> i64 {
        let variant_id = super::pending_aircraft_compatibility_variant_id(db)
            .await
            .expect("pending compatibility variant should exist");
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

    async fn seed_curated_test_aircraft_catalog(db: &AppDb, user_id: i64) -> i64 {
        seed_curated_test_aircraft_catalog_with_family(db, user_id, "182").await
    }

    async fn seed_curated_test_aircraft_catalog_with_family(
        db: &AppDb,
        user_id: i64,
        family: &str,
    ) -> i64 {
        let variant_id = super::pending_aircraft_compatibility_variant_id(db)
            .await
            .expect("pending compatibility variant should exist");
        let staging_listing_id = query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours,
              registration_number, serial_number, ingestion_state
            ) VALUES (?, ?, 'https://example.test/curated-aircraft-stage', 2023,
                      1000, 0, 'N123T', 'TESTSERIAL', 'incomplete')
            RETURNING id
            "#,
            variant_id,
            user_id
        )
        .expect("curated aircraft staging listing should seed");
        let mut raw_identity = listing_values_with_variant("182T");
        raw_identity.model = family.to_string();
        raw_identity.source_url = Some("https://example.test/curated-aircraft-stage".to_string());
        raw_identity.registration_number = Some("N123T".to_string());
        raw_identity.serial_number = Some("TESTSERIAL".to_string());
        super::stage_literal_aircraft_identity_observation(db, staging_listing_id, &raw_identity)
            .await
            .expect("raw aircraft identity input should stage");
        let grounding = require_listing_faa_admission(db, staging_listing_id)
            .await
            .expect("curated aircraft staging listing should be FAA admitted");
        crate::aircraft::identity::seed_test_curated_identity_assignment(
            db,
            staging_listing_id,
            &grounding,
        )
        .await
        .expect("exact approved test aircraft identity should seed");
        let projected_variant_id = query_scalar_one!(
            db,
            i64,
            "SELECT aircraft_model_variant_id FROM aircraft_sale_listings WHERE id = ?",
            staging_listing_id
        )
        .expect("projected compatibility variant should load");
        execute_query!(
            db,
            "DELETE FROM aircraft_sale_listings WHERE id = ?",
            staging_listing_id
        )
        .expect("curated aircraft staging listing should be removed");
        projected_variant_id
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

        let pending = resolve_listing_avionics_values(
            &db,
            &mut values,
            None,
            Some("https://example.com/listing"),
            None,
        )
        .await
        .expect("unknown equipment should be staged for explicit review");
        let pending = pending.pending_review_aspects;
        assert_eq!(pending.len(), 2);
        assert!(pending
            .iter()
            .any(|aspect| aspect.label.contains("Imaginary 999")));
        assert!(pending.iter().all(|aspect| aspect
            .reason
            .contains("Automated product verification could not complete safely")));
        assert!(values.avionics.is_empty());
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
    async fn normalized_attested_label_records_the_resolved_product_without_typography_rematching()
    {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GTX 345R", "Transponder")
                .await
                .expect("approved graph identity should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(parsed_avionics(
            "GTX-345R",
        ))];

        let pending = resolve_listing_avionics_values(
            &db,
            &mut values,
            None,
            Some("https://example.com/listing"),
            Some("The aircraft has a Garmin GTX-345R transponder installed."),
        )
        .await
        .expect("the normalized label should resolve locally without Gemini");

        assert!(pending.pending_review_aspects.is_empty());
        assert_eq!(pending.occurrence_dispositions.len(), 1);
        assert_eq!(
            pending.occurrence_dispositions[0].avionics_model_id,
            Some(approved_id)
        );
        assert_eq!(values.avionics.len(), 1);
        assert_eq!(values.avionics[0].avionics_model_id, Some(approved_id));
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM avionics_product_reuse_attestations
                 WHERE avionics_model_id = ?",
                approved_id
            )
            .expect("reuse attestation count should load"),
            1,
            "local association must reuse the existing product attestation"
        );
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[test]
    fn approved_product_requires_exactly_high_listing_installation_evidence() {
        for source_confidence in [None, Some("medium"), Some("low")] {
            let resolution = listing_avionics_identity_resolution::<CatalogError>(
                Ok(AvionicsIdentityOutcome::Approved(
                    approved_avionics_identity(),
                )),
                source_confidence,
            );
            let ListingAvionicsIdentityResolution::Pending {
                reason,
                suggested_product,
            } = resolution
            else {
                panic!("weak listing evidence must remain pending")
            };
            assert_eq!(reason, AVIONICS_INSTALLATION_EVIDENCE_NOT_HIGH_REASON);
            let suggested_product =
                suggested_product.expect("the verified catalog identity should be suggested");
            assert_eq!(suggested_product.id, Some(42));
            assert_eq!(suggested_product.manufacturer, "Garmin");
            assert_eq!(suggested_product.model, "GTX 345R");
            assert_eq!(
                suggested_product
                    .stable_identifier
                    .expect("verified identifier should be retained")
                    .value,
                "011-03520-00"
            );
        }

        let high = listing_avionics_identity_resolution::<CatalogError>(
            Ok(AvionicsIdentityOutcome::Approved(
                approved_avionics_identity(),
            )),
            Some("high"),
        );
        assert!(matches!(
            high,
            ListingAvionicsIdentityResolution::Approved(identity) if identity.id == 42
        ));
    }

    #[test]
    fn provider_and_catalog_failures_become_safe_pending_reviews() {
        for error in [
            CatalogError::Gemini("provider response included sensitive details".to_string()),
            CatalogError::Validation("catalog response violated an invariant".to_string()),
            CatalogError::Database("database driver internals".to_string()),
        ] {
            let resolution = listing_avionics_identity_resolution(Err(error), Some("high"));
            let ListingAvionicsIdentityResolution::Pending {
                reason,
                suggested_product,
            } = resolution
            else {
                panic!("automated verification failures must remain pending")
            };
            assert_eq!(reason, AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON);
            assert!(suggested_product.is_none());
            assert!(!reason.contains("sensitive"));
            assert!(!reason.contains("driver"));
        }

        let rejection = listing_avionics_identity_resolution::<CatalogError>(
            Ok(AvionicsIdentityOutcome::Rejected {
                reason: "grounded rejection".to_string(),
            }),
            Some("high"),
        );
        assert!(matches!(
            rejection,
            ListingAvionicsIdentityResolution::Rejected { reason }
                if reason == "grounded rejection"
        ));
    }

    #[tokio::test]
    async fn provider_failure_stages_the_observation_instead_of_aborting_the_listing() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let extractor =
            crate::extract::GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(parsed_avionics(
            "GTX 345R",
        ))];

        let pending = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&extractor),
            Some("https://example.com/listing"),
            Some("GTX 345R installed"),
        )
        .await
        .expect("provider failure should become review work");

        let pending = pending.pending_review_aspects;
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].reason,
            AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON
        );
        assert!(pending[0].suggested_product.is_none());
        assert!(values.avionics.is_empty());
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
    async fn listing_link_writer_rechecks_attestation_after_local_resolution() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .expect("pending compatibility variant should exist");
        let listing_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, model_year,
              asking_price_usd, airframe_hours
            ) VALUES (?, 1, 2020, 300000, 1000)
            RETURNING id
            "#,
        )
        .bind(variant_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GTX 345R", "Transponder")
                .await
                .unwrap();
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, model_id).await;
        let mut approved = approved_avionics_identity();
        approved.id = model_id;
        let resolved = listing_avionics_value_from_catalog(
            &ListingAvionicsValue::from_parsed(parsed_avionics("GTX 345R")),
            &approved,
        );

        // Simulate a revocation/invalidation that commits after local
        // resolution but before the atomic listing-link write.
        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();
        let error = replace_listing_avionics(&db, listing_id, &[resolved])
            .await
            .expect_err("the storage boundary must reject stale local reuse");
        assert!(error
            .to_string()
            .contains("not eligible for current-policy reuse"));
        let link_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(link_count, 0);
    }

    #[tokio::test]
    async fn listing_read_exposes_all_types_once_for_one_physical_product() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .expect("pending compatibility variant should exist");
        let listing_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listings (aircraft_model_variant_id, created_by_user_id, model_year, asking_price_usd, airframe_hours) VALUES (?, 1, 2020, 300000, 1000) RETURNING id",
        )
        .bind(variant_id)
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
            grounded_claim_source_urls: Vec::new(),
        }
    }

    async fn ensure_approved_test_avionics_model(
        db: &AppDb,
        manufacturer: &str,
        model: &str,
        avionics_type: &str,
    ) -> super::StoreResult<i64> {
        let id = super::ensure_avionics_model(db, manufacturer, model, avionics_type).await?;
        let manufacturer_id = query_scalar_one!(
            db,
            i64,
            "SELECT avionics_manufacturer_id FROM avionics_models WHERE id = ?",
            id
        )?;
        ensure_manufacturer_identity(
            db,
            manufacturer_id,
            &ManufacturerIdentityEvidence {
                source_url: "https://manufacturer.example/aviation/".to_string(),
                source_title: "Manufacturer aviation catalog".to_string(),
                evidence_text:
                    "The authoritative manufacturer catalog identifies the test manufacturer."
                        .to_string(),
            },
        )
        .await
        .expect("test manufacturer identity should seed");
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

    async fn attest_approved_test_avionics_model_for_current_policy_reuse(
        db: &AppDb,
        avionics_model_id: i64,
    ) {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origins (
              authority_kind, avionics_manufacturer_identity_id, https_origin,
              evidence_source_url, evidence_source_title, evidence_text,
              approval_basis, approval_reason
            )
            SELECT
              'manufacturer_primary',
              product_identity.avionics_manufacturer_identity_id,
              'https://manufacturer.example',
              'https://manufacturer.example/manuals/test.pdf',
              'Manufacturer test product manual',
              'The first-party manufacturer manual identifies the exact approved test product.',
              'curated_bootstrap',
              'Test fixture for exact manufacturer source authority'
            FROM avionics_approved_product_identities product_identity
            WHERE product_identity.avionics_model_id = ?
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(avionics_model_id)
        .execute(pool)
        .await
        .expect("current-policy source origin should seed");

        let mut transaction = pool
            .begin()
            .await
            .expect("reuse-attestation transaction should start");
        assert!(
            refresh_reuse_attestation_sqlite(
                db,
                &mut transaction,
                avionics_model_id,
                "https://manufacturer.example/manuals/test.pdf",
            )
            .await
            .expect("current-policy reuse attestation should refresh"),
            "approved fixture must satisfy the complete current-policy reuse contract"
        );
        assert!(
            reuse_attestation_is_current_sqlite(db, &mut transaction, avionics_model_id)
                .await
                .expect("current-policy reuse attestation should validate")
        );
        transaction
            .commit()
            .await
            .expect("reuse attestation should commit");
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
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .expect("pending compatibility variant should exist");
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

    async fn assert_signed_capture_drift_rolls_back_without_deleting_same_source_listing(
        drift_install_key: bool,
    ) {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let source_url = if drift_install_key {
            "https://example.test/replay-race/install-key"
        } else {
            "https://example.test/replay-race/rendered-bytes"
        };
        let rendered_html = "<html>exact signed capture</html>";
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        let checkpoint = r#"{"checkpoint":"exact"}"#;
        let checkpoint_sha256 = format!("{:x}", Sha256::digest(checkpoint.as_bytes()));
        let install_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO plugin_installs (user_id, public_key_base64)
            VALUES (?, 'exact-public-key')
            RETURNING id
            "#,
            user.id
        )
        .expect("install should seed");
        let submission_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, extracted_listing_json
            ) VALUES (?, ?, ?, ?, ?, 'exact-signature', ?)
            RETURNING id
            "#,
            user.id,
            install_id,
            source_url,
            rendered_html,
            rendered_html_sha256.as_str(),
            checkpoint
        )
        .expect("submission should seed");
        let submitted_at = query_scalar_one!(
            &db,
            String,
            "SELECT submitted_at FROM plugin_submissions WHERE id = ?",
            submission_id
        )
        .expect("submission timestamp should load");
        let binding = super::SignedSourceListingBinding {
            submission_id,
            user_id: user.id,
            plugin_install_id: install_id,
            install_public_key_base64: "exact-public-key".to_string(),
            install_revoked_at: None,
            source_url: source_url.to_string(),
            submitted_at,
            rendered_html: rendered_html.to_string(),
            rendered_html_sha256,
            signature_base64: "exact-signature".to_string(),
            expected_extracted_listing_json: Some(checkpoint.to_string()),
            expected_extracted_listing_sha256: Some(checkpoint_sha256.clone()),
            expected_extraction_error: None,
            bound_extracted_listing_json: checkpoint.to_string(),
            bound_extracted_listing_sha256: checkpoint_sha256,
        };

        // This is the harmful race compensation used to mishandle: an ordinary
        // same-user/source listing appears after replay preflight but before the
        // exact capture bind. It is never a rollback target.
        let ordinary_listing_id = seed_blank_identity_listing(&db, user.id, source_url).await;
        if drift_install_key {
            execute_query!(
                &db,
                "UPDATE plugin_installs SET public_key_base64 = 'rotated-key' WHERE id = ?",
                install_id
            )
            .expect("install key drift should seed");
        } else {
            execute_query!(
                &db,
                "UPDATE plugin_submissions SET rendered_html = '<html>changed bytes</html>' WHERE id = ?",
                submission_id
            )
            .expect("rendered-byte drift should seed");
        }
        let mut values = listing_values_with_variant("182T");
        values.source_url = Some(source_url.to_string());
        let literal_values = values.clone();
        let error = super::insert_listing(
            &db,
            user.id,
            &values,
            &literal_values,
            false,
            Some(&binding),
        )
        .await
        .expect_err("capture drift must reject the atomic bind");
        assert!(matches!(error, super::ListingStoreError::State(_)));

        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listings WHERE source_url = ?",
                source_url
            )
            .expect("same-source listing count should load"),
            1
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT id FROM aircraft_sale_listings WHERE source_url = ?",
                source_url
            )
            .expect("ordinary listing should remain"),
            ordinary_listing_id
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                Option<i64>,
                "SELECT canonical_listing_id FROM plugin_submissions WHERE id = ?",
                submission_id
            )
            .expect("capture binding should remain queryable"),
            None
        );
    }

    #[tokio::test]
    async fn signed_capture_rendered_byte_drift_rolls_back_without_compensation_deletion() {
        assert_signed_capture_drift_rolls_back_without_deleting_same_source_listing(false).await;
    }

    #[tokio::test]
    async fn signed_capture_install_key_drift_rolls_back_without_compensation_deletion() {
        assert_signed_capture_drift_rolls_back_without_deleting_same_source_listing(true).await;
    }

    #[tokio::test]
    async fn routine_listing_writes_isolate_raw_aircraft_labels_from_all_catalog_evidence() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        seed_curated_test_aircraft_catalog(&db, user.id).await;

        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Raw Duplicate Maker",
            "model": "Raw Duplicate Family",
            "variant": "Raw Duplicate Variant",
            "model_year": 2023,
            "asking_price_usd": 525000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "N123T",
            "serial_number": "TESTSERIAL",
            "avionics": []
        }));
        preview.source_url = Some("https://example.test/listing/raw-aircraft-input".to_string());
        preview.parsed_listing.avionics = vec![parsed_avionics("Imaginary 999")];

        let created = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect("raw aircraft labels should remain review input, not catalog identities");
        assert_eq!(created.ingestion_state, "pending_review");
        assert_ne!(created.aircraft.manufacturer, "Raw Duplicate Maker");
        assert_ne!(created.aircraft.model, "Raw Duplicate Family");
        assert_ne!(created.aircraft.variant, "Raw Duplicate Variant");
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                r#"
                SELECT COUNT(*)
                FROM aircraft_valuation_compatibility_projections
                WHERE aircraft_model_variant_id = ?
                "#,
                created.aircraft_model_variant_id
            )
            .expect("projection membership should load"),
            1
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                r#"
                SELECT COUNT(*)
                FROM (
                  SELECT normalized_name FROM aircraft_manufacturers
                  UNION ALL
                  SELECT normalized_name FROM aircraft_models
                  UNION ALL
                  SELECT normalized_name FROM aircraft_model_variants
                ) catalog_label
                WHERE normalized_name IN (
                  'raw duplicate maker',
                  'raw duplicate family',
                  'raw duplicate variant'
                )
                "#
            )
            .expect("raw catalog label count should load"),
            0
        );
        let first_input = query_as_optional!(
            &db,
            (String, String, String, String),
            r#"
            SELECT observed_make, observed_family, observed_designation, input_json
            FROM aircraft_listing_identity_input_observations
            WHERE aircraft_sale_listing_id = ?
            ORDER BY id DESC
            LIMIT 1
            "#,
            created.id
        )
        .expect("raw input observation should load")
        .expect("raw input observation should exist");
        assert_eq!(first_input.0, "Raw Duplicate Maker");
        assert_eq!(first_input.1, "Raw Duplicate Family");
        assert_eq!(first_input.2, "Raw Duplicate Variant");
        assert!(first_input
            .3
            .contains("\"observation_kind\":\"literal_listing_input\""));
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_identity_observations WHERE aircraft_sale_listing_id = ?",
                created.id
            )
            .expect("authoritative observation count should load"),
            0
        );

        let original_projection_id = created.aircraft_model_variant_id;
        let updated = super::update_listing(
            &db,
            user.id,
            created.id,
            &json!({
                "manufacturer": "Second Raw Duplicate Maker",
                "model": "Second Raw Duplicate Family",
                "variant": "Second Raw Duplicate Variant",
                "avionics": [{
                    "manufacturer": "Garmin",
                    "model": "Imaginary 1000",
                    "types": ["GPS"],
                    "source_evidence_text": "Imaginary 1000 installed",
                    "source_confidence": "high"
                }]
            }),
            None,
        )
        .await
        .expect("routine update should restage raw input without changing canonical identity");
        assert_eq!(updated.ingestion_state, "pending_review");
        assert_eq!(
            updated.aircraft_model_variant_id, original_projection_id,
            "raw update fields must not select or create a different aircraft variant"
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                r#"
                SELECT COUNT(*)
                FROM (
                  SELECT normalized_name FROM aircraft_manufacturers
                  UNION ALL
                  SELECT normalized_name FROM aircraft_models
                  UNION ALL
                  SELECT normalized_name FROM aircraft_model_variants
                ) catalog_label
                WHERE normalized_name IN (
                  'raw duplicate maker',
                  'raw duplicate family',
                  'raw duplicate variant',
                  'second raw duplicate maker',
                  'second raw duplicate family',
                  'second raw duplicate variant'
                )
                "#
            )
            .expect("updated raw catalog label count should load"),
            0
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_listing_identity_input_observations WHERE aircraft_sale_listing_id = ?",
                created.id
            )
            .expect("input observation count should load"),
            2
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_identity_observations WHERE aircraft_sale_listing_id = ?",
                created.id
            )
            .expect("authoritative observation count should reload"),
            0
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                r#"
                SELECT COUNT(*)
                FROM curation_evidence_claims
                WHERE lower(
                  subject_text || ' ' || predicate_text || ' ' ||
                  object_text || ' ' || quoted_evidence
                ) LIKE '%raw duplicate%'
                "#
            )
            .expect("raw authoritative claim count should load"),
            0
        );
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
        // Seed the curated catalog through the same FAA-backed assignment and
        // compatibility-projection boundary used by production.
        let variant_id =
            seed_curated_test_aircraft_catalog_with_family(&db, user.id, "182 Skylane").await;
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
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_model_spec_versions (
              aircraft_model_id, aircraft_model_variant_id, effective_from,
              source_url, configuration_scope, source_confidence,
              evidence_kind, is_valuation_eligible
            )
            SELECT
              variant.aircraft_model_id, variant.id, '2023-01-01',
              'https://manufacturer.example/aircraft-spec',
              'factory_default', 'high', 'authoritative_reference', TRUE
            FROM aircraft_model_variants variant
            WHERE variant.id = ?
            "#,
            variant_id
        )
        .expect("valuation-ready aircraft spec should seed");
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
        assert_eq!(listing.aircraft.manufacturer, "CESSNA AIRCRAFT CO");
        assert_eq!(listing.aircraft.model, "182 Skylane");
        assert_eq!(listing.aircraft.variant, "182T");
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
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .expect("pending compatibility variant should exist");
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
        assert!(matches!(
            error,
            super::ListingStoreError::AircraftAdmission(AircraftAdmissionError::Rejected {
                listing_id: None,
                reason: BlockReason::NonNRegistration,
                ..
            })
        ));
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
        let source_url = "https://example.test/listing/identity-recovery";
        let legacy_listing_id = seed_blank_identity_listing(&db, user.id, source_url).await;

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
    async fn source_identity_repair_survives_pending_avionics_review() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        seed_curated_test_aircraft_catalog(&db, user.id).await;
        let source_url = "https://example.test/listing/identity-before-enrichment";
        let listing_id = seed_blank_identity_listing(&db, user.id, source_url).await;
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

        let after = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect("unresolved avionics should be retained for review");

        assert_eq!(after.id, listing_id);
        assert_eq!(after.registration_number.as_deref(), Some("N123T"));
        assert_eq!(after.serial_number.as_deref(), Some("TESTSERIAL"));
        assert_eq!(after.ingestion_state, "pending_review");
        assert_eq!(after.ingestion_error, None);
        assert!(!after.is_verified);
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT count(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
                listing_id
            )
            .expect("pending review count should load"),
            1
        );
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT count(*) FROM aircraft_sale_listings")
                .expect("listing count should load"),
            1
        );
    }

    #[tokio::test]
    async fn ordinary_patch_preserves_pending_review_until_avionics_are_explicitly_replaced() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        seed_curated_test_aircraft_catalog(&db, user.id).await;
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
            "serial_number": "TESTSERIAL",
            "avionics": []
        }));
        preview.source_url = Some("https://example.test/listing/pending-patch".to_string());
        preview.parsed_listing.avionics = vec![parsed_avionics("Imaginary 999")];

        let pending = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect("unresolved avionics should stage a review");
        assert_eq!(pending.ingestion_state, "pending_review");

        let linked_model_id = ensure_approved_test_avionics_model(&db, "Garmin", "GNS 430W", "GPS")
            .await
            .expect("covered catalog product should seed");
        let link_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, configuration_action, source_confidence
            ) VALUES (?, ?, 2, 'listing', 'legacy GNS 430W evidence',
                      'installed', 'low')
            RETURNING id
            "#,
            pending.id,
            linked_model_id
        )
        .expect("covered listing link should seed");
        let covered_aspect = PendingReviewAspect::avionics(
            "covered-legacy-link",
            "avionics",
            "Garmin GNS 430W",
            "Garmin GNS 430W installed",
            "legacy association requires reviewer corroboration",
            2,
            "installed",
            Some("legacy GNS 430W evidence".to_string()),
            Some("low".to_string()),
        )
        .with_suggested_product(ReviewProduct::verified(
            linked_model_id,
            "Garmin",
            "GNS 430W",
            vec!["GPS".to_string()],
        ))
        .with_covered_association(
            link_id,
            ListingAssociationRole::Installed,
            linked_model_id,
        );
        stage_pending_review(&db, pending.id, None, &[covered_aspect])
            .await
            .expect("covered link review should replace the initial unresolved bundle");

        let review_before = query_as_optional!(
            &db,
            (String, String, i64, String, String),
            r#"
            SELECT extraction_sha256, catalog_revision_sha256,
                   pending_aspect_count, review_payload_json,
                   review_payload_sha256
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
            "#,
            pending.id
        )
        .expect("pending review should load")
        .expect("pending review should exist");
        let link_before = query_as_optional!(
            &db,
            (
                i64,
                i64,
                i64,
                i64,
                String,
                Option<String>,
                String,
                Option<i64>,
                Option<String>,
                String,
                String
            ),
            r#"
            SELECT id, aircraft_sale_listing_id, avionics_model_id, quantity,
                   source, source_notes, configuration_action,
                   replaces_avionics_model_id, source_confidence,
                   created_at, updated_at
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
            link_id
        )
        .expect("covered listing link should load")
        .expect("covered listing link should exist");

        let updated = super::update_listing(
            &db,
            user.id,
            pending.id,
            &json!({"asking_price_usd": 515000, "status": "pending"}),
            None,
        )
        .await
        .expect("ordinary patch should preserve unresolved review evidence");
        assert_eq!(updated.asking_price_usd, 515000.0);
        assert_eq!(updated.status, "pending");
        assert_eq!(updated.ingestion_state, "pending_review");
        assert!(!updated.is_verified);
        assert_eq!(updated.aircraft, pending.aircraft);
        assert_eq!(
            updated.aircraft_model_variant_id,
            pending.aircraft_model_variant_id
        );
        assert_eq!(updated.model_year, pending.model_year);
        assert_eq!(updated.source_url, pending.source_url);
        assert_eq!(updated.registration_number, pending.registration_number);
        assert_eq!(updated.serial_number, pending.serial_number);

        let review_after_ordinary_patch = query_as_optional!(
            &db,
            (String, String, i64, String, String),
            r#"
            SELECT extraction_sha256, catalog_revision_sha256,
                   pending_aspect_count, review_payload_json,
                   review_payload_sha256
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
            "#,
            pending.id
        )
        .expect("pending review should reload")
        .expect("ordinary patch must not clear the pending review");
        assert_eq!(review_after_ordinary_patch, review_before);
        let link_after_ordinary_patch = query_as_optional!(
            &db,
            (
                i64,
                i64,
                i64,
                i64,
                String,
                Option<String>,
                String,
                Option<i64>,
                Option<String>,
                String,
                String
            ),
            r#"
            SELECT id, aircraft_sale_listing_id, avionics_model_id, quantity,
                   source, source_notes, configuration_action,
                   replaces_avionics_model_id, source_confidence,
                   created_at, updated_at
            FROM aircraft_sale_listing_avionics
            WHERE id = ?
            "#,
            link_id
        )
        .expect("covered listing link should reload")
        .expect("ordinary patch must not replace the covered listing link");
        assert_eq!(link_after_ordinary_patch, link_before);

        let listing_before_invalid_patches = super::get_listing(&db, user.id, pending.id)
            .await
            .expect("listing should load before rejected patches");
        let invalid_patches = [
            ("null avionics", json!({"avionics": null})),
            ("object avionics", json!({"avionics": {}})),
            ("malformed avionics entry", json!({"avionics": [{}]})),
            ("manufacturer context", json!({"manufacturer": "Piper"})),
            ("model context", json!({"model": "PA-28"})),
            ("variant context", json!({"variant": "Archer"})),
            ("model-year context", json!({"model_year": 2022})),
            (
                "source context",
                json!({"source_url": "https://example.test/listing/different"}),
            ),
            (
                "registration context",
                json!({"registration_number": "N999X"}),
            ),
            ("serial context", json!({"serial_number": "DIFFERENT"})),
        ];
        for (case, invalid_patch) in invalid_patches {
            let error = super::update_listing(&db, user.id, pending.id, &invalid_patch, None)
                .await
                .expect_err("invalid avionics/context patch must fail before mutation");
            assert!(
                matches!(error, super::ListingStoreError::Validation(_)),
                "{case} returned {error}"
            );

            let review_after_invalid_patch = query_as_optional!(
                &db,
                (String, String, i64, String, String),
                r#"
                SELECT extraction_sha256, catalog_revision_sha256,
                       pending_aspect_count, review_payload_json,
                       review_payload_sha256
                FROM aircraft_sale_listing_pending_reviews
                WHERE listing_id = ?
                "#,
                pending.id
            )
            .expect("pending review should reload after rejected patch")
            .expect("rejected patch must not clear the pending review");
            assert_eq!(review_after_invalid_patch, review_before, "{case}");
            assert_eq!(
                super::get_listing(&db, user.id, pending.id)
                    .await
                    .expect("listing should reload after rejected patch"),
                listing_before_invalid_patches,
                "{case}"
            );
            let link_after_invalid_patch = query_as_optional!(
                &db,
                (
                    i64,
                    i64,
                    i64,
                    i64,
                    String,
                    Option<String>,
                    String,
                    Option<i64>,
                    Option<String>,
                    String,
                    String
                ),
                r#"
                SELECT id, aircraft_sale_listing_id, avionics_model_id, quantity,
                       source, source_notes, configuration_action,
                       replaces_avionics_model_id, source_confidence,
                       created_at, updated_at
                FROM aircraft_sale_listing_avionics
                WHERE id = ?
                "#,
                link_id
            )
            .expect("covered link should reload after rejected patch")
            .expect("rejected patch must not replace the covered link");
            assert_eq!(link_after_invalid_patch, link_before, "{case}");
        }

        let restaged = super::update_listing(
            &db,
            user.id,
            pending.id,
            &json!({
                "source_url": "https://example.test/listing/pending-patch-restaged",
                "avionics": [{
                    "manufacturer": "Garmin",
                    "model": "Imaginary 1000",
                    "types": ["Transponder"],
                    "quantity": 1,
                    "configuration_action": "installed",
                    "source_evidence_text": "Imaginary 1000 installed",
                    "source_confidence": "high"
                }]
            }),
            None,
        )
        .await
        .expect("explicit avionics replacement should restage the review");
        assert_eq!(restaged.ingestion_state, "pending_review");
        assert_eq!(
            restaged.source_url.as_deref(),
            Some("https://example.test/listing/pending-patch-restaged")
        );

        let review_after_explicit_replacement = query_as_optional!(
            &db,
            (i64, String, String),
            r#"
            SELECT pending_aspect_count, review_payload_json, review_payload_sha256
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
            "#,
            pending.id
        )
        .expect("restaged review should load")
        .expect("explicit avionics replacement should retain a review");
        assert_eq!(review_after_explicit_replacement.0, 1);
        assert!(review_after_explicit_replacement
            .1
            .contains("Imaginary 1000"));
        assert!(!review_after_explicit_replacement.1.contains("GNS 430W"));
        assert_ne!(review_after_explicit_replacement.2, review_before.4);
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT count(*) FROM aircraft_sale_listing_avionics WHERE id = ?",
                link_id
            )
            .expect("replaced link count should load"),
            0
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
    async fn signed_source_serial_correction_isolated_from_same_source_and_tail() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let variant_id = seed_curated_test_aircraft_catalog(&db, user.id).await;
        let source_url = "https://example.test/listing/signed-source-serial-correction";
        let existing_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, is_verified,
              source_url, model_year, asking_price_usd, currency, status,
              ingestion_state, ingestion_error, registration_number, serial_number,
              airframe_hours
            )
            VALUES (?, ?, FALSE, ?, 2020, 111111, 'USD', 'active',
                    'quarantined', 'retain exact legacy row', 'N123T',
                    'LEGACY-WRONG', 777)
            RETURNING id
            "#,
            variant_id,
            user.id,
            source_url
        )
        .expect("legacy same-source and same-tail listing should seed");
        let before = super::get_listing(&db, user.id, existing_id)
            .await
            .expect("legacy listing should load");
        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2020,
            "asking_price_usd": 400000,
            "currency": "USD",
            "airframe_hours": 900,
            "status": "active",
            "registration_number": "N123T",
            "serial_number": "TESTSERAL",
            "avionics": []
        }));
        preview.source_url = Some(source_url.to_string());
        preview.context_text = Some("Registration N123T; serial TESTSERAL".to_string());

        let ordinary_error = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect_err("ordinary listing creation must remain strict");
        assert!(ordinary_error.to_string().contains("conflict"));
        assert_eq!(
            super::get_listing(&db, user.id, existing_id)
                .await
                .expect("strict rejection must retain the existing listing"),
            before
        );

        let created = super::create_listing_with_progress_and_occurrence_dispositions(
            &db,
            user.id,
            &preview,
            None,
            None,
            None,
            super::ListingCreationMode::SignedSource,
            None,
        )
        .await
        .expect("signed source admission should isolate the corrected observation");
        assert_ne!(created.listing.id, existing_id);
        assert_eq!(created.listing.serial_number.as_deref(), Some("TESTSERIAL"));
        assert_eq!(created.listing.ingestion_state, "quarantined");
        assert_eq!(
            created.listing.ingestion_error.as_deref(),
            Some(super::SOURCE_IDENTITY_RECEIPT_PENDING)
        );
        assert!(!created.listing.is_verified);
        assert!(created.listing.ingestion_completed_at.is_none());
        let gated_before_finalize = created.listing.clone();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        assert!(sqlx::query(
            "UPDATE aircraft_sale_listings SET ingestion_state = 'incomplete', ingestion_error = NULL WHERE id = ?",
        )
        .bind(created.listing.id)
        .execute(pool)
        .await
        .is_err(), "the database must reject bypassing the receipt gate directly");
        let finalize_error = super::finalize_reviewed_listing_ingestion(
            &db,
            created.listing.id,
            None,
            preview.context_text.as_deref(),
        )
        .await
        .expect_err("a corrected listing must not finalize before its bound receipt exists");
        assert!(finalize_error
            .to_string()
            .contains("immutable source identity correction receipt"));
        assert_eq!(
            super::get_listing(&db, user.id, created.listing.id)
                .await
                .expect("receipt-gated listing should remain private"),
            gated_before_finalize
        );
        assert_eq!(
            created.source_serial_correction.as_ref().map(|correction| (
                correction.observed_serial_number.as_str(),
                correction.corrected_serial_number.as_str()
            )),
            Some(("TESTSERAL", "TESTSERIAL"))
        );
        assert_eq!(
            super::get_listing(&db, user.id, existing_id)
                .await
                .expect("legacy listing should remain byte-for-byte unchanged"),
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
    async fn different_source_pending_review_does_not_mutate_blank_listing() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        seed_curated_test_aircraft_catalog(&db, user.id).await;
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

        let pending_listing = super::create_listing(&db, user.id, &preview, None, None)
            .await
            .expect("the distinct source should be retained for avionics review");
        assert_eq!(
            super::get_listing(&db, user.id, listing_id)
                .await
                .expect("unrelated source listing should remain"),
            before
        );
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT count(*) FROM aircraft_sale_listings")
                .expect("listing count should load"),
            2
        );
        assert_ne!(pending_listing.id, listing_id);
        assert_eq!(
            pending_listing.source_url.as_deref(),
            Some("https://example.test/listing/different-source")
        );
        assert_eq!(
            pending_listing.registration_number.as_deref(),
            Some("N123T")
        );
        assert_eq!(pending_listing.serial_number.as_deref(), Some("TESTSERIAL"));
        assert_eq!(pending_listing.ingestion_state, "pending_review");
        assert_eq!(pending_listing.ingestion_error, None);
        assert!(!pending_listing.is_verified);
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT count(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
                pending_listing.id
            )
            .expect("pending review count should load"),
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
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .expect("pending compatibility variant should exist");
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
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let variant_id =
            seed_curated_test_aircraft_catalog_with_family(&db, user.id, "Readiness").await;
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
            super::listing_model_year_factory_reference_is_pending(&db, listing_id)
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
            super::listing_model_year_factory_reference_is_pending(&db, listing_id)
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
            !super::listing_model_year_factory_reference_is_pending(&db, listing_id)
                .await
                .expect("valuation-grade records should be complete")
        );
        assert!(super::listing_factory_reference_is_pending(&db, listing_id)
            .await
            .expect("missing aircraft spec should keep combined readiness pending"));

        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_model_spec_versions (
              aircraft_model_id, aircraft_model_variant_id, effective_from,
              source_url, configuration_scope, source_confidence,
              evidence_kind, is_valuation_eligible
            )
            SELECT
              variant.aircraft_model_id, variant.id, '2024-01-01',
              'https://manufacturer.example/aircraft-spec',
              'factory_default', 'high', 'authoritative_reference', TRUE
            FROM aircraft_model_variants variant
            WHERE variant.id = ?
            "#,
            variant_id
        )
        .expect("valuation-ready aircraft spec should seed");
        assert!(
            !super::listing_factory_reference_is_pending(&db, listing_id)
                .await
                .expect("all shared valuation references should be ready")
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

    #[tokio::test]
    async fn review_aircraft_identity_preparation_requires_curated_catalog_without_mutation() {
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
            "https://example.test/review-aircraft-curation-required",
        )
        .await;
        execute_query!(
            &db,
            r#"
            UPDATE aircraft_sale_listings
            SET registration_number = 'N123T', serial_number = 'TESTSERIAL'
            WHERE id = ?
            "#,
            listing_id
        )
        .expect("FAA identity should seed");

        let linked_model_id = ensure_approved_test_avionics_model(&db, "Garmin", "GNS 430W", "GPS")
            .await
            .expect("existing approved avionics should seed");
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, source_confidence, configuration_action
            ) VALUES (?, ?, 1, 'listing', 'GNS 430W installed', 'high', 'installed')
            "#,
            listing_id,
            linked_model_id
        )
        .expect("existing listing avionics should seed");
        let aspect = PendingReviewAspect::avionics(
            "unknown-unit",
            "avionics",
            "Unknown unit",
            "Unknown unit installed",
            "identity requires review",
            1,
            "installed",
            Some("Unknown unit installed".to_string()),
            Some("medium".to_string()),
        );
        stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .expect("pending review should stage");

        let review_before = query_as_optional!(
            &db,
            (String, String, String, i64),
            r#"
            SELECT extraction_sha256, catalog_revision_sha256,
                   review_payload_sha256, pending_aspect_count
            FROM aircraft_sale_listing_pending_reviews
            WHERE listing_id = ?
            "#,
            listing_id
        )
        .expect("pending review should load");
        let links_before = query_as_all!(
            &db,
            (
                i64,
                i64,
                i64,
                String,
                Option<String>,
                Option<String>,
                String
            ),
            r#"
            SELECT id, avionics_model_id, quantity, source, source_notes,
                   source_confidence, configuration_action
            FROM aircraft_sale_listing_avionics
            WHERE aircraft_sale_listing_id = ?
            ORDER BY id
            "#,
            listing_id
        )
        .expect("listing avionics should load");

        let error = super::ensure_listing_canonical_aircraft_identity(&db, listing_id)
            .await
            .expect_err("an empty aircraft catalog must require explicit curation");
        assert!(matches!(
            error,
            super::ListingStoreError::Validation(ref message)
                if message.contains("requires aircraft identity curation")
                    && message.contains("exact candidates: 0")
        ));
        assert_eq!(
            query_as_optional!(
                &db,
                (String, String, String, i64),
                r#"
                SELECT extraction_sha256, catalog_revision_sha256,
                       review_payload_sha256, pending_aspect_count
                FROM aircraft_sale_listing_pending_reviews
                WHERE listing_id = ?
                "#,
                listing_id
            )
            .expect("pending review should remain queryable"),
            review_before
        );
        assert_eq!(
            query_as_all!(
                &db,
                (
                    i64,
                    i64,
                    i64,
                    String,
                    Option<String>,
                    Option<String>,
                    String
                ),
                r#"
                SELECT id, avionics_model_id, quantity, source, source_notes,
                       source_confidence, configuration_action
                FROM aircraft_sale_listing_avionics
                WHERE aircraft_sale_listing_id = ?
                ORDER BY id
                "#,
                listing_id
            )
            .expect("listing avionics should remain queryable"),
            links_before
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_identity_assignments WHERE aircraft_sale_listing_id = ?",
                listing_id
            )
            .expect("assignment count should load"),
            0
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_current_identity_assignments WHERE aircraft_sale_listing_id = ?",
                listing_id
            )
            .expect("current assignment count should load"),
            0
        );
    }

    #[tokio::test]
    async fn initial_finalization_defers_only_catalog_curation_for_a_pending_review() {
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
            "https://example.test/review-aircraft-deferred-curation",
        )
        .await;
        execute_query!(
            &db,
            r#"
            UPDATE aircraft_sale_listings
            SET registration_number = 'N123T', serial_number = 'TESTSERIAL'
            WHERE id = ?
            "#,
            listing_id
        )
        .expect("FAA identity should seed");
        let aspect = PendingReviewAspect::avionics(
            "unknown-unit",
            "avionics",
            "Unknown unit",
            "Unknown unit installed",
            "identity requires review",
            1,
            "installed",
            Some("Unknown unit installed".to_string()),
            Some("medium".to_string()),
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .expect("pending review should stage");

        super::finalize_listing_ingestion(&db, listing_id, None, None)
            .await
            .expect("missing canonical catalog identity should remain review work");

        let state = query_as_optional!(
            &db,
            (String, Option<String>, bool, i64, String),
            r#"
            SELECT listing.ingestion_state, listing.ingestion_error,
                   listing.is_verified, review.pending_aspect_count,
                   review.review_payload_sha256
            FROM aircraft_sale_listings listing
            JOIN aircraft_sale_listing_pending_reviews review
              ON review.listing_id = listing.id
            WHERE listing.id = ?
            "#,
            listing_id
        )
        .expect("pending listing state should load")
        .expect("pending listing should exist");
        assert_eq!(state.0, "pending_review");
        assert_eq!(state.1, None);
        assert!(!state.2);
        assert_eq!(state.3, 1);
        assert_eq!(state.4, staged.review_payload_sha256);
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_identity_assignments WHERE aircraft_sale_listing_id = ?",
                listing_id
            )
            .expect("assignment count should load"),
            0
        );
    }

    #[tokio::test]
    async fn initial_finalization_does_not_defer_raw_faa_rejection_for_a_pending_review() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let listing_id = seed_blank_identity_listing(
            &db,
            user.id,
            "https://example.test/review-aircraft-missing-registration",
        )
        .await;
        let aspect = PendingReviewAspect::avionics(
            "unknown-unit",
            "avionics",
            "Unknown unit",
            "Unknown unit installed",
            "identity requires review",
            1,
            "installed",
            Some("Unknown unit installed".to_string()),
            Some("medium".to_string()),
        );
        stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .expect("pending review should stage");

        let error = super::finalize_listing_ingestion(&db, listing_id, None, None)
            .await
            .expect_err("raw FAA rejection must remain an ingestion failure");
        assert!(matches!(
            error,
            super::ListingStoreError::AircraftAdmission(AircraftAdmissionError::Rejected {
                listing_id: Some(failed_listing_id),
                reason: BlockReason::MissingRegistration,
                ..
            }) if failed_listing_id == listing_id
        ));
    }

    #[tokio::test]
    async fn initial_finalization_keeps_shared_reference_work_incomplete() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        seed_curated_test_aircraft_catalog(&db, user.id).await;
        let listing_id = seed_blank_identity_listing(
            &db,
            user.id,
            "https://example.test/initial-pending-factory-reference",
        )
        .await;
        execute_query!(
            &db,
            r#"
            UPDATE aircraft_sale_listings
            SET registration_number = 'N123T',
                serial_number = 'TESTSERIAL',
                ingestion_state = 'incomplete',
                ingestion_error = 'stale failure',
                ingestion_completed_at = CURRENT_TIMESTAMP,
                is_verified = FALSE
            WHERE id = ?
            "#,
            listing_id
        )
        .expect("FAA identity should seed");

        super::finalize_listing_ingestion(&db, listing_id, None, None)
            .await
            .expect("missing shared factory reference must remain retryable");

        assert!(super::listing_factory_reference_is_pending(&db, listing_id)
            .await
            .expect("derived reference readiness should load"));
        let state = query_as_optional!(
            &db,
            (String, Option<String>, bool, Option<String>),
            r#"
            SELECT ingestion_state, ingestion_error, is_verified,
                   ingestion_completed_at
            FROM aircraft_sale_listings
            WHERE id = ?
            "#,
            listing_id
        )
        .expect("pending-reference listing state should load")
        .expect("pending-reference listing should exist");
        assert_eq!(state.0, "incomplete");
        assert_eq!(state.1, None);
        assert!(!state.2);
        assert_eq!(state.3, None);
    }

    #[tokio::test]
    async fn review_aircraft_identity_preparation_assigns_exact_catalog_idempotently() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let projected_variant_id = seed_curated_test_aircraft_catalog(&db, user.id).await;
        let listing_id = seed_blank_identity_listing(
            &db,
            user.id,
            "https://example.test/review-aircraft-exact-catalog",
        )
        .await;
        execute_query!(
            &db,
            r#"
            UPDATE aircraft_sale_listings
            SET registration_number = 'N123T', serial_number = 'TESTSERIAL'
            WHERE id = ?
            "#,
            listing_id
        )
        .expect("FAA identity should seed");
        let aspect = PendingReviewAspect::avionics(
            "unknown-unit",
            "avionics",
            "Unknown unit",
            "Unknown unit installed",
            "identity requires review",
            1,
            "installed",
            Some("Unknown unit installed".to_string()),
            Some("medium".to_string()),
        );
        let staged = stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .expect("pending review should stage");

        super::ensure_listing_canonical_aircraft_identity(&db, listing_id)
            .await
            .expect("one exact curated identity should be assigned");
        let first_assignment_id = query_scalar_one!(
            &db,
            i64,
            r#"
            SELECT identity_assignment_id
            FROM aircraft_sale_listing_current_identity_assignments
            WHERE aircraft_sale_listing_id = ?
            "#,
            listing_id
        )
        .expect("current assignment should load");
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_identity_assignments WHERE aircraft_sale_listing_id = ?",
                listing_id
            )
            .expect("assignment count should load"),
            1
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_exact_compatibility_projections WHERE listing_id = ? AND identity_assignment_id = ?",
                listing_id,
                first_assignment_id
            )
            .expect("exact projection count should load"),
            1
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT aircraft_model_variant_id FROM aircraft_sale_listings WHERE id = ?",
                listing_id
            )
            .expect("listing projection should load"),
            projected_variant_id
        );

        super::ensure_listing_canonical_aircraft_identity(&db, listing_id)
            .await
            .expect("repeated preparation should reuse the current assignment");
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                r#"
                SELECT identity_assignment_id
                FROM aircraft_sale_listing_current_identity_assignments
                WHERE aircraft_sale_listing_id = ?
                "#,
                listing_id
            )
            .expect("current assignment should remain"),
            first_assignment_id
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_identity_assignments WHERE aircraft_sale_listing_id = ?",
                listing_id
            )
            .expect("assignment count should remain stable"),
            1
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                String,
                "SELECT review_payload_sha256 FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
                listing_id
            )
            .expect("pending review should remain"),
            staged.review_payload_sha256
        );
    }

    #[tokio::test]
    async fn reviewed_finalization_is_idempotent_after_successful_publication() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        seed_curated_test_aircraft_catalog(&db, user.id).await;
        let listing_id = seed_blank_identity_listing(
            &db,
            user.id,
            "https://example.test/already-finalized-review",
        )
        .await;

        execute_query!(
            &db,
            r#"
            UPDATE aircraft_sale_listings
            SET registration_number = 'N123T',
                serial_number = 'TESTSERIAL',
                ingestion_state = 'incomplete',
                ingestion_error = NULL,
                is_verified = FALSE
            WHERE id = ?
            "#,
            listing_id
        )
        .expect("FAA identity should seed");
        super::ensure_listing_canonical_aircraft_identity(&db, listing_id)
            .await
            .expect("exact curated aircraft identity should be assigned");
        execute_query!(
            &db,
            r#"
            UPDATE aircraft_sale_listings
            SET ingestion_state = 'ready',
                ingestion_error = NULL,
                ingestion_completed_at = CURRENT_TIMESTAMP,
                is_verified = TRUE
            WHERE id = ?
            "#,
            listing_id
        )
        .expect("listing should represent an already successful finalization");

        // The fixture intentionally lacks enrichment data and there is no
        // extractor. Re-entering normal finalization would fail, so success
        // proves the idempotent ready guard returned first.
        let outcome = super::finalize_reviewed_listing_ingestion(&db, listing_id, None, None)
            .await
            .expect("an already ready verified listing should be a no-op");
        assert_eq!(outcome, super::ListingFinalizationOutcome::Ready);
    }

    #[tokio::test]
    async fn reviewed_finalization_keeps_pending_model_year_reference_incomplete() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        let variant_id = seed_curated_test_aircraft_catalog(&db, user.id).await;
        let listing_id = seed_blank_identity_listing(
            &db,
            user.id,
            "https://example.test/pending-model-year-reference",
        )
        .await;
        execute_query!(
            &db,
            r#"
            UPDATE aircraft_sale_listings
            SET registration_number = 'N123T',
                serial_number = 'TESTSERIAL',
                ingestion_state = 'incomplete',
                ingestion_error = NULL,
                ingestion_completed_at = NULL,
                is_verified = FALSE
            WHERE id = ?
            "#,
            listing_id
        )
        .expect("FAA identity should seed");
        super::ensure_listing_canonical_aircraft_identity(&db, listing_id)
            .await
            .expect("exact curated aircraft identity should be assigned");
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_model_spec_versions (
              aircraft_model_id, aircraft_model_variant_id, effective_from,
              source_url, configuration_scope, source_confidence,
              evidence_kind, is_valuation_eligible
            )
            SELECT
              variant.aircraft_model_id, variant.id, '2023-01-01',
              'https://manufacturer.example/aircraft-spec',
              'factory_default', 'high', 'authoritative_reference', TRUE
            FROM aircraft_model_variants variant
            WHERE variant.id = ?
            "#,
            variant_id
        )
        .expect("valuation-ready aircraft spec should seed");

        let outcome = super::finalize_reviewed_listing_ingestion(&db, listing_id, None, None)
            .await
            .expect("missing shared factory reference must remain retryable");
        assert!(matches!(
            outcome,
            super::ListingFinalizationOutcome::PendingReference { ref reason }
                if reason.contains("model-year factory reference")
        ));
        assert!(super::listing_factory_reference_is_pending(&db, listing_id)
            .await
            .expect("derived reference readiness should load"));

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects sqlite")
        };
        let state: (String, Option<String>, bool, Option<String>) = sqlx::query_as(
            "SELECT ingestion_state, ingestion_error, is_verified, ingestion_completed_at FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .expect("pending-reference listing should remain queryable");
        assert_eq!(state.0, "incomplete");
        assert_eq!(state.1, None);
        assert!(!state.2);
        assert_eq!(state.3, None);
    }

    #[tokio::test]
    async fn reviewed_finalization_requires_current_faa_admission_before_enrichment() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let listing_id = seed_blank_identity_listing(
            &db,
            user.id,
            "https://example.test/reviewed-without-faa-identity",
        )
        .await;

        execute_query!(
            &db,
            "UPDATE aircraft_sale_listings SET ingestion_state = 'incomplete', ingestion_error = NULL WHERE id = ?",
            listing_id
        )
        .expect("listing should enter post-review finalization state");

        let error = super::finalize_reviewed_listing_ingestion(&db, listing_id, None, None)
            .await
            .expect_err("a reviewed listing without FAA admission must not be published");
        assert!(matches!(
            error,
            super::ListingStoreError::AircraftAdmission(AircraftAdmissionError::Rejected {
                listing_id: Some(failed_listing_id),
                reason: BlockReason::MissingRegistration,
                ..
            }) if failed_listing_id == listing_id
        ));

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects sqlite")
        };
        let state: (String, Option<String>, bool, Option<String>) = sqlx::query_as(
            "SELECT ingestion_state, ingestion_error, is_verified, ingestion_completed_at FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .expect("quarantined listing should remain queryable");
        assert_eq!(state.0, "quarantined");
        assert!(state
            .1
            .as_deref()
            .is_some_and(|value| value.contains("FAA aircraft admission rejected")));
        assert!(!state.2);
        assert!(state.3.is_none());
    }

    #[tokio::test]
    async fn readiness_failure_preserves_a_bundle_that_appeared_before_quarantine() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let listing_id = seed_blank_identity_listing(
            &db,
            user.id,
            "https://example.test/review-appeared-during-finalization",
        )
        .await;
        let aspect = PendingReviewAspect::avionics(
            "concurrent-review",
            "avionics",
            "Garmin Unknown",
            "Garmin Unknown installed",
            "identity requires review",
            1,
            "installed",
            Some("Garmin Unknown installed".to_string()),
            Some("medium".to_string()),
        );
        stage_pending_review(&db, listing_id, None, &[aspect])
            .await
            .expect("concurrent review should stage");

        let readiness_error = super::mark_reviewed_listing_ready(&db, listing_id)
            .await
            .expect_err("a newly pending bundle must block readiness");
        let quarantined: super::StoreResult<()> =
            super::quarantine_after_error(&db, listing_id, readiness_error).await;
        assert!(matches!(
            quarantined,
            Err(super::ListingStoreError::Ingestion { .. })
        ));

        let state = query_as_optional!(
            &db,
            (String, Option<String>, Option<String>, bool, i64),
            r#"
            SELECT listing.ingestion_state, listing.ingestion_error,
                   listing.ingestion_completed_at, listing.is_verified,
                   COUNT(review.id)
            FROM aircraft_sale_listings listing
            LEFT JOIN aircraft_sale_listing_pending_reviews review
              ON review.listing_id = listing.id
            WHERE listing.id = ?
            GROUP BY listing.id
            "#,
            listing_id
        )
        .expect("listing state should load")
        .expect("listing should exist");
        assert_eq!(state.0, "pending_review");
        assert_eq!(state.1, None);
        assert_eq!(state.2, None);
        assert!(!state.3);
        assert_eq!(state.4, 1);
    }

    #[tokio::test]
    async fn failed_explicit_avionics_restage_clears_stale_covered_link_ids() {
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
            "https://example.test/failed-explicit-avionics-restage",
        )
        .await;
        execute_query!(
            &db,
            r#"
            UPDATE aircraft_sale_listings
            SET registration_number = 'N123T', serial_number = 'TESTSERIAL',
                ingestion_state = 'incomplete', ingestion_error = NULL
            WHERE id = ?
            "#,
            listing_id
        )
        .expect("FAA identity should seed");
        let linked_model_id = ensure_approved_test_avionics_model(&db, "Garmin", "GNS 430W", "GPS")
            .await
            .expect("covered catalog product should seed");
        let old_link_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, quantity, source,
              source_notes, configuration_action, source_confidence
            ) VALUES (?, ?, 1, 'listing', 'old covered evidence',
                      'installed', 'low')
            RETURNING id
            "#,
            listing_id,
            linked_model_id
        )
        .expect("covered listing link should seed");
        let old_aspect = PendingReviewAspect::avionics(
            "old-covered-link",
            "avionics",
            "Garmin GNS 430W",
            "Garmin GNS 430W installed",
            "legacy association requires review",
            1,
            "installed",
            Some("old covered evidence".to_string()),
            Some("low".to_string()),
        )
        .with_covered_association(
            old_link_id,
            ListingAssociationRole::Installed,
            linked_model_id,
        );
        stage_pending_review(&db, listing_id, None, &[old_aspect])
            .await
            .expect("old covered review should stage");
        execute_query!(
            &db,
            r#"
            CREATE TRIGGER fail_pending_review_restage
            BEFORE UPDATE ON aircraft_sale_listing_pending_reviews
            BEGIN
              SELECT RAISE(FAIL, 'forced pending review restage failure');
            END
            "#
        )
        .expect("restage failure trigger should install");

        let error = super::update_listing(
            &db,
            user.id,
            listing_id,
            &json!({
                "avionics": [{
                    "manufacturer": "Garmin",
                    "model": "Imaginary 2000",
                    "types": ["GPS"],
                    "quantity": 1,
                    "configuration_action": "installed",
                    "source_evidence_text": "Imaginary 2000 installed",
                    "source_confidence": "medium"
                }]
            }),
            None,
        )
        .await
        .expect_err("forced restage failure should fail the explicit patch");
        assert!(matches!(error, super::ListingStoreError::Ingestion { .. }));

        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
                listing_id
            )
            .expect("pending review count should load"),
            0
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE id = ?",
                old_link_id
            )
            .expect("old link count should load"),
            0
        );
        let state = query_as_optional!(
            &db,
            (String, Option<String>),
            "SELECT ingestion_state, ingestion_error FROM aircraft_sale_listings WHERE id = ?",
            listing_id
        )
        .expect("listing state should load")
        .expect("listing should exist");
        assert_eq!(state.0, "quarantined");
        assert!(state
            .1
            .is_some_and(|message| message.contains("forced pending review restage failure")));
    }
}
