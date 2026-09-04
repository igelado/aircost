use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Connection, FromRow};

use crate::aircraft::curation::visual::{
    VisibleIdentifierKind, VisualConsensusStatus, VisualEvidenceConfidence,
    VisualIdentifierResolution,
};
use crate::aircraft::faa::{
    admit_aircraft_source_identity, normalize_n_number, normalize_serial_key,
    require_aircraft_admission, require_listing_admission, require_listing_faa_admission,
    AircraftAdmissionError, AircraftGrounding, BlockReason, FaaSerialCorrection,
};
use crate::aircraft::identity::{
    ensure_listing_identity_assignment_from_approved_catalog, EnsureIdentityAssignmentOutcome,
};
use crate::avionics::authorization::{
    listing_authorization_state, listing_authorization_state_postgres,
    listing_authorization_state_sqlite,
};
#[cfg(test)]
use crate::avionics::catalog::grounded_resolution_receipt_seed_for_replay;
use crate::avionics::catalog::{
    approved_avionics_identity_for_grounded_replay, authoritative_source_revocation_count,
    deterministic_generic_avionics_rejection_reason, grounded_resolution_receipt_basis_for_replay,
    grounded_resolution_request_sha256, resolve_avionics_identity_for_listing_materialization,
    resolve_verified_local_avionics_identity, resolve_verified_local_avionics_model_observation,
    resolve_verified_local_controller_run_on_avionics_identity,
    unique_exact_avionics_model_observation_review_candidate, ApprovedAvionicsIdentity,
    AvionicsIdentityOutcome, AvionicsIdentityRequest, AvionicsReviewCatalogCandidate, CatalogError,
    GroundedAvionicsResolutionReceiptSeed,
};
use crate::avionics::fingerprint::{
    active_collision_closure_revision_sha256_postgres,
    active_collision_closure_revision_sha256_sqlite, catalog_product_fingerprint_for_id,
    catalog_product_fingerprint_from_rows, fingerprint_grounded_collision_closure,
    grounded_collision_closure_revision_sha256, ActiveCollisionCatalogFingerprintRow,
    AvionicsFingerprintError, CatalogFingerprintRow, ACTIVE_COLLISION_CATALOG_ROWS_SQL,
    APPROVED_CATALOG_ROWS_SQL,
};
use crate::avionics::reuse::{
    countable_unit_product_reuse_attestation_is_current,
    countable_unit_reuse_attestation_is_current_postgres,
    countable_unit_reuse_attestation_is_current_sqlite, reuse_attestation_is_current_postgres,
    reuse_attestation_is_current_sqlite,
};
use crate::cleanup::{cleanup_orphan_records, CleanupError};
use crate::db::{AppDb, DatabaseBackend};
use crate::extract::{optional_f64, optional_i64, optional_string, GeminiListingExtractor};
use crate::html::listing::source::{
    controller_extraction_source_has_exact_avionics_line, ListingEvidenceUnits,
};
use crate::listing::avionics::disposition::{AutomaticOccurrenceDisposition, OccurrenceRole};
use crate::listing::avionics::extraction::{
    exact_controller_leading_dual_evidence_proof, ExactControllerLeadingDualEvidenceProof,
    ListingAvionicsEvidenceObservation,
};
use crate::listing::avionics::{
    approved_avionics_product_key, validate_canonical_avionics_actions, CanonicalAvionicsAction,
};
use crate::listing::evidence::MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES;
use crate::listing::review::{
    association_observation_sha256_from_values, clear_pending_review, replace_pending_review,
    replace_pending_review_preserving_source_identity_receipt_gate, ListingAssociationRole,
    PendingReviewAspect, ReviewAction, ReviewAspectId, ReviewProduct, StableIdentifier,
    POSTGRES_LISTING_CHILD_LOCK_SQL, POSTGRES_RESTAGE_CATALOG_LOCK_SQL,
};
use crate::models::{
    is_plausible_asking_price_usd, is_top_overhaul_time_evidence, AircraftSummary, ListingPreview,
    ListingValuationFact, ParsedAvionics, ParsedAvionicsReference, ParsedInstalledComponent,
    SaleListing,
};
use crate::normalize::{
    is_usable_avionics_label, normalize_avionics_manufacturer_name, normalize_avionics_model_name,
    normalize_name,
};
use crate::plugin::current_checkpoint_contains_avionics_source_evidence;

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
}

#[derive(Debug)]
pub(crate) struct ListingCreationResult {
    pub(crate) listing: SaleListing,
    pub(crate) occurrence_dispositions: Vec<AutomaticOccurrenceDisposition>,
    pub(crate) source_serial_correction: Option<FaaSerialCorrection>,
    pub(crate) source_visual_correction: Option<SourceVisualRegistrationCorrection>,
}

#[derive(Clone, Debug)]
pub(crate) struct SourceVisualRegistrationCorrection {
    pub(crate) observed_registration_number: String,
    pub(crate) corrected_registration_number: String,
    pub(crate) corrected_serial_number: Option<String>,
    pub(crate) grounding: AircraftGrounding,
    pub(crate) resolution: VisualIdentifierResolution,
    pub(crate) media_url: String,
}

#[derive(Clone, Debug, Deserialize, FromRow, Serialize)]
pub(crate) struct PinnedSourceVisualCorrectionArtifact {
    pub(crate) plugin_submission_id: i64,
    pub(crate) rendered_html_sha256: String,
    pub(crate) observed_registration_number: String,
    pub(crate) corrected_registration_number: String,
    pub(crate) corrected_serial_number: Option<String>,
    pub(crate) faa_registry_snapshot_id: i64,
    pub(crate) faa_snapshot_archive_sha256: String,
    pub(crate) faa_source_record_sha256: String,
    pub(crate) primary_photo_asset_id: String,
    pub(crate) primary_photo_url: String,
    pub(crate) primary_photo_sha256: String,
    pub(crate) visual_resolution_sha256: String,
    pub(crate) visual_resolution_json: String,
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

async fn recover_source_visual_registration(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    binding: &SignedSourceListingBinding,
    observed_registration: Option<&str>,
    observed_snapshot_id: i64,
) -> StoreResult<Option<SourceVisualRegistrationCorrection>> {
    recover_source_visual_registration_from_retained_source(
        db,
        extractor,
        &binding.source_url,
        &binding.rendered_html,
        observed_registration,
        observed_snapshot_id,
        binding.submission_id,
        &binding.rendered_html_sha256,
    )
    .await
}

async fn recover_source_visual_registration_from_retained_source(
    db: &AppDb,
    extractor: &GeminiListingExtractor,
    source_url: &str,
    rendered_html: &str,
    observed_registration: Option<&str>,
    observed_snapshot_id: i64,
    submission_id: i64,
    rendered_html_sha256: &str,
) -> StoreResult<Option<SourceVisualRegistrationCorrection>> {
    let Some(observed_registration) = observed_registration
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    if !crate::html::clean::listing_body_contains_exact_structurally_visible_text_span(
        rendered_html,
        observed_registration,
    ) {
        return Ok(None);
    }
    let Some((resolution, media_url)) = extractor
        .recover_visible_aircraft_identity_from_primary_photo(
            source_url,
            rendered_html,
            submission_id,
            rendered_html_sha256,
        )
        .await?
    else {
        return Ok(None);
    };
    admit_source_visual_registration(
        db,
        observed_registration,
        observed_snapshot_id,
        resolution,
        media_url,
    )
    .await
}

async fn admit_source_visual_registration(
    db: &AppDb,
    observed_registration: &str,
    observed_snapshot_id: i64,
    resolution: VisualIdentifierResolution,
    media_url: String,
) -> StoreResult<Option<SourceVisualRegistrationCorrection>> {
    let Some(observed_n_number) = normalize_n_number(observed_registration) else {
        return Ok(None);
    };
    if resolution.photos.len() != 1
        || resolution.registration_consensus.status != VisualConsensusStatus::AutoAccept
    {
        return Ok(None);
    }
    let Some(candidate_n_number) = resolution
        .registration_consensus
        .normalized_n_number
        .as_deref()
        .and_then(normalize_n_number)
    else {
        return Ok(None);
    };
    if candidate_n_number == observed_n_number {
        return Ok(None);
    }
    let visible_serials = resolution
        .candidates
        .iter()
        .filter(|candidate| candidate.kind == VisibleIdentifierKind::ManufacturerSerial)
        .filter(|candidate| {
            !candidate.evidence.is_empty()
                && candidate.evidence.iter().all(|evidence| {
                    matches!(
                        evidence.confidence,
                        VisualEvidenceConfidence::High | VisualEvidenceConfidence::VeryHigh
                    )
                })
        })
        .map(|candidate| candidate.visible_text.trim())
        .collect::<Vec<_>>();
    if visible_serials.len() > 1 || visible_serials.iter().any(|serial| serial.is_empty()) {
        return Ok(None);
    }
    let visible_serial_keys = visible_serials
        .iter()
        .map(|serial| normalize_serial_key(serial))
        .collect::<Option<HashSet<_>>>();
    let Some(visible_serial_keys) = visible_serial_keys else {
        return Ok(None);
    };
    if visible_serial_keys.len() > 1 {
        return Ok(None);
    }
    let visible_serial = visible_serials.first().copied();
    let grounding = match require_aircraft_admission(db, Some(&candidate_n_number), None).await {
        Ok(grounding) => grounding,
        Err(AircraftAdmissionError::Rejected { .. }) => return Ok(None),
        Err(error) => return Err(listing_admission_error(error)),
    };
    let corrected_serial_number = grounding.manufacturer_serial_raw.clone();
    if let Some(visible_serial) = visible_serial {
        let Some(faa_serial) = corrected_serial_number.as_deref() else {
            return Ok(None);
        };
        if normalize_serial_key(visible_serial) != normalize_serial_key(faa_serial) {
            return Ok(None);
        }
    }
    let admission_serial = visible_serial.or(corrected_serial_number.as_deref());
    let grounding =
        match require_aircraft_admission(db, Some(&candidate_n_number), admission_serial).await {
            Ok(grounding) => grounding,
            Err(AircraftAdmissionError::Rejected { .. }) => return Ok(None),
            Err(error) => return Err(listing_admission_error(error)),
        };
    if grounding.snapshot.id != observed_snapshot_id {
        return Ok(None);
    }
    let correction = SourceVisualRegistrationCorrection {
        observed_registration_number: observed_registration.to_string(),
        corrected_registration_number: candidate_n_number,
        corrected_serial_number,
        grounding,
        resolution,
        media_url,
    };
    validate_visual_faa_pair(db, &correction).await?;
    Ok(Some(correction))
}

async fn validate_visual_faa_pair(
    db: &AppDb,
    correction: &SourceVisualRegistrationCorrection,
) -> StoreResult<()> {
    let observed =
        normalize_n_number(&correction.observed_registration_number).ok_or_else(|| {
            ListingStoreError::State(
                "visual correction observed registration is invalid".to_string(),
            )
        })?;
    let count = query_scalar_one!(
        db,
        i64,
        r#"
        SELECT COUNT(*)
        FROM faa_registry_snapshots snapshot
        JOIN faa_registry_coverage observed
          ON observed.snapshot_id = snapshot.id
         AND observed.n_number = ?
         AND observed.lookup_status = 'absent'
        JOIN faa_registry_coverage corrected
          ON corrected.snapshot_id = snapshot.id
         AND corrected.n_number = ?
         AND corrected.lookup_status = 'matched'
        JOIN faa_registry_aircraft aircraft
          ON aircraft.snapshot_id = snapshot.id
         AND aircraft.n_number = corrected.n_number
        WHERE snapshot.id = ?
          AND snapshot.id = (
            SELECT id FROM faa_registry_snapshots
            ORDER BY snapshot_date DESC, id DESC LIMIT 1
          )
          AND snapshot.archive_sha256 = ?
          AND aircraft.source_record_sha256 = ?
          AND (
            aircraft.manufacturer_serial_raw = ?
            OR (aircraft.manufacturer_serial_raw IS NULL AND ? IS NULL)
          )
        "#,
        observed,
        correction.corrected_registration_number.as_str(),
        correction.grounding.snapshot.id,
        correction.grounding.snapshot.archive_sha256.as_str(),
        correction.grounding.source_record_sha256.as_str(),
        correction.corrected_serial_number.as_deref(),
        correction.corrected_serial_number.as_deref()
    )?;
    if count != 1 {
        return Err(ListingStoreError::State(
            "visual correction FAA absence/match pair changed before binding".to_string(),
        ));
    }
    Ok(())
}

fn pinned_visual_resolution(resolution: &VisualIdentifierResolution) -> VisualIdentifierResolution {
    let mut normalized = resolution.clone();
    normalized.interaction_id = None;
    normalized.model.clear();
    normalized.prompt_version.clear();
    normalized.schema_version.clear();
    normalized.total_input_tokens = None;
    normalized.total_output_tokens = None;
    normalized
}

fn pinned_source_visual_artifact(
    binding: &SignedSourceListingBinding,
    correction: &SourceVisualRegistrationCorrection,
) -> StoreResult<PinnedSourceVisualCorrectionArtifact> {
    let photo = correction.resolution.photos.first().ok_or_else(|| {
        ListingStoreError::State("visual correction has no audited primary photo".to_string())
    })?;
    let primary_photo_asset_id = photo
        .image_id
        .strip_prefix("asset-")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ListingStoreError::State(
                "visual correction primary photo has no retained asset id".to_string(),
            )
        })?
        .to_string();
    let visual_resolution_json =
        serde_json::to_string(&pinned_visual_resolution(&correction.resolution))
            .map_err(|error| ListingStoreError::Database(error.to_string()))?;
    let visual_resolution_sha256 =
        format!("{:x}", Sha256::digest(visual_resolution_json.as_bytes()));
    Ok(PinnedSourceVisualCorrectionArtifact {
        plugin_submission_id: binding.submission_id,
        rendered_html_sha256: binding.rendered_html_sha256.clone(),
        observed_registration_number: correction.observed_registration_number.clone(),
        corrected_registration_number: correction.corrected_registration_number.clone(),
        corrected_serial_number: correction.corrected_serial_number.clone(),
        faa_registry_snapshot_id: correction.grounding.snapshot.id,
        faa_snapshot_archive_sha256: correction.grounding.snapshot.archive_sha256.clone(),
        faa_source_record_sha256: correction.grounding.source_record_sha256.clone(),
        primary_photo_asset_id,
        primary_photo_url: correction.media_url.clone(),
        primary_photo_sha256: photo.sha256.clone(),
        visual_resolution_sha256,
        visual_resolution_json,
    })
}

#[derive(Debug)]
struct ResolvedListingAvionics {
    pending_review_aspects: Vec<PendingReviewAspect>,
    occurrence_dispositions: Vec<AutomaticOccurrenceDisposition>,
}

const GROUNDED_CAPABILITY_SET_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:listing-avionics-grounded-capability-set";
const GROUNDED_OCCURRENCE_CAPABILITY_FINGERPRINT_DOMAIN: &[u8] =
    b"aircost:listing-avionics-grounded-occurrence-capability";

#[derive(Clone, Debug)]
struct ListingGroundedCapability {
    occurrence_index: usize,
    occurrence_role: OccurrenceRole,
    configuration_action: String,
    source_notes: Option<String>,
    seed: GroundedAvionicsResolutionReceiptSeed,
}

#[derive(Clone, Debug)]
struct GroundedCapabilityReplayScope {
    listing_id: i64,
    plugin_submission_id: i64,
    rendered_html_sha256: String,
    extracted_listing_sha256: String,
    allow_provider_fallback: bool,
}

#[derive(Clone, Debug)]
struct ExactListingSourceCaptureScope {
    plugin_submission_id: i64,
    rendered_html_sha256: String,
    extracted_listing_sha256: String,
}

impl ExactListingSourceCaptureScope {
    fn from_signed_binding(binding: &SignedSourceListingBinding) -> Self {
        Self {
            plugin_submission_id: binding.submission_id,
            rendered_html_sha256: binding.rendered_html_sha256.clone(),
            extracted_listing_sha256: binding.bound_extracted_listing_sha256.clone(),
        }
    }

    fn from_replay_scope(scope: &GroundedCapabilityReplayScope) -> Self {
        Self {
            plugin_submission_id: scope.plugin_submission_id,
            rendered_html_sha256: scope.rendered_html_sha256.clone(),
            extracted_listing_sha256: scope.extracted_listing_sha256.clone(),
        }
    }
}

#[derive(Debug, FromRow)]
struct ExactSignedListingCheckpointRow {
    plugin_submission_id: i64,
    rendered_html_sha256: String,
    extracted_listing_json: String,
}

async fn exact_signed_listing_checkpoint_scope(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<Option<ExactListingSourceCaptureScope>> {
    let rows = query_as_all!(
        db,
        ExactSignedListingCheckpointRow,
        r#"
        SELECT submission.id AS plugin_submission_id,
               submission.rendered_html_sha256,
               submission.extracted_listing_json
        FROM plugin_submissions submission
        WHERE submission.canonical_listing_id = ?
          AND submission.extracted_listing_json IS NOT NULL
          AND submission.extraction_error IS NULL
        ORDER BY submission.id
        "#,
        listing_id
    )?;
    let [row] = rows.as_slice() else {
        return if rows.is_empty() {
            Ok(None)
        } else {
            Err(ListingStoreError::State(format!(
                "listing {listing_id} has multiple retained signed extraction checkpoints"
            )))
        };
    };
    Ok(Some(ExactListingSourceCaptureScope {
        plugin_submission_id: row.plugin_submission_id,
        rendered_html_sha256: row.rendered_html_sha256.clone(),
        extracted_listing_sha256: format!(
            "{:x}",
            Sha256::digest(row.extracted_listing_json.as_bytes())
        ),
    }))
}

enum GroundedCapabilityReplayOutcome {
    Approved(
        ApprovedAvionicsIdentity,
        GroundedAvionicsResolutionReceiptSeed,
    ),
    Absent,
    RetiredStale,
}

#[derive(Clone, Debug)]
struct PreparedGroundedCapabilityBinding {
    occurrence_index: usize,
    occurrence_role: OccurrenceRole,
    avionics_model_id: i64,
    requested_quantity: i64,
    configuration_action: String,
    source_notes: Option<String>,
    seed: GroundedAvionicsResolutionReceiptSeed,
    product_fingerprint: String,
    collision_closure_sha256: String,
}

#[derive(Debug, FromRow)]
struct StoredGroundedCapabilityRow {
    plugin_submission_id: i64,
    occurrence_index: i64,
    occurrence_role: String,
    avionics_model_id: i64,
    requested_quantity: i64,
    configuration_action: String,
    request_sha256: String,
    capability_sha256: String,
    grounded_resolution_sha256: String,
    evidence_capture_sha256: String,
    extracted_listing_sha256: String,
    submission_canonical_listing_id: Option<i64>,
    submission_rendered_html_sha256: Option<String>,
    extracted_listing_json: Option<String>,
    extraction_error: Option<String>,
    product_fingerprint: String,
    collision_closure_sha256: String,
    source_revocation_count: i64,
}

fn grounded_capability_submission_checkpoint_is_current(
    listing_id: i64,
    row: &StoredGroundedCapabilityRow,
) -> bool {
    row.submission_canonical_listing_id == Some(listing_id)
        && row.submission_rendered_html_sha256.as_deref()
            == Some(row.evidence_capture_sha256.as_str())
        && row.extraction_error.is_none()
        && row
            .extracted_listing_json
            .as_deref()
            .map(|checkpoint| format!("{:x}", Sha256::digest(checkpoint.as_bytes())))
            == Some(row.extracted_listing_sha256.clone())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredGroundedCapabilityScope {
    plugin_submission_id: i64,
    evidence_capture_sha256: String,
    extracted_listing_sha256: String,
}

fn stored_grounded_capability_scope(
    row: &StoredGroundedCapabilityRow,
) -> StoredGroundedCapabilityScope {
    StoredGroundedCapabilityScope {
        plugin_submission_id: row.plugin_submission_id,
        evidence_capture_sha256: row.evidence_capture_sha256.clone(),
        extracted_listing_sha256: row.extracted_listing_sha256.clone(),
    }
}

fn require_exact_grounded_capability_scope(
    expected: &mut Option<StoredGroundedCapabilityScope>,
    row: &StoredGroundedCapabilityRow,
) -> StoreResult<()> {
    let actual = stored_grounded_capability_scope(row);
    match expected {
        Some(expected) if expected != &actual => Err(ListingStoreError::Validation(
            "pending grounded capabilities do not share one exact submission, capture, and extraction checkpoint"
                .to_string(),
        )),
        Some(_) => Ok(()),
        None => {
            *expected = Some(actual);
            Ok(())
        }
    }
}

fn grounded_occurrence_capability_sha256(capability: &ListingGroundedCapability) -> String {
    let mut hasher = Sha256::new();
    hasher.update(GROUNDED_OCCURRENCE_CAPABILITY_FINGERPRINT_DOMAIN);
    for value in [
        capability.occurrence_index.to_string(),
        capability.occurrence_role.as_str().to_string(),
        capability.configuration_action.clone(),
        capability.source_notes.clone().unwrap_or_default(),
        capability.seed.avionics_model_id().to_string(),
        capability.seed.requested_quantity().to_string(),
        capability.seed.request_sha256().to_string(),
        capability.seed.capability_sha256().to_string(),
        capability.seed.product_fingerprint().to_string(),
        capability.seed.collision_closure_sha256().to_string(),
        capability.seed.source_revocation_count().to_string(),
    ] {
        hasher.update((value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn grounded_capability_set_sha256(
    listing_id: i64,
    avionics_model_id: i64,
    association_role: &str,
    quantity: i64,
    configuration_action: &str,
    rows: &[StoredGroundedCapabilityRow],
) -> String {
    let mut receipts = rows.iter().collect::<Vec<_>>();
    receipts.sort_by_key(|row| (row.occurrence_index, row.occurrence_role.clone()));
    let mut hasher = Sha256::new();
    hasher.update(GROUNDED_CAPABILITY_SET_FINGERPRINT_DOMAIN);
    for value in [
        listing_id.to_string(),
        avionics_model_id.to_string(),
        association_role.to_string(),
        quantity.to_string(),
        configuration_action.to_string(),
    ] {
        hasher.update((value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
    for row in receipts {
        for value in [
            row.plugin_submission_id.to_string(),
            row.occurrence_index.to_string(),
            row.occurrence_role.clone(),
            row.avionics_model_id.to_string(),
            row.requested_quantity.to_string(),
            row.configuration_action.clone(),
            row.request_sha256.clone(),
            row.capability_sha256.clone(),
            row.grounded_resolution_sha256.clone(),
            row.evidence_capture_sha256.clone(),
            row.extracted_listing_sha256.clone(),
            row.product_fingerprint.clone(),
            row.collision_closure_sha256.clone(),
            row.source_revocation_count.to_string(),
        ] {
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

const AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON: &str =
    "Automated product verification could not complete safely. Confirm or discard this observation manually.";
const AVIONICS_INSTALLATION_EVIDENCE_NOT_HIGH_REASON: &str =
    "The product identity was verified automatically, but the listing does not provide high-confidence evidence that this unit is installed.";
const AVIONICS_MANUFACTURER_REVIEW_REQUIRED_REASON: &str =
    "The listing names a concrete avionics model without a manufacturer, and the current verified catalog does not identify one unique reusable product. Select an existing verified product or discard this observation.";
const AVIONICS_SIGNED_CHECKPOINT_REQUIRED_REASON: &str =
    "The product identity matches the verified catalog, but this automatic association has no exact signed listing checkpoint. Confirm or discard it manually.";
pub(crate) const SOURCE_IDENTITY_RECEIPT_PENDING: &str =
    "source_identity_correction_receipt_pending";
pub(crate) const AVIONICS_AUTHORIZATION_INVALIDATED: &str = "avionics_authorization_invalidated";

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

#[derive(Clone, Debug, Eq, PartialEq)]
enum ListingSourceConfidenceBasis {
    RetainedHigh,
    ExactControllerLeadingDualCountableUnit(ExactControllerLeadingDualEvidenceProof),
}

#[derive(Clone, Debug)]
struct ListingAvionicsValue {
    avionics_model_id: Option<i64>,
    manufacturer: Option<String>,
    model: String,
    avionics_types: Vec<String>,
    quantity: i64,
    source: String,
    source_notes: Option<String>,
    source_confidence: Option<String>,
    source_confidence_basis: Option<ListingSourceConfidenceBasis>,
    configuration_action: String,
    replaces: Option<ParsedAvionicsReference>,
    replaces_avionics_model_id: Option<i64>,
    grounded_capabilities: Vec<ListingGroundedCapability>,
    replacement_grounded_capabilities: Vec<ListingGroundedCapability>,
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
            source_confidence_basis: None,
            configuration_action: item.configuration_action,
            replaces: item.replaces,
            replaces_avionics_model_id: None,
            grounded_capabilities: Vec::new(),
            replacement_grounded_capabilities: Vec::new(),
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
    let mut source_visual_correction = None;
    let (grounding, source_serial_correction) = if creation_mode.permits_source_serial_correction()
    {
        match admit_aircraft_source_identity(
            db,
            values.registration_number.as_deref(),
            admission_serial.as_deref(),
            preview.context_text.as_deref(),
        )
        .await
        {
            Ok(admission) => (admission.grounding, admission.serial_correction),
            Err(
                rejected @ AircraftAdmissionError::Rejected {
                    reason: BlockReason::RegistrationNotFound,
                    snapshot_id: Some(observed_snapshot_id),
                    ..
                },
            ) => {
                let Some(binding) = signed_source_binding else {
                    return Err(listing_admission_error(rejected));
                };
                let Some(extractor) = extractor else {
                    return Err(listing_admission_error(rejected));
                };
                let Some(correction) = Box::pin(recover_source_visual_registration(
                    db,
                    extractor,
                    binding,
                    values.registration_number.as_deref(),
                    observed_snapshot_id,
                ))
                .await?
                else {
                    return Err(listing_admission_error(rejected));
                };
                values.registration_number = Some(correction.corrected_registration_number.clone());
                values.serial_number = correction.corrected_serial_number.clone();
                let grounding = correction.grounding.clone();
                source_visual_correction = Some(correction);
                (grounding, None)
            }
            Err(error) => return Err(listing_admission_error(error)),
        }
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
    let source_identity_correction =
        source_serial_correction.is_some() || source_visual_correction.is_some();
    let may_reuse_listing = creation_mode.may_reuse_listing() && !source_identity_correction;
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
    // Only a signed source capture can own a paid same-case capability.
    // Ordinary REST creation is deliberately local-only even when the
    // process has a configured provider.
    let avionics_extractor = signed_source_binding.and(extractor);
    let exact_source_capture_scope =
        signed_source_binding.map(ExactListingSourceCaptureScope::from_signed_binding);
    let resolved_avionics = resolve_listing_avionics_values(
        db,
        &mut values,
        avionics_extractor,
        preview.source_url.as_deref(),
        preview.context_text.as_deref(),
        preview.source_evidence_units.as_ref(),
        None,
        exact_source_capture_scope.as_ref(),
    )
    .await?;

    // Prefer the exact source row repaired above. Looking it up again by tail
    // could select a different, newer listing if the user has retained more
    // than one observation for the same aircraft.
    if may_reuse_listing {
        if let Some(listing_id) = identity_repair_listing_id {
            emit_listing_progress(progress, "saving_listing", "Repairing existing listing");
            stage_existing_listing_signed_source_grounded_capabilities(
                db,
                listing_id,
                signed_source_binding,
                &values.avionics,
            )
            .await?;
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
            finalize_listing_ingestion(db, listing_id).await?;
            return Ok(ListingCreationResult {
                listing: get_listing(db, user_id, listing_id).await?,
                occurrence_dispositions: resolved_avionics.occurrence_dispositions,
                source_serial_correction,
                source_visual_correction,
            });
        }
    }

    if may_reuse_listing {
        if let Some(registration_number) = &values.registration_number {
            if let Some(listing_id) =
                unverified_listing_id_for_tail(db, user_id, registration_number).await?
            {
                emit_listing_progress(progress, "saving_listing", "Updating existing listing");
                stage_existing_listing_signed_source_grounded_capabilities(
                    db,
                    listing_id,
                    signed_source_binding,
                    &values.avionics,
                )
                .await?;
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
                finalize_listing_ingestion(db, listing_id).await?;
                return Ok(ListingCreationResult {
                    listing: get_listing(db, user_id, listing_id).await?,
                    occurrence_dispositions: resolved_avionics.occurrence_dispositions,
                    source_serial_correction,
                    source_visual_correction,
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
                stage_existing_listing_signed_source_grounded_capabilities(
                    db,
                    listing_id,
                    signed_source_binding,
                    &values.avionics,
                )
                .await?;
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
                finalize_listing_ingestion(db, listing_id).await?;
                return Ok(ListingCreationResult {
                    listing: get_listing(db, user_id, listing_id).await?,
                    occurrence_dispositions: resolved_avionics.occurrence_dispositions,
                    source_serial_correction,
                    source_visual_correction,
                });
            }
        }
    }

    if may_reuse_listing && resolved_avionics.pending_review_aspects.is_empty() {
        if let Some(listing_id) = matching_verified_listing_id(db, &values).await? {
            stage_existing_listing_signed_source_grounded_capabilities(
                db,
                listing_id,
                signed_source_binding,
                &values.avionics,
            )
            .await?;
            emit_listing_progress(progress, "saving_listing", "Refreshing matching listing");
            refresh_listing_timestamp(db, listing_id, values.source_url.as_deref()).await?;
            if let Some(binding) = signed_source_binding {
                retire_exact_bound_grounded_capabilities(
                    db,
                    &GroundedCapabilityReplayScope {
                        listing_id,
                        plugin_submission_id: binding.submission_id,
                        rendered_html_sha256: binding.rendered_html_sha256.clone(),
                        extracted_listing_sha256: binding.bound_extracted_listing_sha256.clone(),
                        allow_provider_fallback: true,
                    },
                )
                .await?;
            }
            return Ok(ListingCreationResult {
                listing: get_listing(db, user_id, listing_id).await?,
                occurrence_dispositions: resolved_avionics.occurrence_dispositions,
                source_serial_correction,
                source_visual_correction,
            });
        }
    }

    emit_listing_progress(progress, "saving_listing", "Saving listing");
    let listing_id = insert_listing(
        db,
        user_id,
        &values,
        &literal_identity_values,
        source_identity_correction,
        signed_source_binding,
        source_visual_correction.as_ref(),
    )
    .await?;
    replace_listing_pending_review(
        db,
        listing_id,
        &resolved_avionics.pending_review_aspects,
        source_identity_correction,
    )
    .await?;
    if source_identity_correction {
        return Ok(ListingCreationResult {
            listing: get_listing(db, user_id, listing_id).await?,
            occurrence_dispositions: resolved_avionics.occurrence_dispositions,
            source_serial_correction,
            source_visual_correction,
        });
    }
    emit_listing_progress(
        progress,
        "refreshing_estimates",
        "Refreshing valuation inputs",
    );
    finalize_listing_ingestion(db, listing_id).await?;
    Ok(ListingCreationResult {
        listing: get_listing(db, user_id, listing_id).await?,
        occurrence_dispositions: resolved_avionics.occurrence_dispositions,
        source_serial_correction,
        source_visual_correction,
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
    let replay_scope = grounded_capability_replay_scope(db, listing_id).await?;
    let exact_source_capture_scope = replay_scope
        .as_ref()
        .map(ExactListingSourceCaptureScope::from_replay_scope);
    let resolved_avionics = resolve_listing_avionics_values(
        db,
        &mut values,
        extractor,
        preview.source_url.as_deref(),
        preview.context_text.as_deref(),
        preview.source_evidence_units.as_ref(),
        replay_scope.as_ref(),
        exact_source_capture_scope.as_ref(),
    )
    .await?;
    stage_bound_replay_grounded_capabilities(db, replay_scope.as_ref(), &values.avionics).await?;
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
        source_visual_correction: None,
    })
}

/// Revalidate the single retained primary photo for a receipt-gated visual
/// registration correction after a process interruption. The corrected row
/// must still match the newly FAA-admitted visual result exactly before any
/// child projection can be rebuilt or the immutable receipt can be written.
pub(crate) async fn resume_signed_source_visual_correction_listing(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
    submission_id: i64,
    preview: &ListingPreview,
    extractor: Option<&GeminiListingExtractor>,
    rendered_html: &str,
    rebuild_children: bool,
) -> StoreResult<ListingCreationResult> {
    let values = values_from_preview(preview, None)?;
    let source_url = values.source_url.as_deref().ok_or_else(|| {
        ListingStoreError::State(
            "receipt-gated visual correction lost its retained source URL".to_string(),
        )
    })?;
    let artifact = query_as_optional!(
        db,
        PinnedSourceVisualCorrectionArtifact,
        r#"
        SELECT plugin_submission_id, rendered_html_sha256,
               observed_registration_number, corrected_registration_number,
               corrected_serial_number, faa_registry_snapshot_id,
               faa_snapshot_archive_sha256, faa_source_record_sha256,
               primary_photo_asset_id, primary_photo_url, primary_photo_sha256,
               visual_resolution_sha256, visual_resolution_json
        FROM aircraft_source_visual_correction_artifacts
        WHERE plugin_submission_id = ?
        "#,
        submission_id
    )?
    .ok_or_else(|| {
        ListingStoreError::State(
            "receipt-gated visual correction lost its pinned artifact".to_string(),
        )
    })?;
    let rendered_html_sha256 = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
    if artifact.rendered_html_sha256 != rendered_html_sha256
        || values.registration_number.as_deref()
            != Some(artifact.observed_registration_number.as_str())
    {
        return Err(ListingStoreError::State(
            "receipt-gated visual artifact no longer matches its signed capture".to_string(),
        ));
    }
    let discovery = crate::html::listing::media::discover(source_url, rendered_html)
        .map_err(|error| ListingStoreError::State(error.to_string()))?;
    let primary = discovery.aircraft_photos.first().ok_or_else(|| {
        ListingStoreError::State("retained capture lost its primary aircraft photo".to_string())
    })?;
    if primary.asset_id != artifact.primary_photo_asset_id
        || primary.media_url != artifact.primary_photo_url
    {
        return Err(ListingStoreError::State(
            "retained primary aircraft photo changed after visual pinning".to_string(),
        ));
    }
    if format!(
        "{:x}",
        Sha256::digest(artifact.visual_resolution_json.as_bytes())
    ) != artifact.visual_resolution_sha256
    {
        return Err(ListingStoreError::State(
            "pinned visual resolution hash is invalid".to_string(),
        ));
    }
    let resolution: VisualIdentifierResolution =
        serde_json::from_str(&artifact.visual_resolution_json)
            .map_err(|error| ListingStoreError::State(error.to_string()))?;
    if resolution.photos.len() != 1
        || resolution.photos[0].image_id != format!("asset-{}", primary.asset_id)
        || resolution.photos[0].sha256 != artifact.primary_photo_sha256
    {
        return Err(ListingStoreError::State(
            "pinned primary photo audit is inconsistent".to_string(),
        ));
    }
    let grounding = require_aircraft_admission(
        db,
        Some(&artifact.corrected_registration_number),
        artifact.corrected_serial_number.as_deref(),
    )
    .await
    .map_err(listing_admission_error)?;
    if grounding.snapshot.id != artifact.faa_registry_snapshot_id
        || grounding.snapshot.archive_sha256 != artifact.faa_snapshot_archive_sha256
        || grounding.source_record_sha256 != artifact.faa_source_record_sha256
    {
        return Err(ListingStoreError::State(
            "pinned visual correction FAA record is no longer current".to_string(),
        ));
    }
    let correction = SourceVisualRegistrationCorrection {
        observed_registration_number: artifact.observed_registration_number,
        corrected_registration_number: artifact.corrected_registration_number,
        corrected_serial_number: artifact.corrected_serial_number,
        grounding,
        resolution,
        media_url: artifact.primary_photo_url,
    };
    validate_visual_faa_pair(db, &correction).await?;
    resume_signed_source_visual_correction_listing_with_correction(
        db,
        user_id,
        listing_id,
        preview,
        extractor,
        correction,
        rebuild_children,
    )
    .await
}

async fn resume_signed_source_visual_correction_listing_with_correction(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
    preview: &ListingPreview,
    extractor: Option<&GeminiListingExtractor>,
    correction: SourceVisualRegistrationCorrection,
    rebuild_children: bool,
) -> StoreResult<ListingCreationResult> {
    let mut values = values_from_preview(preview, None)?;
    let literal_identity_values = values.clone();
    values.registration_number = Some(correction.corrected_registration_number.clone());
    values.serial_number = correction.corrected_serial_number.clone();
    apply_faa_grounding_identity(&mut values, &correction.grounding);
    let current = get_listing(db, user_id, listing_id).await?;
    if current.created_by_user_id != user_id
        || current.source_url != values.source_url
        || current.ingestion_state != "quarantined"
        || current.ingestion_error.as_deref() != Some(SOURCE_IDENTITY_RECEIPT_PENDING)
        || current.registration_number != values.registration_number
        || current.serial_number != values.serial_number
    {
        return Err(ListingStoreError::State(format!(
            "receipt-gated visual listing {listing_id} changed before deterministic recovery"
        )));
    }
    let occurrence_dispositions = if rebuild_children {
        let replay_scope = grounded_capability_replay_scope(db, listing_id).await?;
        let exact_source_capture_scope = replay_scope
            .as_ref()
            .map(ExactListingSourceCaptureScope::from_replay_scope);
        let resolved_avionics = resolve_listing_avionics_values(
            db,
            &mut values,
            extractor,
            preview.source_url.as_deref(),
            preview.context_text.as_deref(),
            preview.source_evidence_units.as_ref(),
            replay_scope.as_ref(),
            exact_source_capture_scope.as_ref(),
        )
        .await?;
        stage_bound_replay_grounded_capabilities(db, replay_scope.as_ref(), &values.avionics)
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
        resolved_avionics.occurrence_dispositions
    } else {
        Vec::new()
    };
    Ok(ListingCreationResult {
        listing: get_listing(db, user_id, listing_id).await?,
        occurrence_dispositions,
        source_serial_correction: None,
        source_visual_correction: Some(correction),
    })
}

/// Deterministically rebuild every mutable child projection for an already
/// atomically bound replay listing. The capture binding is only an ownership
/// anchor; callers must not treat it as proof that materialization completed.
pub(crate) async fn resume_bound_replay_listing(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
    plugin_submission_id: i64,
    rendered_html_sha256: &str,
    extracted_listing_sha256: &str,
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
    let replay_scope = GroundedCapabilityReplayScope {
        listing_id,
        plugin_submission_id,
        rendered_html_sha256: rendered_html_sha256.to_string(),
        extracted_listing_sha256: extracted_listing_sha256.to_string(),
        allow_provider_fallback: true,
    };
    let exact_source_capture_scope =
        ExactListingSourceCaptureScope::from_replay_scope(&replay_scope);
    let resolved_avionics = resolve_listing_avionics_values(
        db,
        &mut values,
        extractor,
        preview.source_url.as_deref(),
        preview.context_text.as_deref(),
        preview.source_evidence_units.as_ref(),
        Some(&replay_scope),
        Some(&exact_source_capture_scope),
    )
    .await?;
    if current.is_verified {
        if !resolved_avionics.pending_review_aspects.is_empty()
            || !listing_matches_values(db, &current, &values).await?
        {
            return Err(ListingStoreError::State(format!(
                "verified bound replay listing {listing_id} no longer matches its retained checkpoint"
            )));
        }
        retire_exact_bound_grounded_capabilities(db, &replay_scope).await?;
        return Ok(ListingCreationResult {
            listing: current,
            occurrence_dispositions: resolved_avionics.occurrence_dispositions,
            source_serial_correction: None,
            source_visual_correction: None,
        });
    }
    stage_bound_replay_grounded_capabilities(db, Some(&replay_scope), &values.avionics).await?;
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
    finalize_listing_ingestion(db, listing_id).await?;
    Ok(ListingCreationResult {
        listing: get_listing(db, user_id, listing_id).await?,
        occurrence_dispositions: resolved_avionics.occurrence_dispositions,
        source_serial_correction: None,
        source_visual_correction: None,
    })
}

async fn grounded_capability_replay_scope(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<Option<GroundedCapabilityReplayScope>> {
    let capability_rows = query_as_all!(
        db,
        StoredGroundedCapabilityRow,
        r#"
        SELECT capability.plugin_submission_id, capability.occurrence_index,
               capability.occurrence_role, capability.avionics_model_id,
               capability.requested_quantity, capability.configuration_action,
               capability.request_sha256, capability.capability_sha256,
               capability.grounded_resolution_sha256,
               capability.evidence_capture_sha256,
               capability.extracted_listing_sha256,
               submission.canonical_listing_id AS submission_canonical_listing_id,
               submission.rendered_html_sha256 AS submission_rendered_html_sha256,
               submission.extracted_listing_json,
               submission.extraction_error,
               capability.product_fingerprint,
               capability.collision_closure_sha256,
               capability.source_revocation_count
        FROM aircraft_sale_listing_avionics_grounded_capabilities capability
        LEFT JOIN plugin_submissions submission
          ON submission.id = capability.plugin_submission_id
        WHERE capability.listing_id = ?
        "#,
        listing_id
    )?;
    let mut scopes = Vec::<StoredGroundedCapabilityScope>::new();
    let mut retired_stale_scope = None;
    for row in capability_rows {
        if !grounded_capability_submission_checkpoint_is_current(listing_id, &row) {
            retired_stale_scope.get_or_insert_with(|| stored_grounded_capability_scope(&row));
            // Retire only the exact stale proof. The retained submission may
            // still be a valid paid-grounding scope for a fresh resolution.
            retire_exact_stale_grounded_capability(db, listing_id, &row)
                .await
                .map_err(ListingStoreError::State)?;
            continue;
        }
        let scope = StoredGroundedCapabilityScope {
            plugin_submission_id: row.plugin_submission_id,
            evidence_capture_sha256: row.evidence_capture_sha256,
            extracted_listing_sha256: row.extracted_listing_sha256,
        };
        if !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    let ([] | [_]) = scopes.as_slice() else {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} has pending grounded capabilities from multiple source checkpoints"
        )));
    };
    let Some(scope) = scopes.into_iter().next() else {
        if let Some(scope) = retired_stale_scope.clone() {
            return Ok(Some(GroundedCapabilityReplayScope {
                listing_id,
                plugin_submission_id: scope.plugin_submission_id,
                rendered_html_sha256: scope.evidence_capture_sha256,
                extracted_listing_sha256: scope.extracted_listing_sha256,
                allow_provider_fallback: false,
            }));
        }
        let Some(bound) = exact_signed_listing_checkpoint_scope(db, listing_id).await? else {
            return Ok(None);
        };
        return Ok(Some(GroundedCapabilityReplayScope {
            listing_id,
            plugin_submission_id: bound.plugin_submission_id,
            rendered_html_sha256: bound.rendered_html_sha256.clone(),
            extracted_listing_sha256: bound.extracted_listing_sha256,
            allow_provider_fallback: true,
        }));
    };
    Ok(Some(GroundedCapabilityReplayScope {
        listing_id,
        plugin_submission_id: scope.plugin_submission_id,
        rendered_html_sha256: scope.evidence_capture_sha256,
        extracted_listing_sha256: scope.extracted_listing_sha256,
        allow_provider_fallback: retired_stale_scope.is_none(),
    }))
}

pub(crate) async fn finalize_signed_source_listing_after_receipt(
    db: &AppDb,
    user_id: i64,
    listing_id: i64,
    submission_id: i64,
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
          AND decision.correction_kind IN ('faa_serial', 'visual_identifier')
          AND decision.rendered_html_sha256 = submission.rendered_html_sha256
          AND submission.user_id = ?
          AND submission.canonical_listing_id = decision.aircraft_sale_listing_id
          AND submission.extraction_error IS NULL
          AND listing.created_by_user_id = submission.user_id
          AND (
            listing.registration_number = decision.corrected_registration_number
            OR (listing.registration_number IS NULL AND decision.corrected_registration_number IS NULL)
          )
          AND (
            listing.serial_number = decision.corrected_serial_number
            OR (listing.serial_number IS NULL AND decision.corrected_serial_number IS NULL)
          )
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
    finalize_listing_ingestion(db, listing_id).await?;
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
    _extractor: Option<&GeminiListingExtractor>,
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
                // Unsigned PATCH has no exact retained capture/checkpoint to
                // own a paid same-case capability. It may use current local
                // reuse only; every unattested identity remains pending for
                // the signed ingestion or manual review workflow.
                None,
                source_url.as_deref(),
                None,
                None,
                None,
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
    finalize_listing_ingestion(db, listing_id).await?;
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

fn prepare_grounded_capability_bindings(
    avionics: &[ListingAvionicsValue],
) -> StoreResult<Vec<PreparedGroundedCapabilityBinding>> {
    let mut prepared = Vec::new();
    for item in avionics {
        let installed_id = item.avionics_model_id.ok_or_else(|| {
            ListingStoreError::Validation(
                "grounded capability has no resolved installed catalog product".to_string(),
            )
        })?;
        if !item.grounded_capabilities.is_empty()
            && (item.grounded_capabilities.len() != 1
                || item.grounded_capabilities.iter().any(|capability| {
                    capability.occurrence_role != OccurrenceRole::Primary
                        || capability.configuration_action != item.configuration_action
                        || capability.seed.avionics_model_id() != installed_id
                        || capability.seed.requested_quantity() != item.quantity
                }))
        {
            return Err(ListingStoreError::State(format!(
                "one grounded occurrence must cover the exact quantity and action for avionics catalog id {installed_id}"
            )));
        }
        for capability in &item.grounded_capabilities {
            prepared.push(PreparedGroundedCapabilityBinding {
                occurrence_index: capability.occurrence_index,
                occurrence_role: capability.occurrence_role,
                avionics_model_id: installed_id,
                requested_quantity: capability.seed.requested_quantity(),
                configuration_action: item.configuration_action.clone(),
                source_notes: capability.source_notes.clone(),
                seed: capability.seed.clone(),
                product_fingerprint: capability.seed.product_fingerprint().to_string(),
                collision_closure_sha256: capability.seed.collision_closure_sha256().to_string(),
            });
        }

        if !item.replacement_grounded_capabilities.is_empty() {
            let replacement_id = item.replaces_avionics_model_id.ok_or_else(|| {
                ListingStoreError::State(
                    "replacement grounded capability has no resolved target".to_string(),
                )
            })?;
            if item.replacement_grounded_capabilities.len() != 1
                || item
                    .replacement_grounded_capabilities
                    .iter()
                    .any(|capability| {
                        capability.occurrence_role != OccurrenceRole::Replacement
                            || capability.configuration_action != item.configuration_action
                            || capability.seed.avionics_model_id() != replacement_id
                            || capability.seed.requested_quantity() != 1
                    })
            {
                return Err(ListingStoreError::State(format!(
                    "one grounded occurrence must cover the exact replacement semantics for avionics catalog id {replacement_id}"
                )));
            }
            for capability in &item.replacement_grounded_capabilities {
                prepared.push(PreparedGroundedCapabilityBinding {
                    occurrence_index: capability.occurrence_index,
                    occurrence_role: capability.occurrence_role,
                    avionics_model_id: replacement_id,
                    requested_quantity: 1,
                    configuration_action: item.configuration_action.clone(),
                    source_notes: capability.source_notes.clone(),
                    seed: capability.seed.clone(),
                    product_fingerprint: capability.seed.product_fingerprint().to_string(),
                    collision_closure_sha256: capability
                        .seed
                        .collision_closure_sha256()
                        .to_string(),
                });
            }
        }
    }
    let mut coordinates = HashSet::new();
    if prepared.iter().any(|capability| {
        !coordinates.insert((capability.occurrence_index, capability.occurrence_role))
    }) {
        return Err(ListingStoreError::State(
            "grounded capability coordinates are not unique".to_string(),
        ));
    }
    Ok(prepared)
}

async fn stage_bound_replay_grounded_capabilities(
    db: &AppDb,
    scope: Option<&GroundedCapabilityReplayScope>,
    avionics: &[ListingAvionicsValue],
) -> StoreResult<()> {
    let prepared = prepare_grounded_capability_bindings(avionics)?;
    if prepared.is_empty() {
        return Ok(());
    }
    let scope = scope.ok_or_else(|| {
        ListingStoreError::State(
            "grounded replay capabilities require one exact retained signed checkpoint".to_string(),
        )
    })?;
    let lock_listing_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            "UPDATE aircraft_sale_listings SET updated_at = updated_at WHERE id = ? RETURNING id",
        ),
        DatabaseBackend::Postgres(_) => {
            db.sql("SELECT id FROM aircraft_sale_listings WHERE id = ? FOR UPDATE")
        }
    };
    let lock_submission_sql = match db.backend() {
        DatabaseBackend::Sqlite(_) => db.sql(
            r#"
            SELECT extracted_listing_json
            FROM plugin_submissions
            WHERE id = ? AND canonical_listing_id = ?
              AND rendered_html_sha256 = ?
              AND extracted_listing_json IS NOT NULL
              AND extraction_error IS NULL
            "#,
        ),
        DatabaseBackend::Postgres(_) => db.sql(
            r#"
            SELECT extracted_listing_json
            FROM plugin_submissions
            WHERE id = ? AND canonical_listing_id = ?
              AND rendered_html_sha256 = ?
              AND extracted_listing_json IS NOT NULL
              AND extraction_error IS NULL
            FOR UPDATE
            "#,
        ),
    };
    let select_existing_sql = db.sql(
        r#"
        SELECT capability.plugin_submission_id, capability.occurrence_index,
               capability.occurrence_role, capability.avionics_model_id,
               capability.requested_quantity, capability.configuration_action,
               capability.request_sha256, capability.capability_sha256,
               capability.grounded_resolution_sha256,
               capability.evidence_capture_sha256,
               capability.extracted_listing_sha256,
               submission.canonical_listing_id AS submission_canonical_listing_id,
               submission.rendered_html_sha256 AS submission_rendered_html_sha256,
               submission.extracted_listing_json,
               submission.extraction_error,
               capability.product_fingerprint,
               capability.collision_closure_sha256,
               capability.source_revocation_count
        FROM aircraft_sale_listing_avionics_grounded_capabilities capability
        JOIN plugin_submissions submission
          ON submission.id = capability.plugin_submission_id
        WHERE capability.listing_id = ?
          AND capability.plugin_submission_id = ?
          AND capability.occurrence_index = ?
          AND capability.occurrence_role = ?
        "#,
    );
    let delete_existing_sql = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_avionics_grounded_capabilities
        WHERE listing_id = ? AND plugin_submission_id = ?
          AND occurrence_index = ? AND occurrence_role = ?
          AND avionics_model_id = ? AND requested_quantity = ?
          AND configuration_action = ? AND request_sha256 = ?
          AND capability_sha256 = ? AND grounded_resolution_sha256 = ?
          AND evidence_capture_sha256 = ? AND extracted_listing_sha256 = ?
          AND product_fingerprint = ? AND collision_closure_sha256 = ?
          AND source_revocation_count = ?
        "#,
    );
    let insert_sql = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics_grounded_capabilities (
          listing_id, plugin_submission_id, occurrence_index, occurrence_role,
          avionics_model_id, requested_quantity, configuration_action,
          request_sha256, capability_sha256, grounded_resolution_sha256,
          evidence_capture_sha256, extracted_listing_sha256,
          product_fingerprint, collision_closure_sha256,
          source_revocation_count
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    );
    let approved_catalog_rows_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let active_collision_rows_sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    macro_rules! stage_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            if matches!(db.backend(), DatabaseBackend::Postgres(_)) {
                sqlx::query(POSTGRES_RESTAGE_CATALOG_LOCK_SQL)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(POSTGRES_LISTING_CHILD_LOCK_SQL)
                    .execute(&mut *transaction)
                    .await?;
            }
            let locked_listing: Option<i64> = sqlx::query_scalar(&lock_listing_sql)
                .bind(scope.listing_id)
                .fetch_optional(&mut *transaction)
                .await?;
            if locked_listing != Some(scope.listing_id) {
                return Err(ListingStoreError::State(format!(
                    "listing {} changed before grounded replay capability staging",
                    scope.listing_id
                )));
            }
            let extracted_listing_json: Option<String> = sqlx::query_scalar(&lock_submission_sql)
                .bind(scope.plugin_submission_id)
                .bind(scope.listing_id)
                .bind(scope.rendered_html_sha256.as_str())
                .fetch_optional(&mut *transaction)
                .await?;
            let extracted_listing_json = extracted_listing_json.ok_or_else(|| {
                ListingStoreError::State(
                    "retained signed checkpoint changed before grounded capability staging"
                        .to_string(),
                )
            })?;
            if format!("{:x}", Sha256::digest(extracted_listing_json.as_bytes()))
                != scope.extracted_listing_sha256
            {
                return Err(ListingStoreError::State(
                    "retained signed extraction hash changed before grounded capability staging"
                        .to_string(),
                ));
            }
            let foreign_scope_count: i64 = sqlx::query_scalar(&db.sql(
                r#"
                SELECT COUNT(*)
                FROM aircraft_sale_listing_avionics_grounded_capabilities
                WHERE listing_id = ?
                  AND (
                    plugin_submission_id <> ?
                    OR evidence_capture_sha256 <> ?
                    OR extracted_listing_sha256 <> ?
                  )
                "#,
            ))
            .bind(scope.listing_id)
            .bind(scope.plugin_submission_id)
            .bind(scope.rendered_html_sha256.as_str())
            .bind(scope.extracted_listing_sha256.as_str())
            .fetch_one(&mut *transaction)
            .await?;
            if foreign_scope_count != 0 {
                return Err(ListingStoreError::State(
                    "pending grounded capabilities do not share the exact replay checkpoint"
                        .to_string(),
                ));
            }
            let catalog_rows =
                sqlx::query_as::<_, CatalogFingerprintRow>(&approved_catalog_rows_sql)
                    .fetch_all(&mut *transaction)
                    .await?;
            let collision_rows = sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(
                &active_collision_rows_sql,
            )
            .fetch_all(&mut *transaction)
            .await?;
            let source_revocation_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM avionics_authoritative_source_origin_revocations",
            )
            .fetch_one(&mut *transaction)
            .await?;
            for capability in &prepared {
                let current_product = catalog_product_fingerprint_from_rows(
                    &catalog_rows,
                    capability.avionics_model_id,
                )
                .ok_or_else(|| {
                    ListingStoreError::State(format!(
                        "grounded replay product {} is no longer approved",
                        capability.avionics_model_id
                    ))
                })?;
                let current_collision = fingerprint_grounded_collision_closure(
                    &collision_rows,
                    capability.avionics_model_id,
                )
                .ok_or_else(|| {
                    ListingStoreError::State(format!(
                        "grounded replay product {} lost its collision closure",
                        capability.avionics_model_id
                    ))
                })?;
                if current_product != capability.product_fingerprint
                    || current_collision != capability.collision_closure_sha256
                    || source_revocation_count != capability.seed.source_revocation_count()
                {
                    return Err(ListingStoreError::State(format!(
                        "grounded replay product {} changed before capability staging",
                        capability.avionics_model_id
                    )));
                }
                let occurrence = ListingGroundedCapability {
                    occurrence_index: capability.occurrence_index,
                    occurrence_role: capability.occurrence_role,
                    configuration_action: capability.configuration_action.clone(),
                    source_notes: capability.source_notes.clone(),
                    seed: capability.seed.clone(),
                };
                let capability_sha256 = grounded_occurrence_capability_sha256(&occurrence);
                let grounded_resolution_sha256 = capability
                    .seed
                    .bind(scope.listing_id)
                    .resolution_sha256()
                    .to_string();
                let existing =
                    sqlx::query_as::<_, StoredGroundedCapabilityRow>(&select_existing_sql)
                        .bind(scope.listing_id)
                        .bind(scope.plugin_submission_id)
                        .bind(capability.occurrence_index as i64)
                        .bind(capability.occurrence_role.as_str())
                        .fetch_optional(&mut *transaction)
                        .await?;
                if let Some(existing) = existing {
                    let exact = existing.avionics_model_id == capability.avionics_model_id
                        && existing.requested_quantity == capability.requested_quantity
                        && existing.configuration_action == capability.configuration_action
                        && existing.request_sha256 == capability.seed.request_sha256()
                        && existing.capability_sha256 == capability_sha256
                        && existing.grounded_resolution_sha256 == grounded_resolution_sha256
                        && existing.evidence_capture_sha256 == scope.rendered_html_sha256
                        && existing.extracted_listing_sha256 == scope.extracted_listing_sha256
                        && existing.product_fingerprint == current_product
                        && existing.collision_closure_sha256 == current_collision
                        && existing.source_revocation_count == source_revocation_count;
                    if exact {
                        continue;
                    }
                    let deleted = sqlx::query(&delete_existing_sql)
                        .bind(scope.listing_id)
                        .bind(existing.plugin_submission_id)
                        .bind(existing.occurrence_index)
                        .bind(existing.occurrence_role.as_str())
                        .bind(existing.avionics_model_id)
                        .bind(existing.requested_quantity)
                        .bind(existing.configuration_action.as_str())
                        .bind(existing.request_sha256.as_str())
                        .bind(existing.capability_sha256.as_str())
                        .bind(existing.grounded_resolution_sha256.as_str())
                        .bind(existing.evidence_capture_sha256.as_str())
                        .bind(existing.extracted_listing_sha256.as_str())
                        .bind(existing.product_fingerprint.as_str())
                        .bind(existing.collision_closure_sha256.as_str())
                        .bind(existing.source_revocation_count)
                        .execute(&mut *transaction)
                        .await?;
                    if deleted.rows_affected() != 1 {
                        return Err(ListingStoreError::State(
                            "stale grounded capability changed during exact replacement"
                                .to_string(),
                        ));
                    }
                }
                sqlx::query(&insert_sql)
                    .bind(scope.listing_id)
                    .bind(scope.plugin_submission_id)
                    .bind(capability.occurrence_index as i64)
                    .bind(capability.occurrence_role.as_str())
                    .bind(capability.avionics_model_id)
                    .bind(capability.requested_quantity)
                    .bind(capability.configuration_action.as_str())
                    .bind(capability.seed.request_sha256())
                    .bind(capability_sha256)
                    .bind(grounded_resolution_sha256)
                    .bind(scope.rendered_html_sha256.as_str())
                    .bind(scope.extracted_listing_sha256.as_str())
                    .bind(current_product)
                    .bind(current_collision)
                    .bind(source_revocation_count)
                    .execute(&mut *transaction)
                    .await?;
            }
            transaction.commit().await?;
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => stage_in_transaction!(pool),
        DatabaseBackend::Postgres(pool) => stage_in_transaction!(pool),
    }
    Ok(())
}

async fn retire_exact_bound_grounded_capabilities(
    db: &AppDb,
    scope: &GroundedCapabilityReplayScope,
) -> StoreResult<()> {
    execute_query!(
        db,
        r#"
        DELETE FROM aircraft_sale_listing_avionics_grounded_capabilities
        WHERE listing_id = ?
          AND plugin_submission_id = ?
          AND evidence_capture_sha256 = ?
          AND extracted_listing_sha256 = ?
        "#,
        scope.listing_id,
        scope.plugin_submission_id,
        scope.rendered_html_sha256.as_str(),
        scope.extracted_listing_sha256.as_str(),
    )?;
    Ok(())
}

async fn stage_existing_listing_signed_source_grounded_capabilities(
    db: &AppDb,
    listing_id: i64,
    binding: Option<&SignedSourceListingBinding>,
    avionics: &[ListingAvionicsValue],
) -> StoreResult<()> {
    let grounded_capabilities = prepare_grounded_capability_bindings(avionics)?;
    if grounded_capabilities.is_empty() && binding.is_none() {
        return Ok(());
    }
    let binding = binding.ok_or_else(|| {
        ListingStoreError::State(
            "grounded existing-listing update requires its exact signed source binding".to_string(),
        )
    })?;
    let expected_sha256 = binding
        .expected_extracted_listing_json
        .as_deref()
        .map(|checkpoint| format!("{:x}", Sha256::digest(checkpoint.as_bytes())));
    if expected_sha256 != binding.expected_extracted_listing_sha256
        || format!(
            "{:x}",
            Sha256::digest(binding.bound_extracted_listing_json.as_bytes())
        ) != binding.bound_extracted_listing_sha256
    {
        return Err(ListingStoreError::State(
            "signed source binding does not match its exact extraction checkpoint".to_string(),
        ));
    }
    let bind_sql = db.sql(match db.backend() {
        DatabaseBackend::Sqlite(_) => {
            r#"
            UPDATE plugin_submissions
            SET canonical_listing_id = ?, extracted_listing_json = ?, extraction_error = NULL
            WHERE id = ? AND user_id = ? AND plugin_install_id = ?
              AND source_url = ? AND submitted_at = ?
              AND rendered_html = ? AND rendered_html_sha256 = ?
              AND signature_base64 = ? AND canonical_listing_id IS NULL
              AND extracted_listing_json IS ? AND extraction_error IS ?
            "#
        }
        DatabaseBackend::Postgres(_) => {
            r#"
            UPDATE plugin_submissions
            SET canonical_listing_id = ?, extracted_listing_json = ?, extraction_error = NULL
            WHERE id = ? AND user_id = ? AND plugin_install_id = ?
              AND source_url = ? AND submitted_at = ?
              AND rendered_html = ? AND rendered_html_sha256 = ?
              AND signature_base64 = ? AND canonical_listing_id IS NULL
              AND extracted_listing_json IS NOT DISTINCT FROM ?
              AND extraction_error IS NOT DISTINCT FROM ?
            "#
        }
    });
    let exact_bound_sql = db.sql(match db.backend() {
        DatabaseBackend::Sqlite(_) => {
            r#"
            SELECT COUNT(*) FROM plugin_submissions
            WHERE id = ? AND user_id = ? AND plugin_install_id = ?
              AND canonical_listing_id = ? AND source_url = ? AND submitted_at = ?
              AND rendered_html = ? AND rendered_html_sha256 = ?
              AND signature_base64 = ? AND extracted_listing_json = ?
              AND extraction_error IS NULL
            "#
        }
        DatabaseBackend::Postgres(_) => {
            r#"
            SELECT COUNT(*) FROM plugin_submissions
            WHERE id = ? AND user_id = ? AND plugin_install_id = ?
              AND canonical_listing_id = ? AND source_url = ? AND submitted_at = ?
              AND rendered_html = ? AND rendered_html_sha256 = ?
              AND signature_base64 = ? AND extracted_listing_json = ?
              AND extraction_error IS NULL
            "#
        }
    });
    let install_sql = db.sql(match db.backend() {
        DatabaseBackend::Sqlite(_) => {
            r#"
            SELECT id FROM plugin_installs
            WHERE id = ? AND user_id = ? AND public_key_base64 = ?
              AND revoked_at IS ?
              AND (revoked_at IS NULL OR julianday(?) <= julianday(revoked_at))
            "#
        }
        DatabaseBackend::Postgres(_) => {
            r#"
            SELECT id FROM plugin_installs
            WHERE id = ? AND user_id = ? AND public_key_base64 = ?
              AND revoked_at IS NOT DISTINCT FROM ?
              AND (revoked_at IS NULL OR CAST(? AS TIMESTAMPTZ) <= CAST(revoked_at AS TIMESTAMPTZ))
            FOR SHARE
            "#
        }
    });
    let listing_lock_sql = db.sql(match db.backend() {
        DatabaseBackend::Sqlite(_) | DatabaseBackend::Postgres(_) => {
            "UPDATE aircraft_sale_listings SET source_url = ? WHERE id = ? RETURNING id"
        }
    });
    macro_rules! bind_in_transaction {
        ($pool:expr) => {{
            let mut transaction = $pool.begin().await?;
            let locked_listing: Option<i64> = sqlx::query_scalar(&listing_lock_sql)
                .bind(binding.source_url.as_str())
                .bind(listing_id)
                .fetch_optional(&mut *transaction)
                .await?;
            if locked_listing != Some(listing_id) {
                return Err(ListingStoreError::State(format!(
                    "existing listing {listing_id} changed before signed capability binding"
                )));
            }
            let install_id: Option<i64> = sqlx::query_scalar(&install_sql)
                .bind(binding.plugin_install_id)
                .bind(binding.user_id)
                .bind(binding.install_public_key_base64.as_str())
                .bind(binding.install_revoked_at.as_deref())
                .bind(binding.submitted_at.as_str())
                .fetch_optional(&mut *transaction)
                .await?;
            if install_id != Some(binding.plugin_install_id) {
                return Err(ListingStoreError::State(
                    "signed source install changed before existing-listing capability binding"
                        .to_string(),
                ));
            }
            let bound = sqlx::query(&bind_sql)
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
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if bound != 1 {
                let exact_count: i64 = sqlx::query_scalar(&exact_bound_sql)
                    .bind(binding.submission_id)
                    .bind(binding.user_id)
                    .bind(binding.plugin_install_id)
                    .bind(listing_id)
                    .bind(binding.source_url.as_str())
                    .bind(binding.submitted_at.as_str())
                    .bind(binding.rendered_html.as_str())
                    .bind(binding.rendered_html_sha256.as_str())
                    .bind(binding.signature_base64.as_str())
                    .bind(binding.bound_extracted_listing_json.as_str())
                    .fetch_one(&mut *transaction)
                    .await?;
                if exact_count != 1 {
                    return Err(ListingStoreError::State(
                        "signed source changed before existing-listing capability binding"
                            .to_string(),
                    ));
                }
            }
            transaction.commit().await?;
            Ok::<_, ListingStoreError>(())
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => bind_in_transaction!(pool)?,
        DatabaseBackend::Postgres(pool) => bind_in_transaction!(pool)?,
    }
    let scope = GroundedCapabilityReplayScope {
        listing_id,
        plugin_submission_id: binding.submission_id,
        rendered_html_sha256: binding.rendered_html_sha256.clone(),
        extracted_listing_sha256: binding.bound_extracted_listing_sha256.clone(),
        allow_provider_fallback: true,
    };
    stage_bound_replay_grounded_capabilities(db, Some(&scope), avionics).await
}

async fn insert_listing(
    db: &AppDb,
    user_id: i64,
    values: &ListingValues,
    literal_identity_values: &ListingValues,
    source_identity_receipt_pending: bool,
    signed_source_binding: Option<&SignedSourceListingBinding>,
    source_visual_correction: Option<&SourceVisualRegistrationCorrection>,
) -> StoreResult<i64> {
    let aircraft_model_variant_id = pending_aircraft_compatibility_variant_id(db).await?;
    let installed_engine_model_id = resolve_installed_engine_model_id(db, values).await?;
    let installed_propeller_model_id = resolve_installed_propeller_model_id(db, values).await?;
    let grounded_capabilities = prepare_grounded_capability_bindings(&values.avionics)?;
    if !grounded_capabilities.is_empty() && signed_source_binding.is_none() {
        return Err(ListingStoreError::State(
            "grounded listing capabilities require an exact signed source binding".to_string(),
        ));
    }
    let pinned_visual_artifact = match (signed_source_binding, source_visual_correction) {
        (Some(binding), Some(correction)) => {
            Some(pinned_source_visual_artifact(binding, correction)?)
        }
        (None, Some(_)) => {
            return Err(ListingStoreError::State(
                "visual source correction cannot be pinned without its signed capture".to_string(),
            ))
        }
        _ => None,
    };
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
                  AND julianday(plugin_submissions.submitted_at) IS NOT NULL
                  AND (
                    exact_install.revoked_at IS NULL
                    OR (
                      julianday(exact_install.revoked_at) IS NOT NULL
                      AND julianday(plugin_submissions.submitted_at)
                        <= julianday(exact_install.revoked_at)
                    )
                  )
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
                  AND CAST(plugin_submissions.submitted_at AS TIMESTAMPTZ) IS NOT NULL
                  AND (
                    exact_install.revoked_at IS NULL
                    OR CAST(plugin_submissions.submitted_at AS TIMESTAMPTZ)
                      <= CAST(exact_install.revoked_at AS TIMESTAMPTZ)
                  )
              )
            "#
        }
    });
    let lock_exact_install_sql = db.sql(match db.backend() {
        DatabaseBackend::Sqlite(_) => {
            r#"
            SELECT id FROM plugin_installs
            WHERE id = ? AND user_id = ? AND public_key_base64 = ?
              AND revoked_at IS ?
              AND julianday(?) IS NOT NULL
              AND (
                revoked_at IS NULL
                OR (julianday(revoked_at) IS NOT NULL AND julianday(?) <= julianday(revoked_at))
              )
            "#
        }
        DatabaseBackend::Postgres(_) => {
            r#"
            SELECT id FROM plugin_installs
            WHERE id = ? AND user_id = ? AND public_key_base64 = ?
              AND revoked_at IS NOT DISTINCT FROM ?
              AND CAST(? AS TIMESTAMPTZ) IS NOT NULL
              AND (
                revoked_at IS NULL
                OR CAST(? AS TIMESTAMPTZ) <= CAST(revoked_at AS TIMESTAMPTZ)
              )
            FOR SHARE
            "#
        }
    });
    let insert_grounded_capability_sql = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics_grounded_capabilities (
          listing_id, plugin_submission_id, occurrence_index, occurrence_role,
          avionics_model_id, requested_quantity, configuration_action,
          request_sha256, capability_sha256, grounded_resolution_sha256,
          evidence_capture_sha256, extracted_listing_sha256,
          product_fingerprint, collision_closure_sha256,
          source_revocation_count
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    );
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
            if pinned_visual_artifact.is_some()
                && matches!(db.backend(), DatabaseBackend::Postgres(_))
            {
                sqlx::query(
                    "LOCK TABLE faa_registry_snapshots, faa_registry_coverage, \
                     faa_registry_aircraft IN SHARE MODE",
                )
                .execute(&mut *transaction)
                .await?;
            }
            if let Some(binding) = signed_source_binding {
                let install_id = sqlx::query_scalar::<_, i64>(&lock_exact_install_sql)
                    .bind(binding.plugin_install_id)
                    .bind(binding.user_id)
                    .bind(binding.install_public_key_base64.as_str())
                    .bind(binding.install_revoked_at.as_deref())
                    .bind(binding.submitted_at.as_str())
                    .bind(binding.submitted_at.as_str())
                    .fetch_optional(&mut *transaction)
                    .await?;
                if install_id != Some(binding.plugin_install_id) {
                    return Err(ListingStoreError::State(
                        "signed source install changed before its listing could be atomically bound"
                            .to_string(),
                    ));
                }
            }
            if let Some(artifact) = pinned_visual_artifact.as_ref() {
                let pair_count = sqlx::query_scalar::<_, i64>(&db.sql(
                    r#"
                    SELECT COUNT(*)
                    FROM faa_registry_snapshots snapshot
                    JOIN faa_registry_coverage observed
                      ON observed.snapshot_id = snapshot.id
                     AND observed.n_number = ?
                     AND observed.lookup_status = 'absent'
                    JOIN faa_registry_coverage corrected
                      ON corrected.snapshot_id = snapshot.id
                     AND corrected.n_number = ?
                     AND corrected.lookup_status = 'matched'
                    JOIN faa_registry_aircraft aircraft
                      ON aircraft.snapshot_id = snapshot.id
                     AND aircraft.n_number = corrected.n_number
                    WHERE snapshot.id = ?
                      AND snapshot.id = (
                        SELECT id FROM faa_registry_snapshots
                        ORDER BY snapshot_date DESC, id DESC LIMIT 1
                      )
                      AND snapshot.archive_sha256 = ?
                      AND aircraft.source_record_sha256 = ?
                      AND (
                        aircraft.manufacturer_serial_raw = ?
                        OR (aircraft.manufacturer_serial_raw IS NULL AND ? IS NULL)
                      )
                    "#,
                ))
                .bind(artifact.observed_registration_number.as_str())
                .bind(artifact.corrected_registration_number.as_str())
                .bind(artifact.faa_registry_snapshot_id)
                .bind(artifact.faa_snapshot_archive_sha256.as_str())
                .bind(artifact.faa_source_record_sha256.as_str())
                .bind(artifact.corrected_serial_number.as_deref())
                .bind(artifact.corrected_serial_number.as_deref())
                .fetch_one(&mut *transaction)
                .await?;
                if pair_count != 1 {
                    return Err(ListingStoreError::State(
                        "visual correction FAA absence/match pair changed before atomic binding"
                            .to_string(),
                    ));
                }
                sqlx::query(&db.sql(
                    r#"
                    INSERT INTO aircraft_source_visual_correction_artifacts (
                      plugin_submission_id, rendered_html_sha256,
                      observed_registration_number, corrected_registration_number,
                      corrected_serial_number, faa_registry_snapshot_id,
                      faa_snapshot_archive_sha256, faa_source_record_sha256,
                      primary_photo_asset_id, primary_photo_url, primary_photo_sha256,
                      visual_resolution_sha256, visual_resolution_json
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                ))
                .bind(artifact.plugin_submission_id)
                .bind(artifact.rendered_html_sha256.as_str())
                .bind(artifact.observed_registration_number.as_str())
                .bind(artifact.corrected_registration_number.as_str())
                .bind(artifact.corrected_serial_number.as_deref())
                .bind(artifact.faa_registry_snapshot_id)
                .bind(artifact.faa_snapshot_archive_sha256.as_str())
                .bind(artifact.faa_source_record_sha256.as_str())
                .bind(artifact.primary_photo_asset_id.as_str())
                .bind(artifact.primary_photo_url.as_str())
                .bind(artifact.primary_photo_sha256.as_str())
                .bind(artifact.visual_resolution_sha256.as_str())
                .bind(artifact.visual_resolution_json.as_str())
                .execute(&mut *transaction)
                .await?;
            }
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
                .await
                .map_err(listing_insert_error)?;
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
                for capability in &grounded_capabilities {
                    let receipt = capability.seed.bind(listing_id);
                    sqlx::query(&insert_grounded_capability_sql)
                        .bind(listing_id)
                        .bind(binding.submission_id)
                        .bind(capability.occurrence_index as i64)
                        .bind(capability.occurrence_role.as_str())
                        .bind(capability.avionics_model_id)
                        .bind(capability.requested_quantity)
                        .bind(capability.configuration_action.as_str())
                        .bind(capability.seed.request_sha256())
                        .bind(grounded_occurrence_capability_sha256(&ListingGroundedCapability {
                            occurrence_index: capability.occurrence_index,
                            occurrence_role: capability.occurrence_role,
                            configuration_action: capability.configuration_action.clone(),
                            source_notes: capability.source_notes.clone(),
                            seed: capability.seed.clone(),
                        }))
                        .bind(receipt.resolution_sha256())
                        .bind(binding.rendered_html_sha256.as_str())
                        .bind(binding.bound_extracted_listing_sha256.as_str())
                        .bind(capability.product_fingerprint.as_str())
                        .bind(capability.collision_closure_sha256.as_str())
                        .bind(capability.seed.source_revocation_count())
                        .execute(&mut *transaction)
                        .await?;
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
    if let Err(error) = Box::pin(replace_listing_avionics(db, listing_id, &values.avionics)).await {
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

fn listing_insert_error(error: sqlx::Error) -> ListingStoreError {
    let source_claim_conflict = match &error {
        sqlx::Error::Database(database) => {
            database.constraint() == Some("uq_aircraft_sale_listings_owner_source")
                || database.message().contains(
                    "aircraft_sale_listings.created_by_user_id, aircraft_sale_listings.source_url",
                )
        }
        _ => false,
    };
    if source_claim_conflict {
        ListingStoreError::State("listing source is already claimed by this owner".to_string())
    } else {
        error.into()
    }
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
        if let Err(error) =
            Box::pin(replace_listing_avionics(db, listing_id, &values.avionics)).await
        {
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
    source_evidence_units: Option<&ListingEvidenceUnits>,
    replay_scope: Option<&GroundedCapabilityReplayScope>,
    exact_source_capture_scope: Option<&ExactListingSourceCaptureScope>,
) -> StoreResult<ResolvedListingAvionics> {
    let retained_listing_context = listing_context.unwrap_or_default();
    let mut resolved: Vec<ListingAvionicsValue> = Vec::new();
    let mut pending = Vec::new();
    let mut dispositions = Vec::new();

    for (index, item) in values.avionics.clone().into_iter().enumerate() {
        let occurrence_evidence = exact_occurrence_evidence_from_source_units(
            source_evidence_units,
            item.source_notes.as_deref(),
        );
        let controller_run_on_line = source_url.is_some_and(|source_url| {
            controller_extraction_source_has_exact_avionics_line(
                source_url,
                retained_listing_context,
                occurrence_evidence,
            )
        });
        let exact_leading_dual_proof =
            source_url
                .zip(exact_source_capture_scope)
                .and_then(|(source_url, capture)| {
                    exact_controller_leading_dual_evidence_proof(
                        source_url,
                        retained_listing_context,
                        capture.plugin_submission_id,
                        &capture.rendered_html_sha256,
                        &capture.extracted_listing_sha256,
                        &ListingAvionicsEvidenceObservation {
                            manufacturer: item.manufacturer.as_deref(),
                            model: &item.model,
                            avionics_types: &item.avionics_types,
                            quantity: item.quantity,
                            configuration_action: &item.configuration_action,
                            source_confidence: item.source_confidence.as_deref(),
                            source_evidence_text: item.source_notes.as_deref(),
                        },
                    )
                });
        let primary = resolve_listing_avionics_observation(
            db,
            values,
            extractor,
            source_url,
            occurrence_evidence,
            item.manufacturer.as_deref(),
            &item.model,
            &item.avionics_types,
            item.quantity,
            item.source_confidence.as_deref(),
            item.source_notes.as_deref(),
            replay_scope,
            index,
            OccurrenceRole::Primary,
            item.configuration_action.as_str(),
            controller_run_on_line,
            exact_leading_dual_proof,
        )
        .await;
        let primary = match primary {
            ListingAvionicsIdentityResolution::Approved {
                identity,
                grounded_receipt_seed: None,
                ..
            } if exact_source_capture_scope.is_none() && extractor.is_none() => {
                ListingAvionicsIdentityResolution::Pending {
                    suggested_product: Some(review_product_from_identity(&identity)),
                    reason: AVIONICS_SIGNED_CHECKPOINT_REQUIRED_REASON.to_string(),
                }
            }
            outcome => outcome,
        };

        match primary {
            ListingAvionicsIdentityResolution::DeterministicGenericRejected
            | ListingAvionicsIdentityResolution::GroundedRejected { .. } => {
                // High-confidence garbage never enters either the canonical
                // catalog or the review queue.
                dispositions.push(AutomaticOccurrenceDisposition::discarded(
                    index,
                    OccurrenceRole::Primary,
                ));
                match resolve_listing_avionics_replacement(
                    db,
                    values,
                    extractor,
                    source_url,
                    occurrence_evidence,
                    controller_run_on_line,
                    index,
                    &item,
                    replay_scope,
                )
                .await?
                {
                    ListingAvionicsReplacementResolution::None => {}
                    ListingAvionicsReplacementResolution::Rejected => {
                        dispositions.push(AutomaticOccurrenceDisposition::discarded(
                            index,
                            OccurrenceRole::Replacement,
                        ));
                    }
                    ListingAvionicsReplacementResolution::Approved { identity, .. } => {
                        dispositions.push(AutomaticOccurrenceDisposition::linked(
                            index,
                            OccurrenceRole::Replacement,
                            identity.id,
                        ));
                    }
                    ListingAvionicsReplacementResolution::Pending(aspect) => {
                        pending.push(*aspect);
                    }
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
                    occurrence_evidence,
                    controller_run_on_line,
                    index,
                    &item,
                    replay_scope,
                )
                .await?;
                let (replaces_product_id, replacement_aspect_id) = match &replacement {
                    ListingAvionicsReplacementResolution::None => (None, None),
                    ListingAvionicsReplacementResolution::Rejected => {
                        dispositions.push(AutomaticOccurrenceDisposition::discarded(
                            index,
                            OccurrenceRole::Replacement,
                        ));
                        (None, None)
                    }
                    ListingAvionicsReplacementResolution::Approved { identity, .. } => {
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
                let pending_item =
                    if matches!(&replacement, ListingAvionicsReplacementResolution::Rejected) {
                        if item.configuration_action == "removes" {
                            item.clone()
                        } else {
                            listing_avionics_without_replacement(&item)
                        }
                    } else {
                        item.clone()
                    };
                let self_removal_target =
                    matches!(&replacement, ListingAvionicsReplacementResolution::Rejected)
                        .then_some(())
                        .filter(|_| item.configuration_action == "removes")
                        .and_then(|_| suggested_product.as_ref().and_then(|product| product.id));
                let self_removal_aspect_id =
                    (matches!(&replacement, ListingAvionicsReplacementResolution::Rejected)
                        && item.configuration_action == "removes"
                        && self_removal_target.is_none())
                    .then(|| ReviewAspectId::String(format!("avionics:{index}:removal-product")));
                pending.push(pending_avionics_aspect(
                    ReviewAspectId::String(format!("avionics:{index}:primary")),
                    &pending_item,
                    reason.clone(),
                    suggested_product.clone(),
                    self_removal_target.or(replaces_product_id),
                    self_removal_aspect_id.clone().or(replacement_aspect_id),
                ));
                if let Some(self_removal_aspect_id) = self_removal_aspect_id {
                    let self_removal_target_item = listing_avionics_without_replacement(&item);
                    pending.push(pending_avionics_aspect(
                        self_removal_aspect_id,
                        &self_removal_target_item,
                        reason,
                        suggested_product,
                        None,
                        None,
                    ));
                }
                if let ListingAvionicsReplacementResolution::Pending(aspect) = replacement {
                    pending.push(*aspect);
                }
            }
            ListingAvionicsIdentityResolution::Approved {
                identity,
                grounded_receipt_seed,
                source_confidence_basis,
            } => {
                let replacement = resolve_listing_avionics_replacement(
                    db,
                    values,
                    extractor,
                    source_url,
                    occurrence_evidence,
                    controller_run_on_line,
                    index,
                    &item,
                    replay_scope,
                )
                .await?;
                match replacement {
                    ListingAvionicsReplacementResolution::None => {
                        dispositions.push(AutomaticOccurrenceDisposition::linked(
                            index,
                            OccurrenceRole::Primary,
                            identity.id,
                        ));
                        let mut resolved_item = listing_avionics_value_from_catalog(
                            &item,
                            &identity,
                            source_confidence_basis.clone(),
                        );
                        if let Some(seed) = grounded_receipt_seed {
                            resolved_item
                                .grounded_capabilities
                                .push(ListingGroundedCapability {
                                    occurrence_index: index,
                                    occurrence_role: OccurrenceRole::Primary,
                                    configuration_action: resolved_item
                                        .configuration_action
                                        .clone(),
                                    source_notes: item.source_notes.clone(),
                                    seed,
                                });
                        }
                        resolved.push(resolved_item);
                    }
                    ListingAvionicsReplacementResolution::Rejected => {
                        dispositions.push(AutomaticOccurrenceDisposition::linked(
                            index,
                            OccurrenceRole::Primary,
                            identity.id,
                        ));
                        dispositions.push(AutomaticOccurrenceDisposition::discarded(
                            index,
                            OccurrenceRole::Replacement,
                        ));
                        let mut resolved_item = listing_avionics_after_generic_target_rejection(
                            &item,
                            &identity,
                            source_confidence_basis.clone(),
                        );
                        if let Some(seed) = grounded_receipt_seed {
                            resolved_item
                                .grounded_capabilities
                                .push(ListingGroundedCapability {
                                    occurrence_index: index,
                                    occurrence_role: OccurrenceRole::Primary,
                                    configuration_action: resolved_item
                                        .configuration_action
                                        .clone(),
                                    source_notes: item.source_notes.clone(),
                                    seed,
                                });
                        }
                        resolved.push(resolved_item);
                    }
                    ListingAvionicsReplacementResolution::Approved {
                        identity: replaced,
                        grounded_receipt_seed: replacement_seed,
                    } => {
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
                        let mut resolved_item = listing_avionics_value_from_catalog(
                            &item,
                            &identity,
                            source_confidence_basis.clone(),
                        );
                        if let Some(seed) = grounded_receipt_seed {
                            resolved_item
                                .grounded_capabilities
                                .push(ListingGroundedCapability {
                                    occurrence_index: index,
                                    occurrence_role: OccurrenceRole::Primary,
                                    configuration_action: resolved_item
                                        .configuration_action
                                        .clone(),
                                    source_notes: item.source_notes.clone(),
                                    seed,
                                });
                        }
                        resolved_item.replaces = Some(ParsedAvionicsReference {
                            manufacturer: Some(replaced.manufacturer),
                            model: replaced.model,
                            avionics_types: replaced.avionics_types,
                        });
                        resolved_item.replaces_avionics_model_id = Some(replaced.id);
                        if let Some(seed) = replacement_seed {
                            resolved_item.replacement_grounded_capabilities.push(
                                ListingGroundedCapability {
                                    occurrence_index: index,
                                    occurrence_role: OccurrenceRole::Replacement,
                                    configuration_action: resolved_item
                                        .configuration_action
                                        .clone(),
                                    source_notes: item.source_notes.clone(),
                                    seed,
                                },
                            );
                        }
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

    values.avionics = require_unique_resolved_listing_avionics(resolved)?;
    Ok(ResolvedListingAvionics {
        pending_review_aspects: pending,
        occurrence_dispositions: dispositions,
    })
}

enum ListingAvionicsIdentityResolution {
    Approved {
        identity: ApprovedAvionicsIdentity,
        grounded_receipt_seed: Option<GroundedAvionicsResolutionReceiptSeed>,
        source_confidence_basis: ListingSourceConfidenceBasis,
    },
    DeterministicGenericRejected,
    GroundedRejected {
        reason: String,
    },
    Pending {
        reason: String,
        suggested_product: Option<ReviewProduct>,
    },
}

async fn retire_exact_stale_grounded_capability(
    db: &AppDb,
    listing_id: i64,
    row: &StoredGroundedCapabilityRow,
) -> Result<(), String> {
    let sql = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_avionics_grounded_capabilities
        WHERE listing_id = ?
          AND plugin_submission_id = ?
          AND occurrence_index = ?
          AND occurrence_role = ?
          AND avionics_model_id = ?
          AND requested_quantity = ?
          AND configuration_action = ?
          AND request_sha256 = ?
          AND capability_sha256 = ?
          AND grounded_resolution_sha256 = ?
          AND evidence_capture_sha256 = ?
          AND extracted_listing_sha256 = ?
          AND product_fingerprint = ?
          AND collision_closure_sha256 = ?
          AND source_revocation_count = ?
        "#,
    );
    macro_rules! delete_exact {
        ($pool:expr) => {{
            sqlx::query(&sql)
                .bind(listing_id)
                .bind(row.plugin_submission_id)
                .bind(row.occurrence_index)
                .bind(row.occurrence_role.as_str())
                .bind(row.avionics_model_id)
                .bind(row.requested_quantity)
                .bind(row.configuration_action.as_str())
                .bind(row.request_sha256.as_str())
                .bind(row.capability_sha256.as_str())
                .bind(row.grounded_resolution_sha256.as_str())
                .bind(row.evidence_capture_sha256.as_str())
                .bind(row.extracted_listing_sha256.as_str())
                .bind(row.product_fingerprint.as_str())
                .bind(row.collision_closure_sha256.as_str())
                .bind(row.source_revocation_count)
                .execute($pool)
                .await
                .map(|result| result.rows_affected())
        }};
    }
    let deleted = match db.backend() {
        DatabaseBackend::Sqlite(pool) => delete_exact!(pool),
        DatabaseBackend::Postgres(pool) => delete_exact!(pool),
    }
    .map_err(|error| error.to_string())?;
    if deleted != 1 {
        return Err(
            "pending grounded capability changed concurrently while retiring stale proof"
                .to_string(),
        );
    }
    Ok(())
}

async fn replay_grounded_listing_avionics_identity(
    db: &AppDb,
    scope: &GroundedCapabilityReplayScope,
    request: &AvionicsIdentityRequest,
    occurrence_index: usize,
    occurrence_role: OccurrenceRole,
    configuration_action: &str,
    source_notes: Option<&str>,
) -> Result<GroundedCapabilityReplayOutcome, String> {
    let sql = db.sql(
        r#"
        SELECT capability.plugin_submission_id, capability.occurrence_index,
               capability.occurrence_role, capability.avionics_model_id,
               capability.requested_quantity, capability.configuration_action,
               capability.request_sha256, capability.capability_sha256,
               capability.grounded_resolution_sha256,
               capability.evidence_capture_sha256,
               capability.extracted_listing_sha256,
               submission.canonical_listing_id AS submission_canonical_listing_id,
               submission.rendered_html_sha256 AS submission_rendered_html_sha256,
               submission.extracted_listing_json,
               submission.extraction_error,
               capability.product_fingerprint,
               capability.collision_closure_sha256,
               capability.source_revocation_count
        FROM aircraft_sale_listing_avionics_grounded_capabilities capability
        LEFT JOIN plugin_submissions submission
          ON submission.id = capability.plugin_submission_id
        WHERE capability.listing_id = ?
          AND capability.plugin_submission_id = ?
          AND capability.occurrence_index = ?
          AND capability.occurrence_role = ?
        "#,
    );
    let row = match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            sqlx::query_as::<_, StoredGroundedCapabilityRow>(&sql)
                .bind(scope.listing_id)
                .bind(scope.plugin_submission_id)
                .bind(occurrence_index as i64)
                .bind(occurrence_role.as_str())
                .fetch_optional(pool)
                .await
        }
        DatabaseBackend::Postgres(pool) => {
            sqlx::query_as::<_, StoredGroundedCapabilityRow>(&sql)
                .bind(scope.listing_id)
                .bind(scope.plugin_submission_id)
                .bind(occurrence_index as i64)
                .bind(occurrence_role.as_str())
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|error| error.to_string())?;
    let Some(row) = row else {
        return Ok(GroundedCapabilityReplayOutcome::Absent);
    };
    if row.evidence_capture_sha256 != scope.rendered_html_sha256
        || row.extracted_listing_sha256 != scope.extracted_listing_sha256
        || !grounded_capability_submission_checkpoint_is_current(scope.listing_id, &row)
    {
        retire_exact_stale_grounded_capability(db, scope.listing_id, &row).await?;
        return Ok(GroundedCapabilityReplayOutcome::RetiredStale);
    }
    let current_source_revocation_count = authoritative_source_revocation_count(db)
        .await
        .map_err(|error| error.to_string())?;
    if row.source_revocation_count != current_source_revocation_count {
        retire_exact_stale_grounded_capability(db, scope.listing_id, &row).await?;
        return Ok(GroundedCapabilityReplayOutcome::RetiredStale);
    }
    let Some(identity) = approved_avionics_identity_for_grounded_replay(db, row.avionics_model_id)
        .await
        .map_err(|error| error.to_string())?
    else {
        retire_exact_stale_grounded_capability(db, scope.listing_id, &row).await?;
        return Ok(GroundedCapabilityReplayOutcome::RetiredStale);
    };
    let seed = grounded_resolution_receipt_basis_for_replay(request, &identity)
        .bind_catalog_snapshot(
            row.product_fingerprint.clone(),
            row.collision_closure_sha256.clone(),
            row.source_revocation_count,
        );
    let current_product = match catalog_product_fingerprint_for_id(db, identity.id).await {
        Ok(fingerprint) => fingerprint,
        Err(AvionicsFingerprintError::Conflict(_)) => {
            retire_exact_stale_grounded_capability(db, scope.listing_id, &row).await?;
            return Ok(GroundedCapabilityReplayOutcome::RetiredStale);
        }
        Err(error) => return Err(error.to_string()),
    };
    let current_collision = match grounded_collision_closure_revision_sha256(db, identity.id).await
    {
        Ok(fingerprint) => fingerprint,
        Err(AvionicsFingerprintError::Conflict(_)) => {
            retire_exact_stale_grounded_capability(db, scope.listing_id, &row).await?;
            return Ok(GroundedCapabilityReplayOutcome::RetiredStale);
        }
        Err(error) => return Err(error.to_string()),
    };
    if row.occurrence_index != occurrence_index as i64
        || row.occurrence_role != occurrence_role.as_str()
        || row.requested_quantity != request.quantity
        || row.configuration_action != configuration_action
        || row.request_sha256 != grounded_resolution_request_sha256(request)
        || row.request_sha256 != seed.request_sha256()
        || row.capability_sha256
            != grounded_occurrence_capability_sha256(&ListingGroundedCapability {
                occurrence_index,
                occurrence_role,
                configuration_action: configuration_action.to_string(),
                source_notes: source_notes.map(str::to_string),
                seed: seed.clone(),
            })
        || row.grounded_resolution_sha256 != seed.bind(scope.listing_id).resolution_sha256()
        || row.product_fingerprint != current_product
        || row.collision_closure_sha256 != current_collision
        || !grounded_capability_submission_checkpoint_is_current(scope.listing_id, &row)
    {
        retire_exact_stale_grounded_capability(db, scope.listing_id, &row).await?;
        return Ok(GroundedCapabilityReplayOutcome::RetiredStale);
    }
    Ok(GroundedCapabilityReplayOutcome::Approved(identity, seed))
}

async fn resolve_listing_avionics_identity(
    db: &AppDb,
    extractor: Option<&GeminiListingExtractor>,
    request: &AvionicsIdentityRequest,
    source_confidence: Option<&str>,
    source_notes: Option<&str>,
    replay_scope: Option<&GroundedCapabilityReplayScope>,
    occurrence_index: usize,
    occurrence_role: OccurrenceRole,
    configuration_action: &str,
    controller_run_on_line: bool,
    exact_leading_dual_proof: Option<ExactControllerLeadingDualEvidenceProof>,
) -> ListingAvionicsIdentityResolution {
    let structure_issue =
        if request.manufacturer.trim().is_empty() || request.model.trim().is_empty() {
            Some("candidate is missing a manufacturer or model label")
        } else if !request
            .avionics_types
            .iter()
            .any(|value| !value.trim().is_empty())
        {
            Some("candidate is missing an avionics capability observation")
        } else if request.quantity < 1 {
            Some("candidate quantity must be at least one")
        } else {
            None
        };
    if let Some(reason) = structure_issue {
        return ListingAvionicsIdentityResolution::Pending {
            reason: reason.to_string(),
            suggested_product: None,
        };
    }
    if deterministic_generic_avionics_rejection_reason(request).is_some() {
        return ListingAvionicsIdentityResolution::DeterministicGenericRejected;
    }
    if let Some(scope) = replay_scope {
        match replay_grounded_listing_avionics_identity(
            db,
            scope,
            request,
            occurrence_index,
            occurrence_role,
            configuration_action,
            source_notes,
        )
        .await
        {
            Ok(GroundedCapabilityReplayOutcome::Approved(identity, grounded_receipt_seed)) => {
                return listing_avionics_identity_resolution::<CatalogError>(
                    Ok(AvionicsIdentityOutcome::Approved(identity)),
                    source_confidence,
                    Some(grounded_receipt_seed),
                );
            }
            Ok(GroundedCapabilityReplayOutcome::Absent) if scope.allow_provider_fallback => {}
            Ok(GroundedCapabilityReplayOutcome::Absent)
            | Ok(GroundedCapabilityReplayOutcome::RetiredStale) => {
                return ListingAvionicsIdentityResolution::Pending {
                    reason: AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON.to_string(),
                    suggested_product: None,
                };
            }
            Err(_) => {
                return ListingAvionicsIdentityResolution::Pending {
                    reason: AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON.to_string(),
                    suggested_product: None,
                };
            }
        }
    }
    if request.listing_context.trim().is_empty() {
        return ListingAvionicsIdentityResolution::Pending {
            reason: AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON.to_string(),
            suggested_product: None,
        };
    }
    match resolve_verified_local_avionics_identity(db, request).await {
        Ok(Some(identity)) => {
            return listing_avionics_local_identity_resolution(
                db,
                identity,
                source_confidence,
                exact_leading_dual_proof,
            )
            .await;
        }
        Ok(None) => {}
        Err(error) => {
            return listing_avionics_identity_resolution(Err(error), source_confidence, None);
        }
    }
    if controller_run_on_line {
        match resolve_verified_local_controller_run_on_avionics_identity(db, request).await {
            Ok(Some(identity)) => {
                return listing_avionics_local_identity_resolution(
                    db,
                    identity,
                    source_confidence,
                    exact_leading_dual_proof,
                )
                .await;
            }
            Ok(None) => {}
            Err(error) => {
                return listing_avionics_identity_resolution(Err(error), source_confidence, None);
            }
        }
    }
    if let Some(extractor) = extractor {
        return match resolve_avionics_identity_for_listing_materialization(db, extractor, request)
            .await
        {
            Ok(resolution) => listing_avionics_identity_resolution(
                Ok::<_, CatalogError>(resolution.outcome),
                source_confidence,
                resolution.grounded_receipt_seed,
            ),
            Err(error) => listing_avionics_identity_resolution(Err(error), source_confidence, None),
        };
    }

    ListingAvionicsIdentityResolution::Pending {
        reason: AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON.to_string(),
        suggested_product: None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_listing_avionics_observation(
    db: &AppDb,
    values: &ListingValues,
    extractor: Option<&GeminiListingExtractor>,
    source_url: Option<&str>,
    listing_context: &str,
    manufacturer: Option<&str>,
    model: &str,
    avionics_types: &[String],
    quantity: i64,
    source_confidence: Option<&str>,
    source_notes: Option<&str>,
    replay_scope: Option<&GroundedCapabilityReplayScope>,
    occurrence_index: usize,
    occurrence_role: OccurrenceRole,
    configuration_action: &str,
    controller_run_on_line: bool,
    exact_leading_dual_proof: Option<ExactControllerLeadingDualEvidenceProof>,
) -> ListingAvionicsIdentityResolution {
    let request = listing_avionics_identity_request(
        values,
        source_url,
        listing_context,
        manufacturer.unwrap_or_default(),
        model,
        avionics_types,
        quantity,
    );
    if !model.trim().is_empty()
        && avionics_types.iter().any(|value| !value.trim().is_empty())
        && quantity >= 1
        && deterministic_generic_avionics_rejection_reason(&request).is_some()
    {
        return ListingAvionicsIdentityResolution::DeterministicGenericRejected;
    }
    let Some(_) = manufacturer else {
        if model.trim().is_empty()
            || avionics_types.iter().all(|value| value.trim().is_empty())
            || quantity < 1
            || listing_context.trim().is_empty()
        {
            return ListingAvionicsIdentityResolution::Pending {
                reason: AVIONICS_MANUFACTURER_REVIEW_REQUIRED_REASON.to_string(),
                suggested_product: None,
            };
        }
        return match resolve_verified_local_avionics_model_observation(
            db,
            model,
            avionics_types,
            listing_context,
        )
        .await
        {
            Ok(Some(identity)) => {
                listing_avionics_local_identity_resolution(
                    db,
                    identity,
                    source_confidence,
                    exact_leading_dual_proof,
                )
                .await
            }
            Ok(None) => match unique_exact_avionics_model_observation_review_candidate(
                db,
                model,
                avionics_types,
                listing_context,
            )
            .await
            {
                Ok(candidate) => ListingAvionicsIdentityResolution::Pending {
                    reason: AVIONICS_MANUFACTURER_REVIEW_REQUIRED_REASON.to_string(),
                    suggested_product: candidate.as_ref().map(review_product_from_candidate),
                },
                Err(error) => {
                    return listing_avionics_identity_resolution(
                        Err(error),
                        source_confidence,
                        None,
                    )
                }
            },
            Err(error) => listing_avionics_identity_resolution(Err(error), source_confidence, None),
        };
    };

    resolve_listing_avionics_identity(
        db,
        extractor,
        &request,
        source_confidence,
        source_notes,
        replay_scope,
        occurrence_index,
        occurrence_role,
        configuration_action,
        controller_run_on_line,
        exact_leading_dual_proof,
    )
    .await
}

async fn listing_avionics_local_identity_resolution(
    db: &AppDb,
    identity: ApprovedAvionicsIdentity,
    source_confidence: Option<&str>,
    exact_leading_dual_proof: Option<ExactControllerLeadingDualEvidenceProof>,
) -> ListingAvionicsIdentityResolution {
    let qualified_explicit_count = source_confidence == Some("medium")
        && exact_leading_dual_proof.is_some()
        && identity.manufacturer_identifier_kind == "manufacturer_part_number"
        && countable_unit_product_reuse_attestation_is_current(db, identity.id)
            .await
            .unwrap_or(false);
    let source_confidence_basis = if source_confidence == Some("high") {
        Some(ListingSourceConfidenceBasis::RetainedHigh)
    } else if qualified_explicit_count {
        Some(
            ListingSourceConfidenceBasis::ExactControllerLeadingDualCountableUnit(
                exact_leading_dual_proof
                    .expect("qualified explicit count has an exact capture-bound proof"),
            ),
        )
    } else {
        None
    };
    if let Some(source_confidence_basis) = source_confidence_basis {
        ListingAvionicsIdentityResolution::Approved {
            identity,
            grounded_receipt_seed: None,
            source_confidence_basis,
        }
    } else {
        ListingAvionicsIdentityResolution::Pending {
            reason: AVIONICS_INSTALLATION_EVIDENCE_NOT_HIGH_REASON.to_string(),
            suggested_product: Some(review_product_from_identity(&identity)),
        }
    }
}

fn listing_avionics_identity_resolution<E>(
    outcome: Result<AvionicsIdentityOutcome, E>,
    source_confidence: Option<&str>,
    grounded_receipt_seed: Option<GroundedAvionicsResolutionReceiptSeed>,
) -> ListingAvionicsIdentityResolution {
    match outcome {
        Ok(AvionicsIdentityOutcome::Approved(identity)) if source_confidence == Some("high") => {
            ListingAvionicsIdentityResolution::Approved {
                identity,
                grounded_receipt_seed,
                source_confidence_basis: ListingSourceConfidenceBasis::RetainedHigh,
            }
        }
        Ok(AvionicsIdentityOutcome::Approved(identity)) => {
            ListingAvionicsIdentityResolution::Pending {
                reason: AVIONICS_INSTALLATION_EVIDENCE_NOT_HIGH_REASON.to_string(),
                suggested_product: Some(review_product_from_identity(&identity)),
            }
        }
        Ok(AvionicsIdentityOutcome::Rejected { reason }) => {
            ListingAvionicsIdentityResolution::GroundedRejected { reason }
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
        quantity,
    }
}

enum ListingAvionicsReplacementResolution {
    None,
    Rejected,
    Approved {
        identity: Box<ApprovedAvionicsIdentity>,
        grounded_receipt_seed: Option<GroundedAvionicsResolutionReceiptSeed>,
    },
    Pending(Box<PendingReviewAspect>),
}

fn listing_avionics_replacement_resolution(
    index: usize,
    replaced: &ParsedAvionicsReference,
    item: &ListingAvionicsValue,
    resolution: ListingAvionicsIdentityResolution,
) -> ListingAvionicsReplacementResolution {
    match resolution {
        ListingAvionicsIdentityResolution::Approved {
            identity,
            grounded_receipt_seed,
            ..
        } => ListingAvionicsReplacementResolution::Approved {
            identity: Box::new(identity),
            grounded_receipt_seed,
        },
        ListingAvionicsIdentityResolution::DeterministicGenericRejected => {
            ListingAvionicsReplacementResolution::Rejected
        }
        ListingAvionicsIdentityResolution::GroundedRejected { reason } => {
            ListingAvionicsReplacementResolution::Pending(Box::new(pending_replacement_aspect(
                index,
                replaced,
                item,
                format!("grounded classification rejected this replacement target: {reason}"),
            )))
        }
        ListingAvionicsIdentityResolution::Pending {
            reason,
            suggested_product,
        } => {
            let mut aspect = pending_replacement_aspect(index, replaced, item, reason);
            aspect.suggested_product = suggested_product;
            ListingAvionicsReplacementResolution::Pending(Box::new(aspect))
        }
    }
}

async fn resolve_listing_avionics_replacement(
    db: &AppDb,
    values: &ListingValues,
    extractor: Option<&GeminiListingExtractor>,
    source_url: Option<&str>,
    listing_context: &str,
    controller_run_on_line: bool,
    index: usize,
    item: &ListingAvionicsValue,
    replay_scope: Option<&GroundedCapabilityReplayScope>,
) -> StoreResult<ListingAvionicsReplacementResolution> {
    if item.configuration_action == "installed" {
        if item.replaces.is_some() || item.replaces_avionics_model_id.is_some() {
            return Err(ListingStoreError::Validation(format!(
                "installed avionics cannot also declare a replacement target: {}",
                avionics_observation_label(item.manufacturer.as_deref(), &item.model)
            )));
        }
        return Ok(ListingAvionicsReplacementResolution::None);
    }
    let Some(replaced) = item.replaces.as_ref() else {
        return Err(ListingStoreError::Validation(format!(
            "avionics action {} requires a concrete replacement target: {}",
            item.configuration_action,
            avionics_observation_label(item.manufacturer.as_deref(), &item.model)
        )));
    };
    let resolution = resolve_listing_avionics_observation(
        db,
        values,
        extractor,
        source_url,
        listing_context,
        replaced.manufacturer.as_deref(),
        &replaced.model,
        &replaced.avionics_types,
        1,
        item.source_confidence.as_deref(),
        item.source_notes.as_deref(),
        replay_scope,
        index,
        OccurrenceRole::Replacement,
        item.configuration_action.as_str(),
        controller_run_on_line,
        None,
    )
    .await;
    Ok(listing_avionics_replacement_resolution(
        index, replaced, item, resolution,
    ))
}

fn pending_avionics_aspect(
    id: ReviewAspectId,
    item: &ListingAvionicsValue,
    reason: String,
    suggested_product: Option<ReviewProduct>,
    replaces_product_id: Option<i64>,
    replacement_aspect_id: Option<ReviewAspectId>,
) -> PendingReviewAspect {
    let proposed_product = item.manufacturer.as_deref().map(|manufacturer| {
        review_product_from_observation(manufacturer, &item.model, &item.avionics_types)
    });
    let mut allowed_actions = vec![ReviewAction::UseVerifiedProduct];
    if proposed_product.is_some() {
        allowed_actions.push(ReviewAction::CreateVerifiedProduct);
    }
    allowed_actions.push(ReviewAction::Discard);
    PendingReviewAspect {
        id,
        kind: "avionics".to_string(),
        label: avionics_observation_label(item.manufacturer.as_deref(), &item.model),
        observed_text: avionics_observation_text(
            item.manufacturer.as_deref(),
            &item.model,
            &item.avionics_types,
            item.quantity,
            &item.configuration_action,
        ),
        required: true,
        reason,
        suggested_product,
        proposed_product,
        allowed_actions,
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
    let proposed_product = replaced.manufacturer.as_deref().map(|manufacturer| {
        review_product_from_observation(manufacturer, &replaced.model, &replaced.avionics_types)
    });
    let mut allowed_actions = vec![ReviewAction::UseVerifiedProduct];
    if proposed_product.is_some() {
        allowed_actions.push(ReviewAction::CreateVerifiedProduct);
    }
    allowed_actions.push(ReviewAction::Discard);
    PendingReviewAspect {
        id: ReviewAspectId::String(format!("avionics:{index}:replacement")),
        kind: "avionics".to_string(),
        label: avionics_observation_label(replaced.manufacturer.as_deref(), &replaced.model),
        observed_text: avionics_observation_text(
            replaced.manufacturer.as_deref(),
            &replaced.model,
            &replaced.avionics_types,
            1,
            "installed",
        ),
        required: true,
        reason,
        suggested_product: None,
        proposed_product,
        allowed_actions,
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
        valuation_scope: crate::listing::review::AvionicsValuationScope::Unit,
        suite_components: Vec::new(),
        verification_method: None,
        verified_by_user_id: None,
        reviewed_at: None,
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
        valuation_scope: crate::listing::review::AvionicsValuationScope::Unit,
        suite_components: Vec::new(),
        verification_method: None,
        verified_by_user_id: None,
        reviewed_at: None,
        identity_source_url: Some(identity.evidence_url.clone()),
        identity_source_title: Some(identity.evidence_title.clone()),
        identity_evidence_text: Some(identity.evidence.clone()),
    }
}

fn review_product_from_candidate(candidate: &AvionicsReviewCatalogCandidate) -> ReviewProduct {
    let stable_identifier = if candidate.manufacturer_identifier_kind.trim().is_empty()
        || candidate.manufacturer_identifier.trim().is_empty()
    {
        None
    } else {
        Some(StableIdentifier {
            kind: candidate.manufacturer_identifier_kind.clone(),
            value: candidate.manufacturer_identifier.clone(),
        })
    };
    ReviewProduct {
        id: Some(candidate.id),
        manufacturer: candidate.manufacturer.clone(),
        model: candidate.model.clone(),
        capabilities: candidate.avionics_types.clone(),
        stable_identifier,
        valuation_scope: crate::listing::review::AvionicsValuationScope::Unit,
        suite_components: Vec::new(),
        verification_method: None,
        verified_by_user_id: None,
        reviewed_at: None,
        identity_source_url: None,
        identity_source_title: None,
        identity_evidence_text: None,
    }
}

fn avionics_observation_text(
    manufacturer: Option<&str>,
    model: &str,
    capabilities: &[String],
    quantity: i64,
    configuration_action: &str,
) -> String {
    format!(
        "{} · {} · quantity {} · {}",
        avionics_observation_label(manufacturer, model),
        capabilities.join(", "),
        quantity.max(1),
        configuration_action
    )
}

fn avionics_observation_label(manufacturer: Option<&str>, model: &str) -> String {
    manufacturer
        .map(|manufacturer| format!("{} {}", manufacturer.trim(), model.trim()))
        .unwrap_or_else(|| model.trim().to_string())
        .trim()
        .to_string()
}

fn listing_avionics_value_from_catalog(
    original: &ListingAvionicsValue,
    identity: &ApprovedAvionicsIdentity,
    source_confidence_basis: ListingSourceConfidenceBasis,
) -> ListingAvionicsValue {
    let source = if matches!(
        &source_confidence_basis,
        ListingSourceConfidenceBasis::ExactControllerLeadingDualCountableUnit(_)
    ) {
        "listing_explicit_count".to_string()
    } else {
        original.source.clone()
    };
    ListingAvionicsValue {
        avionics_model_id: Some(identity.id),
        manufacturer: Some(identity.manufacturer.clone()),
        model: identity.model.clone(),
        avionics_types: identity.avionics_types.clone(),
        quantity: original.quantity.max(1),
        source,
        // The listing link retains only the listing occurrence. Authoritative
        // product evidence belongs to the single approved catalog identity.
        source_notes: original.source_notes.clone(),
        // Reaching the approved resolution state requires either high listing
        // evidence or the exact publisher-count plus countable-unit proof.
        // Product identity alone never upgrades a weak listing mention.
        source_confidence: Some("high".to_string()),
        source_confidence_basis: Some(source_confidence_basis),
        configuration_action: original.configuration_action.clone(),
        replaces: original.replaces.clone(),
        replaces_avionics_model_id: original.replaces_avionics_model_id,
        grounded_capabilities: Vec::new(),
        replacement_grounded_capabilities: Vec::new(),
    }
}

fn listing_avionics_without_replacement(original: &ListingAvionicsValue) -> ListingAvionicsValue {
    let mut independent = original.clone();
    independent.configuration_action = "installed".to_string();
    independent.replaces = None;
    independent.replaces_avionics_model_id = None;
    independent
}

fn listing_avionics_after_generic_target_rejection(
    original: &ListingAvionicsValue,
    identity: &ApprovedAvionicsIdentity,
    source_confidence_basis: ListingSourceConfidenceBasis,
) -> ListingAvionicsValue {
    if original.configuration_action != "removes" {
        let independent = listing_avionics_without_replacement(original);
        return listing_avionics_value_from_catalog(
            &independent,
            identity,
            source_confidence_basis,
        );
    }

    let mut removal =
        listing_avionics_value_from_catalog(original, identity, source_confidence_basis);
    removal.replaces = Some(ParsedAvionicsReference {
        manufacturer: Some(identity.manufacturer.clone()),
        model: identity.model.clone(),
        avionics_types: identity.avionics_types.clone(),
    });
    removal.replaces_avionics_model_id = Some(identity.id);
    removal
}

fn reject_duplicate_listing_avionics(
    existing: &ListingAvionicsValue,
    incoming: &ListingAvionicsValue,
) -> StoreResult<()> {
    let existing_model_id = existing.avionics_model_id.filter(|id| *id > 0);
    let incoming_model_id = incoming.avionics_model_id.filter(|id| *id > 0);
    if existing_model_id.is_none() || existing_model_id != incoming_model_id {
        return Err(ListingStoreError::State(
            "duplicate avionics validation requires one resolved catalog product".to_string(),
        ));
    }
    if existing
        .manufacturer
        .as_deref()
        .map(normalize_avionics_manufacturer_name)
        != incoming
            .manufacturer
            .as_deref()
            .map(normalize_avionics_manufacturer_name)
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

    Err(ListingStoreError::Validation(format!(
        "catalog avionics model {} resolved from multiple listing occurrences; quantity must come from one explicit source-validated occurrence",
        existing_model_id.expect("resolved catalog id checked above")
    )))
}

fn matching_avionics_reference(
    left: Option<&ParsedAvionicsReference>,
    right: Option<&ParsedAvionicsReference>,
) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    left.manufacturer
        .as_deref()
        .map(normalize_avionics_manufacturer_name)
        == right
            .manufacturer
            .as_deref()
            .map(normalize_avionics_manufacturer_name)
        && normalize_avionics_model_name(&left.model) == normalize_avionics_model_name(&right.model)
        && canonical_avionics_types(&left.avionics_types)
            == canonical_avionics_types(&right.avionics_types)
}

fn require_unique_resolved_listing_avionics(
    avionics: impl IntoIterator<Item = ListingAvionicsValue>,
) -> StoreResult<Vec<ListingAvionicsValue>> {
    let mut unique: Vec<ListingAvionicsValue> = Vec::new();
    let mut seen = HashMap::<i64, usize>::new();
    for item in avionics {
        let avionics_model_id = item.avionics_model_id.filter(|id| *id > 0).ok_or_else(|| {
            ListingStoreError::Validation(format!(
                "avionics must resolve to a catalog id before persistence: {}",
                avionics_observation_label(item.manufacturer.as_deref(), &item.model)
            ))
        })?;
        if let Some(index) = seen.get(&avionics_model_id).copied() {
            reject_duplicate_listing_avionics(&unique[index], &item)?;
        } else {
            seen.insert(avionics_model_id, unique.len());
            unique.push(item);
        }
    }
    Ok(unique)
}

/// Select only one extraction-validated occurrence from the bounded source
/// adapter context. Provider and local identity work must never receive page
/// fields unrelated to the occurrence being resolved.
fn exact_occurrence_evidence_from_source_units<'a>(
    source_evidence_units: Option<&ListingEvidenceUnits>,
    source_evidence_text: Option<&'a str>,
) -> &'a str {
    let evidence = source_evidence_text.map(str::trim).unwrap_or_default();
    if evidence.is_empty()
        || evidence.len() > MAX_RECOVERED_ASSOCIATION_EVIDENCE_BYTES
        || !source_evidence_units.is_some_and(|units| units.contains_exact_span(evidence))
    {
        ""
    } else {
        evidence
    }
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
            let manufacturer = explicit_nullable_string_member(
                object,
                "manufacturer",
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
                replaces: parsed_avionics_reference(object.get("replaces"), index)?,
                source_evidence_text: optional_string(object.get("source_evidence_text")),
                source_confidence: optional_string(object.get("source_confidence")),
            }))
        })
        .collect()
}

fn parsed_avionics_reference(
    value: Option<&Value>,
    index: usize,
) -> StoreResult<Option<ParsedAvionicsReference>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or_else(|| {
        ListingStoreError::Validation(format!(
            "avionics[{index}].replaces must be an object or null"
        ))
    })?;
    Ok(Some(ParsedAvionicsReference {
        manufacturer: explicit_nullable_string_member(
            object,
            "manufacturer",
            &format!("avionics[{index}].replaces.manufacturer"),
        )?,
        model: required_string_from_value(
            object.get("model").unwrap_or(&Value::Null),
            &format!("avionics[{index}].replaces.model"),
        )?,
        avionics_types: avionics_types_from_object(object),
    }))
}

fn explicit_nullable_string_member(
    object: &serde_json::Map<String, Value>,
    member: &str,
    field_name: &str,
) -> StoreResult<Option<String>> {
    let value = object.get(member).ok_or_else(|| {
        ListingStoreError::Validation(format!(
            "{field_name} must be explicitly present as null or a non-empty string"
        ))
    })?;
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ListingStoreError::Validation(format!(
                "{field_name} must be null or a non-empty string"
            ))
        })?;
    Ok(Some(value.to_string()))
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
    let is_valid = match field_name {
        "engine_time_basis" => matches!(
            value.as_str(),
            "SNEW" | "SMOH" | "SFOH" | "SFRM" | "unknown"
        ),
        "propeller_time_basis" => matches!(
            value.as_str(),
            "SNEW" | "SMOH" | "SFOH" | "SPOH" | "unknown"
        ),
        _ => false,
    };
    if is_valid {
        Ok(value)
    } else {
        let expected = if field_name == "engine_time_basis" {
            "SNEW, SMOH, SFOH, SFRM, or unknown"
        } else {
            "SNEW, SMOH, SFOH, SPOH, or unknown"
        };
        Err(ListingStoreError::Validation(format!(
            "{field_name} must be {expected}"
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
                "avionics capability types are required for {}",
                avionics_observation_label(item.manufacturer.as_deref(), &item.model)
            )));
        }
        if item.quantity < 1 {
            return Err(ListingStoreError::Validation(format!(
                "avionics quantity must be at least 1 for {}",
                avionics_observation_label(item.manufacturer.as_deref(), &item.model)
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
    let basis_is_valid = if component == "engine" {
        matches!(basis, "SNEW" | "SMOH" | "SFOH" | "SFRM" | "unknown")
    } else {
        matches!(basis, "SNEW" | "SMOH" | "SFOH" | "SPOH" | "unknown")
    };
    if !basis_is_valid {
        return Err(ListingStoreError::Validation(format!(
            "{component}_time_basis is invalid"
        )));
    }
    if component == "engine" && evidence.is_some_and(is_top_overhaul_time_evidence) {
        return Err(ListingStoreError::Validation(
            "engine top-overhaul time (STOH/TSTOH) cannot be stored as engine_hours".to_string(),
        ));
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
          AND NOT EXISTS (
            SELECT 1 FROM plugin_submission_materialization_receipts receipt
            WHERE receipt.aircraft_sale_listing_id = l.id
          )
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
                "avionics must resolve to an approved product graph before verified-listing refresh: {}",
                avionics_observation_label(item.manufacturer.as_deref(), &item.model)
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
) -> StoreResult<ListingFinalizationOutcome> {
    let invalid_avionics_count =
        listing_invalid_avionics_product_graph_count(db, listing_id).await?;
    if invalid_avionics_count > 0 {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} has {invalid_avionics_count} avionics associations without an approved canonical product identity or required suite composition"
        )));
    }

    let authorization_state = listing_authorization_state(db, listing_id)
        .await
        .map_err(|error| ListingStoreError::State(error.to_string()))?;
    if !authorization_state.all_automatic_associations_current() {
        return Err(ListingStoreError::State(
            AVIONICS_AUTHORIZATION_INVALIDATED.to_string(),
        ));
    }

    if let Ok(Some(identity)) = listing_aircraft_identity(db, listing_id).await {
        mark_valuation_snapshot_stale_best_effort(db, identity.aircraft_model_id).await;
    }
    let _ = cleanup_orphan_records(db).await;
    Ok(ListingFinalizationOutcome::Ready)
}

async fn finalize_listing_ingestion(db: &AppDb, listing_id: i64) -> StoreResult<()> {
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
    match complete_listing_ingestion(db, listing_id).await {
        Ok(ListingFinalizationOutcome::Ready) => {
            mark_listing_ready(db, listing_id).await?;
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
    let replacement = if preserve_source_identity_receipt_gate {
        replace_pending_review_preserving_source_identity_receipt_gate(
            db, listing_id, None, aspects,
        )
        .await
    } else {
        replace_pending_review(db, listing_id, None, aspects).await
    };
    match replacement {
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
/// incomplete and private until canonical aircraft and avionics product-graph
/// checks have passed. Factory-reference availability is a separate valuation
/// concern and never demotes an otherwise verified listing. Finalization is
/// local and never performs model enrichment or other network work.
pub async fn finalize_reviewed_listing_ingestion(
    db: &AppDb,
    listing_id: i64,
) -> Result<ListingFinalizationOutcome, ListingStoreError> {
    if source_identity_receipt_is_pending(db, listing_id).await? {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} is waiting for its immutable source identity correction receipt"
        )));
    }
    let listing_exists = query_scalar_one!(
        db,
        i64,
        "SELECT COUNT(*) FROM aircraft_sale_listings WHERE id = ?",
        listing_id
    )? == 1;
    if !listing_exists {
        return Err(ListingStoreError::NotFound(format!(
            "listing {listing_id} not found"
        )));
    }
    if listing_has_pending_review(db, listing_id).await? {
        return Err(ListingStoreError::State(format!(
            "listing {listing_id} still has a pending review"
        )));
    }
    if let Err(error) = ensure_listing_canonical_aircraft_identity(db, listing_id).await {
        return quarantine_after_error(db, listing_id, error).await;
    }
    match complete_listing_ingestion(db, listing_id).await {
        Ok(ListingFinalizationOutcome::Ready) => {
            match mark_reviewed_listing_ready(db, listing_id).await {
                Ok(()) => Ok(ListingFinalizationOutcome::Ready),
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

/// Publish only from the same locked snapshot that proves current avionics
/// authority. PostgreSQL uses a serializable transaction plus the existing
/// child-table locks; SQLite acquires its writer lock before revalidation.
async fn execute_ready_listing_update(
    db: &AppDb,
    statement: &str,
    listing_id: i64,
) -> StoreResult<u64> {
    let statement = db.sql(statement);
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            let mut transaction = pool.begin().await?;
            sqlx::query("UPDATE aircraft_sale_listings SET updated_at = updated_at WHERE id = ?")
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            let authorization_state =
                listing_authorization_state_sqlite(db, &mut transaction, listing_id)
                    .await
                    .map_err(|error| ListingStoreError::State(error.to_string()))?;
            if !authorization_state.all_automatic_associations_current() {
                return Err(ListingStoreError::State(
                    AVIONICS_AUTHORIZATION_INVALIDATED.to_string(),
                ));
            }
            let updated = sqlx::query(&statement)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            transaction.commit().await?;
            Ok(updated)
        }
        DatabaseBackend::Postgres(pool) => {
            let mut connection = pool.acquire().await?;
            let mut transaction = connection
                .begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")
                .await?;
            sqlx::query(POSTGRES_LISTING_CHILD_LOCK_SQL)
                .execute(&mut *transaction)
                .await?;
            let authorization_state =
                listing_authorization_state_postgres(db, &mut transaction, listing_id)
                    .await
                    .map_err(|error| ListingStoreError::State(error.to_string()))?;
            if !authorization_state.all_automatic_associations_current() {
                return Err(ListingStoreError::State(
                    AVIONICS_AUTHORIZATION_INVALIDATED.to_string(),
                ));
            }
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

async fn listing_invalid_avionics_product_graph_count(
    db: &AppDb,
    listing_id: i64,
) -> StoreResult<i64> {
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
            NOT EXISTS (
              SELECT 1
              FROM avionics_approved_product_graph_identities approved_identity
              WHERE approved_identity.avionics_model_id = link.avionics_model_id
            )
            OR (
              link.replaces_avionics_model_id IS NOT NULL
              AND NOT EXISTS (
                SELECT 1
                FROM avionics_approved_product_graph_identities approved_identity
                WHERE approved_identity.avionics_model_id = link.replaces_avionics_model_id
              )
            )
            OR (
              model.valuation_scope = 'integrated_suite'
              AND (
                NOT EXISTS (
                  SELECT 1
                  FROM avionics_suite_components membership
                  WHERE membership.suite_model_id = model.id
                )
                OR EXISTS (
                  SELECT 1
                  FROM avionics_suite_components membership
                  WHERE membership.suite_model_id = model.id
                    AND NOT EXISTS (
                      SELECT 1
                      FROM avionics_approved_product_graph_identities component_identity
                      WHERE component_identity.avionics_model_id = membership.component_model_id
                    )
                )
              )
            )
          )
        "#,
        listing_id
    )?)
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
        source_confidence_basis: Option<ListingSourceConfidenceBasis>,
        configuration_action: String,
        replaces_avionics_model_id: Option<i64>,
        canonical_identity_key: String,
        replacement_identity_key: Option<String>,
        grounded_capabilities: Vec<ListingGroundedCapability>,
        replacement_grounded_capabilities: Vec<ListingGroundedCapability>,
    }

    struct PreparedListingAuthorization {
        installed_reuse: bool,
        installed_collision_closure_sha256: String,
        installed_capabilities: Vec<StoredGroundedCapabilityRow>,
        replacement_reuse: bool,
        replacement_collision_closure_sha256: Option<String>,
        replacement_capabilities: Vec<StoredGroundedCapabilityRow>,
        link_source_notes: Option<String>,
    }

    // Require one source-validated occurrence per physical catalog product
    // before persistence. This is deliberately repeated at the storage
    // boundary so no caller can infer quantity from duplicate rows or delegate
    // conflict resolution to the database uniqueness constraint.
    let avionics = require_unique_resolved_listing_avionics(
        avionics
            .iter()
            .filter(|item| {
                item.manufacturer
                    .as_deref()
                    .is_some_and(|manufacturer| is_usable_avionics_label(manufacturer, &item.model))
            })
            .cloned(),
    )?;

    // Validate the entire replacement set before touching existing links.
    // The transaction below then makes trigger/race failures all-or-nothing.
    let needs_reuse_capture = avionics.iter().any(|item| {
        item.grounded_capabilities.is_empty()
            || (item.replaces_avionics_model_id.is_some()
                && item.replacement_grounded_capabilities.is_empty())
    });
    let exact_capture_scope = if needs_reuse_capture {
        exact_signed_listing_checkpoint_scope(db, listing_id).await?
    } else {
        None
    };
    let mut prepared = Vec::new();
    for item in &avionics {
        if matches!(
            item.source_confidence_basis.as_ref(),
            Some(ListingSourceConfidenceBasis::ExactControllerLeadingDualCountableUnit(_))
        ) && (item.quantity != 2
            || item.source != "listing_explicit_count"
            || item.source_confidence.as_deref() != Some("high")
            || item.configuration_action != "installed"
            || item.replaces.is_some()
            || item.replaces_avionics_model_id.is_some())
        {
            return Err(ListingStoreError::Validation(
                "derived Controller Dual confidence requires one fresh installed quantity-two explicit-count association"
                    .to_string(),
            ));
        }
        let manufacturer = item.manufacturer.as_deref().ok_or_else(|| {
            ListingStoreError::Validation(
                "manufacturer-less avionics observations cannot be persisted without a canonical catalog resolution"
                    .to_string(),
            )
        })?;
        let avionics_model_id = validated_catalog_avionics_model_id(
            db,
            item.avionics_model_id.ok_or_else(|| {
                ListingStoreError::Validation(format!(
                    "avionics must resolve to a catalog id before persistence: {}",
                    avionics_observation_label(item.manufacturer.as_deref(), &item.model)
                ))
            })?,
            manufacturer,
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
                let replaced_manufacturer = replaced.manufacturer.as_deref().ok_or_else(|| {
                    ListingStoreError::Validation(
                        "manufacturer-less replacement observations cannot be persisted without a canonical catalog resolution"
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
                    replaced_manufacturer,
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
            source_confidence_basis: item.source_confidence_basis.clone(),
            configuration_action: item.configuration_action.clone(),
            replaces_avionics_model_id,
            canonical_identity_key,
            replacement_identity_key,
            grounded_capabilities: item.grounded_capabilities.clone(),
            replacement_grounded_capabilities: item.replacement_grounded_capabilities.clone(),
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
        DatabaseBackend::Postgres(_) => db.sql(POSTGRES_RESTAGE_CATALOG_LOCK_SQL),
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
            RETURNING id
            "#,
    );
    let select_grounded_capability_sql = db.sql(
        r#"
        SELECT capability.plugin_submission_id, capability.occurrence_index,
               capability.occurrence_role, capability.avionics_model_id,
               capability.requested_quantity, capability.configuration_action,
               capability.request_sha256, capability.capability_sha256,
               capability.grounded_resolution_sha256,
               capability.evidence_capture_sha256,
               capability.extracted_listing_sha256,
               submission.canonical_listing_id AS submission_canonical_listing_id,
               submission.rendered_html_sha256 AS submission_rendered_html_sha256,
               submission.extracted_listing_json,
               submission.extraction_error,
               capability.product_fingerprint,
               capability.collision_closure_sha256,
               capability.source_revocation_count
        FROM aircraft_sale_listing_avionics_grounded_capabilities capability
        LEFT JOIN plugin_submissions submission
          ON submission.id = capability.plugin_submission_id
        WHERE capability.listing_id = ?
          AND capability.occurrence_index = ?
          AND capability.occurrence_role = ?
        "#,
    );
    let delete_grounded_capability_sql = db.sql(
        r#"
        DELETE FROM aircraft_sale_listing_avionics_grounded_capabilities
        WHERE listing_id = ? AND plugin_submission_id = ?
          AND occurrence_index = ? AND occurrence_role = ?
        "#,
    );
    let insert_grounded_authorization_sql = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics_link_authorizations (
          listing_link_id, association_role, avionics_model_id,
          authorization_kind, observation_sha256, product_fingerprint,
          grounded_resolution_sha256, evidence_capture_sha256,
          plugin_submission_id, extracted_listing_sha256,
          collision_closure_sha256, source_revocation_count
        ) VALUES (?, ?, ?, 'same_case_grounded', ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    );
    let insert_reuse_authorization_sql = db.sql(
        r#"
        INSERT INTO aircraft_sale_listing_avionics_link_authorizations (
          listing_link_id, association_role, avionics_model_id,
          authorization_kind, observation_sha256, product_fingerprint,
          grounded_resolution_sha256, evidence_capture_sha256,
          plugin_submission_id, extracted_listing_sha256,
          collision_closure_sha256
        )
        SELECT ?, ?, ?, 'manufacturer_reuse', ?,
               attestation.product_fingerprint, NULL, ?, ?, ?, ?
        FROM avionics_product_reuse_attestations attestation
        WHERE attestation.avionics_model_id = ?
        "#,
    );
    let select_authorization_capture_sql = db.sql(
        r#"
        SELECT submission.rendered_html, submission.rendered_html_sha256,
               submission.extracted_listing_json, listing.source_url
        FROM plugin_submissions submission
        JOIN aircraft_sale_listings listing
          ON listing.id = submission.canonical_listing_id
        WHERE submission.id = ?
          AND listing.id = ?
          AND submission.rendered_html_sha256 = ?
          AND submission.source_url = listing.source_url
          AND submission.extracted_listing_json IS NOT NULL
          AND submission.extraction_error IS NULL
        "#,
    );
    let approved_catalog_rows_sql = db.sql(APPROVED_CATALOG_ROWS_SQL);
    let active_collision_rows_sql = db.sql(ACTIVE_COLLISION_CATALOG_ROWS_SQL);
    macro_rules! replace_in_transaction {
        ($pool:expr, $reuse_is_current:path, $countable_reuse_is_current:path, $active_collision_revision:path) => {{
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
            let catalog_rows = sqlx::query_as::<_, CatalogFingerprintRow>(
                &approved_catalog_rows_sql,
            )
            .fetch_all(&mut *transaction)
            .await?;
            let active_collision_rows =
                sqlx::query_as::<_, ActiveCollisionCatalogFingerprintRow>(
                    &active_collision_rows_sql,
                )
                .fetch_all(&mut *transaction)
                .await?;
            let source_revocation_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM avionics_authoritative_source_origin_revocations",
            )
            .fetch_one(&mut *transaction)
            .await?;
            let mut authorizations = Vec::with_capacity(prepared.len());
            let mut consumed_coordinates = HashSet::new();
            let mut exact_capability_scope = None;
            for item in &prepared {
                let current_product_fingerprint =
                    catalog_product_fingerprint_from_rows(&catalog_rows, item.avionics_model_id)
                        .ok_or_else(|| {
                            ListingStoreError::Validation(format!(
                                "avionics catalog id {} has no current approved product fingerprint",
                                item.avionics_model_id
                            ))
                        })?;
                let current_collision = fingerprint_grounded_collision_closure(
                    &active_collision_rows,
                    item.avionics_model_id,
                )
                .ok_or_else(|| {
                    ListingStoreError::Validation(format!(
                        "avionics catalog id {} has no current grounded collision closure",
                        item.avionics_model_id
                    ))
                })?;
                let installed_reuse =
                    $reuse_is_current(db, &mut transaction, item.avionics_model_id).await?;
                let exact_count_basis = matches!(
                    item.source_confidence_basis.as_ref(),
                    Some(
                        ListingSourceConfidenceBasis::ExactControllerLeadingDualCountableUnit(_)
                    )
                );
                if exact_count_basis
                    && !$countable_reuse_is_current(
                        db,
                        &mut transaction,
                        item.avionics_model_id,
                    )
                    .await?
                {
                    return Err(ListingStoreError::Validation(format!(
                        "avionics catalog id {} lost its exact countable-unit reuse proof before persistence",
                        item.avionics_model_id
                    )));
                }
                let installed_collision_closure_sha256 = if installed_reuse {
                    $active_collision_revision(db, &mut transaction, item.avionics_model_id)
                        .await
                        .map_err(|error| ListingStoreError::State(error.to_string()))?
                } else {
                    current_collision.clone()
                };
                let mut installed_capabilities = Vec::new();
                for capability in &item.grounded_capabilities {
                    if !consumed_coordinates.insert((
                        capability.occurrence_index,
                        capability.occurrence_role,
                    )) {
                        return Err(ListingStoreError::State(
                            "one grounded capability was assigned to multiple links".to_string(),
                        ));
                    }
                    let row = sqlx::query_as::<_, StoredGroundedCapabilityRow>(
                        &select_grounded_capability_sql,
                    )
                    .bind(listing_id)
                    .bind(capability.occurrence_index as i64)
                    .bind(capability.occurrence_role.as_str())
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or_else(|| {
                        ListingStoreError::Validation(format!(
                            "listing {listing_id} has no exact pending grounded capability for occurrence {} {}",
                            capability.occurrence_index,
                            capability.occurrence_role.as_str()
                        ))
                    })?;
                    let expected_receipt = capability.seed.bind(listing_id);
                    if row.avionics_model_id != item.avionics_model_id
                        || row.requested_quantity != capability.seed.requested_quantity()
                        || row.configuration_action != item.configuration_action
                        || row.request_sha256 != capability.seed.request_sha256()
                        || row.capability_sha256
                            != grounded_occurrence_capability_sha256(capability)
                        || row.grounded_resolution_sha256
                            != expected_receipt.resolution_sha256()
                        || row.product_fingerprint != current_product_fingerprint
                        || row.collision_closure_sha256 != current_collision
                        || row.source_revocation_count != source_revocation_count
                        || !grounded_capability_submission_checkpoint_is_current(
                            listing_id,
                            &row,
                        )
                    {
                        return Err(ListingStoreError::Validation(format!(
                            "pending grounded capability for avionics catalog id {} is stale or not bound to the exact occurrence",
                            item.avionics_model_id
                        )));
                    }
                    require_exact_grounded_capability_scope(
                        &mut exact_capability_scope,
                        &row,
                    )?;
                    installed_capabilities.push(row);
                }
                let installed_quantity_coverage = installed_capabilities
                    .iter()
                    .map(|row| row.requested_quantity)
                    .max()
                    .unwrap_or_default();
                if !installed_capabilities.is_empty()
                    && (installed_quantity_coverage != item.quantity
                        || installed_capabilities
                            .iter()
                            .any(|row| row.requested_quantity > item.quantity))
                {
                    return Err(ListingStoreError::Validation(format!(
                        "pending grounded capabilities do not cover exact quantity {} for avionics catalog id {}",
                        item.quantity, item.avionics_model_id
                    )));
                }
                if !installed_reuse && installed_capabilities.is_empty() {
                    return Err(ListingStoreError::Validation(format!(
                        "avionics catalog id {} is not eligible for current-policy reuse and has no exact pending grounded capability",
                        item.avionics_model_id
                    )));
                }
                let installed_reuse = installed_reuse && installed_capabilities.is_empty();

                let (
                    replacement_reuse,
                    replacement_collision_closure_sha256,
                    replacement_capabilities,
                ) =
                    if let Some(target_id) = item.replaces_avionics_model_id {
                        let current_target_product = catalog_product_fingerprint_from_rows(
                            &catalog_rows,
                            target_id,
                        )
                        .ok_or_else(|| {
                            ListingStoreError::Validation(format!(
                                "replacement avionics catalog id {target_id} has no current approved product fingerprint"
                            ))
                        })?;
                        let current_target_collision = fingerprint_grounded_collision_closure(
                            &active_collision_rows,
                            target_id,
                        )
                        .ok_or_else(|| {
                            ListingStoreError::Validation(format!(
                                "replacement avionics catalog id {target_id} has no current grounded collision closure"
                            ))
                        })?;
                        let reuse = $reuse_is_current(db, &mut transaction, target_id).await?;
                        let mut rows = Vec::new();
                        for capability in &item.replacement_grounded_capabilities {
                            if !consumed_coordinates.insert((
                                capability.occurrence_index,
                                capability.occurrence_role,
                            )) {
                                return Err(ListingStoreError::State(
                                    "one grounded replacement capability was assigned to multiple links"
                                        .to_string(),
                                ));
                            }
                            let row = sqlx::query_as::<_, StoredGroundedCapabilityRow>(
                                &select_grounded_capability_sql,
                            )
                            .bind(listing_id)
                            .bind(capability.occurrence_index as i64)
                            .bind(capability.occurrence_role.as_str())
                            .fetch_optional(&mut *transaction)
                            .await?
                            .ok_or_else(|| {
                                ListingStoreError::Validation(format!(
                                    "listing {listing_id} has no exact pending grounded replacement capability for occurrence {}",
                                    capability.occurrence_index
                                ))
                            })?;
                            let expected_receipt = capability.seed.bind(listing_id);
                            if row.avionics_model_id != target_id
                                || row.requested_quantity != 1
                                || row.configuration_action != item.configuration_action
                                || row.request_sha256 != capability.seed.request_sha256()
                                || row.capability_sha256
                                    != grounded_occurrence_capability_sha256(capability)
                                || row.grounded_resolution_sha256
                                    != expected_receipt.resolution_sha256()
                                || row.product_fingerprint != current_target_product
                                || row.collision_closure_sha256 != current_target_collision
                                || row.source_revocation_count != source_revocation_count
                                || !grounded_capability_submission_checkpoint_is_current(
                                    listing_id,
                                    &row,
                                )
                            {
                                return Err(ListingStoreError::Validation(format!(
                                    "pending grounded capability for replacement avionics catalog id {target_id} is stale or not bound to the exact occurrence"
                                    )));
                            }
                            require_exact_grounded_capability_scope(
                                &mut exact_capability_scope,
                                &row,
                            )?;
                            rows.push(row);
                        }
                        if rows.iter().any(|row| row.requested_quantity != 1) {
                            return Err(ListingStoreError::Validation(format!(
                                "pending grounded replacement capabilities do not cover exact quantity 1 for avionics catalog id {target_id}"
                            )));
                        }
                        if !reuse && rows.is_empty() {
                            return Err(ListingStoreError::Validation(format!(
                                "replacement avionics catalog id {target_id} is not eligible for current-policy reuse and has no exact pending grounded capability"
                            )));
                        }
                        let authorization_collision = if reuse && rows.is_empty() {
                            $active_collision_revision(db, &mut transaction, target_id)
                                .await
                                .map_err(|error| ListingStoreError::State(error.to_string()))?
                        } else {
                            current_target_collision
                        };
                        (
                            reuse && rows.is_empty(),
                            Some(authorization_collision),
                            rows,
                        )
                    } else {
                        (true, None, Vec::new())
                    };
                let link_source_notes = if installed_reuse && replacement_reuse {
                    item.source_notes.clone()
                } else {
                    item.grounded_capabilities
                        .iter()
                        .filter(|_| !installed_reuse)
                        .chain(
                            item.replacement_grounded_capabilities
                                .iter()
                                .filter(|_| !replacement_reuse),
                        )
                        .filter_map(|capability| capability.source_notes.as_ref())
                        .find(|notes| !notes.trim().is_empty())
                        .cloned()
                        .ok_or_else(|| {
                            ListingStoreError::Validation(format!(
                                "same-case avionics authorization for catalog id {} requires exact listing evidence text",
                                item.avionics_model_id
                            ))
                        })
                        .map(Some)?
                };
                authorizations.push(PreparedListingAuthorization {
                    installed_reuse,
                    installed_collision_closure_sha256,
                    installed_capabilities,
                    replacement_reuse,
                    replacement_collision_closure_sha256,
                    replacement_capabilities,
                    link_source_notes,
                });
            }
            let stored_capability_count = sqlx::query_scalar::<_, i64>(
                &db.sql(
                    "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities WHERE listing_id = ?",
                ),
            )
            .bind(listing_id)
            .fetch_one(&mut *transaction)
            .await?;
            if stored_capability_count != consumed_coordinates.len() as i64 {
                return Err(ListingStoreError::Validation(format!(
                    "listing {listing_id} pending grounded capability set does not exactly cover the materialized avionics graph"
                )));
            }
            let reuse_capture = if authorizations.iter().any(|authorization| {
                authorization.installed_reuse
                    || (authorization.replacement_reuse
                        && authorization.replacement_collision_closure_sha256.is_some())
            }) {
                let scope = exact_capture_scope.as_ref().ok_or_else(|| {
                    ListingStoreError::Validation(format!(
                        "listing {listing_id} has locally reusable avionics but no exact signed extraction checkpoint"
                    ))
                })?;
                let capture: (String, String, String, String) =
                    sqlx::query_as(&select_authorization_capture_sql)
                        .bind(scope.plugin_submission_id)
                        .bind(listing_id)
                        .bind(scope.rendered_html_sha256.as_str())
                        .fetch_optional(&mut *transaction)
                        .await?
                        .ok_or_else(|| {
                            ListingStoreError::Validation(format!(
                                "listing {listing_id} lost the exact signed extraction checkpoint needed for local avionics reuse"
                            ))
                        })?;
                if format!("{:x}", Sha256::digest(capture.0.as_bytes()))
                    != scope.rendered_html_sha256
                    || format!("{:x}", Sha256::digest(capture.2.as_bytes()))
                        != scope.extracted_listing_sha256
                {
                    return Err(ListingStoreError::State(format!(
                        "listing {listing_id} signed extraction checkpoint content no longer matches its hashes"
                    )));
                }
                Some((scope, capture))
            } else {
                None
            };
            sqlx::query(&delete_sql)
                .bind(listing_id)
                .execute(&mut *transaction)
                .await?;
            for (item, authorization) in prepared.iter().zip(&authorizations) {
                let listing_link_id = sqlx::query_scalar::<_, i64>(&insert_sql)
                    .bind(listing_id)
                    .bind(item.avionics_model_id)
                    .bind(item.quantity)
                    .bind(item.source.as_str())
                    .bind(authorization.link_source_notes.as_deref())
                    .bind(item.source_confidence.as_deref())
                    .bind(item.configuration_action.as_str())
                    .bind(item.replaces_avionics_model_id)
                    .fetch_one(&mut *transaction)
                    .await?;
                for (role, target_id, reuse, collision_closure, capabilities, quantity) in [
                    (
                        ListingAssociationRole::Installed,
                        item.avionics_model_id,
                        authorization.installed_reuse,
                        Some(authorization.installed_collision_closure_sha256.as_str()),
                        authorization.installed_capabilities.as_slice(),
                        item.quantity,
                    ),
                    (
                        ListingAssociationRole::Replacement,
                        item.replaces_avionics_model_id.unwrap_or_default(),
                        authorization.replacement_reuse,
                        authorization.replacement_collision_closure_sha256.as_deref(),
                        authorization.replacement_capabilities.as_slice(),
                        1,
                    ),
                ] {
                    if target_id <= 0 {
                        continue;
                    }
                    if reuse {
                        let (scope, (rendered_html, _, extracted_listing_json, source_url)) =
                            reuse_capture.as_ref().ok_or_else(|| {
                                ListingStoreError::State(
                                    "local avionics reuse lost its exact capture scope".to_string(),
                                )
                            })?;
                        let evidence_text = authorization
                            .link_source_notes
                            .as_deref()
                            .filter(|evidence| !evidence.trim().is_empty())
                            .ok_or_else(|| {
                                ListingStoreError::Validation(format!(
                                    "reused avionics catalog id {target_id} requires exact retained listing evidence"
                                ))
                            })?;
                        if !current_checkpoint_contains_avionics_source_evidence(
                            extracted_listing_json,
                            evidence_text,
                        ) {
                            return Err(ListingStoreError::Validation(format!(
                                "reused avionics catalog id {target_id} is absent from the exact retained extraction checkpoint"
                            )));
                        }
                        if role == ListingAssociationRole::Installed {
                            if let Some(
                                ListingSourceConfidenceBasis::ExactControllerLeadingDualCountableUnit(
                                    proof,
                                ),
                            ) = item.source_confidence_basis.as_ref()
                            {
                                if evidence_text != proof.evidence_text()
                                    || scope.plugin_submission_id != proof.plugin_submission_id()
                                    || scope.rendered_html_sha256 != proof.rendered_html_sha256()
                                    || scope.extracted_listing_sha256
                                        != proof.extracted_listing_sha256()
                                {
                                    return Err(ListingStoreError::Validation(format!(
                                        "countable-unit avionics catalog id {target_id} no longer carries the exact Controller checkpoint that produced its source proof"
                                    )));
                                }
                                let exact_capture_source =
                                    crate::html::listing::source::listing_extraction_source(
                                        source_url,
                                        rendered_html,
                                    )
                                    .map_err(|error| {
                                        ListingStoreError::Validation(format!(
                                            "countable-unit avionics catalog id {target_id} lost its exact retained Controller source envelope: {error}"
                                        ))
                                    })?;
                                if !controller_extraction_source_has_exact_avionics_line(
                                    source_url,
                                    &exact_capture_source,
                                    evidence_text,
                                ) {
                                    return Err(ListingStoreError::Validation(format!(
                                        "countable-unit avionics catalog id {target_id} lost the exact Controller field that produced its source proof"
                                    )));
                                }
                            }
                        }
                        let observation_sha256 = association_observation_sha256_from_values(
                            listing_id,
                            listing_link_id,
                            role,
                            target_id,
                            item.avionics_model_id,
                            item.replaces_avionics_model_id,
                            item.quantity,
                            item.configuration_action.as_str(),
                            evidence_text,
                        );
                        let inserted = sqlx::query(&insert_reuse_authorization_sql)
                            .bind(listing_link_id)
                            .bind(role.as_str())
                            .bind(target_id)
                            .bind(observation_sha256)
                            .bind(scope.rendered_html_sha256.as_str())
                            .bind(scope.plugin_submission_id)
                            .bind(scope.extracted_listing_sha256.as_str())
                            .bind(collision_closure.ok_or_else(|| {
                                ListingStoreError::State(format!(
                                    "reused avionics catalog id {target_id} has no collision closure"
                                ))
                            })?)
                            .bind(target_id)
                            .execute(&mut *transaction)
                            .await?
                            .rows_affected();
                        if inserted != 1 {
                            return Err(ListingStoreError::Validation(format!(
                                "avionics catalog id {target_id} lost its reuse attestation before authorization"
                            )));
                        }
                    } else {
                        let product_fingerprint = capabilities[0].product_fingerprint.as_str();
                        let collision_closure = capabilities[0].collision_closure_sha256.as_str();
                        let exact_scope = exact_capability_scope
                            .as_ref()
                            .ok_or_else(|| {
                                ListingStoreError::Validation(
                                    "same-case authorization has no exact capability scope"
                                        .to_string(),
                                )
                            })?;
                        let resolution_sha256 = grounded_capability_set_sha256(
                            listing_id,
                            target_id,
                            role.as_str(),
                            quantity,
                            item.configuration_action.as_str(),
                            capabilities,
                        );
                        let observation_sha256 = association_observation_sha256_from_values(
                            listing_id,
                            listing_link_id,
                            role,
                            target_id,
                            item.avionics_model_id,
                            item.replaces_avionics_model_id,
                            item.quantity,
                            item.configuration_action.as_str(),
                            authorization
                                .link_source_notes
                                .as_deref()
                                .unwrap_or_default(),
                        );
                        sqlx::query(&insert_grounded_authorization_sql)
                            .bind(listing_link_id)
                            .bind(role.as_str())
                            .bind(target_id)
                            .bind(observation_sha256)
                            .bind(product_fingerprint)
                            .bind(resolution_sha256)
                            .bind(exact_scope.evidence_capture_sha256.as_str())
                            .bind(exact_scope.plugin_submission_id)
                            .bind(exact_scope.extracted_listing_sha256.as_str())
                            .bind(collision_closure)
                            .bind(source_revocation_count)
                            .execute(&mut *transaction)
                            .await?;
                    }
                    for capability in capabilities {
                        sqlx::query(&delete_grounded_capability_sql)
                            .bind(listing_id)
                            .bind(capability.plugin_submission_id)
                            .bind(capability.occurrence_index)
                            .bind(capability.occurrence_role.as_str())
                            .execute(&mut *transaction)
                            .await?;
                    }
                }
            }
            transaction.commit().await?;
        }};
    }
    match db.backend() {
        DatabaseBackend::Sqlite(pool) => {
            replace_in_transaction!(
                pool,
                reuse_attestation_is_current_sqlite,
                countable_unit_reuse_attestation_is_current_sqlite,
                active_collision_closure_revision_sha256_sqlite
            )
        }
        DatabaseBackend::Postgres(pool) => {
            replace_in_transaction!(
                pool,
                reuse_attestation_is_current_postgres,
                countable_unit_reuse_attestation_is_current_postgres,
                active_collision_closure_revision_sha256_postgres
            )
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
            manufacturer: Some(row.manufacturer),
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
                        manufacturer: Some(manufacturer),
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

    use crate::aircraft::curation::visual::{
        VisibilityBasis, VisibleAircraftIdentifier, VisibleIdentifierKind, VisualConsensusBasis,
        VisualConsensusStatus, VisualEvidenceConfidence, VisualIdentifierImageEvidence,
        VisualIdentifierResolution, VisualIdentifierStatus, VisualPhotoAudit,
        VisualRegistrationConsensus,
    };
    use crate::aircraft::faa::{
        require_listing_faa_admission, store_release, AircraftAdmissionError, BlockReason,
        ReleaseFixtureBuilder, ReleaseMetadata,
    };
    use crate::aircraft::reference::persistence::{
        assemble_and_publish_reference_version, ApprovedReferenceVersionDraft,
        ReferenceApplicabilityDraft, ReferenceComponentDraft, ReferenceConfigurationIdentityDraft,
        ReferencePriceDraft,
    };
    use crate::aircraft::repair::{record_bound_source_visual_correction, AircraftRepairOutcome};
    use crate::avionics::catalog::{
        ApprovedAvionicsIdentity, AvionicsIdentityOutcome, AvionicsIdentityRequest, CatalogError,
    };
    use crate::avionics::manufacturer::{
        ensure_manufacturer_identity, ManufacturerIdentityEvidence,
    };
    use crate::avionics::reuse::{
        refresh_reuse_attestation_sqlite, reuse_attestation_is_current_sqlite,
    };
    use crate::db::{AppDb, DatabaseBackend};
    use crate::extract::{preview_manual_listing, GeminiListingExtractor};
    use crate::listing::avionics::disposition::{AutomaticOccurrenceDisposition, OccurrenceRole};
    use crate::listing::review::{
        stage_pending_review, ListingAssociationRole, PendingReviewAspect, ReviewAction,
        ReviewProduct,
    };
    use crate::models::{ParsedAvionics, ParsedAvionicsReference};

    use super::{
        avionics_from_value, component_time_basis_from_value,
        exact_occurrence_evidence_from_source_units, listing_avionics_identity_request,
        listing_avionics_identity_resolution, listing_avionics_replacement_resolution,
        listing_avionics_value_from_catalog, replace_listing_avionics,
        require_unique_resolved_listing_avionics, resolve_listing_avionics_values,
        validate_component_time, ExactListingSourceCaptureScope, ListingAvionicsIdentityResolution,
        ListingAvionicsReplacementResolution, ListingAvionicsValue, ListingSourceConfidenceBasis,
        ListingValues, ResolvedListingAvionics, AUTOMATED_AVIONICS_VERIFICATION_FAILED_REASON,
        AVIONICS_INSTALLATION_EVIDENCE_NOT_HIGH_REASON,
    };

    struct PendingGroundedReplayFixture {
        db: AppDb,
        listing_id: i64,
        model_id: i64,
        request: AvionicsIdentityRequest,
        scope: super::GroundedCapabilityReplayScope,
    }

    const FAA_AIRCRAFT_REFERENCE: &str = "CODE,MFR,MODEL,TYPE-ACFT,TYPE-ENG,AC-CAT,BUILD-CERT-IND,NO-ENG,NO-SEATS,AC-WEIGHT,SPEED,TC-DATA-SHEET,TC-DATA-HOLDER\n2072738,CESSNA AIRCRAFT CO,182T,4,1,1,0,01,004,CLASS 1,0145,3A13,TEXTRON AVIATION INC\n";
    const FAA_ENGINE_REFERENCE: &str =
        "CODE,MFR,MODEL,TYPE,HORSEPOWER,THRUST\n41528,LYCOMING,IO-540-AB1A5,1,00230,000000\n";

    fn avionics_checkpoint(
        manufacturer: Option<&str>,
        model: &str,
        avionics_types: &[&str],
        quantity: i64,
        source_confidence: &str,
        evidence: &str,
    ) -> String {
        serde_json::to_string(
            &preview_manual_listing(&json!({
                "manufacturer": "Cessna",
                "model": "182",
                "variant": "182T",
                "model_year": 2023,
                "asking_price_usd": 525000,
                "currency": "USD",
                "airframe_hours": 400,
                "status": "active",
                "avionics": [{
                    "manufacturer": manufacturer,
                    "model": model,
                    "types": avionics_types,
                    "quantity": quantity,
                    "configuration_action": "installed",
                    "source_evidence_text": evidence,
                    "source_confidence": source_confidence
                }]
            }))
            .parsed_listing,
        )
        .expect("test avionics checkpoint should serialize")
    }

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

    async fn seed_faa_aircraft_with_absent_claim(
        db: &AppDb,
        matched_n_number: &str,
        serial: &str,
        absent_n_number: &str,
    ) {
        let suffix = matched_n_number
            .strip_prefix('N')
            .expect("test FAA N-number must include N prefix");
        let master = format!(
            "N-NUMBER,SERIAL NUMBER,MFR MDL CODE,ENG MFR MDL,YEAR MFR\n{suffix},{serial},2072738,41528,2023\n"
        );
        let release = ReleaseFixtureBuilder::from_csv(
            ReleaseMetadata::official("2026-07-20", "a".repeat(64)),
            Cursor::new(master),
            Cursor::new(FAA_AIRCRAFT_REFERENCE),
            Cursor::new(FAA_ENGINE_REFERENCE),
            [matched_n_number, absent_n_number],
        )
        .expect("test FAA release should parse");
        store_release(db, &release)
            .await
            .expect("test FAA release should store");
    }

    fn visual_resolution(registration: &str) -> VisualIdentifierResolution {
        let image_id = "asset-primary".to_string();
        VisualIdentifierResolution {
            status: VisualIdentifierStatus::CandidatesVisible,
            candidates: vec![VisibleAircraftIdentifier {
                kind: VisibleIdentifierKind::Registration,
                visible_text: registration.to_string(),
                evidence_count: 1,
                evidence: vec![VisualIdentifierImageEvidence {
                    image_id: image_id.clone(),
                    visible_text: registration.to_string(),
                    confidence: VisualEvidenceConfidence::VeryHigh,
                    box_2d: [100, 100, 200, 300],
                    visibility_basis: VisibilityBasis::ExteriorRegistrationMarking,
                    location_description: "aft fuselage".to_string(),
                }],
            }],
            registration_consensus: VisualRegistrationConsensus {
                status: VisualConsensusStatus::AutoAccept,
                basis: VisualConsensusBasis::SingleRegistrationImage,
                normalized_n_number: Some(registration.to_string()),
                literal_registrations: vec![registration.to_string()],
                literal_serials: Vec::new(),
                registration_evidence_count: 1,
                serial_evidence_count: 0,
                supporting_image_ids: vec![image_id.clone()],
                reason: "one complete visible N-number".to_string(),
            },
            refusal_reason: None,
            photos: vec![VisualPhotoAudit {
                image_id,
                mime_type: "image/jpeg".to_string(),
                byte_count: 3,
                sha256: "b".repeat(64),
            }],
            interaction_id: Some("interaction-test".to_string()),
            model: "gemini-3.1-flash-lite".to_string(),
            prompt_version: "aircraft-visible-identifier-v1".to_string(),
            schema_version: "aircraft-visible-identifier-schema-v1".to_string(),
            total_input_tokens: Some(10),
            total_output_tokens: Some(2),
        }
    }

    fn visual_resolution_for_asset(
        registration: &str,
        asset_id: &str,
    ) -> VisualIdentifierResolution {
        let mut resolution = visual_resolution(registration);
        let image_id = format!("asset-{asset_id}");
        resolution.photos[0].image_id = image_id.clone();
        resolution.registration_consensus.supporting_image_ids = vec![image_id.clone()];
        for candidate in &mut resolution.candidates {
            for evidence in &mut candidate.evidence {
                evidence.image_id = image_id.clone();
            }
        }
        resolution.registration_consensus =
            crate::aircraft::curation::visual::evaluate_visual_registration_consensus(
                &resolution.candidates,
            );
        resolution
    }

    fn visual_resolution_with_serial(
        registration: &str,
        serial: &str,
    ) -> VisualIdentifierResolution {
        let mut resolution = visual_resolution(registration);
        let image_id = resolution.photos[0].image_id.clone();
        resolution.candidates.push(VisibleAircraftIdentifier {
            kind: VisibleIdentifierKind::ManufacturerSerial,
            visible_text: serial.to_string(),
            evidence_count: 1,
            evidence: vec![VisualIdentifierImageEvidence {
                image_id,
                visible_text: serial.to_string(),
                confidence: VisualEvidenceConfidence::VeryHigh,
                box_2d: [250, 250, 350, 600],
                visibility_basis: VisibilityBasis::ManufacturerSerialLabel,
                location_description: "manufacturer data plate".to_string(),
            }],
        });
        resolution.registration_consensus =
            crate::aircraft::curation::visual::evaluate_visual_registration_consensus(
                &resolution.candidates,
            );
        resolution
    }

    struct PreparedBoundVisualCorrection {
        preview: super::ListingPreview,
        correction: super::SourceVisualRegistrationCorrection,
        rendered_html: String,
        binding: super::SignedSourceListingBinding,
        literal_values: ListingValues,
        corrected_values: ListingValues,
    }

    async fn prepare_bound_visual_correction(
        db: &AppDb,
        user_id: i64,
        serial: &str,
    ) -> PreparedBoundVisualCorrection {
        seed_faa_aircraft_with_absent_claim(db, "N123T", serial, "N182PF").await;
        if matches!(db.backend(), DatabaseBackend::Sqlite(_)) {
            seed_curated_test_aircraft_catalog(db, user_id).await;
        }
        let source_url = "https://www.controller.com/listing/for-sale/256858675/2010-cessna-turbo-182t-skylane-piston-single-aircraft";
        let media_url = "https://media.sandhills.com/img.axd?id=1&wid=4326165471&w=0&h=0&sz=Max&checksum=SIGNED1";
        let rendered_html = format!(
            r#"<html><body><p>Registration N182PF</p><div class="mc-items"><div class="mc-item mc-img mc-selected"><img data-fullscreen="{media_url}" alt="Cessna exterior"></div></div></body></html>"#
        );
        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2023,
            "asking_price_usd": 525000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "N182PF",
            "serial_number": null,
            "avionics": []
        }));
        preview.source_url = Some(source_url.to_string());
        preview.context_text = Some("Registration N182PF".to_string());
        let correction = super::admit_source_visual_registration(
            db,
            "N182PF",
            1,
            visual_resolution_for_asset("N123T", "1"),
            media_url.to_string(),
        )
        .await
        .unwrap()
        .unwrap();
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        let bound_json = serde_json::to_string(&preview.parsed_listing).unwrap();
        let bound_sha256 = format!("{:x}", Sha256::digest(bound_json.as_bytes()));
        let install_id = query_scalar_one!(
            db,
            i64,
            "INSERT INTO plugin_installs (user_id, public_key_base64) \
             VALUES (?, 'test-public-key') RETURNING id",
            user_id
        )
        .unwrap();
        let submission_id = query_scalar_one!(
            db,
            i64,
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, submitted_at, rendered_html,
                 rendered_html_sha256, signature_base64
               ) VALUES (?, ?, ?, '2026-07-20 12:34:56', ?, ?, 'test-signature')
               RETURNING id"#,
            user_id,
            install_id,
            source_url,
            &rendered_html,
            &rendered_html_sha256
        )
        .unwrap();
        let binding = super::SignedSourceListingBinding {
            submission_id,
            user_id,
            plugin_install_id: install_id,
            install_public_key_base64: "test-public-key".to_string(),
            install_revoked_at: None,
            source_url: source_url.to_string(),
            submitted_at: "2026-07-20 12:34:56".to_string(),
            rendered_html: rendered_html.clone(),
            rendered_html_sha256,
            signature_base64: "test-signature".to_string(),
            expected_extracted_listing_json: None,
            expected_extracted_listing_sha256: None,
            expected_extraction_error: None,
            bound_extracted_listing_json: bound_json,
            bound_extracted_listing_sha256: bound_sha256,
        };
        let literal_values = super::values_from_preview(&preview, None).unwrap();
        let mut corrected_values = literal_values.clone();
        corrected_values.registration_number =
            Some(correction.corrected_registration_number.clone());
        corrected_values.serial_number = correction.corrected_serial_number.clone();
        super::apply_faa_grounding_identity(&mut corrected_values, &correction.grounding);
        PreparedBoundVisualCorrection {
            preview,
            correction,
            rendered_html,
            binding,
            literal_values,
            corrected_values,
        }
    }

    async fn seed_bound_visual_correction(
        db: &AppDb,
        user_id: i64,
        serial: &str,
    ) -> (
        i64,
        i64,
        super::ListingPreview,
        super::SourceVisualRegistrationCorrection,
        String,
    ) {
        let prepared = prepare_bound_visual_correction(db, user_id, serial).await;
        let listing_id = super::insert_listing(
            db,
            user_id,
            &prepared.corrected_values,
            &prepared.literal_values,
            true,
            Some(&prepared.binding),
            Some(&prepared.correction),
        )
        .await
        .unwrap();
        (
            listing_id,
            prepared.binding.submission_id,
            prepared.preview,
            prepared.correction,
            prepared.rendered_html,
        )
    }

    #[tokio::test]
    async fn source_visual_recovery_accepts_a_different_current_faa_registration() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_faa_aircraft_with_absent_claim(&db, "N123AB", "SERIAL123", "N182PF").await;

        let correction = super::admit_source_visual_registration(
            &db,
            "N182PF",
            1,
            visual_resolution("N123AB"),
            "https://images.example.test/primary.jpg".to_string(),
        )
        .await
        .expect("current FAA lookup should complete")
        .expect("a different exact current FAA registration should be accepted");

        assert_eq!(correction.observed_registration_number, "N182PF");
        assert_eq!(correction.corrected_registration_number, "N123AB");
        assert_eq!(
            correction.corrected_serial_number.as_deref(),
            Some("SERIAL123")
        );
        assert_eq!(correction.resolution.photos.len(), 1);
        assert_eq!(correction.grounding.n_number, "N123AB");
    }

    #[tokio::test]
    async fn source_visual_recovery_rejects_the_same_absent_registration() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_faa_aircraft_with_absent_claim(&db, "N123AB", "SERIAL123", "N182PF").await;

        let correction = super::admit_source_visual_registration(
            &db,
            "N182PF",
            1,
            visual_resolution("N182PF"),
            "https://images.example.test/primary.jpg".to_string(),
        )
        .await
        .expect("visual recovery should fail closed without an FAA error");

        assert!(correction.is_none());
        let error = super::require_aircraft_admission(&db, Some("N182PF"), None)
            .await
            .expect_err("the absent source claim must remain inadmissible");
        assert!(matches!(
            error,
            AircraftAdmissionError::Rejected {
                reason: BlockReason::RegistrationNotFound,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn source_visual_recovery_rejects_malformed_and_non_n_identifiers() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        seed_faa_aircraft_with_absent_claim(&db, "N123AB", "SERIAL123", "N182PF").await;

        for invalid in ["ABC123", "N-@@@"] {
            let correction = super::admit_source_visual_registration(
                &db,
                "N182PF",
                1,
                visual_resolution(invalid),
                "https://images.example.test/primary.jpg".to_string(),
            )
            .await
            .expect("malformed visual identifiers should be discarded safely");
            assert!(correction.is_none(), "{invalid} must not be admitted");
        }
    }

    #[tokio::test]
    async fn source_visual_recovery_requires_an_asserted_serial_to_match_current_faa() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        seed_faa_aircraft_with_absent_claim(&db, "N123AB", "SERIAL-123", "N182PF").await;

        let accepted = super::admit_source_visual_registration(
            &db,
            "N182PF",
            1,
            visual_resolution_with_serial("N123AB", "serial 123"),
            "https://images.example.test/primary.jpg".to_string(),
        )
        .await
        .unwrap()
        .expect("normalized-equal visual and FAA serials must be admitted");
        assert_eq!(
            accepted.corrected_serial_number.as_deref(),
            Some("SERIAL-123")
        );

        let conflict = super::admit_source_visual_registration(
            &db,
            "N182PF",
            1,
            visual_resolution_with_serial("N123AB", "SERIAL-999"),
            "https://images.example.test/primary.jpg".to_string(),
        )
        .await
        .unwrap();
        assert!(conflict.is_none());
    }

    #[tokio::test]
    async fn source_visual_recovery_rejects_an_asserted_serial_when_faa_has_none() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        seed_faa_aircraft_with_absent_claim(&db, "N123AB", "", "N182PF").await;

        let asserted = super::admit_source_visual_registration(
            &db,
            "N182PF",
            1,
            visual_resolution_with_serial("N123AB", "SERIAL-123"),
            "https://images.example.test/primary.jpg".to_string(),
        )
        .await
        .unwrap();
        assert!(asserted.is_none());

        let registration_only = super::admit_source_visual_registration(
            &db,
            "N182PF",
            1,
            visual_resolution("N123AB"),
            "https://images.example.test/primary.jpg".to_string(),
        )
        .await
        .unwrap()
        .expect("registration-only evidence may use the current FAA NULL serial");
        assert_eq!(registration_only.corrected_serial_number, None);
    }

    #[tokio::test]
    async fn exact_current_faa_registration_does_not_invoke_visual_recovery() {
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
        preview.source_url = Some("https://example.test/exact-faa".to_string());
        preview.context_text = Some("Registration N123T; serial TESTSERIAL".to_string());
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        super::create_listing_with_progress_and_occurrence_dispositions(
            &db,
            user.id,
            &preview,
            None,
            Some(&extractor),
            None,
            super::ListingCreationMode::SignedSource,
            None,
        )
        .await
        .expect("an exact current FAA identity should materialize without visual recovery");

        assert_eq!(extractor.primary_visual_recovery_call_count(), 0);
    }

    #[tokio::test]
    async fn interrupted_visual_correction_revalidates_the_exact_receipt_gated_row() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft_with_absent_claim(&db, "N123T", "TESTSERIAL", "N182PF").await;
        let variant_id = seed_curated_test_aircraft_catalog(&db, user.id).await;
        let source_url = "https://example.test/interrupted-visual-correction";
        let listing_id = query_scalar_one!(
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
                    'quarantined', ?, 'N123T', 'TESTSERIAL', 400)
            RETURNING id
            "#,
            variant_id,
            user.id,
            source_url,
            super::SOURCE_IDENTITY_RECEIPT_PENDING
        )
        .expect("receipt-gated corrected listing should seed");
        let mut preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2023,
            "asking_price_usd": 525000,
            "currency": "USD",
            "airframe_hours": 400,
            "status": "active",
            "registration_number": "N182PF",
            "serial_number": null,
            "avionics": []
        }));
        preview.source_url = Some(source_url.to_string());
        preview.context_text = Some("Registration N182PF".to_string());
        let correction = super::admit_source_visual_registration(
            &db,
            "N182PF",
            1,
            visual_resolution("N123T"),
            "https://images.example.test/primary.jpg".to_string(),
        )
        .await
        .expect("FAA revalidation should complete")
        .expect("the different current FAA registration should be recoverable");

        let resumed = super::resume_signed_source_visual_correction_listing_with_correction(
            &db, user.id, listing_id, &preview, None, correction, false,
        )
        .await
        .expect("the exact unchanged receipt-gated row should resume");

        assert_eq!(
            resumed.listing.registration_number.as_deref(),
            Some("N123T")
        );
        assert_eq!(resumed.listing.serial_number.as_deref(), Some("TESTSERIAL"));
        assert!(resumed.source_serial_correction.is_none());
        assert_eq!(
            resumed
                .source_visual_correction
                .as_ref()
                .map(|correction| correction.observed_registration_number.as_str()),
            Some("N182PF")
        );
    }

    #[tokio::test]
    async fn pinned_visual_correction_resumes_without_provider_or_media_and_commits_receipt_gate() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let (listing_id, submission_id, preview, correction, rendered_html) =
            seed_bound_visual_correction(&db, user.id, "TESTSERIAL").await;
        let extractor = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        let resumed = super::resume_signed_source_visual_correction_listing(
            &db,
            user.id,
            listing_id,
            submission_id,
            &preview,
            Some(&extractor),
            &rendered_html,
            false,
        )
        .await
        .expect("restart must use only the pinned signed artifact");
        assert_eq!(extractor.primary_visual_recovery_call_count(), 0);
        assert_eq!(
            resumed.listing.ingestion_error.as_deref(),
            Some(super::SOURCE_IDENTITY_RECEIPT_PENDING)
        );
        let review_aspect = PendingReviewAspect::avionics(
            "avionics:0:primary",
            "avionics",
            "Garmin G5",
            "Garmin G5 · Flight Display · quantity 1 · installed",
            "automated verification could not complete",
            1,
            "installed",
            Some("Garmin G5".to_string()),
            Some("high".to_string()),
        );
        super::replace_listing_pending_review(&db, listing_id, &[review_aspect], true)
            .await
            .expect("review evidence must stage without releasing the correction receipt gate");
        let gated = super::get_listing(&db, user.id, listing_id).await.unwrap();
        assert_eq!(gated.ingestion_state, "quarantined");
        assert_eq!(
            gated.ingestion_error.as_deref(),
            Some(super::SOURCE_IDENTITY_RECEIPT_PENDING)
        );

        let outcome = record_bound_source_visual_correction(
            &db,
            user.id,
            listing_id,
            submission_id,
            resumed.source_visual_correction.as_ref().unwrap(),
        )
        .await
        .expect("the visual receipt and receipt-gate release must commit together");
        assert!(matches!(outcome, AircraftRepairOutcome::Applied { .. }));
        let finalized = super::finalize_signed_source_listing_after_receipt(
            &db,
            user.id,
            listing_id,
            submission_id,
        )
        .await
        .expect("a crash after receipt but before finalization must be resumable");
        assert_eq!(finalized.registration_number.as_deref(), Some("N123T"));
        assert_eq!(finalized.serial_number.as_deref(), Some("TESTSERIAL"));
        assert_ne!(
            finalized.ingestion_error.as_deref(),
            Some(super::SOURCE_IDENTITY_RECEIPT_PENDING)
        );
        assert_eq!(finalized.ingestion_state, "pending_review");
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT pending_aspect_count FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
                listing_id
            )
            .unwrap(),
            1
        );
        assert_eq!(correction.grounding.snapshot.id, 1);
    }

    #[tokio::test]
    async fn visual_correction_receipt_and_finalization_support_a_null_faa_serial() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let (listing_id, submission_id, preview, _, rendered_html) =
            seed_bound_visual_correction(&db, user.id, "").await;

        let resumed = super::resume_signed_source_visual_correction_listing(
            &db,
            user.id,
            listing_id,
            submission_id,
            &preview,
            None,
            &rendered_html,
            false,
        )
        .await
        .expect("NULL FAA serial must compare null-safely");
        assert_eq!(resumed.listing.serial_number, None);
        record_bound_source_visual_correction(
            &db,
            user.id,
            listing_id,
            submission_id,
            resumed.source_visual_correction.as_ref().unwrap(),
        )
        .await
        .expect("NULL-serial visual receipt must commit");
        let finalized = super::finalize_signed_source_listing_after_receipt(
            &db,
            user.id,
            listing_id,
            submission_id,
        )
        .await
        .expect("NULL-serial visual receipt must be found during finalization");
        assert_eq!(finalized.serial_number, None);
    }

    #[tokio::test]
    async fn source_visual_recovery_rejects_a_newer_snapshot_after_the_initial_absence() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        seed_faa_aircraft_with_absent_claim(&db, "N123T", "OLD-SERIAL", "N182PF").await;
        let master = "N-NUMBER,SERIAL NUMBER,MFR MDL CODE,ENG MFR MDL,YEAR MFR\n123T,NEW-SERIAL,2072738,41528,2023\n";
        let release = ReleaseFixtureBuilder::from_csv(
            ReleaseMetadata::official("2026-07-21", "c".repeat(64)),
            Cursor::new(master),
            Cursor::new(FAA_AIRCRAFT_REFERENCE),
            Cursor::new(FAA_ENGINE_REFERENCE),
            ["N123T", "N182PF"],
        )
        .unwrap();
        store_release(&db, &release).await.unwrap();

        let correction = super::admit_source_visual_registration(
            &db,
            "N182PF",
            1,
            visual_resolution("N123T"),
            "https://images.example.test/primary.jpg".to_string(),
        )
        .await
        .unwrap();
        assert!(
            correction.is_none(),
            "visual evidence gathered for snapshot 1 cannot bind against snapshot 2"
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database in AIRCOST_TEST_POSTGRES_URL"]
    async fn postgres_visual_artifact_bind_waits_for_newer_faa_snapshot_and_refuses_stale_pair() {
        let database_url = std::env::var("AIRCOST_TEST_POSTGRES_URL")
            .expect("AIRCOST_TEST_POSTGRES_URL must identify an isolated PostgreSQL database");
        let reset = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::query("DROP SCHEMA public CASCADE")
            .execute(&reset)
            .await
            .unwrap();
        sqlx::query("CREATE SCHEMA public")
            .execute(&reset)
            .await
            .unwrap();
        reset.close().await;

        let db = AppDb::connect(&database_url).await.unwrap();
        let user = db.current_user(None).await.unwrap();
        let prepared = prepare_bound_visual_correction(&db, user.id, "TESTSERIAL").await;
        let DatabaseBackend::Postgres(pool) = db.backend() else {
            unreachable!()
        };
        let mut newer_release = pool.begin().await.unwrap();
        let newer_evidence_source_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO public.curation_evidence_sources (
                 source_url, resolved_url, source_title, publisher, source_domain,
                 source_tier, content_sha256, retrieved_at
               )
               SELECT source_url, resolved_url,
                      'FAA Releasable Aircraft Registry 2026-07-21', publisher,
                      source_domain, source_tier, repeat('c', 64), CURRENT_TIMESTAMP
               FROM public.curation_evidence_sources
               WHERE id = $1
               RETURNING id"#,
        )
        .bind(prepared.correction.grounding.snapshot.evidence_source_id)
        .fetch_one(&mut *newer_release)
        .await
        .unwrap();
        let newer_snapshot_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO public.faa_registry_snapshots (
                 evidence_source_id, snapshot_date, source_url,
                 archive_sha256, source_manifest_sha256, target_set_sha256,
                 master_member_name, master_member_sha256,
                 aircraft_member_name, aircraft_member_sha256,
                 engine_member_name, engine_member_sha256, record_hash_domain
               )
               SELECT $2, '2026-07-21', source_url,
                      repeat('c', 64), repeat('d', 64), repeat('e', 64),
                      master_member_name, master_member_sha256,
                      aircraft_member_name, aircraft_member_sha256,
                      engine_member_name, engine_member_sha256, record_hash_domain
               FROM public.faa_registry_snapshots
               WHERE id = $1
               RETURNING id"#,
        )
        .bind(prepared.correction.grounding.snapshot.id)
        .bind(newer_evidence_source_id)
        .fetch_one(&mut *newer_release)
        .await
        .unwrap();

        let mut bind = Box::pin(super::insert_listing(
            &db,
            user.id,
            &prepared.corrected_values,
            &prepared.literal_values,
            true,
            Some(&prepared.binding),
            Some(&prepared.correction),
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut bind)
                .await
                .is_err(),
            "the initial artifact bind must wait for an in-flight FAA snapshot writer"
        );
        newer_release.commit().await.unwrap();

        let error = bind.await.unwrap_err().to_string();
        assert!(
            error
                .contains("visual correction FAA absence/match pair changed before atomic binding"),
            "unexpected stale-pair refusal: {error}"
        );
        let latest_snapshot_id: i64 = sqlx::query_scalar(
            "SELECT id FROM public.faa_registry_snapshots \
                                ORDER BY snapshot_date DESC, id DESC LIMIT 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(latest_snapshot_id, newer_snapshot_id);
        let artifact_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM public.aircraft_source_visual_correction_artifacts \
             WHERE plugin_submission_id = $1",
        )
        .bind(prepared.binding.submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let canonical_listing_id: Option<i64> = sqlx::query_scalar(
            "SELECT canonical_listing_id FROM public.plugin_submissions WHERE id = $1",
        )
        .bind(prepared.binding.submission_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(artifact_count, 0);
        assert_eq!(canonical_listing_id, None);
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

    async fn publish_test_aircraft_reference(
        db: &AppDb,
        aircraft_model_variant_id: i64,
        model_year: i64,
        avionics_model_ids: &[i64],
    ) {
        let (family_id, designation_id, generation_id, package_id) = query_as_optional!(
            db,
            (i64, i64, Option<i64>, Option<i64>),
            r#"
            SELECT aircraft_model_family_id, aircraft_designation_id,
                   aircraft_generation_id, aircraft_factory_package_id
            FROM aircraft_valuation_compatibility_projections
            WHERE aircraft_model_variant_id = ?
            "#,
            aircraft_model_variant_id
        )
        .expect("reference hierarchy should load")
        .expect("curated compatibility projection should exist");
        let source_id = query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO curation_evidence_sources (
              source_url, source_title, source_domain, source_tier, retrieved_at
            ) VALUES (?, 'Listing test factory reference', 'manufacturer.example',
              'manufacturer_primary', '2026-08-19')
            RETURNING id
            "#,
            format!(
                "https://manufacturer.example/listing-reference/{aircraft_model_variant_id}/{model_year}"
            )
        )
        .expect("primary reference source should seed");
        let claim_id = query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO curation_evidence_claims (
              evidence_source_id, claim_kind, subject_text, predicate_text,
              object_text, quoted_evidence, validation_status, validated_at
            ) VALUES (?, 'specification', 'fixture aircraft', 'defines',
              'complete factory configuration',
              'Primary fixture source defines the complete factory configuration.',
              'validated', '2026-08-19')
            RETURNING id
            "#,
            source_id
        )
        .expect("validated reference claim should seed");
        let applicability_claim_id = query_scalar_one!(
            db,
            i64,
            "INSERT INTO curation_evidence_claims (evidence_source_id, claim_kind, subject_text, predicate_text, object_text, quoted_evidence, validation_status, validated_at) VALUES (?, 'applicability', 'fixture aircraft', 'applies in', 'GLOBAL', 'Primary fixture source defines global applicability.', 'validated', '2026-08-19') RETURNING id",
            source_id
        )
        .expect("validated applicability claim should seed");
        let price_claim_id = query_scalar_one!(
            db,
            i64,
            "INSERT INTO curation_evidence_claims (evidence_source_id, claim_kind, subject_text, predicate_text, object_text, quoted_evidence, validation_status, validated_at) VALUES (?, 'price', 'fixture aircraft', 'equipped MSRP', '500000 USD', 'Primary fixture source states the equipped MSRP.', 'validated', '2026-08-19') RETURNING id",
            source_id
        )
        .expect("validated price claim should seed");
        let factory_claim_id = query_scalar_one!(
            db,
            i64,
            "INSERT INTO curation_evidence_claims (evidence_source_id, claim_kind, subject_text, predicate_text, object_text, quoted_evidence, validation_status, validated_at) VALUES (?, 'standard_equipment', 'fixture aircraft', 'includes', 'factory equipment', 'Primary fixture source defines the standard factory equipment.', 'validated', '2026-08-19') RETURNING id",
            source_id
        )
        .expect("validated factory-equipment claim should seed");
        let observation_id = query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO aircraft_identity_observations (
              observed_make, observed_family, observed_designation, model_year,
              exact_source_evidence, observation_sha256
            ) VALUES ('Listing Test Maker', 'Listing Test Family',
              'Listing Test Designation', ?, 'listing reference fixture', ?)
            RETURNING id
            "#,
            model_year,
            format!("{aircraft_model_variant_id:032x}{model_year:032x}")
        )
        .expect("reference observation should seed");
        let case_id = query_scalar_one!(
            db,
            i64,
            r#"
            INSERT INTO aircraft_identity_resolution_cases (
              observation_id, resolution_scope, job_fingerprint, catalog_revision
            ) VALUES (?, 'reference_profile', ?, 'listing-test')
            RETURNING id
            "#,
            observation_id,
            format!("listing-reference-{aircraft_model_variant_id}-{model_year}")
        )
        .expect("reference resolution case should seed");
        let mut decision_ids = Vec::new();
        for entity_kind in ["reference_configuration", "reference_profile"] {
            let decision_id = query_scalar_one!(
                db,
                i64,
                r#"
                INSERT INTO aircraft_identity_decisions (
                  resolution_case_id, entity_kind, decision_action, decision_status,
                  decision_payload_json, deterministic_validation_json,
                  deterministic_validation_passed, rationale, decided_at
                ) VALUES (?, ?, 'approve_new', 'approved', '{}', '{}', TRUE,
                  'listing test reference', '2026-08-19')
                RETURNING id
                "#,
                case_id,
                entity_kind
            )
            .expect("approved reference decision should seed");
            execute_query!(
                db,
                r#"
                INSERT INTO aircraft_identity_decision_claims (
                  decision_id, evidence_claim_id, evidence_role
                ) VALUES (?, ?, 'identity')
                "#,
                decision_id,
                claim_id
            )
            .expect("reference decision evidence should seed");
            decision_ids.push(decision_id);
        }
        let market_id = query_scalar_one!(
            db,
            i64,
            "SELECT id FROM aircraft_markets WHERE code = 'GLOBAL'"
        )
        .expect("global aircraft market should exist");
        assemble_and_publish_reference_version(
            db,
            &ApprovedReferenceVersionDraft {
                identity: ReferenceConfigurationIdentityDraft {
                    aircraft_model_family_id: family_id,
                    aircraft_designation_id: designation_id,
                    aircraft_generation_id: generation_id,
                    tier_package_id: package_id,
                    display_name: format!("Listing test reference {model_year}"),
                    approval_decision_id: decision_ids[0],
                },
                model_year,
                revision: 1,
                supersedes_version_id: None,
                profile_approval_decision_id: decision_ids[1],
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
                    direct_cited_nominal_dollar_year: model_year,
                    evidence_claim_id: price_claim_id,
                },
                dollar_normalization: None,
                avionics: avionics_model_ids
                    .iter()
                    .copied()
                    .map(|catalog_id| ReferenceComponentDraft {
                        catalog_id,
                        quantity: 1,
                        included_in_tier: false,
                        evidence_claim_id: factory_claim_id,
                    })
                    .collect(),
                engines: vec![],
                propellers: vec![],
                features: vec![],
                avionics_set_evidence_claim_id: factory_claim_id,
                engines_set_evidence_claim_id: factory_claim_id,
                propellers_set_evidence_claim_id: factory_claim_id,
                features_set_evidence_claim_id: factory_claim_id,
            },
        )
        .await
        .expect("complete immutable aircraft reference should publish");
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

    #[test]
    fn listing_validation_rejects_nonpositive_avionics_quantity_before_identity_work() {
        let mut values = listing_values_with_variant("182T SKYLANE");
        let mut generic = ListingAvionicsValue::from_parsed(parsed_avionics("TAWS"));
        generic.quantity = 0;
        values.avionics = vec![generic];

        let error = super::validate_listing_values(&values)
            .expect_err("malformed quantity must fail before deterministic discard");
        assert!(error
            .to_string()
            .contains("avionics quantity must be at least 1"));
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
            None,
            Some("https://example.com/listing"),
            None,
            None,
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
        let retained = "<main>The aircraft has a Garmin GTX-345R installed.</main>";
        let source_units = test_listing_evidence_units("https://example.com/listing", retained);
        let exact_source_capture_scope = validated_test_source_capture_scope(retained);

        let pending = resolve_listing_avionics_values(
            &db,
            &mut values,
            None,
            Some("https://example.com/listing"),
            Some("The aircraft has a Garmin GTX-345R installed."),
            Some(&source_units),
            None,
            Some(&exact_source_capture_scope),
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
    fn identity_work_receives_only_the_checkpoint_occurrence_not_adjacent_fields() {
        let retained = "<main><div>UNRELATED AIRCRAFT DESCRIPTION</div><div>GDC74 Air Data Computer</div><div>UNRELATED PRICE</div></main>";
        let source_units = test_listing_evidence_units("https://example.com/listing", retained);
        let occurrence = exact_occurrence_evidence_from_source_units(
            Some(&source_units),
            Some("GDC74 Air Data Computer"),
        );
        let values = listing_values_with_variant("182T SKYLANE");
        let request = listing_avionics_identity_request(
            &values,
            Some("https://example.com/listing"),
            occurrence,
            "Garmin",
            "GDC 74",
            &["Air Data Computer".to_string()],
            1,
        );

        assert_eq!(occurrence, "GDC74 Air Data Computer");
        assert_eq!(request.listing_context, "GDC74 Air Data Computer");
        assert_eq!(request.manufacturer, "Garmin");
        assert!(!request.listing_context.contains("UNRELATED"));
    }

    #[tokio::test]
    async fn model_only_observation_reuses_one_attested_catalog_product_without_gemini() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "G5", "Flight Display")
                .await
                .expect("approved graph identity should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let retained = "G5 installed";
        let source_units = test_listing_evidence_units("https://example.com/listing", retained);
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(ParsedAvionics {
            manufacturer: None,
            model: "G5".to_string(),
            avionics_types: vec!["Flight Display".to_string()],
            quantity: 1,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some(retained.to_string()),
            source_confidence: Some("high".to_string()),
        })];

        let outcome = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some("https://example.com/listing"),
            Some(retained),
            Some(&source_units),
            None,
            None,
        )
        .await
        .expect("one exact attested model should resolve locally");

        assert!(outcome.pending_review_aspects.is_empty());
        assert_eq!(values.avionics[0].avionics_model_id, Some(approved_id));
        assert_eq!(values.avionics[0].manufacturer.as_deref(), Some("Garmin"));
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[tokio::test]
    async fn unresolved_model_only_observation_stays_pending_without_gemini_or_fake_product() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id =
            ensure_approved_test_avionics_model(&db, "L3", "WX-500", "Lightning Detection")
                .await
                .expect("approved but unattested catalog product should seed");
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let retained = "WX500 Stormscope";
        let source_units = test_listing_evidence_units("https://example.com/listing", retained);
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(ParsedAvionics {
            manufacturer: None,
            model: "WX500".to_string(),
            avionics_types: vec!["Lightning Detection".to_string()],
            quantity: 1,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some(retained.to_string()),
            source_confidence: Some("high".to_string()),
        })];

        let outcome = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some("https://example.com/listing"),
            Some(retained),
            Some(&source_units),
            None,
            None,
        )
        .await
        .expect("unmatched model-only evidence should remain reviewable");

        assert!(values.avionics.is_empty());
        assert_eq!(outcome.pending_review_aspects.len(), 1);
        let aspect = &outcome.pending_review_aspects[0];
        assert_eq!(aspect.label, "WX500");
        assert!(aspect.proposed_product.is_none());
        let suggestion = aspect
            .suggested_product
            .as_ref()
            .expect("the sole collision-safe approved row should be suggested for review");
        assert_eq!(suggestion.id, Some(approved_id));
        assert_eq!(suggestion.manufacturer, "L3");
        assert_eq!(suggestion.model, "WX-500");
        assert!(!aspect
            .allowed_actions
            .contains(&ReviewAction::CreateVerifiedProduct));
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[tokio::test]
    async fn unrebindable_occurrence_never_reaches_the_configured_provider() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(parsed_avionics(
            "GTX 345R",
        ))];
        let retained =
            "<main>The retained listing contains no matching avionics occurrence.</main>";
        let source_units = test_listing_evidence_units("https://example.com/listing", retained);

        let resolved = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some("https://example.com/listing"),
            Some("The retained listing contains no matching avionics occurrence."),
            Some(&source_units),
            None,
            None,
        )
        .await
        .expect("failed occurrence rebinding should stage review without provider work");

        assert_eq!(resolved.pending_review_aspects.len(), 1);
        assert!(values.avionics.is_empty());
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[tokio::test]
    async fn generic_sibling_dom_units_cannot_authorize_cross_unit_occurrence() {
        const SOURCE_URL: &str = "https://example.com/listing";
        const RETAINED_HTML: &str = "<main><div>Garmin GTX</div><div>345R installed</div></main>";
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GTX 345R", "Transponder")
                .await
                .expect("approved graph identity should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;
        let flattened =
            crate::html::listing::source::listing_extraction_source(SOURCE_URL, RETAINED_HTML)
                .expect("generic extraction source should build");
        assert!(flattened.contains("Garmin GTX 345R installed"));
        let source_units = test_listing_evidence_units(SOURCE_URL, RETAINED_HTML);
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(parsed_avionics(
            "GTX 345R",
        ))];

        let resolved = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some(SOURCE_URL),
            Some(&flattened),
            Some(&source_units),
            None,
            None,
        )
        .await
        .expect("cross-unit evidence should stage review without provider work");

        assert_eq!(resolved.pending_review_aspects.len(), 1);
        assert!(values.avionics.is_empty());
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[tokio::test]
    async fn exact_controller_run_on_reuses_only_the_attested_canonical_product() {
        const CONTROLLER_URL: &str =
            "https://www.controller.com/listing/for-sale/257959105/example";
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id = ensure_approved_test_avionics_model(&db, "Garmin", "GIA 63W", "GPS")
            .await
            .expect("approved graph identity should seed");
        for capability in ["NAV", "COM"] {
            let type_id = super::ensure_named_row(&db, "avionics_types", capability)
                .await
                .expect("capability should seed");
            execute_query!(
                &db,
                "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
                approved_id,
                type_id,
            )
            .expect("capability membership should seed");
        }
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;
        let neighboring_id = ensure_approved_test_avionics_model(&db, "Garmin", "GIA 64W", "GPS")
            .await
            .expect("same-maker neighboring identity should seed");
        for capability in ["NAV", "COM"] {
            let type_id = super::ensure_named_row(&db, "avionics_types", capability)
                .await
                .expect("neighboring capability should seed");
            execute_query!(
                &db,
                "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
                neighboring_id,
                type_id,
            )
            .expect("neighboring capability membership should seed");
        }
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, neighboring_id).await;
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        let html = include_str!("../tests/fixtures/controller/id25_like_listing.html").replace(
            "GARMIN GIA-63W #1\nGARMIN GIA-63W #2\nBENDIX/KING KAP 140",
            "GIA63WNAV/COM/GPS(Dual)",
        );
        let source = crate::html::listing::source::listing_extraction_source(CONTROLLER_URL, &html)
            .expect("Controller fixture should produce a bounded source envelope");
        let source_units = test_listing_evidence_units(CONTROLLER_URL, &html);
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
            model: "GIA63W".to_string(),
            avionics_types: vec!["NAV".to_string(), "COM".to_string(), "GPS".to_string()],
            quantity: 2,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some("GIA63WNAV/COM/GPS(Dual)".to_string()),
            source_confidence: Some("high".to_string()),
        })];

        let resolved = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some(CONTROLLER_URL),
            Some(&source),
            Some(&source_units),
            None,
            None,
        )
        .await
        .expect("the exact Controller run-on should resolve without Gemini");

        assert!(resolved.pending_review_aspects.is_empty());
        assert_eq!(values.avionics.len(), 1);
        assert_eq!(values.avionics[0].avionics_model_id, Some(approved_id));
        assert_eq!(values.avionics[0].quantity, 2);
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[tokio::test]
    async fn exact_controller_leading_dual_reuses_a_countable_unit_without_gemini() {
        const CONTROLLER_URL: &str =
            "https://www.controller.com/listing/for-sale/257959105/example";
        const EVIDENCE: &str = "Dual Garmin GIA63W COM/NAV/GPS/WAAS";
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id = ensure_approved_test_avionics_model(&db, "Garmin", "GIA63W", "GPS")
            .await
            .expect("approved graph identity should seed");
        for capability in ["NAV", "COM"] {
            let type_id = super::ensure_named_row(&db, "avionics_types", capability)
                .await
                .expect("capability should seed");
            execute_query!(
                &db,
                "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
                approved_id,
                type_id,
            )
            .expect("capability membership should seed");
        }
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let html = include_str!("../tests/fixtures/controller/id25_like_listing.html").replace(
            "GARMIN GIA-63W #1\nGARMIN GIA-63W #2\nBENDIX/KING KAP 140",
            EVIDENCE,
        );
        let source = crate::html::listing::source::listing_extraction_source(CONTROLLER_URL, &html)
            .expect("Controller fixture should produce a bounded source envelope");
        let source_units = test_listing_evidence_units(CONTROLLER_URL, &html);
        let user = db.current_user(None).await.unwrap();
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .unwrap();
        let listing_id: i64 = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours, ingestion_state
            ) VALUES (?, ?, ?, 2023, 525000, 400, 'incomplete')
            RETURNING id
            "#,
            variant_id,
            user.id,
            CONTROLLER_URL
        )
        .unwrap();
        let install_id: i64 = query_scalar_one!(
            &db,
            i64,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'dual-unit-key') RETURNING id",
            user.id
        )
        .unwrap();
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(html.as_bytes()));
        let extracted_listing_json = avionics_checkpoint(
            Some("Garmin"),
            "GIA63W",
            &["COM", "NAV", "GPS"],
            2,
            "medium",
            EVIDENCE,
        );
        let extracted_listing_sha256 =
            format!("{:x}", Sha256::digest(extracted_listing_json.as_bytes()));
        let proving_submission_id: i64 = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, canonical_listing_id,
              extracted_listing_json
            ) VALUES (?, ?, ?, ?, ?, 'dual-unit-signature', ?, ?)
            RETURNING id
            "#,
            user.id,
            install_id,
            CONTROLLER_URL,
            html.as_str(),
            rendered_html_sha256.as_str(),
            listing_id,
            extracted_listing_json.as_str()
        )
        .unwrap();
        let exact_source_capture_scope = ExactListingSourceCaptureScope {
            plugin_submission_id: proving_submission_id,
            rendered_html_sha256: rendered_html_sha256.clone(),
            extracted_listing_sha256: extracted_listing_sha256.clone(),
        };
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
            model: "GIA63W".to_string(),
            avionics_types: vec!["COM".to_string(), "NAV".to_string(), "GPS".to_string()],
            quantity: 2,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some(EVIDENCE.to_string()),
            source_confidence: Some("medium".to_string()),
        })];

        let resolved = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some(CONTROLLER_URL),
            Some(&source),
            Some(&source_units),
            None,
            Some(&exact_source_capture_scope),
        )
        .await
        .expect("the exact counted unit should resolve without Gemini");

        assert!(resolved.pending_review_aspects.is_empty());
        assert_eq!(values.avionics.len(), 1);
        assert_eq!(values.avionics[0].avionics_model_id, Some(approved_id));
        assert_eq!(values.avionics[0].quantity, 2);
        assert_eq!(
            values.avionics[0].source_confidence.as_deref(),
            Some("high")
        );
        assert_eq!(values.avionics[0].source, "listing_explicit_count");
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );

        let decoy_html = format!("{html}\n<!-- later retained capture -->");
        let decoy_html_sha256 = format!("{:x}", Sha256::digest(decoy_html.as_bytes()));
        execute_query!(
            &db,
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, canonical_listing_id
            ) VALUES (?, ?, ?, ?, ?, 'dual-unit-decoy-signature', ?)
            "#,
            user.id,
            install_id,
            CONTROLLER_URL,
            decoy_html.as_str(),
            decoy_html_sha256.as_str(),
            listing_id
        )
        .unwrap();
        execute_query!(
            &db,
            "UPDATE avionics_models SET valuation_scope = 'integrated_suite' WHERE id = ?",
            approved_id
        )
        .unwrap();
        let stale_error = replace_listing_avionics(&db, listing_id, &values.avionics)
            .await
            .expect_err("a unit-to-suite change before commit must reject the derived count");
        assert!(stale_error
            .to_string()
            .contains("lost its exact countable-unit reuse proof"));
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?",
                listing_id
            )
            .unwrap(),
            0
        );
        execute_query!(
            &db,
            "UPDATE avionics_models SET valuation_scope = 'unit' WHERE id = ?",
            approved_id
        )
        .unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let mut refresh = pool.begin().await.unwrap();
        assert!(refresh_reuse_attestation_sqlite(
            &db,
            &mut refresh,
            approved_id,
            "https://manufacturer.example/manuals/test.pdf",
        )
        .await
        .unwrap());
        refresh.commit().await.unwrap();
        replace_listing_avionics(&db, listing_id, &values.avionics)
            .await
            .expect("the exact counted unit should persist with its signed authorization");
        let persisted: (i64, i64, String, String, String, String, String) = sqlx::query_as(
            r#"
            SELECT link.avionics_model_id, link.quantity, link.source,
                   link.source_confidence, authorization.authorization_kind,
                   authorization.evidence_capture_sha256,
                   authorization.collision_closure_sha256
            FROM aircraft_sale_listing_avionics link
            JOIN aircraft_sale_listing_avionics_link_authorizations authorization
              ON authorization.listing_link_id = link.id
             AND authorization.association_role = 'installed'
            WHERE link.aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let expected_collision_closure =
            crate::avionics::fingerprint::active_collision_closure_revision_sha256(
                &db,
                approved_id,
            )
            .await
            .unwrap();
        assert_eq!(
            persisted,
            (
                approved_id,
                2,
                "listing_explicit_count".to_string(),
                "high".to_string(),
                "manufacturer_reuse".to_string(),
                rendered_html_sha256,
                expected_collision_closure,
            )
        );
        assert_ne!(persisted.5, decoy_html_sha256);
    }

    #[tokio::test]
    async fn exact_controller_model_only_dual_display_reuses_unique_countable_unit() {
        const CONTROLLER_URL: &str = "https://www.controller.com/listing/for-sale/256495649/2006-cessna-turbo-182t-skylane-piston-single-aircraft";
        const EVIDENCE: &str = "Dual GDU-1040 PFD/MFD";
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GDU 1040", "Flight Display")
                .await
                .expect("approved graph identity should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let html = include_str!("../tests/fixtures/controller/id25_like_listing.html").replace(
            "GARMIN GIA-63W #1\nGARMIN GIA-63W #2\nBENDIX/KING KAP 140",
            EVIDENCE,
        );
        let source = crate::html::listing::source::listing_extraction_source(CONTROLLER_URL, &html)
            .expect("Controller fixture should produce a bounded source envelope");
        let source_units = test_listing_evidence_units(CONTROLLER_URL, &html);
        let user = db.current_user(None).await.unwrap();
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .unwrap();
        let listing_id: i64 = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours, ingestion_state
            ) VALUES (?, ?, ?, 2006, 319000, 1250, 'incomplete')
            RETURNING id
            "#,
            variant_id,
            user.id,
            CONTROLLER_URL
        )
        .unwrap();
        let install_id: i64 = query_scalar_one!(
            &db,
            i64,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'model-only-dual-key') RETURNING id",
            user.id
        )
        .unwrap();
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(html.as_bytes()));
        let extracted_listing_json =
            avionics_checkpoint(None, "GDU-1040", &["Flight Display"], 2, "medium", EVIDENCE);
        let extracted_listing_sha256 =
            format!("{:x}", Sha256::digest(extracted_listing_json.as_bytes()));
        let proving_submission_id: i64 = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, canonical_listing_id,
              extracted_listing_json
            ) VALUES (?, ?, ?, ?, ?, 'model-only-dual-signature', ?, ?)
            RETURNING id
            "#,
            user.id,
            install_id,
            CONTROLLER_URL,
            html.as_str(),
            rendered_html_sha256.as_str(),
            listing_id,
            extracted_listing_json.as_str()
        )
        .unwrap();
        let exact_source_capture_scope = ExactListingSourceCaptureScope {
            plugin_submission_id: proving_submission_id,
            rendered_html_sha256: rendered_html_sha256.clone(),
            extracted_listing_sha256,
        };
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(ParsedAvionics {
            manufacturer: None,
            model: "GDU-1040".to_string(),
            avionics_types: vec!["Flight Display".to_string()],
            quantity: 2,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some(EVIDENCE.to_string()),
            source_confidence: Some("medium".to_string()),
        })];

        let resolved = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some(CONTROLLER_URL),
            Some(&source),
            Some(&source_units),
            None,
            Some(&exact_source_capture_scope),
        )
        .await
        .expect("the exact model-only display count should resolve without Gemini");

        assert!(resolved.pending_review_aspects.is_empty());
        assert_eq!(values.avionics.len(), 1);
        assert_eq!(values.avionics[0].avionics_model_id, Some(approved_id));
        assert_eq!(values.avionics[0].manufacturer.as_deref(), Some("Garmin"));
        assert_eq!(values.avionics[0].quantity, 2);
        assert_eq!(
            values.avionics[0].source_confidence.as_deref(),
            Some("high")
        );
        assert_eq!(values.avionics[0].source, "listing_explicit_count");
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );

        replace_listing_avionics(&db, listing_id, &values.avionics)
            .await
            .expect("the model-only counted unit should persist with signed authorization");
        let persisted: (i64, i64, String, String, String) = sqlx::query_as(
            r#"
            SELECT link.avionics_model_id, link.quantity, link.source,
                   authorization.authorization_kind,
                   authorization.evidence_capture_sha256
            FROM aircraft_sale_listing_avionics link
            JOIN aircraft_sale_listing_avionics_link_authorizations authorization
              ON authorization.listing_link_id = link.id
             AND authorization.association_role = 'installed'
            WHERE link.aircraft_sale_listing_id = ?
            "#,
        )
        .bind(listing_id)
        .fetch_one(match db.backend() {
            DatabaseBackend::Sqlite(pool) => pool,
            DatabaseBackend::Postgres(_) => unreachable!("test uses SQLite"),
        })
        .await
        .unwrap();
        assert_eq!(
            persisted,
            (
                approved_id,
                2,
                "listing_explicit_count".to_string(),
                "manufacturer_reuse".to_string(),
                rendered_html_sha256,
            )
        );
    }

    async fn resolve_test_model_only_dual_display(
        db: &AppDb,
    ) -> (ListingValues, ResolvedListingAvionics) {
        const CONTROLLER_URL: &str =
            "https://www.controller.com/listing/for-sale/256495649/example";
        const EVIDENCE: &str = "Dual GDU-1040 PFD/MFD";
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let html = include_str!("../tests/fixtures/controller/id25_like_listing.html").replace(
            "GARMIN GIA-63W #1\nGARMIN GIA-63W #2\nBENDIX/KING KAP 140",
            EVIDENCE,
        );
        let source = crate::html::listing::source::listing_extraction_source(CONTROLLER_URL, &html)
            .expect("Controller fixture should produce a bounded source envelope");
        let source_units = test_listing_evidence_units(CONTROLLER_URL, &html);
        let exact_source_capture_scope = ExactListingSourceCaptureScope {
            plugin_submission_id: 1,
            rendered_html_sha256: format!("{:x}", Sha256::digest(html.as_bytes())),
            extracted_listing_sha256: "0".repeat(64),
        };
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(ParsedAvionics {
            manufacturer: None,
            model: "GDU-1040".to_string(),
            avionics_types: vec!["Flight Display".to_string()],
            quantity: 2,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some(EVIDENCE.to_string()),
            source_confidence: Some("medium".to_string()),
        })];
        let resolved = resolve_listing_avionics_values(
            db,
            &mut values,
            Some(&unreachable),
            Some(CONTROLLER_URL),
            Some(&source),
            Some(&source_units),
            None,
            Some(&exact_source_capture_scope),
        )
        .await
        .expect("unsafe model-only display count should remain reviewable without Gemini");
        (values, resolved)
    }

    async fn assert_model_only_dual_display_pending(
        db: &AppDb,
        values: &ListingValues,
        resolved: &ResolvedListingAvionics,
    ) {
        assert!(values.avionics.is_empty());
        assert_eq!(resolved.pending_review_aspects.len(), 1);
        assert_eq!(
            query_scalar_one!(db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[tokio::test]
    async fn model_only_dual_display_rejects_unattested_stale_ambiguous_and_suite_products() {
        let unattested = AppDb::connect("sqlite::memory:").await.unwrap();
        ensure_approved_test_avionics_model(&unattested, "Garmin", "GDU 1040", "Flight Display")
            .await
            .unwrap();
        let (values, resolved) = resolve_test_model_only_dual_display(&unattested).await;
        assert_model_only_dual_display_pending(&unattested, &values, &resolved).await;

        let stale = AppDb::connect("sqlite::memory:").await.unwrap();
        let stale_id =
            ensure_approved_test_avionics_model(&stale, "Garmin", "GDU 1040", "Flight Display")
                .await
                .unwrap();
        attest_approved_test_avionics_model_for_current_policy_reuse(&stale, stale_id).await;
        let stale_origin_id = query_scalar_one!(
            &stale,
            i64,
            "SELECT avionics_authoritative_source_origin_id FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?",
            stale_id,
        )
        .unwrap();
        let stale_policy = query_scalar_one!(
            &stale,
            String,
            "SELECT policy_version FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?",
            stale_id,
        )
        .unwrap();
        execute_query!(
            &stale,
            "DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?",
            stale_id,
        )
        .unwrap();
        execute_query!(
            &stale,
            r#"
            INSERT INTO avionics_product_reuse_attestations (
              avionics_model_id, avionics_authoritative_source_origin_id,
              policy_version, product_fingerprint
            )
            VALUES (?, ?, ?, ?)
            "#,
            stale_id,
            stale_origin_id,
            stale_policy,
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let (values, resolved) = resolve_test_model_only_dual_display(&stale).await;
        assert_model_only_dual_display_pending(&stale, &values, &resolved).await;

        let ambiguous = AppDb::connect("sqlite::memory:").await.unwrap();
        let canonical_id =
            ensure_approved_test_avionics_model(&ambiguous, "Garmin", "GDU 1040", "Flight Display")
                .await
                .unwrap();
        attest_approved_test_avionics_model_for_current_policy_reuse(&ambiguous, canonical_id)
            .await;
        super::ensure_avionics_model(
            &ambiguous,
            "Other Manufacturer",
            "GDU-1040",
            "Flight Display",
        )
        .await
        .expect("an active cross-manufacturer collision should seed");
        let (values, resolved) = resolve_test_model_only_dual_display(&ambiguous).await;
        assert_model_only_dual_display_pending(&ambiguous, &values, &resolved).await;

        let suite = AppDb::connect("sqlite::memory:").await.unwrap();
        let suite_id =
            ensure_approved_test_avionics_model(&suite, "Garmin", "GDU 1040", "Flight Display")
                .await
                .unwrap();
        execute_query!(
            &suite,
            "UPDATE avionics_models SET valuation_scope = 'integrated_suite' WHERE id = ?",
            suite_id,
        )
        .unwrap();
        attest_approved_test_avionics_model_for_current_policy_reuse(&suite, suite_id).await;
        let (values, resolved) = resolve_test_model_only_dual_display(&suite).await;
        assert_model_only_dual_display_pending(&suite, &values, &resolved).await;
    }

    #[tokio::test]
    async fn exact_controller_leading_dual_does_not_count_an_integrated_suite() {
        const CONTROLLER_URL: &str =
            "https://www.controller.com/listing/for-sale/257959105/example";
        const EVIDENCE: &str = "Dual Garmin G1000 GPS/NAV/WAAS";
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id = ensure_approved_test_avionics_model(&db, "Garmin", "G1000", "GPS")
            .await
            .expect("approved suite identity should seed");
        let nav_id = super::ensure_named_row(&db, "avionics_types", "NAV")
            .await
            .expect("capability should seed");
        execute_query!(
            &db,
            "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
            approved_id,
            nav_id,
        )
        .expect("suite capability should seed");
        execute_query!(
            &db,
            "UPDATE avionics_models SET valuation_scope = 'integrated_suite' WHERE id = ?",
            approved_id,
        )
        .expect("suite scope should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let html = include_str!("../tests/fixtures/controller/id25_like_listing.html").replace(
            "GARMIN GIA-63W #1\nGARMIN GIA-63W #2\nBENDIX/KING KAP 140",
            EVIDENCE,
        );
        let source = crate::html::listing::source::listing_extraction_source(CONTROLLER_URL, &html)
            .expect("Controller fixture should produce a bounded source envelope");
        let source_units = test_listing_evidence_units(CONTROLLER_URL, &html);
        let exact_source_capture_scope = ExactListingSourceCaptureScope {
            plugin_submission_id: 1,
            rendered_html_sha256: format!("{:x}", Sha256::digest(html.as_bytes())),
            extracted_listing_sha256: "0".repeat(64),
        };
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
            model: "G1000".to_string(),
            avionics_types: vec!["GPS".to_string(), "NAV".to_string()],
            quantity: 2,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some(EVIDENCE.to_string()),
            source_confidence: Some("medium".to_string()),
        })];

        let resolved = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some(CONTROLLER_URL),
            Some(&source),
            Some(&source_units),
            None,
            Some(&exact_source_capture_scope),
        )
        .await
        .expect("suite count must remain pending without provider work");

        assert_eq!(resolved.pending_review_aspects.len(), 1);
        assert!(values.avionics.is_empty());
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[tokio::test]
    async fn capture_25_flight_deck_occurrence_reuses_g1000_nxi_without_gemini() {
        const CONTROLLER_URL: &str =
            "https://www.controller.com/listing/for-sale/257959105/example";
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id = ensure_approved_test_avionics_model(
            &db,
            "Garmin",
            "G1000 NXi",
            "Integrated Flight Deck",
        )
        .await
        .expect("approved graph identity should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let html = include_str!("../tests/fixtures/controller/id25_like_listing.html");
        let source = crate::html::listing::source::listing_extraction_source(CONTROLLER_URL, html)
            .expect("capture-25 fixture should produce a bounded source envelope");
        let source_units = test_listing_evidence_units(CONTROLLER_URL, html);
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
            model: "G1000 NXi".to_string(),
            avionics_types: vec!["Integrated Flight Deck".to_string()],
            quantity: 1,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some("GARMIN G1000 NXI".to_string()),
            source_confidence: Some("high".to_string()),
        })];

        let resolved = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some(CONTROLLER_URL),
            Some(&source),
            Some(&source_units),
            None,
            None,
        )
        .await
        .expect("the exact non-radio Controller field should resolve locally");

        assert!(resolved.pending_review_aspects.is_empty());
        assert_eq!(values.avionics.len(), 1);
        assert_eq!(values.avionics[0].avionics_model_id, Some(approved_id));
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[tokio::test]
    async fn controller_run_on_variants_and_non_controller_sources_fail_closed() {
        const CONTROLLER_URL: &str =
            "https://www.controller.com/listing/for-sale/257959105/example";
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id = ensure_approved_test_avionics_model(&db, "Garmin", "GIA 63W", "GPS")
            .await
            .expect("approved graph identity should seed");
        for capability in ["NAV", "COM"] {
            let type_id = super::ensure_named_row(&db, "avionics_types", capability)
                .await
                .expect("capability should seed");
            execute_query!(
                &db,
                "INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id) VALUES (?, ?)",
                approved_id,
                type_id,
            )
            .expect("capability membership should seed");
        }
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;

        for (source_url, evidence, model, capabilities, quantity) in [
            (
                CONTROLLER_URL,
                "GIA63WNXiNAV/COM/GPS(Dual)",
                "GIA 63W NXi",
                vec!["NAV", "COM", "GPS"],
                2,
            ),
            (
                CONTROLLER_URL,
                "GIA63WNAV/COM/GPS/Traffic(Dual)",
                "GIA 63W",
                vec!["NAV", "COM", "GPS", "Traffic"],
                2,
            ),
            (
                CONTROLLER_URL,
                "GIA63WNAV/COM/GPS(Dual)",
                "GIA 63W",
                vec!["NAV", "COM", "GPS"],
                1,
            ),
            (
                "https://example.com/listing",
                "GIA63WNAV/COM/GPS(Dual)",
                "GIA 63W",
                vec!["NAV", "COM", "GPS"],
                2,
            ),
        ] {
            let (source, source_units) = if source_url == CONTROLLER_URL {
                let html = include_str!("../tests/fixtures/controller/id25_like_listing.html")
                    .replace(
                        "GARMIN GIA-63W #1\nGARMIN GIA-63W #2\nBENDIX/KING KAP 140",
                        evidence,
                    );
                (
                    crate::html::listing::source::listing_extraction_source(source_url, &html)
                        .expect("Controller fixture should produce a bounded source envelope"),
                    test_listing_evidence_units(source_url, &html),
                )
            } else {
                (
                    evidence.to_string(),
                    test_listing_evidence_units(source_url, evidence),
                )
            };
            let mut values = listing_values_with_variant("182T SKYLANE");
            values.avionics = vec![ListingAvionicsValue::from_parsed(ParsedAvionics {
                manufacturer: Some("Garmin".to_string()),
                model: model.to_string(),
                avionics_types: capabilities.into_iter().map(str::to_string).collect(),
                quantity,
                configuration_action: "installed".to_string(),
                replaces: None,
                source_evidence_text: Some(evidence.to_string()),
                source_confidence: Some("high".to_string()),
            })];

            let resolved = resolve_listing_avionics_values(
                &db,
                &mut values,
                None,
                Some(source_url),
                Some(&source),
                Some(&source_units),
                None,
                None,
            )
            .await
            .expect("unsafe run-on shape should stage review rather than assign");

            assert_eq!(resolved.pending_review_aspects.len(), 1, "{evidence}");
            assert!(values.avionics.is_empty(), "{evidence}");
        }
    }

    #[tokio::test]
    async fn repeated_exact_occurrences_fail_closed_without_gemini() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let approved_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GDU 1044B", "Flight Display")
                .await
                .expect("approved graph identity should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, approved_id).await;
        let parsed = ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
            model: "GDU 1044B".to_string(),
            avionics_types: vec!["Flight Display".to_string()],
            quantity: 1,
            configuration_action: "installed".to_string(),
            replaces: None,
            source_evidence_text: Some("Garmin GDU 1044B Flight Display".to_string()),
            source_confidence: Some("high".to_string()),
        };
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![
            ListingAvionicsValue::from_parsed(parsed.clone()),
            ListingAvionicsValue::from_parsed(parsed),
        ];
        let retained =
            "Garmin GDU 1044B Flight Display\nGarmin GDU 1044B Flight Display\nUNRELATED FIELD";
        let source_units = test_listing_evidence_units("https://example.com/listing", retained);
        let exact_source_capture_scope = validated_test_source_capture_scope(retained);

        let error = resolve_listing_avionics_values(
            &db,
            &mut values,
            None,
            Some("https://example.com/listing"),
            Some(retained),
            Some(&source_units),
            None,
            Some(&exact_source_capture_scope),
        )
        .await
        .expect_err("duplicate occurrences must not infer one physical quantity");

        assert!(error
            .to_string()
            .contains("resolved from multiple listing occurrences"));
        assert_eq!(values.avionics.len(), 2);
        assert!(values
            .avionics
            .iter()
            .all(|occurrence| occurrence.avionics_model_id.is_none()));
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
                None,
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
            None,
        );
        assert!(matches!(
            high,
            ListingAvionicsIdentityResolution::Approved { identity, .. } if identity.id == 42
        ));
    }

    #[test]
    fn provider_and_catalog_failures_become_safe_pending_reviews() {
        for error in [
            CatalogError::Gemini("provider response included sensitive details".to_string()),
            CatalogError::Validation("catalog response violated an invariant".to_string()),
            CatalogError::Database("database driver internals".to_string()),
        ] {
            let resolution = listing_avionics_identity_resolution(Err(error), Some("high"), None);
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
            None,
        );
        assert!(matches!(
            rejection,
            ListingAvionicsIdentityResolution::GroundedRejected { reason }
                if reason == "grounded rejection"
        ));
    }

    #[test]
    fn listing_avionics_json_requires_explicit_nullable_manufacturer_members() {
        let current = json!([{
            "manufacturer": null,
            "model": "WX500",
            "types": ["Lightning Detection"],
            "quantity": 1,
            "configuration_action": "installed",
            "replaces": null,
            "source_evidence_text": "WX500 Stormscope",
            "source_confidence": "high"
        }]);
        let parsed = avionics_from_value(&current).unwrap();
        assert_eq!(parsed[0].manufacturer, None);

        let mut missing_primary = current.clone();
        missing_primary[0]
            .as_object_mut()
            .unwrap()
            .remove("manufacturer");
        assert!(avionics_from_value(&missing_primary)
            .unwrap_err()
            .to_string()
            .contains("explicitly present"));

        let missing_replacement = json!([{
            "manufacturer": "Garmin",
            "model": "GTN 750Xi",
            "types": ["GPS"],
            "quantity": 1,
            "configuration_action": "replaces",
            "replaces": {"model": "GNS 530W", "types": ["GPS"]},
            "source_evidence_text": "Garmin GTN 750Xi replaces GNS 530W",
            "source_confidence": "high"
        }]);
        assert!(avionics_from_value(&missing_replacement)
            .unwrap_err()
            .to_string()
            .contains("replaces.manufacturer"));
    }

    #[test]
    fn replacement_discards_only_deterministic_generic_rejections() {
        let mut item = ListingAvionicsValue::from_parsed(parsed_avionics("GTN 750Xi"));
        item.configuration_action = "replaces".to_string();
        let replaced = ParsedAvionicsReference {
            manufacturer: Some("Garmin".to_string()),
            model: "GNS 530W".to_string(),
            avionics_types: vec!["GPS".to_string()],
        };

        assert!(matches!(
            listing_avionics_replacement_resolution(
                0,
                &replaced,
                &item,
                ListingAvionicsIdentityResolution::DeterministicGenericRejected,
            ),
            ListingAvionicsReplacementResolution::Rejected
        ));
        let grounded = listing_avionics_replacement_resolution(
            0,
            &replaced,
            &item,
            ListingAvionicsIdentityResolution::GroundedRejected {
                reason: "grounded rejection".to_string(),
            },
        );
        let ListingAvionicsReplacementResolution::Pending(aspect) = grounded else {
            panic!("grounded replacement rejection must remain reviewable")
        };
        assert!(aspect.reason.contains("grounded rejection"));
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
        let source_units =
            test_listing_evidence_units("https://example.com/listing", "GTX 345R installed");

        let pending = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&extractor),
            Some("https://example.com/listing"),
            Some("GTX 345R installed"),
            Some(&source_units),
            None,
            None,
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
    async fn exact_generic_listing_observation_emits_an_explicit_discard_disposition() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let extractor =
            crate::extract::GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        for configured_extractor in [None, Some(&extractor)] {
            for (manufacturer, model, capability) in [
                (Some("Garmin"), "Primary Flight Display", "Flight Display"),
                (Some("Garmin"), "Multifunction Display", "Flight Display"),
                (None, "Synthetic Vision Technology (SVT)", "Flight Display"),
                (None, "XM Weather & Audio", "Datalink"),
                (
                    Some("PS Engineering"),
                    "4-Place Voice-Activated Intercom System",
                    "Audio Panel",
                ),
                (
                    Some("Electronics International"),
                    "Digital EGT, CHT, & Outside Air Temp Gauge",
                    "Engine Monitor",
                ),
                (None, "Pilot's Clock", "Clock/Timer"),
                (None, "Remote ELT", "ELT"),
            ] {
                let mut values = listing_values_with_variant("182T SKYLANE");
                let mut generic = parsed_avionics(model);
                generic.manufacturer = manufacturer.map(str::to_string);
                generic.avionics_types = vec![capability.to_string()];
                values.avionics = vec![ListingAvionicsValue::from_parsed(generic)];

                let resolved = resolve_listing_avionics_values(
                    &db,
                    &mut values,
                    configured_extractor,
                    Some("https://example.com/listing"),
                    Some(model),
                    None,
                    None,
                    None,
                )
                .await
                .expect("exact generic discard must not depend on provider availability");

                assert!(resolved.pending_review_aspects.is_empty(), "{model}");
                assert!(values.avionics.is_empty(), "{model}");
                assert_eq!(resolved.occurrence_dispositions.len(), 1, "{model}");
                let disposition = &resolved.occurrence_dispositions[0];
                assert_eq!(disposition.outcome, "discarded", "{model}");
                assert_eq!(disposition.avionics_model_id, None, "{model}");
                assert_eq!(
                    disposition.reason_code, "automatic_identity_rejected",
                    "{model}"
                );
            }
        }
    }

    #[tokio::test]
    async fn ambiguous_uavionix_wingtip_description_remains_pending() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let mut values = listing_values_with_variant("182T SKYLANE");
        let mut ambiguous = parsed_avionics("Wingtip Beacons");
        ambiguous.manufacturer = Some("Uavionix".to_string());
        ambiguous.avionics_types = vec!["Transponder".to_string()];
        values.avionics = vec![ListingAvionicsValue::from_parsed(ambiguous)];

        let resolved = resolve_listing_avionics_values(
            &db,
            &mut values,
            None,
            Some("https://example.com/listing"),
            Some("Uavionix Wingtip Beacons with ADS-B IN & OUT"),
            None,
            None,
            None,
        )
        .await
        .expect("an ambiguous product description should stage review work");

        assert_eq!(resolved.pending_review_aspects.len(), 1);
        assert!(resolved.occurrence_dispositions.is_empty());
        assert!(values.avionics.is_empty());
    }

    #[tokio::test]
    async fn generic_primary_does_not_discard_a_concrete_replacement_without_a_key() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let replacement_id = ensure_approved_test_avionics_model(&db, "Garmin", "GNS 430W", "GPS")
            .await
            .expect("concrete replacement should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, replacement_id).await;
        let unreachable =
            crate::extract::GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let source_units = test_listing_evidence_units(
            "https://example.com/listing",
            "Garmin GPS replaces Garmin GNS 430W",
        );

        for configured_extractor in [None, Some(&unreachable)] {
            let mut values = listing_values_with_variant("182T SKYLANE");
            let mut generic = parsed_avionics("GPS");
            generic.configuration_action = "replaces".to_string();
            generic.source_evidence_text = Some("Garmin GPS replaces Garmin GNS 430W".to_string());
            generic.replaces = Some(ParsedAvionicsReference {
                manufacturer: Some("Garmin".to_string()),
                model: "GNS 430W".to_string(),
                avionics_types: vec!["GPS".to_string()],
            });
            values.avionics = vec![ListingAvionicsValue::from_parsed(generic)];

            let resolved = resolve_listing_avionics_values(
                &db,
                &mut values,
                configured_extractor,
                Some("https://example.com/listing"),
                Some("Garmin GPS replaces Garmin GNS 430W"),
                Some(&source_units),
                None,
                None,
            )
            .await
            .expect("each occurrence role should resolve independently");

            assert!(resolved.pending_review_aspects.is_empty());
            assert!(values.avionics.is_empty());
            assert_eq!(resolved.occurrence_dispositions.len(), 2);
            assert_eq!(
                resolved.occurrence_dispositions[0],
                AutomaticOccurrenceDisposition::discarded(0, OccurrenceRole::Primary)
            );
            assert_eq!(
                resolved.occurrence_dispositions[1],
                AutomaticOccurrenceDisposition::linked(
                    0,
                    OccurrenceRole::Replacement,
                    replacement_id,
                )
            );
        }
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
    }

    #[tokio::test]
    async fn concrete_primary_keeps_its_local_link_when_replacement_is_generic() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let primary_id = ensure_approved_test_avionics_model(&db, "Garmin", "GNS 430W", "GPS")
            .await
            .expect("concrete primary should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, primary_id).await;
        let unreachable =
            crate::extract::GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let source_units = test_listing_evidence_units(
            "https://example.com/listing",
            "Garmin GNS 430W changes Garmin GPS",
        );
        let exact_source_capture_scope =
            validated_test_source_capture_scope("Garmin GNS 430W changes Garmin GPS");

        for configured_extractor in [None, Some(&unreachable)] {
            for action in ["replaces", "removes"] {
                let mut values = listing_values_with_variant("182T SKYLANE");
                let mut concrete = parsed_avionics("GNS 430W");
                concrete.configuration_action = action.to_string();
                concrete.source_evidence_text =
                    Some("Garmin GNS 430W changes Garmin GPS".to_string());
                concrete.replaces = Some(ParsedAvionicsReference {
                    manufacturer: Some("Garmin".to_string()),
                    model: "GPS".to_string(),
                    avionics_types: vec!["GPS".to_string()],
                });
                values.avionics = vec![ListingAvionicsValue::from_parsed(concrete)];

                let resolved = resolve_listing_avionics_values(
                    &db,
                    &mut values,
                    configured_extractor,
                    Some("https://example.com/listing"),
                    Some("Garmin GNS 430W changes Garmin GPS"),
                    Some(&source_units),
                    None,
                    Some(&exact_source_capture_scope),
                )
                .await
                .expect("each occurrence role should resolve independently");

                assert!(resolved.pending_review_aspects.is_empty());
                assert_eq!(values.avionics.len(), 1);
                assert_eq!(values.avionics[0].avionics_model_id, Some(primary_id));
                assert_eq!(
                    values.avionics[0].configuration_action,
                    if action == "removes" {
                        "removes"
                    } else {
                        "installed"
                    }
                );
                assert_eq!(
                    values.avionics[0].replaces_avionics_model_id,
                    (action == "removes").then_some(primary_id)
                );
                assert_eq!(resolved.occurrence_dispositions.len(), 2);
                assert_eq!(
                    resolved.occurrence_dispositions[0],
                    AutomaticOccurrenceDisposition::linked(0, OccurrenceRole::Primary, primary_id)
                );
                assert_eq!(
                    resolved.occurrence_dispositions[1],
                    AutomaticOccurrenceDisposition::discarded(0, OccurrenceRole::Replacement)
                );
            }
        }
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
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
            ListingSourceConfidenceBasis::RetainedHigh,
        );
        item.configuration_action = "replaces".to_string();
        item.replaces = Some(crate::models::ParsedAvionicsReference {
            manufacturer: Some("Unknown Maker".to_string()),
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
            ListingSourceConfidenceBasis::RetainedHigh,
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

    #[test]
    fn grounded_capability_prebind_rejects_multiple_occurrence_receipts() {
        let values = listing_values_with_variant("182T SKYLANE");
        let context = "GTX 345R installed";
        let identity = approved_avionics_identity();
        let make_seed = |quantity| {
            let request = super::listing_avionics_identity_request(
                &values,
                values.source_url.as_deref(),
                context,
                "Garmin",
                "GTX 345R",
                &["Transponder".to_string()],
                quantity,
            );
            super::grounded_resolution_receipt_seed_for_replay(&request, &identity)
        };
        let mut item = listing_avionics_value_from_catalog(
            &ListingAvionicsValue::from_parsed(parsed_avionics("GTX 345R")),
            &identity,
            ListingSourceConfidenceBasis::RetainedHigh,
        );
        item.quantity = i64::MAX;
        item.grounded_capabilities = vec![
            super::ListingGroundedCapability {
                occurrence_index: 0,
                occurrence_role: OccurrenceRole::Primary,
                configuration_action: "installed".to_string(),
                source_notes: Some("GTX 345R installed".to_string()),
                seed: make_seed(i64::MAX),
            },
            super::ListingGroundedCapability {
                occurrence_index: 1,
                occurrence_role: OccurrenceRole::Primary,
                configuration_action: "installed".to_string(),
                source_notes: Some("GTX 345R installed".to_string()),
                seed: make_seed(1),
            },
        ];

        let error = super::prepare_grounded_capability_bindings(&[item])
            .expect_err("one catalog link cannot consume multiple primary occurrences");
        assert!(error
            .to_string()
            .contains("one grounded occurrence must cover the exact quantity"));
    }

    #[test]
    fn grounded_replacement_capabilities_reject_multiple_occurrences() {
        let values = listing_values_with_variant("182T SKYLANE");
        let context = "GTX 345R replaces GTX 327";
        let mut replacement_identity = approved_avionics_identity();
        replacement_identity.id = 327;
        replacement_identity.model = "GTX 327".to_string();
        let replacement_request = super::listing_avionics_identity_request(
            &values,
            values.source_url.as_deref(),
            context,
            "Garmin",
            "GTX 327",
            &["Transponder".to_string()],
            1,
        );
        let replacement_seed = super::grounded_resolution_receipt_seed_for_replay(
            &replacement_request,
            &replacement_identity,
        );
        let primary_identity = approved_avionics_identity();
        let mut item = listing_avionics_value_from_catalog(
            &ListingAvionicsValue::from_parsed(parsed_avionics("GTX 345R")),
            &primary_identity,
            ListingSourceConfidenceBasis::RetainedHigh,
        );
        item.configuration_action = "replaces".to_string();
        item.replaces_avionics_model_id = Some(replacement_identity.id);
        item.replacement_grounded_capabilities = (0..2)
            .map(|occurrence_index| super::ListingGroundedCapability {
                occurrence_index,
                occurrence_role: OccurrenceRole::Replacement,
                configuration_action: "replaces".to_string(),
                source_notes: Some("GTX 345R replaces GTX 327".to_string()),
                seed: replacement_seed.clone(),
            })
            .collect();

        let error = super::prepare_grounded_capability_bindings(&[item])
            .expect_err("one replacement link cannot consume multiple occurrences");
        assert!(error
            .to_string()
            .contains("one grounded occurrence must cover the exact replacement semantics"));
    }

    #[test]
    fn same_case_authorization_digest_commits_exact_occurrence_capabilities() {
        let row = |occurrence_index, capability_byte: char| super::StoredGroundedCapabilityRow {
            plugin_submission_id: 7,
            occurrence_index,
            occurrence_role: "primary".to_string(),
            avionics_model_id: 42,
            requested_quantity: 1,
            configuration_action: "installed".to_string(),
            request_sha256: "1".repeat(64),
            capability_sha256: capability_byte.to_string().repeat(64),
            grounded_resolution_sha256: "3".repeat(64),
            evidence_capture_sha256: "4".repeat(64),
            extracted_listing_sha256: "5".repeat(64),
            submission_canonical_listing_id: Some(11),
            submission_rendered_html_sha256: Some("4".repeat(64)),
            extracted_listing_json: Some("{}".to_string()),
            extraction_error: None,
            product_fingerprint: "6".repeat(64),
            collision_closure_sha256: "7".repeat(64),
            source_revocation_count: 0,
        };
        let ordered = vec![row(0, 'a'), row(1, 'b')];
        let reversed = vec![row(1, 'b'), row(0, 'a')];
        let altered_occurrence = vec![row(0, 'a'), row(1, 'c')];
        let digest =
            super::grounded_capability_set_sha256(11, 42, "installed", 2, "installed", &ordered);
        assert_eq!(
            digest,
            super::grounded_capability_set_sha256(11, 42, "installed", 2, "installed", &reversed,),
            "database row order must not change an authorization"
        );
        assert_ne!(
            digest,
            super::grounded_capability_set_sha256(
                11,
                42,
                "installed",
                2,
                "installed",
                &altered_occurrence,
            ),
            "the final authorization must commit each consumed occurrence capability"
        );
    }

    #[tokio::test]
    async fn replay_retires_capability_for_product_removed_from_approved_graph() {
        let fixture = pending_grounded_replay_fixture(false).await;
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        // Simulate an externally migrated/corrupted catalog state. Normal
        // application writes cannot demote an approved product, but replay
        // must still retire an old capability if the approved graph is absent.
        sqlx::query("DROP TRIGGER avionics_models_approved_identity_immutable")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE avionics_models SET catalog_status = 'unreviewed', verification_method = NULL, verified_by_user_id = NULL WHERE id = ?",
        )
            .bind(fixture.model_id)
            .execute(pool)
            .await
            .unwrap();

        let replay = super::replay_grounded_listing_avionics_identity(
            &fixture.db,
            &fixture.scope,
            &fixture.request,
            0,
            OccurrenceRole::Primary,
            "installed",
            Some("GTX 345R installed"),
        )
        .await
        .expect("an absent approved product is stale proof, not permanent poison");
        assert!(matches!(
            replay,
            super::GroundedCapabilityReplayOutcome::RetiredStale
        ));
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
        assert!(
            super::grounded_capability_replay_scope(&fixture.db, fixture.listing_id)
                .await
                .expect(
                    "the exact retained signed checkpoint should remain available for re-grounding"
                )
                .is_some()
        );
    }

    #[tokio::test]
    async fn replay_retires_capability_after_collision_closure_drift() {
        let fixture = pending_grounded_replay_fixture(true).await;
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };

        let replay = super::replay_grounded_listing_avionics_identity(
            &fixture.db,
            &fixture.scope,
            &fixture.request,
            0,
            OccurrenceRole::Primary,
            "installed",
            Some("GTX 345R installed"),
        )
        .await
        .expect("collision drift is stale proof, not permanent poison");
        assert!(matches!(
            replay,
            super::GroundedCapabilityReplayOutcome::RetiredStale
        ));
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
        assert!(
            super::grounded_capability_replay_scope(&fixture.db, fixture.listing_id)
                .await
                .expect("the signed checkpoint should survive stale capability retirement")
                .is_some()
        );
    }

    #[tokio::test]
    async fn replay_scope_retires_capability_when_extraction_checkpoint_is_cleared() {
        let fixture = pending_grounded_replay_fixture(false).await;
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        sqlx::query("UPDATE plugin_submissions SET extracted_listing_json = NULL WHERE id = ?")
            .bind(fixture.scope.plugin_submission_id)
            .execute(pool)
            .await
            .unwrap();

        let retired_scope =
            super::grounded_capability_replay_scope(&fixture.db, fixture.listing_id)
                .await
                .expect("a cleared checkpoint is stale proof, not a replay error")
                .expect("the stale attempt must retain its paid-fallback suppression");
        assert!(!retired_scope.allow_provider_fallback);
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn replay_scope_retires_capability_when_extraction_becomes_failed() {
        let fixture = pending_grounded_replay_fixture(false).await;
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        sqlx::query(
            "UPDATE plugin_submissions SET extraction_error = 'invalid checkpoint' WHERE id = ?",
        )
        .bind(fixture.scope.plugin_submission_id)
        .execute(pool)
        .await
        .unwrap();

        let retired_scope =
            super::grounded_capability_replay_scope(&fixture.db, fixture.listing_id)
                .await
                .expect("a failed checkpoint is stale proof, not a replay error")
                .expect("the stale attempt must retain its paid-fallback suppression");
        assert!(!retired_scope.allow_provider_fallback);
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities WHERE listing_id = ?",
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
    }

    async fn assert_submission_scope_drift_retires_capability_without_provider(
        drift_canonical_binding: bool,
    ) {
        let fixture = pending_grounded_replay_fixture(false).await;
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        if drift_canonical_binding {
            sqlx::query("UPDATE plugin_submissions SET canonical_listing_id = NULL WHERE id = ?")
                .bind(fixture.scope.plugin_submission_id)
                .execute(pool)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE plugin_submissions SET rendered_html_sha256 = ? WHERE id = ?")
                .bind("f".repeat(64))
                .bind(fixture.scope.plugin_submission_id)
                .execute(pool)
                .await
                .unwrap();
        }

        let scope = super::grounded_capability_replay_scope(&fixture.db, fixture.listing_id)
            .await
            .expect("stale capability retirement should succeed")
            .expect("the retired scope should block paid fallback in this attempt");
        assert!(!scope.allow_provider_fallback);
        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(parsed_avionics(
            "GTX 345R",
        ))];
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let source_units = test_listing_evidence_units(
            "https://example.com/listing",
            "<p>Garmin GTX 345R installed</p>",
        );
        let resolution = resolve_listing_avionics_values(
            &fixture.db,
            &mut values,
            Some(&unreachable),
            Some("https://example.com/listing"),
            Some("Garmin GTX 345R installed"),
            Some(&source_units),
            Some(&scope),
            None,
        )
        .await
        .expect("stale proof should become review work without paid fallback");

        assert_eq!(resolution.pending_review_aspects.len(), 1);
        assert!(values.avionics.is_empty());
        let state: (i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities WHERE listing_id = ?),
                 (SELECT COUNT(*) FROM gemini_api_usage)"#,
        )
        .bind(fixture.listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(state, (0, 0));
    }

    #[tokio::test]
    async fn canonical_listing_drift_retires_capability_without_paid_fallback() {
        assert_submission_scope_drift_retires_capability_without_provider(true).await;
    }

    #[tokio::test]
    async fn rendered_capture_drift_retires_capability_without_paid_fallback() {
        assert_submission_scope_drift_retires_capability_without_provider(false).await;
    }

    #[tokio::test]
    async fn source_revocation_invalidates_every_pending_grounded_capability() {
        let fixture = pending_grounded_replay_fixture(false).await;
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        attest_approved_test_avionics_model_for_current_policy_reuse(&fixture.db, fixture.model_id)
            .await;

        revoke_test_manufacturer_source_origin(&fixture.db, fixture.model_id).await;

        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            remaining, 0,
            "one append-only authority revocation invalidates every pending grounded capability"
        );
    }

    #[tokio::test]
    async fn source_revocation_epoch_retires_a_surviving_capability_without_provider_fallback() {
        let fixture = pending_grounded_replay_fixture(false).await;
        let DatabaseBackend::Sqlite(pool) = fixture.db.backend() else {
            unreachable!()
        };
        attest_approved_test_avionics_model_for_current_policy_reuse(&fixture.db, fixture.model_id)
            .await;
        // Simulate a database whose external cleanup trigger was removed after
        // startup. Replay must independently reject the stale epoch.
        sqlx::query("DROP TRIGGER listing_avionics_authorizations_invalidate_origin_revocation")
            .execute(pool)
            .await
            .unwrap();
        revoke_test_manufacturer_source_origin(&fixture.db, fixture.model_id).await;
        let surviving: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(surviving, 1, "the fixture must exercise runtime defense");

        let mut values = listing_values_with_variant("182T SKYLANE");
        values.avionics = vec![ListingAvionicsValue::from_parsed(parsed_avionics(
            "GTX 345R",
        ))];
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let source_units = test_listing_evidence_units(
            "https://example.com/listing",
            "<p>Garmin GTX 345R installed</p>",
        );
        let resolution = resolve_listing_avionics_values(
            &fixture.db,
            &mut values,
            Some(&unreachable),
            Some("https://example.com/listing"),
            Some("Garmin GTX 345R installed"),
            Some(&source_units),
            Some(&fixture.scope),
            None,
        )
        .await
        .expect("a stale source epoch must fail closed as pending review");

        assert_eq!(resolution.pending_review_aspects.len(), 1);
        assert!(values.avionics.is_empty());
        let state: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities),
                 (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_link_authorizations),
                 (SELECT COUNT(*) FROM gemini_api_usage)"#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            state,
            (0, 0, 0),
            "runtime replay must retire stale authority and invoke no paid fallback"
        );
    }

    #[tokio::test]
    async fn paid_same_case_capability_precedes_reuse_and_survives_later_reuse_revocation() {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let user = db.current_user(None).await.unwrap();
        let model_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GTX 345R", "Transponder")
                .await
                .unwrap();
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, model_id).await;
        let identity = super::approved_avionics_identity_for_grounded_replay(&db, model_id)
            .await
            .unwrap()
            .unwrap();
        let mut values = listing_values_with_variant("182T SKYLANE");
        let parsed = parsed_avionics("GTX 345R");
        let context = "GTX 345R installed";
        let request = super::listing_avionics_identity_request(
            &values,
            values.source_url.as_deref(),
            context,
            parsed.manufacturer.as_deref().unwrap(),
            &parsed.model,
            &parsed.avionics_types,
            parsed.quantity,
        );
        let product_fingerprint = super::catalog_product_fingerprint_for_id(&db, model_id)
            .await
            .unwrap();
        let collision_closure = super::grounded_collision_closure_revision_sha256(&db, model_id)
            .await
            .unwrap();
        let seed = super::grounded_resolution_receipt_basis_for_replay(&request, &identity)
            .bind_catalog_snapshot(product_fingerprint, collision_closure, 0);
        let mut resolved = listing_avionics_value_from_catalog(
            &ListingAvionicsValue::from_parsed(parsed),
            &identity,
            ListingSourceConfidenceBasis::RetainedHigh,
        );
        resolved
            .grounded_capabilities
            .push(super::ListingGroundedCapability {
                occurrence_index: 0,
                occurrence_role: OccurrenceRole::Primary,
                configuration_action: "installed".to_string(),
                source_notes: Some("GTX 345R installed".to_string()),
                seed,
            });
        values.avionics = vec![resolved];

        let rendered_html = "<p>GTX 345R installed</p>";
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        let extracted_listing_json = avionics_checkpoint(
            Some("Garmin"),
            "GTX 345R",
            &["Transponder"],
            1,
            "high",
            "GTX 345R installed",
        );
        let extracted_listing_sha256 =
            format!("{:x}", Sha256::digest(extracted_listing_json.as_bytes()));
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64, created_at) VALUES (?, 'grounded-bind-key', '2026-08-23 12:34:56') RETURNING id",
        )
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let submitted_at = "2026-08-24 12:34:56";
        let submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, submitted_at,
                 rendered_html, rendered_html_sha256, signature_base64
               ) VALUES (?, ?, ?, ?, ?, ?, 'grounded-bind-signature')
               RETURNING id"#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind(values.source_url.as_deref().unwrap())
        .bind(submitted_at)
        .bind(rendered_html)
        .bind(rendered_html_sha256.as_str())
        .fetch_one(pool)
        .await
        .unwrap();
        let binding = super::SignedSourceListingBinding {
            submission_id,
            user_id: user.id,
            plugin_install_id: install_id,
            install_public_key_base64: "grounded-bind-key".to_string(),
            install_revoked_at: None,
            source_url: values.source_url.clone().unwrap(),
            submitted_at: submitted_at.to_string(),
            rendered_html: rendered_html.to_string(),
            rendered_html_sha256,
            signature_base64: "grounded-bind-signature".to_string(),
            expected_extracted_listing_json: None,
            expected_extracted_listing_sha256: None,
            expected_extraction_error: None,
            bound_extracted_listing_json: extracted_listing_json,
            bound_extracted_listing_sha256: extracted_listing_sha256,
        };
        let literal_values = values.clone();

        let listing_id = super::insert_listing(
            &db,
            user.id,
            &values,
            &literal_values,
            false,
            Some(&binding),
            None,
        )
        .await
        .expect("normal signed-source materialization should consume the same-case proof");

        sqlx::query("DELETE FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?")
            .bind(model_id)
            .execute(pool)
            .await
            .unwrap();

        let state: (i64, i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?),
                 (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_link_authorizations authorization
                    JOIN aircraft_sale_listing_avionics link ON link.id = authorization.listing_link_id
                   WHERE link.aircraft_sale_listing_id = ? AND authorization.authorization_kind = 'same_case_grounded'),
                 (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities WHERE listing_id = ?),
                 (SELECT COUNT(*) FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?)"#,
        )
        .bind(listing_id)
        .bind(listing_id)
        .bind(listing_id)
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(state, (1, 1, 0, 0));
        let provider_calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(provider_calls, 0);

        revoke_test_manufacturer_source_origin(&db, model_id).await;
        let authorization_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_link_authorizations",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            authorization_count, 0,
            "a later authority revocation must invalidate materialized same-case authorization"
        );
    }

    #[tokio::test]
    async fn grounded_capability_survives_failed_bind_and_replays_without_provider_or_global_reuse()
    {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects SQLite")
        };
        let user = db.current_user(None).await.unwrap();
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listings (aircraft_model_variant_id, created_by_user_id, source_url, model_year, asking_price_usd, airframe_hours) VALUES (?, ?, 'https://example.com/listing', 2023, 300000, 1000) RETURNING id",
        )
        .bind(variant_id)
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GTX 345R", "Transponder")
                .await
                .unwrap();
        let identity = super::approved_avionics_identity_for_grounded_replay(&db, model_id)
            .await
            .unwrap()
            .unwrap();
        let mut values = listing_values_with_variant("182T SKYLANE");
        let listing_context = "GTX 345R installed";
        let request = super::listing_avionics_identity_request(
            &values,
            Some("https://example.com/listing"),
            listing_context,
            "Garmin",
            "GTX 345R",
            &["Transponder".to_string()],
            1,
        );
        let rendered_html = "<p>Garmin GTX 345R installed</p>";
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        let extraction_json = avionics_checkpoint(
            Some("Garmin"),
            "GTX 345R",
            &["Transponder"],
            1,
            "high",
            "GTX 345R installed",
        );
        let extracted_listing_sha256 = format!("{:x}", Sha256::digest(extraction_json.as_bytes()));
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'grounded-test-key') RETURNING id",
        )
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, rendered_html,
                 rendered_html_sha256, signature_base64,
                 extracted_listing_json, canonical_listing_id
               ) VALUES (?, ?, 'https://example.com/listing', ?, ?, 'signature', ?, ?)
               RETURNING id"#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind(rendered_html)
        .bind(rendered_html_sha256.as_str())
        .bind(extraction_json.as_str())
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let product_fingerprint = super::catalog_product_fingerprint_for_id(&db, model_id)
            .await
            .unwrap();
        let collision_closure = super::grounded_collision_closure_revision_sha256(&db, model_id)
            .await
            .unwrap();
        let seed = super::grounded_resolution_receipt_basis_for_replay(&request, &identity)
            .bind_catalog_snapshot(product_fingerprint.clone(), collision_closure.clone(), 0);
        let capability = super::ListingGroundedCapability {
            occurrence_index: 0,
            occurrence_role: OccurrenceRole::Primary,
            configuration_action: "installed".to_string(),
            source_notes: Some("GTX 345R installed".to_string()),
            seed: seed.clone(),
        };
        sqlx::query(
            r#"INSERT INTO aircraft_sale_listing_avionics_grounded_capabilities (
                 listing_id, plugin_submission_id, occurrence_index, occurrence_role,
                 avionics_model_id, requested_quantity, configuration_action,
                 request_sha256, capability_sha256, grounded_resolution_sha256,
                 evidence_capture_sha256, extracted_listing_sha256,
                 product_fingerprint, collision_closure_sha256,
                 source_revocation_count
               ) VALUES (?, ?, 0, 'primary', ?, 1, 'installed', ?, ?, ?, ?, ?, ?, ?, 0)"#,
        )
        .bind(listing_id)
        .bind(submission_id)
        .bind(model_id)
        .bind(seed.request_sha256())
        .bind(super::grounded_occurrence_capability_sha256(&capability))
        .bind(seed.bind(listing_id).resolution_sha256())
        .bind(rendered_html_sha256.as_str())
        .bind(extracted_listing_sha256.as_str())
        .bind(product_fingerprint.as_str())
        .bind(collision_closure.as_str())
        .execute(pool)
        .await
        .unwrap();

        let mut wrong_quantity = listing_avionics_value_from_catalog(
            &ListingAvionicsValue::from_parsed(parsed_avionics("GTX 345R")),
            &identity,
            ListingSourceConfidenceBasis::RetainedHigh,
        );
        wrong_quantity.quantity = 2;
        wrong_quantity
            .grounded_capabilities
            .push(capability.clone());
        replace_listing_avionics(&db, listing_id, &[wrong_quantity])
            .await
            .expect_err("quantity-one proof must not authorize quantity two");
        let pending_after_failure: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities WHERE listing_id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(pending_after_failure, 1);

        let second_capability = super::ListingGroundedCapability {
            occurrence_index: 1,
            occurrence_role: OccurrenceRole::Primary,
            configuration_action: "installed".to_string(),
            source_notes: Some("GTX 345R installed".to_string()),
            seed: seed.clone(),
        };

        let foreign_install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'other-grounded-test-key') RETURNING id",
        )
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let foreign_submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, rendered_html,
                 rendered_html_sha256, signature_base64,
                 extracted_listing_json, canonical_listing_id
               ) VALUES (?, ?, 'https://example.com/listing', ?, ?,
                         'other-signature', ?, ?)
               RETURNING id"#,
        )
        .bind(user.id)
        .bind(foreign_install_id)
        .bind(rendered_html)
        .bind(rendered_html_sha256.as_str())
        .bind(extraction_json)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO aircraft_sale_listing_avionics_grounded_capabilities (
                 listing_id, plugin_submission_id, occurrence_index, occurrence_role,
                 avionics_model_id, requested_quantity, configuration_action,
                 request_sha256, capability_sha256, grounded_resolution_sha256,
                 evidence_capture_sha256, extracted_listing_sha256,
                 product_fingerprint, collision_closure_sha256,
                 source_revocation_count
               ) VALUES (?, ?, 1, 'primary', ?, 1, 'installed', ?, ?, ?, ?, ?, ?, ?, 0)"#,
        )
        .bind(listing_id)
        .bind(foreign_submission_id)
        .bind(model_id)
        .bind(seed.request_sha256())
        .bind(super::grounded_occurrence_capability_sha256(
            &second_capability,
        ))
        .bind(seed.bind(listing_id).resolution_sha256())
        .bind(rendered_html_sha256.as_str())
        .bind(extracted_listing_sha256.as_str())
        .bind(product_fingerprint.as_str())
        .bind(collision_closure.as_str())
        .execute(pool)
        .await
        .unwrap();
        let mut mixed_scope = listing_avionics_value_from_catalog(
            &ListingAvionicsValue::from_parsed(parsed_avionics("GTX 345R")),
            &identity,
            ListingSourceConfidenceBasis::RetainedHigh,
        );
        mixed_scope.grounded_capabilities = vec![capability.clone(), second_capability];
        let mixed_error = replace_listing_avionics(&db, listing_id, &[mixed_scope])
            .await
            .expect_err("one authorization cannot mix signed submission checkpoints");
        assert!(mixed_error
            .to_string()
            .contains("one exact submission, capture, and extraction checkpoint"));
        sqlx::query(
            "DELETE FROM aircraft_sale_listing_avionics_grounded_capabilities \
             WHERE listing_id = ? AND plugin_submission_id = ?",
        )
        .bind(listing_id)
        .bind(foreign_submission_id)
        .execute(pool)
        .await
        .unwrap();

        values.avionics = vec![ListingAvionicsValue::from_parsed(parsed_avionics(
            "GTX 345R",
        ))];
        let replay_scope = super::GroundedCapabilityReplayScope {
            listing_id,
            plugin_submission_id: submission_id,
            rendered_html_sha256,
            extracted_listing_sha256,
            allow_provider_fallback: true,
        };
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");
        let source_units = test_listing_evidence_units(
            "https://example.com/listing",
            "<p>Garmin GTX 345R installed</p>",
        );
        let resolved = resolve_listing_avionics_values(
            &db,
            &mut values,
            Some(&unreachable),
            Some("https://example.com/listing"),
            Some("Garmin GTX 345R installed"),
            Some(&source_units),
            Some(&replay_scope),
            None,
        )
        .await
        .expect("exact pending capability should replay provider-free");
        assert!(resolved.pending_review_aspects.is_empty());
        replace_listing_avionics(&db, listing_id, &values.avionics)
            .await
            .expect("retry should atomically link, authorize, and consume");

        let state: (i64, i64, i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT COUNT(*) FROM aircraft_sale_listing_avionics WHERE aircraft_sale_listing_id = ?),
                 (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_link_authorizations authorization
                    JOIN aircraft_sale_listing_avionics link ON link.id = authorization.listing_link_id
                   WHERE link.aircraft_sale_listing_id = ? AND authorization.authorization_kind = 'same_case_grounded'),
                 (SELECT COUNT(*) FROM aircraft_sale_listing_avionics_grounded_capabilities WHERE listing_id = ?),
                 (SELECT COUNT(*) FROM avionics_product_reuse_attestations WHERE avionics_model_id = ?),
                 (SELECT COUNT(*) FROM avionics_authoritative_source_origins WHERE https_origin = 'https://manufacturer.example')"#,
        )
        .bind(listing_id)
        .bind(listing_id)
        .bind(listing_id)
        .bind(model_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            state,
            (1, 1, 0, 0, 0),
            "the exact signed occurrence must not broaden its historical product into global source authority or reuse"
        );
        let provider_calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gemini_api_usage")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(provider_calls, 0);
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
    fn duplicate_capability_mentions_cannot_infer_one_physical_unit() {
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

        let error = require_unique_resolved_listing_avionics([gps, transponder])
            .expect_err("catalog identity alone cannot establish physical co-reference");

        assert!(error
            .to_string()
            .contains("resolved from multiple listing occurrences"));
    }

    #[test]
    fn one_explicit_occurrence_preserves_its_declared_quantity() {
        let explicit_pair = resolved_avionics_value(
            375,
            &["GPS", "Transponder"],
            "installed",
            None,
            Some("Dual Garmin GNX 375"),
            Some("medium"),
            2,
        );

        let unique = require_unique_resolved_listing_avionics([explicit_pair])
            .expect("one explicit occurrence owns its declared quantity");

        assert_eq!(unique.len(), 1);
        assert_eq!(unique[0].quantity, 2);
        assert_eq!(unique[0].avionics_types, vec!["GPS", "Transponder"]);
        assert_eq!(
            unique[0].source_notes.as_deref(),
            Some("Dual Garmin GNX 375")
        );
    }

    #[test]
    fn duplicate_occurrences_cannot_select_a_maximum_quantity() {
        let single = resolved_avionics_value(
            375,
            &["GPS", "Transponder"],
            "installed",
            None,
            Some("Garmin GNX 375 installed"),
            Some("high"),
            1,
        );
        let mut pair = single.clone();
        pair.quantity = 2;

        let error = require_unique_resolved_listing_avionics([single, pair])
            .expect_err("conflicting occurrence quantities must not be reduced with max");

        assert!(error
            .to_string()
            .contains("quantity must come from one explicit source-validated occurrence"));
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

        let error = require_unique_resolved_listing_avionics([installed, replaces])
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

        let error = require_unique_resolved_listing_avionics([replaces_gtx_327, replaces_gtx_330])
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

        let error =
            require_unique_resolved_listing_avionics([replaces_gtx_327, conflicting_reference])
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
            verified_local_reuse_proof: None,
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
                verification_method = 'automated',
                verified_by_user_id = NULL,
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

    async fn revoke_test_manufacturer_source_origin(db: &AppDb, avionics_model_id: i64) {
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!("test uses SQLite")
        };
        let user = db.current_user(None).await.unwrap();
        let source_origin_id: i64 = sqlx::query_scalar(
            r#"
            SELECT source_origin.id
            FROM avionics_authoritative_source_origins source_origin
            JOIN avionics_approved_product_identities product_identity
              ON product_identity.avionics_manufacturer_identity_id =
                 source_origin.avionics_manufacturer_identity_id
            WHERE product_identity.avionics_model_id = ?
              AND source_origin.https_origin = 'https://manufacturer.example'
            "#,
        )
        .bind(avionics_model_id)
        .fetch_one(pool)
        .await
        .expect("test authority origin should exist");
        sqlx::query(
            r#"
            INSERT INTO avionics_authoritative_source_origin_revocations (
              avionics_authoritative_source_origin_id, revoked_by_user_id, reason
            ) VALUES (?, ?, 'authoritative test source is no longer trusted')
            "#,
        )
        .bind(source_origin_id)
        .bind(user.id)
        .execute(pool)
        .await
        .expect("test source origin should be revocable");
    }

    async fn pending_grounded_replay_fixture(
        stale_collision_hash: bool,
    ) -> PendingGroundedReplayFixture {
        let db = AppDb::connect("sqlite::memory:").await.unwrap();
        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            unreachable!()
        };
        let user = db.current_user(None).await.unwrap();
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .unwrap();
        let listing_id: i64 = sqlx::query_scalar(
            "INSERT INTO aircraft_sale_listings (aircraft_model_variant_id, created_by_user_id, source_url, model_year, asking_price_usd, airframe_hours) VALUES (?, ?, 'https://example.com/listing', 2023, 300000, 1000) RETURNING id",
        )
        .bind(variant_id)
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let model_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GTX 345R", "Transponder")
                .await
                .unwrap();
        let identity = super::approved_avionics_identity_for_grounded_replay(&db, model_id)
            .await
            .unwrap()
            .unwrap();
        let values = listing_values_with_variant("182T SKYLANE");
        let request = super::listing_avionics_identity_request(
            &values,
            Some("https://example.com/listing"),
            "GTX 345R installed",
            "Garmin",
            "GTX 345R",
            &["Transponder".to_string()],
            1,
        );
        let rendered_html = "<p>Garmin GTX 345R installed</p>";
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        let extraction_json = "{}";
        let extracted_listing_sha256 = format!("{:x}", Sha256::digest(extraction_json.as_bytes()));
        let install_id: i64 = sqlx::query_scalar(
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'stale-grounded-key') RETURNING id",
        )
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let submission_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO plugin_submissions (
                 user_id, plugin_install_id, source_url, rendered_html,
                 rendered_html_sha256, signature_base64,
                 extracted_listing_json, canonical_listing_id
               ) VALUES (?, ?, 'https://example.com/listing', ?, ?,
                         'stale-signature', ?, ?)
               RETURNING id"#,
        )
        .bind(user.id)
        .bind(install_id)
        .bind(rendered_html)
        .bind(rendered_html_sha256.as_str())
        .bind(extraction_json)
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let product_fingerprint = super::catalog_product_fingerprint_for_id(&db, model_id)
            .await
            .unwrap();
        let collision_closure = if stale_collision_hash {
            "0".repeat(64)
        } else {
            super::grounded_collision_closure_revision_sha256(&db, model_id)
                .await
                .unwrap()
        };
        let seed = super::grounded_resolution_receipt_basis_for_replay(&request, &identity)
            .bind_catalog_snapshot(product_fingerprint.clone(), collision_closure.clone(), 0);
        let capability = super::ListingGroundedCapability {
            occurrence_index: 0,
            occurrence_role: OccurrenceRole::Primary,
            configuration_action: "installed".to_string(),
            source_notes: Some("GTX 345R installed".to_string()),
            seed: seed.clone(),
        };
        sqlx::query(
            r#"INSERT INTO aircraft_sale_listing_avionics_grounded_capabilities (
                 listing_id, plugin_submission_id, occurrence_index, occurrence_role,
                 avionics_model_id, requested_quantity, configuration_action,
                 request_sha256, capability_sha256, grounded_resolution_sha256,
                 evidence_capture_sha256, extracted_listing_sha256,
                 product_fingerprint, collision_closure_sha256,
                 source_revocation_count
               ) VALUES (?, ?, 0, 'primary', ?, 1, 'installed', ?, ?, ?, ?, ?, ?, ?, 0)"#,
        )
        .bind(listing_id)
        .bind(submission_id)
        .bind(model_id)
        .bind(seed.request_sha256())
        .bind(super::grounded_occurrence_capability_sha256(&capability))
        .bind(seed.bind(listing_id).resolution_sha256())
        .bind(rendered_html_sha256.as_str())
        .bind(extracted_listing_sha256.as_str())
        .bind(product_fingerprint)
        .bind(collision_closure)
        .execute(pool)
        .await
        .unwrap();
        PendingGroundedReplayFixture {
            db,
            listing_id,
            model_id,
            request,
            scope: super::GroundedCapabilityReplayScope {
                listing_id,
                plugin_submission_id: submission_id,
                rendered_html_sha256,
                extracted_listing_sha256,
                allow_provider_fallback: true,
            },
        }
    }

    fn test_listing_evidence_units(
        source_url: &str,
        retained_html: &str,
    ) -> crate::html::listing::source::ListingEvidenceUnits {
        crate::html::listing::source::listing_evidence_units(source_url, retained_html)
            .expect("test listing should produce source-unit proof")
    }

    fn validated_test_source_capture_scope(
        retained_source: &str,
    ) -> ExactListingSourceCaptureScope {
        ExactListingSourceCaptureScope {
            plugin_submission_id: 1,
            rendered_html_sha256: format!("{:x}", Sha256::digest(retained_source.as_bytes())),
            extracted_listing_sha256: "0".repeat(64),
        }
    }

    fn parsed_avionics(model: &str) -> ParsedAvionics {
        ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
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
            manufacturer: Some("Garmin".to_string()),
            model: "GNX 375".to_string(),
            avionics_types: avionics_types
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            quantity,
            source: "listing".to_string(),
            source_notes: source_notes.map(ToString::to_string),
            source_confidence: source_confidence.map(ToString::to_string),
            source_confidence_basis: source_confidence
                .is_some_and(|confidence| confidence == "high")
                .then_some(ListingSourceConfidenceBasis::RetainedHigh),
            configuration_action: configuration_action.to_string(),
            replaces: replaces_avionics_model_id.map(|target| {
                crate::models::ParsedAvionicsReference {
                    manufacturer: Some("Garmin".to_string()),
                    model: format!("GTX {target}"),
                    avionics_types: vec!["Transponder".to_string()],
                }
            }),
            replaces_avionics_model_id,
            grounded_capabilities: Vec::new(),
            replacement_grounded_capabilities: Vec::new(),
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

    #[test]
    fn listing_storage_accepts_sfrm_but_rejects_top_overhaul_as_engine_time() {
        assert_eq!(
            component_time_basis_from_value(&json!("SFRM"), "engine_time_basis").unwrap(),
            "SFRM"
        );
        assert!(component_time_basis_from_value(&json!("SFRM"), "propeller_time_basis").is_err());
        assert!(component_time_basis_from_value(&json!("SPOH"), "engine_time_basis").is_err());

        validate_component_time(
            "engine",
            Some(315.0),
            "SFRM",
            Some("315 SFRM"),
            Some("high"),
        )
        .expect("factory-remanufacture engine time should persist");
        let error = validate_component_time(
            "engine",
            Some(1_180.0),
            "SFOH",
            Some("1,180 TSTOH"),
            Some("high"),
        )
        .expect_err("top-overhaul time must fail closed");
        assert!(error.to_string().contains("top-overhaul time"));
    }

    #[tokio::test]
    async fn current_sqlite_schema_persists_only_supported_engine_time_bases() {
        let preview = preview_manual_listing(&json!({
            "manufacturer": "Cessna",
            "model": "182",
            "variant": "182T",
            "model_year": 2020,
            "asking_price_usd": 450000,
            "currency": "USD",
            "airframe_hours": 900,
            "engine_hours": 292,
            "engine_time_basis": "SPOH",
            "engine_time_evidence": "Engine time: 292 hours",
            "engine_time_confidence": "high",
            "propeller_hours": null,
            "propeller_time_basis": "unknown",
            "status": "active",
            "avionics": [],
            "valuation_facts": []
        }));
        let parsed_values = super::values_from_preview(&preview, None)
            .expect("parser output should cross the storage boundary");
        assert_eq!(parsed_values.engine_hours, None);
        assert_eq!(parsed_values.engine_time_basis, "unknown");
        assert_eq!(parsed_values.engine_time_evidence, None);
        assert_eq!(parsed_values.engine_time_confidence, None);

        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("current schema should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        let variant_id = super::pending_aircraft_compatibility_variant_id(&db)
            .await
            .expect("pending compatibility variant should exist");
        let insert = |basis: &'static str, source_url: &'static str| {
            let db = db.clone();
            async move {
                execute_query!(
                    &db,
                    r#"
                    INSERT INTO aircraft_sale_listings (
                      aircraft_model_variant_id, created_by_user_id, source_url,
                      model_year, asking_price_usd, airframe_hours,
                      engine_hours, engine_time_basis,
                      engine_time_evidence, engine_time_confidence
                    ) VALUES (?, ?, ?, 2020, 450000, 900, 315, ?, '315 SFRM', 'high')
                    "#,
                    variant_id,
                    user.id,
                    source_url,
                    basis
                )
            }
        };

        insert("SFRM", "https://example.test/sfrm")
            .await
            .expect("current schema should persist SFRM");
        assert!(
            insert("SPOH", "https://example.test/spoh").await.is_err(),
            "propeller-overhaul basis must not enter the engine field"
        );

        let listing_id = query_scalar_one!(
            &db,
            i64,
            "SELECT id FROM aircraft_sale_listings WHERE source_url = 'https://example.test/sfrm'"
        )
        .expect("stored SFRM listing should be queryable");
        let before = query_as_optional!(
            &db,
            (Option<f64>, String, Option<String>, Option<String>),
            r#"
            SELECT engine_hours, engine_time_basis,
                   engine_time_evidence, engine_time_confidence
            FROM aircraft_sale_listings WHERE id = ?
            "#,
            listing_id
        )
        .expect("engine state should load")
        .expect("listing should exist");
        let error = super::update_listing(
            &db,
            user.id,
            listing_id,
            &json!({
                "engine_hours": 292,
                "engine_time_basis": "SPOH",
                "engine_time_evidence": "Engine time: 292 hours",
                "engine_time_confidence": "high"
            }),
            None,
        )
        .await
        .expect_err("bare engine SPOH update must fail before persistence");
        assert!(error.to_string().contains("engine_time_basis must be"));
        let after = query_as_optional!(
            &db,
            (Option<f64>, String, Option<String>, Option<String>),
            r#"
            SELECT engine_hours, engine_time_basis,
                   engine_time_evidence, engine_time_confidence
            FROM aircraft_sale_listings WHERE id = ?
            "#,
            listing_id
        )
        .expect("engine state should reload")
        .expect("listing should remain");
        assert_eq!(after, before, "rejected update must not mutate the listing");
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
            None,
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
    async fn unsigned_rest_create_is_local_only_and_does_not_mutate_avionics_catalog() {
        let db = AppDb::connect("sqlite::memory:")
            .await
            .expect("test database should initialize");
        let user = db
            .current_user(None)
            .await
            .expect("developer user should exist");
        seed_faa_aircraft(&db, "N123T", "TESTSERIAL").await;
        seed_curated_test_aircraft_catalog(&db, user.id).await;
        let models_before = query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM avionics_models")
            .expect("catalog count should load");
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
        preview.source_url = Some("https://example.test/listing/unsigned-local-only".to_string());
        preview.parsed_listing.avionics = vec![parsed_avionics("Imaginary 999")];
        let unreachable = GeminiListingExtractor::with_test_endpoint("http://127.0.0.1:9");

        let listing = super::create_listing(&db, user.id, &preview, None, Some(&unreachable))
            .await
            .expect("unsigned create should retain unresolved avionics for review");

        assert_eq!(listing.ingestion_state, "pending_review");
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM gemini_api_usage")
                .expect("usage count should load"),
            0
        );
        assert_eq!(
            query_scalar_one!(&db, i64, "SELECT COUNT(*) FROM avionics_models")
                .expect("catalog count should reload"),
            models_before
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listing_pending_reviews WHERE listing_id = ?",
                listing.id
            )
            .expect("pending review count should load"),
            1
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
        publish_test_aircraft_reference(&db, variant_id, 2023, &[avionics_model_id]).await;
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
    async fn signed_source_serial_correction_rejects_an_existing_owner_source_claim() {
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

        let signed_error = super::create_listing_with_progress_and_occurrence_dispositions(
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
        .expect_err("signed source admission must not create a second owner/source row");
        assert!(signed_error.to_string().contains("already claimed"));
        assert_eq!(
            super::get_listing(&db, user.id, existing_id)
                .await
                .expect("legacy listing should remain byte-for-byte unchanged"),
            before
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                "SELECT COUNT(*) FROM aircraft_sale_listings WHERE created_by_user_id = ? AND source_url = ?",
                user.id,
                source_url
            )
            .expect("source claim count should load"),
            1
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
    async fn readiness_uses_approved_avionics_graph_without_scalar_values() {
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
              model_year, asking_price_usd, currency, status, airframe_hours,
              registration_number, serial_number
            ) VALUES (?, ?, 'https://example.test/readiness', 2024, 500000,
                      'USD', 'active', 100, 'N123T', 'TESTSERIAL')
            RETURNING id
            "#,
            variant_id,
            user.id
        )
        .expect("listing should seed");
        let mut raw_identity = listing_values_with_variant("182T");
        raw_identity.model = "Readiness".to_string();
        raw_identity.model_year = 2024;
        raw_identity.source_url = Some("https://example.test/readiness".to_string());
        raw_identity.registration_number = Some("N123T".to_string());
        raw_identity.serial_number = Some("TESTSERIAL".to_string());
        super::stage_literal_aircraft_identity_observation(&db, listing_id, &raw_identity)
            .await
            .expect("raw readiness identity should stage");
        let grounding = require_listing_faa_admission(&db, listing_id)
            .await
            .expect("readiness listing should be FAA admitted");
        crate::aircraft::identity::seed_test_curated_identity_assignment(
            &db, listing_id, &grounding,
        )
        .await
        .expect("readiness listing should receive its canonical identity assignment");
        execute_query!(
            &db,
            r#"
            INSERT INTO aircraft_sale_listing_avionics (
              aircraft_sale_listing_id, avionics_model_id, source, source_notes,
              source_confidence
            ) VALUES (?, ?, 'human_review', 'explicitly installed suite', 'high')
            "#,
            listing_id,
            suite_id
        )
        .expect("listing avionics should seed");

        assert_eq!(
            super::listing_invalid_avionics_product_graph_count(&db, listing_id)
                .await
                .expect("approved unit product graph should load"),
            0
        );
        assert!(
            !crate::aircraft::reference::persistence::listing_reference_status(&db, listing_id)
                .await
                .expect("missing immutable reference should load")
                .ready
        );

        execute_query!(
            &db,
            r#"
            UPDATE avionics_models
            SET valuation_scope = 'integrated_suite'
            WHERE id = ?
            "#,
            suite_id
        )
        .expect("suite scope should seed");

        assert_eq!(
            super::listing_invalid_avionics_product_graph_count(&db, listing_id)
                .await
                .expect("suite membership should still be required"),
            1
        );
        assert!(
            !crate::aircraft::reference::persistence::listing_reference_status(&db, listing_id)
                .await
                .expect("missing immutable reference should remain unavailable")
                .ready
        );
        assert!(super::complete_listing_ingestion(&db, listing_id)
            .await
            .expect_err("a suite without approved composition must not finalize")
            .to_string()
            .contains("required suite composition"));

        execute_query!(
            &db,
            "INSERT INTO avionics_suite_components (suite_model_id, component_model_id, quantity) VALUES (?, ?, 1)",
            suite_id,
            component_id
        )
        .expect("suite membership should seed");
        assert_eq!(
            super::listing_invalid_avionics_product_graph_count(&db, listing_id)
                .await
                .expect("approved suite graph should be complete"),
            0
        );
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                r#"
                SELECT COUNT(*)
                FROM avionics_models
                WHERE id = ?
                  AND estimated_unit_value_usd IS NULL
                  AND replacement_cost_usd IS NULL
                  AND value_reference_year IS NULL
                  AND value_source IS NULL
                "#,
                suite_id
            )
            .expect("scalar value state should load"),
            1
        );
        assert!(
            !crate::aircraft::reference::persistence::listing_reference_status(&db, listing_id)
                .await
                .expect("missing immutable aircraft reference should remain unavailable")
                .ready
        );

        publish_test_aircraft_reference(&db, variant_id, 2024, &[suite_id]).await;
        assert!(
            crate::aircraft::reference::persistence::listing_reference_status(&db, listing_id)
                .await
                .expect("all shared valuation references should be ready")
                .ready
        );
        assert_eq!(
            super::complete_listing_ingestion(&db, listing_id)
                .await
                .expect("local finalization should not require Gemini or scalar avionics prices"),
            super::ListingFinalizationOutcome::Ready
        );
    }

    #[tokio::test]
    async fn completion_requires_live_authority_for_automatic_avionics_only() {
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
        let avionics_model_id = ensure_approved_test_avionics_model(&db, "Garmin", "G5", "Display")
            .await
            .expect("approved avionics should seed");
        let listing_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (?, ?, 'https://example.test/authority-completion',
                      2020, 150000, 500)
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
              aircraft_sale_listing_id, avionics_model_id, source,
              source_notes, source_confidence
            ) VALUES (?, ?, 'listing', 'Garmin G5 installed', 'high')
            "#,
            listing_id,
            avionics_model_id
        )
        .expect("automatic listing association should seed");

        let error = super::complete_listing_ingestion(&db, listing_id)
            .await
            .expect_err("automatic avionics without live authority must not complete");
        assert_eq!(error.to_string(), super::AVIONICS_AUTHORIZATION_INVALIDATED);

        execute_query!(
            &db,
            "UPDATE aircraft_sale_listing_avionics SET source = 'human_review' WHERE aircraft_sale_listing_id = ?",
            listing_id
        )
        .expect("reviewer authority should replace automatic provenance");
        assert_eq!(
            super::complete_listing_ingestion(&db, listing_id)
                .await
                .expect("explicit reviewer authority is row-free"),
            super::ListingFinalizationOutcome::Ready
        );
    }

    #[tokio::test]
    async fn manufacturer_reuse_authorizes_installed_and_replacement_endpoints_with_active_closures(
    ) {
        const SOURCE_URL: &str = "https://example.test/active-reuse-authorization";
        const EVIDENCE: &str = "Garmin GNX 375 replaces Garmin GTX 327";

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
        let installed_id = ensure_approved_test_avionics_model(&db, "Garmin", "GNX 375", "GPS")
            .await
            .expect("installed product should seed");
        let replacement_id =
            ensure_approved_test_avionics_model(&db, "Garmin", "GTX 327", "Transponder")
                .await
                .expect("replacement product should seed");
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, installed_id).await;
        attest_approved_test_avionics_model_for_current_policy_reuse(&db, replacement_id).await;

        let listing_id = query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO aircraft_sale_listings (
              aircraft_model_variant_id, created_by_user_id, source_url,
              model_year, asking_price_usd, airframe_hours
            ) VALUES (?, ?, ?, 2020, 150000, 500)
            RETURNING id
            "#,
            variant_id,
            user.id,
            SOURCE_URL
        )
        .expect("listing should seed");
        let rendered_html = format!("<main>{EVIDENCE}</main>");
        let rendered_html_sha256 = format!("{:x}", Sha256::digest(rendered_html.as_bytes()));
        let extracted_listing_json = serde_json::to_string(
            &preview_manual_listing(&json!({
                "manufacturer": "Cessna",
                "model": "182",
                "variant": "182T",
                "model_year": 2020,
                "asking_price_usd": 150000,
                "currency": "USD",
                "airframe_hours": 500,
                "status": "active",
                "avionics": [{
                    "manufacturer": "Garmin",
                    "model": "GNX 375",
                    "types": ["GPS"],
                    "quantity": 1,
                    "configuration_action": "replaces",
                    "replaces": {
                        "manufacturer": "Garmin",
                        "model": "GTX 327",
                        "types": ["Transponder"]
                    },
                    "source_evidence_text": EVIDENCE,
                    "source_confidence": "high"
                }]
            }))
            .parsed_listing,
        )
        .expect("signed extraction checkpoint should serialize");
        let install_id = query_scalar_one!(
            &db,
            i64,
            "INSERT INTO plugin_installs (user_id, public_key_base64) VALUES (?, 'active-reuse-key') RETURNING id",
            user.id
        )
        .expect("plugin install should seed");
        query_scalar_one!(
            &db,
            i64,
            r#"
            INSERT INTO plugin_submissions (
              user_id, plugin_install_id, source_url, rendered_html,
              rendered_html_sha256, signature_base64, canonical_listing_id,
              extracted_listing_json
            ) VALUES (?, ?, ?, ?, ?, 'active-reuse-signature', ?, ?)
            RETURNING id
            "#,
            user.id,
            install_id,
            SOURCE_URL,
            rendered_html.as_str(),
            rendered_html_sha256.as_str(),
            listing_id,
            extracted_listing_json.as_str()
        )
        .expect("signed submission should seed");

        let mut value = ListingAvionicsValue::from_parsed(ParsedAvionics {
            manufacturer: Some("Garmin".to_string()),
            model: "GNX 375".to_string(),
            avionics_types: vec!["GPS".to_string()],
            quantity: 1,
            configuration_action: "replaces".to_string(),
            replaces: Some(ParsedAvionicsReference {
                manufacturer: Some("Garmin".to_string()),
                model: "GTX 327".to_string(),
                avionics_types: vec!["Transponder".to_string()],
            }),
            source_evidence_text: Some(EVIDENCE.to_string()),
            source_confidence: Some("high".to_string()),
        });
        value.avionics_model_id = Some(installed_id);
        value.replaces_avionics_model_id = Some(replacement_id);
        value.source_confidence_basis = Some(ListingSourceConfidenceBasis::RetainedHigh);

        replace_listing_avionics(&db, listing_id, &[value])
            .await
            .expect("manufacturer reuse should persist both endpoint authorizations");

        let authorization_state =
            crate::avionics::authorization::listing_authorization_state(&db, listing_id)
                .await
                .expect("persisted authority should load");
        assert!(authorization_state.all_automatic_associations_current());
        assert_eq!(
            query_scalar_one!(
                &db,
                i64,
                r#"
                SELECT COUNT(*)
                FROM aircraft_sale_listing_avionics_link_authorizations authorization
                JOIN aircraft_sale_listing_avionics link
                  ON link.id = authorization.listing_link_id
                WHERE link.aircraft_sale_listing_id = ?
                  AND authorization.authorization_kind = 'manufacturer_reuse'
                  AND authorization.association_role IN ('installed', 'replacement')
                "#,
                listing_id
            )
            .expect("endpoint authorization count should load"),
            2
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

        super::finalize_listing_ingestion(&db, listing_id)
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

        let error = super::finalize_listing_ingestion(&db, listing_id)
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
    async fn initial_finalization_verifies_listing_without_factory_reference() {
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

        super::finalize_listing_ingestion(&db, listing_id)
            .await
            .expect("missing shared factory reference must not block listing verification");

        assert!(
            !crate::aircraft::reference::persistence::listing_reference_status(&db, listing_id)
                .await
                .expect("valuation reference status should load")
                .ready
        );
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
        .expect("verified listing state should load")
        .expect("verified listing should exist");
        assert_eq!(state.0, "ready");
        assert_eq!(state.1, None);
        assert!(state.2);
        assert!(state.3.is_some());
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
        let outcome = super::finalize_reviewed_listing_ingestion(&db, listing_id)
            .await
            .expect("an already ready verified listing should be a no-op");
        assert_eq!(outcome, super::ListingFinalizationOutcome::Ready);
    }

    #[tokio::test]
    async fn reviewed_finalization_verifies_without_model_year_reference() {
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
        let outcome = super::finalize_reviewed_listing_ingestion(&db, listing_id)
            .await
            .expect("missing shared factory reference must not block listing verification");
        assert_eq!(outcome, super::ListingFinalizationOutcome::Ready);
        assert!(
            !crate::aircraft::reference::persistence::listing_reference_status(&db, listing_id)
                .await
                .expect("valuation reference status should load")
                .ready
        );

        let DatabaseBackend::Sqlite(pool) = db.backend() else {
            panic!("test expects sqlite")
        };
        let state: (String, Option<String>, bool, Option<String>) = sqlx::query_as(
            "SELECT ingestion_state, ingestion_error, is_verified, ingestion_completed_at FROM aircraft_sale_listings WHERE id = ?",
        )
        .bind(listing_id)
        .fetch_one(pool)
        .await
        .expect("verified listing should remain queryable");
        assert_eq!(state.0, "ready");
        assert_eq!(state.1, None);
        assert!(state.2);
        assert!(state.3.is_some());
    }

    #[tokio::test]
    async fn reviewed_finalization_requires_current_faa_admission() {
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

        let error = super::finalize_reviewed_listing_ingestion(&db, listing_id)
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
